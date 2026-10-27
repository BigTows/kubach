//! What a running `helm` change is doing, from its `--debug` log (#382).
//!
//! `helm install` and `upgrade` print nothing until they finish, and they
//! may wait minutes for hooks (a post-install Job) or `--wait`. With
//! `--debug` they log each step to stderr: Helm 4 as `level=DEBUG msg="…"
//! key=value…`, Helm 3 as `file.go:42: [debug] …`. [`phase`] turns such a
//! line into a short phase for the progress toast.
//!
//! `--debug` also makes helm print the user-supplied and computed values,
//! the hooks and the manifest after the release summary; values may hold
//! secrets, so [`strip_debug_output`] removes those sections again.

/// Sections `--debug` adds to the release summary on stdout, before `NOTES:`.
const DEBUG_SECTIONS: [&str; 4] = [
    "USER-SUPPLIED VALUES:",
    "COMPUTED VALUES:",
    "HOOKS:",
    "MANIFEST:",
];

/// helm's stdout without the sections `--debug` adds (values, hooks,
/// manifest); the summary and the chart's `NOTES:` stay.
pub fn strip_debug_output(stdout: &str) -> String {
    let mut kept = String::with_capacity(stdout.len());
    let mut skipping = false;
    for line in stdout.split_inclusive('\n') {
        let bare = line.trim_end();
        if DEBUG_SECTIONS.contains(&bare) {
            skipping = true;
        } else if bare == "NOTES:" {
            skipping = false;
        }
        if !skipping {
            kept.push_str(line);
        }
    }
    kept
}

/// Whether `line` of stderr is a `--debug` log line rather than an error
/// or a warning meant for the user.
pub fn is_debug_line(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("level=DEBUG") || helm3_debug(line).is_some()
}

/// stderr of a failed call without the `--debug` log: what helm says went
/// wrong. Everything when only log lines are there.
pub fn failure_text(stderr: &str) -> String {
    let kept: Vec<&str> = stderr
        .lines()
        .filter(|line| !is_debug_line(line) && !line.trim().is_empty())
        .collect();
    if kept.is_empty() {
        stderr.trim().to_owned()
    } else {
        kept.join("\n")
    }
}

/// The phase a line of helm's stderr describes, e.g. `Waiting for Job
/// cert-manager-startupapicheck: InProgress`; `None` for blank lines.
pub fn phase(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    if let Some(text) = helm3_debug(line) {
        return Some(capitalize(text.trim()));
    }
    if !line.starts_with("level=") {
        return Some(line.to_owned());
    }
    let fields = logfmt(line);
    let get = |key: &str| {
        fields
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| value.as_str())
    };
    let msg = get("msg").unwrap_or_default();
    let object = match (get("kind"), get("name")) {
        (Some(kind), Some(name)) => Some(format!("{kind} {name}")),
        _ => None,
    };
    let text = match msg {
        "waiting for resource" => {
            let object = object.unwrap_or_else(|| "a resource".to_owned());
            match get("actualStatus") {
                Some(status) => format!("Waiting for {object}: {status}"),
                None => format!("Waiting for {object}"),
            }
        }
        "waiting for resources" => {
            let count = get("count").unwrap_or("the");
            let noun = if count == "1" {
                "resource"
            } else {
                "resources"
            };
            match get("timeout") {
                Some(timeout) => format!("Waiting for {count} {noun} (timeout {timeout})"),
                None => format!("Waiting for {count} {noun}"),
            }
        }
        _ => {
            let mut text = capitalize(msg);
            if let Some(object) = object {
                text = format!("{text}: {object}");
            }
            if let Some(error) = get("error") {
                text = format!("{text} ({error})");
            }
            text
        }
    };
    (!text.is_empty()).then_some(text)
}

/// The message of a Helm 3 debug line (`client.go:142: [debug] …`).
fn helm3_debug(line: &str) -> Option<&str> {
    let (_, text) = line.split_once("[debug] ")?;
    Some(text)
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// `key=value key="quoted value"` pairs of a logfmt line.
fn logfmt(line: &str) -> Vec<(&str, String)> {
    let mut fields = Vec::new();
    let mut rest = line.trim();
    while !rest.is_empty() {
        let Some((key, tail)) = rest.split_once('=') else {
            break;
        };
        let key = key.trim();
        let (value, tail) = if let Some(quoted) = tail.strip_prefix('"') {
            let mut value = String::new();
            let mut chars = quoted.char_indices();
            let mut end = quoted.len();
            while let Some((index, ch)) = chars.next() {
                match ch {
                    '\\' => {
                        if let Some((_, escaped)) = chars.next() {
                            value.push(escaped);
                        }
                    }
                    '"' => {
                        end = index + 1;
                        break;
                    }
                    _ => value.push(ch),
                }
            }
            (value, &quoted[end.min(quoted.len())..])
        } else {
            let end = tail.find(' ').unwrap_or(tail.len());
            (tail[..end].to_owned(), &tail[end..])
        };
        fields.push((key, value));
        rest = tail.trim_start();
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::{failure_text, is_debug_line, phase, strip_debug_output};

    #[test]
    fn helm4_lines_become_phases() {
        assert_eq!(
            phase(
                r#"level=DEBUG msg="waiting for resource" namespace=cert-manager name=cm-startupapicheck kind=Job expectedStatus=Current actualStatus=InProgress"#
            )
            .as_deref(),
            Some("Waiting for Job cm-startupapicheck: InProgress")
        );
        assert_eq!(
            phase(r#"level=DEBUG msg="waiting for resources" count=1 timeout=5m0s"#).as_deref(),
            Some("Waiting for 1 resource (timeout 5m0s)")
        );
        assert_eq!(
            phase(
                r#"level=DEBUG msg="Created resource via patch" namespace=apps name=demo-check gvk="batch/v1, Kind=Job""#
            )
            .as_deref(),
            Some("Created resource via patch")
        );
        assert_eq!(
            phase(
                r#"level=DEBUG msg="ignoring delete failure" namespace=a name=demo-check kind=Job error="jobs.batch \"demo-check\" not found""#
            )
            .as_deref(),
            Some(r#"Ignoring delete failure: Job demo-check (jobs.batch "demo-check" not found)"#)
        );
        assert_eq!(
            phase(r#"level=WARN msg="upgrade failed" name=demo error="context canceled""#)
                .as_deref(),
            Some("Upgrade failed (context canceled)")
        );
    }

    #[test]
    fn helm3_and_plain_lines() {
        assert_eq!(
            phase("client.go:142: [debug] creating 1 resource(s)").as_deref(),
            Some("Creating 1 resource(s)")
        );
        assert_eq!(
            phase("WARNING: Kubernetes configuration file is group-readable").as_deref(),
            Some("WARNING: Kubernetes configuration file is group-readable")
        );
        assert_eq!(phase("   "), None);
    }

    #[test]
    fn debug_sections_are_removed_from_the_summary() {
        let stdout = "NAME: demo\nSTATUS: deployed\nUSER-SUPPLIED VALUES:\npassword: s3cret\n\n\
                      COMPUTED VALUES:\npassword: s3cret\n\nHOOKS:\n---\nkind: Job\n\
                      MANIFEST:\n---\nkind: Secret\n\nNOTES:\nThanks for installing.\n";
        let kept = strip_debug_output(stdout);
        assert_eq!(
            kept,
            "NAME: demo\nSTATUS: deployed\nNOTES:\nThanks for installing.\n"
        );
        assert!(!kept.contains("s3cret"));
        // Without debug sections nothing changes.
        assert_eq!(strip_debug_output("NAME: demo\n"), "NAME: demo\n");
    }

    #[test]
    fn failures_skip_the_debug_log() {
        let stderr = "level=DEBUG msg=\"preparing upgrade\" name=demo\n\
                      level=WARN msg=\"upgrade failed\" name=demo error=\"context canceled\"\n\
                      Error: UPGRADE FAILED: context canceled\n";
        assert_eq!(
            failure_text(stderr),
            "level=WARN msg=\"upgrade failed\" name=demo error=\"context canceled\"\n\
             Error: UPGRADE FAILED: context canceled"
        );
        assert!(is_debug_line(
            "install.go:214: [debug] Original chart version"
        ));
        assert_eq!(
            failure_text("level=DEBUG msg=\"x\"\n"),
            "level=DEBUG msg=\"x\""
        );
    }
}
