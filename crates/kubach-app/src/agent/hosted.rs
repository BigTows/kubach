//! Who may use a context imported into the vault (#121, ADR 0005 and
//! 0008): the rules the app applies before running a session for `kubach
//! mcp` and when the vault changes, and the user's approval of every such
//! session (#191). UI-free, so it is tested directly.

use std::collections::HashSet;
use std::future::Future;

use kubach_core::Contexts;
use kubach_core::settings::Settings;
use kubach_core::state::AppState;
use kubach_core::vault::Imported;
use kubach_i18n::t;
use kubach_mcp::bridge::{HostRefusal, HostRequest};
use kubach_mcp::hosted::Gone;
use kubach_mcp::{ConfirmationDecision, ConfirmationRequest, Confirmer, RequestKind, SessionMode};

use super::cli::{Options, Plan, resolve};

/// Contexts of the vault that have their credentials in memory: none while
/// it is locked.
pub(super) fn unlocked_contexts(imported: &[Imported]) -> HashSet<String> {
    imported
        .iter()
        .filter(|entry| entry.config.is_ok())
        .flat_map(|entry| entry.contexts.iter().cloned())
        .collect()
}

/// Whether a session on vault context `context` has to end: the vault
/// locked or the context left it.
pub(super) fn lock_ends(context: &str, unlocked: &HashSet<String>) -> Option<Gone> {
    (!unlocked.contains(context)).then(|| Gone::locked(context))
}

/// Checks a `kubach mcp` request against the vault and the settings.
///
/// `contexts` are kubeconfig plus the vault (`None` if unreadable),
/// `unlocked` the vault's contexts with credentials in memory. Only vault
/// contexts are hosted: kubeconfig ones `kubach mcp` serves itself.
pub(super) fn plan(
    request: &HostRequest,
    settings: &Settings,
    contexts: Option<&Contexts>,
    vault_names: &[String],
    unlocked: &HashSet<String>,
    state: Option<&AppState>,
) -> Result<Plan, HostRefusal> {
    let name = request.context.as_str();
    let imported = contexts
        .and_then(|contexts| contexts.source(name))
        .is_some_and(|source| source.is_imported());
    if !imported || !unlocked.contains(name) {
        return Err(if vault_names.iter().any(|known| known == name) {
            HostRefusal::locked(name)
        } else {
            HostRefusal::denied(format!(
                "context `{name}` is not imported into the Kubach vault; contexts from kubeconfig \
                 are served by `kubach mcp` itself"
            ))
        });
    }
    let Some(contexts) = contexts else {
        return Err(HostRefusal::locked(name));
    };
    let options = Options {
        context: Some(name.to_owned()),
        namespaces: request.namespaces.clone(),
        profile: request.profile,
        ttl: request.ttl_secs.map(std::time::Duration::from_secs),
        grant_read: request.grant_read,
    };
    resolve(options, settings, contexts, state).map_err(HostRefusal::denied)
}

/// What the user is asked before the app starts a session on a vault
/// context for a `kubach mcp` process: context, namespaces, profile, time
/// and the process. `process` is what the OS says about the pid the process
/// reported ([`describe_process`]).
pub(super) fn consent_request(
    request: &HostRequest,
    plan: &Plan,
    process: Option<&str>,
) -> ConfirmationRequest {
    let spec = &plan.spec;
    let ttl = spec
        .ttl
        .unwrap_or(kubach_mcp::DEFAULT_SESSION_TTL)
        .min(kubach_mcp::MAX_SESSION_TTL);
    let minutes = ttl.as_secs().div_ceil(60);
    // Shown to the user only, in the language of the interface.
    let time = match (minutes / 60, minutes % 60) {
        (0, m) => t!("settings-duration-minutes", minutes = m),
        (h, 0) => t!("settings-duration-hours", hours = h),
        (h, m) => t!("settings-duration-hours-minutes", hours = h, minutes = m),
    };
    let reading = if plan.grant_read {
        t!("agent-consent-reading-granted")
    } else {
        t!("agent-consent-reading-asked")
    };
    let namespaces = if spec.namespaces.is_empty() {
        t!("agent-consent-namespaces-any")
    } else {
        spec.namespaces.join(", ")
    };
    let process = process
        .map(str::to_owned)
        .unwrap_or_else(|| t!("agent-consent-unknown-process", pid = request.pid));
    ConfirmationRequest {
        kind: RequestKind::Call,
        session_id: t!("agent-consent-session", pid = request.pid),
        context: spec.context.clone(),
        client: format!("kubach mcp: {process}"),
        tool: t!("agent-consent-tool"),
        namespace: spec.namespaces.join(", "),
        summary: t!("agent-consent-summary", context = spec.context.as_str()),
        details: [
            t!("agent-consent-namespaces", namespaces = namespaces),
            t!(
                "agent-consent-access",
                access = super::access_label(plan.policy.minimum.capped(spec.profile))
            ),
            t!("agent-consent-time", time = time),
            t!("agent-consent-reading", reading = reading),
            t!("agent-consent-process", process = process.as_str()),
            t!("agent-consent-warning"),
        ]
        .join("\n"),
        dangerous: Vec::new(),
        // The identity is chosen when the session starts and may fall back
        // to the vault's own credentials: warn as for the weakest one.
        mode: SessionMode::ClientOnly,
        allow_remember: false,
    }
}

/// Runs `start` only once the user approved `consent`; a rejection, no
/// answer within the timeout or a closed window is a refusal for the
/// process.
pub(super) async fn after_consent<T>(
    confirmer: &dyn Confirmer,
    consent: &ConfirmationRequest,
    context: &str,
    start: impl Future<Output = Result<T, String>>,
) -> Result<T, HostRefusal> {
    match kubach_mcp::confirm::ask(confirmer, consent).await {
        ConfirmationDecision::Approved | ConfirmationDecision::ApprovedAlways => start
            .await
            .map_err(|error| HostRefusal::denied(format!("cannot start the session: {error}"))),
        ConfirmationDecision::Rejected { .. } => Err(HostRefusal::rejected(context)),
        // Values answer only Secret forms (#230); anything else is no yes.
        ConfirmationDecision::Provided { .. } => Err(HostRefusal::denied(
            "unexpected answer to the session question",
        )),
        ConfirmationDecision::Denied { reason } => Err(HostRefusal::denied(format!(
            "The user did not approve the session on context `{context}` in the Kubach window \
             ({}). Ask the user to watch the Kubach window, then retry.",
            reason.as_deref().unwrap_or("no answer")
        ))),
    }
}

/// The process `pid` and its parent (the agent that started it), as `ps`
/// shows them: `kubach (pid 4242), started by claude (pid 4100)`. `None`
/// when there is no such process or the OS cannot tell.
pub(super) fn describe_process(pid: u32) -> Option<String> {
    let (ppid, name) = ps(pid)?;
    let parent = ps(ppid)
        .map(|(_, parent)| format!(", started by {parent} (pid {ppid})"))
        .unwrap_or_default();
    Some(format!("{name} (pid {pid}){parent}"))
}

/// Parent pid and executable name of `pid`.
#[cfg(unix)]
fn ps(pid: u32) -> Option<(u32, String)> {
    // An absolute path: never a `ps` from the working directory (#202).
    let ps = ["/bin/ps", "/usr/bin/ps"]
        .into_iter()
        .find(|path| std::path::Path::new(path).exists())?;
    let output = std::process::Command::new(ps)
        .args(["-o", "ppid=", "-o", "comm=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_ps(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(not(unix))]
fn ps(_pid: u32) -> Option<(u32, String)> {
    None
}

/// `"  4100 /usr/local/bin/claude"` → `(4100, "claude")`.
fn parse_ps(line: &str) -> Option<(u32, String)> {
    let line = line.lines().next()?.trim();
    let (ppid, command) = line.split_once(char::is_whitespace)?;
    let command = command.trim();
    let name = command.rsplit('/').next().unwrap_or(command);
    if name.is_empty() {
        return None;
    }
    Some((ppid.parse().ok()?, name.to_owned()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::PathBuf;

    use kubach_core::Contexts;
    use kubach_core::kubeconfig::{FileProblem, Source};
    use kubach_core::settings::{ContextKey, ContextPolicy, Settings};
    use kubach_core::vault::Imported;
    use kubach_mcp::Profile;
    use kubach_mcp::bridge::{HostRequest, RefusalKind};
    use kubach_mcp::hosted::EndReason;

    use std::sync::atomic::{AtomicBool, Ordering};

    use kubach_mcp::ScriptedConfirmer;
    use kubach_mcp::confirm::DenyAll;

    use super::{
        after_consent, consent_request, describe_process, lock_ends, parse_ps, plan,
        unlocked_contexts,
    };

    const LOCAL: &str = r#"
apiVersion: v1
kind: Config
current-context: dev
clusters:
- name: dev
  cluster: { server: "https://127.0.0.1:6443" }
contexts:
- name: dev
  context: { cluster: dev, user: dev }
users:
- name: dev
  user: { token: local }
"#;

    const IMPORTED: &str = r#"
apiVersion: v1
kind: Config
clusters:
- name: prod
  cluster: { server: "https://prod.example.com" }
contexts:
- name: prod-imported
  context: { cluster: prod, user: prod, namespace: shop }
users:
- name: prod
  user: { token: vault-secret }
"#;

    const SERVER: &str = "https://prod.example.com";

    fn imported(unlocked: bool) -> Vec<Imported> {
        vec![Imported {
            source: Source::Imported {
                vault: PathBuf::from("/tmp/mcp121-vault.kubach"),
                id: "1".into(),
                name: "prod.yaml".into(),
            },
            config: if unlocked {
                Ok(kube::config::Kubeconfig::from_yaml(IMPORTED).unwrap())
            } else {
                Err(FileProblem::Locked)
            },
            contexts: vec!["prod-imported".into()],
        }]
    }

    fn local_file(dir: &tempfile::TempDir) -> Source {
        let path = dir.path().join("config");
        std::fs::write(&path, LOCAL).unwrap();
        Source::Linked(path)
    }

    fn request(context: &str) -> HostRequest {
        HostRequest {
            pid: 1,
            context: context.into(),
            namespaces: Vec::new(),
            profile: None,
            ttl_secs: None,
            grant_read: false,
        }
    }

    fn allowed() -> Settings {
        let mut settings = Settings::default();
        let key = ContextKey::new("prod-imported", SERVER);
        settings.security.set_policy(&key, ContextPolicy::default());
        settings
    }

    #[test]
    fn only_unlocked_vault_contexts_count() {
        assert!(unlocked_contexts(&imported(false)).is_empty());
        let unlocked = unlocked_contexts(&imported(true));
        assert_eq!(unlocked, HashSet::from(["prod-imported".to_owned()]));

        let gone = lock_ends("prod-imported", &HashSet::new()).unwrap();
        assert_eq!(gone.reason, EndReason::Locked);
        assert!(lock_ends("prod-imported", &unlocked).is_none());
    }

    #[test]
    fn hosts_vault_contexts_only_while_unlocked_and_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let names = vec!["prod-imported".to_owned()];

        // Locked: the context is known by name only.
        let locked = Contexts::load_all(vec![local_file(&dir)], imported(false)).unwrap();
        let refusal = plan(
            &request("prod-imported"),
            &allowed(),
            Some(&locked),
            &names,
            &HashSet::new(),
            None,
        )
        .unwrap_err();
        assert_eq!(refusal.kind, RefusalKind::Locked);

        let open = Contexts::load_all(vec![local_file(&dir)], imported(true)).unwrap();
        let unlocked = unlocked_contexts(&imported(true));

        // Unlocked but AI agent access turned off for the context (#39, #9).
        let mut off = Settings::default();
        off.security.set_policy(
            &ContextKey::new("prod-imported", SERVER),
            ContextPolicy {
                mcp_enabled: false,
                ..ContextPolicy::default()
            },
        );
        let refusal = plan(
            &request("prod-imported"),
            &off,
            Some(&open),
            &names,
            &unlocked,
            None,
        )
        .unwrap_err();
        assert_eq!(refusal.kind, RefusalKind::Denied);
        assert!(
            refusal.message.contains("AI agent access is off"),
            "{}",
            refusal.message
        );

        // Unlocked and allowed: no namespace pinned (#401), up to its maximum
        // profile (rights are asked for, #227).
        let plan_ok = plan(
            &request("prod-imported"),
            &allowed(),
            Some(&open),
            &names,
            &unlocked,
            None,
        )
        .unwrap();
        assert_eq!(plan_ok.spec.context, "prod-imported");
        assert_eq!(plan_ok.spec.server_url, SERVER);
        assert!(plan_ok.spec.namespaces.is_empty());
        assert_eq!(plan_ok.spec.hints[0].name, "shop");
        assert_eq!(plan_ok.spec.profile, Profile::Deploy);

        // A kubeconfig context is not the app's to host.
        let refusal = plan(
            &request("dev"),
            &allowed(),
            Some(&open),
            &names,
            &unlocked,
            None,
        )
        .unwrap_err();
        assert_eq!(refusal.kind, RefusalKind::Denied);

        // Unknown everywhere.
        let refusal = plan(
            &request("nope"),
            &allowed(),
            Some(&open),
            &names,
            &unlocked,
            None,
        )
        .unwrap_err();
        assert_eq!(refusal.kind, RefusalKind::Denied);
    }

    fn approved_plan() -> super::Plan {
        let dir = tempfile::tempdir().unwrap();
        let open = Contexts::load_all(vec![local_file(&dir)], imported(true)).unwrap();
        plan(
            &request("prod-imported"),
            &allowed(),
            Some(&open),
            &["prod-imported".to_owned()],
            &unlocked_contexts(&imported(true)),
            None,
        )
        .unwrap()
    }

    /// The user is asked about the context, namespace, access and process.
    #[test]
    fn the_user_sees_what_starts() {
        let plan = approved_plan();
        let consent = consent_request(
            &request("prod-imported"),
            &plan,
            Some("kubach (pid 1), started by claude (pid 0)"),
        );
        assert_eq!(consent.context, "prod-imported");
        assert_eq!(consent.namespace, "");
        assert!(
            consent.details.contains("Namespaces: any the agent names"),
            "{}",
            consent.details
        );
        // The minimal access, never a ceiling (#15).
        assert!(
            consent.details.contains("Without asking:") && consent.details.contains("Read"),
            "{}",
            consent.details
        );
        assert!(consent.client.contains("started by claude"));
        assert!(!consent.allow_remember);
        assert!(!consent.mode.server_enforced());
        let unknown = consent_request(&request("prod-imported"), &plan, None);
        assert!(unknown.client.contains("pid 1"), "{}", unknown.client);
    }

    /// No session starts without the user's yes (#191).
    #[test]
    fn hosted_sessions_start_only_when_approved() {
        let consent = consent_request(&request("prod-imported"), &approved_plan(), None);
        let run = |confirmer: Box<dyn kubach_mcp::Confirmer>| {
            let consent = consent.clone();
            let started = std::sync::Arc::new(AtomicBool::new(false));
            let flag = started.clone();
            let result = futures::executor::block_on(kubach_core::runtime::spawn(async move {
                after_consent(confirmer.as_ref(), &consent, "prod-imported", async move {
                    flag.store(true, Ordering::SeqCst);
                    Ok::<_, String>(())
                })
                .await
            }))
            .unwrap();
            (result, started.load(Ordering::SeqCst))
        };

        let (result, started) = run(Box::new(ScriptedConfirmer::denying()));
        assert_eq!(result.unwrap_err().kind, RefusalKind::Rejected);
        assert!(!started);

        // No answer (no window, timeout): refused too, may be asked again.
        let (result, started) = run(Box::new(DenyAll));
        assert_eq!(result.unwrap_err().kind, RefusalKind::Denied);
        assert!(!started);

        let (result, started) = run(Box::new(ScriptedConfirmer::approving()));
        assert!(result.is_ok());
        assert!(started);
    }

    #[test]
    fn processes_by_pid() {
        assert_eq!(
            parse_ps("  4100 /usr/local/bin/claude\n"),
            Some((4100, "claude".to_owned()))
        );
        assert_eq!(parse_ps(" 1 launchd"), Some((1, "launchd".to_owned())));
        assert_eq!(parse_ps(""), None);
        #[cfg(unix)]
        {
            let me = describe_process(std::process::id()).unwrap();
            assert!(me.contains(&format!("pid {}", std::process::id())), "{me}");
        }
    }
}
