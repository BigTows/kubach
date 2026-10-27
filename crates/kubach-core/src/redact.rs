//! Keeps credentials out of error texts.
//!
//! kube-rs reports a failed exec plugin as `AuthExecRun { cmd, out, .. }`
//! where `cmd` is the `Debug` form of the command: on Unix that is every
//! argument and every environment variable set for it
//! (`--oidc-client-secret=…`, `AWS_SECRET_ACCESS_KEY=…`), and `out` holds the
//! plugin's whole output. Its `Display` prints all of that, so an error shown
//! as is puts secrets on the screen, into logs and into MCP answers.
//!
//! [`client`] builds kube clients whose request errors are already cleaned
//! up ([`sanitize_kube_error`]), and [`auth_error_text`] words an auth error
//! with only the program name, the exit code and the plugin's stderr, cut and
//! masked with [`mask_secrets`].

use std::borrow::Cow;
use std::error::Error as StdError;
use std::path::Path;
use std::process::Output;
use std::sync::LazyLock;

use kube::client::AuthError;
use regex::Regex;

/// Longest plugin stderr kept in an error.
const STDERR_LIMIT: usize = 400;

const MASK: &str = "***";

static PEM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"-----BEGIN ([A-Z0-9 ]+)-----[\s\S]*?(?:-----END [A-Z0-9 ]+-----|\z)").unwrap()
});
static JWT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"eyJ[A-Za-z0-9_-]{4,}\.[A-Za-z0-9_-]{4,}\.[A-Za-z0-9_-]*").unwrap()
});
static BEARER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b((?:bearer|basic)\s+)[A-Za-z0-9._~+/=-]{4,}").unwrap());
/// `--client-secret value`: a secret-looking flag with its value after a space.
static FLAG_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(^|\s)(--?[A-Za-z0-9_-]*(?:password|passwd|secret|token|api[_-]?key|access[_-]?key|private[_-]?key|credentials?)[A-Za-z0-9_-]*[ \t]+)("?)([^\s"'-][^\s"']*)"#,
    )
    .unwrap()
});
/// `secret=…`, `--oidc-client-secret=…`, `token: …`, `"access_token":"…"`.
static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\b([A-Za-z0-9_.-]*(?:password|passwd|pwd|secret|token|api[_-]?key|access[_-]?key|private[_-]?key|credentials?)[A-Za-z0-9_.-]*)("?[ \t]*[=:][ \t]*)(\\?"?)([^\s"'\\,;&]+(?:\\[^\s"'\\,;&]+)*)"#,
    )
    .unwrap()
});
static URL_PASSWORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([a-zA-Z][a-zA-Z0-9+.-]*://[^\s:/@]+:)[^\s@/]+@").unwrap());

/// Best-effort masking of credentials in free text: PEM blocks, JWTs,
/// `Bearer …`, `--*secret*=…`, `token=…`, `password: …`, URL passwords.
///
/// The one text masker of Kubach: error texts here and everything the MCP
/// agent or its audit log sees (`kubach_mcp::masking`) go through it.
/// Masking is not a guarantee.
pub fn mask_secrets(text: &str) -> String {
    mask_text(text).into_owned()
}

/// [`mask_secrets`] that borrows `text` when there is nothing to mask.
pub fn mask_text(text: &str) -> Cow<'_, str> {
    let mut out = Cow::Borrowed(text);
    let mut apply = |re: &Regex, replacement: &str| {
        if re.is_match(&out) {
            out = Cow::Owned(re.replace_all(&out, replacement).into_owned());
        }
    };
    apply(&PEM, "-----BEGIN $1----- *** (masked)");
    apply(&JWT, MASK);
    apply(&BEARER, "${1}***");
    apply(&FLAG_VALUE, "${1}${2}${3}***");
    apply(&ASSIGNMENT, "${1}${2}${3}***");
    apply(&URL_PASSWORD, "${1}***@");
    out
}

/// Cuts `text` to `limit` characters, noting the cut.
fn shorten(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

/// Plugin stderr fit for an error: trimmed, cut and masked.
fn stderr_text(stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    shorten(&mask_secrets(stderr.trim()), STDERR_LIMIT)
}

/// File name of the program in a failed command from kube-rs: the `Debug`
/// form of an exec plugin (`cd "…" && env -u X KEY="…" "prog" "arg"`, or
/// `["prog"] "arg0" …`) or `"{cmd} {args}"` of the `gcp` auth provider.
/// Arguments and environment are never returned.
pub fn program_name(cmd: &str) -> Option<String> {
    let tokens = tokenize(cmd);
    let mut quoted = false;
    let mut program = None;
    let mut iter = tokens.iter().peekable();
    while let Some(token) = iter.next() {
        match token.as_str() {
            "cd" | "-u" => {
                iter.next();
            }
            "&&" | "env" | "-i" => {}
            // `KEY="value"`: an environment variable.
            t if !t.starts_with(['"', '[']) && t.contains('=') => {}
            t => {
                let t = t.trim_start_matches('[').trim_end_matches(']');
                quoted = t.starts_with('"');
                program = Some(unquote(t));
                break;
            }
        }
    }
    // Not a `Debug` form: the first word is the program.
    let program = match program {
        Some(program) if quoted => program,
        _ => cmd.split_whitespace().next()?.to_owned(),
    };
    let name = Path::new(&program)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or(program);
    (!name.is_empty()).then_some(name)
}

/// Splits on spaces outside `"…"` (with `\"` escapes).
fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars();
    let mut in_quotes = false;
    while let Some(c) = chars.next() {
        match c {
            '\\' if in_quotes => {
                current.push(c);
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            '"' => {
                in_quotes = !in_quotes;
                current.push(c);
            }
            c if c.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn unquote(token: &str) -> String {
    let inner = token
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(token);
    inner.replace("\\\"", "\"").replace("\\\\", "\\")
}

/// The program in `cmd`, for messages.
fn program_label(cmd: &str) -> String {
    program_name(cmd).map_or_else(|| "exec plugin".to_owned(), |name| format!("`{name}`"))
}

/// Exit code (or signal) of a failed plugin.
fn exit_text(status: &std::process::ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit code {code}"),
        None => status.to_string(),
    }
}

/// An auth error in words, without the plugin's arguments, environment or
/// stdout: only the program name, its exit code and its stderr (cut and
/// masked).
pub fn auth_error_text(error: &AuthError) -> String {
    match error {
        AuthError::AuthExecRun { cmd, status, out } => {
            let stderr = stderr_text(&out.stderr);
            let head = format!(
                "auth plugin {} failed with {}",
                program_label(cmd),
                exit_text(status)
            );
            if stderr.is_empty() {
                head
            } else {
                format!("{head}: {stderr}")
            }
        }
        AuthError::AuthExec(message) => auth_exec_text(message),
        other => mask_secrets(&other.to_string()),
    }
}

/// `AuthError::AuthExec` messages of the `gcp` provider quote the token
/// command's whole output; those are replaced.
fn auth_exec_text(message: &str) -> String {
    if message.starts_with("Failed to query")
        || message.starts_with("Target ")
        || message.starts_with("Result is not a string")
    {
        "failed exec auth: the auth provider's output has no token at the configured path".into()
    } else {
        mask_secrets(&format!("failed exec auth: {message}"))
    }
}

/// The same auth error, with only what [`auth_error_text`] shows: so its
/// `Display` and `Debug` are safe wherever it ends up.
pub fn sanitize_auth_error(error: AuthError) -> AuthError {
    match error {
        AuthError::AuthExecRun { cmd, status, out } => AuthError::AuthExecRun {
            cmd: program_name(&cmd).unwrap_or_else(|| "exec plugin".into()),
            status,
            out: Output {
                status: out.status,
                stdout: Vec::new(),
                stderr: stderr_text(&out.stderr).into_bytes(),
            },
        },
        AuthError::AuthExec(message) => {
            let text = auth_exec_text(&message);
            let text = text.strip_prefix("failed exec auth: ").unwrap_or(&text);
            AuthError::AuthExec(text.to_owned())
        }
        other => other,
    }
}

/// `error` with any auth error in it sanitized ([`sanitize_auth_error`]).
pub fn sanitize_kube_error(error: kube::Error) -> kube::Error {
    match error {
        kube::Error::Auth(auth) => kube::Error::Auth(sanitize_auth_error(auth)),
        kube::Error::Service(boxed) => kube::Error::Service(sanitize_boxed(boxed)),
        other => other,
    }
}

/// A boxed error from the client's service stack, sanitized when it is an
/// auth error.
pub fn sanitize_boxed(error: tower::BoxError) -> tower::BoxError {
    match error.downcast::<AuthError>() {
        Ok(auth) => Box::new(sanitize_auth_error(*auth)),
        Err(error) => match error.downcast::<kube::Error>() {
            Ok(kube) => Box::new(sanitize_kube_error(*kube)),
            Err(error) => error,
        },
    }
}

/// A kube client for `config` whose request errors never carry exec plugin
/// arguments, environment or output (the plugin reruns whenever its token
/// expires, long after connecting), on the connection stack of
/// [`crate::transport`]. Use instead of `Client::try_from`.
pub fn client(config: kube::Config) -> Result<kube::Client, kube::Error> {
    let builder = crate::transport::builder(config).map_err(sanitize_kube_error)?;
    Ok(builder
        .with_layer(&tower::util::MapErrLayer::new(sanitize_boxed))
        .build())
}

/// `error` in words for the user: an auth error anywhere in its sources is
/// replaced with [`auth_error_text`] (the outer messages embed its unsafe
/// `Display`), everything else is masked with [`mask_secrets`].
///
/// `prefix` is what the caller's own error says before its source, e.g.
/// `"cluster is unreachable"`.
pub fn error_text(prefix: Option<&str>, error: &(dyn StdError + 'static)) -> String {
    let mut source: Option<&(dyn StdError + 'static)> = Some(error);
    while let Some(current) = source {
        if let Some(auth) = current.downcast_ref::<AuthError>() {
            let text = format!("auth error: {}", auth_error_text(auth));
            return match prefix {
                Some(prefix) => format!("{prefix}: {text}"),
                None => text,
            };
        }
        source = current.source();
    }
    let text = mask_secrets(&error.to_string());
    match prefix {
        Some(prefix) => format!("{prefix}: {text}"),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Command, ExitStatus};

    use super::*;

    const SECRET: &str = "SUPERSECRET";

    fn failed_status() -> ExitStatus {
        Command::new(if cfg!(windows) { "cmd" } else { "sh" })
            .args(if cfg!(windows) {
                ["/C", "exit 3"]
            } else {
                ["-c", "exit 3"]
            })
            .status()
            .unwrap()
    }

    /// The error kube-rs builds for a failed exec plugin with secrets in its
    /// arguments and environment.
    fn exec_run(stderr: &str) -> AuthError {
        let mut cmd = Command::new("/usr/local/bin/kubectl");
        cmd.args([
            "oidc-login",
            "get-token",
            &format!("--oidc-client-secret={SECRET}"),
        ]);
        cmd.env("AWS_SECRET_ACCESS_KEY", SECRET);
        cmd.env("KUBERNETES_EXEC_INFO", r#"{"kind":"ExecCredential"}"#);
        cmd.env_remove("DROPPED");
        let status = failed_status();
        AuthError::AuthExecRun {
            cmd: format!("{cmd:?}"),
            status,
            out: Output {
                status,
                stdout: format!("{{\"token\":\"{SECRET}\"}}").into_bytes(),
                stderr: stderr.as_bytes().to_vec(),
            },
        }
    }

    #[test]
    fn exec_error_shows_program_code_and_stderr_only() {
        let error = exec_run("login failed\n");
        // The premise: kube-rs' own text leaks the secret.
        if cfg!(unix) {
            assert!(error.to_string().contains(SECRET));
        }
        let text = auth_error_text(&error);
        assert!(!text.contains(SECRET), "{text}");
        assert!(!text.contains("oidc-login"), "{text}");
        assert_eq!(
            text,
            "auth plugin `kubectl` failed with exit code 3: login failed"
        );
    }

    #[test]
    fn stderr_is_masked_and_cut() {
        let stderr = format!(
            "error: --oidc-client-secret={SECRET} rejected; token={SECRET}; \
             Authorization: Bearer {SECRET}; --client-secret {SECRET} {}",
            "x".repeat(2000)
        );
        let text = auth_error_text(&exec_run(&stderr));
        assert!(!text.contains(SECRET), "{text}");
        assert!(text.contains("--oidc-client-secret=***"), "{text}");
        assert!(text.len() < 600, "{}", text.len());
        assert!(text.ends_with('…'));
    }

    #[test]
    fn sanitized_error_is_safe_in_display_and_debug() {
        let error = sanitize_auth_error(exec_run(&format!("token: {SECRET}")));
        for text in [error.to_string(), format!("{error:?}")] {
            assert!(!text.contains(SECRET), "{text}");
            assert!(!text.contains("AWS_SECRET_ACCESS_KEY"), "{text}");
        }
        let AuthError::AuthExecRun { cmd, out, .. } = &error else {
            panic!("{error:?}");
        };
        assert_eq!(cmd, "kubectl");
        assert!(out.stdout.is_empty());
    }

    #[test]
    fn error_text_walks_sources() {
        let kube = kube::Error::Auth(exec_run(""));
        let cluster = crate::ClusterError::Unreachable(kube);
        let text = error_text(Some("cluster is unreachable"), &cluster);
        assert!(!text.contains(SECRET), "{text}");
        assert_eq!(
            text,
            "cluster is unreachable: auth error: auth plugin `kubectl` failed with exit code 3"
        );
        // Wrapped as a service error, as `Client::send` does for errors
        // raised in the auth layer.
        let service = sanitize_kube_error(kube::Error::Service(Box::new(exec_run(""))));
        assert!(!service.to_string().contains(SECRET), "{service}");
        assert!(!format!("{service:?}").contains(SECRET));
    }

    #[test]
    fn gcp_provider_output_is_not_quoted() {
        let error = AuthError::AuthExec(format!(
            "Failed to query \"{{\\\"access_token\\\": \\\"{SECRET}\\\"}}\" as a JsonPath: x"
        ));
        assert!(!auth_error_text(&error).contains(SECRET));
        assert!(!sanitize_auth_error(error).to_string().contains(SECRET));
        let run = AuthError::AuthExecRun {
            cmd: format!("/opt/gcloud config-helper --secret={SECRET}"),
            status: failed_status(),
            out: Output {
                status: failed_status(),
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        };
        assert_eq!(
            auth_error_text(&run),
            "auth plugin `gcloud` failed with exit code 3"
        );
    }

    #[test]
    fn program_names() {
        let cases = [
            (r#""aws" "eks" "get-token""#, Some("aws")),
            (
                r#"KUBERNETES_EXEC_INFO="{\"a\": \"b c\"}" TOKEN="x y" "/usr/bin/kubelogin" "get-token""#,
                Some("kubelogin"),
            ),
            (r#"cd "/tmp dir" && env -u A -u B "gke" "x""#, Some("gke")),
            (r#"env -i K="v" ["/bin/prog"] "argv0" "a""#, Some("prog")),
            ("gcloud config-helper --format=json", Some("gcloud")),
            ("", None),
        ];
        for (cmd, expected) in cases {
            assert_eq!(program_name(cmd).as_deref(), expected, "{cmd}");
        }
    }

    #[test]
    fn masks_common_secrets() {
        let text = mask_secrets(&format!(
            "--oidc-client-secret={SECRET} access_token=\"{SECRET}\" \
             password: {SECRET} https://user:{SECRET}@host Bearer {SECRET} \
             --token {SECRET} {{\"token\":\"{SECRET}\"}}"
        ));
        assert!(!text.contains(SECRET), "{text}");
        // Plain words stay.
        // A backslash inside a bare value does not end it.
        let text = mask_secrets(&format!("password=ab\\{SECRET} token=x}}{SECRET}"));
        assert!(!text.contains(SECRET), "{text}");
        assert!(matches!(mask_text("all good here"), Cow::Borrowed(_)));
        assert_eq!(mask_secrets("No id-token for oidc"), "No id-token for oidc");
        assert_eq!(
            mask_secrets("connection refused (os error 61)"),
            "connection refused (os error 61)"
        );
    }
}
