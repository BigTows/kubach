//! Confirmation before a Debug tab adds an ephemeral container to a pod
//! (#346): the container stays in the pod spec until the pod is deleted.
//! "Don't ask again" turns it off in the settings (General).

use gpui_kit::*;
use kubach_core::streams::debug::DEFAULT_IMAGE;
use kubach_i18n::{Message, msg, t};
use kubach_ui::ConfirmDialog;

use super::Dock;
use super::target::{PodRef, StreamRequest};
use crate::action_context;
use crate::cluster::Cluster;
use crate::settings::AppSettings;

/// Whether Debug asks before adding a container.
pub(super) fn enabled(cx: &App) -> bool {
    AppSettings::try_global(cx)
        .is_none_or(|settings| !settings.read(cx).get().general.skip_debug_confirm)
}

/// The image without its digest: `docker.io/library/busybox:1.37`.
fn short_image(image: &str) -> &str {
    image.split_once('@').map_or(image, |(name, _)| name)
}

/// What the confirmation says.
fn message(pod: &PodRef, target: Option<&str>, image: &str) -> Message {
    let (image, namespace, name) = (short_image(image), &pod.namespace, &pod.name);
    match target {
        Some(container) => msg!(
            "dock-debug-message-target",
            image = image,
            namespace = namespace,
            pod = name,
            container = container
        ),
        None => msg!(
            "dock-debug-message",
            image = image,
            namespace = namespace,
            pod = name
        ),
    }
}

/// The `kubectl debug` equivalent.
fn command(pod: &PodRef, target: Option<&str>, image: &str) -> String {
    let mut command = format!(
        "kubectl debug -it {} -n {} --image={image}",
        pod.name, pod.namespace
    );
    if let Some(target) = target {
        command.push_str(&format!(" --target={target}"));
    }
    command
}

/// Asks before opening a Debug tab on `pod` in `dock`; the tab opens only
/// if the context it was asked in is still the active one (#344).
pub(super) fn open(
    dock: WeakEntity<Dock>,
    cluster: Entity<Cluster>,
    pod: PodRef,
    container: Option<String>,
    window: &mut Window,
    cx: &mut App,
) {
    // Not connected: the Debug tab says so itself (Dock::open).
    let Some(connection) = cluster.read(cx).connection().cloned() else {
        return;
    };
    let target = container
        .clone()
        .or_else(|| pod.default_container().map(str::to_owned));
    let message = message(&pod, target.as_deref(), DEFAULT_IMAGE);
    let command = action_context::kubectl_in_context(
        &command(&pod, target.as_deref(), DEFAULT_IMAGE),
        &connection.context,
    );
    let title = t!("dock-debug-title", pod = &pod.name);
    ConfirmDialog::open(window, cx, move |_, _| {
        let context = connection.context.clone();
        ConfirmDialog::new(title)
            .context(context)
            .message(message.to_string())
            .command(command)
            .option(t!("dock-debug-dont-ask"), false)
            .confirm_label(t!("dock-debug-confirm"))
            .on_confirm_with_option(move |dont_ask, window, cx| {
                if !action_context::still_active(&cluster, &connection, cx) {
                    return;
                }
                if dont_ask && let Some(settings) = AppSettings::try_global(cx) {
                    settings.update(cx, |settings, cx| {
                        settings.edit(cx, |s| s.general.skip_debug_confirm = true)
                    });
                }
                let request = StreamRequest::Debug {
                    pod: pod.clone(),
                    container: container.clone(),
                };
                dock.update(cx, |dock, cx| dock.open_tab(request, window, cx))
                    .ok();
            })
    });
}

#[cfg(test)]
mod tests {
    use super::{command, message, short_image};
    use crate::dock::target::PodRef;

    fn pod() -> PodRef {
        let object = serde_json::from_value(serde_json::json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": "web-0", "namespace": "ns"},
            "spec": {"containers": [{"name": "app"}, {"name": "sidecar"}]}
        }))
        .unwrap();
        PodRef::from_object(&object).unwrap()
    }

    #[test]
    fn message_and_command_name_pod_target_and_image() {
        let image = "docker.io/library/busybox:1.37@sha256:abc";
        assert_eq!(short_image(image), "docker.io/library/busybox:1.37");
        assert_eq!(short_image("busybox"), "busybox");
        assert_eq!(
            message(&pod(), Some("app"), image).english(),
            "Adds an ephemeral debug container (docker.io/library/busybox:1.37) to pod ns/web-0, \
             sharing processes with app. It cannot be removed: it stays in the pod until the pod \
             is deleted."
        );
        assert_eq!(
            command(&pod(), Some("app"), image),
            "kubectl debug -it web-0 -n ns --image=docker.io/library/busybox:1.37@sha256:abc --target=app"
        );
        assert_eq!(
            command(&pod(), None, "busybox"),
            "kubectl debug -it web-0 -n ns --image=busybox"
        );
    }
}
