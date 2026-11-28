//! A dock tab streaming the logs of one pod or of every pod of a workload.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::StreamExt as _;
use futures::future::Either;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::ansi::{self, AnsiColor, AnsiStyle};
use kubach_core::runtime;
use kubach_core::streams::{
    FetchedLines, HistoryRequest, HistoryState, INITIAL_TAIL_LINES, LogDownloadError,
    LogDownloadOptions, LogDownloadReport, LogEvent, LogHistory, LogLine, LogOptions, LogSource,
    MAX_HISTORY_BYTES, MAX_HISTORY_LINES, SourceFailure, StreamError, download_logs, fetch_history,
    log_file_name_now, save_log_text, sources_for_selector, stream_logs,
};
use kubach_i18n::t;
use kubach_ui::{
    DEFAULT_LOG_CAPACITY, DownloadProgress, LogEntry, LogTopNotice, LogView, LogViewEvent,
    SpanColor, SpanStyle, StyledSpan, format_bytes,
};

use super::containers::{OnPick, fill_menu};
use super::resume::{Next, Resume};
use super::target::{ContainerMenu, LogTarget};
use crate::cluster::Cluster;

/// Events applied to the view per UI update.
const BATCH_EVENTS: usize = 256;
/// Pause between UI updates of a stream. Every update of the tab on screen
/// redraws the window (the views the tab is in; cached ones beside them
/// are reused), so applying each batch as it arrives redraws it at the rate
/// of the stream: a pod printing a few hundred lines a second kept the
/// window drawing over a hundred frames a second (#142). Lines that arrive during
/// the pause wait in the stream (which merges them into fewer batches) and
/// are shown together; a line after a quiet period is shown at once.
const UPDATE_INTERVAL: Duration = Duration::from_millis(50);
/// Pause between redraws of the bytes a download has written.
const DOWNLOAD_PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

/// Saving the log of the tab to a file (#214): shown in a row under the
/// toolbar until it is dismissed.
enum Download {
    Running {
        path: PathBuf,
        /// Bytes in the file so far.
        written: Arc<AtomicU64>,
        /// Reads from a context of the vault: ends when it locks.
        vault: bool,
        /// Dropping the task stops the download and removes its
        /// unfinished file.
        cancellable: bool,
        _task: Task<()>,
    },
    Saved {
        path: PathBuf,
        bytes: u64,
        /// Sources the file has nothing of, with the reason.
        skipped: Vec<String>,
    },
    Failed(String),
}

/// Why the log of a source is not in a saved file, for the list of skipped
/// sources.
fn skip_reason(reason: &SourceFailure) -> String {
    match reason {
        SourceFailure::Forbidden => t!("dock-logs-download-skip-forbidden"),
        SourceFailure::NotFound => t!("dock-logs-download-skip-gone"),
        SourceFailure::Other(message) => message.clone(),
    }
}

/// What the tab says about a download that saved nothing.
fn download_error(error: &LogDownloadError, path: &std::path::Path) -> String {
    match error {
        LogDownloadError::Source { log, reason } => {
            let source = log.label();
            match reason {
                SourceFailure::Forbidden => t!("dock-logs-download-forbidden", source = source),
                SourceFailure::NotFound => t!("dock-logs-download-gone", source = source),
                SourceFailure::Other(message) => t!(
                    "dock-logs-download-source-failed",
                    source = source,
                    error = message.as_str()
                ),
            }
        }
        LogDownloadError::File(error) => t!(
            "dock-logs-download-file-failed",
            path = path.display().to_string(),
            error = error.to_string()
        ),
    }
}

/// Where the stream is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogsState {
    Starting,
    Streaming,
    /// Every source ended (previous logs, deleted pods, stopped containers).
    Finished,
    /// Closed with the vault its context is in (ADR 0008).
    Locked,
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
pub fn log_entry(line: LogLine, tag: Option<(SharedString, usize)>) -> LogEntry {
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
        // Kept as a moment; the view formats it for the lines it draws.
        entry = entry.timestamp(timestamp);
    }
    if let Some((label, color)) = tag {
        entry = entry.tag(label, color);
    }
    entry
}

/// The tag of every source, made once per stream: the lines of a source
/// share its label (#171). Before, every line cloned the label as a
/// `String` and, when it is longer than the 23 bytes a `SharedString` keeps
/// inline, allocated it once more for the view.
fn source_tags(sources: &[Arc<LogSource>]) -> Vec<SharedString> {
    sources.iter().map(|source| source.label().into()).collect()
}

/// Label and color shown before lines of `source` when several sources are
/// merged; `tags` are the [`source_tags`] of `sources`.
fn tag_of(
    sources: &[Arc<LogSource>],
    tags: &[SharedString],
    source: &LogSource,
) -> Option<(SharedString, usize)> {
    if sources.len() < 2 {
        return None;
    }
    let known = sources.iter().position(|known| **known == *source);
    Some(
        match known.and_then(|index| Some((tags.get(index)?, index))) {
            Some((tag, index)) => (tag.clone(), index),
            None => (source.label().into(), 0),
        },
    )
}

/// Applies the events of `stream` to `tab` until the stream ends, at most
/// once per [`UPDATE_INTERVAL`]. False when the tab is gone.
async fn pump(
    stream: impl futures::Stream<Item = LogEvent> + Unpin,
    tab: &WeakEntity<LogsTab>,
    cx: &mut AsyncApp,
) -> bool {
    let mut batches = stream.ready_chunks(BATCH_EVENTS);
    while let Some(batch) = batches.next().await {
        if tab.update(cx, |tab, cx| tab.apply(batch, cx)).is_err() {
            return false;
        }
        cx.background_executor().timer(UPDATE_INTERVAL).await;
    }
    true
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

impl EventEmitter<super::TabChanged> for LogsTab {}

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
    /// [`source_tags`] of `sources`.
    tags: Vec<SharedString>,
    state: LogsState,
    status: LogsStatus,
    /// Earlier lines loaded on scrolling to the top.
    history: Option<LogHistory>,
    history_error: Option<String>,
    /// The stream runs on a context of the vault and ends when it locks.
    vault: bool,
    /// The last download of the log to a file, running or finished.
    download: Option<Download>,
    /// The lock closed a running stream: it starts again after the unlock.
    resume: Option<Resume>,
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
            tags: Vec::new(),
            state: LogsState::Starting,
            status: LogsStatus::default(),
            history: None,
            history_error: None,
            vault: false,
            download: None,
            resume: None,
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

    /// Moves the stream on; the dock's tab strip shows the state (and the
    /// title, which a restart with another container changed).
    fn set_state(&mut self, state: LogsState, cx: &mut Context<Self>) {
        self.state = state;
        cx.emit(super::TabChanged);
        cx.notify();
    }

    /// (Re)starts the stream with the current container and previous-logs
    /// choice; the old stream is dropped, which closes it.
    pub fn restart(&mut self, cx: &mut Context<Self>) {
        self._stream = None;
        self._history_load = None;
        self.history = None;
        self.history_error = None;
        self.sources.clear();
        self.tags.clear();
        self.status = LogsStatus::default();
        self.view.update(cx, |view, cx| {
            view.clear(cx);
            view.set_top_notice(None, cx);
            view.set_placeholder(t!("dock-connecting"), cx);
        });
        self.set_state(LogsState::Starting, cx);

        let tab_client =
            crate::action_context::tab_client(self.cluster.read(cx), self.context.as_deref(), cx);
        let client = match tab_client {
            Ok(tab) => {
                self.vault = tab.vault;
                // Started by hand: nothing is left to bring back.
                self.resume = None;
                tab.client
            }
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
                this.tags = source_tags(&this.sources);
                this.history = Some(LogHistory::new(&this.sources, INITIAL_TAIL_LINES));
                this.view.update(cx, |view, cx| {
                    view.set_placeholder(t!("dock-logs-waiting"), cx)
                });
                this.set_state(LogsState::Streaming, cx);
            });
            if started.is_err() {
                return;
            }
            if !pump(stream, &this, cx).await {
                return;
            }
            this.update(cx, |this, cx| {
                this.view.update(cx, |view, cx| {
                    view.set_placeholder(t!("dock-logs-no-lines"), cx)
                });
                this.set_state(LogsState::Finished, cx);
            })
            .ok();
        }));
    }

    /// The vault locked: a stream on one of its contexts closes, the lines
    /// read so far stay, and the tab says why (ADR 0008).
    pub fn vault_locked(&mut self, cx: &mut Context<Self>) {
        self.lock_download(cx);
        if !std::mem::take(&mut self.vault) {
            return;
        }
        self._stream = None;
        self._history_load = None;
        if matches!(self.state, LogsState::Starting | LogsState::Streaming) {
            self.view.update(cx, |view, cx| {
                view.set_placeholder(t!("dock-vault-locked"), cx)
            });
            self.set_state(LogsState::Locked, cx);
            self.resume = Some(Resume::new(&self.cluster, cx, Self::resume));
        }
        self.update_top_notice(cx);
        cx.notify();
    }

    /// The vault or the contexts changed after a lock closed the stream:
    /// once the vault is unlocked and the context connected again, the
    /// stream starts over with the same target, container and
    /// previous-logs choice (#213). The lines stay on screen until then.
    fn resume(&mut self, cx: &mut Context<Self>) {
        let Some(resume) = &mut self.resume else {
            return;
        };
        match resume.next(&self.cluster, self.context.as_deref(), cx) {
            // Locked again while its context was connecting.
            Next::Wait if self.state == LogsState::Starting => {
                self.view.update(cx, |view, cx| {
                    view.set_placeholder(t!("dock-vault-locked"), cx)
                });
                self.set_state(LogsState::Locked, cx);
            }
            Next::Wait => {}
            Next::Connecting if self.state == LogsState::Locked => {
                self.view.update(cx, |view, cx| {
                    view.set_placeholder(t!("dock-connecting"), cx)
                });
                self.set_state(LogsState::Starting, cx);
            }
            Next::Connecting => {}
            Next::Start => {
                self.resume = None;
                self.restart(cx);
            }
            Next::Fail(error) => {
                self.resume = None;
                self.fail(error, cx);
            }
            Next::Forget => self.resume = None,
        }
    }

    fn fail(&mut self, error: String, cx: &mut Context<Self>) {
        self.view
            .update(cx, |view, cx| view.set_placeholder(error.clone(), cx));
        self.set_state(LogsState::Failed(error), cx);
    }

    /// Source label and color shown before lines of `source` when several
    /// sources are merged.
    fn tag(&self, source: &LogSource) -> Option<(SharedString, usize)> {
        tag_of(&self.sources, &self.tags, source)
    }

    /// Shows a batch of the stream. The lines redraw the view, which
    /// notifies for them itself (and only reaches a window while the tab
    /// is shown); the tab redraws its toolbar when the health of the
    /// stream changed, not with every batch (#166).
    fn apply(&mut self, batch: Vec<LogEvent>, cx: &mut Context<Self>) {
        let status = self.status.clone();
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
        if self.status != status {
            cx.notify();
        }
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
            crate::action_context::tab_client(self.cluster.read(cx), self.context.as_deref(), cx)
                .map(|tab| tab.client)
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
        let full = self.view.update(cx, |view, cx| {
            view.prepend(entries, cx);
            view.has_evicted()
        });
        // Not all of them fit: asking for more would load them again.
        if full && let Some(history) = &mut self.history {
            history.turn_off();
        }
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

    /// The name offered for a saved log: `<pod>_<container>_<time>.log`.
    fn download_name(&self) -> String {
        match &self.target {
            LogTarget::Pod(pod) => log_file_name_now(&pod.name, self.container.as_deref()),
            LogTarget::Selector { title, .. } => log_file_name_now(title, None),
        }
    }

    /// Asks where to save, then saves the lines of the tab or, with
    /// `everything`, the whole log from the server.
    fn download(&mut self, everything: bool, cx: &mut Context<Self>) {
        let directory = kubach_core::env::home_dir()
            .map(|home| home.join("Downloads"))
            .filter(|dir| dir.is_dir())
            .or_else(kubach_core::env::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        let path = cx.prompt_for_new_path(&directory, Some(&self.download_name()));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = path.await else {
                return;
            };
            this.update(cx, |this, cx| {
                // Another one was started while the dialog was open.
                if matches!(this.download, Some(Download::Running { .. })) {
                    return;
                }
                if everything {
                    this.save_everything(path, cx);
                } else {
                    this.save_visible(path, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Saves the lines the tab holds, as it shows them (source tags, and
    /// timestamps when they are on). Search only highlights, so every
    /// line is saved.
    fn save_visible(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let text = self.view.read(cx).text();
        let bytes = text.len() as u64;
        let target = path.clone();
        let task = cx.spawn(async move |this, cx| {
            let write_to = target.clone();
            let saved = cx
                .background_spawn(async move { save_log_text(&write_to, &text) })
                .await;
            this.update(cx, |this, cx| {
                this.download = Some(match saved {
                    Ok(()) => Download::Saved {
                        path: target,
                        bytes,
                        skipped: Vec::new(),
                    },
                    Err(error) => {
                        Download::Failed(download_error(&LogDownloadError::File(error), &target))
                    }
                });
                cx.notify();
            })
            .ok();
        });
        self.download = Some(Download::Running {
            path,
            written: Arc::new(AtomicU64::new(bytes)),
            vault: false,
            cancellable: false,
            _task: task,
        });
    }

    /// Streams the whole log of the tab's sources from the server to
    /// `path`, with the tab's choice of timestamps and of the previous
    /// container instance.
    fn save_everything(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let tab_client =
            crate::action_context::tab_client(self.cluster.read(cx), self.context.as_deref(), cx);
        let tab_client = match tab_client {
            Ok(tab_client) => tab_client,
            Err(error) => {
                self.download = Some(Download::Failed(error));
                return;
            }
        };
        if self.sources.is_empty() {
            self.download = Some(Download::Failed(t!("dock-logs-no-containers")));
            return;
        }
        let sources: Vec<LogSource> = self
            .sources
            .iter()
            .map(|source| LogSource::clone(source))
            .collect();
        let options = LogDownloadOptions {
            previous: self.previous,
            timestamps: self.view.read(cx).show_timestamps(),
        };
        let written = Arc::new(AtomicU64::new(0));
        let mut work = runtime::spawn({
            let (client, target, written) = (tab_client.client, path.clone(), written.clone());
            async move { download_logs(&client, sources, options, &target, &written).await }
        });
        let target = path.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = loop {
                let tick =
                    std::pin::pin!(cx.background_executor().timer(DOWNLOAD_PROGRESS_INTERVAL));
                match futures::future::select(&mut work, tick).await {
                    Either::Left((result, _)) => break result,
                    Either::Right(_) => {
                        if this.update(cx, |_, cx| cx.notify()).is_err() {
                            return;
                        }
                    }
                }
            };
            let result = result.map_err(|error| error.to_string());
            this.update(cx, |this, cx| this.download_finished(target, result, cx))
                .ok();
        });
        self.download = Some(Download::Running {
            path,
            written,
            vault: tab_client.vault,
            cancellable: true,
            _task: task,
        });
    }

    fn download_finished(
        &mut self,
        path: PathBuf,
        result: Result<Result<LogDownloadReport, LogDownloadError>, String>,
        cx: &mut Context<Self>,
    ) {
        self.download = Some(match result {
            Ok(Ok(report)) => Download::Saved {
                bytes: report.bytes,
                skipped: report
                    .skipped
                    .iter()
                    .map(|(source, reason)| format!("{}: {}", source.label(), skip_reason(reason)))
                    .collect(),
                path,
            },
            Ok(Err(error)) => Download::Failed(download_error(&error, &path)),
            Err(error) => Download::Failed(error),
        });
        cx.notify();
    }

    /// The vault locked: a download reading from one of its contexts stops
    /// and its unfinished file is removed (ADR 0008).
    fn lock_download(&mut self, cx: &mut Context<Self>) {
        if matches!(self.download, Some(Download::Running { vault: true, .. })) {
            self.download = Some(Download::Failed(t!("dock-logs-download-locked")));
            cx.notify();
        }
    }

    /// "Download" in the toolbar: the lines of the tab or the whole log.
    fn render_download_button(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tab = cx.entity().downgrade();
        let running = matches!(self.download, Some(Download::Running { .. }));
        // The whole log is read from the sources the stream found.
        let no_sources = self.sources.is_empty();
        Button::new("log-download")
            .ghost()
            .xsmall()
            .icon(Lucide::Download)
            .tooltip(t!("dock-logs-download"))
            .disabled(running)
            .dropdown_menu(move |menu: PopupMenu, _, _| {
                [
                    (t!("dock-logs-download-visible"), false, false),
                    (t!("dock-logs-download-all"), true, no_sources),
                ]
                .into_iter()
                .fold(menu, |menu, (label, everything, disabled)| {
                    let tab = tab.clone();
                    menu.item(PopupMenuItem::new(label).disabled(disabled).on_click(
                        move |_, _, cx| {
                            tab.update(cx, |tab, cx| tab.download(everything, cx)).ok();
                        },
                    ))
                })
            })
    }

    /// The row under the toolbar: progress of a running download with
    /// "Cancel", or how the last one ended.
    fn render_download(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let download = self.download.as_ref()?;
        let theme = cx.theme();
        let row = div()
            .flex()
            .items_start()
            .gap_2()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(theme.border)
            .text_xs();
        let dismiss = || {
            Button::new("log-download-dismiss")
                .ghost()
                .xsmall()
                .icon(IconName::Close)
                .tooltip(t!("dock-logs-download-dismiss"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.download = None;
                    cx.notify();
                }))
        };
        let row = match download {
            Download::Running {
                path,
                written,
                cancellable,
                ..
            } => {
                let file = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let progress = DownloadProgress::new(
                    "log-download-progress",
                    t!("dock-logs-downloading", file = file),
                )
                .bytes(written.load(Ordering::Relaxed), None);
                let progress = if *cancellable {
                    progress.on_cancel(cx.listener(|this, _, _, cx| {
                        // Dropping the task removes the unfinished file.
                        this.download = None;
                        cx.notify();
                    }))
                } else {
                    progress
                };
                row.child(div().flex_1().min_w_0().child(progress))
            }
            Download::Saved {
                path,
                bytes,
                skipped,
            } => row
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(div().text_color(theme.muted_foreground).child(t!(
                            "dock-logs-download-saved",
                            path = path.display().to_string(),
                            size = format_bytes(*bytes)
                        )))
                        .when(!skipped.is_empty(), |column| {
                            column.child(div().text_color(theme.warning).child(t!(
                                "dock-logs-download-skipped",
                                sources = skipped.join("; ")
                            )))
                        }),
                )
                .child(dismiss()),
            Download::Failed(error) => row
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(theme.danger)
                        .child(error.clone()),
                )
                .child(dismiss()),
        };
        Some(row.into_any_element())
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
        let download_button = self.render_download_button(cx);
        let theme = cx.theme();
        let view = self.view.read(cx);
        let (current, total) = view.match_position();
        let has_query = !view.search_query().is_empty();
        // The buffer is searched over several frames (#171): until it is
        // done the total is a lower bound, and a step may be waiting for it.
        let searching = view.search_running();
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
            LogsState::Locked => indicators.push(
                div()
                    .text_xs()
                    .text_color(theme.warning)
                    .child(t!("dock-vault-locked"))
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
                    .disabled(total == 0 && !searching)
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
                    .disabled(total == 0 && !searching)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.view.update(cx, |view, cx| view.next_match(cx));
                    })),
            )
            .when(has_query, |bar| {
                bar.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "{current}/{total}{}",
                            if searching { "+" } else { "" }
                        )),
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
            .child(download_button)
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
            .children(self.render_download(cx))
            .child(div().flex_1().min_h_0().child(self.view.clone()))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kubach_core::streams::{LogEvent, LogLine, LogSource};

    use kubach_ui::SpanColor;

    use super::{LogsStatus, group_thousands, log_entry, source_tags, tag_of};

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
        assert_eq!(
            entry.time(),
            Some("2026-10-22T10:11:12.345Z".parse().unwrap())
        );
        assert_eq!(entry.tag.unwrap().color, 2);
    }

    #[test]
    fn lines_of_a_source_share_its_tag() {
        // A pod of a Deployment: the label is too long to be kept inline.
        let source = |container: &str| {
            Arc::new(LogSource::new(
                "ns",
                "payments-api-7d9f8c6b5-x2x4z",
                container,
            ))
        };
        let sources = vec![source("a"), source("b")];
        let tags = source_tags(&sources);
        let line = |container: &str| LogLine {
            source: source(container),
            timestamp: None,
            text: "hello".into(),
        };
        let label = |container: &str| {
            let tag = tag_of(&sources, &tags, &source(container));
            log_entry(line(container), tag).tag.unwrap()
        };
        let (first, second, other) = (label("b"), label("b"), label("a"));
        assert_eq!(first.label.as_ref(), "payments-api-7d9f8c6b5-x2x4z/b");
        assert_eq!(first.color, 1);
        assert_eq!(other.label.as_ref(), "payments-api-7d9f8c6b5-x2x4z/a");
        assert_eq!(other.color, 0);
        // One allocation for every line of the source.
        assert!(std::ptr::eq(first.label.as_ptr(), second.label.as_ptr()));
        assert!(std::ptr::eq(first.label.as_ptr(), tags[1].as_ptr()));
        assert!(!std::ptr::eq(first.label.as_ptr(), other.label.as_ptr()));
        // A source the stream did not start with still gets a label.
        let unknown = tag_of(&sources, &tags, &source("c")).unwrap();
        assert_eq!(unknown.0.as_ref(), "payments-api-7d9f8c6b5-x2x4z/c");
        assert_eq!(unknown.1, 0);
        // A single source is not tagged.
        assert_eq!(tag_of(&sources[..1], &tags[..1], &source("a")), None);
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
