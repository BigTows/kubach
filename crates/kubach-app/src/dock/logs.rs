//! A dock tab streaming the logs of one pod or of every pod of a workload.

use std::rc::Rc;
use std::sync::Arc;

use futures::StreamExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::ansi::{self, AnsiColor, AnsiStyle};
use kubach_core::runtime;
use kubach_core::streams::{
    FetchedLines, HistoryRequest, HistoryState, INITIAL_TAIL_LINES, LogEvent, LogHistory, LogLine,
    LogOptions, LogSource, MAX_HISTORY_BYTES, MAX_HISTORY_LINES, StreamError, fetch_history,
    sources_for_selector, stream_logs,
};
use kubach_i18n::t;
use kubach_ui::{
    DEFAULT_LOG_CAPACITY, LogEntry, LogTopNotice, LogView, LogViewEvent, SpanColor, SpanStyle,
    StyledSpan,
};

use super::containers::{OnPick, fill_menu};
use super::target::{ContainerMenu, LogTarget};
use crate::cluster::Cluster;

/// Events applied to the view per UI update.
const BATCH_EVENTS: usize = 64;

/// Where the stream is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogsState {
    Starting,
    Streaming,
    /// Every source ended (previous logs, deleted pods, stopped containers).
    Finished,
    Failed(String),
}

/// Health of a running stream, summed over its sources.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogsStatus {
    /// Sources waiting to reconnect, with the reason.
    pub reconnecting: Vec<(String, String)>,
    pub dropped: u64,
    pub ended: usize,
    pub errors: Vec<String>,
}

impl LogsStatus {
    /// Applies an event other than lines.
    pub fn apply(&mut self, event: &LogEvent) {
        match event {
            LogEvent::Lines {
                source, dropped, ..
            } => {
                self.dropped += dropped;
                let label = source.label();
                self.reconnecting.retain(|(waiting, _)| *waiting != label);
            }
            LogEvent::Reconnecting { source, reason, .. } => {
                let label = source.label();
                self.reconnecting.retain(|(waiting, _)| *waiting != label);
                self.reconnecting.push((label, reason.clone()));
            }
            LogEvent::Ended { source, error } => {
                let label = source.label();
                self.reconnecting.retain(|(waiting, _)| *waiting != label);
                self.ended += 1;
                if let Some(error) = error {
                    self.errors.push(format!("{label}: {error}"));
                }
            }
        }
    }
}

fn span_color(color: AnsiColor) -> SpanColor {
    match color {
        AnsiColor::Named(index) => SpanColor::Named(index),
        AnsiColor::Indexed(index) => SpanColor::Indexed(index),
        AnsiColor::Rgb(r, g, b) => SpanColor::Rgb(r, g, b),
    }
}

fn span_style(style: AnsiStyle) -> SpanStyle {
    SpanStyle {
        foreground: style.foreground.map(span_color),
        background: style.background.map(span_color),
        bold: style.bold,
        dim: style.dim,
        italic: style.italic,
        underline: style.underline,
        strikethrough: style.strikethrough,
    }
}

/// Converts a line for the view; `tag` names its source when several are
/// merged. ANSI escape sequences become colors, so the text searched and
/// copied has none.
pub fn log_entry(line: LogLine, tag: Option<(String, usize)>) -> LogEntry {
    let mut entry = if ansi::has_controls(&line.text) {
        let parsed = ansi::parse(&line.text);
        let spans = parsed
            .spans
            .into_iter()
            .map(|span| StyledSpan {
                range: span.range,
                style: span_style(span.style),
            })
            .collect();
        LogEntry::new(parsed.text).spans(spans)
    } else {
        LogEntry::new(line.text)
    };
    if let Some(timestamp) = line.timestamp {
        entry = entry.timestamp(timestamp.strftime("%Y-%m-%dT%H:%M:%S%.3fZ").to_string());
    }
    if let Some((label, color)) = tag {
        entry = entry.tag(label, color);
    }
    entry
}

/// `50000` as `50,000`.
fn group_thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (ix, digit) in digits.chars().enumerate() {
        if ix > 0 && (digits.len() - ix).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

pub struct LogsTab {
    cluster: Entity<Cluster>,
    /// Context the tab was opened in: restarts and earlier lines come from
    /// it, whatever is active now (#344).
    context: Option<String>,
    target: LogTarget,
    /// `None` streams every container.
    container: Option<String>,
    previous: bool,
    view: Entity<LogView>,
    search: Entity<InputState>,
    sources: Vec<Arc<LogSource>>,
    state: LogsState,
    status: LogsStatus,
    /// Earlier lines loaded on scrolling to the top.
    history: Option<LogHistory>,
    history_error: Option<String>,
    _history_load: Option<Task<()>>,
    _stream: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl LogsTab {
    pub fn new(
        cluster: Entity<Cluster>,
        target: LogTarget,
        container: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let view = cx.new(|cx| LogView::new(DEFAULT_LOG_CAPACITY, cx));
        let search = cx.new(|cx| InputState::new(window, cx));
        let mut subscriptions = vec![
            cx.observe(&view, |_, _, cx| cx.notify()),
            cx.subscribe(&view, |this, _, event: &LogViewEvent, cx| match event {
                LogViewEvent::ReachedTop => this.load_earlier(cx),
            }),
            cx.subscribe(
                &search,
                |this, search, event: &InputEvent, cx| match event {
                    InputEvent::Change => {
                        let query = search.read(cx).value();
                        this.view.update(cx, |view, cx| view.set_search(&query, cx));
                    }
                    InputEvent::PressEnter { shift, .. } => {
                        let shift = *shift;
                        this.view.update(cx, |view, cx| {
                            if shift {
                                view.prev_match(cx)
                            } else {
                                view.next_match(cx)
                            }
                        });
                    }
                    _ => {}
                },
            ),
        ];
        subscriptions.extend(crate::i18n::localized_placeholder(
            &search,
            || t!("dock-logs-search"),
            window,
            cx,
        ));
        let container = container.or_else(|| match &target {
            LogTarget::Pod(pod) if pod.containers.len() > 1 => None,
            LogTarget::Pod(pod) => pod.default_container().map(str::to_owned),
            LogTarget::Selector { .. } => None,
        });
        let context = cluster.read(cx).active_name().map(str::to_owned);
        let mut this = Self {
            cluster,
            context,
            target,
            container,
            previous: false,
            view,
            search,
            sources: Vec::new(),
            state: LogsState::Starting,
            status: LogsStatus::default(),
            history: None,
            history_error: None,
            _history_load: None,
            _stream: None,
            _subscriptions: subscriptions,
        };
        this.restart(cx);
        this
    }

    /// Context the tab was opened in; its hotbar color marks the tab (#461).
    pub fn context(&self) -> Option<&str> {
        self.context.as_deref()
    }

    pub fn title(&self) -> String {
        let title = match (&self.target, &self.container) {
            (LogTarget::Pod(pod), Some(container)) if pod.containers.len() > 1 => {
                format!("{}/{container}", pod.name)
            }
            (target, _) => target.title(),
        };
        crate::action_context::title_in_context(&title, self.context.as_deref())
    }

    pub fn state(&self) -> &LogsState {
        &self.state
    }

    pub fn line_count(&self, cx: &App) -> usize {
        self.view.read(cx).len()
    }

    /// (Re)starts the stream with the current container and previous-logs
    /// choice; the old stream is dropped, which closes it.
    pub fn restart(&mut self, cx: &mut Context<Self>) {
        self._stream = None;
        self._history_load = None;
        self.history = None;
        self.history_error = None;
        self.sources.clear();
        self.status = LogsStatus::default();
        self.state = LogsState::Starting;
        self.view.update(cx, |view, cx| {
            view.clear(cx);
            view.set_top_notice(None, cx);
            view.set_placeholder(t!("dock-connecting"), cx);
        });
        cx.notify();

        let client = match crate::action_context::tab_connection(
            self.cluster.read(cx),
            self.context.as_deref(),
        ) {
            Ok(connection) => connection.client.clone(),
            Err(error) => {
                self.fail(error, cx);
                return;
            }
        };
        let options = LogOptions {
            follow: !self.previous,
            previous: self.previous,
            tail_lines: Some(INITIAL_TAIL_LINES),
            timestamps: true,
            ..LogOptions::default()
        };
        let target = self.target.clone();
        let container = self.container.clone();
        self._stream = Some(cx.spawn(async move |this, cx| {
            let sources = match target {
                // A chosen container may also be an init container.
                LogTarget::Pod(pod) => match &container {
                    Some(name) => vec![LogSource::new(&pod.namespace, &pod.name, name)],
                    None => pod
                        .containers
                        .iter()
                        .map(|name| LogSource::new(&pod.namespace, &pod.name, name))
                        .collect(),
                },
                LogTarget::Selector {
                    namespace,
                    selector,
                    ..
                } => {
                    let client = client.clone();
                    let found = runtime::spawn(async move {
                        sources_for_selector(&client, &namespace, &selector).await
                    })
                    .await;
                    match found {
                        Ok(Ok(sources)) => sources,
                        Ok(Err(error)) => {
                            this.update(cx, |this, cx| this.fail(error.to_string(), cx))
                                .ok();
                            return;
                        }
                        Err(error) => {
                            this.update(cx, |this, cx| this.fail(error.to_string(), cx))
                                .ok();
                            return;
                        }
                    }
                }
            };
            if sources.is_empty() {
                this.update(cx, |this, cx| this.fail(t!("dock-logs-no-containers"), cx))
                    .ok();
                return;
            }
            let stream = stream_logs(&client, sources.clone(), options);
            let started = this.update(cx, |this, cx| {
                this.sources = sources.into_iter().map(Arc::new).collect();
                this.history = Some(LogHistory::new(&this.sources, INITIAL_TAIL_LINES));
                this.state = LogsState::Streaming;
                this.view.update(cx, |view, cx| {
                    view.set_placeholder(t!("dock-logs-waiting"), cx)
                });
                cx.notify();
            });
            if started.is_err() {
                return;
            }
            let mut batches = stream.ready_chunks(BATCH_EVENTS);
            while let Some(batch) = batches.next().await {
                if this.update(cx, |this, cx| this.apply(batch, cx)).is_err() {
                    return;
                }
            }
            this.update(cx, |this, cx| {
                this.state = LogsState::Finished;
                this.view.update(cx, |view, cx| {
                    view.set_placeholder(t!("dock-logs-no-lines"), cx)
                });
                cx.notify();
            })
            .ok();
        }));
    }

    fn fail(&mut self, error: String, cx: &mut Context<Self>) {
        self.view
            .update(cx, |view, cx| view.set_placeholder(error.clone(), cx));
        self.state = LogsState::Failed(error);
        cx.notify();
    }

    /// Source label and color shown before lines of `source` when several
    /// sources are merged.
    fn tag(&self, source: &LogSource) -> Option<(String, usize)> {
        (self.sources.len() > 1).then(|| {
            let color = self
                .sources
                .iter()
                .position(|known| **known == *source)
                .unwrap_or_default();
            (source.label(), color)
        })
    }

    fn apply(&mut self, batch: Vec<LogEvent>, cx: &mut Context<Self>) {
        let mut entries = Vec::new();
        for event in batch {
            self.status.apply(&event);
            if let LogEvent::Lines {
                source,
                lines,
                dropped,
            } = event
            {
                if let Some(history) = &mut self.history {
                    history.observe(&source, &lines, dropped);
                }
                let tag = self.tag(&source);
                entries.extend(lines.into_iter().map(|line| log_entry(line, tag.clone())));
            }
        }
        let evicted = self.view.update(cx, |view, cx| {
            view.push(entries, cx);
            view.has_evicted()
        });
        if evicted && let Some(history) = &mut self.history {
            history.turn_off();
        }
        self.update_top_notice(cx);
        cx.notify();
    }

    /// Fetches lines older than the shown ones (the view reached its top).
    fn load_earlier(&mut self, cx: &mut Context<Self>) {
        if self._history_load.is_some() {
            return;
        }
        let Some(history) = &mut self.history else {
            return;
        };
        let requests = history.plan();
        if requests.is_empty() {
            self.update_top_notice(cx);
            return;
        }
        let Ok(client) =
            crate::action_context::tab_connection(self.cluster.read(cx), self.context.as_deref())
                .map(|connection| connection.client.clone())
        else {
            return;
        };
        let previous = self.previous;
        self._history_load = Some(cx.spawn(async move |this, cx| {
            let fetched =
                runtime::spawn(async move { fetch_history(&client, requests, previous).await })
                    .await;
            this.update(cx, |this, cx| {
                this._history_load = None;
                match fetched {
                    Ok(fetched) => this.show_earlier(fetched, cx),
                    Err(error) => this.history_error = Some(error.to_string()),
                }
                this.update_top_notice(cx);
                cx.notify();
            })
            .ok();
        }));
        self.update_top_notice(cx);
    }

    fn show_earlier(
        &mut self,
        fetched: Vec<(HistoryRequest, Result<FetchedLines, StreamError>)>,
        cx: &mut Context<Self>,
    ) {
        let mut answered = Vec::new();
        let mut error = None;
        for (request, lines) in fetched {
            match lines {
                Ok(lines) => answered.push((request, lines)),
                Err(failure) => error = Some(format!("{}: {failure}", request.source.label())),
            }
        }
        let Some(history) = &mut self.history else {
            return;
        };
        let lines = history.apply(answered);
        let failed = error.is_some();
        self.history_error = error;
        if failed && lines.is_empty() {
            // Not asking again right away; scrolling away and back retries.
            return;
        }
        let entries: Vec<LogEntry> = lines
            .into_iter()
            .map(|line| {
                let tag = self.tag(&line.source);
                log_entry(line, tag)
            })
            .collect();
        self.view.update(cx, |view, cx| view.prepend(entries, cx));
    }

    fn update_top_notice(&mut self, cx: &mut Context<Self>) {
        let notice = match &self.history {
            None => None,
            Some(_) if self._history_load.is_some() => Some(LogTopNotice::Loading(
                t!("dock-logs-loading-earlier").into(),
            )),
            Some(_) if self.history_error.is_some() => Some(LogTopNotice::Warning(
                t!(
                    "dock-logs-earlier-failed",
                    error = self.history_error.as_deref().unwrap_or_default()
                )
                .into(),
            )),
            Some(history) => match history.state() {
                HistoryState::Beginning => {
                    Some(LogTopNotice::Info(t!("dock-logs-beginning").into()))
                }
                HistoryState::Capped => Some(LogTopNotice::Warning(
                    t!(
                        "dock-logs-capped",
                        lines = group_thousands(MAX_HISTORY_LINES),
                        mib = MAX_HISTORY_BYTES / (1024 * 1024)
                    )
                    .into(),
                )),
                HistoryState::More | HistoryState::Off => None,
            },
        };
        self.view
            .update(cx, |view, cx| view.set_top_notice(notice, cx));
    }

    fn set_container(&mut self, container: Option<String>, cx: &mut Context<Self>) {
        if self.container != container {
            self.container = container;
            self.restart(cx);
        }
    }

    fn set_previous(&mut self, previous: bool, cx: &mut Context<Self>) {
        if self.previous != previous {
            self.previous = previous;
            self.restart(cx);
        }
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        let text = self.view.read(cx).text();
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn render_container_picker(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let LogTarget::Pod(pod) = &self.target else {
            return None;
        };
        let label: SharedString = self
            .container
            .clone()
            .unwrap_or_else(|| t!("dock-all-containers"))
            .into();
        let menu = ContainerMenu::for_logs(pod);
        let current = self.container.clone();
        let tab = cx.entity().downgrade();
        let on_pick: OnPick = Rc::new(move |value, _, cx| {
            tab.update(cx, |tab, cx| tab.set_container(value, cx)).ok();
        });
        Some(
            Button::new("log-container")
                .outline()
                .xsmall()
                .label(label)
                .disabled(menu.len() < 2)
                .dropdown_menu(move |popup: PopupMenu, _, _| {
                    fill_menu(popup, &menu, Some(current.as_deref()), on_pick.clone())
                }),
        )
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let container_picker = self.render_container_picker(cx);
        let theme = cx.theme();
        let view = self.view.read(cx);
        let (current, total) = view.match_position();
        let has_query = !view.search_query().is_empty();
        let follow = view.follow_state();
        let wrap = view.wrap();
        let timestamps = view.show_timestamps();

        let toggle = |id: &'static str, label: String, on: bool| {
            Button::new(id).ghost().xsmall().label(label).selected(on)
        };

        let mut indicators: Vec<AnyElement> = Vec::new();
        if !self.status.reconnecting.is_empty() {
            let reasons = self
                .status
                .reconnecting
                .iter()
                .map(|(source, reason)| format!("{source}: {reason}"))
                .collect::<Vec<_>>()
                .join("\n");
            indicators.push(
                // Not a button: nothing to click, and the loader has to spin.
                div()
                    .id("log-reconnecting")
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_1()
                    .text_xs()
                    .text_color(theme.warning)
                    .child(
                        kubach_ui::spinner()
                            .icon(Icon::new(IconName::LoaderCircle))
                            .with_size(px(12.))
                            .color(theme.warning),
                    )
                    .child(t!("dock-logs-reconnecting"))
                    .tooltip(move |window, cx| Tooltip::new(reasons.clone()).build(window, cx))
                    .into_any_element(),
            );
        }
        if self.status.dropped > 0 {
            indicators.push(
                div()
                    .text_xs()
                    .text_color(theme.warning)
                    .child(t!("dock-logs-dropped", count = self.status.dropped))
                    .into_any_element(),
            );
        }
        match &self.state {
            LogsState::Finished => indicators.push(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t!("dock-logs-ended-indicator"))
                    .into_any_element(),
            ),
            LogsState::Failed(error) => indicators.push(
                div()
                    .max_w(px(320.))
                    .truncate()
                    .text_xs()
                    .text_color(theme.danger)
                    .child(error.clone())
                    .into_any_element(),
            ),
            _ => {}
        }
        if let Some(error) = self.status.errors.last() {
            indicators.push(
                div()
                    .max_w(px(320.))
                    .truncate()
                    .text_xs()
                    .text_color(theme.danger)
                    .child(error.clone())
                    .into_any_element(),
            );
        }

        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(theme.border)
            .children(container_picker)
            .child(
                toggle("log-previous", t!("dock-logs-previous"), self.previous).on_click(
                    cx.listener(|this, _, _, cx| {
                        let previous = !this.previous;
                        this.set_previous(previous, cx);
                    }),
                ),
            )
            .child(
                toggle("log-timestamps", t!("dock-logs-timestamps"), timestamps).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.view
                            .update(cx, |view, cx| view.set_show_timestamps(!timestamps, cx));
                    }),
                ),
            )
            .child(
                toggle("log-wrap", t!("dock-logs-wrap"), wrap).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.view.update(cx, |view, cx| view.set_wrap(!wrap, cx));
                    },
                )),
            )
            .child(
                div()
                    .w(px(220.))
                    .child(Input::new(&self.search).xsmall().cleanable(true)),
            )
            .child(
                Button::new("log-prev-match")
                    .ghost()
                    .xsmall()
                    .icon(IconName::ChevronUp)
                    .tooltip(t!("dock-logs-prev-match"))
                    .disabled(total == 0)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.view.update(cx, |view, cx| view.prev_match(cx));
                    })),
            )
            .child(
                Button::new("log-next-match")
                    .ghost()
                    .xsmall()
                    .icon(IconName::ChevronDown)
                    .tooltip(t!("dock-logs-next-match"))
                    .disabled(total == 0)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.view.update(cx, |view, cx| view.next_match(cx));
                    })),
            )
            .when(has_query, |bar| {
                bar.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("{current}/{total}")),
                )
            })
            .child(div().flex_1())
            .children(indicators)
            .child(
                Button::new("log-follow")
                    .ghost()
                    .xsmall()
                    .map(|button| {
                        if follow.is_following() {
                            button.icon(IconName::Pause).label(t!("dock-logs-pause"))
                        } else {
                            button
                                .icon(IconName::ArrowDown)
                                .label(t!("dock-logs-follow"))
                        }
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.view.update(cx, |view, cx| {
                            if follow.is_following() {
                                view.pause(cx)
                            } else {
                                view.follow(cx)
                            }
                        });
                    })),
            )
            .child(
                Button::new("log-copy")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Copy)
                    .tooltip(t!("dock-logs-copy"))
                    .on_click(cx.listener(|this, _, _, cx| this.copy(cx))),
            )
            .child(
                Button::new("log-clear")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Delete)
                    .tooltip(t!("dock-logs-clear"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        // Earlier lines would bring back what was cleared.
                        this._history_load = None;
                        if let Some(history) = &mut this.history {
                            history.turn_off();
                        }
                        this.view.update(cx, |view, cx| view.clear(cx));
                        this.update_top_notice(cx);
                    })),
            )
    }
}

impl Render for LogsTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.render_toolbar(cx))
            .child(div().flex_1().min_h_0().child(self.view.clone()))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kubach_core::streams::{LogEvent, LogLine, LogSource};

    use kubach_ui::SpanColor;

    use super::{LogsStatus, group_thousands, log_entry};

    fn source(container: &str) -> Arc<LogSource> {
        Arc::new(LogSource::new("ns", "pod", container))
    }

    #[test]
    fn status_tracks_reconnects_drops_and_ends() {
        let mut status = LogsStatus::default();
        status.apply(&LogEvent::Reconnecting {
            source: source("a"),
            attempt: 1,
            reason: "closed".into(),
        });
        status.apply(&LogEvent::Reconnecting {
            source: source("a"),
            attempt: 2,
            reason: "again".into(),
        });
        assert_eq!(
            status.reconnecting,
            vec![("pod/a".to_owned(), "again".to_owned())]
        );
        status.apply(&LogEvent::Lines {
            source: source("a"),
            lines: Vec::new(),
            dropped: 7,
        });
        assert!(status.reconnecting.is_empty());
        assert_eq!(status.dropped, 7);
        status.apply(&LogEvent::Ended {
            source: source("b"),
            error: Some("boom".into()),
        });
        assert_eq!(status.ended, 1);
        assert_eq!(status.errors, vec!["pod/b: boom".to_owned()]);
    }

    #[test]
    fn groups_thousands() {
        assert_eq!(group_thousands(50_000), "50,000");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn entries_carry_tag_and_timestamp() {
        let line = LogLine {
            source: source("a"),
            timestamp: Some("2026-10-22T10:11:12.345678Z".parse().unwrap()),
            text: "hello".into(),
        };
        let entry = log_entry(line, Some(("pod/a".into(), 2)));
        assert_eq!(entry.text, "hello");
        assert!(entry.spans.is_empty());
        assert_eq!(entry.timestamp.as_deref(), Some("2026-10-22T10:11:12.345Z"));
        assert_eq!(entry.tag.unwrap().color, 2);
    }

    #[test]
    fn ansi_codes_become_spans() {
        let line = LogLine {
            source: source("a"),
            timestamp: None,
            text: "\x1b[0;33mWARNING\x1b[0;m done".into(),
        };
        let entry = log_entry(line, None);
        assert_eq!(entry.text, "WARNING done");
        assert_eq!(entry.spans.len(), 1);
        assert_eq!(entry.spans[0].range, 0..7);
        assert_eq!(entry.spans[0].style.foreground, Some(SpanColor::Named(3)));
    }
}
