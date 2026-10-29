//! A cluster added by hand: the fields of the "Add manually" form turned
//! into a one-context kubeconfig (and back, for configs simple enough to
//! show in the form).
//!
//! The result goes through the same [`ImportPlan`](super::ImportPlan) as an
//! imported file, so it gets the same preview and conflict renaming.

use std::collections::HashMap;
use std::fmt;

use base64::Engine as _;
use kubach_i18n::{Message, msg};
use kube::config::{
    AuthInfo, Cluster, Context, ExecConfig, Kubeconfig, NamedAuthInfo, NamedCluster, NamedContext,
};
use secrecy::{ExposeSecret as _, SecretString};
use zeroize::Zeroizing;

/// `apiVersion` of exec credential plugins when none is given.
pub const DEFAULT_EXEC_API_VERSION: &str = "client.authentication.k8s.io/v1beta1";

/// How the API server's certificate is checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaMode {
    /// The system trust store (for servers with a public certificate).
    System,
    /// A CA certificate in PEM (pasted or read from a file), or its base64.
    Pem(String),
    /// No check at all (`insecure-skip-tls-verify`).
    InsecureSkipVerify,
}

/// How Kubach signs in to the cluster.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Auth {
    /// A bearer token (service account, OIDC id-token, …).
    Token(Zeroizing<String>),
    /// A client certificate and its private key, PEM or base64.
    ClientCert {
        cert: String,
        key: Zeroizing<String>,
    },
    /// A user name and password (HTTP basic auth; most clusters have it
    /// off).
    Basic {
        username: String,
        password: Zeroizing<String>,
    },
    /// OpenID Connect through the kubelogin exec plugin
    /// (`kubectl oidc-login get-token`), see [`OidcSpec`].
    Oidc(OidcSpec),
    /// An exec credential plugin (`aws`, `gke-gcloud-auth-plugin`,
    /// `kubelogin`, …).
    Exec(ExecSpec),
}

/// OpenID Connect sign-in. Stored as an exec plugin running
/// [kubelogin](https://github.com/int128/kubelogin) (`kubectl oidc-login
/// get-token`), which opens the browser, caches and refreshes the tokens;
/// the `auth-provider: oidc` of old kubeconfigs is deprecated in client-go
/// (ADR 0008).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OidcSpec {
    pub issuer_url: String,
    pub client_id: String,
    /// Empty for public clients.
    pub client_secret: Zeroizing<String>,
    /// Scopes requested besides `openid` (`email`, `groups`, …).
    pub extra_scopes: Vec<String>,
}

/// Plugin command and leading arguments of [`Auth::Oidc`].
pub const OIDC_COMMAND: &str = "kubectl";
const OIDC_SUBCOMMAND: [&str; 2] = ["oidc-login", "get-token"];
/// kubectl plugin binary of kubelogin (`kubectl krew install oidc-login`).
pub const OIDC_PLUGIN_BINARY: &str = "kubectl-oidc_login";
const OIDC_ISSUER_FLAG: &str = "--oidc-issuer-url=";
const OIDC_CLIENT_ID_FLAG: &str = "--oidc-client-id=";
/// Prefix of the argument holding the OIDC client secret (masked in the
/// import preview).
pub const OIDC_CLIENT_SECRET_FLAG: &str = "--oidc-client-secret=";
const OIDC_SCOPE_FLAG: &str = "--oidc-extra-scope=";

impl OidcSpec {
    fn to_exec(&self) -> Result<ExecSpec, FormProblem> {
        let issuer = self.issuer_url.trim();
        if issuer.is_empty() {
            return Err(FormProblem::EmptyIssuer);
        }
        let uri: http::Uri = issuer
            .parse()
            .map_err(|_| FormProblem::InvalidIssuer(issuer.to_owned()))?;
        if uri.scheme_str() != Some("https") || uri.host().is_none_or(str::is_empty) {
            return Err(FormProblem::InvalidIssuer("use an https:// URL".into()));
        }
        let client_id = self.client_id.trim();
        if client_id.is_empty() {
            return Err(FormProblem::EmptyClientId);
        }
        let mut args: Vec<String> = OIDC_SUBCOMMAND.map(String::from).to_vec();
        args.push(format!("{OIDC_ISSUER_FLAG}{issuer}"));
        args.push(format!("{OIDC_CLIENT_ID_FLAG}{client_id}"));
        let secret = self.client_secret.trim();
        if !secret.is_empty() {
            args.push(format!("{OIDC_CLIENT_SECRET_FLAG}{secret}"));
        }
        args.extend(
            self.extra_scopes
                .iter()
                .map(|scope| scope.trim())
                .filter(|scope| !scope.is_empty())
                .map(|scope| format!("{OIDC_SCOPE_FLAG}{scope}")),
        );
        Ok(ExecSpec {
            command: OIDC_COMMAND.into(),
            args,
            env: Vec::new(),
            api_version: DEFAULT_EXEC_API_VERSION.into(),
        })
    }

    /// The OIDC settings of a kubelogin exec plugin written by
    /// [`to_exec`](Self::to_exec) (or by hand in the same shape).
    fn from_exec(exec: &ExecSpec) -> Option<Self> {
        if exec.command != OIDC_COMMAND || !exec.env.is_empty() {
            return None;
        }
        if !matches!(exec.api_version.as_str(), "" | DEFAULT_EXEC_API_VERSION) {
            return None;
        }
        let (head, rest) = exec.args.split_at_checked(OIDC_SUBCOMMAND.len())?;
        if head != OIDC_SUBCOMMAND {
            return None;
        }
        let mut spec = Self::default();
        for arg in rest {
            if let Some(value) = arg.strip_prefix(OIDC_ISSUER_FLAG) {
                spec.issuer_url = value.to_owned();
            } else if let Some(value) = arg.strip_prefix(OIDC_CLIENT_ID_FLAG) {
                spec.client_id = value.to_owned();
            } else if let Some(value) = arg.strip_prefix(OIDC_CLIENT_SECRET_FLAG) {
                spec.client_secret = Zeroizing::new(value.to_owned());
            } else {
                // Other kubelogin flags: keep the plugin as is.
                let value = arg.strip_prefix(OIDC_SCOPE_FLAG)?;
                spec.extra_scopes.push(value.to_owned());
            }
        }
        // Only what the form writes back the same way.
        let mut normalized = exec.clone();
        normalized.api_version = DEFAULT_EXEC_API_VERSION.into();
        (spec.to_exec().ok()? == normalized).then_some(spec)
    }
}

/// Splits the extra scopes field: names separated by spaces or commas.
pub fn split_scopes(text: &str) -> Vec<String> {
    text.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|scope| !scope.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Ready-made exec plugins of managed clusters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecTemplate {
    /// Amazon EKS: `aws eks get-token`.
    Eks,
    /// Google GKE: `gke-gcloud-auth-plugin`.
    Gke,
    /// Azure AKS: `kubelogin get-token` with the Azure CLI login.
    Aks,
    /// Yandex Cloud Managed Kubernetes: `yc k8s create-token` (#38).
    Yandex,
}

/// Server id (application id) of the AKS AAD server, the same for every
/// cluster with AKS-managed Entra ID.
pub const AKS_SERVER_ID: &str = "6dae42f8-4368-4678-94ff-3960e28e3630";

/// What an [`ExecTemplate`] needs; unused fields are ignored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TemplateParams {
    /// EKS cluster name.
    pub cluster_name: String,
    /// EKS region; empty for the profile's default.
    pub region: String,
    /// AWS profile (EKS) or yc profile (Yandex); empty for the default
    /// one.
    pub profile: String,
    /// AKS server id.
    pub server_id: String,
}

impl ExecTemplate {
    pub const ALL: [Self; 4] = [Self::Eks, Self::Gke, Self::Aks, Self::Yandex];

    pub fn label(self) -> &'static str {
        match self {
            Self::Eks => "Amazon EKS",
            Self::Gke => "Google GKE",
            Self::Aks => "Azure AKS",
            Self::Yandex => "Yandex Cloud",
        }
    }

    /// The plugin program.
    pub fn command(self) -> &'static str {
        match self {
            Self::Eks => "aws",
            Self::Gke => "gke-gcloud-auth-plugin",
            Self::Aks => "kubelogin",
            Self::Yandex => "yc",
        }
    }

    /// How to install the plugin, for a "not found" warning.
    pub fn install_hint(self) -> Message {
        match self {
            Self::Eks => msg!("manual-install-eks"),
            Self::Gke => msg!("manual-install-gke"),
            Self::Aks => msg!("manual-install-aks"),
            Self::Yandex => msg!("manual-install-yc"),
        }
    }

    /// Parameters a new form starts with.
    pub fn default_params(self) -> TemplateParams {
        TemplateParams {
            server_id: AKS_SERVER_ID.into(),
            ..TemplateParams::default()
        }
    }

    /// The exec plugin for `params` (as the cloud CLIs write it).
    pub fn exec(self, params: &TemplateParams) -> Result<ExecSpec, FormProblem> {
        let (args, env) = match self {
            Self::Eks => {
                let cluster = params.cluster_name.trim();
                if cluster.is_empty() {
                    return Err(FormProblem::EmptyClusterName);
                }
                let mut args = Vec::new();
                let region = params.region.trim();
                if !region.is_empty() {
                    args.extend(["--region".to_owned(), region.to_owned()]);
                }
                args.extend(
                    [
                        "eks",
                        "get-token",
                        "--cluster-name",
                        cluster,
                        "--output",
                        "json",
                    ]
                    .map(String::from),
                );
                let profile = params.profile.trim();
                let env = if profile.is_empty() {
                    Vec::new()
                } else {
                    vec![("AWS_PROFILE".to_owned(), profile.to_owned())]
                };
                (args, env)
            }
            Self::Gke => (Vec::new(), Vec::new()),
            Self::Aks => {
                let server_id = params.server_id.trim();
                if server_id.is_empty() {
                    return Err(FormProblem::EmptyServerId);
                }
                let args = ["get-token", "--login", "azurecli", "--server-id", server_id]
                    .map(String::from)
                    .to_vec();
                (args, Vec::new())
            }
            // As `yc managed-kubernetes cluster get-credentials` writes it.
            Self::Yandex => {
                let mut args = vec!["k8s".to_owned(), "create-token".to_owned()];
                let profile = params.profile.trim();
                if !profile.is_empty() {
                    args.push(format!("--profile={profile}"));
                }
                (args, Vec::new())
            }
        };
        Ok(ExecSpec {
            command: self.command().into(),
            args,
            env,
            api_version: DEFAULT_EXEC_API_VERSION.into(),
        })
    }

    /// The template and its parameters that give exactly `exec`, if any.
    pub fn detect(exec: &ExecSpec) -> Option<(Self, TemplateParams)> {
        let template = Self::ALL
            .into_iter()
            .find(|template| template.command() == exec.command)?;
        let value_of = |flag: &str| {
            exec.args
                .iter()
                .position(|arg| arg == flag)
                .and_then(|ix| exec.args.get(ix + 1))
                .cloned()
                .unwrap_or_default()
        };
        let params = TemplateParams {
            cluster_name: value_of("--cluster-name"),
            region: value_of("--region"),
            profile: exec
                .env
                .iter()
                .find(|(name, _)| name == "AWS_PROFILE")
                .map(|(_, value)| value.clone())
                .or_else(|| {
                    exec.args
                        .iter()
                        .find_map(|arg| arg.strip_prefix("--profile="))
                        .map(str::to_owned)
                })
                .unwrap_or_default(),
            server_id: value_of("--server-id"),
        };
        let mut normalized = exec.clone();
        if normalized.api_version.is_empty() {
            normalized.api_version = DEFAULT_EXEC_API_VERSION.into();
        }
        (template.exec(&params).ok()? == normalized).then_some((template, params))
    }
}

/// Programs the sign-in runs that are not found, with `PATH` from `env`
/// (the plugin's own environment) or else `path`. Empty for sign-ins
/// without a program.
pub fn missing_commands(auth: &Auth, path: &std::ffi::OsStr) -> Vec<String> {
    let (commands, env): (Vec<&str>, &[(String, String)]) = match auth {
        Auth::Exec(exec) if !exec.command.trim().is_empty() => {
            (vec![exec.command.trim()], &exec.env)
        }
        Auth::Oidc(_) => (vec![OIDC_COMMAND, OIDC_PLUGIN_BINARY], &[]),
        _ => return Vec::new(),
    };
    let path = env
        .iter()
        .find(|(name, _)| name == "PATH")
        .map_or(path, |(_, value)| std::ffi::OsStr::new(value.as_str()));
    commands
        .into_iter()
        .filter(|command| !crate::which::command_exists(command, path))
        .map(str::to_owned)
        .collect()
}

/// An exec credential plugin as entered in the form.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExecSpec {
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Empty means [`DEFAULT_EXEC_API_VERSION`].
    pub api_version: String,
}

/// The form's fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManualCluster {
    /// Name of the context (also used for its cluster and user).
    pub context: String,
    /// API server URL; `https://` is added when no scheme is given.
    pub server: String,
    /// Default namespace of the context; empty for none.
    pub namespace: String,
    pub ca: CaMode,
    pub auth: Auth,
}

/// Form field a [`FormProblem`] belongs to, so the UI shows it there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Context,
    Server,
    Ca,
    Token,
    Cert,
    Key,
    Username,
    Password,
    Issuer,
    ClientId,
    ClusterName,
    ServerId,
    Command,
    Args,
    Env,
}

/// Why the form can't be turned into a kubeconfig.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormProblem {
    EmptyContext,
    EmptyServer,
    InvalidServer(String),
    EmptyCa,
    InvalidCa,
    EmptyToken,
    InvalidCert,
    InvalidKey,
    EmptyUsername,
    EmptyPassword,
    EmptyIssuer,
    InvalidIssuer(String),
    EmptyClientId,
    EmptyClusterName,
    EmptyServerId,
    EmptyCommand,
    /// The arguments end with a backslash.
    TrailingBackslash,
    /// A quote (`'` or `"`) in the arguments is not closed.
    UnclosedQuote(char),
    InvalidEnv(String),
}

impl FormProblem {
    pub fn field(&self) -> Field {
        match self {
            FormProblem::EmptyContext => Field::Context,
            FormProblem::EmptyServer | FormProblem::InvalidServer(_) => Field::Server,
            FormProblem::EmptyCa | FormProblem::InvalidCa => Field::Ca,
            FormProblem::EmptyToken => Field::Token,
            FormProblem::InvalidCert => Field::Cert,
            FormProblem::InvalidKey => Field::Key,
            FormProblem::EmptyUsername => Field::Username,
            FormProblem::EmptyPassword => Field::Password,
            FormProblem::EmptyIssuer | FormProblem::InvalidIssuer(_) => Field::Issuer,
            FormProblem::EmptyClientId => Field::ClientId,
            FormProblem::EmptyClusterName => Field::ClusterName,
            FormProblem::EmptyServerId => Field::ServerId,
            FormProblem::EmptyCommand => Field::Command,
            FormProblem::TrailingBackslash | FormProblem::UnclosedQuote(_) => Field::Args,
            FormProblem::InvalidEnv(_) => Field::Env,
        }
    }

    /// The problem for the screen; [`Display`](fmt::Display) is its English
    /// text.
    pub fn message(&self) -> Message {
        match self {
            FormProblem::EmptyContext => msg!("manual-problem-empty-context"),
            FormProblem::EmptyServer => msg!("manual-problem-empty-server"),
            FormProblem::InvalidServer(error) => {
                msg!("manual-problem-invalid-server", error = error.as_str())
            }
            FormProblem::EmptyCa => msg!("manual-problem-empty-ca"),
            FormProblem::InvalidCa => msg!("manual-problem-invalid-ca"),
            FormProblem::EmptyToken => msg!("manual-problem-empty-token"),
            FormProblem::InvalidCert => msg!("manual-problem-invalid-cert"),
            FormProblem::InvalidKey => msg!("manual-problem-invalid-key"),
            FormProblem::EmptyUsername => msg!("manual-problem-empty-username"),
            FormProblem::EmptyPassword => msg!("manual-problem-empty-password"),
            FormProblem::EmptyIssuer => msg!("manual-problem-empty-issuer"),
            FormProblem::InvalidIssuer(error) => {
                msg!("manual-problem-invalid-issuer", error = error.as_str())
            }
            FormProblem::EmptyClientId => msg!("manual-problem-empty-client-id"),
            FormProblem::EmptyClusterName => msg!("manual-problem-empty-cluster-name"),
            FormProblem::EmptyServerId => msg!("manual-problem-empty-server-id"),
            FormProblem::EmptyCommand => msg!("manual-problem-empty-command"),
            FormProblem::TrailingBackslash => msg!("manual-problem-trailing-backslash"),
            FormProblem::UnclosedQuote(quote) => {
                msg!("manual-problem-unclosed-quote", quote = quote.to_string())
            }
            FormProblem::InvalidEnv(entry) => {
                msg!("manual-problem-invalid-env", entry = entry.as_str())
            }
        }
    }
}

impl fmt::Display for FormProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message().english())
    }
}

impl std::error::Error for FormProblem {}

/// A kubeconfig feature the form has no field for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    CaFile,
    Proxy,
    TlsServerName,
    BasicWithOthers,
    AuthProvider,
    CredentialFiles,
    Impersonation,
}

/// Why a kubeconfig can't be shown in the form (it stays editable as YAML).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotSimple {
    Invalid(String),
    Contexts(usize),
    MissingCluster,
    Unsupported(Feature),
}

impl NotSimple {
    /// The reason for the screen; [`Display`](fmt::Display) is its English
    /// text.
    pub fn message(&self) -> Message {
        match self {
            NotSimple::Invalid(error) => msg!("manual-yaml-invalid", error = error.as_str()),
            NotSimple::Contexts(count) => msg!("manual-yaml-contexts", count = *count),
            NotSimple::MissingCluster => msg!("manual-yaml-missing-cluster"),
            NotSimple::Unsupported(feature) => {
                let feature = match feature {
                    Feature::CaFile => msg!("manual-feature-ca-file"),
                    Feature::Proxy => msg!("manual-feature-proxy"),
                    Feature::TlsServerName => msg!("manual-feature-tls-server-name"),
                    Feature::BasicWithOthers => msg!("manual-feature-basic-with-others"),
                    Feature::AuthProvider => msg!("manual-feature-auth-provider"),
                    Feature::CredentialFiles => msg!("manual-feature-credential-files"),
                    Feature::Impersonation => msg!("manual-feature-impersonation"),
                };
                msg!("manual-yaml-unsupported", feature = feature)
            }
        }
    }
}

impl fmt::Display for NotSimple {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message().english())
    }
}

impl std::error::Error for NotSimple {}

impl ManualCluster {
    /// The kubeconfig the form describes: one context, cluster and user,
    /// all named after the context, with certificates inlined.
    pub fn to_kubeconfig(&self) -> Result<Kubeconfig, FormProblem> {
        let name = self.context.trim();
        if name.is_empty() {
            return Err(FormProblem::EmptyContext);
        }
        let server = normalize_server(&self.server)?;
        let mut cluster = Cluster {
            server: Some(server),
            ..Cluster::default()
        };
        match &self.ca {
            CaMode::System => {}
            CaMode::Pem(text) if text.trim().is_empty() => return Err(FormProblem::EmptyCa),
            CaMode::Pem(text) => {
                cluster.certificate_authority_data =
                    Some(pem_data(text, PemKind::Certificate).ok_or(FormProblem::InvalidCa)?);
            }
            CaMode::InsecureSkipVerify => cluster.insecure_skip_tls_verify = Some(true),
        }
        let mut user = AuthInfo::default();
        match &self.auth {
            Auth::Token(token) => {
                let token = token.trim();
                if token.is_empty() {
                    return Err(FormProblem::EmptyToken);
                }
                user.token = Some(SecretString::from(token.to_owned()));
            }
            Auth::ClientCert { cert, key } => {
                user.client_certificate_data =
                    Some(pem_data(cert, PemKind::Certificate).ok_or(FormProblem::InvalidCert)?);
                let key = pem_data(key, PemKind::PrivateKey).ok_or(FormProblem::InvalidKey)?;
                user.client_key_data = Some(SecretString::from(key));
            }
            Auth::Basic { username, password } => {
                let username = username.trim();
                if username.is_empty() {
                    return Err(FormProblem::EmptyUsername);
                }
                // The password is taken as typed: spaces may be part of it.
                if password.is_empty() {
                    return Err(FormProblem::EmptyPassword);
                }
                user.username = Some(username.to_owned());
                user.password = Some(SecretString::from(password.to_string()));
            }
            Auth::Oidc(oidc) => user.exec = Some(exec_config(&oidc.to_exec()?)?),
            Auth::Exec(exec) => user.exec = Some(exec_config(exec)?),
        }
        let namespace = self.namespace.trim();
        Ok(Kubeconfig {
            api_version: Some("v1".into()),
            kind: Some("Config".into()),
            current_context: Some(name.to_owned()),
            clusters: vec![NamedCluster {
                name: name.to_owned(),
                cluster: Some(cluster),
                ..NamedCluster::default()
            }],
            auth_infos: vec![NamedAuthInfo {
                name: name.to_owned(),
                auth_info: Some(user),
                ..NamedAuthInfo::default()
            }],
            contexts: vec![NamedContext {
                name: name.to_owned(),
                context: Some(Context {
                    cluster: name.to_owned(),
                    user: Some(name.to_owned()),
                    namespace: (!namespace.is_empty()).then(|| namespace.to_owned()),
                    ..Context::default()
                }),
                ..NamedContext::default()
            }],
            ..Kubeconfig::default()
        })
    }

    /// [`to_kubeconfig`](Self::to_kubeconfig) as YAML.
    pub fn to_yaml(&self) -> Result<Zeroizing<String>, FormProblem> {
        let config = self.to_kubeconfig()?;
        // Serializing plain structs of strings can't fail.
        Ok(Zeroizing::new(
            serde_saphyr::to_string(&config).unwrap_or_default(),
        ))
    }

    /// The form for a one-context kubeconfig in YAML, e.g. when switching
    /// from the YAML tab back to the form.
    pub fn from_yaml(yaml: &str) -> Result<Self, NotSimple> {
        let config =
            Kubeconfig::from_yaml(yaml).map_err(|error| NotSimple::Invalid(error.to_string()))?;
        Self::from_kubeconfig(&config)
    }

    /// See [`from_yaml`](Self::from_yaml). Configs with several contexts,
    /// files referenced by path or auth providers don't fit. A kubelogin
    /// exec plugin in the form's shape comes back as [`Auth::Oidc`].
    pub fn from_kubeconfig(config: &Kubeconfig) -> Result<Self, NotSimple> {
        let [named] = config.contexts.as_slice() else {
            return Err(NotSimple::Contexts(config.contexts.len()));
        };
        let context = named.context.clone().unwrap_or_default();
        let cluster = config
            .clusters
            .iter()
            .find(|c| c.name == context.cluster)
            .and_then(|c| c.cluster.clone())
            .ok_or(NotSimple::MissingCluster)?;
        if cluster.certificate_authority.is_some() {
            return Err(NotSimple::Unsupported(Feature::CaFile));
        }
        if cluster.proxy_url.is_some() {
            return Err(NotSimple::Unsupported(Feature::Proxy));
        }
        if cluster.tls_server_name.is_some() {
            return Err(NotSimple::Unsupported(Feature::TlsServerName));
        }
        let ca = if cluster.insecure_skip_tls_verify == Some(true) {
            CaMode::InsecureSkipVerify
        } else if let Some(data) = &cluster.certificate_authority_data {
            CaMode::Pem(data_pem(data).unwrap_or_else(|| data.clone()))
        } else {
            CaMode::System
        };
        let user = match &context.user {
            Some(name) => config
                .auth_infos
                .iter()
                .find(|u| &u.name == name)
                .and_then(|u| u.auth_info.clone())
                .unwrap_or_default(),
            None => AuthInfo::default(),
        };
        let auth = auth_of(&user)?;
        Ok(Self {
            context: named.name.clone(),
            server: cluster.server.unwrap_or_default(),
            namespace: context.namespace.unwrap_or_default(),
            ca,
            auth,
        })
    }
}

pub(crate) fn exec_config(exec: &ExecSpec) -> Result<ExecConfig, FormProblem> {
    let command = exec.command.trim();
    if command.is_empty() {
        return Err(FormProblem::EmptyCommand);
    }
    let api_version = match exec.api_version.trim() {
        "" => DEFAULT_EXEC_API_VERSION,
        version => version,
    };
    let env = exec
        .env
        .iter()
        .map(|(name, value)| {
            HashMap::from([
                ("name".to_owned(), name.clone()),
                ("value".to_owned(), value.clone()),
            ])
        })
        .collect::<Vec<_>>();
    Ok(ExecConfig {
        api_version: Some(api_version.to_owned()),
        command: Some(command.to_owned()),
        args: (!exec.args.is_empty()).then(|| exec.args.clone()),
        env: (!env.is_empty()).then_some(env),
        ..ExecConfig::default()
    })
}

fn auth_of(user: &AuthInfo) -> Result<Auth, NotSimple> {
    let other_credentials = user.token.is_some()
        || user.client_certificate_data.is_some()
        || user.client_key_data.is_some()
        || user.exec.is_some()
        || user.auth_provider.is_some();
    if user.username.is_some() || user.password.is_some() {
        if other_credentials {
            return Err(NotSimple::Unsupported(Feature::BasicWithOthers));
        }
        return Ok(Auth::Basic {
            username: user.username.clone().unwrap_or_default(),
            password: Zeroizing::new(
                user.password
                    .as_ref()
                    .map(|password| password.expose_secret().to_owned())
                    .unwrap_or_default(),
            ),
        });
    }
    if user.auth_provider.is_some() {
        return Err(NotSimple::Unsupported(Feature::AuthProvider));
    }
    if user.token_file.is_some() || user.client_certificate.is_some() || user.client_key.is_some() {
        return Err(NotSimple::Unsupported(Feature::CredentialFiles));
    }
    if user.impersonate.is_some() || user.impersonate_groups.is_some() {
        return Err(NotSimple::Unsupported(Feature::Impersonation));
    }
    if let Some(exec) = &user.exec {
        let env = exec
            .env
            .iter()
            .flatten()
            .map(|pair| {
                (
                    pair.get("name").cloned().unwrap_or_default(),
                    pair.get("value").cloned().unwrap_or_default(),
                )
            })
            .collect();
        let spec = ExecSpec {
            command: exec.command.clone().unwrap_or_default(),
            args: exec.args.clone().unwrap_or_default(),
            env,
            api_version: exec.api_version.clone().unwrap_or_default(),
        };
        return Ok(match OidcSpec::from_exec(&spec) {
            Some(oidc) => Auth::Oidc(oidc),
            None => Auth::Exec(spec),
        });
    }
    if let Some(cert) = &user.client_certificate_data {
        let key = user
            .client_key_data
            .as_ref()
            .map(|key| key.expose_secret().to_owned())
            .unwrap_or_default();
        let key = Zeroizing::new(key);
        return Ok(Auth::ClientCert {
            cert: data_pem(cert).unwrap_or_else(|| cert.clone()),
            key: Zeroizing::new(data_pem(&key).unwrap_or_else(|| key.to_string())),
        });
    }
    let token = user
        .token
        .as_ref()
        .map(|token| token.expose_secret().to_owned())
        .unwrap_or_default();
    Ok(Auth::Token(Zeroizing::new(token)))
}

/// The API server URL with `https://` added when no scheme is given and no
/// trailing slash.
pub fn normalize_server(server: &str) -> Result<String, FormProblem> {
    let server = server.trim();
    if server.is_empty() {
        return Err(FormProblem::EmptyServer);
    }
    let url = if server.contains("://") {
        server.to_owned()
    } else {
        format!("https://{server}")
    };
    let invalid = |why: &str| FormProblem::InvalidServer(why.to_owned());
    let uri: http::Uri = url.parse().map_err(|_| invalid(server))?;
    match uri.scheme_str() {
        Some("https" | "http") => {}
        _ => return Err(invalid("use https:// or http://")),
    }
    if uri.host().is_none_or(str::is_empty) {
        return Err(invalid("no host"));
    }
    Ok(url.trim_end_matches('/').to_owned())
}

#[derive(Clone, Copy)]
enum PemKind {
    Certificate,
    PrivateKey,
}

impl PemKind {
    fn matches(self, pem: &str) -> bool {
        match self {
            PemKind::Certificate => pem.contains("-----BEGIN CERTIFICATE-----"),
            PemKind::PrivateKey => pem
                .lines()
                .any(|line| line.starts_with("-----BEGIN ") && line.ends_with("PRIVATE KEY-----")),
        }
    }
}

/// The base64 kubeconfig stores for a PEM block, which is either pasted as
/// is or already base64 (copied from another kubeconfig).
fn pem_data(text: &str, kind: PemKind) -> Option<String> {
    let engine = base64::engine::general_purpose::STANDARD;
    let text = text.trim();
    if kind.matches(text) {
        return Some(engine.encode(format!("{text}\n")));
    }
    let compact: String = text.split_whitespace().collect();
    let decoded = Zeroizing::new(engine.decode(&compact).ok()?);
    kind.matches(std::str::from_utf8(&decoded).ok()?)
        .then_some(compact)
}

/// PEM text of kubeconfig `*-data`, when it decodes to text.
fn data_pem(data: &str) -> Option<String> {
    let engine = base64::engine::general_purpose::STANDARD;
    let compact: String = data.split_whitespace().collect();
    let decoded = engine.decode(compact).ok()?;
    let text = String::from_utf8(decoded).ok()?;
    text.contains("-----BEGIN").then_some(text)
}

/// Splits a command line into arguments: whitespace separates them, single
/// and double quotes group, a backslash escapes the next character (except
/// inside single quotes).
pub fn split_args(line: &str) -> Result<Vec<String>, FormProblem> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_arg = false;
    let mut quote: Option<char> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (Some('\''), c) => current.push(c),
            (_, '\\') => {
                let Some(next) = chars.next() else {
                    return Err(FormProblem::TrailingBackslash);
                };
                current.push(next);
                in_arg = true;
            }
            (Some(_), c) => current.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                in_arg = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_arg {
                    args.push(std::mem::take(&mut current));
                    in_arg = false;
                }
            }
            (None, c) => {
                current.push(c);
                in_arg = true;
            }
        }
    }
    if let Some(quote) = quote {
        return Err(FormProblem::UnclosedQuote(quote));
    }
    if in_arg {
        args.push(current);
    }
    Ok(args)
}

/// Inverse of [`split_args`]: arguments joined with spaces, quoted when they
/// need it.
pub fn join_args(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            let plain = !arg.is_empty()
                && !arg
                    .chars()
                    .any(|c| c.is_whitespace() || matches!(c, '\'' | '"' | '\\'));
            if plain {
                arg.clone()
            } else {
                format!("'{}'", arg.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `NAME=value` pairs separated by whitespace (quotes as in
/// [`split_args`]).
pub fn parse_env(line: &str) -> Result<Vec<(String, String)>, FormProblem> {
    split_args(line)
        .map_err(|_| FormProblem::InvalidEnv(line.trim().to_owned()))?
        .into_iter()
        .map(|pair| match pair.split_once('=') {
            Some((name, value)) if valid_env_name(name) => Ok((name.to_owned(), value.to_owned())),
            _ => Err(FormProblem::InvalidEnv(pair)),
        })
        .collect()
}

/// Inverse of [`parse_env`].
pub fn join_env(env: &[(String, String)]) -> String {
    let pairs: Vec<String> = env
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    join_args(&pairs)
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::vault::ImportPlan;

    #[test]
    fn problems_are_messages_with_english_display() {
        use kubach_i18n::Locale;
        let problem = split_args("a 'b").unwrap_err();
        assert_eq!(problem, FormProblem::UnclosedQuote('\''));
        assert_eq!(problem.field(), Field::Args);
        assert_eq!(problem.to_string(), "Unclosed ' quote");
        assert_eq!(
            problem.message().translate(Locale::Ru),
            "Не закрыта кавычка '"
        );
        assert_eq!(
            split_args("a\\").unwrap_err(),
            FormProblem::TrailingBackslash
        );
        let unsupported = NotSimple::Unsupported(Feature::Proxy);
        assert_eq!(
            unsupported.to_string(),
            "The config uses a proxy, which the form can't show"
        );
        assert_eq!(
            unsupported.message().translate(Locale::Ru),
            "В конфиге есть прокси, а форма такое не показывает"
        );
        assert_eq!(
            ExecTemplate::Aks.install_hint().english(),
            "Install kubelogin (az aks install-cli) and sign in with az login."
        );
    }

    const CA: &str =
        "-----BEGIN CERTIFICATE-----\nMIIBszCCAVmgAwIBAgIUQ0E=\n-----END CERTIFICATE-----";
    const KEY: &str =
        "-----BEGIN EC PRIVATE KEY-----\nMHcCAQEEIEtleQ==\n-----END EC PRIVATE KEY-----";

    fn form(auth: Auth) -> ManualCluster {
        ManualCluster {
            context: " dev ".into(),
            server: "dev.example.com:6443/".into(),
            namespace: String::new(),
            ca: CaMode::Pem(CA.into()),
            auth,
        }
    }

    fn token(text: &str) -> Auth {
        Auth::Token(Zeroizing::new(text.into()))
    }

    fn basic(username: &str, password: &str) -> Auth {
        Auth::Basic {
            username: username.into(),
            password: Zeroizing::new(password.into()),
        }
    }

    fn oidc() -> OidcSpec {
        OidcSpec {
            issuer_url: "https://dex.example.com".into(),
            client_id: "kubernetes".into(),
            client_secret: Zeroizing::new("s3cret".into()),
            extra_scopes: vec!["email".into(), "groups".into()],
        }
    }

    fn user_of(cluster: &ManualCluster) -> AuthInfo {
        let config = cluster.to_kubeconfig().unwrap();
        config.auth_infos[0].auth_info.clone().unwrap()
    }

    #[test]
    fn basic_auth_writes_username_and_password() {
        let user = user_of(&form(basic(" admin ", " pa ss ")));
        assert_eq!(user.username.as_deref(), Some("admin"));
        // Spaces may belong to a password.
        assert_eq!(user.password.unwrap().expose_secret(), " pa ss ");
        assert!(user.token.is_none());
        assert_eq!(
            form(basic(" ", "p")).to_kubeconfig().unwrap_err().field(),
            Field::Username
        );
        assert_eq!(
            form(basic("admin", "")).to_kubeconfig().unwrap_err(),
            FormProblem::EmptyPassword
        );
    }

    #[test]
    fn basic_auth_mixed_with_other_credentials_stays_yaml() {
        let yaml = r#"
apiVersion: v1
kind: Config
clusters:
  - { name: c, cluster: { server: "https://c" } }
users:
  - { name: u, user: { username: a, password: b, token: t } }
contexts:
  - { name: a, context: { cluster: c, user: u } }
"#;
        assert!(matches!(
            ManualCluster::from_yaml(yaml),
            Err(NotSimple::Unsupported(_))
        ));
    }

    #[test]
    fn oidc_becomes_a_kubelogin_exec_plugin() {
        let user = user_of(&form(Auth::Oidc(oidc())));
        let exec = user.exec.unwrap();
        assert_eq!(exec.command.as_deref(), Some("kubectl"));
        assert_eq!(exec.api_version.as_deref(), Some(DEFAULT_EXEC_API_VERSION));
        assert_eq!(
            exec.args.unwrap(),
            [
                "oidc-login",
                "get-token",
                "--oidc-issuer-url=https://dex.example.com",
                "--oidc-client-id=kubernetes",
                "--oidc-client-secret=s3cret",
                "--oidc-extra-scope=email",
                "--oidc-extra-scope=groups",
            ]
        );
        // The import preview doesn't show the client secret.
        let yaml = form(Auth::Oidc(oidc())).to_yaml().unwrap();
        let plan = ImportPlan::from_yaml(&yaml, &HashSet::new()).unwrap();
        let line = plan.review.programs[0].command_line();
        assert!(!line.contains("s3cret"), "{line}");
        assert!(line.contains("--oidc-client-secret=•••"), "{line}");
        let public = OidcSpec {
            client_secret: Zeroizing::new("  ".into()),
            extra_scopes: split_scopes(" email, groups "),
            ..oidc()
        };
        let args = user_of(&form(Auth::Oidc(public)))
            .exec
            .unwrap()
            .args
            .unwrap();
        assert!(!args.iter().any(|a| a.starts_with(OIDC_CLIENT_SECRET_FLAG)));
        assert_eq!(args.len(), 6);
    }

    #[test]
    fn oidc_problems_point_at_their_field() {
        let with = |spec: OidcSpec| form(Auth::Oidc(spec)).to_kubeconfig().unwrap_err();
        let empty = with(OidcSpec {
            issuer_url: " ".into(),
            ..oidc()
        });
        assert_eq!(empty, FormProblem::EmptyIssuer);
        let http = with(OidcSpec {
            issuer_url: "http://dex".into(),
            ..oidc()
        });
        assert_eq!(http.field(), Field::Issuer);
        let no_client = with(OidcSpec {
            client_id: String::new(),
            ..oidc()
        });
        assert_eq!(no_client, FormProblem::EmptyClientId);
    }

    #[test]
    fn other_kubelogin_flags_stay_a_plain_exec_plugin() {
        let yaml = r#"
apiVersion: v1
kind: Config
clusters:
  - { name: c, cluster: { server: "https://c" } }
users:
  - name: u
    user:
      exec:
        apiVersion: client.authentication.k8s.io/v1beta1
        command: kubectl
        args: [oidc-login, get-token, --oidc-issuer-url=https://i, --oidc-client-id=k, --grant-type=device-code]
contexts:
  - { name: a, context: { cluster: c, user: u } }
"#;
        let cluster = ManualCluster::from_yaml(yaml).unwrap();
        assert!(matches!(cluster.auth, Auth::Exec(_)));
    }

    #[test]
    fn templates_write_what_the_cloud_clis_write() {
        let params = TemplateParams {
            cluster_name: "prod".into(),
            region: "eu-west-1".into(),
            profile: "ops".into(),
            ..ExecTemplate::Eks.default_params()
        };
        let eks = ExecTemplate::Eks.exec(&params).unwrap();
        assert_eq!(eks.command, "aws");
        assert_eq!(
            eks.args,
            [
                "--region",
                "eu-west-1",
                "eks",
                "get-token",
                "--cluster-name",
                "prod",
                "--output",
                "json"
            ]
        );
        assert_eq!(eks.env, [("AWS_PROFILE".to_owned(), "ops".to_owned())]);
        assert_eq!(eks.api_version, DEFAULT_EXEC_API_VERSION);
        let plain = TemplateParams {
            cluster_name: "prod".into(),
            ..TemplateParams::default()
        };
        let eks_plain = ExecTemplate::Eks.exec(&plain).unwrap();
        assert_eq!(eks_plain.args[0], "eks");
        assert!(eks_plain.env.is_empty());
        assert_eq!(
            ExecTemplate::Eks
                .exec(&TemplateParams::default())
                .unwrap_err()
                .field(),
            Field::ClusterName
        );

        let gke = ExecTemplate::Gke
            .exec(&ExecTemplate::Gke.default_params())
            .unwrap();
        assert_eq!(gke.command, "gke-gcloud-auth-plugin");
        assert!(gke.args.is_empty());

        let aks = ExecTemplate::Aks
            .exec(&ExecTemplate::Aks.default_params())
            .unwrap();
        assert_eq!(aks.command, "kubelogin");
        assert_eq!(
            aks.args,
            [
                "get-token",
                "--login",
                "azurecli",
                "--server-id",
                AKS_SERVER_ID
            ]
        );
        assert_eq!(
            ExecTemplate::Aks
                .exec(&TemplateParams::default())
                .unwrap_err(),
            FormProblem::EmptyServerId
        );

        // Each builds a kubeconfig.
        for spec in [eks, gke, aks] {
            let mut cluster = form(Auth::Exec(spec.clone()));
            cluster.ca = CaMode::System;
            let exec = user_of(&cluster).exec.unwrap();
            assert_eq!(exec.command.as_deref(), Some(spec.command.as_str()));
        }
    }

    #[test]
    fn templates_are_detected_back() {
        for (template, params) in [
            (
                ExecTemplate::Eks,
                TemplateParams {
                    cluster_name: "prod".into(),
                    region: "eu-west-1".into(),
                    profile: "ops".into(),
                    server_id: String::new(),
                },
            ),
            (
                ExecTemplate::Eks,
                TemplateParams {
                    cluster_name: "prod".into(),
                    ..TemplateParams::default()
                },
            ),
            (ExecTemplate::Gke, TemplateParams::default()),
            (
                ExecTemplate::Aks,
                TemplateParams {
                    server_id: AKS_SERVER_ID.into(),
                    ..TemplateParams::default()
                },
            ),
        ] {
            let mut exec = template.exec(&params).unwrap();
            assert_eq!(ExecTemplate::detect(&exec), Some((template, params)));
            exec.api_version.clear();
            assert!(ExecTemplate::detect(&exec).is_some());
            exec.args.push("--extra".into());
            assert_eq!(ExecTemplate::detect(&exec), None);
        }
    }

    #[test]
    fn missing_commands_are_looked_up_in_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().as_os_str();
        let exec = |command: &str, env: Vec<(String, String)>| {
            Auth::Exec(ExecSpec {
                command: command.into(),
                env,
                ..ExecSpec::default()
            })
        };
        assert_eq!(missing_commands(&exec("aws", vec![]), path), ["aws"]);
        assert_eq!(
            missing_commands(&Auth::Oidc(oidc()), path),
            ["kubectl", "kubectl-oidc_login"]
        );
        assert!(missing_commands(&token("t"), path).is_empty());
        assert!(missing_commands(&exec(" ", vec![]), path).is_empty());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let aws = dir.path().join("aws");
            std::fs::write(&aws, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&aws, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(missing_commands(&exec("aws", vec![]), path).is_empty());
            let full = aws.to_string_lossy().into_owned();
            assert!(missing_commands(&exec(&full, vec![]), "".as_ref()).is_empty());
            // The plugin's own PATH wins.
            let other = vec![("PATH".to_owned(), "/nonexistent".to_owned())];
            assert_eq!(missing_commands(&exec("aws", other), path), ["aws"]);
        }
    }

    #[test]
    fn token_form_becomes_a_one_context_kubeconfig() {
        let config = form(token("t0k3n")).to_kubeconfig().unwrap();
        assert_eq!(config.current_context.as_deref(), Some("dev"));
        assert_eq!(config.contexts.len(), 1);
        let context = config.contexts[0].context.as_ref().unwrap();
        assert_eq!(context.cluster, "dev");
        assert_eq!(context.user.as_deref(), Some("dev"));
        assert_eq!(context.namespace, None);
        let cluster = config.clusters[0].cluster.as_ref().unwrap();
        assert_eq!(
            cluster.server.as_deref(),
            Some("https://dev.example.com:6443")
        );
        let data = cluster.certificate_authority_data.as_deref().unwrap();
        assert_eq!(data_pem(data).unwrap(), format!("{CA}\n"));
        let user = config.auth_infos[0].auth_info.as_ref().unwrap();
        assert_eq!(user.token.as_ref().unwrap().expose_secret(), "t0k3n");
    }

    #[test]
    fn yaml_goes_through_the_import_plan() {
        let mut cluster = form(token("t0k3n"));
        cluster.namespace = "web".into();
        let yaml = cluster.to_yaml().unwrap();
        assert!(yaml.contains("t0k3n"));
        let taken: HashSet<String> = ["dev".to_owned()].into();
        let plan = ImportPlan::from_yaml(&yaml, &taken).unwrap();
        assert_eq!(plan.context_names(), ["dev-imported"]);
        assert_eq!(
            plan.contexts[0].server.as_deref(),
            Some("https://dev.example.com:6443")
        );
        assert!(plan.review.programs.is_empty());
    }

    #[test]
    fn client_certificate_accepts_pem_or_base64() {
        let engine = base64::engine::general_purpose::STANDARD;
        let auth = Auth::ClientCert {
            cert: engine.encode(CA),
            key: Zeroizing::new(KEY.into()),
        };
        let config = form(auth).to_kubeconfig().unwrap();
        let user = config.auth_infos[0].auth_info.as_ref().unwrap();
        assert_eq!(
            user.client_certificate_data.as_deref(),
            Some(engine.encode(CA).as_str())
        );
        let key = user.client_key_data.as_ref().unwrap().expose_secret();
        assert_eq!(data_pem(key).unwrap(), format!("{KEY}\n"));

        let bad_key = Auth::ClientCert {
            cert: CA.into(),
            key: Zeroizing::new(CA.into()),
        };
        assert_eq!(
            form(bad_key).to_kubeconfig().unwrap_err(),
            FormProblem::InvalidKey
        );
        let bad_cert = Auth::ClientCert {
            cert: "hello".into(),
            key: Zeroizing::new(KEY.into()),
        };
        assert_eq!(
            form(bad_cert).to_kubeconfig().unwrap_err(),
            FormProblem::InvalidCert
        );
    }

    #[test]
    fn exec_plugin_with_args_and_env() {
        let exec = ExecSpec {
            command: "aws".into(),
            args: split_args("eks get-token --cluster-name 'my prod'").unwrap(),
            env: parse_env("AWS_PROFILE=prod AWS_REGION=eu-west-1").unwrap(),
            api_version: String::new(),
        };
        let mut cluster = form(Auth::Exec(exec.clone()));
        cluster.ca = CaMode::System;
        let config = cluster.to_kubeconfig().unwrap();
        let user = config.auth_infos[0].auth_info.as_ref().unwrap();
        let plugin = user.exec.as_ref().unwrap();
        assert_eq!(plugin.command.as_deref(), Some("aws"));
        assert_eq!(
            plugin.api_version.as_deref(),
            Some(DEFAULT_EXEC_API_VERSION)
        );
        assert_eq!(
            plugin.args.as_deref().unwrap(),
            ["eks", "get-token", "--cluster-name", "my prod"]
        );
        let env = plugin.env.as_ref().unwrap();
        assert_eq!(env[0]["name"], "AWS_PROFILE");
        assert_eq!(env[1]["value"], "eu-west-1");
        let cluster_config = config.clusters[0].cluster.as_ref().unwrap();
        assert_eq!(cluster_config.certificate_authority_data, None);

        // The import preview lists the command.
        let plan = ImportPlan::from_yaml(&cluster.to_yaml().unwrap(), &HashSet::new()).unwrap();
        assert_eq!(
            plan.review.programs[0].command_line(),
            "aws eks get-token --cluster-name my prod"
        );
    }

    #[test]
    fn insecure_skip_verify_is_written() {
        let mut cluster = form(token("t"));
        cluster.ca = CaMode::InsecureSkipVerify;
        let config = cluster.to_kubeconfig().unwrap();
        let c = config.clusters[0].cluster.as_ref().unwrap();
        assert_eq!(c.insecure_skip_tls_verify, Some(true));
        assert_eq!(c.certificate_authority_data, None);
    }

    #[test]
    fn form_problems_point_at_their_field() {
        let mut cluster = form(token("t"));
        cluster.context = "  ".into();
        assert_eq!(cluster.to_kubeconfig().unwrap_err().field(), Field::Context);
        cluster.context = "dev".into();
        cluster.server = String::new();
        assert_eq!(
            cluster.to_kubeconfig().unwrap_err(),
            FormProblem::EmptyServer
        );
        cluster.server = "ftp://dev".into();
        assert_eq!(cluster.to_kubeconfig().unwrap_err().field(), Field::Server);
        cluster.server = "https://dev".into();
        cluster.ca = CaMode::Pem("  ".into());
        assert_eq!(cluster.to_kubeconfig().unwrap_err(), FormProblem::EmptyCa);
        cluster.ca = CaMode::Pem("not a cert".into());
        assert_eq!(cluster.to_kubeconfig().unwrap_err(), FormProblem::InvalidCa);
        cluster.ca = CaMode::System;
        cluster.auth = token(" ");
        assert_eq!(
            cluster.to_kubeconfig().unwrap_err(),
            FormProblem::EmptyToken
        );
        cluster.auth = Auth::Exec(ExecSpec::default());
        assert_eq!(
            cluster.to_kubeconfig().unwrap_err(),
            FormProblem::EmptyCommand
        );
    }

    #[test]
    fn server_urls_are_normalized() {
        assert_eq!(
            normalize_server(" 10.0.0.1:6443 ").unwrap(),
            "https://10.0.0.1:6443"
        );
        assert_eq!(
            normalize_server("http://localhost:8080/").unwrap(),
            "http://localhost:8080"
        );
        assert_eq!(
            normalize_server("https://[::1]:6443").unwrap(),
            "https://[::1]:6443"
        );
        assert!(normalize_server("https://").is_err());
        assert!(normalize_server("https://bad host").is_err());
        assert!(normalize_server("unix:///var/run").is_err());
    }

    #[test]
    fn args_split_like_a_shell() {
        assert_eq!(
            split_args(r#"  a "b c" 'd "e"' f\ g "" "#).unwrap(),
            ["a", "b c", r#"d "e""#, "f g", ""]
        );
        assert!(split_args("'open").is_err());
        assert!(split_args("x\\").is_err());
        assert_eq!(split_args("   ").unwrap(), Vec::<String>::new());
        let args: Vec<String> = ["get-token", "my prod", "it's", ""]
            .map(String::from)
            .to_vec();
        assert_eq!(split_args(&join_args(&args)).unwrap(), args);
    }

    #[test]
    fn env_pairs_are_parsed() {
        assert_eq!(
            parse_env("A=1 B='x y' C=").unwrap(),
            [
                ("A".to_owned(), "1".to_owned()),
                ("B".to_owned(), "x y".to_owned()),
                ("C".to_owned(), String::new())
            ]
        );
        assert_eq!(
            parse_env("A=1 nope").unwrap_err(),
            FormProblem::InvalidEnv("nope".into())
        );
        assert!(parse_env("1A=x").is_err());
        let env = parse_env("A=1 B='x y'").unwrap();
        assert_eq!(parse_env(&join_env(&env)).unwrap(), env);
    }

    #[test]
    fn simple_yaml_round_trips_through_the_form() {
        for auth in [
            token("t0k3n"),
            Auth::ClientCert {
                cert: format!("{CA}\n"),
                key: Zeroizing::new(format!("{KEY}\n")),
            },
            basic("admin", "pa ss"),
            Auth::Oidc(oidc()),
            Auth::Oidc(OidcSpec {
                client_secret: Zeroizing::new(String::new()),
                extra_scopes: Vec::new(),
                ..oidc()
            }),
            Auth::Exec(ExecSpec {
                command: "kubelogin".into(),
                args: vec!["get-token".into()],
                env: vec![("A".into(), "1".into())],
                api_version: "client.authentication.k8s.io/v1".into(),
            }),
        ] {
            let mut cluster = form(auth);
            cluster.context = "dev".into();
            cluster.server = "https://dev.example.com:6443".into();
            cluster.namespace = "web".into();
            cluster.ca = CaMode::Pem(format!("{CA}\n"));
            let back = ManualCluster::from_yaml(&cluster.to_yaml().unwrap()).unwrap();
            assert_eq!(back, cluster);
        }
    }

    #[test]
    fn complex_yaml_stays_yaml() {
        let two = r#"
apiVersion: v1
kind: Config
clusters:
  - { name: c, cluster: { server: "https://c" } }
contexts:
  - { name: a, context: { cluster: c } }
  - { name: b, context: { cluster: c } }
"#;
        assert_eq!(ManualCluster::from_yaml(two), Err(NotSimple::Contexts(2)));
        let ca_file = r#"
apiVersion: v1
kind: Config
clusters:
  - { name: c, cluster: { server: "https://c", certificate-authority: /ca.crt } }
contexts:
  - { name: a, context: { cluster: c } }
"#;
        assert!(matches!(
            ManualCluster::from_yaml(ca_file),
            Err(NotSimple::Unsupported(_))
        ));
        let missing =
            "apiVersion: v1\nkind: Config\ncontexts:\n  - { name: a, context: { cluster: c } }\n";
        assert_eq!(
            ManualCluster::from_yaml(missing),
            Err(NotSimple::MissingCluster)
        );
        assert!(matches!(
            ManualCluster::from_yaml("contexts: [oops: {"),
            Err(NotSimple::Invalid(_))
        ));
    }

    #[test]
    fn yandex_template_like_yc_get_credentials() {
        let params = TemplateParams {
            profile: "prod".into(),
            ..ExecTemplate::Yandex.default_params()
        };
        let yc = ExecTemplate::Yandex.exec(&params).unwrap();
        assert_eq!(yc.command, "yc");
        assert_eq!(yc.args, ["k8s", "create-token", "--profile=prod"]);
        assert!(yc.env.is_empty());
        let (template, detected) = ExecTemplate::detect(&yc).unwrap();
        assert_eq!(template, ExecTemplate::Yandex);
        assert_eq!(detected.profile, "prod");
        let plain = ExecTemplate::Yandex
            .exec(&ExecTemplate::Yandex.default_params())
            .unwrap();
        assert_eq!(plain.args, ["k8s", "create-token"]);
    }
}
