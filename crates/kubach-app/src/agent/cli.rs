//! `kubach mcp`: the MCP server over stdio, for agents that start it as a
//! child process (Claude Desktop, `claude mcp add kubach -- kubach mcp`).
//!
//! No window: the session comes from the command line and the settings
//! file (ADR 0005), and stdout belongs to the MCP protocol, so messages go
//! to stderr. Rights on request (#227) and mutating calls are confirmed in
//! the running app's window through its HTTP server
//! ([`kubach_mcp::bridge`]); without the app they are refused.
//!
//! A context imported into the encrypted vault (ADR 0008) is never opened
//! here: the process reads only the vault's public header to recognise its
//! name, and the app runs the session while its vault is unlocked
//! ([`kubach_mcp::proxy`]). Without the app or with the vault locked such a
//! context gives the agent an explanation instead of tools.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use kubach_core::Contexts;
use kubach_core::plugin::BoxFuture;
use kubach_core::settings::{self, ContextKey, Settings};
use kubach_core::state::{AppState, NamespaceChoice};
use kubach_mcp::bridge::{self, BridgeClient, BridgeConfirmer, ExternalSession, HostRequest};
use kubach_mcp::proxy::ProxyServer;
use kubach_mcp::{
    AuditLog, ConfirmationDecision, ConfirmationRequest, ContextChooser, ContextEntry, McpServer,
    NamespaceHint, NamespaceSource, Profile, RequestKind, SecurityPolicy, Session, SessionMode,
    SessionSpec, SharedServer,
};

use super::AUDIT_FILE;
use super::install::{self, Change, Client, Env as InstallEnv};

/// How often the process tells the app it is alive.
const HEARTBEAT: Duration = Duration::from_secs(5);

const USAGE: &str = "\
Usage: kubach mcp [--context NAME] [--namespace NS]... [--profile PROFILE] [--ttl DURATION]
                  [--grant-read]
       kubach mcp install CLIENT [--yes]
       kubach mcp uninstall CLIENT [--yes]
       kubach mcp clients

install / uninstall add or remove the server `kubach` (this binary, `mcp`, over
stdio, no token) in the config of CLIENT: claude-code, codex, opencode, cursor or
claude-desktop. The file and the change are shown first and applied after you
confirm (--yes skips the question); other servers and settings are kept and the
previous file is saved next to it as <file>.bak-kubach. `clients` shows which
clients are connected.

Serves the Model Context Protocol over stdin/stdout for one session.

  -c, --context NAME     kubeconfig context. Without any option the agent picks
                         it with list_contexts / use_context and Kubach asks
                         you in its window first; with other options but no
                         context: the startup context from Kubach settings,
                         else current-context
  -n, --namespace NS     limit the session to this namespace; repeat for
                         several if the context allows it (default: no limit:
                         the agent names namespaces and Kubach asks you the
                         first time it reads each one)
  -p, --profile PROFILE  read-only, debug or deploy: the most the session may
                         ask for (default: the context's maximum profile)
      --ttl DURATION     session length: 30m, 2h, 1h30m or minutes (default:
                         30 minutes, at most the context's session length)
      --grant-read       allow reading the session's namespaces from the start,
                         without asking in the Kubach window (reads then work
                         without the app). Without --namespace: the context's
                         default namespace only. Secrets and changes are still
                         asked for, and the context's settings still apply
  -h, --help             this help

The context must allow AI agent access: Kubach → Settings → Security.
The session starts without namespaces and rights: the agent lists the
namespaces it can ask for (list_namespaces), and the first time it needs a
right (reading a namespace, revealing a Secret, changing a namespace) Kubach
asks in its window; the answer holds until the session ends. Changes (apply,
delete, exec) are confirmed one by one there as well. That needs Kubach
running with Settings → AI agent → HTTP server on.

A context imported into the Kubach vault works only through the running app
with its vault unlocked, once the user approves the session in its window: the
app runs the session and the credentials never leave it. Locking the vault ends
the session.";

/// Command-line options of `kubach mcp`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub context: Option<String>,
    pub namespaces: Vec<String>,
    pub profile: Option<Profile>,
    pub ttl: Option<Duration>,
    /// `--grant-read`.
    pub grant_read: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Parsed {
    Run(Options),
    Help,
}

fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut options = Options::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_owned())),
            _ => (arg.as_str(), None),
        };
        let mut value = |name: &str| {
            inline
                .clone()
                .or_else(|| args.next().cloned())
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--grant-read" if inline.is_none() => options.grant_read = true,
            "-c" | "--context" => options.context = Some(value("--context")?),
            "-n" | "--namespace" => options.namespaces.push(value("--namespace")?),
            "-p" | "--profile" => {
                let text = value("--profile")?;
                options.profile = Some(Profile::parse(&text).ok_or_else(|| {
                    format!("unknown profile `{text}`: use read-only, debug or deploy")
                })?);
            }
            "--ttl" => {
                let text = value("--ttl")?;
                options.ttl = Some(
                    parse_duration(&text)
                        .ok_or_else(|| format!("`{text}` is not a duration like 30m or 2h"))?,
                );
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(Parsed::Run(options))
}

/// `install CLIENT [--yes]` / `uninstall CLIENT [--yes]` → client and
/// whether to skip the question.
fn parse_connect(args: &[String]) -> Result<(Client, bool), String> {
    let mut client = None;
    let mut yes = false;
    for arg in args {
        match arg.as_str() {
            "-y" | "--yes" => yes = true,
            other if other.starts_with('-') => return Err(format!("unknown argument `{other}`")),
            other if client.is_some() => return Err(format!("unexpected argument `{other}`")),
            other => {
                client = Some(Client::parse(other).ok_or_else(|| {
                    format!(
                        "unknown client `{other}`: use {}",
                        Client::ALL.map(Client::id).join(", ")
                    )
                })?)
            }
        }
    }
    let client = client
        .ok_or_else(|| format!("which client? {}", Client::ALL.map(Client::id).join(", ")))?;
    Ok((client, yes))
}

/// `kubach mcp install|uninstall`: shows the file and the diff, asks, writes.
fn connect_command(install: bool, args: &[String]) -> ExitCode {
    use std::io::IsTerminal as _;
    let (client, yes) = match parse_connect(args) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("kubach mcp: {error}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let Some(env) = InstallEnv::current() else {
        eprintln!("kubach mcp: no home folder");
        return ExitCode::FAILURE;
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            eprintln!("kubach mcp: cannot tell where kubach is: {error}");
            return ExitCode::FAILURE;
        }
    };
    let stdin = std::io::stdin();
    let interactive = stdin.is_terminal();
    let mut input = stdin.lock();
    let mut out = std::io::stderr();
    match connect_client(
        &env,
        client,
        &exe,
        install,
        yes,
        interactive,
        &mut input,
        &mut out,
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("kubach mcp: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The flow of [`connect_command`], with its environment passed in.
#[allow(clippy::too_many_arguments)]
fn connect_client(
    env: &InstallEnv,
    client: Client,
    exe: &Path,
    install: bool,
    yes: bool,
    interactive: bool,
    input: &mut dyn std::io::BufRead,
    out: &mut dyn std::io::Write,
) -> Result<(), String> {
    let plan = if install {
        install::plan_install(env, client, exe)
    } else {
        install::plan_uninstall(env, client)
    }
    .map_err(|error| error.to_string())?;
    if plan.change == Change::Nothing {
        let _ = writeln!(
            out,
            "{}: {}",
            client,
            if install {
                "already connected to this kubach; nothing to change"
            } else {
                "not connected; nothing to change"
            }
        );
        return Ok(());
    }
    let _ = writeln!(out, "{}\n\n{}", plan.summary(), plan.diff());
    if !yes {
        if !interactive {
            return Err("not changing another program's file without a yes: pass --yes".into());
        }
        let _ = write!(out, "Apply this change? [y/N] ");
        let _ = out.flush();
        let mut answer = String::new();
        input
            .read_line(&mut answer)
            .map_err(|error| error.to_string())?;
        if !matches!(answer.trim(), "y" | "Y" | "yes" | "Yes") {
            let _ = writeln!(out, "Nothing changed.");
            return Ok(());
        }
    }
    let backup = install::apply(&plan).map_err(|error| error.to_string())?;
    if let Some(backup) = backup {
        let _ = writeln!(out, "Done. The previous file is {}.", backup.display());
    } else {
        let _ = writeln!(out, "Done.");
    }
    let _ = writeln!(out, "{}", client.restart_hint());
    Ok(())
}

/// `kubach mcp clients`: which clients are connected to this kubach.
fn clients_command() -> ExitCode {
    let (Some(env), Ok(exe)) = (InstallEnv::current(), std::env::current_exe()) else {
        eprintln!("kubach mcp: no home folder or no path to kubach");
        return ExitCode::FAILURE;
    };
    for client in Client::ALL {
        println!(
            "{:<15} {:<55} {}",
            client.id(),
            install::status(&env, client, &exe).label(),
            env.config_path(client).display()
        );
    }
    ExitCode::SUCCESS
}

/// `90`, `30m`, `2h`, `1h30m` → duration; minutes when no unit.
fn parse_duration(text: &str) -> Option<Duration> {
    let text = text.trim();
    if let Ok(minutes) = text.parse::<u64>() {
        return (minutes > 0).then(|| Duration::from_secs(minutes * 60));
    }
    let mut total = 0u64;
    let mut number = String::new();
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            number.push(ch);
            continue;
        }
        let value: u64 = number.parse().ok()?;
        number.clear();
        total += match ch {
            'h' => value * 3600,
            'm' => value * 60,
            's' => value,
            _ => return None,
        };
    }
    (number.is_empty() && total > 0).then(|| Duration::from_secs(total))
}

/// What the session is started with, resolved from options and settings.
#[derive(Debug)]
pub(super) struct Plan {
    pub(super) spec: SessionSpec,
    pub(super) policy: SecurityPolicy,
    /// Grant `read(ns)` for the session's pinned namespaces at the start.
    pub(super) grant_read: bool,
}

/// Namespaces Kubach knows of for context `name` without listing them
/// (#401): the kubeconfig one, the default in Kubach's settings and the
/// ones recently opened in Kubach (`state`). Only for `list_namespaces`;
/// none of them is a right.
pub(crate) fn namespace_hints(
    settings: &Settings,
    name: &str,
    kubeconfig: Option<&str>,
    state: Option<&AppState>,
) -> Vec<NamespaceHint> {
    let mut hints = Vec::new();
    if let Some(namespace) = kubeconfig {
        hints.push(NamespaceHint::new(namespace, NamespaceSource::Kubeconfig));
    }
    if let Some(namespace) = settings.clusters.default_namespace(name) {
        hints.push(NamespaceHint::new(
            namespace,
            NamespaceSource::KubachSettings,
        ));
    }
    if let Some(memory) = state.and_then(|state| state.namespaces(name)) {
        let selected = match &memory.selection {
            NamespaceChoice::Only(names) => names.as_slice(),
            NamespaceChoice::All => &[],
        };
        for namespace in selected.iter().chain(&memory.recent) {
            hints.push(NamespaceHint::new(
                namespace.as_str(),
                NamespaceSource::RecentlyOpened,
            ));
        }
    }
    hints
}

/// The vault context `options` ask for, if the session must run in the app:
/// `--context`, else the startup context, when kubeconfig (`local`, `None`
/// if unreadable) does not have it and the vault's header lists it.
fn vault_context(
    options: &Options,
    settings: &Settings,
    local: Option<&Contexts>,
    vault: &[String],
) -> Option<String> {
    let name = options
        .context
        .as_deref()
        .or(settings.clusters.startup_context.as_deref())?;
    let in_kubeconfig = local.is_some_and(|contexts| contexts.get(name).is_some());
    (!in_kubeconfig && vault.iter().any(|known| known == name)).then(|| name.to_owned())
}

/// Resolves `options` for a context of `contexts`. In the app, `contexts`
/// include the unlocked vault's.
pub(super) fn resolve(
    options: Options,
    settings: &Settings,
    contexts: &Contexts,
    state: Option<&AppState>,
) -> Result<Plan, String> {
    let context = match options.context {
        Some(name) => contexts.get(&name).ok_or_else(|| {
            format!("context `{name}` is neither in kubeconfig nor in the Kubach vault")
        })?,
        None => settings
            .clusters
            .startup_context
            .as_deref()
            .and_then(|name| contexts.get(name))
            .or_else(|| contexts.default_context())
            .ok_or("no context: pass --context")?,
    };
    let name = context.name.clone();
    let server = contexts.server(&name).unwrap_or_default().to_owned();
    let stored = settings
        .security
        .policy(&ContextKey::new(name.clone(), &server));
    let policy = SecurityPolicy::from(&stored);
    let grant_read = options.grant_read;
    if !policy.mcp_enabled {
        return Err(format!(
            "AI agent access is off for context `{name}`. Turn it on in Kubach: Settings → \
             Security → context `{name}` → AI agent access."
        ));
    }
    // No namespace given: the agent names them and each is asked for
    // (#401). `--grant-read` alone keeps its old meaning, the context's
    // default namespace, so no namespace is granted that nobody named.
    let namespaces = if !options.namespaces.is_empty() {
        options.namespaces
    } else if grant_read {
        vec![
            settings
                .clusters
                .default_namespace(&name)
                .map(str::to_owned)
                .or_else(|| context.namespace.clone())
                .unwrap_or_else(|| "default".to_owned()),
        ]
    } else {
        Vec::new()
    };
    let hints = namespace_hints(settings, &name, context.namespace.as_deref(), state);
    Ok(Plan {
        spec: SessionSpec {
            context: name,
            server_url: server,
            namespaces,
            // A ceiling: every right within it is asked for (#227).
            profile: options.profile.unwrap_or(policy.max_profile),
            ttl: options.ttl,
            hints,
        },
        policy,
        grant_read,
    })
}

/// The running app's bridge, if its HTTP server is on. Read on every use,
/// so starting the app or turning the server on later works. The bridge
/// has its own token, which the app writes on every start (#191).
fn locate_app(dir: &Path) -> Option<BridgeClient> {
    let settings = settings::load(&dir.join(settings::SETTINGS_FILE)).settings;
    if !settings.agent.http_enabled {
        return None;
    }
    let token = bridge::read_bridge_token(dir)?;
    Some(BridgeClient::new(settings.agent.http_port, token))
}

/// Runs `kubach mcp` with `args` (after `mcp`).
pub fn run(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        Some(verb @ ("install" | "uninstall")) => {
            return connect_command(verb == "install", &args[1..]);
        }
        Some("clients") => return clients_command(),
        _ => {}
    }
    let options = match parse(args) {
        Ok(Parsed::Run(options)) => options,
        Ok(Parsed::Help) => {
            eprintln!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("kubach mcp: {error}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match futures::executor::block_on(kubach_core::runtime::spawn(serve(options))) {
        Ok(Ok(())) => ExitCode::SUCCESS,
        Ok(Err(error)) => {
            eprintln!("kubach mcp: {error}");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("kubach mcp: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn serve(options: Options) -> Result<(), String> {
    let dir: Option<PathBuf> = settings::config_dir();
    let settings = dir
        .as_ref()
        .map(|dir| settings::load(&dir.join(settings::SETTINGS_FILE)).settings)
        .unwrap_or_default();
    // Nothing pins the session: the agent picks the context with
    // `use_context`, and the user approves it in the app (#421).
    if options.context.is_none()
        && options.namespaces.is_empty()
        && options.profile.is_none()
        && options.ttl.is_none()
        && !options.grant_read
    {
        return serve_choosing(settings, dir).await;
    }
    let contexts = Contexts::load_with_extra(&settings.clusters.resolved_kubeconfig_paths());
    // Names only: nothing of the vault is decrypted in this process.
    let vault = kubach_core::vault::default_path()
        .map(|path| kubach_core::vault::context_names(&path))
        .unwrap_or_default();
    if let Some(context) = vault_context(&options, &settings, contexts.as_ref().ok(), &vault) {
        return serve_hosted(options, context, dir).await;
    }
    let contexts =
        contexts.map_err(|error| format!("cannot read kubeconfig: {}", error.user_message()))?;
    let state = kubach_core::state::default_path().map(|path| kubach_core::state::load(&path));
    let plan = resolve(options, &settings, &contexts, state.as_ref())?;
    let config = contexts
        .config(&plan.spec.context)
        .await
        .map_err(|error| format!("context `{}`: {}", plan.spec.context, error.user_message()))?;
    let session = Session::start(&config, &plan.spec, &plan.policy)
        .await
        .map_err(|error| format!("cannot start the session: {error}"))?;

    let audit = dir.as_ref().and_then(|dir| {
        std::fs::create_dir_all(dir).ok()?;
        match AuditLog::open(dir.join(AUDIT_FILE)) {
            Ok(log) => Some(Arc::new(log)),
            Err(error) => {
                eprintln!("kubach mcp: audit log not written: {error}");
                None
            }
        }
    });
    let locate_dir = dir.clone();
    let confirmer =
        BridgeConfirmer::new(Box::new(move || locate_dir.as_deref().and_then(locate_app)));
    let server = McpServer::new(session.clone(), Arc::new(confirmer), audit);
    server.set_helm(super::helm_env(&settings, true));
    if plan.grant_read {
        if let Err(error) = server.grant_read_at_launch().await {
            let _ = session.close().await;
            return Err(format!("--grant-read: {error}"));
        }
        eprintln!("kubach mcp: reading the session's namespaces is allowed by --grant-read");
    }

    let scope = session.scope();
    eprintln!(
        "kubach mcp: session {} on {} ({}), until {}; {}",
        session.id(),
        scope.context,
        scope.profile,
        scope.expires_at,
        if scope.pinned() {
            format!("namespaces: {}", scope.namespaces.join(", "))
        } else {
            "namespaces are asked for in the Kubach window when the agent first reads them"
                .to_owned()
        },
    );
    for boundary in session.boundaries() {
        eprintln!("kubach mcp: {}", boundary.summary());
    }

    // Only this process knows it: the app lets nobody else report or end
    // the session (#191).
    let key = bridge::random_token();
    let heartbeat = tokio::spawn(heartbeat(session.clone(), key.clone(), dir.clone()));
    let served = kubach_mcp::serve_stdio(server).await;
    heartbeat.abort();
    let _ = session.close().await;
    if let Some(app) = dir.as_deref().and_then(locate_app) {
        let _ = app.ended(session.id(), &key).await;
    }
    served.map_err(|error| error.to_string())
}

/// The session of a context the agent chose, with its heartbeat.
struct Chosen {
    session: Arc<Session>,
    key: String,
    heartbeat: tokio::task::JoinHandle<()>,
}

/// Picks the context for `kubach mcp` started without one (#421): the
/// agent names a kubeconfig context, the user approves it in the running
/// app (over the bridge), and only then the session starts here.
struct StdioChooser {
    settings: Settings,
    dir: Option<PathBuf>,
    audit: Option<Arc<AuditLog>>,
    chosen: std::sync::Mutex<Option<Chosen>>,
}

impl StdioChooser {
    fn contexts(&self) -> Result<Contexts, String> {
        Contexts::load_with_extra(&self.settings.clusters.resolved_kubeconfig_paths())
            .map_err(|error| format!("cannot read kubeconfig: {}", error.user_message()))
    }

    fn vault_names() -> Vec<String> {
        kubach_core::vault::default_path()
            .map(|path| kubach_core::vault::context_names(&path))
            .unwrap_or_default()
    }

    fn confirmer(&self) -> BridgeConfirmer {
        let dir = self.dir.clone();
        BridgeConfirmer::new(Box::new(move || dir.as_deref().and_then(locate_app)))
    }

    async fn start(&self, name: &str, client: &str) -> Result<McpServer, String> {
        let contexts = self.contexts()?;
        let vault = Self::vault_names();
        if contexts.get(name).is_none() && vault.iter().any(|known| known == name) {
            return Err(format!(
                "context `{name}` is in the Kubach vault: its session runs in the app. Ask the \
                 user to configure `kubach mcp --context {name}`, or to connect over HTTP"
            ));
        }
        let (server_url, _) = super::check_context_choice(
            &self.settings,
            Some(&contexts),
            &vault,
            &std::collections::HashSet::new(),
            name,
        )?;
        let question = ConfirmationRequest {
            kind: RequestKind::UseContext {
                server_url,
                source: "kubeconfig".into(),
            },
            session_id: String::new(),
            context: name.to_owned(),
            client: client.to_owned(),
            tool: "use_context".into(),
            namespace: String::new(),
            summary: format!("{client} wants to work in context {name} (kubach mcp)"),
            details: "A yes starts an agent session in this `kubach mcp` process: it begins \
                      without namespaces and rights, which are asked for here when a call \
                      needs them."
                .into(),
            dangerous: Vec::new(),
            mode: SessionMode::ClientOnly,
            allow_remember: false,
        };
        let decision = kubach_mcp::confirm::ask(&self.confirmer(), &question).await;
        match decision {
            decision if decision.is_approved() => {}
            ConfirmationDecision::Rejected { reason } => {
                return Err(format!(
                    "the user did not allow working in context `{name}`{}",
                    reason.map(|r| format!(" ({r})")).unwrap_or_default()
                ));
            }
            other => {
                return Err(format!(
                    "no answer from the user to working in context `{name}`{}. Kubach asks in \
                     its window: it has to run with Settings → AI agent → Serve MCP over HTTP \
                     on, or the user configures `kubach mcp --context {name}`",
                    other
                        .reason()
                        .map(|reason| format!(" ({reason})"))
                        .unwrap_or_default()
                ));
            }
        }
        let state = kubach_core::state::default_path().map(|path| kubach_core::state::load(&path));
        let options = Options {
            context: Some(name.to_owned()),
            ..Options::default()
        };
        let plan = resolve(options, &self.settings, &contexts, state.as_ref())?;
        let config = contexts
            .config(&plan.spec.context)
            .await
            .map_err(|error| format!("context `{name}`: {}", error.user_message()))?;
        let session = Session::start(&config, &plan.spec, &plan.policy)
            .await
            .map_err(|error| format!("cannot start the session: {error}"))?;
        let server = McpServer::new(
            session.clone(),
            Arc::new(self.confirmer()),
            self.audit.clone(),
        );
        server.set_helm(super::helm_env(&self.settings, true));
        eprintln!(
            "kubach mcp: session {} on {name} ({}), chosen by the agent and approved in Kubach",
            session.id(),
            session.scope().profile,
        );
        let key = bridge::random_token();
        let heartbeat = tokio::spawn(heartbeat(session.clone(), key.clone(), self.dir.clone()));
        let previous = kubach_core::sync::lock(&self.chosen).replace(Chosen {
            session,
            key,
            heartbeat,
        });
        if let Some(previous) = previous {
            self.finish(previous).await;
        }
        Ok(server)
    }

    /// Ends a session this process ran and tells the app.
    async fn finish(&self, chosen: Chosen) {
        chosen.heartbeat.abort();
        let _ = chosen.session.close().await;
        if let Some(dir) = &self.dir {
            let _ = app_ended(dir, chosen.session.id(), &chosen.key).await;
        }
    }
}

impl ContextChooser for StdioChooser {
    fn list(&self) -> BoxFuture<'_, Vec<ContextEntry>> {
        Box::pin(async move {
            let contexts = self.contexts().ok();
            let vault = Self::vault_names();
            let mut entries = super::context_entries(
                &self.settings,
                contexts.as_ref(),
                &std::collections::HashSet::new(),
                &vault,
            );
            for entry in &mut entries {
                if entry.source == "vault" {
                    entry.unavailable = Some(format!(
                        "a vault context: the user configures `kubach mcp --context {}` for it",
                        entry.name
                    ));
                }
            }
            entries
        })
    }

    fn choose<'a>(
        &'a self,
        context: &'a str,
        client: &'a str,
    ) -> BoxFuture<'a, Result<McpServer, String>> {
        Box::pin(self.start(context, client))
    }
}

/// `kubach mcp` without a context (#421): MCP is served at once with
/// `list_contexts` and `use_context`; a session starts when the agent
/// picks a context and the user approves it.
async fn serve_choosing(settings: Settings, dir: Option<PathBuf>) -> Result<(), String> {
    let audit = dir.as_ref().and_then(|dir| {
        std::fs::create_dir_all(dir).ok()?;
        AuditLog::open(dir.join(AUDIT_FILE)).ok().map(Arc::new)
    });
    let chooser = Arc::new(StdioChooser {
        settings,
        dir,
        audit,
        chosen: std::sync::Mutex::new(None),
    });
    let shared = SharedServer::new();
    shared.set_chooser(chooser.clone());
    eprintln!(
        "kubach mcp: no context given; the agent picks one with use_context and the user \
         approves it in the Kubach window"
    );
    let served = kubach_mcp::serve_stdio(shared).await;
    let chosen = kubach_core::sync::lock(&chooser.chosen).take();
    if let Some(chosen) = chosen {
        chooser.finish(chosen).await;
    }
    served.map_err(|error| error.to_string())
}

/// Serves a session the app runs on vault context `context`.
async fn serve_hosted(
    options: Options,
    context: String,
    dir: Option<PathBuf>,
) -> Result<(), String> {
    let request = HostRequest {
        pid: std::process::id(),
        context,
        namespaces: options.namespaces,
        profile: options.profile,
        ttl_secs: options.ttl.map(|ttl| ttl.as_secs()),
        grant_read: options.grant_read,
    };
    let locate_dir = dir.clone();
    let proxy = ProxyServer::new(
        Box::new(move || locate_dir.as_deref().and_then(locate_app)),
        request,
    );
    // The user approves the session in the app's window first (#191),
    // which may take a while: MCP is served meanwhile, with a placeholder
    // tool until the session runs.
    eprintln!("kubach mcp: asking for the session in the Kubach window…");
    let connecting = tokio::spawn(connect(proxy.clone()));
    let pinger = tokio::spawn(ping(proxy.clone()));
    let served = kubach_mcp::serve_stdio(proxy.clone()).await;
    connecting.abort();
    pinger.abort();
    proxy.end().await;
    served.map_err(|error| error.to_string())
}

/// Starts the hosted session and reports how it went.
async fn connect(proxy: ProxyServer) {
    match proxy.connect().await {
        Ok(Some(reply)) => {
            let session = &reply.session;
            eprintln!(
                "kubach mcp: session {} on {} ({}), run by the Kubach app on a context of its \
                 vault, until {} or the vault locks",
                session.id, session.context, session.profile, session.expires_at,
            );
            for note in &reply.notes {
                eprintln!("kubach mcp: {note}");
            }
        }
        Ok(None) => {}
        Err(not) if not.may_retry => {
            eprintln!(
                "kubach mcp: no session yet, retried on the agent's next call: {}",
                not.message
            );
        }
        Err(not) => eprintln!("kubach mcp: no session: {}", not.message),
    }
}

/// Keeps the hosted session alive in the app and reports when it ends.
async fn ping(proxy: ProxyServer) {
    let mut ticks = tokio::time::interval(HEARTBEAT);
    loop {
        ticks.tick().await;
        if let Some(gone) = proxy.ping().await {
            eprintln!("kubach mcp: {}", gone.message);
        }
    }
}

/// Reports the session to the app until it ends; closes it when the user
/// revokes it there.
async fn heartbeat(session: Arc<Session>, key: String, dir: Option<PathBuf>) {
    let Some(dir) = dir else { return };
    let mut ticks = tokio::time::interval(HEARTBEAT);
    while session.is_active() {
        ticks.tick().await;
        let Some(app) = locate_app(&dir) else {
            continue;
        };
        let Ok(reply) = app.heartbeat(&ExternalSession::of(&session), &key).await else {
            continue;
        };
        for grant in &reply.revoke_grants {
            if let Err(error) = session.revoke(grant).await {
                eprintln!("kubach mcp: {grant} taken back, but the Role was not updated: {error}");
            }
        }
        if reply.revoked {
            eprintln!("kubach mcp: the session was revoked in Kubach");
            let _ = session.close().await;
        }
    }
    let _ = app_ended(&dir, session.id(), &key).await;
}

async fn app_ended(dir: &Path, id: &str, key: &str) -> Option<()> {
    locate_app(dir)?.ended(id, key).await.ok()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kubach_core::Contexts;
    use kubach_core::settings::{ContextKey, ContextPolicy, Settings};
    use kubach_mcp::{NamespaceHint, NamespaceSource, Profile};

    use super::{Options, Parsed, parse, parse_duration, resolve, vault_context};

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn parses_options() {
        assert_eq!(parse(&args("--help")), Ok(Parsed::Help));
        let Ok(Parsed::Run(options)) = parse(&args(
            "-c minikube --namespace a -n b --profile=debug --ttl 1h30m",
        )) else {
            panic!("not parsed");
        };
        assert_eq!(options.context.as_deref(), Some("minikube"));
        assert_eq!(options.namespaces, ["a", "b"]);
        assert_eq!(options.profile, Some(Profile::Debug));
        assert_eq!(options.ttl, Some(Duration::from_secs(5400)));
        assert!(parse(&args("--profile admin")).is_err());
        assert!(parse(&args("--context")).is_err());
        assert!(parse(&args("--bogus")).is_err());
        let Ok(Parsed::Run(flagged)) = parse(&args("--grant-read -n web")) else {
            panic!("not parsed");
        };
        assert!(flagged.grant_read);
        assert!(!options.grant_read);
        assert!(parse(&args("--grant-read=yes")).is_err());
    }

    #[test]
    fn connect_asks_and_writes_only_after_a_yes() {
        use super::install::{Client, Env, Os, Status, status};
        use super::{connect_client, parse_connect};

        assert_eq!(
            parse_connect(&args("claude-code --yes")),
            Ok((Client::ClaudeCode, true))
        );
        assert!(parse_connect(&args("vim")).is_err());
        assert!(parse_connect(&args("")).is_err());

        let home = tempfile::tempdir().unwrap();
        let env = Env::with_home(Os::Linux, home.path());
        let exe = std::path::PathBuf::from("/opt/kubach/kubach");
        let path = env.config_path(Client::Cursor);
        let run = |install: bool, yes: bool, interactive: bool, answer: &str| {
            let mut out = Vec::new();
            let result = connect_client(
                &env,
                Client::Cursor,
                &exe,
                install,
                yes,
                interactive,
                &mut answer.as_bytes(),
                &mut out,
            );
            (result, String::from_utf8(out).unwrap())
        };

        // No terminal and no --yes: refused, nothing written.
        let (result, out) = run(true, false, false, "");
        assert!(result.unwrap_err().contains("--yes"));
        assert!(out.contains(&path.display().to_string()), "{out}");
        assert!(!path.exists());
        // A no: nothing written.
        let (result, out) = run(true, false, true, "n\n");
        assert!(result.is_ok());
        assert!(out.contains("Nothing changed"), "{out}");
        assert!(!path.exists());
        // A yes: written; the diff showed the server.
        let (result, out) = run(true, false, true, "y\n");
        assert!(result.is_ok(), "{result:?}");
        assert!(out.contains("+    \"kubach\": {"), "{out}");
        assert_eq!(status(&env, Client::Cursor, &exe), Status::Connected);
        // Again: nothing to change, no question.
        let (result, out) = run(true, false, false, "");
        assert!(result.is_ok());
        assert!(out.contains("already connected"), "{out}");
        // --yes removes without asking.
        let (result, _) = run(false, true, false, "");
        assert!(result.is_ok());
        assert_eq!(status(&env, Client::Cursor, &exe), Status::NotConnected);
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("45"), Some(Duration::from_secs(2700)));
        assert_eq!(parse_duration("2h"), Some(Duration::from_secs(7200)));
        assert_eq!(parse_duration("90s"), Some(Duration::from_secs(90)));
        assert_eq!(parse_duration("0"), None);
        assert_eq!(parse_duration("5x"), None);
        assert_eq!(parse_duration("1h5"), None);
    }

    const KUBECONFIG: &str = r#"
apiVersion: v1
kind: Config
current-context: dev
clusters:
- name: dev
  cluster: { server: "https://127.0.0.1:6443" }
contexts:
- name: dev
  context: { cluster: dev, user: dev, namespace: team }
users:
- name: dev
  user: { token: t }
"#;

    #[test]
    fn resolves_from_settings_and_refuses_when_disabled() {
        let contexts = Contexts::from_yaml(KUBECONFIG).unwrap();
        let mut settings = Settings::default();
        let error = resolve(Options::default(), &settings, &contexts, None).unwrap_err();
        assert!(error.contains("AI agent access is off"), "{error}");
        // The flag does not widen the settings.
        let flagged = Options {
            grant_read: true,
            ..Options::default()
        };
        let error = resolve(flagged.clone(), &settings, &contexts, None).unwrap_err();
        assert!(error.contains("AI agent access is off"), "{error}");

        let key = ContextKey::new("dev", "https://127.0.0.1:6443");
        settings
            .security
            .set_policy(&key, ContextPolicy::development());
        let plan = resolve(Options::default(), &settings, &contexts, None).unwrap();
        assert!(!plan.grant_read);
        assert_eq!(plan.spec.context, "dev");
        // No namespace pinned: the agent asks for them (#401); kubeconfig's
        // is only a hint for list_namespaces.
        assert!(plan.spec.namespaces.is_empty());
        assert_eq!(
            plan.spec.hints,
            [NamespaceHint::new("team", NamespaceSource::Kubeconfig)]
        );
        // `--grant-read` alone still means the default namespace only.
        let granted = resolve(flagged, &settings, &contexts, None).unwrap();
        assert!(granted.grant_read);
        assert_eq!(granted.spec.namespaces, ["team"]);
        // The ceiling is the context's maximum; rights are asked for.
        assert_eq!(plan.spec.profile, Profile::Deploy);
        let narrow = Options {
            profile: Some(Profile::ReadOnly),
            ..Options::default()
        };
        assert_eq!(
            resolve(narrow, &settings, &contexts, None)
                .unwrap()
                .spec
                .profile,
            Profile::ReadOnly
        );
        assert!(plan.policy.mcp_enabled);

        settings
            .clusters
            .set_default_namespace("dev", Some("web".into()));
        let pinned = Options {
            namespaces: vec!["shop".into()],
            grant_read: true,
            ..Options::default()
        };
        let plan = resolve(pinned, &settings, &contexts, None).unwrap();
        assert_eq!(plan.spec.namespaces, ["shop"]);
        let flagged = Options {
            grant_read: true,
            ..Options::default()
        };
        let plan = resolve(flagged, &settings, &contexts, None).unwrap();
        assert_eq!(plan.spec.namespaces, ["web"]);

        let unknown = Options {
            context: Some("prod".into()),
            ..Options::default()
        };
        assert!(resolve(unknown, &settings, &contexts, None).is_err());
    }

    #[test]
    fn hints_come_from_kubeconfig_settings_and_recent_namespaces() {
        use kubach_core::state::{AppState, NamespaceChoice, NamespaceMemory};
        let mut settings = Settings::default();
        settings
            .clusters
            .set_default_namespace("dev", Some("web".into()));
        let mut state = AppState::default();
        state.remember_namespaces(
            "dev",
            NamespaceMemory {
                selection: NamespaceChoice::Only(vec!["shop".into()]),
                recent: vec!["shop".into(), "api".into()],
            },
        );
        state.remember_namespaces(
            "other",
            NamespaceMemory {
                selection: NamespaceChoice::Only(vec!["elsewhere".into()]),
                recent: Vec::new(),
            },
        );
        let hints = super::namespace_hints(&settings, "dev", Some("team"), Some(&state));
        let named: Vec<(&str, NamespaceSource)> =
            hints.iter().map(|h| (h.name.as_str(), h.source)).collect();
        assert_eq!(
            named,
            [
                ("team", NamespaceSource::Kubeconfig),
                ("web", NamespaceSource::KubachSettings),
                ("shop", NamespaceSource::RecentlyOpened),
                ("shop", NamespaceSource::RecentlyOpened),
                ("api", NamespaceSource::RecentlyOpened),
            ]
        );
    }

    #[test]
    fn vault_contexts_go_to_the_app() {
        let contexts = Contexts::from_yaml(KUBECONFIG).unwrap();
        let vault = vec!["prod-imported".to_owned(), "dev".to_owned()];
        let mut settings = Settings::default();
        let named = |name: &str| Options {
            context: Some(name.into()),
            ..Options::default()
        };

        assert_eq!(
            vault_context(&named("prod-imported"), &settings, Some(&contexts), &vault),
            Some("prod-imported".into())
        );
        // No kubeconfig at all: the vault still works through the app.
        assert_eq!(
            vault_context(&named("prod-imported"), &settings, None, &vault),
            Some("prod-imported".into())
        );
        // kubeconfig wins on a name in both (the vault renames on import).
        assert_eq!(
            vault_context(&named("dev"), &settings, Some(&contexts), &vault),
            None
        );
        assert_eq!(
            vault_context(&named("nope"), &settings, Some(&contexts), &vault),
            None
        );
        assert_eq!(
            vault_context(&Options::default(), &settings, Some(&contexts), &vault),
            None
        );

        settings.clusters.startup_context = Some("prod-imported".into());
        assert_eq!(
            vault_context(&Options::default(), &settings, Some(&contexts), &vault),
            Some("prod-imported".into())
        );
        // Without the vault's names, a vault context is just unknown.
        assert_eq!(
            vault_context(&named("prod-imported"), &settings, Some(&contexts), &[]),
            None
        );
    }
}
