//! `debug_container` (#64): an ephemeral debug container for pods whose
//! images have no shell (distroless), like
//! `kubectl debug <pod> --image=busybox --target=<container>`.
//!
//! The container is added through the `pods/ephemeralcontainers`
//! subresource, sharing the process namespace of the target container when
//! one is named, and keeps running its image's default command (busybox:
//! `sh` on an open stdin). The agent's command, if any, then runs in it
//! exactly as `exec` runs one: no TTY, bounded and masked output. Later
//! commands go through `exec` with `container: <name>`, each confirmed on
//! its own, so the pod does not collect a container per command.
//!
//! Safety mirrors exec: the debug level, `modify(ns)` where the API server
//! holds the boundary, the user's RBAC (`patch pods/ephemeralcontainers`,
//! and `create pods/exec` for a command), and a confirmation for every call
//! that is never remembered: the change is permanent, an ephemeral
//! container stays in the pod until the pod is deleted. Kubach never sets
//! `privileged`, host namespaces or capabilities. A server-side dry run
//! comes first; when the namespace's Pod Security level rejects the plain
//! container, a restricted security context is tried, and if that is
//! rejected too, nobody is asked.

use std::fmt::Write as _;
use std::time::Duration;

use k8s_openapi::api::core::v1::{Capabilities, Pod, SeccompProfile, SecurityContext};
use kubach_core::streams::debug::{DEFAULT_IMAGE, generate_name};
use kubach_core::streams::{
    DebugContainer, DebugOptions, StreamError, add_debug_container, wait_for_debug_container,
};
use kube::Client;
use kube::api::Api;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::confirm::RequestKind;
use crate::policy::{Denial, DenialCode, Finding, Request};
use crate::tools::{
    Ctx, DEFAULT_EXEC_TIMEOUT, MAX_EXEC_TIMEOUT, ToolError, ToolOutput, authorize, confirm_as,
    ensure_rbac, parse, run_command, shell_words,
};

/// How long the debug container may take to start (image pull included).
const START_TIMEOUT: Duration = Duration::from_secs(90);

/// The user a restricted debug container runs as when neither the target
/// container nor the pod names a non-root one: `nobody`.
const NOBODY: i64 = 65534;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DebugArgs {
    pod: String,
    target_container: Option<String>,
    name: Option<String>,
    image: Option<String>,
    namespace: Option<String>,
    #[serde(default)]
    command: Vec<String>,
    timeout_seconds: Option<u64>,
}

fn stream_error(error: StreamError) -> ToolError {
    match error {
        StreamError::Api(error) => ToolError::from(*error),
        other => ToolError::Failed(other.to_string()),
    }
}

/// Whether the API server refused the container for the namespace's Pod
/// Security level.
fn is_pod_security(error: &StreamError) -> bool {
    matches!(error, StreamError::Api(error) if matches!(
        error.as_ref(),
        kube::Error::Api(status) if status.code == 403 && status.message.contains("PodSecurity")
    ))
}

/// What the restricted Pod Security level asks of a container: no
/// privilege escalation, all capabilities dropped, a non-root user and the
/// runtime's seccomp profile. The user is the target container's or the
/// pod's, so the debug container can see the target's files in `/proc`.
pub(crate) fn restricted_context(pod: &Pod, target: Option<&str>) -> SecurityContext {
    let spec = pod.spec.as_ref();
    let container_user = target.and_then(|target| {
        spec?
            .containers
            .iter()
            .find(|c| c.name == target)?
            .security_context
            .as_ref()?
            .run_as_user
    });
    let pod_user = spec
        .and_then(|spec| spec.security_context.as_ref())
        .and_then(|context| context.run_as_user);
    let user = container_user
        .or(pod_user)
        .filter(|user| *user != 0)
        .unwrap_or(NOBODY);
    SecurityContext {
        allow_privilege_escalation: Some(false),
        capabilities: Some(Capabilities {
            drop: Some(vec!["ALL".to_owned()]),
            add: None,
        }),
        run_as_non_root: Some(true),
        run_as_user: Some(user),
        seccomp_profile: Some(SeccompProfile {
            type_: "RuntimeDefault".to_owned(),
            localhost_profile: None,
        }),
        ..Default::default()
    }
}

/// Every container name the pod already has: a new one must differ.
fn taken_names(pod: &Pod) -> Vec<&str> {
    let Some(spec) = pod.spec.as_ref() else {
        return Vec::new();
    };
    spec.containers
        .iter()
        .map(|c| c.name.as_str())
        .chain(
            spec.init_containers
                .iter()
                .flatten()
                .map(|c| c.name.as_str()),
        )
        .chain(
            spec.ephemeral_containers
                .iter()
                .flatten()
                .map(|c| c.name.as_str()),
        )
        .collect()
}

/// Refuses what cannot work before the user is asked: a pod that is not
/// running, an unknown target, a taken name.
fn check_pod(pod: &Pod, target: Option<&str>, name: Option<&str>) -> Result<(), Denial> {
    let pod_name = pod.metadata.name.clone().unwrap_or_default();
    let phase = pod
        .status
        .as_ref()
        .and_then(|status| status.phase.as_deref())
        .unwrap_or("Unknown");
    if phase != "Running" {
        return Err(Denial::new(
            DenialCode::InvalidInput,
            format!("pod {pod_name} is {phase}; a debug container starts only in a running pod"),
        ));
    }
    if let Some(target) = target {
        let containers: Vec<&str> = pod
            .spec
            .iter()
            .flat_map(|spec| spec.containers.iter().map(|c| c.name.as_str()))
            .collect();
        if !containers.contains(&target) {
            return Err(Denial::new(
                DenialCode::InvalidInput,
                format!(
                    "pod {pod_name} has no container {target:?} to share processes with \
                     (its containers: {})",
                    containers.join(", ")
                ),
            ));
        }
    }
    if let Some(name) = name
        && taken_names(pod).contains(&name)
    {
        return Err(Denial::new(
            DenialCode::InvalidInput,
            format!(
                "pod {pod_name} already has a container named {name}; if it is a running debug \
                 container, use exec with container {name}, else pick another name"
            ),
        ));
    }
    Ok(())
}

/// The dry run under the session's identity; with the restricted security
/// context when Pod Security refuses the plain container. Returns whether
/// the restricted one is needed.
async fn dry_run(
    client: &Client,
    namespace: &str,
    pod: &Pod,
    options: &mut DebugOptions,
) -> Result<bool, ToolError> {
    let pod_name = pod.metadata.name.clone().unwrap_or_default();
    match add_debug_container(client, namespace, &pod_name, options, true).await {
        Ok(_) => return Ok(false),
        Err(error) if is_pod_security(&error) => {}
        Err(error) => return Err(stream_error(error)),
    }
    options.security_context = Some(restricted_context(pod, options.target.as_deref()));
    match add_debug_container(client, namespace, &pod_name, options, true).await {
        Ok(_) => Ok(true),
        Err(error) if is_pod_security(&error) => Err(Denial::new(
            DenialCode::InvalidInput,
            format!(
                "the Pod Security level of namespace {namespace} rejects this debug container \
                 even with a restricted security context (non-root, no capabilities): {}. \
                 Kubach never adds privileged or host settings",
                stream_error(error).message()
            ),
        )
        .into()),
        Err(error) => Err(stream_error(error)),
    }
}

/// What the user must see before an ephemeral container is added.
fn findings(
    name: &str,
    image: &str,
    target: Option<&str>,
    restricted: Option<&SecurityContext>,
) -> Vec<Finding> {
    let finding = |path: &str, message: String| Finding {
        document: None,
        path: path.to_owned(),
        message,
    };
    let mut out = vec![finding(
        "spec.ephemeralContainers",
        format!(
            "adds debug container {name} to the pod for good: ephemeral containers cannot be \
             removed, it stays in the pod spec until the pod is deleted"
        ),
    )];
    if let Some(target) = target {
        out.push(finding(
            "spec.ephemeralContainers.targetContainerName",
            format!(
                "shares the process namespace of container {target}: the debug container sees \
                 its processes and, through /proc, their files and environment"
            ),
        ));
    }
    if image != DEFAULT_IMAGE {
        out.push(finding(
            "spec.ephemeralContainers.image",
            format!("image {image} is chosen by the agent and runs inside the pod"),
        ));
    }
    if let Some(context) = restricted {
        out.push(finding(
            "spec.ephemeralContainers.securityContext",
            format!(
                "the namespace's Pod Security level is restricted: runs as user {}, without \
                 privilege escalation or capabilities",
                context.run_as_user.unwrap_or(NOBODY)
            ),
        ));
    }
    out
}

/// `debug_container`.
pub(crate) async fn debug_container(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    let a: DebugArgs = parse(args)?;
    let request = Request::Debug {
        pod: a.pod,
        target: a.target_container.clone(),
        name: a.name.clone(),
        image: a.image.clone(),
        namespace: a.namespace,
        command: a.command.clone(),
    };
    let allowed = authorize(ctx, &request).await?;
    let namespace = allowed.namespace.clone();
    let mut rights = vec![("patch", "", "pods/ephemeralcontainers")];
    if !a.command.is_empty() {
        rights.push(("create", "", "pods/exec"));
    }
    ensure_rbac(ctx, &namespace, &rights).await?;
    let pod_name = allowed.name.clone().expect("policy validated the name");
    let target = a.target_container.as_deref();
    let client = ctx.session.client_in(&namespace)?;
    let pod = Api::<Pod>::namespaced(client.clone(), &namespace)
        .get(&pod_name)
        .await?;
    check_pod(&pod, target, a.name.as_deref())?;
    let name = match a.name {
        Some(name) => name,
        None => {
            let taken = taken_names(&pod);
            loop {
                let name = generate_name();
                if !taken.contains(&name.as_str()) {
                    break name;
                }
            }
        }
    };
    let image = a.image.unwrap_or_else(|| DEFAULT_IMAGE.to_owned());
    let mut options = DebugOptions {
        image: image.clone(),
        target: a.target_container.clone(),
        name: Some(name.clone()),
        // The image's own command (busybox: `sh`) on an open stdin keeps
        // the container running for exec.
        command: None,
        tty: false,
        stdin: true,
        security_context: None,
    };
    // A container the API server would refuse is not put to the user.
    let restricted = dry_run(&client, &namespace, &pod, &mut options).await?;
    let restricted = restricted
        .then_some(options.security_context.as_ref())
        .flatten();

    let sharing = target
        .map(|target| format!(", sharing processes with container {target}"))
        .unwrap_or_default();
    let place = format!("pod {namespace}/{pod_name}");
    let dangerous = findings(&name, &image, target, restricted);
    let (kind, summary, details) = if a.command.is_empty() {
        (
            RequestKind::Call,
            format!(
                "Add debug container {name} ({image}) to {place}{sharing} and leave it running"
            ),
            format!(
                "Pod: {namespace}/{pod_name}\nTarget container: {}\nImage: {image}\nContainer: \
                 {name}\nCommand: none now; the agent may run commands in it later with exec, \
                 each confirmed on its own.\nThe container cannot be removed: it stays in the \
                 pod until the pod is deleted.",
                target.unwrap_or("none")
            ),
        )
    } else {
        (
            RequestKind::Exec {
                command: a.command.clone(),
            },
            format!(
                "Add debug container {name} ({image}) to {place}{sharing}, then run a command \
                 in it"
            ),
            shell_words(&a.command),
        )
    };
    confirm_as(
        ctx,
        kind,
        &allowed,
        "debug_container",
        summary,
        details,
        dangerous,
    )
    .await?;

    // Noted before the call: a failed answer may still have changed it.
    ctx.session.note_change(&namespace, "Pod", &pod_name);
    let container: DebugContainer =
        add_debug_container(&client, &namespace, &pod_name, &options, false)
            .await
            .map_err(stream_error)?;
    let added = format!("Debug container {name} ({image}) was added to {place}{sharing}");
    wait_for_debug_container(&client, &container, START_TIMEOUT)
        .await
        .map_err(|error| {
            ToolError::Failed(format!(
                "{added}, but it did not start: {error}. It stays in the pod spec until the pod \
                 is deleted; the image may need a long-running default command (a shell)"
            ))
        })?;
    let mut out = format!(
        "{added} and is running. It stays until the pod is deleted: run further commands in it \
         with exec (pod {pod_name}, container {name}) instead of adding another.\n"
    );
    if let Some(context) = restricted {
        let _ = writeln!(
            out,
            "It runs as user {} without capabilities (restricted Pod Security).",
            context.run_as_user.unwrap_or(NOBODY)
        );
    }
    if !a.command.is_empty() {
        let timeout = a
            .timeout_seconds
            .unwrap_or(DEFAULT_EXEC_TIMEOUT)
            .clamp(1, MAX_EXEC_TIMEOUT);
        let result = run_command(ctx, &namespace, &pod_name, Some(&name), &a.command, timeout)
            .await
            .map_err(|error| {
                ToolError::Failed(format!(
                    "{added} and runs, but the command failed: {}",
                    error.message()
                ))
            })?;
        out.push_str(&result);
    }
    Ok(ToolOutput::confirmed(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pod(value: serde_json::Value) -> Pod {
        serde_json::from_value(value).unwrap()
    }

    fn running(spec: serde_json::Value) -> Pod {
        pod(json!({"metadata": {"name": "web-0"}, "spec": spec,
                   "status": {"phase": "Running"}}))
    }

    #[test]
    fn restricted_context_runs_as_the_target_user_never_root() {
        let p = running(json!({
        "securityContext": {"runAsUser": 1000},
        "containers": [
            {"name": "app", "securityContext": {"runAsUser": 472}},
            {"name": "side"},
        ]}));
        let context = restricted_context(&p, Some("app"));
        assert_eq!(context.run_as_user, Some(472));
        assert_eq!(context.run_as_non_root, Some(true));
        assert_eq!(context.allow_privilege_escalation, Some(false));
        assert_eq!(context.privileged, None);
        let caps = context.capabilities.unwrap();
        assert_eq!(caps.drop, Some(vec!["ALL".to_owned()]));
        assert_eq!(caps.add, None);
        assert_eq!(context.seccomp_profile.unwrap().type_, "RuntimeDefault");
        assert_eq!(restricted_context(&p, Some("side")).run_as_user, Some(1000));
        let root = running(json!({"securityContext": {"runAsUser": 0},
                                  "containers": [{"name": "app"}]}));
        assert_eq!(restricted_context(&root, None).run_as_user, Some(NOBODY));
    }

    #[test]
    fn pods_that_cannot_take_a_debug_container_are_refused() {
        let p = running(json!({"containers": [{"name": "app"}],
                               "ephemeralContainers": [{"name": "debugger-a1b2c", "image": "busybox"}]}));
        assert!(check_pod(&p, Some("app"), Some("dbg")).is_ok());
        let missing = check_pod(&p, Some("nope"), None).unwrap_err();
        assert!(
            missing.message.contains("no container \"nope\""),
            "{missing:?}"
        );
        let taken = check_pod(&p, None, Some("debugger-a1b2c")).unwrap_err();
        assert!(taken.message.contains("use exec"), "{taken:?}");
        let pending = pod(json!({"metadata": {"name": "web-0"},
                                 "spec": {"containers": [{"name": "app"}]},
                                 "status": {"phase": "Pending"}}));
        let pending = check_pod(&pending, None, None).unwrap_err();
        assert!(pending.message.contains("is Pending"), "{pending:?}");
    }

    #[test]
    fn findings_say_the_change_is_permanent() {
        let all = findings("dbg", "nicolaka/netshoot:v0.13", Some("app"), None);
        assert!(all[0].message.contains("until the pod is deleted"));
        assert!(all.iter().any(|f| f.path.ends_with("targetContainerName")));
        assert!(all.iter().any(|f| f.path.ends_with("image")));
        assert_eq!(findings("dbg", DEFAULT_IMAGE, None, None).len(), 1);
    }
}
