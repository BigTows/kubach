//! Buttons that open logs, shells and port-forwards from the details panel.

use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::IconName;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::Button;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::*;
use kubach_core::plugin::ResourcePlugin;
use kubach_core::streams::ForwardSource;
use kubach_i18n::t;
use kubach_ui::SplitButton;
use kube::api::DynamicObject;

use super::containers::{OnPick, fill_menu};
use super::forward_dialog::menu_label;
use super::target::StreamRequest;

/// Logs / Shell / Debug / Port forward for `object`, or `None` when it has
/// no pods to stream from. For a pod with several containers, Logs / Shell /
/// Debug are split buttons: the main part opens the default (all containers
/// for logs), the chevron picks a container.
pub fn stream_actions(object: &DynamicObject, plugin: &dyn ResourcePlugin) -> Option<AnyElement> {
    let requests = StreamRequest::for_object(object, plugin);
    if requests.is_empty() {
        return None;
    }
    let buttons = requests.into_iter().enumerate().map(|(ix, request)| {
        let icon: Lucide = match &request {
            StreamRequest::Logs { .. } => IconName::FileText.into(),
            StreamRequest::Shell { .. } => IconName::SquareTerminal.into(),
            StreamRequest::Debug { .. } => IconName::Inspector.into(),
            StreamRequest::PortForward(_) => Lucide::ArrowRightLeft,
        };
        let default = request
            .pod()
            .and_then(|pod| pod.default_container())
            .unwrap_or_default()
            .to_owned();
        // Icon-only buttons: the tooltip names the action.
        let tooltip = match &request {
            StreamRequest::Logs { .. } if request.container_menu().is_some() => {
                t!("dock-entry-logs-all")
            }
            StreamRequest::Logs { .. } => t!("dock-entry-logs"),
            StreamRequest::Shell { .. } if request.container_menu().is_some() => {
                t!("dock-entry-shell-default", container = default)
            }
            StreamRequest::Shell { .. } => t!("dock-entry-shell"),
            StreamRequest::Debug { .. } if request.container_menu().is_some() => {
                t!("dock-entry-debug-default", container = default)
            }
            StreamRequest::Debug { .. } => t!("dock-entry-debug"),
            StreamRequest::PortForward(source) if source.selector.is_some() => {
                t!("dock-entry-forward-selector")
            }
            StreamRequest::PortForward(_) => t!("dock-entry-forward"),
        };
        if let StreamRequest::PortForward(source) = &request
            && !source.ports.is_empty()
        {
            return port_forward_button(ix, source.clone(), icon, tooltip);
        }
        if let Some(menu) = request.container_menu() {
            let picked = request.clone();
            let on_pick: OnPick = Rc::new(move |container, window, cx| {
                super::open_request(picked.with_container(container), window, cx)
            });
            return SplitButton::new(("stream-action", ix), "")
                .icon(icon)
                .tooltip(tooltip)
                .menu_tooltip(t!("dock-choose-container"))
                .on_click(move |_, window, cx| super::open_request(request.clone(), window, cx))
                .menu(move |popup, _, _| fill_menu(popup, &menu, None, on_pick.clone()))
                .into_any_element();
        }
        Button::new(("stream-action", ix))
            .outline()
            .small()
            .icon(icon)
            .tooltip(tooltip)
            .on_click(move |_, window, cx| super::open_request(request.clone(), window, cx))
            .into_any_element()
    });
    Some(
        div()
            .flex()
            .flex_wrap()
            .gap_2()
            .children(buttons)
            .into_any_element(),
    )
}

/// Port forward with known ports: the main part forwards the first TCP port
/// at once, the chevron lists every port (non-TCP ones disabled) and the
/// dialog for picking the local port.
fn port_forward_button(
    ix: usize,
    source: ForwardSource,
    icon: Lucide,
    tooltip: String,
) -> AnyElement {
    let first = source
        .ports
        .iter()
        .find(|port| port.is_tcp())
        .map(|port| port.port);
    let tooltip = match first {
        Some(port) => t!("dock-entry-forward-port", port = port.to_string()),
        None => tooltip,
    };
    let main = source.clone();
    let menu = source.clone();
    SplitButton::new(("stream-action", ix), "")
        .icon(icon)
        .tooltip(tooltip)
        .menu_tooltip(t!("dock-choose-port"))
        .on_click(move |_, window, cx| match first {
            Some(port) => super::start_forward(main.clone(), port, window, cx),
            None => super::open_request(StreamRequest::PortForward(main.clone()), window, cx),
        })
        .menu(move |popup, _, _| fill_port_menu(popup, &menu))
        .into_any_element()
}

/// Ports of `source`, grouped by container when there are several, then
/// "Choose local port…" for the dialog.
fn fill_port_menu(mut popup: PopupMenu, source: &ForwardSource) -> PopupMenu {
    let mut containers: Vec<&str> = Vec::new();
    for port in &source.ports {
        if let Some(container) = port.container.as_deref()
            && !containers.contains(&container)
        {
            containers.push(container);
        }
    }
    let grouped = containers.len() > 1;
    let mut group: Option<&str> = None;
    for port in &source.ports {
        if grouped && port.container.as_deref() != group {
            group = port.container.as_deref();
            if let Some(container) = group {
                popup = popup.label(t!("dock-container", container = container));
            }
        }
        let (forward, number) = (source.clone(), port.port);
        popup = popup.item(
            PopupMenuItem::new(menu_label(port))
                .disabled(!port.is_tcp())
                .on_click(move |_, window, cx| {
                    super::start_forward(forward.clone(), number, window, cx)
                }),
        );
    }
    let dialog = source.clone();
    popup
        .separator()
        .item(
            PopupMenuItem::new(t!("dock-choose-local-port")).on_click(move |_, window, cx| {
                super::open_request(StreamRequest::PortForward(dialog.clone()), window, cx)
            }),
        )
}
