//! "Test connection": asks the API server for its version and turns a
//! failure into words for the user (bad token, unknown CA, wrong address…)
//! instead of a chain of library errors.

use std::error::Error as StdError;
use std::io;
use std::time::Duration;

use kubach_i18n::{Message, msg};

use crate::cluster::{ClusterError, Contexts};

/// How long a test may take, exec plugin included.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

/// Longest detail kept from an error (exec plugins can print a lot).
const DETAIL_LIMIT: usize = 600;

/// What kind of problem a connection test hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// 401: the token or certificate was not accepted.
    Unauthorized,
    /// 403: signed in, but not allowed to read the server version.
    Forbidden,
    /// The server's certificate is not signed by the given CA.
    UnknownCa,
    /// The server's certificate is for another host name.
    CertificateName,
    /// Another TLS problem (handshake, client certificate rejected, …).
    Tls,
    /// The host name doesn't resolve.
    Dns,
    /// Nothing listens at the address.
    Refused,
    /// No answer in time.
    Timeout,
    /// The exec plugin's command was not found.
    ExecNotFound,
    /// The exec plugin ran and failed.
    ExecFailed,
    /// The kubeconfig itself is wrong (bad PEM, unknown context, …).
    InvalidConfig,
    /// The server answered with another HTTP error.
    Server(u16),
    /// Connected, but the server ignored the credentials and took the
    /// request as `system:anonymous` (e.g. basic auth on Kubernetes 1.19+).
    Anonymous,
    Other,
}

/// A failed connection test.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionFailure {
    pub kind: FailureKind,
    /// The underlying error, shortened, for the curious.
    pub detail: String,
}

impl ConnectionFailure {
    /// Sorts `error` by walking its sources: API statuses, exec plugin and
    /// I/O errors by type, TLS and DNS problems by their messages (rustls
    /// and hyper don't expose them as types through kube).
    pub fn classify(error: &ClusterError) -> Self {
        Self::classify_with(error, false)
    }

    /// [`classify`](Self::classify), knowing whether the exec plugin is
    /// `kubectl oidc-login` (from the kubeconfig: the error no longer holds
    /// the plugin's arguments).
    fn classify_with(error: &ClusterError, oidc_login: bool) -> Self {
        let mut kind = None;
        let mut detail = None;
        let mut messages = Vec::new();
        let mut source: Option<&(dyn StdError + 'static)> = Some(error);
        while let Some(current) = source {
            messages.push(current.to_string());
            if kind.is_none() {
                if let Some(kube::Error::Api(status)) = current.downcast_ref::<kube::Error>() {
                    kind = Some(match status.code {
                        401 => FailureKind::Unauthorized,
                        403 => FailureKind::Forbidden,
                        code => FailureKind::Server(code),
                    });
                    detail = Some(status.message.clone());
                } else if let Some(auth) = current.downcast_ref::<kube::client::AuthError>() {
                    let (found, text) = auth_failure(auth, oidc_login);
                    kind = Some(found);
                    detail = text;
                } else if let Some(io) = current.downcast_ref::<io::Error>() {
                    kind = match io.kind() {
                        io::ErrorKind::ConnectionRefused => Some(FailureKind::Refused),
                        io::ErrorKind::TimedOut => Some(FailureKind::Timeout),
                        _ => None,
                    };
                }
            }
            source = current.source();
        }
        let text = messages.join(": ").to_lowercase();
        let kind = kind.or_else(|| by_message(&text)).unwrap_or(match error {
            ClusterError::Kubeconfig(_)
            | ClusterError::UnknownContext(_)
            | ClusterError::File { .. }
            | ClusterError::Client(_) => FailureKind::InvalidConfig,
            ClusterError::Unreachable(_) => FailureKind::Other,
        });
        let detail = detail.unwrap_or_else(|| {
            // The innermost message is usually the most specific one.
            messages.last().cloned().unwrap_or_default()
        });
        Self {
            kind,
            detail: shorten(&crate::redact::mask_secrets(detail.trim())),
        }
    }

    pub fn anonymous() -> Self {
        Self {
            kind: FailureKind::Anonymous,
            detail: format!("The server took the connection as {ANONYMOUS_USER}"),
        }
    }

    pub fn timeout() -> Self {
        Self {
            kind: FailureKind::Timeout,
            detail: format!("No answer in {} s", CHECK_TIMEOUT.as_secs()),
        }
    }

    /// One line saying what happened.
    pub fn title(&self) -> Message {
        match self.kind {
            FailureKind::Unauthorized => msg!("probe-unauthorized"),
            FailureKind::Forbidden => msg!("probe-forbidden"),
            FailureKind::UnknownCa => msg!("probe-unknown-ca"),
            FailureKind::CertificateName => msg!("probe-certificate-name"),
            FailureKind::Tls => msg!("probe-tls"),
            FailureKind::Dns => msg!("probe-dns"),
            FailureKind::Refused => msg!("probe-refused"),
            FailureKind::Timeout => msg!("probe-timeout"),
            FailureKind::ExecNotFound => msg!("probe-exec-not-found"),
            FailureKind::ExecFailed => msg!("probe-exec-failed"),
            FailureKind::InvalidConfig => msg!("probe-invalid-config"),
            FailureKind::Server(code) => msg!("probe-server", code = code),
            FailureKind::Anonymous => msg!("probe-anonymous"),
            FailureKind::Other => msg!("probe-other"),
        }
    }

    /// What to check next, when there is something useful to say.
    pub fn hint(&self) -> Option<Message> {
        Some(match self.kind {
            FailureKind::Unauthorized => msg!("probe-unauthorized-hint"),
            FailureKind::Forbidden => msg!("probe-forbidden-hint"),
            FailureKind::UnknownCa => msg!("probe-unknown-ca-hint"),
            FailureKind::CertificateName => msg!("probe-certificate-name-hint"),
            FailureKind::Tls => msg!("probe-tls-hint"),
            FailureKind::Dns => msg!("probe-dns-hint"),
            FailureKind::Refused => msg!("probe-refused-hint"),
            FailureKind::Timeout => msg!("probe-timeout-hint"),
            FailureKind::ExecNotFound => msg!("probe-exec-not-found-hint"),
            FailureKind::ExecFailed => msg!("probe-exec-failed-hint"),
            FailureKind::Anonymous => msg!("probe-anonymous-hint"),
            FailureKind::InvalidConfig | FailureKind::Server(_) | FailureKind::Other => {
                return None;
            }
        })
    }
}

impl std::fmt::Display for ConnectionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.title().english(), self.detail)
    }
}

/// Kind and detail of an auth error. The detail never holds the plugin's
/// arguments, environment or stdout ([`crate::redact::auth_error_text`]).
fn auth_failure(
    error: &kube::client::AuthError,
    oidc_login: bool,
) -> (FailureKind, Option<String>) {
    use kube::client::AuthError;
    match error {
        AuthError::AuthExecStart(io) if io.kind() == io::ErrorKind::NotFound => {
            (FailureKind::ExecNotFound, Some(io.to_string()))
        }
        // `cmd` is the whole command with its arguments and environment
        // (secrets included): only the stderr and exit code are shown.
        AuthError::AuthExecRun { cmd, status, out } => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stderr = stderr.trim();
            let path = crate::env::path();
            let oidc_login = oidc_login || cmd.contains("\"oidc-login\"");
            if oidc_plugin_missing(oidc_login, stderr, &path) {
                let detail = format!(
                    "kubectl has no oidc-login command: {} (kubelogin) is not in PATH",
                    crate::vault::manual::OIDC_PLUGIN_BINARY
                );
                return (FailureKind::ExecNotFound, Some(detail));
            }
            let detail = if stderr.is_empty() {
                match status.code() {
                    Some(code) => format!("Exited with code {code}"),
                    None => format!("Stopped: {status}"),
                }
            } else {
                crate::redact::mask_secrets(stderr)
            };
            (FailureKind::ExecFailed, Some(detail))
        }
        AuthError::AuthExecStart(_)
        | AuthError::AuthExec(_)
        | AuthError::AuthExecParse(_)
        | AuthError::ExecPluginFailed
        | AuthError::MissingCommand => (
            FailureKind::ExecFailed,
            Some(crate::redact::auth_error_text(error)),
        ),
        _ => (
            FailureKind::InvalidConfig,
            Some(crate::redact::auth_error_text(error)),
        ),
    }
}

/// Whether a failed exec plugin is `kubectl oidc-login` without kubelogin
/// installed. kubectl's complaint usually goes to the inherited stderr, not
/// to `stderr`, so for `oidc_login` plugins the binary is also looked up in
/// `path`.
fn oidc_plugin_missing(oidc_login: bool, stderr: &str, path: &std::ffi::OsStr) -> bool {
    use crate::vault::manual::OIDC_PLUGIN_BINARY;
    if stderr.contains("unknown command") && stderr.contains("oidc-login") {
        return true;
    }
    oidc_login && crate::which::which(OIDC_PLUGIN_BINARY, path).is_none()
}

fn by_message(text: &str) -> Option<FailureKind> {
    let has = |needles: &[&str]| needles.iter().any(|needle| text.contains(needle));
    if has(&["notvalidforname", "not valid for name", "hostname mismatch"]) {
        Some(FailureKind::CertificateName)
    } else if has(&[
        "unknownissuer",
        "unknown issuer",
        "unable to get local issuer",
        "self signed",
        "self-signed",
        "badsignature",
        // rustls with the OS verifier (macOS: "not standards compliant").
        "invalid peer certificate",
    ]) {
        Some(FailureKind::UnknownCa)
    } else if has(&[
        "dns error",
        "failed to lookup address",
        "nodename nor servname",
        "name or service not known",
        "no such host",
    ]) {
        Some(FailureKind::Dns)
    } else if has(&["connection refused"]) {
        Some(FailureKind::Refused)
    } else if has(&["timed out", "deadline has elapsed"]) {
        Some(FailureKind::Timeout)
    } else if has(&["certificate", "tls", "handshake", "ssl"]) {
        Some(FailureKind::Tls)
    } else {
        None
    }
}

fn shorten(text: &str) -> String {
    match text.char_indices().nth(DETAIL_LIMIT) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

impl Contexts {
    /// Connects to `context` and returns the server's version, or why that
    /// failed, within [`CHECK_TIMEOUT`]. Must run on the
    /// [`runtime`](crate::runtime).
    ///
    /// `/version` is open to anonymous requests, and API servers ignore
    /// credentials they don't support (basic auth since Kubernetes 1.19)
    /// instead of rejecting them, so the server is also asked who we are: a
    /// connection that turns out anonymous is reported as
    /// [`FailureKind::Anonymous`].
    pub async fn check(&self, context: &str) -> Result<String, ConnectionFailure> {
        let work = async {
            let connection = self.connect(context).await.map_err(|error| {
                let oidc_login = self.exec_args(context).iter().any(|a| a == "oidc-login");
                ConnectionFailure::classify_with(&error, oidc_login)
            })?;
            if user_name(&connection.client).await.as_deref() == Some(ANONYMOUS_USER) {
                return Err(ConnectionFailure::anonymous());
            }
            Ok(connection.server_version)
        };
        tokio::time::timeout(CHECK_TIMEOUT, work)
            .await
            .unwrap_or_else(|_| Err(ConnectionFailure::timeout()))
    }
}

/// User name of unauthenticated requests.
const ANONYMOUS_USER: &str = "system:anonymous";

/// Who the server takes the client for (SelfSubjectReview, Kubernetes
/// 1.28+); `None` when the server can't say.
async fn user_name(client: &kube::Client) -> Option<String> {
    use k8s_openapi::api::authentication::v1::SelfSubjectReview;
    let api: kube::Api<SelfSubjectReview> = kube::Api::all(client.clone());
    match api
        .create(
            &kube::api::PostParams::default(),
            &SelfSubjectReview::default(),
        )
        .await
    {
        Ok(review) => review.status?.user_info?.username,
        // Anonymous users may not create the review; the denial names them.
        Err(kube::Error::Api(status)) => anonymous_in_denial(&status.message),
        Err(_) => None,
    }
}

fn anonymous_in_denial(message: &str) -> Option<String> {
    message
        .contains(&format!("User \"{ANONYMOUS_USER}\""))
        .then(|| ANONYMOUS_USER.to_owned())
}

#[cfg(test)]
mod tests {
    use kube::core::Status;

    use super::*;

    #[test]
    fn failures_are_translated_and_english_for_logs() {
        use kubach_i18n::Locale;
        let failure = ConnectionFailure::timeout();
        assert_eq!(
            failure.to_string(),
            format!("The server didn't answer in time: {}", failure.detail)
        );
        assert_eq!(
            failure.title().translate(Locale::Ru),
            "Сервер не ответил вовремя"
        );
        assert!(
            failure
                .hint()
                .unwrap()
                .translate(Locale::Ru)
                .contains("VPN")
        );
    }

    fn unreachable(error: kube::Error) -> ClusterError {
        ClusterError::Unreachable(error)
    }

    fn service(error: impl StdError + Send + Sync + 'static) -> ClusterError {
        unreachable(kube::Error::Service(Box::new(error)))
    }

    fn api(code: u16, message: &str) -> ClusterError {
        unreachable(kube::Error::Api(
            Status::failure(message, "").with_code(code).boxed(),
        ))
    }

    #[test]
    fn api_statuses() {
        let failure = ConnectionFailure::classify(&api(401, "Unauthorized"));
        assert_eq!(failure.kind, FailureKind::Unauthorized);
        assert_eq!(failure.detail, "Unauthorized");
        assert!(failure.hint().is_some());
        assert_eq!(
            ConnectionFailure::classify(&api(403, "forbidden: User \"x\"")).kind,
            FailureKind::Forbidden
        );
        let other = ConnectionFailure::classify(&api(503, "unavailable"));
        assert_eq!(other.kind, FailureKind::Server(503));
        assert!(other.title().english().contains("503"));
    }

    #[test]
    fn io_errors_by_kind() {
        let refused = service(io::Error::from(io::ErrorKind::ConnectionRefused));
        assert_eq!(
            ConnectionFailure::classify(&refused).kind,
            FailureKind::Refused
        );
        let timeout = service(io::Error::from(io::ErrorKind::TimedOut));
        assert_eq!(
            ConnectionFailure::classify(&timeout).kind,
            FailureKind::Timeout
        );
    }

    #[test]
    fn tls_and_dns_by_message() {
        let cases = [
            (
                "invalid peer certificate: UnknownIssuer",
                FailureKind::UnknownCa,
            ),
            (
                "invalid peer certificate: Other(OtherError(\"not standards compliant\"))",
                FailureKind::UnknownCa,
            ),
            (
                "invalid peer certificate: NotValidForName",
                FailureKind::CertificateName,
            ),
            (
                "received fatal alert: HandshakeFailure (tls)",
                FailureKind::Tls,
            ),
            (
                "dns error: failed to lookup address information: nodename nor servname provided",
                FailureKind::Dns,
            ),
            (
                "tcp connect error: Connection refused",
                FailureKind::Refused,
            ),
            ("something else entirely", FailureKind::Other),
        ];
        for (message, kind) in cases {
            let failure = ConnectionFailure::classify(&service(io::Error::other(message)));
            assert_eq!(failure.kind, kind, "{message}");
            assert_eq!(failure.detail, message);
        }
    }

    #[test]
    fn exec_plugin_failures() {
        let missing = ClusterError::Client(kube::Error::Auth(
            kube::client::AuthError::AuthExecStart(io::Error::from(io::ErrorKind::NotFound)),
        ));
        let failure = ConnectionFailure::classify(&missing);
        assert_eq!(failure.kind, FailureKind::ExecNotFound);
        assert!(failure.hint().unwrap().english().contains("PATH"));
        let failed = ClusterError::Client(kube::Error::Auth(kube::client::AuthError::AuthExec(
            "Executing aws failed".into(),
        )));
        assert_eq!(
            ConnectionFailure::classify(&failed).kind,
            FailureKind::ExecFailed
        );
    }

    #[cfg(unix)]
    #[test]
    fn exec_failure_detail_hides_arguments_env_and_secrets() {
        use std::os::unix::process::ExitStatusExt as _;
        let status = std::process::ExitStatus::from_raw(3 << 8);
        let error =
            ClusterError::Unreachable(kube::Error::Auth(kube::client::AuthError::AuthExecRun {
                cmd: r#"AWS_SECRET_ACCESS_KEY="S3CR3T" "aws" "--oidc-client-secret=S3CR3T""#.into(),
                status,
                out: std::process::Output {
                    status,
                    stdout: b"S3CR3T".to_vec(),
                    stderr: b"denied: token=S3CR3T".to_vec(),
                },
            }));
        let failure = ConnectionFailure::classify(&error);
        assert_eq!(failure.kind, FailureKind::ExecFailed);
        assert_eq!(failure.detail, "denied: token=***");
        let failed = ClusterError::Client(kube::Error::Auth(kube::client::AuthError::AuthExec(
            "Failed to query \"{\\\"token\\\": \\\"S3CR3T\\\"}\" as a JsonPath".into(),
        )));
        let failure = ConnectionFailure::classify(&failed);
        assert!(!failure.detail.contains("S3CR3T"), "{}", failure.detail);
    }

    #[cfg(unix)]
    #[test]
    fn kubectl_without_the_oidc_plugin_is_a_missing_plugin() {
        use std::os::unix::process::ExitStatusExt as _;
        let run = |stderr: &str| {
            let status = std::process::ExitStatus::from_raw(1 << 8);
            ClusterError::Client(kube::Error::Auth(kube::client::AuthError::AuthExecRun {
                cmd: "kubectl".into(),
                status,
                out: std::process::Output {
                    status,
                    stdout: Vec::new(),
                    stderr: stderr.as_bytes().to_vec(),
                },
            }))
        };
        let missing = ConnectionFailure::classify(&run(
            "error: unknown command \"oidc-login\" for \"kubectl\"\n",
        ));
        assert_eq!(missing.kind, FailureKind::ExecNotFound);
        assert!(missing.hint().unwrap().english().contains("oidc-login"));
        assert!(missing.detail.contains("kubectl-oidc_login"));
        let failed = ConnectionFailure::classify(&run("ExpiredToken"));
        assert_eq!(failed.kind, FailureKind::ExecFailed);
        assert_eq!(failed.detail, "ExpiredToken");
        assert_eq!(
            ConnectionFailure::classify(&run("")).detail,
            "Exited with code 1"
        );
    }

    #[cfg(unix)]
    #[test]
    fn oidc_plugin_is_looked_up_when_kubectl_says_nothing() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().as_os_str();
        assert!(oidc_plugin_missing(true, "", path));
        assert!(!oidc_plugin_missing(false, "", path));
        let plugin = dir.path().join("kubectl-oidc_login");
        std::fs::write(&plugin, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&plugin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!oidc_plugin_missing(true, "", path));
    }

    #[test]
    fn anonymous_connection_explains_ignored_credentials() {
        let failure = ConnectionFailure::anonymous();
        assert_eq!(failure.kind, FailureKind::Anonymous);
        assert!(
            failure
                .title()
                .english()
                .contains("ignored the credentials")
        );
        assert!(failure.detail.contains("system:anonymous"));
        assert!(failure.hint().unwrap().english().contains("1.19"));
        assert_eq!(
            anonymous_in_denial(
                "selfsubjectreviews.authentication.k8s.io is forbidden: User \
                 \"system:anonymous\" cannot create resource \"selfsubjectreviews\""
            )
            .as_deref(),
            Some(ANONYMOUS_USER)
        );
        assert_eq!(
            anonymous_in_denial("forbidden: User \"alice\" cannot create resource"),
            None
        );
    }

    #[test]
    fn config_errors() {
        let unknown = ClusterError::UnknownContext("x".into());
        assert_eq!(
            ConnectionFailure::classify(&unknown).kind,
            FailureKind::InvalidConfig
        );
        assert_eq!(ConnectionFailure::timeout().kind, FailureKind::Timeout);
    }

    #[test]
    fn long_details_are_shortened() {
        let long = "x".repeat(DETAIL_LIMIT + 50);
        let failure = ConnectionFailure::classify(&service(io::Error::other(long)));
        assert_eq!(failure.detail.chars().count(), DETAIL_LIMIT + 1);
        assert!(failure.detail.ends_with('…'));
    }

    #[tokio::test]
    async fn refused_port_is_reported() {
        // A port that was just free: nothing listens there.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let yaml = format!(
            "apiVersion: v1\nkind: Config\nclusters:\n  - {{ name: c, cluster: {{ server: \"https://127.0.0.1:{port}\", insecure-skip-tls-verify: true }} }}\nusers:\n  - {{ name: u, user: {{ token: t }} }}\ncontexts:\n  - {{ name: x, context: {{ cluster: c, user: u }} }}\n"
        );
        let contexts = Contexts::from_yaml(&yaml).unwrap();
        let failure = contexts.check("x").await.unwrap_err();
        assert_eq!(failure.kind, FailureKind::Refused, "{failure}");
        let unknown = contexts.check("nope").await.unwrap_err();
        assert_eq!(unknown.kind, FailureKind::InvalidConfig);
    }
}
