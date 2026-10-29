//! "Import from cloud" (#38): managed Kubernetes clusters listed through
//! the provider's CLI and turned into kubeconfig contexts the way the
//! CLI's own `get-credentials` / `update-kubeconfig` writes them.
//!
//! Supported: Google GKE ([`gke`], `gcloud`), Yandex Cloud Managed
//! Kubernetes ([`yandex`], `yc`) and Amazon EKS ([`eks`], `aws`). Each
//! CLI call is described as data ([`CliCall`]) and run with
//! [`crate::process`]; its JSON is parsed here. Kubach never sees or keeps
//! a cloud token: the CLI keeps the login, and the contexts sign in
//! through an exec plugin of that CLI (`gke-gcloud-auth-plugin`,
//! `yc k8s create-token`, `aws eks get-token`) when Kubach connects. The
//! built config goes to the vault through the usual import preview
//! (ADR 0008).

pub mod eks;
pub mod gke;
pub mod yandex;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use kube::config::{
    AuthInfo, Cluster as KubeCluster, Context, ExecConfig, Kubeconfig, NamedAuthInfo, NamedCluster,
    NamedContext,
};

use crate::tools::cli::{self, CliTool};
use crate::vault::manual::DEFAULT_EXEC_API_VERSION;

/// One CLI call should not take longer (listing many projects can be slow).
const CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// EKS clusters described at once.
const DESCRIBE_PARALLEL: usize = 8;

/// A cloud with managed Kubernetes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Provider {
    Gke,
    Yandex,
    Eks,
}

impl Provider {
    pub const ALL: [Self; 3] = [Self::Gke, Self::Yandex, Self::Eks];

    pub fn label(self) -> &'static str {
        match self {
            Self::Gke => "Google GKE",
            Self::Yandex => "Yandex Cloud",
            Self::Eks => "AWS EKS",
        }
    }

    /// The CLI that lists the clusters and signs in.
    pub fn tool(self) -> CliTool {
        match self {
            Self::Gke => CliTool::Gcloud,
            Self::Yandex => CliTool::Yc,
            Self::Eks => CliTool::Aws,
        }
    }

    /// Commands that sign the CLI in, for the hint when it is not.
    pub fn login_commands(self, profile: Option<&str>) -> Vec<String> {
        match self {
            Self::Gke => vec!["gcloud auth login".into()],
            Self::Yandex => vec!["yc init".into()],
            Self::Eks => match profile.filter(|p| *p != eks::DEFAULT_PROFILE) {
                Some(profile) => vec![
                    format!("aws configure --profile {profile}"),
                    format!("aws sso login --profile {profile}"),
                ],
                None => vec!["aws configure".into(), "aws sso login".into()],
            },
        }
    }

    /// Whether the CLI has profiles to pick from (yc, aws).
    pub fn has_profiles(self) -> bool {
        self != Self::Gke
    }
}

/// A CLI invocation: arguments after the program name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliCall {
    pub args: Vec<String>,
}

impl CliCall {
    pub fn new<I, S>(args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            args: args.into_iter().map(Into::into).collect(),
        }
    }
}

/// Where clusters are looked for: a GKE project, a Yandex folder or an
/// AWS region.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Scope {
    pub id: String,
    /// What the user sees ("my-project", "cloud / folder", "eu-west-1").
    pub label: String,
}

/// What can be chosen before listing clusters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Discovery {
    /// yc / aws profiles; empty for GKE.
    pub profiles: Vec<String>,
    /// The profile used (the CLI's active one unless chosen).
    pub profile: Option<String>,
    pub scopes: Vec<Scope>,
    /// Scopes checked at first (the AWS profile's region).
    pub preselected: Vec<String>,
    /// The account the CLI is signed in with, when it says (gcloud).
    pub account: Option<String>,
}

/// How an endpoint of a cluster is reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EndpointKind {
    /// Public IP address.
    External,
    /// Private address in the cloud network (VPN, bastion, peering).
    Internal,
    /// GKE DNS-based endpoint (public certificate, no CA in the config).
    Dns,
}

/// An API server address of a cluster.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub kind: EndpointKind,
    /// `https://…`.
    pub server: String,
}

/// What the exec plugin of a context needs to know about its cluster.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Gke {
        project: String,
        location: String,
    },
    Yandex {
        id: String,
        profile: Option<String>,
    },
    Eks {
        arn: String,
        region: String,
        profile: Option<String>,
    },
}

/// A cluster found in the cloud.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudCluster {
    pub name: String,
    /// Zone or region.
    pub location: String,
    /// Label of the scope it was found in.
    pub scope: String,
    /// As the cloud says ("RUNNING", "ACTIVE", …).
    pub status: String,
    pub version: Option<String>,
    /// Usable addresses, preferred first.
    pub endpoints: Vec<Endpoint>,
    /// Base64 PEM of the cluster CA.
    pub ca_data: Option<String>,
    pub target: Target,
}

impl CloudCluster {
    /// Unique among clusters of all providers.
    pub fn key(&self) -> String {
        match &self.target {
            Target::Gke { project, location } => {
                format!("gke/{project}/{location}/{}", self.name)
            }
            Target::Yandex { id, .. } => format!("yc/{id}"),
            Target::Eks { arn, .. } => format!("eks/{arn}"),
        }
    }

    pub fn provider(&self) -> Provider {
        match self.target {
            Target::Gke { .. } => Provider::Gke,
            Target::Yandex { .. } => Provider::Yandex,
            Target::Eks { .. } => Provider::Eks,
        }
    }

    pub fn endpoint(&self, kind: EndpointKind) -> Option<&Endpoint> {
        self.endpoints.iter().find(|e| e.kind == kind)
    }

    /// Context name as the CLI's `get-credentials` writes it.
    pub fn context_name(&self) -> String {
        match &self.target {
            Target::Gke { project, location } => {
                format!("gke_{project}_{location}_{}", self.name)
            }
            Target::Yandex { .. } => format!("yc-{}", self.name),
            Target::Eks { arn, .. } => arn.clone(),
        }
    }

    /// Name of the cluster and user entries.
    fn entry_name(&self) -> String {
        match &self.target {
            Target::Yandex { id, .. } => format!("yc-managed-k8s-{id}"),
            _ => self.context_name(),
        }
    }
}

/// A cluster the user checked, with the endpoint to use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub cluster: CloudCluster,
    pub endpoint: EndpointKind,
}

/// The programs the exec plugins run: an absolute path for a CLI Kubach
/// downloaded, the bare name for one in `PATH` (as the CLIs write it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecCommands {
    pub gke_plugin: String,
    pub yc: String,
    pub aws: String,
}

impl Default for ExecCommands {
    fn default() -> Self {
        Self {
            gke_plugin: crate::tools::gcloud::GKE_PLUGIN.into(),
            yc: "yc".into(),
            aws: "aws".into(),
        }
    }
}

impl ExecCommands {
    /// The commands for the CLIs found: an absolute path for a copy Kubach
    /// downloaded (it is not in `PATH`), the bare name for one in `PATH`.
    /// The GKE plugin goes with `gcloud`: its bare name when it is in
    /// `PATH` too, else its absolute path next to gcloud.
    pub fn for_binaries(
        gcloud: Option<&cli::CliBinary>,
        gke_plugin: Option<&Path>,
        plugin_in_path: bool,
        yc: Option<&cli::CliBinary>,
        aws: Option<&cli::CliBinary>,
    ) -> Self {
        let defaults = Self::default();
        let command = |binary: Option<&cli::CliBinary>, fallback: String| match binary {
            Some(binary) if binary.source == cli::CliSource::Downloaded => {
                binary.path.display().to_string()
            }
            _ => fallback,
        };
        let downloaded_gcloud = gcloud.is_some_and(|g| g.source == cli::CliSource::Downloaded);
        let gke_plugin = match gke_plugin {
            Some(path) if downloaded_gcloud || !plugin_in_path => path.display().to_string(),
            _ => defaults.gke_plugin,
        };
        Self {
            gke_plugin,
            yc: command(yc, defaults.yc),
            aws: command(aws, defaults.aws),
        }
    }
}

/// gcloud's install hint for the GKE plugin.
const GKE_INSTALL_HINT: &str = "Install gke-gcloud-auth-plugin for use with kubectl by following \
     https://cloud.google.com/kubernetes-engine/docs/how-to/cluster-access-for-kubectl#install_plugin";

/// The exec plugin of a context for `cluster`.
pub fn exec_config(cluster: &CloudCluster, commands: &ExecCommands) -> ExecConfig {
    use crate::vault::manual::{ExecTemplate, TemplateParams};
    let base = ExecConfig {
        api_version: Some(DEFAULT_EXEC_API_VERSION.into()),
        ..ExecConfig::default()
    };
    match &cluster.target {
        Target::Gke { .. } => ExecConfig {
            command: Some(commands.gke_plugin.clone()),
            install_hint: Some(GKE_INSTALL_HINT.into()),
            provide_cluster_info: true,
            ..base
        },
        Target::Yandex { profile, .. } => {
            let params = TemplateParams {
                profile: profile.clone().unwrap_or_default(),
                ..TemplateParams::default()
            };
            let spec = ExecTemplate::Yandex.exec(&params).unwrap_or_default();
            ExecConfig {
                command: Some(commands.yc.clone()),
                args: Some(spec.args),
                ..base
            }
        }
        Target::Eks {
            region, profile, ..
        } => {
            let params = TemplateParams {
                cluster_name: cluster.name.clone(),
                region: region.clone(),
                profile: profile
                    .clone()
                    .filter(|p| p != eks::DEFAULT_PROFILE)
                    .unwrap_or_default(),
                ..TemplateParams::default()
            };
            let spec = ExecTemplate::Eks.exec(&params).unwrap_or_default();
            let env = spec
                .env
                .into_iter()
                .map(|(name, value)| {
                    HashMap::from([("name".to_owned(), name), ("value".to_owned(), value)])
                })
                .collect::<Vec<_>>();
            ExecConfig {
                command: Some(commands.aws.clone()),
                args: Some(spec.args),
                env: (!env.is_empty()).then_some(env),
                ..base
            }
        }
    }
}

/// A kubeconfig with one context per selection, the first one current.
/// Names follow the CLIs; a name taken twice in the selection gets the
/// cluster's location appended.
pub fn build_kubeconfig(selections: &[Selection], commands: &ExecCommands) -> Kubeconfig {
    let mut config = Kubeconfig {
        api_version: Some("v1".into()),
        kind: Some("Config".into()),
        ..Kubeconfig::default()
    };
    let mut used = HashSet::new();
    for selection in selections {
        let cluster = &selection.cluster;
        let Some(endpoint) = cluster
            .endpoint(selection.endpoint)
            .or_else(|| cluster.endpoints.first())
        else {
            continue;
        };
        let mut context = cluster.context_name();
        let mut entry = cluster.entry_name();
        if !used.insert(context.clone()) {
            context = format!("{context}-{}", cluster.location);
            entry = format!("{entry}-{}", cluster.location);
            used.insert(context.clone());
        }
        // The DNS endpoint has a publicly trusted certificate.
        let ca = (endpoint.kind != EndpointKind::Dns)
            .then(|| cluster.ca_data.clone())
            .flatten();
        config.clusters.push(NamedCluster {
            name: entry.clone(),
            cluster: Some(KubeCluster {
                server: Some(endpoint.server.clone()),
                certificate_authority_data: ca,
                ..KubeCluster::default()
            }),
            ..NamedCluster::default()
        });
        config.auth_infos.push(NamedAuthInfo {
            name: entry.clone(),
            auth_info: Some(AuthInfo {
                exec: Some(exec_config(cluster, commands)),
                ..AuthInfo::default()
            }),
            ..NamedAuthInfo::default()
        });
        config.contexts.push(NamedContext {
            name: context.clone(),
            context: Some(Context {
                cluster: entry.clone(),
                user: Some(entry),
                ..Context::default()
            }),
            ..NamedContext::default()
        });
        if config.current_context.is_none() {
            config.current_context = Some(context);
        }
    }
    config
}

/// Why listing failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CloudError {
    /// The CLI is not signed in (or the session expired).
    #[error("{message}")]
    NotLoggedIn { message: String },
    /// Anything else the CLI reported.
    #[error("{message}")]
    Failed { message: String },
    /// The CLI answered something that is not the expected JSON.
    #[error("unexpected output of {command}: {detail}")]
    Parse { command: String, detail: String },
}

/// Runs `call` with the CLI at `program` (the login shell's environment is
/// added by [`crate::process`]) and returns its standard output.
pub async fn run(provider: Provider, program: &Path, call: &CliCall) -> Result<String, CloudError> {
    let mut command = tokio::process::Command::new(program);
    command.args(&call.args);
    for (name, value) in cli::quiet_env(provider.tool()) {
        command.env(name, value);
    }
    match crate::process::run(command, CALL_TIMEOUT).await {
        Ok(output) => Ok(output.stdout_text()),
        Err(crate::process::ProcessError::Failed { stderr, stdout, .. }) => {
            let text = String::from_utf8_lossy(&stderr);
            let text = if text.trim().is_empty() {
                String::from_utf8_lossy(&stdout).into_owned()
            } else {
                text.into_owned()
            };
            Err(classify(provider, &text))
        }
        Err(error) => Err(CloudError::Failed {
            message: format!("{} {error}", provider.tool().command()),
        }),
    }
}

/// Error output of a failed call as [`CloudError::NotLoggedIn`] when it
/// says the CLI has no (valid) login, else [`CloudError::Failed`].
pub fn classify(provider: Provider, stderr: &str) -> CloudError {
    let lower = stderr.to_ascii_lowercase();
    let signs: &[&str] = match provider {
        Provider::Gke => &[
            "do not currently have an active account",
            "gcloud auth login",
            "reauthentication",
            "refresh token",
            "invalid_grant",
        ],
        Provider::Yandex => &[
            "yc init",
            "not initialized",
            "unauthenticated",
            "iam token",
            "token is expired",
            "federation",
        ],
        Provider::Eks => &[
            "unable to locate credentials",
            "sso session",
            "token has expired",
            "expiredtoken",
            "aws sso login",
            "the config profile",
            "invalidclienttokenid",
            "security token included in the request is invalid",
        ],
    };
    let message = cli::last_lines(stderr);
    if signs.iter().any(|sign| lower.contains(sign)) {
        CloudError::NotLoggedIn { message }
    } else {
        CloudError::Failed { message }
    }
}

fn parse_json(command: &CliCall, text: &str) -> Result<serde_json::Value, CloudError> {
    serde_json::from_str(text).map_err(|error| CloudError::Parse {
        command: command.args.join(" "),
        detail: error.to_string(),
    })
}

/// Base64 of a PEM certificate given as text (yc prints the PEM itself).
fn pem_to_data(pem: &str) -> Option<String> {
    let pem = pem.trim();
    pem.starts_with("-----BEGIN")
        .then(|| base64::engine::general_purpose::STANDARD.encode(pem.as_bytes()))
}

/// `https://` in front of a bare host (gcloud prints addresses without it).
fn https(host: &str) -> String {
    if host.starts_with("https://") {
        host.to_owned()
    } else {
        format!("https://{host}")
    }
}

/// What the CLI offers before listing: profiles, scopes, the account.
pub async fn discover(
    provider: Provider,
    program: &Path,
    profile: Option<&str>,
) -> Result<Discovery, CloudError> {
    match provider {
        Provider::Gke => gke::discover(program).await,
        Provider::Yandex => yandex::discover(program, profile).await,
        Provider::Eks => eks::discover(program, profile).await,
    }
}

/// Clusters in `scopes`; scopes that failed are returned with the error
/// next to the clusters of the others.
pub async fn clusters(
    provider: Provider,
    program: &Path,
    profile: Option<&str>,
    scopes: &[Scope],
) -> (Vec<CloudCluster>, Vec<(Scope, CloudError)>) {
    let mut found = Vec::new();
    let mut failed = Vec::new();
    for scope in scopes {
        let result = match provider {
            Provider::Gke => gke::clusters(program, scope).await,
            Provider::Yandex => yandex::clusters(program, profile, scope).await,
            Provider::Eks => eks::clusters(program, profile, scope).await,
        };
        match result {
            Ok(clusters) => found.extend(clusters),
            Err(error) => failed.push((scope.clone(), error)),
        }
    }
    (found, failed)
}

/// Runs `calls` at most [`DESCRIBE_PARALLEL`] at a time, in order.
async fn run_all(
    provider: Provider,
    program: &Path,
    calls: Vec<CliCall>,
) -> Vec<(CliCall, Result<String, CloudError>)> {
    use futures::StreamExt as _;
    futures::stream::iter(calls)
        .map(|call| async move {
            let result = run(provider, program, &call).await;
            (call, result)
        })
        .buffered(DESCRIBE_PARALLEL)
        .collect()
        .await
}

/// String field `path` (`/a/b`) of `value`.
fn text(value: &serde_json::Value, path: &str) -> Option<String> {
    value
        .pointer(path)
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Keeps one endpoint per address, in kind order.
fn dedup_endpoints(mut endpoints: Vec<Endpoint>) -> Vec<Endpoint> {
    endpoints.sort_by_key(|e| e.kind);
    let mut seen = BTreeMap::new();
    endpoints.retain(|e| seen.insert(e.server.clone(), ()).is_none());
    endpoints
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gke_cluster() -> CloudCluster {
        CloudCluster {
            name: "prod".into(),
            location: "europe-west1".into(),
            scope: "acme-prod".into(),
            status: "RUNNING".into(),
            version: Some("1.31.1-gke.1".into()),
            endpoints: vec![
                Endpoint {
                    kind: EndpointKind::External,
                    server: "https://34.1.2.3".into(),
                },
                Endpoint {
                    kind: EndpointKind::Dns,
                    server: "https://gke-abc.europe-west1.gke.goog".into(),
                },
            ],
            ca_data: Some("Q0EK".into()),
            target: Target::Gke {
                project: "acme-prod".into(),
                location: "europe-west1".into(),
            },
        }
    }

    fn eks_cluster(profile: Option<&str>) -> CloudCluster {
        CloudCluster {
            name: "main".into(),
            location: "eu-west-1".into(),
            scope: "eu-west-1".into(),
            status: "ACTIVE".into(),
            version: Some("1.31".into()),
            endpoints: vec![Endpoint {
                kind: EndpointKind::External,
                server: "https://ABC.gr7.eu-west-1.eks.amazonaws.com".into(),
            }],
            ca_data: Some("RUtTCg==".into()),
            target: Target::Eks {
                arn: "arn:aws:eks:eu-west-1:123456789012:cluster/main".into(),
                region: "eu-west-1".into(),
                profile: profile.map(Into::into),
            },
        }
    }

    fn yc_cluster() -> CloudCluster {
        CloudCluster {
            name: "k8s".into(),
            location: "ru-central1-a".into(),
            scope: "cloud / default".into(),
            status: "RUNNING".into(),
            version: Some("1.30".into()),
            endpoints: vec![Endpoint {
                kind: EndpointKind::Internal,
                server: "https://10.128.0.5".into(),
            }],
            ca_data: Some("WUMK".into()),
            target: Target::Yandex {
                id: "cat0abc".into(),
                profile: Some("default".into()),
            },
        }
    }

    fn yaml(config: &Kubeconfig) -> String {
        serde_saphyr::to_string(config).unwrap()
    }

    #[test]
    fn gke_context_like_get_credentials() {
        let config = build_kubeconfig(
            &[Selection {
                cluster: gke_cluster(),
                endpoint: EndpointKind::External,
            }],
            &ExecCommands {
                gke_plugin:
                    "/data/tools/gcloud/591.0.0/google-cloud-sdk/bin/gke-gcloud-auth-plugin".into(),
                ..ExecCommands::default()
            },
        );
        let name = "gke_acme-prod_europe-west1_prod";
        assert_eq!(config.current_context.as_deref(), Some(name));
        assert_eq!(config.contexts[0].name, name);
        let cluster = config.clusters[0].cluster.as_ref().unwrap();
        assert_eq!(cluster.server.as_deref(), Some("https://34.1.2.3"));
        assert_eq!(cluster.certificate_authority_data.as_deref(), Some("Q0EK"));
        let exec = config.auth_infos[0]
            .auth_info
            .as_ref()
            .unwrap()
            .exec
            .as_ref()
            .unwrap();
        assert_eq!(
            exec.command.as_deref(),
            Some("/data/tools/gcloud/591.0.0/google-cloud-sdk/bin/gke-gcloud-auth-plugin")
        );
        assert_eq!(exec.args, None);
        assert!(exec.provide_cluster_info);
        assert_eq!(
            exec.api_version.as_deref(),
            Some("client.authentication.k8s.io/v1beta1")
        );
        assert!(
            exec.install_hint
                .as_deref()
                .unwrap()
                .contains("gke-gcloud-auth-plugin")
        );
        // It loads as a kubeconfig again.
        let text = yaml(&config);
        let back = Kubeconfig::from_yaml(&text).unwrap();
        assert_eq!(back.contexts.len(), 1);
    }

    #[test]
    fn gke_dns_endpoint_has_no_ca() {
        let config = build_kubeconfig(
            &[Selection {
                cluster: gke_cluster(),
                endpoint: EndpointKind::Dns,
            }],
            &ExecCommands::default(),
        );
        let cluster = config.clusters[0].cluster.as_ref().unwrap();
        assert_eq!(
            cluster.server.as_deref(),
            Some("https://gke-abc.europe-west1.gke.goog")
        );
        assert_eq!(cluster.certificate_authority_data, None);
    }

    #[test]
    fn yandex_context_like_get_credentials() {
        let config = build_kubeconfig(
            &[Selection {
                cluster: yc_cluster(),
                endpoint: EndpointKind::External,
            }],
            &ExecCommands {
                yc: "/data/tools/yc/1.42.0/yc".into(),
                ..ExecCommands::default()
            },
        );
        assert_eq!(config.contexts[0].name, "yc-k8s");
        assert_eq!(config.clusters[0].name, "yc-managed-k8s-cat0abc");
        assert_eq!(config.auth_infos[0].name, "yc-managed-k8s-cat0abc");
        // Only an internal endpoint: it is used even if external was asked.
        let cluster = config.clusters[0].cluster.as_ref().unwrap();
        assert_eq!(cluster.server.as_deref(), Some("https://10.128.0.5"));
        let exec = config.auth_infos[0]
            .auth_info
            .as_ref()
            .unwrap()
            .exec
            .as_ref()
            .unwrap();
        assert_eq!(exec.command.as_deref(), Some("/data/tools/yc/1.42.0/yc"));
        assert_eq!(
            exec.args.as_deref().unwrap(),
            ["k8s", "create-token", "--profile=default"]
        );
    }

    #[test]
    fn eks_context_like_update_kubeconfig() {
        let config = build_kubeconfig(
            &[Selection {
                cluster: eks_cluster(Some("work")),
                endpoint: EndpointKind::External,
            }],
            &ExecCommands::default(),
        );
        let arn = "arn:aws:eks:eu-west-1:123456789012:cluster/main";
        assert_eq!(config.contexts[0].name, arn);
        assert_eq!(config.clusters[0].name, arn);
        let exec = config.auth_infos[0]
            .auth_info
            .as_ref()
            .unwrap()
            .exec
            .as_ref()
            .unwrap();
        assert_eq!(exec.command.as_deref(), Some("aws"));
        assert_eq!(
            exec.args.as_deref().unwrap(),
            [
                "--region",
                "eu-west-1",
                "eks",
                "get-token",
                "--cluster-name",
                "main",
                "--output",
                "json"
            ]
        );
        let env = exec.env.as_ref().unwrap();
        assert_eq!(env[0]["name"], "AWS_PROFILE");
        assert_eq!(env[0]["value"], "work");

        // The default profile is not written.
        let config = build_kubeconfig(
            &[Selection {
                cluster: eks_cluster(Some("default")),
                endpoint: EndpointKind::External,
            }],
            &ExecCommands::default(),
        );
        let exec = config.auth_infos[0]
            .auth_info
            .as_ref()
            .unwrap()
            .exec
            .as_ref()
            .unwrap();
        assert_eq!(exec.env, None);
    }

    #[test]
    fn several_clusters_and_name_clashes() {
        let mut other = yc_cluster();
        other.location = "ru-central1-b".into();
        other.target = Target::Yandex {
            id: "cat0def".into(),
            profile: None,
        };
        let config = build_kubeconfig(
            &[
                Selection {
                    cluster: yc_cluster(),
                    endpoint: EndpointKind::Internal,
                },
                Selection {
                    cluster: other,
                    endpoint: EndpointKind::Internal,
                },
                Selection {
                    cluster: gke_cluster(),
                    endpoint: EndpointKind::External,
                },
            ],
            &ExecCommands::default(),
        );
        let names: Vec<&str> = config.contexts.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "yc-k8s",
                "yc-k8s-ru-central1-b",
                "gke_acme-prod_europe-west1_prod"
            ]
        );
        assert_eq!(config.current_context.as_deref(), Some("yc-k8s"));
        // The vault import accepts it.
        let plan = crate::vault::ImportPlan::from_kubeconfig(config, &HashSet::new());
        assert_eq!(plan.contexts.len(), 3);
        assert!(plan.review.files.is_empty());
    }

    #[test]
    fn exec_commands_for_found_clis() {
        use std::path::PathBuf;
        let binary = |tool, path: &str, source| cli::CliBinary {
            tool,
            path: PathBuf::from(path),
            source,
        };
        let yc = binary(
            CliTool::Yc,
            "/data/tools/yc/1.42.0/yc",
            cli::CliSource::Downloaded,
        );
        let aws = binary(CliTool::Aws, "/usr/local/bin/aws", cli::CliSource::Path);
        let gcloud = binary(
            CliTool::Gcloud,
            "/data/tools/gcloud/591.0.0/google-cloud-sdk/bin/gcloud",
            cli::CliSource::Downloaded,
        );
        let plugin =
            Path::new("/data/tools/gcloud/591.0.0/google-cloud-sdk/bin/gke-gcloud-auth-plugin");
        let commands =
            ExecCommands::for_binaries(Some(&gcloud), Some(plugin), false, Some(&yc), Some(&aws));
        assert_eq!(commands.yc, "/data/tools/yc/1.42.0/yc");
        assert_eq!(commands.aws, "aws");
        assert_eq!(commands.gke_plugin, plugin.display().to_string());

        // gcloud from PATH: the plugin by name when PATH has it too.
        let brew = binary(
            CliTool::Gcloud,
            "/opt/homebrew/bin/gcloud",
            cli::CliSource::Path,
        );
        let sdk_plugin = Path::new("/opt/sdk/bin/gke-gcloud-auth-plugin");
        let in_path = ExecCommands::for_binaries(Some(&brew), Some(sdk_plugin), true, None, None);
        assert_eq!(in_path.gke_plugin, "gke-gcloud-auth-plugin");
        assert_eq!(in_path.yc, "yc");
        let beside = ExecCommands::for_binaries(Some(&brew), Some(sdk_plugin), false, None, None);
        assert_eq!(beside.gke_plugin, "/opt/sdk/bin/gke-gcloud-auth-plugin");
    }

    #[test]
    fn failures_that_mean_no_login() {
        let gcloud = "ERROR: (gcloud.projects.list) You do not currently have an active account \
                      selected.\nPlease run:\n\n  $ gcloud auth login\n";
        assert!(matches!(
            classify(Provider::Gke, gcloud),
            CloudError::NotLoggedIn { .. }
        ));
        let aws = "\nUnable to locate credentials. You can configure credentials by running \
                   \"aws configure\".\n";
        assert!(matches!(
            classify(Provider::Eks, aws),
            CloudError::NotLoggedIn { .. }
        ));
        let sso = "Error when retrieving token from sso: Token has expired and refresh failed\n";
        assert!(matches!(
            classify(Provider::Eks, sso),
            CloudError::NotLoggedIn { .. }
        ));
        let yc = "ERROR: Current profile has no authentication methods. Run 'yc init'.\n";
        assert!(matches!(
            classify(Provider::Yandex, yc),
            CloudError::NotLoggedIn { .. }
        ));
        let denied = "ERROR: (gcloud.container.clusters.list) ResponseError: code=403, \
                      message=Kubernetes Engine API has not been used in project 1";
        assert_eq!(
            classify(Provider::Gke, denied),
            CloudError::Failed {
                message: denied.into()
            }
        );
    }

    #[test]
    fn login_hints() {
        assert_eq!(Provider::Gke.login_commands(None), ["gcloud auth login"]);
        assert_eq!(Provider::Yandex.login_commands(Some("x")), ["yc init"]);
        assert_eq!(
            Provider::Eks.login_commands(Some("work")),
            [
                "aws configure --profile work",
                "aws sso login --profile work"
            ]
        );
        assert_eq!(
            Provider::Eks.login_commands(Some("default")),
            ["aws configure", "aws sso login"]
        );
    }

    #[test]
    fn pem_and_addresses() {
        assert_eq!(pem_to_data("not pem"), None);
        let data =
            pem_to_data("-----BEGIN CERTIFICATE-----\nAA==\n-----END CERTIFICATE-----\n").unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(data)
            .unwrap();
        assert!(decoded.starts_with(b"-----BEGIN CERTIFICATE-----"));
        assert_eq!(https("1.2.3.4"), "https://1.2.3.4");
        assert_eq!(https("https://h"), "https://h");
    }
}
