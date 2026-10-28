//! "Port forwards" screen (under Network in the sidebar): forwards grouped
//! by context, one card per context sized to its rows (#479). A row leads
//! with the local address and where it goes (`ns/svc/name:port`, cut with
//! a tooltip), with the state, connections and pod on a quieter line under
//! it and the actions Open, Copy, Pause and Stop on the right (#465, #476).
//! Forwards of the last run not running yet share the list with Retry and
//! Remove (#474). The target and the pod open their details in the
//! resource browser.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::state::SavedForward;
use kubach_core::streams::{ForwardInfo, ForwardState, short_kind};
use kubach_i18n::t;
use kubach_ui::{EmptyState, PageHeader, SegmentTab, SegmentTabs, StatusDot, Tone, TruncatedText};

use super::forwards::{Forwards, RestoreStatus};
use super::target::http_scheme;
use crate::screens::Reveal;

/// Open the details of an object a forward goes to, in `context` (the
/// active one when `None`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevealForward {
    pub context: Option<String>,
    pub reveal: Reveal,
}

/// The API group and kind of a forward source kind, short (`svc`, as in
/// [`ForwardInfo::source`]) or full (`Service`).
fn source_kind(kind: &str) -> Option<(&'static str, &'static str)> {
    Some(match kind {
        "svc" | "Service" => ("", "Service"),
        "pod" | "Pod" => ("", "Pod"),
        "deploy" | "Deployment" => ("apps", "Deployment"),
        "sts" | "StatefulSet" => ("apps", "StatefulSet"),
        "ds" | "DaemonSet" => ("apps", "DaemonSet"),
        "rs" | "ReplicaSet" => ("apps", "ReplicaSet"),
        _ => return None,
    })
}

/// The object of `kind` (short or full) and `name` in `namespace`.
fn reveal_of(namespace: &str, kind: &str, name: &str) -> Option<Reveal> {
    let (group, kind) = source_kind(kind)?;
    Some(Reveal {
        group: group.to_owned(),
        kind: kind.to_owned(),
        namespace: Some(namespace.to_owned()),
        name: name.to_owned(),
    })
}

/// The object of a source label such as `svc/web`.
fn reveal_of_label(namespace: &str, label: &str) -> Option<Reveal> {
    let (kind, name) = label.split_once('/')?;
    reveal_of(namespace, kind, name)
}

/// The order of the rows inside a context.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SortBy {
    #[default]
    Port,
    /// Forwards in trouble first.
    Status,
    Target,
}

impl SortBy {
    const ALL: [SortBy; 3] = [SortBy::Port, SortBy::Status, SortBy::Target];

    fn label(self) -> String {
        match self {
            SortBy::Port => t!("dock-forwards-sort-port"),
            SortBy::Status => t!("dock-forwards-sort-status"),
            SortBy::Target => t!("dock-forwards-sort-target"),
        }
    }
}

/// What a row is grouped and sorted by.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RowKey {
    context: Option<String>,
    port: u16,
    /// Lower is worse: failed, then changing, then paused, then working.
    rank: u8,
    target: String,
}

fn sort_keys<T>(rows: &mut [(RowKey, T)], by: SortBy) {
    rows.sort_by(|(a, _), (b, _)| {
        a.context.cmp(&b.context).then_with(|| match by {
            SortBy::Port => a.port.cmp(&b.port),
            SortBy::Status => a.rank.cmp(&b.rank).then(a.port.cmp(&b.port)),
            SortBy::Target => a.target.cmp(&b.target).then(a.port.cmp(&b.port)),
        })
    });
}

/// A forward's state as shown: label, tone, what it means (with the last
/// error) for the tooltip, and its rank for sorting.
struct Status {
    label: String,
    tone: Tone,
    explanation: String,
    rank: u8,
}

/// What links and buttons of a row act on.
#[derive(Clone)]
struct Handles {
    screen: WeakEntity<ForwardsScreen>,
    forwards: WeakEntity<Forwards>,
}

impl Handles {
    /// Monospace text cut to fit (full text in a tooltip), opening the
    /// details of `reveal` on click (plain text without one).
    fn link(
        &self,
        id: String,
        label: String,
        context: Option<String>,
        reveal: Option<Reveal>,
        cx: &App,
    ) -> AnyElement {
        let theme = cx.theme();
        let text = div()
            .min_w_0()
            .overflow_hidden()
            .font_family(theme.mono_font_family.clone())
            .child(TruncatedText::new(
                SharedString::from(format!("{id}-text")),
                label,
            ));
        let Some(reveal) = reveal else {
            return text.into_any_element();
        };
        let (hover, screen) = (theme.foreground, self.screen.clone());
        text.id(SharedString::from(id))
            .cursor_pointer()
            .hover(move |text| text.text_color(hover).underline())
            .on_click(move |_, _, cx| {
                let event = RevealForward {
                    context: context.clone(),
                    reveal: reveal.clone(),
                };
                screen.update(cx, |_, cx| cx.emit(event)).ok();
            })
            .into_any_element()
    }
}

/// The state in the tone's color, with what it means in the tooltip of an
/// info icon after it.
fn status_label(id: String, status: &Status, cx: &App) -> AnyElement {
    let explanation = SharedString::from(status.explanation.clone());
    div()
        .id(SharedString::from(id))
        .flex_none()
        .flex()
        .items_center()
        .gap_1()
        .font_weight(FontWeight::MEDIUM)
        .text_color(status.tone.color(cx))
        .child(status.label.clone())
        .child(
            Icon::new(IconName::Info)
                .xsmall()
                .text_color(cx.theme().muted_foreground),
        )
        .tooltip(move |window, cx| Tooltip::new(explanation.clone()).build(window, cx))
        .into_any_element()
}

/// An icon button of a row's actions.
fn action(id: String, icon: IconName, tooltip: String) -> Button {
    Button::new(SharedString::from(id))
        .ghost()
        .xsmall()
        .icon(icon)
        .tooltip(tooltip)
}

/// What a row shows; [`ForwardRow::render`] lays it out.
struct ForwardRow {
    id: String,
    status: Status,
    local: String,
    /// Paused or not running: the address is drawn muted.
    idle: bool,
    target: AnyElement,
    /// The quieter line under the address, after the state.
    details: Vec<AnyElement>,
    actions: Vec<Button>,
}

impl ForwardRow {
    fn render(self, first: bool, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let (muted, hover) = (theme.muted_foreground, theme.list_hover);
        let mut details = vec![status_label(format!("{}-state", self.id), &self.status, cx)];
        for detail in self.details {
            details.push(div().flex_none().child("·").into_any_element());
            details.push(detail);
        }
        div()
            .id(SharedString::from(self.id))
            .flex()
            .items_center()
            .gap_4()
            .px_4()
            .py_2p5()
            .when(!first, |row| row.border_t_1().border_color(theme.border))
            .hover(move |row| row.bg(hover))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_w_0()
                            .text_sm()
                            .child(StatusDot::new(self.status.tone))
                            .child(
                                div()
                                    .flex_none()
                                    .font_family(theme.mono_font_family.clone())
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(if self.idle { muted } else { theme.foreground })
                                    .child(self.local),
                            )
                            .child(
                                Icon::new(IconName::ArrowRight)
                                    .xsmall()
                                    .flex_none()
                                    .text_color(muted),
                            )
                            .child(div().min_w_0().text_color(muted).child(self.target)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .min_w_0()
                            .pl_4()
                            .text_xs()
                            .text_color(muted)
                            .children(details),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .children(self.actions),
            )
            .into_any_element()
    }
}

/// The state of a running (or paused) forward.
fn forward_status(info: &ForwardInfo) -> Status {
    let (label, tone, explanation, rank) = match info.state {
        _ if info.paused => (
            t!("dock-forwards-paused"),
            Tone::Neutral,
            t!("dock-forwards-paused-tooltip"),
            2,
        ),
        ForwardState::Ready => (
            t!("dock-forwards-ready"),
            Tone::Success,
            t!("dock-forwards-ready-tooltip"),
            3,
        ),
        ForwardState::Failing => (
            t!("dock-forwards-failing"),
            Tone::Danger,
            t!("dock-forwards-failing-tooltip"),
            0,
        ),
        ForwardState::Reconnecting => (
            t!("dock-forwards-reconnecting"),
            Tone::Warning,
            t!("dock-forwards-reconnecting-tooltip"),
            1,
        ),
    };
    let mut explanation = vec![explanation];
    if info.error_count > 0 {
        explanation.push(t!("dock-forwards-errors", count = info.error_count));
    }
    // The last error matters while the forward is not working.
    if let Some(error) = info
        .last_error
        .as_ref()
        .filter(|_| info.state != ForwardState::Ready && !info.paused)
    {
        explanation.push(t!("dock-forwards-last-error", error = error));
    }
    Status {
        label,
        tone,
        explanation: explanation.join("\n"),
        rank,
    }
}

/// The row of a running (or paused) forward.
fn forward_row(
    info: &ForwardInfo,
    context: Option<String>,
    port_name: Option<&str>,
    handles: &Handles,
    cx: &App,
) -> (RowKey, ForwardRow) {
    let id = info.id;
    let status = forward_status(info);
    // Ports that look like HTTPS open as such, everything else as HTTP.
    let scheme = http_scheme(info.source_port, port_name)
        .or_else(|| http_scheme(info.remote_port, port_name))
        .unwrap_or("http");
    let url = format!("{scheme}://{}", info.local_addr);
    let address = info.local_addr.to_string();
    let paused = info.paused;
    let (pause, stop) = (handles.forwards.clone(), handles.forwards.clone());
    let actions = vec![
        action(
            format!("forward-open-{id}"),
            IconName::ExternalLink,
            t!("dock-forwards-open", url = &url),
        )
        .on_click(move |_, _, cx| cx.open_url(&url)),
        action(
            format!("forward-copy-{id}"),
            IconName::Copy,
            t!("dock-forwards-copy", address = &address),
        )
        .on_click({
            let address = address.clone();
            move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(address.clone()))
        }),
        if paused {
            action(
                format!("forward-pause-{id}"),
                IconName::Play,
                t!("dock-forwards-resume", address = &address),
            )
        } else {
            action(
                format!("forward-pause-{id}"),
                IconName::Pause,
                t!("dock-forwards-pause"),
            )
        }
        .on_click(move |_, _, cx| {
            pause
                .update(cx, |forwards, cx| forwards.set_paused(id, !paused, cx))
                .ok();
        }),
        action(
            format!("forward-stop-{id}"),
            IconName::Close,
            t!("dock-forwards-stop-tooltip"),
        )
        .on_click(move |_, _, cx| {
            stop.update(cx, |forwards, cx| forwards.stop(id, cx)).ok();
        }),
    ];
    let target = format!("{}/{}:{}", info.namespace, info.source, info.source_port);
    let connections = SharedString::from(t!("dock-forwards-connections-tooltip"));
    let mut details = Vec::new();
    // An AI agent's forward (#17): whose, so the user can stop it.
    if let Some(agent) = &info.agent {
        let tooltip = SharedString::from(t!(
            "dock-forwards-agent-tooltip",
            client = agent.client.as_str(),
            session = agent.session.as_str()
        ));
        details.push(
            div()
                .id(SharedString::from(format!("forward-agent-{id}")))
                .flex_none()
                .text_color(cx.theme().warning)
                .child(t!("dock-forwards-agent", client = agent.client.as_str()))
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .into_any_element(),
        );
    }
    details.push(
        div()
            .id(SharedString::from(format!("forward-connections-{id}")))
            .flex_none()
            .child(t!(
                "dock-forwards-connections",
                active = info.active_connections,
                total = info.total_connections
            ))
            .tooltip(move |window, cx| Tooltip::new(connections.clone()).build(window, cx))
            .into_any_element(),
    );
    // A pod source is its own pod.
    if !info.source.starts_with("pod/") {
        details.push(
            div()
                .flex()
                .items_center()
                .gap_1()
                .min_w_0()
                .child(div().flex_none().child(t!("dock-forwards-pod")))
                .child(handles.link(
                    format!("forward-pod-{id}"),
                    format!("{}:{}", info.pod, info.remote_port),
                    context.clone(),
                    reveal_of(&info.namespace, "Pod", &info.pod),
                    cx,
                ))
                .into_any_element(),
        );
    }
    let key = RowKey {
        context: context.clone(),
        port: info.local_addr.port(),
        rank: status.rank,
        target: target.clone(),
    };
    let row = ForwardRow {
        id: format!("forward-{id}"),
        status,
        local: info.local_addr.to_string(),
        idle: paused,
        target: handles.link(
            format!("forward-target-{id}"),
            target,
            context.clone(),
            reveal_of_label(&info.namespace, &info.source),
            cx,
        ),
        details,
        actions,
    };
    (key, row)
}

/// The state of a forward of the last run that is not running yet, and
/// the line under its address: what it waits for, or why it failed.
fn restore_status(saved: &SavedForward, status: &RestoreStatus) -> (Status, String) {
    let (label, tone, tooltip, detail, rank) = match status {
        RestoreStatus::Waiting { known } => (
            t!("dock-forwards-restore-waiting"),
            Tone::Warning,
            t!("dock-forwards-restore-waiting-tooltip"),
            if *known {
                t!(
                    "dock-forwards-restore-waiting-context",
                    context = &saved.context
                )
            } else {
                t!(
                    "dock-forwards-restore-unknown-context",
                    context = &saved.context
                )
            },
            1,
        ),
        RestoreStatus::Starting => (
            t!("dock-forwards-restore-starting"),
            Tone::Warning,
            t!("dock-forwards-restore-starting-detail"),
            String::new(),
            1,
        ),
        RestoreStatus::Failed(error) => (
            t!("dock-forwards-restore-failed"),
            Tone::Danger,
            t!("dock-forwards-restore-failed-tooltip"),
            error.clone(),
            0,
        ),
    };
    let explanation = if detail.is_empty() {
        tooltip
    } else {
        format!("{tooltip}\n{detail}")
    };
    let status = Status {
        label,
        tone,
        explanation,
        rank,
    };
    (status, detail)
}

/// The row of a forward of the last run not running yet (#474): waiting
/// for its context, starting, or failed with Retry; Remove forgets it.
fn restore_row(
    id: u64,
    saved: &SavedForward,
    status: &RestoreStatus,
    handles: &Handles,
    cx: &App,
) -> (RowKey, ForwardRow) {
    let (shown, detail) = restore_status(saved, status);
    let (retry, remove) = (handles.forwards.clone(), handles.forwards.clone());
    let mut actions = Vec::new();
    if matches!(status, RestoreStatus::Failed(_)) {
        actions.push(
            action(
                format!("forward-restore-retry-{id}"),
                IconName::RefreshCw,
                t!("dock-forwards-retry-tooltip"),
            )
            .on_click(move |_, _, cx| {
                retry.update(cx, |forwards, cx| forwards.retry(id, cx)).ok();
            }),
        );
    }
    actions.push(
        action(
            format!("forward-restore-remove-{id}"),
            IconName::Close,
            t!("dock-forwards-remove-tooltip"),
        )
        .on_click(move |_, _, cx| {
            remove
                .update(cx, |forwards, cx| forwards.remove(id, cx))
                .ok();
        }),
    );
    let details = (!detail.is_empty())
        .then(|| {
            let color = if matches!(status, RestoreStatus::Failed(_)) {
                cx.theme().danger
            } else {
                cx.theme().muted_foreground
            };
            div()
                .min_w_0()
                .text_color(color)
                .child(TruncatedText::new(
                    SharedString::from(format!("forward-restore-detail-{id}")),
                    detail,
                ))
                .into_any_element()
        })
        .into_iter()
        .collect();
    let target = format!(
        "{}/{}/{}:{}",
        saved.namespace,
        short_kind(&saved.kind),
        saved.name,
        saved.port
    );
    let key = RowKey {
        context: Some(saved.context.clone()),
        port: saved.local_port,
        rank: shown.rank,
        target: target.clone(),
    };
    let row = ForwardRow {
        id: format!("forward-restore-{id}"),
        status: shown,
        local: format!("127.0.0.1:{}", saved.local_port),
        idle: true,
        target: handles.link(
            format!("forward-restore-target-{id}"),
            target,
            Some(saved.context.clone()),
            reveal_of(&saved.namespace, &saved.kind, &saved.name),
            cx,
        ),
        details,
        actions,
    };
    (key, row)
}

/// The rows of one context: its name and count over a card of rows.
fn context_group(context: Option<String>, rows: Vec<ForwardRow>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let count = rows.len();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_1()
                .min_w_0()
                .text_sm()
                .child(
                    div()
                        .min_w_0()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child(TruncatedText::new(
                            SharedString::from(format!(
                                "forwards-context-{}",
                                context.as_deref().unwrap_or_default()
                            )),
                            context.unwrap_or_else(|| t!("dock-forwards-context-current")),
                        )),
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(count.to_string()),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.group_box)
                .overflow_hidden()
                .children(
                    rows.into_iter()
                        .enumerate()
                        .map(|(ix, row)| row.render(ix == 0, cx)),
                ),
        )
        .into_any_element()
}

pub struct ForwardsScreen {
    forwards: Entity<Forwards>,
    sort: SortBy,
    _subscription: Subscription,
}

impl ForwardsScreen {
    pub fn new(forwards: Entity<Forwards>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&forwards, |_, _, cx| cx.notify());
        Self {
            forwards,
            sort: SortBy::default(),
            _subscription: subscription,
        }
    }

    /// The rows in order, grouped by context.
    fn groups(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let handles = Handles {
            screen: cx.entity().downgrade(),
            forwards: self.forwards.downgrade(),
        };
        let forwards = self.forwards.read(cx);
        let mut rows: Vec<(RowKey, ForwardRow)> = forwards
            .restoring()
            .iter()
            .map(|restore| restore_row(restore.id, &restore.saved, &restore.status, &handles, cx))
            .chain(forwards.list().iter().map(|info| {
                forward_row(
                    info,
                    forwards.context(info.id).map(str::to_owned),
                    forwards.port_name(info.id),
                    &handles,
                    cx,
                )
            }))
            .collect();
        sort_keys(&mut rows, self.sort);
        let mut groups: Vec<(Option<String>, Vec<ForwardRow>)> = Vec::new();
        for (key, row) in rows {
            match groups.last_mut() {
                Some((context, group)) if *context == key.context => group.push(row),
                _ => groups.push((key.context, vec![row])),
            }
        }
        groups
            .into_iter()
            .map(|(context, rows)| context_group(context, rows, cx))
            .collect()
    }

    fn sort_control(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let screen = cx.entity().downgrade();
        let active = SortBy::ALL
            .iter()
            .position(|by| *by == self.sort)
            .unwrap_or_default();
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("dock-forwards-sort")),
            )
            .child(
                SegmentTabs::new(
                    "forwards-sort",
                    SortBy::ALL
                        .iter()
                        .map(|by| SegmentTab::new(by.label()))
                        .collect(),
                    active,
                )
                .on_select(move |ix, _, cx| {
                    let by = SortBy::ALL.get(*ix).copied().unwrap_or_default();
                    screen
                        .update(cx, |screen, cx| {
                            screen.sort = by;
                            cx.notify();
                        })
                        .ok();
                }),
            )
    }
}

impl Render for ForwardsScreen {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let forwards = self.forwards.read(cx);
        let count = forwards.list().len() + forwards.restoring().len();
        let header = PageHeader::new(t!("nav-port-forwards"))
            .description(t!("dock-forwards-description"))
            .when(count > 1, |header| header.action(self.sort_control(cx)));
        let root = div()
            .id("forwards-screen")
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col();
        let page = || div().w_full().max_w(px(1120.)).px_8();
        if count == 0 {
            return root.child(
                page().pt_8().flex().flex_col().gap_6().child(header).child(
                    EmptyState::new(Lucide::ArrowRightLeft, t!("dock-forwards-empty"))
                        .description(t!("dock-forwards-empty-description")),
                ),
            );
        }
        root.child(page().flex_none().pt_8().pb_6().child(header))
            .child(
                div()
                    .id("forwards-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        page()
                            .pb_8()
                            .flex()
                            .flex_col()
                            .gap_6()
                            .children(self.groups(cx)),
                    ),
            )
    }
}

impl EventEmitter<RevealForward> for ForwardsScreen {}

#[cfg(test)]
mod tests {
    use super::{RowKey, SortBy, reveal_of, reveal_of_label, sort_keys};

    #[test]
    fn sources_open_their_objects() {
        let service = reveal_of_label("monitoring", "svc/prometheus").unwrap();
        assert_eq!(
            (service.group.as_str(), service.kind.as_str()),
            ("", "Service")
        );
        assert_eq!(service.namespace.as_deref(), Some("monitoring"));
        assert_eq!(service.name, "prometheus");
        let deploy = reveal_of_label("web", "deploy/api").unwrap();
        assert_eq!(
            (deploy.group.as_str(), deploy.kind.as_str()),
            ("apps", "Deployment")
        );
        let sts = reveal_of("db", "StatefulSet", "pg").unwrap();
        assert_eq!(
            (sts.group.as_str(), sts.kind.as_str()),
            ("apps", "StatefulSet")
        );
        assert_eq!(reveal_of("ns", "Pod", "web-1").unwrap().kind, "Pod");
        assert!(reveal_of_label("ns", "cronjob/x").is_none());
        assert!(reveal_of_label("ns", "nothing").is_none());
    }

    #[test]
    fn rows_group_by_context_then_sort() {
        let key = |context: &str, port, rank, target: &str| RowKey {
            context: Some(context.to_owned()),
            port,
            rank,
            target: target.to_owned(),
        };
        let rows = || {
            vec![
                (key("prod", 9000, 1, "a/svc/web:80"), 1),
                (key("dev", 8080, 3, "b/svc/api:80"), 2),
                (key("prod", 3000, 0, "c/svc/db:5432"), 3),
                (key("prod", 5000, 3, "a/svc/api:80"), 4),
            ]
        };
        let order = |by| {
            let mut rows = rows();
            sort_keys(&mut rows, by);
            rows.into_iter().map(|(_, n)| n).collect::<Vec<_>>()
        };
        assert_eq!(order(SortBy::Port), [2, 3, 4, 1]);
        assert_eq!(order(SortBy::Status), [2, 3, 1, 4]);
        assert_eq!(order(SortBy::Target), [2, 4, 1, 3]);
    }
}
