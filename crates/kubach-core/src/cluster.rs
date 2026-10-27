//! Kubeconfig contexts and cluster connections.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use kube::config::{KubeConfigOptions, Kubeconfig, KubeconfigError};
use kube::{Client, Config};

use crate::kubeconfig::{self, FileProblem, FileState, Source};

#[derive(Debug, thiserror::Error)]
pub enum ClusterError {
    #[error("failed to read kubeconfig: {0}")]
    Kubeconfig(#[from] KubeconfigError),
    #[error("{}: {problem}", path.display())]
    File { path: PathBuf, problem: FileProblem },
    #[error("context \"{0}\" not found in kubeconfig")]
    UnknownContext(String),
    #[error("failed to create client: {0}")]
    Client(#[source] kube::Error),
    #[error("cluster is unreachable: {0}")]
    Unreachable(#[source] kube::Error),
}

impl ClusterError {
    /// The error in words for the screen, logs and MCP. Unlike `to_string`,
    /// never shows an exec plugin's arguments, environment or output: only
    /// its program name, exit code and stderr (cut and masked), see
    /// [`crate::redact::error_text`].
    pub fn user_message(&self) -> String {
        match self {
            Self::Client(error) => {
                crate::redact::error_text(Some("failed to create client"), error)
            }
            Self::Unreachable(error) => {
                crate::redact::error_text(Some("cluster is unreachable"), error)
            }
            other => crate::redact::error_text(None, other),
        }
    }

    /// Whether the kubeconfig exec plugin (`aws`, `kubelogin`, …) could not
    /// be started because its command was not found, usually a `PATH` issue.
    pub fn is_exec_not_found(&self) -> bool {
        let mut source: Option<&(dyn std::error::Error + 'static)> = Some(self);
        while let Some(error) = source {
            if let Some(kube::client::AuthError::AuthExecStart(io)) = error.downcast_ref() {
                return io.kind() == std::io::ErrorKind::NotFound;
            }
            source = error.source();
        }
        false
    }
}

/// A kubeconfig context as shown to the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextInfo {
    pub name: String,
    pub cluster: String,
    pub user: Option<String>,
    pub namespace: Option<String>,
    /// API server URL of the context's cluster.
    pub server: Option<String>,
}

/// All contexts from the merged kubeconfig, plus the kubeconfig itself so a
/// connection can be built for any of them.
#[derive(Clone, Debug)]
pub struct Contexts {
    kubeconfig: Kubeconfig,
    contexts: Vec<ContextInfo>,
    /// Every file the contexts were loaded from, readable or not.
    files: Vec<FileState>,
    /// File each context comes from (the first one defining it).
    sources: HashMap<String, Source>,
}

impl Contexts {
    /// Reads `KUBECONFIG` (all listed files, merged by kubectl rules) or
    /// `~/.kube/config`.
    pub fn load() -> Result<Self, ClusterError> {
        Self::load_with_extra(&[])
    }

    /// Like [`load`](Self::load), then merges `extra` files (linked in
    /// settings) after it by the same rules, so the first `current-context`
    /// and the first context of a name win.
    ///
    /// Unreadable files are skipped and reported in [`files`](Self::files).
    /// Fails only when no file could be read and none is linked, i.e. there
    /// is no kubeconfig at all.
    pub fn load_with_extra(extra: &[PathBuf]) -> Result<Self, ClusterError> {
        Self::load_sources(kubeconfig::sources_with(extra))
    }

    /// Reads and merges `sources` in order; see [`load_with_extra`](Self::load_with_extra).
    pub fn load_sources(sources: Vec<Source>) -> Result<Self, ClusterError> {
        Self::load_all(sources, Vec::new())
    }

    /// Like [`load_sources`](Self::load_sources), then merges the configs
    /// `imported` into the vault. Clusters and users of linked files and
    /// imported configs that clash with earlier files are renamed (ADR
    /// 0008), so they never borrow another file's server or credentials;
    /// files from `KUBECONFIG` merge by kubectl rules. Entries of a locked vault give no contexts but report
    /// their context names in [`files`](Self::files).
    pub fn load_all(
        sources: Vec<Source>,
        imported: Vec<crate::vault::Imported>,
    ) -> Result<Self, ClusterError> {
        let any_extra = sources.iter().any(Source::is_linked) || !imported.is_empty();
        let mut merged: Option<Kubeconfig> = None;
        let mut files = Vec::with_capacity(sources.len() + imported.len());
        let mut origins: HashMap<String, Source> = HashMap::new();
        let read_files = sources.into_iter().map(|source| {
            let config = kubeconfig::read(source.path());
            (source, config, None)
        });
        let from_vault = imported
            .into_iter()
            .map(|entry| (entry.source, entry.config, Some(entry.contexts)));
        for (source, config, known) in read_files.chain(from_vault) {
            let read = config.and_then(|mut config| {
                if let (Some(previous), Some(tag)) = (&merged, isolation_tag(&source)) {
                    let clusters = previous.clusters.iter().map(|c| c.name.clone()).collect();
                    let users = previous.auth_infos.iter().map(|u| u.name.clone()).collect();
                    crate::vault::import::isolate_names(&mut config, &clusters, &users, &tag);
                }
                let names: Vec<String> = config.contexts.iter().map(|c| c.name.clone()).collect();
                let next = match &merged {
                    Some(previous) => previous
                        .clone()
                        .merge(config)
                        .map_err(|error| FileProblem::Invalid(error.to_string()))?,
                    None => config,
                };
                Ok((next, names))
            });
            let state = match read {
                Ok((next, names)) => {
                    merged = Some(next);
                    for name in &names {
                        origins
                            .entry(name.clone())
                            .or_insert_with(|| source.clone());
                    }
                    FileState {
                        loose_mode: (!source.is_imported())
                            .then(|| kubeconfig::loose_permissions(source.path()))
                            .flatten(),
                        source,
                        problem: None,
                        contexts: names,
                    }
                }
                Err(problem) => FileState {
                    source,
                    contexts: match problem {
                        FileProblem::Locked => known.unwrap_or_default(),
                        _ => Vec::new(),
                    },
                    problem: Some(problem),
                    loose_mode: None,
                },
            };
            files.push(state);
        }
        let config = match merged {
            Some(config) => config,
            None if any_extra => Kubeconfig::default(),
            None => {
                return Err(match files.into_iter().next() {
                    Some(file) => ClusterError::File {
                        path: file.source.path().to_owned(),
                        problem: file.problem.unwrap_or(FileProblem::Missing),
                    },
                    None => KubeconfigError::FindPath.into(),
                });
            }
        };
        let mut contexts = Self::from_kubeconfig(config);
        contexts.files = files;
        contexts.sources = origins;
        Ok(contexts)
    }

    pub fn from_yaml(yaml: &str) -> Result<Self, ClusterError> {
        Ok(Self::from_kubeconfig(Kubeconfig::from_yaml(yaml)?))
    }

    pub fn from_kubeconfig(kubeconfig: Kubeconfig) -> Self {
        let contexts = kubeconfig
            .contexts
            .iter()
            .map(|named| {
                let context = named.context.as_ref();
                let cluster = context.map(|c| c.cluster.clone()).unwrap_or_default();
                let server = kubeconfig
                    .clusters
                    .iter()
                    .find(|named| named.name == cluster)
                    .and_then(|named| named.cluster.as_ref()?.server.clone());
                ContextInfo {
                    name: named.name.clone(),
                    cluster,
                    user: context.and_then(|c| c.user.clone()),
                    namespace: context.and_then(|c| c.namespace.clone()),
                    server,
                }
            })
            .collect();
        Self {
            kubeconfig,
            contexts,
            files: Vec::new(),
            sources: HashMap::new(),
        }
    }

    /// Files read by [`load_with_extra`](Self::load_with_extra), in merge
    /// order, with their problems. Empty for contexts built from YAML.
    pub fn files(&self) -> &[FileState] {
        &self.files
    }

    /// Files that gave no contexts.
    pub fn problems(&self) -> impl Iterator<Item = &FileState> {
        self.files.iter().filter(|file| !file.is_ok())
    }

    /// File `context` comes from, when loaded from files.
    pub fn source(&self, context: &str) -> Option<&Source> {
        self.sources.get(context)
    }

    /// API server URL of the cluster behind `context`.
    pub fn server(&self, context: &str) -> Option<&str> {
        let cluster = &self.get(context)?.cluster;
        self.kubeconfig
            .clusters
            .iter()
            .find(|named| named.name == *cluster)?
            .cluster
            .as_ref()?
            .server
            .as_deref()
    }

    /// Command of the exec auth plugin used by `context`, if any.
    pub fn exec_command(&self, context: &str) -> Option<&str> {
        self.exec_config(context)?.command.as_deref()
    }

    /// Arguments of the exec auth plugin used by `context` (empty without
    /// one). Never show them: they may hold secrets.
    pub fn exec_args(&self, context: &str) -> &[String] {
        self.exec_config(context)
            .and_then(|exec| exec.args.as_deref())
            .unwrap_or_default()
    }

    fn exec_config(&self, context: &str) -> Option<&kube::config::ExecConfig> {
        let user = self.get(context)?.user.as_deref()?;
        self.kubeconfig
            .auth_infos
            .iter()
            .find(|named| named.name == user)?
            .auth_info
            .as_ref()?
            .exec
            .as_ref()
    }

    /// A kubeconfig with only `context` (its cluster and user, credentials
    /// included) as `current-context`, `namespace` replacing its own; for
    /// the temporary kubeconfig of a local terminal
    /// ([`kubeconfig::temp`](crate::kubeconfig::temp)).
    pub fn single_context(&self, context: &str, namespace: Option<&str>) -> Option<Kubeconfig> {
        kubeconfig::temp::single_context(&self.kubeconfig, context, namespace)
    }

    pub fn list(&self) -> &[ContextInfo] {
        &self.contexts
    }

    pub fn is_empty(&self) -> bool {
        self.contexts.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&ContextInfo> {
        self.contexts.iter().find(|context| context.name == name)
    }

    /// The context to open by default: `current-context` when it exists,
    /// otherwise the first one.
    pub fn default_context(&self) -> Option<&ContextInfo> {
        self.kubeconfig
            .current_context
            .as_deref()
            .and_then(|name| self.get(name))
            .or_else(|| self.contexts.first())
    }

    /// The client configuration of `context`, without connecting. Exec
    /// plugins run here. Must run on the [`runtime`](crate::runtime).
    ///
    /// Exec plugins get the login shell's environment ([`crate::env`]):
    /// its `PATH` is where `aws` or `kubelogin` are looked up.
    pub async fn config(&self, context: &str) -> Result<Config, ClusterError> {
        self.config_with_env(context, crate::env::captured()).await
    }

    async fn config_with_env(
        &self,
        context: &str,
        env: &[(std::ffi::OsString, std::ffi::OsString)],
    ) -> Result<Config, ClusterError> {
        if self.get(context).is_none() {
            return Err(ClusterError::UnknownContext(context.to_owned()));
        }
        let options = KubeConfigOptions {
            context: Some(context.to_owned()),
            ..Default::default()
        };
        let mut kubeconfig = self.kubeconfig.clone();
        crate::env::kubeconfig_vars(&mut kubeconfig, env);
        Ok(Config::from_custom_kubeconfig(kubeconfig, &options).await?)
    }

    /// Builds a client for `context` and checks the API server answers.
    ///
    /// Exec plugins, tokens and client certificates are handled by kube-rs.
    /// Must run on the [`runtime`](crate::runtime).
    pub async fn connect(&self, context: &str) -> Result<Connection, ClusterError> {
        let config = self.config(context).await?;
        let client = crate::redact::client(config).map_err(ClusterError::Client)?;
        let version = client.apiserver_version().await.map_err(|error| {
            ClusterError::Unreachable(crate::redact::sanitize_kube_error(error))
        })?;
        Ok(Connection {
            context: context.to_owned(),
            server_version: version.git_version,
            client,
            id: NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed),
        })
    }
}

/// Suffix for the clashing cluster and user names of `source`, when its
/// names are kept apart from earlier files: linked files (file stem) and
/// imported configs (entry name). `KUBECONFIG` files merge like in kubectl.
fn isolation_tag(source: &Source) -> Option<String> {
    match source {
        Source::Env(_) | Source::Home(_) => None,
        Source::Linked(path) => Some(
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .filter(|stem| !stem.is_empty())
                .unwrap_or_else(|| "linked".into()),
        ),
        Source::Imported { name, .. } => Some(name.clone()),
    }
}

/// Source of [`Connection`] identities.
static NEXT_CONNECTION: AtomicU64 = AtomicU64::new(1);

/// A live client for one context.
#[derive(Clone)]
pub struct Connection {
    pub context: String,
    pub server_version: String,
    pub client: Client,
    /// Unique per [`Contexts::connect`]; clones share it.
    id: u64,
}

impl Connection {
    /// Whether both are the same connection, not merely one to the same
    /// context name: a reconnect (or a reloaded kubeconfig) makes a new one.
    pub fn same_as(&self, other: &Connection) -> bool {
        self.id == other.id
    }
}

/// Connection state of the active context, as displayed by the UI.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ConnectionState {
    #[default]
    Disconnected,
    Connecting,
    Connected {
        server_version: String,
    },
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    const KUBECONFIG: &str = r#"
apiVersion: v1
kind: Config
current-context: staging
clusters:
  - name: prod-cluster
    cluster: { server: "https://prod.example.com" }
  - name: staging-cluster
    cluster: { server: "https://staging.example.com" }
users:
  - name: alice
    user: { token: secret }
contexts:
  - name: prod
    context: { cluster: prod-cluster, user: alice }
  - name: staging
    context: { cluster: staging-cluster, user: alice, namespace: web }
"#;

    #[test]
    fn lists_contexts() {
        let contexts = Contexts::from_yaml(KUBECONFIG).unwrap();
        let names: Vec<_> = contexts.list().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["prod", "staging"]);

        let staging = contexts.get("staging").unwrap();
        assert_eq!(staging.cluster, "staging-cluster");
        assert_eq!(staging.user.as_deref(), Some("alice"));
        assert_eq!(staging.namespace.as_deref(), Some("web"));
        assert_eq!(
            staging.server.as_deref(),
            Some("https://staging.example.com")
        );
    }

    #[test]
    fn default_context_is_current_context() {
        let contexts = Contexts::from_yaml(KUBECONFIG).unwrap();
        assert_eq!(contexts.default_context().unwrap().name, "staging");
    }

    #[test]
    fn default_context_falls_back_to_first() {
        let yaml = KUBECONFIG.replace("current-context: staging", "current-context: missing");
        let contexts = Contexts::from_yaml(&yaml).unwrap();
        assert_eq!(contexts.default_context().unwrap().name, "prod");
    }

    #[test]
    fn empty_kubeconfig_has_no_default() {
        let contexts = Contexts::from_yaml("apiVersion: v1\nkind: Config\n").unwrap();
        assert!(contexts.is_empty());
        assert!(contexts.default_context().is_none());
    }

    #[test]
    fn merged_kubeconfigs_keep_first_current_context() {
        let other = r#"
apiVersion: v1
kind: Config
current-context: dev
clusters:
  - name: dev-cluster
    cluster: { server: "https://dev.example.com" }
contexts:
  - name: dev
    context: { cluster: dev-cluster }
"#;
        let merged = Kubeconfig::from_yaml(KUBECONFIG)
            .unwrap()
            .merge(Kubeconfig::from_yaml(other).unwrap())
            .unwrap();
        let contexts = Contexts::from_kubeconfig(merged);
        assert_eq!(contexts.list().len(), 3);
        assert_eq!(contexts.default_context().unwrap().name, "staging");
    }

    #[test]
    fn server_of_context() {
        let contexts = Contexts::from_yaml(KUBECONFIG).unwrap();
        assert_eq!(contexts.server("prod"), Some("https://prod.example.com"));
        assert_eq!(contexts.server("missing"), None);
    }

    #[test]
    fn extra_kubeconfigs_are_merged_and_bad_ones_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let extra = dir.path().join("extra.yaml");
        std::fs::write(
            &extra,
            KUBECONFIG
                .replace("name: prod", "name: extra-prod")
                .replace("name: staging", "name: extra-staging"),
        )
        .unwrap();
        let missing = dir.path().join("missing.yaml");
        // Whatever the machine's own kubeconfig is, the extra contexts are
        // added and the missing file only produces a warning.
        let contexts = Contexts::load_with_extra(&[missing.clone(), extra.clone()]).unwrap();
        assert!(contexts.get("extra-prod").is_some());
        assert!(contexts.get("extra-staging").is_some());
        assert_eq!(
            contexts.source("extra-prod"),
            Some(&Source::Linked(extra.clone()))
        );
        let problems: Vec<_> = contexts
            .problems()
            .filter(|f| f.source.is_linked())
            .collect();
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].path(), missing);
        assert_eq!(problems[0].problem, Some(FileProblem::Missing));

        // A linked file that is gone never fails the load.
        assert!(Contexts::load_with_extra(&[missing]).is_ok());
    }

    #[test]
    fn sources_are_tracked_per_context_and_file() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("config");
        std::fs::write(&base, KUBECONFIG).unwrap();
        let linked = dir.path().join("linked.yaml");
        // `prod` is shadowed by the base file; `qa` is new.
        std::fs::write(
            &linked,
            KUBECONFIG
                .replace("current-context: staging", "current-context: qa")
                .replace("name: staging\n", "name: qa\n"),
        )
        .unwrap();
        let gone = dir.path().join("gone.yaml");
        let bad = dir.path().join("bad.yaml");
        std::fs::write(&bad, "apiVersion: v2\nkind: Config\n").unwrap();

        let contexts = Contexts::load_sources(vec![
            Source::Env(base.clone()),
            Source::Linked(gone.clone()),
            Source::Linked(linked.clone()),
            Source::Linked(bad.clone()),
        ])
        .unwrap();
        let names: Vec<_> = contexts.list().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["prod", "staging", "qa"]);
        assert_eq!(contexts.default_context().unwrap().name, "staging");
        assert_eq!(contexts.source("prod"), Some(&Source::Env(base.clone())));
        assert_eq!(contexts.source("qa"), Some(&Source::Linked(linked.clone())));

        let files = contexts.files();
        assert_eq!(files.len(), 4);
        assert!(files[0].is_ok());
        assert_eq!(files[0].contexts, ["prod", "staging"]);
        assert_eq!(files[1].problem, Some(FileProblem::Missing));
        assert!(files[2].is_ok());
        assert_eq!(files[2].contexts, ["prod", "qa"]);
        // apiVersion conflicts with the files before it.
        assert!(matches!(files[3].problem, Some(FileProblem::Invalid(_))));
        assert_eq!(contexts.problems().count(), 2);
    }

    #[test]
    fn missing_default_kubeconfig_fails_unless_files_are_linked() {
        let dir = tempfile::tempdir().unwrap();
        let home = Source::Home(dir.path().join("config"));
        let error = Contexts::load_sources(vec![home.clone()]).err().unwrap();
        assert!(matches!(
            error,
            ClusterError::File {
                problem: FileProblem::Missing,
                ..
            }
        ));

        let linked = Source::Linked(dir.path().join("eks.yaml"));
        let contexts = Contexts::load_sources(vec![home, linked]).unwrap();
        assert!(contexts.is_empty());
        assert_eq!(contexts.problems().count(), 2);
    }

    #[test]
    fn imported_configs_are_merged_without_borrowing_credentials() {
        use crate::vault::Imported;
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("config");
        std::fs::write(&base, KUBECONFIG).unwrap();
        // Same cluster and user names as the base file, other server/token.
        let other = KUBECONFIG
            .replace("prod.example.com", "imported.example.com")
            .replace("token: secret", "token: imported")
            .replace("name: prod\n", "name: imported-prod\n")
            .replace("name: staging\n", "name: imported-staging\n");
        let source = |name: &str| Source::Imported {
            vault: dir.path().join("vault.kubach"),
            id: name.into(),
            name: name.into(),
        };
        let contexts = Contexts::load_all(
            vec![Source::Env(base)],
            vec![
                Imported {
                    source: source("team"),
                    config: Ok(Kubeconfig::from_yaml(&other).unwrap()),
                    contexts: vec!["imported-prod".into(), "imported-staging".into()],
                },
                Imported {
                    source: source("locked"),
                    config: Err(FileProblem::Locked),
                    contexts: vec!["secret-ctx".into()],
                },
            ],
        )
        .unwrap();
        assert_eq!(
            contexts.server("imported-prod"),
            Some("https://imported.example.com")
        );
        assert_eq!(contexts.server("prod"), Some("https://prod.example.com"));
        let imported = contexts.get("imported-prod").unwrap();
        assert_eq!(imported.cluster, "prod-cluster-team");
        assert_eq!(imported.user.as_deref(), Some("alice-team"));
        assert_eq!(contexts.source("imported-prod"), Some(&source("team")));
        assert!(contexts.source("imported-prod").unwrap().is_imported());

        // A local terminal gets the vault context with its own credentials
        // written out, not the base file's (#319).
        let single = contexts
            .single_context("imported-prod", Some("tools"))
            .unwrap();
        let yaml = crate::kubeconfig::temp::to_yaml(&single).unwrap();
        assert!(yaml.contains("token: imported"));
        assert!(yaml.contains("imported.example.com"));
        assert!(!yaml.contains("token: secret"));
        assert!(yaml.contains("namespace: tools"));
        assert!(yaml.contains("current-context: imported-prod"));

        let locked = &contexts.files()[2];
        assert_eq!(locked.problem, Some(FileProblem::Locked));
        assert_eq!(locked.contexts, ["secret-ctx"]);
        assert!(contexts.get("secret-ctx").is_none());

        // Only a locked vault and no kubeconfig: still loads.
        let only = Contexts::load_all(
            vec![Source::Home(dir.path().join("missing"))],
            vec![Imported {
                source: source("locked"),
                config: Err(FileProblem::Locked),
                contexts: vec!["secret-ctx".into()],
            }],
        )
        .unwrap();
        assert!(only.is_empty());
    }

    #[test]
    fn linked_files_never_borrow_names_from_earlier_files() {
        let dir = tempfile::tempdir().unwrap();
        // Two kubeadm configs: same cluster and user names, other servers.
        let kubeadm = |server: &str, token: &str, context: &str| {
            format!(
                r#"
apiVersion: v1
kind: Config
clusters:
  - name: kubernetes
    cluster: {{ server: "{server}" }}
users:
  - name: kubernetes-admin
    user: {{ token: {token} }}
contexts:
  - name: {context}
    context: {{ cluster: kubernetes, user: kubernetes-admin }}
"#
            )
        };
        let home = dir.path().join("config");
        std::fs::write(&home, kubeadm("https://home.invalid", "home-token", "home")).unwrap();
        let second = dir.path().join("lab.yaml");
        std::fs::write(&second, kubeadm("https://lab.invalid", "lab-token", "lab")).unwrap();
        // A dropped file pointing a context at the home user and its own
        // server, without defining that user.
        let dropped = dir.path().join("dropped.yaml");
        std::fs::write(
            &dropped,
            r#"
apiVersion: v1
kind: Config
clusters:
  - name: evil
    cluster: { server: "https://attacker.invalid" }
contexts:
  - name: x
    context: { cluster: evil, user: kubernetes-admin }
"#,
        )
        .unwrap();

        let contexts = Contexts::load_sources(vec![
            Source::Home(home),
            Source::Linked(second),
            Source::Linked(dropped),
        ])
        .unwrap();
        assert_eq!(contexts.server("home"), Some("https://home.invalid"));
        // The second file's context goes to its own cluster.
        assert_eq!(contexts.server("lab"), Some("https://lab.invalid"));
        let lab = contexts.get("lab").unwrap();
        assert_eq!(lab.cluster, "kubernetes-lab");
        assert_eq!(lab.user.as_deref(), Some("kubernetes-admin-lab"));
        let user_token = |user: &str| {
            contexts
                .kubeconfig
                .auth_infos
                .iter()
                .find(|u| u.name == user)
                .and_then(|u| u.auth_info.as_ref()?.token.as_ref())
                .map(|t| secrecy::ExposeSecret::expose_secret(t).to_owned())
        };
        assert_eq!(
            user_token("kubernetes-admin-lab").as_deref(),
            Some("lab-token")
        );
        // The dropped file gets no credentials.
        let x = contexts.get("x").unwrap();
        assert_eq!(x.user.as_deref(), Some("kubernetes-admin-dropped"));
        assert_eq!(user_token("kubernetes-admin-dropped"), None);
        assert_eq!(contexts.server("x"), Some("https://attacker.invalid"));
    }

    #[tokio::test]
    async fn connect_rejects_unknown_context() {
        let contexts = Contexts::from_yaml(KUBECONFIG).unwrap();
        let error = contexts.connect("nope").await.err().unwrap();
        assert!(matches!(error, ClusterError::UnknownContext(name) if name == "nope"));
    }

    /// Spawns on the shared runtime and awaits from a different executor,
    /// the same way the UI does.
    #[test]
    fn connect_reports_unreachable_cluster() {
        let yaml = KUBECONFIG.replace("https://prod.example.com", "https://127.0.0.1:1");
        let contexts = Contexts::from_yaml(&yaml).unwrap();
        let task = crate::runtime::spawn(async move { contexts.connect("prod").await });

        let other_executor = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let result = other_executor.block_on(task).unwrap();
        assert!(
            matches!(result, Err(ClusterError::Unreachable(_))),
            "{:?}",
            result.err()
        );
    }

    #[test]
    fn missing_exec_plugin_is_reported() {
        let yaml = KUBECONFIG.replace(
            "user: { token: secret }",
            "user: { exec: { apiVersion: client.authentication.k8s.io/v1beta1, \
             command: kubach-test-missing-plugin, interactiveMode: Never } }",
        );
        let contexts = Contexts::from_yaml(&yaml).unwrap();
        assert_eq!(
            contexts.exec_command("prod"),
            Some("kubach-test-missing-plugin")
        );
        assert_eq!(
            Contexts::from_yaml(KUBECONFIG)
                .unwrap()
                .exec_command("prod"),
            None
        );

        let task = crate::runtime::spawn(async move { contexts.connect("prod").await });
        let other_executor = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let error = other_executor.block_on(task).unwrap().err().unwrap();
        assert!(error.is_exec_not_found(), "{error:?}");
    }

    /// A failing exec plugin with a secret in its arguments and environment:
    /// none of the error's texts show it (#189).
    #[cfg(unix)]
    #[test]
    fn failed_exec_plugin_hides_arguments_and_env() {
        const SECRET: &str = "SUPERSECRET";
        let yaml = format!(
            r#"
apiVersion: v1
kind: Config
current-context: prod
clusters:
  - name: prod
    cluster: {{ server: "https://127.0.0.1:1" }}
users:
  - name: alice
    user:
      exec:
        apiVersion: client.authentication.k8s.io/v1beta1
        command: /bin/sh
        args: ["-c", "echo fail >&2; exit 3", "--oidc-client-secret={SECRET}"]
        env: [{{ name: AWS_SECRET_ACCESS_KEY, value: {SECRET} }}]
        interactiveMode: Never
contexts:
  - name: prod
    context: {{ cluster: prod, user: alice }}
"#
        );
        let contexts = Contexts::from_yaml(&yaml).unwrap();
        let task = crate::runtime::spawn(async move { contexts.connect("prod").await });
        let other_executor = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let error = other_executor.block_on(task).unwrap().err().unwrap();
        let message = error.user_message();
        assert_eq!(
            message,
            "failed to create client: auth error: auth plugin `sh` failed with exit code 3: fail"
        );
        for text in [message, error.to_string(), format!("{error:?}")] {
            assert!(!text.contains(SECRET), "{text}");
            assert!(!text.contains("AWS_SECRET_ACCESS_KEY"), "{text}");
        }
    }

    /// An exec plugin found only in the login shell's `PATH` starts: the
    /// variables reach kube-rs through the exec config (#307).
    #[cfg(unix)]
    #[tokio::test]
    async fn exec_plugin_is_found_in_shell_path() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let plugin = dir.path().join("kubach-test-307-plugin");
        std::fs::write(
            &plugin,
            "#!/bin/sh\nprintf '%s' '{\"apiVersion\":\"client.authentication.k8s.io/v1\",\
             \"kind\":\"ExecCredential\",\"status\":{\"token\":\"t-307\"}}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&plugin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let contexts = Contexts::from_yaml(
            r#"
apiVersion: v1
kind: Config
clusters:
  - name: c
    cluster: { server: "https://127.0.0.1:1" }
users:
  - name: u
    user:
      exec:
        apiVersion: client.authentication.k8s.io/v1
        command: kubach-test-307-plugin
        interactiveMode: Never
contexts:
  - name: ctx
    context: { cluster: c, user: u }
"#,
        )
        .unwrap();

        let without = contexts.config_with_env("ctx", &[]).await.unwrap();
        assert!(kube::Client::try_from(without).is_err());

        let shell_path = std::env::join_paths(
            std::iter::once(dir.path().to_owned())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let env = [("PATH".into(), shell_path)];
        let config = contexts.config_with_env("ctx", &env).await.unwrap();
        kube::Client::try_from(config).expect("the plugin is found in the shell's PATH");
    }
}
