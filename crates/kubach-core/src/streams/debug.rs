//! Ephemeral debug containers, like `kubectl debug -it pod --image=… --target=…`.
//!
//! [`create_debug_container`] adds the container through the
//! `ephemeralcontainers` subresource, [`wait_for_debug_container`] waits until
//! it runs; then attach to it with [`start_session`](super::exec::start_session)
//! and [`SessionCommand::Attach`](super::exec::SessionCommand::Attach).

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use k8s_openapi::api::core::v1::{EphemeralContainer, Pod, SecurityContext};
use kube::Client;
use kube::api::{Api, Patch, PatchParams};

use super::StreamError;

/// Default debug image: busybox from Docker Hub with an explicit registry
/// and pinned to the digest of its multi-arch index, so neither a registry
/// mirror nor a re-pushed tag can swap what runs inside the pod (#202).
pub const DEFAULT_IMAGE: &str = "docker.io/library/busybox:1.37@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e";

/// What to start.
#[derive(Clone, Debug)]
pub struct DebugOptions {
    pub image: String,
    /// Share the process namespace of this container (`--target`).
    pub target: Option<String>,
    /// Generated (`debugger-xxxxx`) when not set.
    pub name: Option<String>,
    /// Image entrypoint when not set.
    pub command: Option<Vec<String>>,
    pub tty: bool,
    pub stdin: bool,
    /// None: the pod's defaults. Never privileged (#64).
    pub security_context: Option<SecurityContext>,
}

impl Default for DebugOptions {
    fn default() -> Self {
        Self {
            image: DEFAULT_IMAGE.to_owned(),
            target: None,
            name: None,
            command: None,
            tty: true,
            stdin: true,
            security_context: None,
        }
    }
}

impl DebugOptions {
    /// The ephemeral container to add, named `name`.
    pub fn to_container(&self, name: &str) -> EphemeralContainer {
        EphemeralContainer {
            name: name.to_owned(),
            image: Some(self.image.clone()),
            command: self.command.clone(),
            target_container_name: self.target.clone(),
            stdin: Some(self.stdin),
            tty: Some(self.tty),
            termination_message_policy: Some("File".to_owned()),
            security_context: self.security_context.clone(),
            ..Default::default()
        }
    }
}

/// A created debug container.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugContainer {
    pub namespace: String,
    pub pod: String,
    pub name: String,
}

/// Adds an ephemeral container to `pod`. Must run on the
/// [`runtime`](crate::runtime).
pub async fn create_debug_container(
    client: &Client,
    namespace: &str,
    pod: &str,
    options: &DebugOptions,
) -> Result<DebugContainer, StreamError> {
    add_debug_container(client, namespace, pod, options, false).await
}

/// [`create_debug_container`], or with `dry_run` only the API server's
/// verdict (admission, Pod Security included) without changing the pod.
/// A dry run with a generated name checks that name only: pass
/// [`DebugOptions::name`] to check the one that will be used.
pub async fn add_debug_container(
    client: &Client,
    namespace: &str,
    pod: &str,
    options: &DebugOptions,
    dry_run: bool,
) -> Result<DebugContainer, StreamError> {
    let api: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let current = api.get(pod).await?;
    let spec = current.spec.unwrap_or_default();
    if let Some(target) = &options.target
        && !spec.containers.iter().any(|c| &c.name == target)
    {
        return Err(StreamError::Container {
            pod: pod.to_owned(),
            container: target.clone(),
            reason: "no such container".to_owned(),
        });
    }
    let taken: Vec<&str> = spec
        .containers
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
        .collect();
    let name = match &options.name {
        Some(name) => name.clone(),
        None => loop {
            let name = generate_name();
            if !taken.contains(&name.as_str()) {
                break name;
            }
        },
    };
    // A strategic merge patch appends to the list instead of replacing it.
    let patch = serde_json::json!({
        "spec": { "ephemeralContainers": [options.to_container(&name)] }
    });
    let params = PatchParams {
        dry_run,
        ..Default::default()
    };
    api.patch_ephemeral_containers(pod, &params, &Patch::Strategic(patch))
        .await?;
    Ok(DebugContainer {
        namespace: namespace.to_owned(),
        pod: pod.to_owned(),
        name,
    })
}

/// Waits until the debug container is running. Fails if it terminates, and
/// on timeout reports why it was still waiting (e.g. `ImagePullBackOff`).
pub async fn wait_for_debug_container(
    client: &Client,
    container: &DebugContainer,
    timeout: Duration,
) -> Result<(), StreamError> {
    let api: Api<Pod> = Api::namespaced(client.clone(), &container.namespace);
    let deadline = Instant::now() + timeout;
    let mut waiting = "not started".to_owned();
    while Instant::now() < deadline {
        let pod = api.get(&container.pod).await?;
        let status = pod
            .status
            .and_then(|status| status.ephemeral_container_statuses)
            .into_iter()
            .flatten()
            .find(|status| status.name == container.name);
        if let Some(state) = status.and_then(|status| status.state) {
            if state.running.is_some() {
                return Ok(());
            }
            if let Some(terminated) = state.terminated {
                return Err(StreamError::Container {
                    pod: container.pod.clone(),
                    container: container.name.clone(),
                    reason: terminated
                        .message
                        .or(terminated.reason)
                        .unwrap_or_else(|| format!("exited with {}", terminated.exit_code)),
                });
            }
            if let Some(reason) = state.waiting.and_then(|waiting| waiting.reason) {
                waiting = reason;
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(StreamError::Timeout(format!(
        "debug container {} is {waiting}",
        container.name
    )))
}

/// `debugger-` plus five base-36 characters, like kubectl.
pub fn generate_name() -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    let mut seed = nanos
        ^ COUNTER
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_mul(2_654_435_761);
    let mut suffix = String::with_capacity(5);
    for _ in 0..5 {
        let digit = seed % 36;
        seed /= 36;
        suffix.push(char::from_digit(digit, 36).unwrap_or('x'));
    }
    format!("debugger-{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_ephemeral_container() {
        let options = DebugOptions {
            target: Some("app".into()),
            command: Some(vec!["sh".into()]),
            ..Default::default()
        };
        let container = options.to_container("debugger-abcde");
        assert_eq!(container.name, "debugger-abcde");
        assert_eq!(container.image.as_deref(), Some(DEFAULT_IMAGE));
        assert_eq!(container.target_container_name.as_deref(), Some("app"));
        assert_eq!(container.stdin, Some(true));
        assert_eq!(container.tty, Some(true));
        let json = serde_json::to_value(&container).unwrap();
        assert_eq!(json["targetContainerName"], "app");
    }

    #[test]
    fn default_image_names_registry_and_digest() {
        let (reference, digest) = DEFAULT_IMAGE.split_once("@sha256:").unwrap();
        assert!(reference.starts_with("docker.io/library/busybox:"));
        assert_eq!(digest.len(), 64);
        assert!(
            digest
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        );
    }

    #[test]
    fn generated_names_are_valid_and_differ() {
        let a = generate_name();
        let b = generate_name();
        assert_ne!(a, b);
        assert_eq!(a.len(), "debugger-".len() + 5);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        );
    }
}
