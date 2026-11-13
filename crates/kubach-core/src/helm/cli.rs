//! Changes to releases through the user's `helm` binary (ADR 0006).
//!
//! Kubach never writes the release storage itself: rollback, uninstall and
//! upgrade run the installed `helm` with an explicit context, kubeconfig,
//! namespace and storage driver matching the open connection, so the result
//! is exactly what the user's CLI would do.
//!
//! A context that is in no kubeconfig file (imported into the vault) goes
//! to helm as a temporary kubeconfig with that context alone, which lives
//! for the one helm process ([`HelmTarget::memory`]).
//!
//! Values for an upgrade go through stdin (`--values -`), never a file; the
//! chart kept in the release is written to a private temporary directory
//! that is removed when the call ends.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::process::Command;

use super::chart::StoredChart;
use super::{Driver, Release, Sensitive, progress, release_from_json};
use crate::cluster::Contexts;
use crate::kubeconfig::temp::ContextKubeconfig;
pub use crate::process::Cancel;
use crate::process::{self, ProcessError, Stdin, Stream};
use crate::validation::ValidationError;

/// How long a `helm` call may run before it is killed. Helm's own
/// `--timeout` (5m) only covers waiting for resources, so leave room above it.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(6 * 60);

/// The connection a `helm` call must act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelmTarget {
    /// Kubeconfig context (`--kube-context`).
    pub context: String,
    /// Kubeconfig files the connection was loaded from, in merge order.
    /// Empty leaves helm's own default (`KUBECONFIG` or `~/.kube/config`).
    /// Not used with [`memory`](Self::memory).
    pub kubeconfig: Vec<PathBuf>,
    /// The context itself, when it is in no file (imported into the vault,
    /// #201): each helm process then reads it from a temporary kubeconfig
    /// of its own, the only configuration it sees, and never the files in
    /// [`kubeconfig`](Self::kubeconfig), `KUBECONFIG` or `~/.kube/config`.
    pub memory: Option<ContextKubeconfig>,
    /// Release namespace (`--namespace`).
    pub namespace: String,
    /// Storage driver (`HELM_DRIVER`).
    pub driver: Driver,
    /// Extra environment that may carry credentials (`HELM_KUBETOKEN` of
    /// an agent session's ServiceAccount, #418): set on the process only,
    /// never in the arguments or `Debug` output. Empty for the UI.
    pub secret_env: SecretEnv,
    /// The directory helm runs in ([`HelmCommand::working_dir`]); `None`
    /// leaves the one of the application.
    pub working_dir: Option<PathBuf>,
}

/// Environment variables for helm that may hold credentials; `None`
/// removes an inherited variable.
pub type SecretEnv = Sensitive<Vec<(&'static str, Option<OsString>)>>;

/// Arguments and environment of one `helm` call.
///
/// Positional arguments (release, chart, repository names) come after `--`,
/// so a name read from the cluster such as `--kube-apiserver=…` stays a
/// name and never becomes a flag (#202). Helm 3 and 4 (cobra) accept `--`
/// in every subcommand built here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelmCommand {
    pub args: Vec<OsString>,
    /// Set on top of the inherited environment; `None` removes the variable.
    pub env: Vec<(&'static str, Option<OsString>)>,
    /// Written to helm's stdin (values for `--values -`).
    pub stdin: Option<Sensitive<String>>,
    /// Set after `env`; see [`HelmTarget::secret_env`].
    pub secret_env: SecretEnv,
    /// The context for a temporary kubeconfig written when helm starts;
    /// see [`HelmTarget::memory`].
    memory: Option<ContextKubeconfig>,
    /// The directory helm runs in. Helm takes a chart reference such as
    /// `repo/chart` for a path when a directory of that name is where it
    /// runs, so a call made for an agent runs in an empty private directory
    /// ([`private_dir`]). `None` leaves the one of the application.
    pub working_dir: Option<PathBuf>,
    /// Why the call must not run (an invalid release name, credentials for
    /// a plain `http://` repository); [`run`] refuses it before helm starts.
    refusal: Option<ValidationError>,
}

/// Longest release name Helm accepts.
pub const MAX_RELEASE_NAME: usize = 53;

/// Checks a release name the way Helm does: a DNS-1123 subdomain of at most
/// [`MAX_RELEASE_NAME`] characters. Same rule and messages as the install
/// form ([`super::repo::check_release_name`]), so both refuse the same names.
pub fn check_release_name(name: &str) -> Result<(), ValidationError> {
    super::repo::check_release_name(name)
}

/// Where `helm upgrade` takes the chart from.
#[derive(Clone, Debug, PartialEq)]
pub enum UpgradeChart {
    /// The chart kept in the release (see [`StoredChart::usable`]).
    Stored(StoredChart),
    /// A chart as `helm upgrade` takes it: `repo/name`, `oci://…`, a path
    /// or a URL, with an optional `--version`.
    Reference {
        chart: String,
        version: Option<String>,
    },
    /// A chart archive on disk, by absolute path ([`pull_chart`]): every
    /// call that names it reads the same bytes.
    Archive(PathBuf),
}

/// `helm upgrade` of a release with new values.
#[derive(Clone, Debug, PartialEq)]
pub struct UpgradeRequest {
    pub release: String,
    pub chart: UpgradeChart,
    /// User values as YAML. They replace the release's values on top of the
    /// chart defaults (`--reset-values --values -`), like `helm get values`
    /// edited and passed back.
    pub values: Sensitive<String>,
}

/// Result of [`upgrade`] and [`install`].
#[derive(Debug)]
pub enum UpgradeOutcome {
    /// `--dry-run=server`: the release Helm would write, with its manifest.
    Preview(Box<Release>),
    /// The upgrade or install ran; helm's output.
    Applied(String),
}

/// `helm install` of a repository chart (`repo/chart`) or one given by
/// reference (`oci://…`, a path or a URL).
#[derive(Clone, Debug, PartialEq)]
pub struct InstallRequest {
    pub release: String,
    pub chart: String,
    /// `--version`; `None` takes the latest stable version.
    pub version: Option<String>,
    /// User values as YAML, passed on stdin (`--values -`) like an upgrade.
    pub values: Sensitive<String>,
    /// `--create-namespace`.
    pub create_namespace: bool,
}

/// `helm template` of a chart: the objects it would create, rendered on the
/// client without touching a cluster (#18).
#[derive(Clone, Debug, PartialEq)]
pub struct TemplateRequest {
    pub release: String,
    /// `.Release.Namespace` (`--namespace`); no cluster is contacted.
    pub namespace: String,
    pub chart: UpgradeChart,
    /// User values as YAML, on stdin (`--values -`).
    pub values: Sensitive<String>,
    /// Renders as for an upgrade (`--is-upgrade`), without the chart's
    /// `crds/` directory, which `helm upgrade` leaves alone. An install
    /// renders those CRDs too (`--include-crds`).
    pub is_upgrade: bool,
    /// The directory helm runs in ([`HelmCommand::working_dir`]).
    pub working_dir: Option<PathBuf>,
}

/// Basic auth of a chart repository for `helm repo add`.
#[derive(Clone, Debug, PartialEq)]
pub struct RepoCredentials {
    pub username: String,
    pub password: Sensitive<String>,
}

/// What `helm show` prints about a chart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShowKind {
    /// The chart's default `values.yaml`.
    Values,
    /// The chart's README.
    Readme,
}

impl HelmCommand {
    /// `helm rollback <release> <revision>` on `target`.
    pub fn rollback(target: &HelmTarget, release: &str, revision: u32) -> Self {
        Self::new(
            target,
            &["rollback"],
            Vec::new(),
            vec![release.into(), revision.to_string().into()],
        )
        .for_release(release)
    }

    /// `helm uninstall <release>`, with `--keep-history` to keep the
    /// revisions (the release then lists as `uninstalled`).
    pub fn uninstall(target: &HelmTarget, release: &str, keep_history: bool) -> Self {
        let mut flags = Vec::new();
        if keep_history {
            flags.push("--keep-history".into());
        }
        Self::new(target, &["uninstall"], flags, vec![release.into()]).for_release(release)
    }

    /// `helm upgrade <release> <chart> --reset-values --values -` with the
    /// values on stdin. `chart_dir` is where the stored chart was written;
    /// without it the stored chart shows as `<stored name-version>` (for
    /// display). `dry_run` renders on the server and prints the release as
    /// JSON instead of changing anything.
    pub fn upgrade(
        target: &HelmTarget,
        request: &UpgradeRequest,
        chart_dir: Option<&Path>,
        dry_run: bool,
    ) -> Self {
        let mut flags = Vec::new();
        let chart: OsString = match (&request.chart, chart_dir) {
            (UpgradeChart::Stored(_), Some(dir)) => dir.into(),
            (UpgradeChart::Stored(chart), None) => format!("<stored {}>", chart.reference()).into(),
            (UpgradeChart::Archive(path), _) => path.into(),
            (UpgradeChart::Reference { chart, version }, _) => {
                if let Some(version) = non_empty(version.as_deref()) {
                    flags.extend(["--version".into(), version.into()]);
                }
                chart.trim().into()
            }
        };
        flags.extend(["--reset-values", "--values", "-"].map(OsString::from));
        if dry_run {
            flags.extend(["--dry-run=server", "--output", "json"].map(OsString::from));
        }
        let mut command = Self::new(
            target,
            &["upgrade"],
            flags,
            vec![request.release.clone().into(), chart],
        )
        .for_release(&request.release);
        command.stdin = Some(request.values.clone());
        command
    }

    /// `helm install <release> <chart> [--version v] --values -` with the
    /// values on stdin, optionally `--create-namespace`. `dry_run` renders
    /// on the server and prints the release as JSON instead.
    pub fn install(target: &HelmTarget, request: &InstallRequest, dry_run: bool) -> Self {
        let mut flags = Vec::new();
        if let Some(version) = non_empty(request.version.as_deref()) {
            flags.extend(["--version".into(), version.into()]);
        }
        flags.extend(["--values", "-"].map(OsString::from));
        if request.create_namespace {
            flags.push("--create-namespace".into());
        }
        if dry_run {
            flags.extend(["--dry-run=server", "--output", "json"].map(OsString::from));
        }
        let release = request.release.trim();
        let mut command = Self::new(
            target,
            &["install"],
            flags,
            vec![release.into(), request.chart.trim().into()],
        )
        .for_release(release);
        command.stdin = Some(request.values.clone());
        command
    }

    /// `helm template <release> <chart>` with the values on stdin. Client
    /// side only: no `--validate`, no kube context, so no credentials are
    /// used; `.Capabilities` are helm's defaults and `lookup` finds nothing.
    /// `chart_dir` is where a stored chart was written (see [`Self::upgrade`]).
    pub fn template(request: &TemplateRequest, chart_dir: Option<&Path>) -> Self {
        let (flags, positional) = Self::template_args(request, chart_dir, true);
        let mut args: Vec<OsString> = vec!["template".into()];
        args.extend(flags);
        push_positional(&mut args, positional);
        Self {
            args,
            env: Vec::new(),
            stdin: Some(request.values.clone()),
            secret_env: SecretEnv::default(),
            memory: None,
            working_dir: request.working_dir.clone(),
            refusal: None,
        }
        .for_release(request.release.trim())
    }

    /// `helm template` on `target` with `--dry-run=server` (#114): `lookup`
    /// reads the cluster as `target`'s identity, nothing is written. For a
    /// target of the session's own identity only.
    pub fn template_on_cluster(
        target: &HelmTarget,
        request: &TemplateRequest,
        chart_dir: Option<&Path>,
    ) -> Self {
        let (mut flags, positional) = Self::template_args(request, chart_dir, false);
        flags.push("--dry-run=server".into());
        let mut command = Self::new(target, &["template"], flags, positional);
        command.stdin = Some(request.values.clone());
        command.for_release(request.release.trim())
    }

    /// The flags and positional arguments of `helm template`; `namespace`:
    /// with `--namespace`, which a target otherwise sets.
    fn template_args(
        request: &TemplateRequest,
        chart_dir: Option<&Path>,
        namespace: bool,
    ) -> (Vec<OsString>, Vec<OsString>) {
        let mut flags: Vec<OsString> = Vec::new();
        let chart: OsString = match (&request.chart, chart_dir) {
            (UpgradeChart::Stored(_), Some(dir)) => dir.into(),
            (UpgradeChart::Stored(chart), None) => format!("<stored {}>", chart.reference()).into(),
            (UpgradeChart::Archive(path), _) => path.into(),
            (UpgradeChart::Reference { chart, version }, _) => {
                if let Some(version) = non_empty(version.as_deref()) {
                    flags.extend(["--version".into(), version.into()]);
                }
                chart.trim().into()
            }
        };
        flags.extend(["--values", "-"].map(OsString::from));
        if namespace {
            flags.extend(["--namespace".into(), request.namespace.clone().into()]);
        }
        flags.push(if request.is_upgrade {
            "--is-upgrade".into()
        } else {
            "--include-crds".into()
        });
        (flags, vec![request.release.trim().into(), chart])
    }

    /// `helm pull <chart> --destination <dir>`: the chart archive, with the
    /// user's repositories and logins, as `helm template` fetches it. No
    /// cluster is contacted.
    pub fn pull(chart: &str, version: Option<&str>, destination: &Path) -> Self {
        let mut args: Vec<OsString> = vec!["pull".into()];
        if let Some(version) = non_empty(version) {
            args.extend(["--version".into(), version.into()]);
        }
        args.extend(["--destination".into(), destination.into()]);
        push_positional(&mut args, [chart.trim().into()]);
        Self {
            args,
            env: Vec::new(),
            stdin: None,
            secret_env: SecretEnv::default(),
            memory: None,
            working_dir: None,
            refusal: None,
        }
    }

    /// `helm repo add <name> <url>`: writes the user's `repositories.yaml`
    /// and downloads the index. With `credentials` adds
    /// `--username <user> --password-stdin`: the password goes to helm's
    /// stdin, never into the arguments (seen in the process list and shown
    /// in the UI). Credentials for a URL other than `https://` are refused
    /// by [`run`] (#202).
    pub fn repo_add(name: &str, url: &str, credentials: Option<&RepoCredentials>) -> Self {
        let mut flags = Vec::new();
        if let Some(credentials) = credentials {
            flags.extend([
                "--username",
                credentials.username.trim(),
                "--password-stdin",
            ]);
        }
        let mut command = Self::local(&["repo", "add"], flags, [name.trim(), url.trim()]);
        command.stdin = credentials.map(|credentials| credentials.password.clone());
        if credentials.is_some() {
            command.refusal = super::repo::check_repository_credentials(url).err();
        }
        command
    }

    /// `helm repo update [names…]`: downloads the indexes again (all
    /// repositories without names).
    pub fn repo_update(names: &[&str]) -> Self {
        Self::local(&["repo", "update"], [], names.iter().copied())
    }

    /// `helm repo remove <name>`.
    pub fn repo_remove(name: &str) -> Self {
        Self::local(&["repo", "remove"], [], [name])
    }

    /// `helm show values|readme <chart> [--version v]`. Needs no cluster:
    /// helm resolves the chart with the user's repositories or registry login.
    pub fn show(kind: ShowKind, chart: &str, version: Option<&str>) -> Self {
        let kind = match kind {
            ShowKind::Values => "values",
            ShowKind::Readme => "readme",
        };
        let mut flags = Vec::new();
        if let Some(version) = non_empty(version) {
            flags.extend(["--version", version]);
        }
        Self::local(&["show", kind], flags, [chart.trim()])
    }

    /// A call that does not touch a cluster: `subcommand flags -- positional`.
    fn local<'a>(
        subcommand: &[&'a str],
        flags: impl IntoIterator<Item = &'a str>,
        positional: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let mut args: Vec<OsString> = subcommand
            .iter()
            .copied()
            .chain(flags)
            .map(OsString::from)
            .collect();
        push_positional(&mut args, positional.into_iter().map(OsString::from));
        Self {
            args,
            env: Vec::new(),
            stdin: None,
            secret_env: SecretEnv::default(),
            memory: None,
            working_dir: None,
            refusal: None,
        }
    }

    /// Refuses the call when `release` is not a valid release name: a name
    /// read from the cluster is not trusted.
    fn for_release(mut self, release: &str) -> Self {
        if let Err(error) = check_release_name(release) {
            self.refusal.get_or_insert(error);
        }
        self
    }

    /// `subcommand flags`, the flags pinning it to `target`, then
    /// `-- positional`.
    fn new(
        target: &HelmTarget,
        subcommand: &[&str],
        flags: Vec<OsString>,
        positional: Vec<OsString>,
    ) -> Self {
        let mut args: Vec<OsString> = subcommand.iter().map(OsString::from).collect();
        args.extend(flags);
        args.extend(["--kube-context".into(), target.context.clone().into()]);
        args.extend(["--namespace".into(), target.namespace.clone().into()]);
        let mut env = vec![(
            "HELM_DRIVER",
            Some(OsString::from(driver_name(target.driver))),
        )];
        match target.kubeconfig.as_slice() {
            // The context is in no file: `run_watched` writes it to one and
            // names it. What the user's shell sets for another server or
            // identity does not apply to it either.
            _ if target.memory.is_some() => {
                env.extend(CONNECTION_VARS.iter().map(|name| (*name, None)));
            }
            [] => {}
            // `--kubeconfig` takes one file; it also wins over `KUBECONFIG`.
            [file] => args.extend(["--kubeconfig".into(), file.clone().into()]),
            // Several files merge only through `KUBECONFIG`, by the same
            // first-wins rules the connection used.
            files => {
                // A path containing the separator cannot be joined; the
                // first file then still holds the context in most setups.
                let joined =
                    std::env::join_paths(files).unwrap_or_else(|_| files[0].clone().into());
                env.push(("KUBECONFIG", Some(joined)));
            }
        }
        push_positional(&mut args, positional);
        Self {
            args,
            env,
            stdin: None,
            secret_env: target.secret_env.clone(),
            memory: target.memory.clone(),
            working_dir: target.working_dir.clone(),
            refusal: None,
        }
    }

    /// Runs helm in `dir` ([`Self::working_dir`]).
    pub fn in_dir(mut self, dir: Option<&Path>) -> Self {
        self.working_dir = dir.map(Path::to_owned);
        self
    }

    /// Why [`run`] refuses this call, if it does.
    pub fn refusal(&self) -> Option<&ValidationError> {
        self.refusal.as_ref()
    }

    /// Adds `--debug`, so helm logs each step to stderr while it works and
    /// [`run_watched`] can show what it waits for (#382). The values,
    /// hooks and manifest `--debug` adds to stdout are removed again.
    pub fn with_progress(mut self) -> Self {
        if !self.debug() {
            let at = self
                .args
                .iter()
                .position(|arg| arg == "--")
                .unwrap_or(self.args.len());
            self.args.insert(at, "--debug".into());
        }
        self
    }

    fn debug(&self) -> bool {
        self.args
            .iter()
            .take_while(|arg| *arg != "--")
            .any(|arg| arg == "--debug")
    }

    /// The call as a shell-like line for the UI (`helm rollback web 2 …`).
    pub fn display(&self) -> String {
        std::iter::once(OsStr::new("helm"))
            .chain(self.args.iter().map(OsString::as_os_str))
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Appends `-- positional…`: after `--` helm takes every argument as a
/// name, even one starting with `-`. Nothing is added without arguments.
fn push_positional(args: &mut Vec<OsString>, positional: impl IntoIterator<Item = OsString>) {
    let mut positional = positional.into_iter().peekable();
    if positional.peek().is_some() {
        args.push("--".into());
        args.extend(positional);
    }
}

fn non_empty(text: Option<&str>) -> Option<&str> {
    text.map(str::trim).filter(|text| !text.is_empty())
}

/// `HELM_DRIVER` value of a driver.
pub fn driver_name(driver: Driver) -> &'static str {
    match driver {
        Driver::Secret => "secret",
        Driver::ConfigMap => "configmap",
    }
}

/// Variables of the user's environment that would point helm at another
/// server or identity than the kubeconfig names. Removed for a context
/// that is in no file ([`HelmTarget::memory`]): its temporary kubeconfig is
/// all helm goes by, as Kubach's own connection does.
const CONNECTION_VARS: &[&str] = &[
    "HELM_KUBEAPISERVER",
    "HELM_KUBECAFILE",
    "HELM_KUBETOKEN",
    "HELM_KUBEASUSER",
    "HELM_KUBEASGROUPS",
    "HELM_KUBEINSECURE_SKIP_TLS_VERIFY",
    "HELM_KUBETLS_SERVER_NAME",
];

/// What helm reads `context` from: the kubeconfig files and, for a context
/// that is in none of them (imported into the vault), the context itself
/// ([`HelmTarget::memory`]) with `temp_dir` for its temporary files
/// ([`crate::kubeconfig::temp::dir`]). `None` when the context can't be
/// given to helm: it is not loaded (the vault is locked) or there is no
/// place for the file. Helm then must not run, as the files do not have it.
pub fn kubeconfig_of(
    contexts: &Contexts,
    context: &str,
    linked: &[PathBuf],
    temp_dir: Option<PathBuf>,
) -> Option<(Vec<PathBuf>, Option<ContextKubeconfig>)> {
    contexts.get(context)?;
    if !contexts
        .source(context)
        .is_some_and(crate::kubeconfig::Source::is_imported)
    {
        return Some((kubeconfig_files(linked), None));
    }
    let memory = contexts.context_kubeconfig(context, temp_dir?)?;
    Some((Vec::new(), Some(memory)))
}

/// Kubeconfig files in merge order: `KUBECONFIG` (or `~/.kube/config`) and
/// then `linked` files from settings, as the connection reads them.
pub fn kubeconfig_files(linked: &[PathBuf]) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    for source in crate::kubeconfig::sources_with(linked) {
        let path = source.path().to_owned();
        if !files.contains(&path) {
            files.push(path);
        }
    }
    files
}

#[derive(Debug, thiserror::Error)]
pub enum HelmCliError {
    #[error("helm not found; download it or set its path in Settings → Tools")]
    NotInstalled,
    #[error("can't start {path}: {source}")]
    Spawn {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("helm did not finish in {} s and was stopped", .0.as_secs())]
    Timeout(Duration),
    #[error("{}", cancel_message(*.killed, .stderr))]
    Cancelled { killed: bool, stderr: String },
    #[error("{}", failure_message(*.code, .stderr))]
    Failed { code: Option<i32>, stderr: String },
    #[error("can't write the temporary kubeconfig for helm: {0}")]
    Kubeconfig(std::io::ErrorKind),
    #[error("can't use the chart kept in the release: {0}")]
    StoredChart(String),
    #[error("can't read helm's output: {0}")]
    Output(String),
    #[error(transparent)]
    Refused(ValidationError),
}

fn failure_message(code: Option<i32>, stderr: &str) -> String {
    let stderr = stderr.trim();
    match (code, stderr.is_empty()) {
        (_, false) => stderr.to_owned(),
        (Some(code), true) => format!("helm exited with code {code}"),
        (None, true) => "helm was terminated by a signal".to_owned(),
    }
}

fn cancel_message(killed: bool, stderr: &str) -> String {
    if killed {
        return "Stopped: helm did not exit when asked and was killed; the release may stay \
                pending until it is rolled back or uninstalled"
            .to_owned();
    }
    let said = progress::failure_text(stderr);
    if said.is_empty() {
        "Stopped: helm exited when asked".to_owned()
    } else {
        format!("Stopped: {said}")
    }
}

/// Called with each phase of a running `helm` change (see
/// [`progress::phase`]).
pub type PhaseHandler = Arc<dyn Fn(String) + Send + Sync>;

/// Live phases and cancellation of a `helm` call; the default watches
/// nothing.
#[derive(Clone, Default)]
pub struct HelmWatch {
    pub on_phase: Option<PhaseHandler>,
    /// Asks helm to stop (SIGTERM: install and upgrade then mark the
    /// release failed), and kills it if it does not exit in time.
    pub cancel: Option<Cancel>,
}

impl fmt::Debug for HelmWatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HelmWatch")
            .field("on_phase", &self.on_phase.is_some())
            .field("cancel", &self.cancel)
            .finish()
    }
}

impl HelmWatch {
    /// The process watch: phases come from stderr, the `--debug` log.
    fn process(self) -> process::Watch {
        let on_line = self.on_phase.map(|on_phase| -> process::LineHandler {
            Arc::new(move |stream, line| {
                if stream == Stream::Stderr
                    && let Some(phase) = progress::phase(line)
                {
                    on_phase(phase);
                }
            })
        });
        process::Watch {
            on_line,
            cancel: self.cancel,
        }
    }
}

/// Runs `helm` (`None` when it is not installed) and returns its stdout.
/// The process is killed on timeout or when the future is dropped.
pub async fn run(
    helm: Option<&Path>,
    command: &HelmCommand,
    timeout: Duration,
) -> Result<String, HelmCliError> {
    run_watched(helm, command, timeout, HelmWatch::default()).await
}

/// Like [`run`], reporting helm's phases and stopping it on cancel.
/// Phases need a command [`with_progress`](HelmCommand::with_progress).
pub async fn run_watched(
    helm: Option<&Path>,
    command: &HelmCommand,
    timeout: Duration,
    watch: HelmWatch,
) -> Result<String, HelmCliError> {
    if let Some(refusal) = &command.refusal {
        return Err(HelmCliError::Refused(refusal.clone()));
    }
    let helm = helm.ok_or(HelmCliError::NotInstalled)?;
    let debug = command.debug();
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    // A context that is in no file: written for this process alone and
    // removed when it ends, also when this future is dropped. The kind
    // only: the file's text never reaches a message.
    let kubeconfig = command
        .memory
        .as_ref()
        .map(ContextKubeconfig::write)
        .transpose()
        .map_err(|error| HelmCliError::Kubeconfig(error.kind()))?;
    let mut process = Command::new(helm);
    match &kubeconfig {
        Some(file) => {
            let names = command
                .args
                .iter()
                .position(|arg| arg == "--")
                .unwrap_or(command.args.len());
            process.args(&command.args[..names]);
            process.arg("--kubeconfig").arg(file.path());
            process.args(&command.args[names..]);
        }
        None => {
            process.args(&command.args);
        }
    }
    if let Some(dir) = &command.working_dir {
        process.current_dir(dir);
    }
    for (name, value) in command.env.iter().chain(command.secret_env.expose()) {
        match value {
            Some(value) => process.env(name, value),
            None => process.env_remove(name),
        };
    }
    if let Some(file) = &kubeconfig {
        // The flag wins over the variable; the variable is set as well, so
        // that nothing helm starts reads the user's files instead.
        process.env(crate::kubeconfig::KUBECONFIG_ENV, file.path());
    }
    let stdin = command
        .stdin
        .as_ref()
        .map(|input| Stdin::new(input.expose().as_bytes()));
    let result = process::run_watched(process, stdin, timeout, watch.process()).await;
    drop(kubeconfig);
    match result {
        Ok(output) if debug => Ok(progress::strip_debug_output(&output.stdout_text())),
        Ok(output) => Ok(output.stdout_text()),
        Err(ProcessError::Cancelled { killed, stderr }) => Err(HelmCliError::Cancelled {
            killed,
            stderr: text(&stderr),
        }),
        Err(ProcessError::Failed { status, stderr, .. }) if debug => Err(HelmCliError::Failed {
            code: status.code(),
            stderr: progress::failure_text(&text(&stderr)),
        }),
        Err(ProcessError::NotFound { .. }) => Err(HelmCliError::Spawn {
            path: helm.to_owned(),
            source: std::io::ErrorKind::NotFound.into(),
        }),
        Err(ProcessError::Spawn { source, .. }) => Err(HelmCliError::Spawn {
            path: helm.to_owned(),
            source,
        }),
        Err(ProcessError::Timeout(timeout)) => Err(HelmCliError::Timeout(timeout)),
        Err(ProcessError::Failed { status, stderr, .. }) => Err(HelmCliError::Failed {
            code: status.code(),
            stderr: text(&stderr),
        }),
    }
}

/// Runs `helm upgrade` for `request` on `target`; with `dry_run` returns
/// the release Helm would write, without changing anything.
pub async fn upgrade(
    helm: Option<&Path>,
    target: &HelmTarget,
    request: &UpgradeRequest,
    dry_run: bool,
    timeout: Duration,
) -> Result<UpgradeOutcome, HelmCliError> {
    upgrade_watched(helm, target, request, dry_run, timeout, None).await
}

/// Like [`upgrade`]; with `watch` helm runs with `--debug` and reports its
/// phases (see [`run_watched`]).
pub async fn upgrade_watched(
    helm: Option<&Path>,
    target: &HelmTarget,
    request: &UpgradeRequest,
    dry_run: bool,
    timeout: Duration,
    watch: Option<HelmWatch>,
) -> Result<UpgradeOutcome, HelmCliError> {
    check_release_name(&request.release).map_err(HelmCliError::Refused)?;
    // Kept until helm is done; the directory is removed on drop.
    let chart_dir = write_stored(&request.chart)?;
    let command = HelmCommand::upgrade(
        target,
        request,
        chart_dir.as_ref().map(|(_, path)| path.as_path()),
        dry_run,
    );
    let output = run_progress(helm, command, timeout, watch).await;
    drop(chart_dir);
    let output = output?;
    outcome(output, dry_run)
}

/// Writes the chart kept in a release to a private temporary directory
/// for helm; `None` for a chart reference.
fn write_stored(
    chart: &UpgradeChart,
) -> Result<Option<(tempfile::TempDir, PathBuf)>, HelmCliError> {
    let UpgradeChart::Stored(chart) = chart else {
        return Ok(None);
    };
    chart
        .usable()
        .map_err(|error| HelmCliError::StoredChart(error.to_string()))?;
    let dir = tempfile::Builder::new()
        .prefix("kubach-chart-")
        .tempdir()
        .map_err(|error| HelmCliError::StoredChart(error.to_string()))?;
    let path = chart
        .write_to(dir.path())
        .map_err(|error| HelmCliError::StoredChart(error.to_string()))?;
    Ok(Some((dir, path)))
}

/// Runs `helm template` for `request` (#18): the manifest the chart
/// renders, CRDs and hooks included, without contacting a cluster.
pub async fn template(
    helm: Option<&Path>,
    request: &TemplateRequest,
    timeout: Duration,
) -> Result<String, HelmCliError> {
    check_release_name(request.release.trim()).map_err(HelmCliError::Refused)?;
    let chart_dir = write_stored(&request.chart)?;
    let command =
        HelmCommand::template(request, chart_dir.as_ref().map(|(_, path)| path.as_path()));
    let output = run(helm, &command, timeout).await;
    drop(chart_dir);
    output
}

/// Runs `helm template` for `request` on `target` (#114): as [`template`],
/// with `lookup` answered by the cluster under `target`'s identity.
pub async fn template_on_cluster(
    helm: Option<&Path>,
    target: &HelmTarget,
    request: &TemplateRequest,
    timeout: Duration,
) -> Result<String, HelmCliError> {
    let chart_dir = write_stored(&request.chart)?;
    let command = HelmCommand::template_on_cluster(
        target,
        request,
        chart_dir.as_ref().map(|(_, path)| path.as_path()),
    );
    let output = run(helm, &command, timeout).await;
    drop(chart_dir);
    output
}

/// The templates of `chart` for [`super::lookup::lookup_use`], each with its
/// path as in `helm template`'s `# Source:` comments. A chart reference is
/// fetched with `helm pull` to a temporary directory, removed afterwards;
/// the cluster is not contacted either way (#114).
pub async fn chart_templates(
    helm: Option<&Path>,
    chart: &UpgradeChart,
    timeout: Duration,
) -> Result<Vec<(String, String)>, HelmCliError> {
    let (chart, version) = match chart {
        UpgradeChart::Stored(stored) => {
            let name = stored.metadata()["name"].as_str().unwrap_or_default();
            return Ok(stored
                .templates()
                .iter()
                .map(|file| {
                    (
                        format!("{name}/{}", file.name),
                        String::from_utf8_lossy(&file.data).into_owned(),
                    )
                })
                .collect());
        }
        UpgradeChart::Archive(path) => {
            let path = path.clone();
            return crate::runtime::blocking(move || {
                super::lookup::archive_templates(&std::fs::read(path)?)
            })
            .await
            .map_err(|error| HelmCliError::Output(error.to_string()));
        }
        UpgradeChart::Reference { chart, version } => (chart, version),
    };
    let failed = |error: std::io::Error| HelmCliError::Output(error.to_string());
    let dir = tempfile::Builder::new()
        .prefix("kubach-chart-")
        .tempdir()
        .map_err(failed)?;
    let command = HelmCommand::pull(chart, version.as_deref(), dir.path());
    run(helm, &command, timeout).await?;
    let path = std::fs::read_dir(dir.path())
        .map_err(failed)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|extension| extension == "tgz"))
        .ok_or_else(|| HelmCliError::Output("helm pull saved no chart".into()))?;
    crate::runtime::blocking(move || {
        let _dir = dir;
        super::lookup::archive_templates(&std::fs::read(path)?)
    })
    .await
    .map_err(failed)
}

/// A new directory only its owner can enter (`0700`), removed with what it
/// holds when dropped: on an error, a cancelled call or a panic too. For a
/// chart fetched once ([`pull_chart`]) and for helm to run in
/// ([`HelmCommand::working_dir`]).
pub fn private_dir() -> std::io::Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("kubach-helm-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder.tempdir()
}

/// A chart archive fetched once ([`pull_chart`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PulledChart {
    /// Absolute path of the archive.
    pub archive: PathBuf,
    /// SHA-256 of the archive, in hex.
    pub sha256: String,
}

impl PulledChart {
    /// The archive for `helm upgrade` and `helm template`.
    pub fn chart(&self) -> UpgradeChart {
        UpgradeChart::Archive(self.archive.clone())
    }

    /// Whether the archive still is what was fetched.
    pub async fn unchanged(&self) -> bool {
        let path = self.archive.clone();
        let digest = crate::runtime::blocking(move || archive_digest(&path)).await;
        digest.is_ok_and(|digest| digest == self.sha256)
    }
}

fn archive_digest(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest as _, Sha256};
    let digest = Sha256::digest(std::fs::read(path)?);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Fetches `chart` once with `helm pull` into `destination`, which must be
/// an empty directory of the caller ([`private_dir`]). The calls that
/// render and install it then name the archive by its absolute path, so
/// they all work on what was fetched here, whatever the registry or the
/// repository serves later. helm runs in `working_dir`.
pub async fn pull_chart(
    helm: Option<&Path>,
    chart: &str,
    version: Option<&str>,
    destination: &Path,
    working_dir: Option<&Path>,
    timeout: Duration,
) -> Result<PulledChart, HelmCliError> {
    let failed = |error: std::io::Error| HelmCliError::Output(error.to_string());
    let destination = std::path::absolute(destination).map_err(failed)?;
    let command = HelmCommand::pull(chart, version, &destination).in_dir(working_dir);
    run(helm, &command, timeout).await?;
    let mut archives = std::fs::read_dir(&destination)
        .map_err(failed)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "tgz"));
    let (Some(archive), None) = (archives.next(), archives.next()) else {
        return Err(HelmCliError::Output(
            "helm pull did not save one chart archive".into(),
        ));
    };
    let path = archive.clone();
    let sha256 = crate::runtime::blocking(move || archive_digest(&path))
        .await
        .map_err(failed)?;
    Ok(PulledChart { archive, sha256 })
}

/// Runs `command`, with `--debug` and its phases when watched.
async fn run_progress(
    helm: Option<&Path>,
    command: HelmCommand,
    timeout: Duration,
    watch: Option<HelmWatch>,
) -> Result<String, HelmCliError> {
    match watch {
        Some(watch) => run_watched(helm, &command.with_progress(), timeout, watch).await,
        None => run(helm, &command, timeout).await,
    }
}

/// Runs `helm install` for `request` on `target`; with `dry_run` returns
/// the release Helm would write, without changing anything.
pub async fn install(
    helm: Option<&Path>,
    target: &HelmTarget,
    request: &InstallRequest,
    dry_run: bool,
    timeout: Duration,
) -> Result<UpgradeOutcome, HelmCliError> {
    install_watched(helm, target, request, dry_run, timeout, None).await
}

/// Like [`install`]; with `watch` helm runs with `--debug` and reports its
/// phases (see [`run_watched`]).
pub async fn install_watched(
    helm: Option<&Path>,
    target: &HelmTarget,
    request: &InstallRequest,
    dry_run: bool,
    timeout: Duration,
    watch: Option<HelmWatch>,
) -> Result<UpgradeOutcome, HelmCliError> {
    let command = HelmCommand::install(target, request, dry_run);
    outcome(run_progress(helm, command, timeout, watch).await?, dry_run)
}

/// helm's output as applied, or as the previewed release of a dry run.
fn outcome(output: String, dry_run: bool) -> Result<UpgradeOutcome, HelmCliError> {
    if !dry_run {
        return Ok(UpgradeOutcome::Applied(output));
    }
    // Anything printed before the JSON (a plugin's banner) is skipped.
    let json = output.find('{').map_or("", |start| &output[start..]);
    let release = release_from_json(json.as_bytes())
        .map_err(|error| HelmCliError::Output(error.to_string()))?;
    Ok(UpgradeOutcome::Preview(Box::new(release)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(kubeconfig: &[&str]) -> HelmTarget {
        HelmTarget {
            context: "minikube".into(),
            kubeconfig: kubeconfig.iter().map(PathBuf::from).collect(),
            namespace: "apps".into(),
            driver: Driver::Secret,
            secret_env: SecretEnv::default(),
            memory: None,
            working_dir: None,
        }
    }

    fn args(command: &HelmCommand) -> Vec<String> {
        command
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn rollback_pins_context_namespace_kubeconfig_and_driver() {
        let command = HelmCommand::rollback(&target(&["/home/me/.kube/config"]), "web", 2);
        assert_eq!(
            args(&command),
            [
                "rollback",
                "--kube-context",
                "minikube",
                "--namespace",
                "apps",
                "--kubeconfig",
                "/home/me/.kube/config",
                "--",
                "web",
                "2",
            ]
        );
        assert_eq!(command.env, [("HELM_DRIVER", Some("secret".into()))]);
        assert!(
            command
                .display()
                .starts_with("helm rollback --kube-context")
        );
    }

    #[test]
    fn several_kubeconfigs_go_through_the_environment() {
        let mut target = target(&["/a/config", "/b/config"]);
        target.driver = Driver::ConfigMap;
        let command = HelmCommand::rollback(&target, "web", 1);
        assert!(!args(&command).contains(&"--kubeconfig".to_owned()));
        let joined = std::env::join_paths(["/a/config", "/b/config"]).unwrap();
        assert_eq!(
            command.env,
            [
                ("HELM_DRIVER", Some("configmap".into())),
                ("KUBECONFIG", Some(joined)),
            ]
        );
    }

    #[test]
    fn no_kubeconfig_keeps_helm_default() {
        let command = HelmCommand::rollback(&target(&[]), "web", 1);
        assert!(!args(&command).contains(&"--kubeconfig".to_owned()));
        assert_eq!(command.env.len(), 1);
    }

    #[test]
    fn extra_kubeconfigs_follow_the_base_ones() {
        let extra = PathBuf::from("/extra/kubeconfig");
        let files = kubeconfig_files(std::slice::from_ref(&extra));
        assert_eq!(files.last(), Some(&extra));
        assert!(files.len() >= 2 || std::env::home_dir().is_none());
    }

    #[test]
    fn failure_message_prefers_stderr() {
        assert_eq!(
            failure_message(Some(1), "Error: release: not found\n"),
            "Error: release: not found"
        );
        assert_eq!(failure_message(Some(2), " "), "helm exited with code 2");
        assert_eq!(failure_message(None, ""), "helm was terminated by a signal");
    }

    #[tokio::test]
    async fn missing_helm_is_reported() {
        let command = HelmCommand::rollback(&target(&[]), "web", 1);
        let error = run(None, &command, DEFAULT_TIMEOUT).await.unwrap_err();
        assert!(matches!(error, HelmCliError::NotInstalled));
        let error = run(
            Some(Path::new("/nonexistent/kubach-helm")),
            &command,
            DEFAULT_TIMEOUT,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, HelmCliError::Spawn { .. }));
    }

    /// A fake `helm` script that echoes its arguments and `HELM_DRIVER`.
    #[cfg(unix)]
    fn fake_helm(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join("helm");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runs_helm_with_arguments_and_environment() {
        let dir = tempfile::tempdir().unwrap();
        let helm = fake_helm(dir.path(), r#"echo "$HELM_DRIVER $*""#);
        let command = HelmCommand::rollback(&target(&["/k"]), "web", 3);
        let output = run(Some(&helm), &command, DEFAULT_TIMEOUT).await.unwrap();
        assert_eq!(
            output.trim(),
            "secret rollback --kube-context minikube --namespace apps --kubeconfig /k -- web 3"
        );
    }

    /// A kubeconfig file with context `name` on `server`.
    #[cfg(all(test, unix))]
    fn kubeconfig_yaml(name: &str, server: &str, token: &str) -> String {
        format!(
            "apiVersion: v1\nkind: Config\ncurrent-context: {name}\n\
             clusters:\n  - name: {name}-cluster\n    cluster: {{ server: \"{server}\" }}\n\
             users:\n  - name: {name}-user\n    user: {{ token: {token} }}\n\
             contexts:\n  - name: {name}\n    context: {{ cluster: {name}-cluster, user: {name}-user }}\n"
        )
    }

    /// Contexts as Kubach loads them: the linked `file`, then a vault with
    /// `prod` (and `spare`, which no call asks for); a locked vault gives
    /// only the names.
    #[cfg(all(test, unix))]
    fn contexts_with_vault(file: &Path, unlocked: bool) -> Contexts {
        use crate::kubeconfig::{FileProblem, Source};
        use kube::config::Kubeconfig;
        let config = Kubeconfig::from_yaml(&kubeconfig_yaml(
            "prod",
            "https://vault.example.com",
            "vault-token",
        ))
        .unwrap()
        .merge(
            Kubeconfig::from_yaml(&kubeconfig_yaml(
                "spare",
                "https://spare.example.com",
                "spare-token",
            ))
            .unwrap(),
        )
        .unwrap();
        let imported = crate::vault::Imported {
            source: Source::Imported {
                vault: file.with_file_name("vault.json"),
                id: "1".into(),
                name: "work".into(),
            },
            config: if unlocked {
                Ok(config)
            } else {
                Err(FileProblem::Locked)
            },
            contexts: vec!["prod".into(), "spare".into()],
        };
        Contexts::load_all(vec![Source::Linked(file.to_owned())], vec![imported]).unwrap()
    }

    /// A fake `helm` that writes what it was given to `log`: its arguments,
    /// `KUBECONFIG`, the modes of the `--kubeconfig` file and its folder,
    /// then the file itself. `after` runs last.
    #[cfg(all(test, unix))]
    fn recording_helm(dir: &Path, log: &Path, after: &str) -> PathBuf {
        let body = format!(
            r#"previous=; kubeconfig=
for argument in "$@"; do
  [ "$previous" = --kubeconfig ] && kubeconfig="$argument"
  previous="$argument"
done
{{
  printf 'args=%s\n' "$*"
  printf 'env=%s\n' "$KUBECONFIG"
  printf 'file=%s\n' "$kubeconfig"
  printf 'modes=%s %s\n' "$(ls -l "$kubeconfig" | cut -c1-10)" \
    "$(ls -ld "$(dirname "$kubeconfig")" | cut -c1-10)"
  cat "$kubeconfig"
}} > '{}'
{after}"#,
            log.display()
        );
        fake_helm(dir, &body)
    }

    /// The target the screens build for `context` (the namespace aside).
    #[cfg(all(test, unix))]
    fn screen_target(contexts: &Contexts, context: &str, file: &Path, temp: &Path) -> HelmTarget {
        let (kubeconfig, memory) =
            kubeconfig_of(contexts, context, &[file.to_owned()], Some(temp.to_owned())).unwrap();
        HelmTarget {
            context: context.into(),
            kubeconfig,
            memory,
            ..target(&[])
        }
    }

    #[cfg(all(test, unix))]
    fn files_in(dir: &Path) -> usize {
        std::fs::read_dir(dir).map_or(0, Iterator::count)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_vault_context_reaches_helm_alone_in_a_file_of_its_own() {
        use kube::config::Kubeconfig;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config");
        std::fs::write(
            &file,
            kubeconfig_yaml("dev", "https://dev.example.com", "dev-token"),
        )
        .unwrap();
        let temp = dir.path().join("run").join("kubeconfig");
        let contexts = contexts_with_vault(&file, true);
        let target = screen_target(&contexts, "prod", &file, &temp);
        assert!(target.kubeconfig.is_empty());
        // The file gets a context of the same name on another server after
        // Kubach read it: helm must not see it.
        std::fs::write(
            &file,
            kubeconfig_yaml("prod", "https://elsewhere.example.com", "file-token"),
        )
        .unwrap();

        let command = HelmCommand::rollback(&target, "web", 3);
        // The line shown to the user names no file: there is none yet.
        assert_eq!(
            command.display(),
            "helm rollback --kube-context prod --namespace apps -- web 3"
        );
        for name in CONNECTION_VARS {
            assert!(command.env.contains(&(*name, None)), "{name}");
        }
        assert!(!command.env.iter().any(|(name, _)| *name == "KUBECONFIG"));

        let log = dir.path().join("log");
        let helm = recording_helm(dir.path(), &log, "");
        run(Some(&helm), &command, DEFAULT_TIMEOUT).await.unwrap();
        let said = std::fs::read_to_string(&log).unwrap();
        let mut lines = said.lines();
        let args = lines.next().unwrap();
        let env = lines.next().unwrap().strip_prefix("env=").unwrap();
        let path = lines.next().unwrap().strip_prefix("file=").unwrap();
        let modes = lines.next().unwrap();
        let yaml: String = lines.map(|line| format!("{line}\n")).collect();

        assert_eq!(
            args,
            format!(
                "args=rollback --kube-context prod --namespace apps --kubeconfig {path} -- web 3"
            )
        );
        // The flag and the variable name the same file, in Kubach's folder.
        assert_eq!(env, path);
        assert_eq!(Path::new(path).parent(), Some(temp.as_path()));
        assert_eq!(modes, "modes=-rw------- drwx------");
        // That one context, with its own server and credentials.
        let seen = Kubeconfig::from_yaml(&yaml).unwrap();
        assert_eq!(seen.current_context.as_deref(), Some("prod"));
        assert_eq!(seen.contexts.len(), 1);
        assert_eq!(seen.clusters.len(), 1);
        assert_eq!(seen.auth_infos.len(), 1);
        assert!(yaml.contains("https://vault.example.com"));
        assert!(yaml.contains("vault-token"));
        for other in ["elsewhere.example.com", "file-token", "spare", "dev"] {
            assert!(!yaml.contains(other), "{other}");
        }
        // Gone with the process.
        assert_eq!(files_in(&temp), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_file_of_a_vault_context_is_removed_however_helm_ends() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config");
        std::fs::write(
            &file,
            kubeconfig_yaml("dev", "https://dev.example.com", "dev-token"),
        )
        .unwrap();
        let temp = dir.path().join("run").join("kubeconfig");
        let contexts = contexts_with_vault(&file, true);
        let target = screen_target(&contexts, "prod", &file, &temp);
        let command = HelmCommand::uninstall(&target, "web", false);
        let log = dir.path().join("log");

        // helm fails.
        let failing = recording_helm(dir.path(), &log, "echo 'Error: no' >&2; exit 1");
        let error = run(Some(&failing), &command, DEFAULT_TIMEOUT)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "Error: no");
        assert!(
            std::fs::read_to_string(&log)
                .unwrap()
                .contains("vault-token")
        );
        assert_eq!(files_in(&temp), 0);

        // helm is stopped for running too long.
        std::fs::remove_file(&log).unwrap();
        let slow = recording_helm(dir.path(), &log, "sleep 5");
        let error = run(Some(&slow), &command, Duration::from_millis(500))
            .await
            .unwrap_err();
        assert!(matches!(error, HelmCliError::Timeout(_)));
        assert_eq!(files_in(&temp), 0);

        // The call is dropped while helm runs.
        std::fs::remove_file(&log).ok();
        let call = tokio::spawn({
            let (slow, command) = (slow.clone(), command.clone());
            async move { run(Some(&slow), &command, DEFAULT_TIMEOUT).await }
        });
        for _ in 0..200 {
            if log.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(files_in(&temp), 1);
        call.abort();
        let _ = call.await;
        assert_eq!(files_in(&temp), 0);

        // An upgrade and an install go the same way, each with its own file.
        let done = recording_helm(dir.path(), &log, "cat > /dev/null; echo done");
        let outcome = upgrade(
            Some(&done),
            &target,
            &request(UpgradeChart::Reference {
                chart: "repo/web".into(),
                version: None,
            }),
            false,
            DEFAULT_TIMEOUT,
        )
        .await
        .unwrap();
        assert!(matches!(outcome, UpgradeOutcome::Applied(_)));
        assert!(
            std::fs::read_to_string(&log)
                .unwrap()
                .contains("vault-token")
        );
        std::fs::remove_file(&log).unwrap();
        install(
            Some(&done),
            &target,
            &install_request(),
            false,
            DEFAULT_TIMEOUT,
        )
        .await
        .unwrap();
        assert!(
            std::fs::read_to_string(&log)
                .unwrap()
                .contains("vault-token")
        );
        assert_eq!(files_in(&temp), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn helm_does_not_start_without_the_file_of_a_vault_context() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config");
        std::fs::write(
            &file,
            kubeconfig_yaml("prod", "https://elsewhere.example.com", "file-token"),
        )
        .unwrap();
        // Nothing can be created under a file.
        let temp = file.join("kubeconfig");
        let contexts = contexts_with_vault(&dir.path().join("missing"), true);
        let target = screen_target(&contexts, "prod", &file, &temp);
        let log = dir.path().join("log");
        let helm = recording_helm(dir.path(), &log, "");
        let error = run(
            Some(&helm),
            &HelmCommand::rollback(&target, "web", 1),
            DEFAULT_TIMEOUT,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, HelmCliError::Kubeconfig(_)), "{error}");
        assert!(!error.to_string().contains("token"));
        assert!(!log.exists());
    }

    #[cfg(unix)]
    #[test]
    fn only_a_loaded_vault_context_is_given_from_memory() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config");
        std::fs::write(
            &file,
            kubeconfig_yaml("dev", "https://dev.example.com", "dev-token"),
        )
        .unwrap();
        let linked = [file.clone()];
        let temp = || Some(dir.path().join("run"));
        let contexts = contexts_with_vault(&file, true);
        // A context of a file: the files, as before.
        let (files, memory) = kubeconfig_of(&contexts, "dev", &linked, temp()).unwrap();
        assert_eq!(files, kubeconfig_files(&linked));
        assert!(memory.is_none());
        // A context of the vault: never the files.
        let (files, memory) = kubeconfig_of(&contexts, "prod", &linked, temp()).unwrap();
        assert!(files.is_empty());
        assert!(memory.is_some());
        assert!(!format!("{memory:?}").contains("token"));
        // No place for the file, or the vault is locked: nothing for helm.
        assert!(kubeconfig_of(&contexts, "prod", &linked, None).is_none());
        let locked = contexts_with_vault(&file, false);
        assert!(kubeconfig_of(&locked, "prod", &linked, temp()).is_none());
        assert!(kubeconfig_of(&locked, "dev", &linked, temp()).is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reports_failures_and_timeouts() {
        let dir = tempfile::tempdir().unwrap();
        let failing = fake_helm(dir.path(), "echo 'Error: no revision 9' >&2; exit 1");
        let command = HelmCommand::rollback(&target(&[]), "web", 9);
        let error = run(Some(&failing), &command, DEFAULT_TIMEOUT)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "Error: no revision 9");

        let slow_dir = tempfile::tempdir().unwrap();
        let slow = fake_helm(slow_dir.path(), "sleep 5");
        let error = run(Some(&slow), &command, Duration::from_millis(100))
            .await
            .unwrap_err();
        assert!(matches!(error, HelmCliError::Timeout(_)));
    }

    #[test]
    fn uninstall_keeps_history_only_when_asked() {
        let command = HelmCommand::uninstall(&target(&[]), "web", false);
        assert_eq!(
            args(&command),
            [
                "uninstall",
                "--kube-context",
                "minikube",
                "--namespace",
                "apps",
                "--",
                "web",
            ]
        );
        assert_eq!(command.stdin, None);
        let command = HelmCommand::uninstall(&target(&[]), "web", true);
        assert_eq!(args(&command)[..2], ["uninstall", "--keep-history"]);
        assert_eq!(args(&command).last().unwrap(), "web");
    }

    fn stored_chart() -> StoredChart {
        use base64::Engine as _;
        let data = base64::engine::general_purpose::STANDARD.encode("kind: ConfigMap\n");
        StoredChart::from_json(serde_json::json!({
            "metadata": {"apiVersion": "v2", "name": "web", "version": "1.2.0"},
            "values": {"replicaCount": 1},
            "templates": [{"name": "templates/cm.yaml", "data": data}]
        }))
        .unwrap()
    }

    fn request(chart: UpgradeChart) -> UpgradeRequest {
        UpgradeRequest {
            release: "web".into(),
            chart,
            values: Sensitive::new("replicaCount: 3\n".into()),
        }
    }

    #[test]
    fn upgrade_passes_values_on_stdin() {
        let reference = request(UpgradeChart::Reference {
            chart: " bitnami/nginx ".into(),
            version: Some("18.1.0".into()),
        });
        let command = HelmCommand::upgrade(&target(&[]), &reference, None, false);
        assert_eq!(
            args(&command),
            [
                "upgrade",
                "--version",
                "18.1.0",
                "--reset-values",
                "--values",
                "-",
                "--kube-context",
                "minikube",
                "--namespace",
                "apps",
                "--",
                "web",
                "bitnami/nginx",
            ]
        );
        assert_eq!(
            command.stdin.as_ref().map(|s| s.expose().as_str()),
            Some("replicaCount: 3\n")
        );
        assert!(!command.display().contains("replicaCount"));

        let preview = HelmCommand::upgrade(&target(&[]), &reference, None, true);
        assert!(
            preview
                .display()
                .contains("--values - --dry-run=server --output json --kube-context")
        );

        let unversioned = request(UpgradeChart::Reference {
            chart: "./web".into(),
            version: Some(" ".into()),
        });
        let command = HelmCommand::upgrade(&target(&[]), &unversioned, None, false);
        assert!(!args(&command).contains(&"--version".to_owned()));

        let stored = request(UpgradeChart::Stored(stored_chart()));
        let shown = HelmCommand::upgrade(&target(&[]), &stored, None, false);
        assert_eq!(args(&shown).last().unwrap(), "<stored web-1.2.0>");
        let real = HelmCommand::upgrade(&target(&[]), &stored, Some(Path::new("/tmp/x")), false);
        assert_eq!(args(&real).last().unwrap(), "/tmp/x");
    }

    #[test]
    fn an_archive_is_named_by_its_path_and_helm_runs_where_told() {
        let mut target = target(&[]);
        target.working_dir = Some("/work/cwd".into());
        let request = UpgradeRequest {
            release: "web".into(),
            chart: UpgradeChart::Archive("/work/chart/web-1.2.0.tgz".into()),
            values: Sensitive::new(String::new()),
        };
        let command = HelmCommand::upgrade(&target, &request, None, true);
        assert_eq!(args(&command).last().unwrap(), "/work/chart/web-1.2.0.tgz");
        assert!(!args(&command).iter().any(|arg| arg == "--version"));
        assert_eq!(command.working_dir.as_deref(), Some(Path::new("/work/cwd")));
        let shown = HelmCommand::show(ShowKind::Values, "repo/web", None)
            .in_dir(Some(Path::new("/work/cwd")));
        assert_eq!(shown.working_dir.as_deref(), Some(Path::new("/work/cwd")));
    }

    #[cfg(unix)]
    #[test]
    fn a_private_directory_is_the_owners_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = private_dir().unwrap();
        let mode = std::fs::metadata(dir.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn template_renders_on_the_client_only() {
        let mut request = TemplateRequest {
            release: "web".into(),
            namespace: "apps".into(),
            chart: UpgradeChart::Reference {
                chart: "bitnami/nginx".into(),
                version: Some("18.1.0".into()),
            },
            values: Sensitive::new("replicaCount: 2\n".into()),
            is_upgrade: false,
            working_dir: None,
        };
        let command = HelmCommand::template(&request, None);
        assert_eq!(
            args(&command),
            [
                "template",
                "--version",
                "18.1.0",
                "--values",
                "-",
                "--namespace",
                "apps",
                "--include-crds",
                "--",
                "web",
                "bitnami/nginx",
            ]
        );
        // No context, kubeconfig or credentials: nothing reaches a cluster.
        assert!(command.env.is_empty());
        assert!(command.secret_env.expose().is_empty());
        assert_eq!(
            command.stdin.as_ref().unwrap().expose(),
            "replicaCount: 2\n"
        );
        request.chart = UpgradeChart::Stored(stored_chart());
        request.is_upgrade = true;
        let stored = HelmCommand::template(&request, Some(Path::new("/tmp/x")));
        let shown = args(&stored);
        assert!(shown.contains(&"--is-upgrade".to_owned()));
        assert!(!shown.contains(&"--include-crds".to_owned()));
        assert_eq!(shown.last().unwrap(), "/tmp/x");
        request.release = "--kube-apiserver=x".into();
        assert!(HelmCommand::template(&request, None).refusal.is_some());
    }

    #[test]
    fn template_on_a_cluster_is_pinned_to_the_target() {
        let request = TemplateRequest {
            release: "web".into(),
            namespace: "apps".into(),
            chart: UpgradeChart::Reference {
                chart: "bitnami/nginx".into(),
                version: None,
            },
            values: Sensitive::new("a: 1\n".into()),
            is_upgrade: true,
            working_dir: None,
        };
        let command = HelmCommand::template_on_cluster(&target(&["/k"]), &request, None);
        assert_eq!(
            args(&command),
            [
                "template",
                "--values",
                "-",
                "--is-upgrade",
                "--dry-run=server",
                "--kube-context",
                "minikube",
                "--namespace",
                "apps",
                "--kubeconfig",
                "/k",
                "--",
                "web",
                "bitnami/nginx",
            ]
        );
        assert_eq!(command.stdin.as_ref().unwrap().expose(), "a: 1\n");
    }

    #[tokio::test]
    async fn a_stored_chart_gives_its_templates_without_helm() {
        let chart = StoredChart::from_json(serde_json::json!({
            "metadata": {"name": "web", "version": "1.0.0"},
            "templates": [{"name": "templates/a.yaml", "data": "a2luZDogQQ=="}],
        }))
        .unwrap();
        let files = chart_templates(None, &UpgradeChart::Stored(chart), Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(
            files,
            [("web/templates/a.yaml".to_owned(), "kind: A".to_owned())]
        );
    }

    #[test]
    fn pull_needs_no_cluster() {
        let command = HelmCommand::pull(" repo/web ", Some("1.2.3"), Path::new("/tmp/x"));
        assert_eq!(
            args(&command),
            [
                "pull",
                "--version",
                "1.2.3",
                "--destination",
                "/tmp/x",
                "--",
                "repo/web",
            ]
        );
        assert!(command.env.is_empty());
        assert!(command.secret_env.expose().is_empty());
    }

    fn install_request() -> InstallRequest {
        InstallRequest {
            release: " web ".into(),
            chart: "bitnami/nginx".into(),
            version: Some("18.1.0".into()),
            values: Sensitive::new("replicaCount: 2\n".into()),
            create_namespace: true,
        }
    }

    #[test]
    fn install_passes_values_on_stdin() {
        let command = HelmCommand::install(&target(&[]), &install_request(), false);
        assert_eq!(
            args(&command),
            [
                "install",
                "--version",
                "18.1.0",
                "--values",
                "-",
                "--create-namespace",
                "--kube-context",
                "minikube",
                "--namespace",
                "apps",
                "--",
                "web",
                "bitnami/nginx",
            ]
        );
        assert_eq!(
            command.stdin.as_ref().map(|s| s.expose().as_str()),
            Some("replicaCount: 2\n")
        );
        assert!(!command.display().contains("replicaCount"));
        assert_eq!(command.env, [("HELM_DRIVER", Some("secret".into()))]);

        let mut oci = install_request();
        oci.chart = "oci://registry.example/charts/web".into();
        oci.version = None;
        oci.create_namespace = false;
        let preview = HelmCommand::install(&target(&[]), &oci, true);
        assert_eq!(
            preview.display(),
            "helm install --values - --dry-run=server --output json \
             --kube-context minikube --namespace apps -- web oci://registry.example/charts/web"
        );
    }

    #[test]
    fn repository_and_show_commands_need_no_cluster() {
        let add = HelmCommand::repo_add(" stable ", " https://charts.example ", None);
        assert_eq!(
            args(&add),
            ["repo", "add", "--", "stable", "https://charts.example"]
        );
        assert!(add.env.is_empty() && add.stdin.is_none());
        let credentials = RepoCredentials {
            username: " me ".into(),
            password: Sensitive::new("s3cret".into()),
        };
        let add = HelmCommand::repo_add("private", "https://charts.example", Some(&credentials));
        assert_eq!(
            args(&add),
            [
                "repo",
                "add",
                "--username",
                "me",
                "--password-stdin",
                "--",
                "private",
                "https://charts.example",
            ]
        );
        assert!(add.env.is_empty());
        assert_eq!(add.stdin.as_ref().unwrap().expose(), "s3cret");
        // The password is in neither the arguments, the shown line nor the
        // debug output that may end up in logs.
        assert!(!add.display().contains("s3cret"));
        assert!(!format!("{add:?}").contains("s3cret"));
        assert_eq!(args(&HelmCommand::repo_update(&[])), ["repo", "update"]);
        assert_eq!(
            args(&HelmCommand::repo_update(&["a", "b"])),
            ["repo", "update", "--", "a", "b"]
        );
        assert_eq!(
            args(&HelmCommand::repo_remove("stable")),
            ["repo", "remove", "--", "stable"]
        );
        assert_eq!(
            HelmCommand::show(ShowKind::Values, "stable/web", Some("1.0.0")).display(),
            "helm show values --version 1.0.0 -- stable/web"
        );
        assert_eq!(
            HelmCommand::show(ShowKind::Readme, "oci://r/web", Some(" ")).display(),
            "helm show readme -- oci://r/web"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn install_preview_parses_the_release() {
        let dir = tempfile::tempdir().unwrap();
        let helm = fake_helm(
            dir.path(),
            r#"while [ "$1" != "--" ]; do shift; done
values=$(cat)
printf '{"name":"%s","version":1,"info":{"status":"pending-install"},"manifest":"%s"}\n' "$2" "$values""#,
        );
        let outcome = install(
            Some(&helm),
            &target(&[]),
            &install_request(),
            true,
            DEFAULT_TIMEOUT,
        )
        .await
        .unwrap();
        let UpgradeOutcome::Preview(release) = outcome else {
            panic!("expected a preview");
        };
        assert_eq!(release.name, "web");
        assert_eq!(release.revision, 1);
        assert_eq!(release.manifest.expose(), "replicaCount: 2");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn upgrade_preview_renders_the_stored_chart() {
        let dir = tempfile::tempdir().unwrap();
        // Checks the chart directory and stdin, then prints release JSON
        // with the values it got as the manifest.
        let helm = fake_helm(
            dir.path(),
            r#"for arg; do chart="$arg"; done
test -f "$chart/Chart.yaml" && test -f "$chart/templates/cm.yaml" || exit 3
echo "$chart" > "$(dirname "$0")/chart-path"
values=$(cat)
echo 'banner'
printf '{"name":"web","version":4,"info":{"status":"pending-upgrade"},"manifest":"%s"}\n' "$values""#,
        );
        let request = request(UpgradeChart::Stored(stored_chart()));
        let outcome = upgrade(Some(&helm), &target(&[]), &request, true, DEFAULT_TIMEOUT)
            .await
            .unwrap();
        let UpgradeOutcome::Preview(release) = outcome else {
            panic!("expected a preview");
        };
        assert_eq!(release.revision, 4);
        assert_eq!(release.manifest.expose(), "replicaCount: 3");
        // The temporary chart is gone once helm is done.
        let chart = std::fs::read_to_string(dir.path().join("chart-path")).unwrap();
        assert!(!Path::new(chart.trim()).exists(), "{chart}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn upgrade_reports_unusable_charts_and_bad_output() {
        let dir = tempfile::tempdir().unwrap();
        let helm = fake_helm(dir.path(), "cat > /dev/null; echo 'not json'");
        let with_deps = StoredChart::from_json(serde_json::json!({
            "metadata": {"name": "web", "version": "1", "dependencies": [{"name": "redis"}]}
        }))
        .unwrap();
        let error = upgrade(
            Some(&helm),
            &target(&[]),
            &request(UpgradeChart::Stored(with_deps)),
            true,
            DEFAULT_TIMEOUT,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, HelmCliError::StoredChart(_)), "{error}");

        let reference = request(UpgradeChart::Reference {
            chart: "repo/web".into(),
            version: None,
        });
        let error = upgrade(Some(&helm), &target(&[]), &reference, true, DEFAULT_TIMEOUT)
            .await
            .unwrap_err();
        assert!(matches!(error, HelmCliError::Output(_)), "{error}");
        let applied = upgrade(
            Some(&helm),
            &target(&[]),
            &reference,
            false,
            DEFAULT_TIMEOUT,
        )
        .await
        .unwrap();
        assert!(matches!(applied, UpgradeOutcome::Applied(out) if out.trim() == "not json"));
    }

    #[test]
    fn progress_adds_debug_before_the_names() {
        let command = HelmCommand::rollback(&target(&[]), "web", 2).with_progress();
        let shown = args(&command);
        let dash = shown.iter().position(|arg| arg == "--").unwrap();
        assert_eq!(shown[dash - 1], "--debug");
        assert_eq!(shown[dash + 1..], ["web", "2"]);
        // Once only.
        let again = command.clone().with_progress();
        assert_eq!(again.args, command.args);
        let local = HelmCommand::repo_update(&[]).with_progress();
        assert_eq!(args(&local), ["repo", "update", "--debug"]);
    }

    /// Phases come from the `--debug` log on stderr while helm waits; the
    /// values `--debug` prints are not in the result (#382).
    #[cfg(unix)]
    #[tokio::test]
    async fn watched_install_reports_phases_and_hides_values() {
        use std::sync::Mutex;

        let dir = tempfile::tempdir().unwrap();
        let helm = fake_helm(
            dir.path(),
            r#"case " $* " in *" --debug "*) ;; *) exit 9;; esac
cat > /dev/null
echo 'level=DEBUG msg="waiting for resource" name=web-check kind=Job actualStatus=InProgress' >&2
printf 'NAME: web\nUSER-SUPPLIED VALUES:\npassword: s3cret\nMANIFEST:\nkind: Secret\nNOTES:\nHi\n'"#,
        );
        let phases: Arc<Mutex<Vec<String>>> = Arc::default();
        let sink = phases.clone();
        let watch = HelmWatch {
            on_phase: Some(Arc::new(move |phase| sink.lock().unwrap().push(phase))),
            cancel: None,
        };
        let outcome = install_watched(
            Some(&helm),
            &target(&[]),
            &install_request(),
            false,
            DEFAULT_TIMEOUT,
            Some(watch),
        )
        .await
        .unwrap();
        let UpgradeOutcome::Applied(output) = outcome else {
            panic!("expected applied");
        };
        assert_eq!(output, "NAME: web\nNOTES:\nHi\n");
        assert_eq!(
            *phases.lock().unwrap(),
            ["Waiting for Job web-check: InProgress"]
        );
    }

    /// Cancel sends SIGTERM; helm's own words end up in the error.
    #[cfg(unix)]
    #[tokio::test]
    async fn watched_call_can_be_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let helm = fake_helm(
            dir.path(),
            r#"trap 'echo "Error: UPGRADE FAILED: context canceled" >&2; exit 1' TERM
echo 'level=DEBUG msg="waiting for resources" count=1' >&2
while :; do sleep 0.1; done"#,
        );
        let cancel = Cancel::new();
        let trigger = cancel.clone();
        let watch = HelmWatch {
            on_phase: Some(Arc::new(move |_| trigger.cancel())),
            cancel: Some(cancel),
        };
        let command = HelmCommand::rollback(&target(&[]), "web", 1).with_progress();
        let error = run_watched(Some(&helm), &command, DEFAULT_TIMEOUT, watch)
            .await
            .unwrap_err();
        assert!(
            matches!(error, HelmCliError::Cancelled { killed: false, .. }),
            "{error}"
        );
        assert_eq!(
            error.to_string(),
            "Stopped: Error: UPGRADE FAILED: context canceled"
        );
    }

    #[test]
    fn release_names_follow_helm_rules() {
        for name in [
            "web",
            "my-app",
            "a.b-c",
            "x1",
            &"a".repeat(MAX_RELEASE_NAME),
        ] {
            assert_eq!(check_release_name(name), Ok(()), "{name}");
        }
        for name in [
            "",
            "Web",
            "-web",
            "web-",
            "a..b",
            ".web",
            "web_1",
            "web 1",
            "--kube-apiserver=http://evil",
            &"a".repeat(MAX_RELEASE_NAME + 1),
        ] {
            assert!(check_release_name(name).is_err(), "{name}");
        }
    }

    #[test]
    fn positional_arguments_follow_double_dash() {
        // A name starting with `-` stays after `--`, behind every flag.
        let evil = "--kube-apiserver=http://evil";
        let command = HelmCommand::uninstall(&target(&["/k"]), evil, false);
        let args = args(&command);
        let dash = args.iter().position(|arg| arg == "--").unwrap();
        assert_eq!(args[dash + 1..], [evil]);
        assert!(command.refusal().is_some());
        assert_eq!(HelmCommand::repo_update(&[]).args, ["repo", "update"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn invalid_release_is_refused_before_helm_starts() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran");
        let helm = fake_helm(dir.path(), &format!("touch '{}'", marker.display()));
        let evil = "--kube-apiserver=http://evil";
        for command in [
            HelmCommand::rollback(&target(&[]), evil, 1),
            HelmCommand::uninstall(&target(&[]), "Web", true),
        ] {
            let error = run(Some(&helm), &command, DEFAULT_TIMEOUT)
                .await
                .unwrap_err();
            assert!(matches!(error, HelmCliError::Refused(_)), "{error}");
        }
        let mut bad_install = install_request();
        bad_install.release = "web_1".into();
        let error = install(
            Some(&helm),
            &target(&[]),
            &bad_install,
            false,
            DEFAULT_TIMEOUT,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, HelmCliError::Refused(_)), "{error}");
        let mut bad_upgrade = request(UpgradeChart::Stored(stored_chart()));
        bad_upgrade.release = evil.into();
        let error = upgrade(
            Some(&helm),
            &target(&[]),
            &bad_upgrade,
            true,
            DEFAULT_TIMEOUT,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, HelmCliError::Refused(_)), "{error}");
        assert!(!marker.exists(), "helm must not run");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn credentials_for_plain_http_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran");
        let helm = fake_helm(
            dir.path(),
            &format!("cat > /dev/null; touch '{}'", marker.display()),
        );
        let credentials = RepoCredentials {
            username: "me".into(),
            password: Sensitive::new("s3cret".into()),
        };
        let plain = HelmCommand::repo_add("private", " HTTP://charts.example ", Some(&credentials));
        assert!(plain.refusal().is_some());
        let error = run(Some(&helm), &plain, DEFAULT_TIMEOUT).await.unwrap_err();
        assert!(matches!(error, HelmCliError::Refused(_)), "{error}");
        assert!(!marker.exists(), "helm must not run");

        // Without credentials an http repository is still allowed, and
        // https with credentials runs.
        let anonymous = HelmCommand::repo_add("public", "http://charts.example", None);
        assert_eq!(anonymous.refusal(), None);
        let secure = HelmCommand::repo_add("private", "https://charts.example", Some(&credentials));
        assert_eq!(secure.refusal(), None);
        run(Some(&helm), &secure, DEFAULT_TIMEOUT).await.unwrap();
        assert!(marker.exists());
    }
}
