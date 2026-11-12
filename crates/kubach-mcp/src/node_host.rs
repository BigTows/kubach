//! `node_host_read` (#106): read-only diagnostics of a node's host for the
//! agent — the kernel log, the journal, files under `/proc` and the like.
//!
//! None of that is in the Kubernetes API. It is read the way the app's
//! "Shell on node" works (ADR 0004, #318): a temporary privileged pod on
//! the node, and an exec that enters the host's namespaces. The agent gets
//! no shell there:
//!
//! - one program of [`PROGRAMS`] per call, with arguments checked against
//!   that program's list ([`host_argv`]): nothing that writes, follows or
//!   pages, files by absolute path only. The argv reaches the program as
//!   it is, there is no shell in between to interpret it;
//! - the user's credentials for this one call (a session identity never
//!   creates privileged pods), never in a session pinned to namespaces;
//! - a window for every call with the exact command. "Don't ask again"
//!   there is for that exact command on that node only
//!   ([`rememberable`](crate::remember::rememberable)); a file that may
//!   hold credentials ([`sensitive`]) needs its own tick every time;
//! - bounded, masked output, and the pod is deleted when the call ends.

use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::core::v1::Node;
use kubach_core::rbac::ResourceAccess;
use kubach_core::streams::StreamError;
use kubach_core::streams::node_shell::{
    self, CreatedPod, DEFAULT_IMAGE, DEFAULT_NAMESPACE, create_shell_pod, delete_shell_pod,
    wait_running,
};
use kubach_core::sync;
use kube::Client;
use kube::api::Api;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::confirm::RequestKind;
use crate::node_maintenance::NODE_USER_IDENTITY;
use crate::policy::{Denial, DenialCode, Finding, Request};
use crate::tools::{
    Ctx, DEFAULT_EXEC_TIMEOUT, MAX_EXEC_TIMEOUT, ToolError, ToolOutput, authorize, confirm_as,
    ensure_rbac, parse, run_command_as, shell_words,
};

/// The programs the agent may run on a host.
pub const PROGRAMS: [&str; 7] = ["dmesg", "journalctl", "cat", "head", "tail", "ls", "ps"];

/// Where node shell pods go and what they run: the user's choice in
/// Settings → General → Terminal, read on each call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeShellPod {
    pub namespace: String,
    pub image: String,
}

impl Default for NodeShellPod {
    fn default() -> Self {
        Self {
            namespace: DEFAULT_NAMESPACE.to_owned(),
            image: DEFAULT_IMAGE.to_owned(),
        }
    }
}

/// Reads [`NodeShellPod`] from the user's settings; set by the app.
pub type NodeShellPrefs = Arc<dyn Fn() -> NodeShellPod + Send + Sync>;

/// The pod has this long to start (image pull included): less than the
/// tab's two minutes, the agent's call is waiting.
const START_TIMEOUT: Duration = Duration::from_secs(60);

/// Journal lines shown when the agent names no count.
const DEFAULT_JOURNAL_LINES: &str = "500";

const MAX_ARGS: usize = 32;
const MAX_ARG_LEN: usize = 256;
const MAX_PATHS: usize = 8;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostArgs {
    node: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    timeout_seconds: Option<u64>,
}

/// An option that takes a value, and what its value may be.
type Valued = (&'static str, fn(&str) -> bool);

/// What a program accepts.
struct Syntax {
    /// Long options without a value.
    plain: &'static [&'static str],
    /// Options with a value, as the next argument or after `=`.
    valued: &'static [Valued],
    /// Long options whose value is optional and only comes after `=`.
    optional: &'static [&'static str],
    /// Letters of short options without a value; they may be combined.
    short: &'static str,
    /// What is not an option.
    positional: Positional,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Positional {
    None,
    /// Absolute paths, at least one.
    Paths,
    /// `FIELD=value` matches of the journal.
    JournalMatches,
    /// BSD-style `aux`.
    PsLetters,
}

fn any(_: &str) -> bool {
    true
}

fn count(value: &str) -> bool {
    let digits = value.trim_start_matches(['+', '-']);
    !digits.is_empty() && digits.len() <= 9 && digits.bytes().all(|b| b.is_ascii_digit())
}

fn pids(value: &str) -> bool {
    value.bytes().all(|b| b.is_ascii_digit() || b == b',')
}

fn word(value: &str) -> bool {
    value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"_,=:%.-".contains(&b))
}

fn syntax(program: &str) -> Option<Syntax> {
    Some(match program {
        "dmesg" => Syntax {
            plain: &[
                "--ctime",
                "--kernel",
                "--userspace",
                "--decode",
                "--notime",
                "--reltime",
                "--json",
                "--nopager",
            ],
            valued: &[
                ("-l", word),
                ("--level", word),
                ("-f", word),
                ("--facility", word),
                ("--since", any),
                ("--until", any),
                ("--time-format", word),
            ],
            optional: &[],
            short: "TkuxteJP",
            positional: Positional::None,
        },
        "journalctl" => Syntax {
            plain: &[
                "--dmesg",
                "--reverse",
                "--catalog",
                "--quiet",
                "--utc",
                "--all",
                "--full",
                "--no-full",
                "--no-hostname",
                "--no-pager",
                "--system",
                "--list-boots",
                "--disk-usage",
            ],
            valued: &[
                ("-u", any),
                ("--unit", any),
                ("-t", any),
                ("--identifier", any),
                ("-p", word),
                ("--priority", word),
                ("-S", any),
                ("--since", any),
                ("-U", any),
                ("--until", any),
                ("-n", count),
                ("--lines", count),
                ("-g", any),
                ("--grep", any),
                ("-o", word),
                ("--output", word),
                ("--case-sensitive", word),
            ],
            optional: &["--boot"],
            short: "krxqabl",
            positional: Positional::JournalMatches,
        },
        "cat" => Syntax {
            plain: &[],
            valued: &[],
            optional: &[],
            short: "",
            positional: Positional::Paths,
        },
        "head" | "tail" => Syntax {
            plain: &[],
            valued: &[
                ("-n", count),
                ("--lines", count),
                ("-c", count),
                ("--bytes", count),
            ],
            optional: &[],
            short: "",
            positional: Positional::Paths,
        },
        "ls" => Syntax {
            plain: &[],
            valued: &[],
            optional: &[],
            short: "ladnh1tZ",
            positional: Positional::Paths,
        },
        "ps" => Syntax {
            plain: &[],
            valued: &[("-o", word), ("-p", pids), ("-C", word)],
            optional: &[],
            short: "eAfFlLwZT",
            positional: Positional::PsLetters,
        },
        _ => return None,
    })
}

/// The argv run on the host for `program` with the agent's `args`, or why
/// not. Only what the program's list names passes; the journal never pages
/// and is cut to its last lines unless the agent says how many.
pub fn host_argv(program: &str, args: &[String]) -> Result<Vec<String>, String> {
    let Some(syntax) = syntax(program) else {
        return Err(format!(
            "command must be one of {}; `{program}` is not",
            PROGRAMS.join(", ")
        ));
    };
    if args.len() > MAX_ARGS {
        return Err(format!("at most {MAX_ARGS} arguments"));
    }
    let refused = |arg: &str| {
        format!(
            "`{arg}` is not among the arguments of {program} this tool passes: only reading, \
             nothing that follows, pages or writes"
        )
    };
    let mut positional = 0;
    let mut lines = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg.is_empty() || arg.len() > MAX_ARG_LEN || arg.chars().any(char::is_control) {
            return Err(format!(
                "an argument is empty, longer than {MAX_ARG_LEN} bytes or has control characters"
            ));
        }
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name, Some(value)),
            _ => (arg.as_str(), None),
        };
        if let Some((_, accepts)) = syntax.valued.iter().find(|(flag, _)| *flag == name) {
            let value = match inline {
                Some(value) => value,
                None => iter
                    .next()
                    .map(String::as_str)
                    .ok_or_else(|| format!("{name} of {program} needs a value"))?,
            };
            if value.is_empty()
                || value.len() > MAX_ARG_LEN
                || value.chars().any(char::is_control)
                || !accepts(value)
            {
                return Err(format!(
                    "`{value}` is not a value {name} of {program} takes"
                ));
            }
            lines |= matches!(name, "-n" | "--lines");
        } else if syntax.optional.contains(&name) {
            if inline.is_some_and(|value| !count(value)) {
                return Err(refused(arg));
            }
        } else if inline.is_none() && syntax.plain.contains(&name) {
            // Passed as it is.
        } else if let Some(letters) = arg.strip_prefix('-') {
            if letters.is_empty() || !letters.chars().all(|c| syntax.short.contains(c)) {
                return Err(refused(arg));
            }
        } else {
            positional += 1;
            let ok = match syntax.positional {
                Positional::None => false,
                Positional::Paths => {
                    host_path(arg)?;
                    true
                }
                Positional::JournalMatches => arg.split_once('=').is_some_and(|(field, value)| {
                    !value.is_empty()
                        && !field.is_empty()
                        && field
                            .bytes()
                            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                }),
                // No `e`: it prints every process's environment.
                Positional::PsLetters => arg.chars().all(|c| "auxwf".contains(c)),
            };
            if !ok {
                return Err(refused(arg));
            }
        }
    }
    if syntax.positional == Positional::Paths && !(1..=MAX_PATHS).contains(&positional) {
        return Err(format!(
            "{program} needs 1 to {MAX_PATHS} absolute paths on the host"
        ));
    }
    let mut argv = vec![program.to_owned()];
    argv.extend(args.iter().cloned());
    if program == "journalctl" {
        if !args.iter().any(|arg| arg == "--no-pager") {
            argv.push("--no-pager".into());
        }
        if !lines {
            argv.extend(["-n".into(), DEFAULT_JOURNAL_LINES.into()]);
        }
    }
    Ok(argv)
}

/// An absolute path without `..`, and not a device.
fn host_path(path: &str) -> Result<(), String> {
    if !path.starts_with('/') {
        return Err(format!("`{path}` is not an absolute path on the host"));
    }
    let parts = components(path);
    if parts.contains(&"..") {
        return Err(format!("`{path}` has `..` in it: name the file itself"));
    }
    if parts.first() == Some(&"dev") {
        return Err(format!("`{path}` is a device, not a file to read"));
    }
    Ok(())
}

fn components(path: &str) -> Vec<&str> {
    path.split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect()
}

/// Folders whose files hold credentials or the data of every pod on the
/// node.
const SENSITIVE_ROOTS: [&[&str]; 17] = [
    &["etc", "kubernetes"],
    &["etc", "rancher"],
    &["etc", "ssh"],
    &["etc", "ssl", "private"],
    &["etc", "shadow"],
    &["etc", "gshadow"],
    &["etc", "sudoers"],
    &["root"],
    &["home"],
    &["run", "secrets"],
    &["var", "run", "secrets"],
    &["var", "lib", "kubelet"],
    &["var", "lib", "etcd"],
    &["var", "lib", "rancher"],
    &["var", "lib", "containerd"],
    &["var", "lib", "docker"],
    &["var", "lib", "containers"],
];

/// Under `/proc`: a process's environment and memory, and the doors into
/// its container's files.
const SENSITIVE_PROC: [&str; 6] = ["environ", "mem", "root", "cwd", "kcore", "pagemap"];

/// Why reading `path` needs the user's own tick every time, if it does.
fn sensitive_path(path: &str) -> Option<&'static str> {
    let parts = components(path);
    if parts.first() == Some(&"proc") {
        return parts
            .iter()
            .any(|part| SENSITIVE_PROC.contains(part))
            .then_some(
                "a process's environment, memory or files: they belong to pods of any \
                 namespace and may hold credentials",
            );
    }
    SENSITIVE_ROOTS
        .iter()
        .any(|root| parts.starts_with(root))
        .then_some("may hold credentials or the data of pods of any namespace")
}

/// The files of `argv` whose content may be a secret: each needs its own
/// tick in the window, and such a command is never remembered. Masking the
/// output is a best effort, not a guarantee.
pub fn sensitive(argv: &[String]) -> Vec<Finding> {
    let reads_files = matches!(
        argv.first().map(String::as_str),
        Some("cat" | "head" | "tail")
    );
    if !reads_files {
        return Vec::new();
    }
    argv.iter()
        .skip(1)
        .filter(|arg| arg.starts_with('/'))
        .filter_map(|path| {
            sensitive_path(path).map(|why| Finding {
                document: None,
                path: path.clone(),
                message: why.to_owned(),
            })
        })
        .collect()
}

/// Deletes the pod when the call ends, however it ends: awaited on the
/// normal way out, in the background when the call is dropped.
struct PodGuard {
    client: Client,
    namespace: String,
    pod: Option<CreatedPod>,
}

impl PodGuard {
    async fn delete(mut self) -> Result<(), String> {
        let Some(pod) = self.pod.take() else {
            return Ok(());
        };
        delete_shell_pod(&self.client, &self.namespace, &pod.name, pod.uid.as_deref())
            .await
            .map_err(|error| error.to_string())
    }
}

impl Drop for PodGuard {
    fn drop(&mut self) {
        let Some(pod) = self.pod.take() else {
            return;
        };
        let (client, namespace) = (self.client.clone(), self.namespace.clone());
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = delete_shell_pod(&client, &namespace, &pod.name, pod.uid.as_deref()).await;
            });
        }
    }
}

fn stream_error(error: StreamError) -> ToolError {
    match error {
        StreamError::Api(error) => ToolError::from(*error),
        other => ToolError::Failed(other.to_string()),
    }
}

/// `node_host_read`.
pub(crate) async fn node_host_read(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    let a: HostArgs = parse(args)?;
    let argv =
        host_argv(&a.command, &a.args).map_err(|why| Denial::new(DenialCode::InvalidInput, why))?;
    let allowed = authorize(
        ctx,
        &Request::NodeHost {
            name: a.node.clone(),
            command: argv.clone(),
        },
    )
    .await?;
    let prefs = ctx
        .node_shell
        .as_ref()
        .map(|prefs| prefs())
        .unwrap_or_default();
    // What the user's RBAC does not allow is refused before anyone is asked.
    if !ctx
        .session
        .user_can(&ResourceAccess::new("get", "", "nodes"))
        .await
    {
        return Err(Denial::new(
            DenialCode::Rbac,
            "your Kubernetes account has no right to get nodes in the cluster; Kubach cannot \
             grant RBAC rights. Ask the cluster administrator",
        )
        .into());
    }
    ensure_rbac(
        ctx,
        &prefs.namespace,
        &[
            ("create", "", "pods"),
            ("get", "", "pods"),
            ("delete", "", "pods"),
            ("create", "", "pods/exec"),
        ],
    )
    .await?;
    let client = ctx.session.user_client();
    Api::<Node>::all(client.clone()).get(&a.node).await?;

    let rendered = shell_words(&argv);
    let timeout = a
        .timeout_seconds
        .unwrap_or(DEFAULT_EXEC_TIMEOUT)
        .clamp(1, MAX_EXEC_TIMEOUT);
    confirm_as(
        ctx,
        RequestKind::NodeCommand {
            node: a.node.clone(),
            command: argv.clone(),
        },
        &allowed,
        ctx.tool,
        format!(
            "Run a read-only command on the host of node {} as your kubeconfig user",
            a.node
        ),
        format!(
            "{rendered}\n\nRuns through a temporary privileged pod in namespace {} (image {}), \
             deleted when the command ends. The host holds the data of every namespace on the \
             node; the output is cut and masked before the agent sees it.",
            prefs.namespace, prefs.image
        ),
        sensitive(&argv),
    )
    .await?;
    *sync::lock(&ctx.identity) = Some(NODE_USER_IDENTITY.to_owned());

    let created = create_shell_pod(&client, &a.node, &prefs.namespace, &prefs.image)
        .await
        .map_err(stream_error)?;
    let pod = created.name.clone();
    let guard = PodGuard {
        client: client.clone(),
        namespace: prefs.namespace.clone(),
        pod: Some(created),
    };
    let ran = async {
        wait_running(&client, &prefs.namespace, &pod, START_TIMEOUT)
            .await
            .map_err(stream_error)?;
        run_command_as(
            &client,
            &prefs.namespace,
            &pod,
            Some("shell"),
            &node_shell::host_command(&argv),
            timeout,
        )
        .await
    }
    .await;
    let deleted = guard.delete().await;
    let mut out = format!("node {}: {rendered}\n{}", a.node, ran?);
    if let Err(error) = deleted {
        out.push_str(&format!(
            "--- the temporary pod {}/{pod} could not be deleted ({error}); Kubach removes it \
             the next time the user connects to this cluster\n",
            prefs.namespace
        ));
    }
    Ok(ToolOutput {
        audit: Some(json!({"node": a.node, "command": argv})),
        ..ToolOutput::confirmed(out)
    })
}

#[cfg(test)]
mod tests {
    use super::{host_argv, sensitive};

    fn argv(program: &str, args: &[&str]) -> Result<Vec<String>, String> {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        host_argv(program, &args)
    }

    fn ok(program: &str, args: &[&str]) -> String {
        argv(program, args)
            .unwrap_or_else(|why| panic!("{program} {args:?}: {why}"))
            .join(" ")
    }

    #[test]
    fn only_listed_programs_run() {
        for program in [
            "sh", "bash", "rm", "kubectl", "nsenter", "/bin/cat", "cat;id", "",
        ] {
            assert!(argv(program, &[]).is_err(), "{program}");
        }
    }

    #[test]
    fn reading_arguments_pass_as_they_are() {
        assert_eq!(
            ok("dmesg", &["-T", "--level", "err,warn"]),
            "dmesg -T --level err,warn"
        );
        assert_eq!(
            ok("dmesg", &["-Tx", "--since=-1h"]),
            "dmesg -Tx --since=-1h"
        );
        assert_eq!(
            ok("cat", &["/proc/1234/attr/current"]),
            "cat /proc/1234/attr/current"
        );
        assert_eq!(
            ok("cat", &["/etc/nginx/nginx.conf"]),
            "cat /etc/nginx/nginx.conf"
        );
        assert_eq!(
            ok("tail", &["-n", "100", "/var/log/syslog"]),
            "tail -n 100 /var/log/syslog"
        );
        assert_eq!(ok("ls", &["-la", "/proc/1234/fd"]), "ls -la /proc/1234/fd");
        assert_eq!(
            ok("ps", &["-e", "-o", "pid,ppid,comm"]),
            "ps -e -o pid,ppid,comm"
        );
        assert_eq!(ok("ps", &["aux"]), "ps aux");
    }

    #[test]
    fn the_journal_never_pages_and_is_cut_to_its_last_lines() {
        assert_eq!(
            ok(
                "journalctl",
                &["-k", "-u", "kubelet.service", "--since", "10 min ago"]
            ),
            "journalctl -k -u kubelet.service --since 10 min ago --no-pager -n 500"
        );
        assert_eq!(
            ok("journalctl", &["--no-pager", "-n", "50", "_PID=1"]),
            "journalctl --no-pager -n 50 _PID=1"
        );
        assert_eq!(
            ok("journalctl", &["--boot=-1", "--lines=20", "-p", "err"]),
            "journalctl --boot=-1 --lines=20 -p err --no-pager"
        );
    }

    #[test]
    fn nothing_that_writes_follows_or_leaves_the_list() {
        for (program, args) in [
            // Clearing and console levels of the kernel log.
            ("dmesg", &["-C"][..]),
            ("dmesg", &["--clear"]),
            ("dmesg", &["-c"]),
            ("dmesg", &["-n", "1"]),
            ("dmesg", &["--console-off"]),
            ("dmesg", &["-w"]),
            ("dmesg", &["/etc/shadow"]),
            // Journal maintenance and following.
            ("journalctl", &["--vacuum-time=1s"]),
            ("journalctl", &["--rotate"]),
            ("journalctl", &["--flush"]),
            ("journalctl", &["-f"]),
            ("journalctl", &["--follow"]),
            ("journalctl", &["-kf"]),
            ("journalctl", &["-D", "/tmp"]),
            ("journalctl", &["--file=/etc/shadow"]),
            ("journalctl", &["-n", "all"]),
            ("journalctl", &["/usr/bin/dockerd"]),
            ("journalctl", &["-u"]),
            // Files: absolute paths only, no options, no devices.
            ("cat", &[]),
            ("cat", &["etc/passwd"]),
            ("cat", &["/var/log/../../etc/shadow"]),
            ("cat", &["-v", "/etc/hosts"]),
            ("cat", &["/dev/sda"]),
            ("cat", &["/etc/hosts\n/etc/shadow"]),
            ("tail", &["-f", "/var/log/syslog"]),
            ("tail", &["--follow=name", "/var/log/syslog"]),
            ("head", &["-n", "10; id", "/etc/hosts"]),
            ("ls", &["-R", "/"]),
            ("ls", &[]),
            // The environment of every process.
            ("ps", &["auxe"]),
            ("ps", &["e"]),
            ("ps", &["-o", "pid;id"]),
        ] {
            assert!(argv(program, args).is_err(), "{program} {args:?}");
        }
        let many: Vec<&str> = std::iter::repeat_n("-T", 33).collect();
        assert!(argv("dmesg", &many).is_err());
    }

    #[test]
    fn files_that_may_hold_credentials_need_their_own_tick() {
        let found = |program: &str, args: &[&str]| -> Vec<String> {
            sensitive(&argv(program, args).unwrap())
                .into_iter()
                .map(|finding| finding.path)
                .collect()
        };
        assert!(found("cat", &["/etc/nginx/nginx.conf"]).is_empty());
        assert!(found("cat", &["/proc/1234/stack", "/proc/1234/attr/current"]).is_empty());
        assert!(found("tail", &["-n", "20", "/var/log/syslog"]).is_empty());
        assert_eq!(
            found("cat", &["/etc/hosts", "/etc/kubernetes/pki/ca.key"]),
            ["/etc/kubernetes/pki/ca.key"]
        );
        for path in [
            "/proc/1234/environ",
            "/proc/1234/task/1235/environ",
            "/proc/1234/root/etc/passwd",
            "/proc/1/cwd/x",
            "/var/lib/kubelet/pods/uid/volumes/kubernetes.io~secret/token/token",
            "/root/.kube/config",
            "/home/user/.ssh/id_ed25519",
            "/etc//shadow",
            "/etc/./ssh/ssh_host_ed25519_key",
            "/run/secrets/kubernetes.io/serviceaccount/token",
        ] {
            assert_eq!(found("head", &[path]), [path], "{path}");
        }
        // Names in a folder are not its files' content.
        assert!(found("ls", &["-la", "/etc/kubernetes/pki"]).is_empty());
        assert!(found("dmesg", &["-T"]).is_empty());
    }
}
