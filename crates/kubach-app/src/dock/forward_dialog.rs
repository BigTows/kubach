//! Dialog that starts a port-forward to a pod, or to a ready pod of a
//! Service or workload. Every start goes through it, so the local port can
//! be picked (#464): empty is a random free port, a port the user typed is
//! remembered per context, namespace, source and port in the application
//! state and offered next time.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::runtime;
use kubach_core::state::forward_key;
use kubach_core::streams::{
    ForwardInfo, ForwardPort, ForwardSource, LocalPort, TargetPod, TargetPort, target_pod,
};
use kubach_i18n::t;

use super::forwards::{ForwardRequest, Forwards};
use super::target::http_scheme;
use crate::cluster::Cluster;
use crate::state::StateStore;

/// Parses a port field; empty means "any free port" when `allow_auto`.
pub fn parse_port(text: &str, allow_auto: bool) -> Result<u16, String> {
    let text = text.trim();
    if text.is_empty() {
        return if allow_auto {
            Ok(0)
        } else {
            Err(t!("dock-forward-enter-port"))
        };
    }
    match text.parse::<u16>() {
        Ok(0) | Err(_) => Err(t!("dock-forward-invalid-port", text = text)),
        Ok(port) => Ok(port),
    }
}

/// Button text for a port: `80 → http (web)`, `8080 (http)`.
fn port_label(port: &ForwardPort) -> String {
    let mut text = port.port.to_string();
    match &port.target {
        TargetPort::Number(number) if *number != port.port => {
            text.push_str(&format!(" → {number}"));
        }
        TargetPort::Name(name) => text.push_str(&format!(" → {name}")),
        TargetPort::Number(_) => {}
    }
    if let Some(name) = &port.name {
        text.push_str(&format!(" ({name})"));
    }
    if !port.is_tcp() {
        text.push_str(&format!(" {}", port.protocol));
    }
    text
}

/// Menu item text for a port: `80 → 8080 · http · TCP` for a Service
/// (`port → targetPort`), `8080 · http · TCP` for a container port; non-TCP
/// ports say why they are off.
pub fn menu_label(port: &ForwardPort) -> String {
    let mut text = port.port.to_string();
    if port.container.is_none() {
        match &port.target {
            TargetPort::Number(number) => text.push_str(&format!(" → {number}")),
            TargetPort::Name(name) => text.push_str(&format!(" → {name}")),
        }
    }
    if let Some(name) = &port.name {
        text.push_str(&format!(" · {name}"));
    }
    text.push_str(&format!(" · {}", port.protocol));
    if !port.is_tcp() {
        return t!("dock-forward-menu-tcp-only", port = text);
    }
    text
}

/// Name of the TCP port `port` of `source`, for "Open in browser".
fn port_name(source: &ForwardSource, port: u16) -> Option<String> {
    source
        .ports
        .iter()
        .find(|candidate| candidate.port == port && candidate.is_tcp())
        .and_then(|candidate| candidate.name.clone())
}

/// "Forwarding 127.0.0.1:8080 → svc/web:80" with Open and Copy buttons for
/// `http://127.0.0.1:8080`.
fn notify_started(
    info: &ForwardInfo,
    source: &ForwardSource,
    port_name: Option<&str>,
    window: &mut Window,
    cx: &mut App,
) {
    let scheme = http_scheme(info.source_port, port_name)
        .or_else(|| http_scheme(info.remote_port, port_name))
        .unwrap_or("http");
    let url = format!("{scheme}://{}", info.local_addr);
    let mut message = format!("{}:{} → {url}", info.source, info.source_port);
    if source.selector.is_some() {
        message = t!(
            "dock-forward-started-via",
            route = message,
            pod = &info.pod,
            port = info.remote_port.to_string()
        );
    }
    let id = info.id;
    let notification = Notification::success(message.clone())
        .title(t!("dock-forward-started"))
        .content(move |_, _, cx| {
            let theme = cx.theme();
            let (open, copy) = (url.clone(), url.clone());
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(message.clone()),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            Button::new(("forward-open-url", id))
                                .outline()
                                .xsmall()
                                .label(t!("dock-forward-open"))
                                .tooltip(t!("dock-forward-open-in-browser", url = &open))
                                .on_click(move |_, _, cx| cx.open_url(&open)),
                        )
                        .child(
                            Button::new(("forward-copy-url", id))
                                .ghost()
                                .xsmall()
                                .label(t!("dock-forward-copy"))
                                .tooltip(t!("dock-forward-copy-tooltip"))
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                                }),
                        ),
                )
                .into_any_element()
        });
    window.push_notification(notification, cx);
}

/// Local port remembered for `port` of `source` in `context`.
fn saved_port(cx: &App, context: Option<&str>, source: &ForwardSource, port: u16) -> Option<u16> {
    let context = context?;
    let key = forward_key(&source.namespace, &source.label(), port);
    StateStore::try_global(cx)?
        .read(cx)
        .get()
        .forward_port(context, &key)
}

/// Remembers the local port picked for `port` of `source` in `context`;
/// `None` (a random port) forgets it.
fn remember_port(
    cx: &mut App,
    context: &str,
    source: &ForwardSource,
    port: u16,
    local: Option<u16>,
) {
    let Some(store) = StateStore::try_global(cx) else {
        return;
    };
    let key = forward_key(&source.namespace, &source.label(), port);
    store.update(cx, |store, cx| {
        store.edit(cx, |state| {
            state.remember_forward_port(context, &key, local)
        })
    });
}

/// Opens the dialog for `source`, with `port` picked when given (a port
/// button or menu item) and the local port field focused then.
pub fn open(
    cluster: Entity<Cluster>,
    forwards: Entity<Forwards>,
    source: ForwardSource,
    port: Option<u16>,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| ForwardForm::new(cluster, forwards, source, port, window, cx));
    let focus = match port {
        Some(_) => form.read(cx).local.clone(),
        None => form.read(cx).remote.clone(),
    };
    focus.update(cx, |input, cx| input.focus(window, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title(t!("dock-forward-title"))
            .w(px(460.))
            .child(form.clone())
    });
}

/// The pod the forward would go to, looked up when the dialog opens.
enum Target {
    Loading,
    Found(TargetPod),
    Failed(String),
}

pub struct ForwardForm {
    cluster: Entity<Cluster>,
    /// Where the dialog was opened: the forward goes there, and only while
    /// it is still the active connection (#344).
    connection: Option<kubach_core::Connection>,
    forwards: Entity<Forwards>,
    source: ForwardSource,
    target: Target,
    remote: Entity<InputState>,
    local: Entity<InputState>,
    /// What the local field was filled with for the current remote port;
    /// while the user has not changed it, another remote port refills it.
    prefilled: String,
    error: Option<String>,
    starting: bool,
    _subscriptions: Vec<Subscription>,
    _lookup: Option<Task<()>>,
    _start: Option<Task<()>>,
}

impl ForwardForm {
    fn new(
        cluster: Entity<Cluster>,
        forwards: Entity<Forwards>,
        source: ForwardSource,
        port: Option<u16>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let first = port
            .or_else(|| {
                source
                    .ports
                    .iter()
                    .find(|port| port.is_tcp())
                    .map(|port| port.port)
            })
            .map(|port| port.to_string())
            .unwrap_or_default();
        let connection = cluster.read(cx).connection().cloned();
        let context = connection.as_ref().map(|c| c.context.as_str());
        let prefilled = parse_port(&first, false)
            .ok()
            .and_then(|port| saved_port(cx, context, &source, port))
            .map(|port| port.to_string())
            .unwrap_or_default();
        let placeholder: fn() -> String = if source.is_service() {
            || t!("dock-forward-service-port")
        } else {
            || t!("dock-forward-pod-port")
        };
        let remote = cx.new(|cx| InputState::new(window, cx).default_value(first));
        let local = cx.new(|cx| InputState::new(window, cx).default_value(prefilled.clone()));
        let on_input = |this: &mut Self,
                        event: &InputEvent,
                        window: &mut Window,
                        cx: &mut Context<Self>| match event {
            InputEvent::PressEnter { .. } => this.submit(window, cx),
            InputEvent::Change => {
                this.error = None;
                cx.notify()
            }
            _ => {}
        };
        let mut subscriptions = vec![
            cx.subscribe_in(&remote, window, move |this, _, event, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.refill_local(window, cx);
                }
                on_input(this, event, window, cx)
            }),
            cx.subscribe_in(&local, window, move |this, _, event, window, cx| {
                on_input(this, event, window, cx)
            }),
        ];
        subscriptions.extend(crate::i18n::localized_placeholder(
            &remote,
            placeholder,
            window,
            cx,
        ));
        subscriptions.extend(crate::i18n::localized_placeholder(
            &local,
            || t!("dock-forward-local-placeholder"),
            window,
            cx,
        ));
        let mut form = Self {
            cluster,
            connection,
            forwards,
            source,
            target: Target::Loading,
            remote,
            local,
            prefilled,
            error: None,
            starting: false,
            _subscriptions: subscriptions,
            _lookup: None,
            _start: None,
        };
        form.look_up_target(cx);
        form
    }

    /// Context the dialog was opened in.
    fn context(&self) -> Option<&str> {
        self.connection.as_ref().map(|c| c.context.as_str())
    }

    /// Another remote port: the local field takes the port remembered for
    /// it, unless the user already typed one.
    fn refill_local(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.local.read(cx).value().as_ref() != self.prefilled.as_str() {
            return;
        }
        let saved = parse_port(&self.remote.read(cx).value(), false)
            .ok()
            .and_then(|port| saved_port(cx, self.context(), &self.source, port))
            .map(|port| port.to_string())
            .unwrap_or_default();
        if saved == self.prefilled {
            return;
        }
        self.prefilled = saved.clone();
        self.local
            .update(cx, |input, cx| input.set_value(saved, window, cx));
    }

    /// Finds the pod a forward would go to now, for the preview.
    fn look_up_target(&mut self, cx: &mut Context<Self>) {
        let client = match self.client(cx) {
            Ok(client) => client,
            Err(error) => {
                self.target = Target::Failed(error);
                return;
            }
        };
        let source = self.source.clone();
        let task = runtime::spawn(async move { target_pod(&client, &source).await });
        self._lookup = Some(cx.spawn(async move |this, cx| {
            let target = match task.await {
                Ok(Ok(pod)) => Target::Found(pod),
                Ok(Err(error)) => Target::Failed(error.to_string()),
                Err(error) => Target::Failed(error.to_string()),
            };
            this.update(cx, |this, cx| {
                this.target = target;
                cx.notify();
            })
            .ok();
        }));
    }

    /// Client of the connection the dialog was opened in, while it is
    /// still the active one.
    fn client(&self, cx: &App) -> Result<kube::Client, String> {
        let connection = self
            .connection
            .as_ref()
            .ok_or_else(|| t!("dock-not-connected"))?;
        crate::action_context::check(self.cluster.read(cx), connection)
            .map_err(|changed| changed.to_string())?;
        Ok(connection.client.clone())
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.starting {
            return;
        }
        let remote = parse_port(&self.remote.read(cx).value(), false);
        let local = parse_port(&self.local.read(cx).value(), true);
        let (port, local_port) = match (remote, local) {
            (Ok(remote), Ok(local)) => (remote, local),
            (Err(error), _) | (_, Err(error)) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let client = match self.client(cx) {
            Ok(client) => client,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let port_name = port_name(&self.source, port);
        let context = self.context().map(ToOwned::to_owned);
        let request = ForwardRequest {
            context: context.clone(),
            source: self.source.clone(),
            port,
            local_port: LocalPort::Exact(local_port),
            port_name: port_name.clone(),
        };
        let task = self
            .forwards
            .update(cx, |forwards, cx| forwards.start(client, request, cx));
        self.starting = true;
        self.error = None;
        cx.notify();
        self._start = Some(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.starting = false;
                match result {
                    Ok(info) => {
                        if let Some(context) = &context {
                            let picked = (local_port != 0).then_some(local_port);
                            remember_port(cx, context, &this.source, port, picked);
                        }
                        window.close_dialog(cx);
                        notify_started(&info, &this.source, port_name.as_deref(), window, cx);
                    }
                    Err(error) => {
                        this.error = Some(error);
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
    }

    /// "→ pod web-1:8080" for the port in the input, or why it cannot go.
    fn route_preview(&self, cx: &App) -> Option<Result<String, String>> {
        let Target::Found(pod) = &self.target else {
            return None;
        };
        let port = parse_port(&self.remote.read(cx).value(), false).ok()?;
        Some(
            pod.port_for(&self.source, port)
                .map(|pod_port| {
                    t!(
                        "dock-forward-route",
                        pod = pod.name(),
                        port = pod_port.to_string()
                    )
                })
                .map_err(|error| error.to_string()),
        )
    }
}

impl Render for ForwardForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let label = |text: String| {
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.muted_foreground)
                .child(text)
        };
        let note = |text: String, color: Hsla| div().text_xs().text_color(color).child(text);
        let has_udp = self.source.ports.iter().any(|port| !port.is_tcp());
        let ports = self.source.ports.iter().enumerate().map(|(ix, port)| {
            let value = port.port.to_string();
            let tooltip = if !port.is_tcp() {
                t!("dock-forward-port-not-tcp", protocol = &port.protocol)
            } else if let Some(container) = &port.container {
                t!("dock-container", container = container)
            } else {
                match &port.target {
                    TargetPort::Number(number) => {
                        t!(
                            "dock-forward-service-to-pod-port",
                            port = number.to_string()
                        )
                    }
                    TargetPort::Name(name) => {
                        t!("dock-forward-service-to-named-port", name = name)
                    }
                }
            };
            let remote = self.remote.clone();
            Button::new(("forward-port", ix))
                .outline()
                .xsmall()
                .label(port_label(port))
                .tooltip(tooltip)
                .disabled(!port.is_tcp())
                .on_click(move |_, window, cx| {
                    let value = value.clone();
                    remote.update(cx, |input, cx| input.set_value(value, window, cx));
                })
        });
        let (ports_label, port_label_text) = if self.source.is_service() {
            (
                t!("dock-forward-service-ports"),
                t!("dock-forward-service-port"),
            )
        } else {
            (
                t!("dock-forward-container-ports"),
                t!("dock-forward-pod-port"),
            )
        };
        let target = match &self.target {
            Target::Loading => note(t!("dock-forward-looking"), theme.muted_foreground),
            Target::Found(pod) => {
                let text = if self.source.selector.is_some() {
                    t!("dock-forward-ready-pod", pod = pod.name())
                } else {
                    t!("dock-forward-pod", pod = pod.name())
                };
                note(text, theme.muted_foreground)
            }
            Target::Failed(error) => note(error.clone(), theme.danger),
        };
        let preview = self.route_preview(cx).map(|preview| match preview {
            Ok(text) => note(text, theme.muted_foreground),
            Err(error) => note(error, theme.danger),
        });
        // The local port is checked as it is typed; a taken one is reported
        // by the start (`self.error`).
        let local_hint = match parse_port(&self.local.read(cx).value(), true) {
            Err(error) => note(error, theme.danger),
            Ok(0) => note(t!("dock-forward-local-random"), theme.muted_foreground),
            Ok(_) => note(t!("dock-forward-local-remembered"), theme.muted_foreground),
        };

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_sm().text_color(theme.foreground).child(format!(
                        "{}/{}",
                        self.source.namespace,
                        self.source.label()
                    )))
                    .children(self.source.selector.clone().map(|selector| {
                        note(
                            t!("dock-forward-pods", selector = selector),
                            theme.muted_foreground,
                        )
                    }))
                    .child(target),
            )
            .when(!self.source.ports.is_empty(), |form| {
                form.child(label(ports_label))
                    .child(div().flex().flex_wrap().gap_1().children(ports))
                    .when(has_udp, |form| {
                        form.child(note(
                            t!("dock-forward-udp-disabled"),
                            theme.muted_foreground,
                        ))
                    })
            })
            .child(label(port_label_text))
            .child(Input::new(&self.remote).small())
            .children(preview)
            .child(label(t!("dock-forward-local-port")))
            .child(Input::new(&self.local).small())
            .child(local_hint)
            .children(
                self.error
                    .clone()
                    .map(|error| div().text_xs().text_color(theme.danger).child(error)),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .pt_2()
                    .child(
                        Button::new("forward-cancel")
                            .ghost()
                            .small()
                            .label(t!("dock-cancel"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("forward-start")
                            .primary()
                            .small()
                            .label(if self.starting {
                                t!("dock-forward-starting")
                            } else {
                                t!("dock-forward-start")
                            })
                            .disabled(self.starting)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use kubach_core::streams::{ForwardPort, TargetPort};

    use super::{menu_label, parse_port, port_label};

    #[test]
    fn parses_ports() {
        assert_eq!(parse_port(" 8080 ", false), Ok(8080));
        assert_eq!(parse_port("", true), Ok(0));
        assert!(parse_port("", false).is_err());
        assert!(parse_port("0", true).is_err());
        assert!(parse_port("70000", true).is_err());
        assert!(parse_port("http", true).is_err());
    }

    #[test]
    fn labels_ports() {
        let port = |port, target, name: Option<&str>, protocol: &str| ForwardPort {
            port,
            name: name.map(str::to_owned),
            protocol: protocol.into(),
            target,
            container: None,
        };
        assert_eq!(
            port_label(&port(
                80,
                TargetPort::Name("http".into()),
                Some("web"),
                "TCP"
            )),
            "80 → http (web)"
        );
        assert_eq!(
            port_label(&port(8080, TargetPort::Number(8080), Some("http"), "TCP")),
            "8080 (http)"
        );
        assert_eq!(
            port_label(&port(53, TargetPort::Number(5353), None, "UDP")),
            "53 → 5353 UDP"
        );
    }

    #[test]
    fn labels_menu_items() {
        let service = |port, target, name: Option<&str>, protocol: &str| ForwardPort {
            port,
            name: name.map(str::to_owned),
            protocol: protocol.into(),
            target,
            container: None,
        };
        assert_eq!(
            menu_label(&service(80, TargetPort::Number(8080), Some("http"), "TCP")),
            "80 → 8080 · http · TCP"
        );
        assert_eq!(
            menu_label(&service(80, TargetPort::Name("web".into()), None, "TCP")),
            "80 → web · TCP"
        );
        assert_eq!(
            menu_label(&service(53, TargetPort::Number(53), Some("dns"), "UDP")),
            "53 → 53 · dns · UDP (port-forward is TCP only)"
        );
        let container = ForwardPort {
            port: 9090,
            name: Some("metrics".into()),
            protocol: "TCP".into(),
            target: TargetPort::Number(9090),
            container: Some("app".into()),
        };
        assert_eq!(menu_label(&container), "9090 · metrics · TCP");
    }
}
