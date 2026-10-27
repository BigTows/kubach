//! Container logs: one or many containers merged into a single stream of
//! tagged, line-oriented batches.
//!
//! Each source is read by its own task on the I/O runtime. Lines are batched
//! while the consumer is busy; when a source's backlog exceeds
//! [`LogOptions::max_pending_lines`] the oldest lines are dropped and the next
//! batch reports how many were lost. Following streams reconnect (bounded) when
//! the server closes them while the container is still running, resuming from
//! the last seen timestamp without duplicating lines.

use std::collections::VecDeque;
use std::future::poll_fn;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use futures::channel::mpsc;
use futures::{AsyncRead, AsyncReadExt, SinkExt, Stream};
use k8s_openapi::api::core::v1::{ContainerStatus, Pod};
use k8s_openapi::jiff::Timestamp;
use kube::Client;
use kube::api::{Api, ListParams, LogParams};

use super::StreamError;
use crate::runtime::{self, BackgroundTask};

/// Batches buffered in the channel per stream (plus one slot per source).
const CHANNEL_BATCHES: usize = 16;
/// Lines longer than this are cut (#195): the first `MAX_LINE_BYTES` are
/// kept and [`CUT_LINE_MARK`] is appended, the rest of the line is skipped.
pub const MAX_LINE_BYTES: usize = 64 * 1024;
/// Appended to a line cut at [`MAX_LINE_BYTES`].
pub const CUT_LINE_MARK: &str = " … (line cut)";

/// One container whose logs are streamed.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LogSource {
    pub namespace: String,
    pub pod: String,
    pub container: String,
}

impl LogSource {
    pub fn new(
        namespace: impl Into<String>,
        pod: impl Into<String>,
        container: impl Into<String>,
    ) -> Self {
        Self {
            namespace: namespace.into(),
            pod: pod.into(),
            container: container.into(),
        }
    }

    /// `pod/container`, as shown next to merged lines.
    pub fn label(&self) -> String {
        format!("{}/{}", self.pod, self.container)
    }
}

/// Where logs start from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogSince {
    /// Relative to now.
    Seconds(i64),
    Time(Timestamp),
}

/// What to fetch and how to stream it.
#[derive(Clone, Debug)]
pub struct LogOptions {
    /// Keep the stream open and receive new lines.
    pub follow: bool,
    /// Logs of the previous (terminated) container instance. Never reconnects.
    pub previous: bool,
    /// Only the last N lines.
    pub tail_lines: Option<i64>,
    pub since: Option<LogSince>,
    /// Fill [`LogLine::timestamp`]. The server is always asked for timestamps
    /// (they are needed to resume after reconnects), this only controls
    /// whether they are exposed.
    pub timestamps: bool,
    pub limit_bytes: Option<i64>,
    /// Reconnects in a row without receiving a line before giving up.
    pub max_reconnects: u32,
    /// Per-source backlog while the consumer is slow; older lines are dropped
    /// beyond it.
    pub max_pending_lines: usize,
}

impl Default for LogOptions {
    fn default() -> Self {
        Self {
            follow: true,
            previous: false,
            tail_lines: None,
            since: None,
            timestamps: true,
            limit_bytes: None,
            max_reconnects: 5,
            max_pending_lines: 10_000,
        }
    }
}

impl LogOptions {
    /// Request parameters for `container`. Timestamps are always requested.
    pub fn to_params(&self, container: &str) -> LogParams {
        let (since_seconds, since_time) = match self.since {
            Some(LogSince::Seconds(seconds)) => (Some(seconds), None),
            Some(LogSince::Time(time)) => (None, Some(time)),
            None => (None, None),
        };
        LogParams {
            container: Some(container.to_owned()),
            follow: self.follow,
            limit_bytes: self.limit_bytes,
            pretty: false,
            previous: self.previous,
            since_seconds,
            since_time,
            tail_lines: self.tail_lines,
            timestamps: true,
        }
    }

    /// Parameters for reconnecting after `last` was the newest line seen:
    /// everything since then, no tail. Without a seen line the original
    /// parameters are reused.
    fn resume_params(&self, container: &str, last: Option<Timestamp>) -> LogParams {
        let mut params = self.to_params(container);
        if let Some(last) = last {
            params.tail_lines = None;
            params.since_seconds = None;
            params.since_time = Some(last);
        }
        params
    }
}

/// One log line, tagged with its source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogLine {
    pub source: Arc<LogSource>,
    pub timestamp: Option<Timestamp>,
    pub text: String,
}

/// What a [`LogStream`] yields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogEvent {
    /// New lines from `source`, oldest first. `dropped` lines were discarded
    /// right before them because the consumer fell behind.
    Lines {
        source: Arc<LogSource>,
        lines: Vec<LogLine>,
        dropped: u64,
    },
    /// The server closed the stream; reconnecting after a backoff.
    Reconnecting {
        source: Arc<LogSource>,
        attempt: u32,
        reason: String,
    },
    /// No more lines from `source`. `error` is set when it stopped on a failure.
    Ended {
        source: Arc<LogSource>,
        error: Option<String>,
    },
}

/// Merged log events of several sources, in arrival order. Ends after every
/// source has sent [`LogEvent::Ended`]; dropping it stops all requests.
pub struct LogStream {
    events: mpsc::Receiver<LogEvent>,
    _tasks: Vec<BackgroundTask>,
}

impl Stream for LogStream {
    type Item = LogEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<LogEvent>> {
        Pin::new(&mut self.events).poll_next(cx)
    }
}

/// Starts streaming `sources` on the I/O runtime. Can be called from any thread.
pub fn stream_logs(client: &Client, sources: Vec<LogSource>, options: LogOptions) -> LogStream {
    let (tx, events) = mpsc::channel(CHANNEL_BATCHES);
    let tasks = sources
        .into_iter()
        .map(|source| {
            let client = client.clone();
            let options = options.clone();
            let tx = tx.clone();
            runtime::spawn(follow_source(client, Arc::new(source), options, tx))
        })
        .collect();
    LogStream {
        events,
        _tasks: tasks,
    }
}

/// Containers of `pod` as log sources; init containers first when asked.
pub fn sources_for_pod(pod: &Pod, include_init: bool) -> Vec<LogSource> {
    let namespace = pod.metadata.namespace.clone().unwrap_or_default();
    let name = pod.metadata.name.clone().unwrap_or_default();
    let Some(spec) = &pod.spec else {
        return Vec::new();
    };
    let init = spec
        .init_containers
        .iter()
        .flatten()
        .filter(|_| include_init);
    init.chain(&spec.containers)
        .map(|container| LogSource::new(&namespace, &name, &container.name))
        .collect()
}

/// All containers of the pods matching `label_selector` in `namespace`.
/// Must run on the [`runtime`](crate::runtime).
pub async fn sources_for_selector(
    client: &Client,
    namespace: &str,
    label_selector: &str,
) -> Result<Vec<LogSource>, StreamError> {
    let api: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let pods = api
        .list(&ListParams::default().labels(label_selector))
        .await?;
    Ok(pods
        .items
        .iter()
        .flat_map(|pod| sources_for_pod(pod, false))
        .collect())
}

/// Reads one source until it ends, reconnecting while following.
async fn follow_source(
    client: Client,
    source: Arc<LogSource>,
    options: LogOptions,
    tx: mpsc::Sender<LogEvent>,
) {
    let api: Api<Pod> = Api::namespaced(client, &source.namespace);
    let mut outbox = Outbox::new(tx, source.clone(), options.max_pending_lines);
    let mut resume = ResumeFilter::default();
    let mut params = options.to_params(&source.container);
    let mut failures = 0u32;
    loop {
        let result = match api.log_stream(&source.pod, &params).await {
            Ok(reader) => pump(reader, &mut outbox, &mut resume, options.timestamps).await,
            Err(error) => Err(StreamError::from(error)),
        };
        let error = match result {
            Ok(Pumped::ConsumerGone) => return,
            Ok(Pumped::Eof { lines }) => {
                if lines > 0 {
                    failures = 0;
                }
                None
            }
            Err(error) => Some(error.to_string()),
        };
        if !options.follow || options.previous {
            outbox.send(LogEvent::Ended { source, error }).await;
            return;
        }
        let reason = error
            .clone()
            .unwrap_or_else(|| "stream closed by server".to_owned());
        // A restarting container (crash loop back-off, image pull) is waited
        // for without spending reconnect attempts.
        let deadline = Instant::now() + RESTART_WAIT;
        let mut announced = false;
        let liveness = loop {
            let liveness = match api.get_opt(&source.pod).await {
                Ok(Some(pod)) => container_liveness(&pod, &source.container),
                Ok(None) => Liveness::Gone,
                // Unknown: let the bounded retries decide.
                Err(_) => Liveness::Running,
            };
            if liveness != Liveness::Restarting || Instant::now() >= deadline {
                break liveness;
            }
            if !announced {
                announced = true;
                let event = LogEvent::Reconnecting {
                    source: source.clone(),
                    attempt: failures + 1,
                    reason: format!("{reason}; waiting for the container to start"),
                };
                if !outbox.send(event).await {
                    return;
                }
            }
            if outbox.tx.is_closed() {
                return;
            }
            tokio::time::sleep(RESTART_POLL).await;
        };
        if liveness != Liveness::Running {
            outbox.send(LogEvent::Ended { source, error }).await;
            return;
        }
        failures += 1;
        if failures > options.max_reconnects {
            let error = format!(
                "gave up after {} reconnects: {reason}",
                options.max_reconnects
            );
            outbox
                .send(LogEvent::Ended {
                    source,
                    error: Some(error),
                })
                .await;
            return;
        }
        if !announced {
            let event = LogEvent::Reconnecting {
                source: source.clone(),
                attempt: failures,
                reason,
            };
            if !outbox.send(event).await {
                return;
            }
        }
        tokio::time::sleep(backoff(failures)).await;
        params = options.resume_params(&source.container, resume.begin_resume());
    }
}

fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(1 << attempt.saturating_sub(1).min(3))
}

/// Whether a container can still produce logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Liveness {
    Running,
    /// Waiting to (re)start: creating, back-off, terminated with a restart
    /// policy that restarts it.
    Restarting,
    /// Will not log again.
    Gone,
}

/// How long a follow stream waits for a restarting container.
const RESTART_WAIT: Duration = Duration::from_secs(10 * 60);
const RESTART_POLL: Duration = Duration::from_secs(2);

/// Whether `container` of `pod` is running, about to (re)start or done.
pub fn container_liveness(pod: &Pod, container: &str) -> Liveness {
    if pod.metadata.deletion_timestamp.is_some() {
        return Liveness::Gone;
    }
    let Some(status) = &pod.status else {
        return Liveness::Gone;
    };
    if matches!(status.phase.as_deref(), Some("Succeeded" | "Failed")) {
        return Liveness::Gone;
    }
    let find = |statuses: &Option<Vec<ContainerStatus>>| {
        statuses
            .iter()
            .flatten()
            .find(|status| status.name == container)
            .and_then(|status| status.state.clone())
    };
    let spec = pod.spec.as_ref();
    let pod_policy = spec.and_then(|spec| spec.restart_policy.as_deref());
    let (state, policy) = if let Some(state) = find(&status.container_statuses) {
        (state, pod_policy.unwrap_or("Always"))
    } else if let Some(state) = find(&status.init_container_statuses) {
        // Sidecars (`restartPolicy: Always` init containers) always restart,
        // other init containers only on failure.
        let sidecar = spec
            .and_then(|spec| spec.init_containers.as_ref())
            .into_iter()
            .flatten()
            .any(|c| c.name == container && c.restart_policy.as_deref() == Some("Always"));
        let policy = match (sidecar, pod_policy) {
            (true, _) => "Always",
            (false, Some("Never")) => "Never",
            (false, _) => "OnFailure",
        };
        (state, policy)
    } else if let Some(state) = find(&status.ephemeral_container_statuses) {
        (state, "Never")
    } else {
        // Not reported yet: the pod is still being set up.
        return match status.phase.as_deref() {
            Some("Pending") => Liveness::Restarting,
            _ => Liveness::Gone,
        };
    };
    if state.running.is_some() {
        return Liveness::Running;
    }
    if state.waiting.is_some() {
        return Liveness::Restarting;
    }
    // Terminated: the kubelet restarts it unless the policy says otherwise.
    let restarts = match state.terminated.map(|terminated| terminated.exit_code) {
        Some(code) => match policy {
            "Always" => true,
            "OnFailure" => code != 0,
            _ => false,
        },
        None => false,
    };
    if restarts {
        Liveness::Restarting
    } else {
        Liveness::Gone
    }
}

/// How a single connection ended.
#[derive(Debug, PartialEq, Eq)]
enum Pumped {
    Eof { lines: usize },
    ConsumerGone,
}

/// Reads `reader` to its end, sending lines through `outbox` and flushing it.
async fn pump<R: AsyncRead + Unpin>(
    mut reader: R,
    outbox: &mut Outbox,
    resume: &mut ResumeFilter,
    keep_timestamps: bool,
) -> Result<Pumped, StreamError> {
    let mut splitter = LineSplitter::default();
    let mut buf = vec![0u8; 16 * 1024];
    let mut lines = 0;
    let mut accept = |text: String, outbox: &mut Outbox| {
        let (timestamp, text) = split_timestamp(&text);
        if resume.accept(timestamp) {
            lines += 1;
            outbox.push(LogLine {
                source: outbox.source.clone(),
                timestamp: timestamp.filter(|_| keep_timestamps),
                text: text.to_owned(),
            });
        }
    };
    loop {
        let has_pending = outbox.has_pending();
        tokio::select! {
            read = reader.read(&mut buf) => {
                let n = read?;
                if n == 0 {
                    break;
                }
                for text in splitter.push(&buf[..n]) {
                    accept(text, outbox);
                }
            }
            ready = poll_fn(|cx| outbox.tx.poll_ready(cx)), if has_pending => {
                if ready.is_err() || !outbox.start_send() {
                    return Ok(Pumped::ConsumerGone);
                }
            }
        }
    }
    if let Some(text) = splitter.finish() {
        accept(text, outbox);
    }
    if !outbox.flush().await {
        return Ok(Pumped::ConsumerGone);
    }
    Ok(Pumped::Eof { lines })
}

/// Per-source backlog in front of the shared bounded channel.
struct Outbox {
    tx: mpsc::Sender<LogEvent>,
    source: Arc<LogSource>,
    pending: VecDeque<LogLine>,
    dropped: u64,
    max_pending: usize,
}

impl Outbox {
    fn new(tx: mpsc::Sender<LogEvent>, source: Arc<LogSource>, max_pending: usize) -> Self {
        Self {
            tx,
            source,
            pending: VecDeque::new(),
            dropped: 0,
            max_pending: max_pending.max(1),
        }
    }

    fn has_pending(&self) -> bool {
        !self.pending.is_empty() || self.dropped > 0
    }

    /// Queues a line, dropping the oldest one when the backlog is full.
    fn push(&mut self, line: LogLine) {
        if self.pending.len() >= self.max_pending {
            self.pending.pop_front();
            self.dropped += 1;
        }
        self.pending.push_back(line);
    }

    fn take_batch(&mut self) -> LogEvent {
        LogEvent::Lines {
            source: self.source.clone(),
            lines: self.pending.drain(..).collect(),
            dropped: std::mem::take(&mut self.dropped),
        }
    }

    /// Sends the backlog as one batch; call after `poll_ready` succeeded.
    fn start_send(&mut self) -> bool {
        let batch = self.take_batch();
        self.tx.start_send(batch).is_ok()
    }

    /// Waits until the backlog is handed over. False when the consumer is gone.
    async fn flush(&mut self) -> bool {
        if !self.has_pending() {
            return true;
        }
        let batch = self.take_batch();
        self.tx.send(batch).await.is_ok()
    }

    /// Flushes, then sends a control event. False when the consumer is gone.
    async fn send(&mut self, event: LogEvent) -> bool {
        self.flush().await && self.tx.send(event).await.is_ok()
    }
}

/// Skips lines already delivered before a reconnect: the server resends
/// everything from the last timestamp (inclusive, possibly truncated to the
/// second).
#[derive(Debug, Default)]
struct ResumeFilter {
    last: Option<Timestamp>,
    seen_at_last: usize,
    skip: Option<(Timestamp, usize)>,
}

impl ResumeFilter {
    /// Starts skipping already seen lines; returns where to resume from.
    fn begin_resume(&mut self) -> Option<Timestamp> {
        self.skip = self.last.map(|last| (last, self.seen_at_last));
        self.last
    }

    fn accept(&mut self, timestamp: Option<Timestamp>) -> bool {
        let Some(timestamp) = timestamp else {
            return true;
        };
        if let Some((cutoff, remaining)) = &mut self.skip {
            if timestamp < *cutoff {
                return false;
            }
            if timestamp == *cutoff && *remaining > 0 {
                *remaining -= 1;
                return false;
            }
            self.skip = None;
        }
        if self.last == Some(timestamp) {
            self.seen_at_last += 1;
        } else {
            self.last = Some(timestamp);
            self.seen_at_last = 1;
        }
        true
    }
}

/// Lines of [`fetch_log_lines`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FetchedLines {
    /// Oldest first.
    pub lines: Vec<LogLine>,
    /// The answer reached the byte limit: the newest lines of the requested
    /// tail are missing (an incomplete last line is dropped).
    pub truncated: bool,
}

impl From<Vec<LogLine>> for FetchedLines {
    fn from(lines: Vec<LogLine>) -> Self {
        Self {
            lines,
            truncated: false,
        }
    }
}

/// Fetches the last `tail_lines` lines of `source` in one request (no
/// follow), split exactly like a [`LogStream`] splits them, with timestamps.
/// At most `limit_bytes` are read (the server is asked for no more, and a
/// longer answer is cut here too); the server counts them from the start of
/// the tail, so a cut answer keeps the oldest lines. Used to load lines older
/// than the ones already streamed. Must run on the [`runtime`](crate::runtime).
pub async fn fetch_log_lines(
    client: &Client,
    source: Arc<LogSource>,
    previous: bool,
    tail_lines: i64,
    limit_bytes: usize,
) -> Result<FetchedLines, StreamError> {
    let api: Api<Pod> = Api::namespaced(client.clone(), &source.namespace);
    let limit_bytes = limit_bytes.max(1);
    let options = LogOptions {
        follow: false,
        previous,
        tail_lines: Some(tail_lines.max(1)),
        limit_bytes: Some(i64::try_from(limit_bytes).unwrap_or(i64::MAX)),
        ..LogOptions::default()
    };
    let reader = api
        .log_stream(&source.pod, &options.to_params(&source.container))
        .await?;
    read_lines(reader, &source, limit_bytes).await
}

/// Reads `reader` as timestamped log lines of `source`, to its end but at
/// most `limit` bytes.
async fn read_lines<R: AsyncRead + Unpin>(
    mut reader: R,
    source: &Arc<LogSource>,
    limit: usize,
) -> Result<FetchedLines, StreamError> {
    let mut splitter = LineSplitter::default();
    let mut buf = vec![0u8; 64 * 1024];
    let mut lines = Vec::new();
    let line = |text: String| {
        let (timestamp, text) = split_timestamp(&text);
        LogLine {
            source: source.clone(),
            timestamp,
            text: text.to_owned(),
        }
    };
    let mut read = 0;
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        let n = n.min(limit - read);
        read += n;
        lines.extend(splitter.push(&buf[..n]).into_iter().map(line));
        if read >= limit {
            // The rest is not read; the unfinished last line is dropped.
            return Ok(FetchedLines {
                lines,
                truncated: true,
            });
        }
    }
    lines.extend(splitter.finish().map(line));
    Ok(FetchedLines {
        lines,
        truncated: false,
    })
}

/// Splits `2024-01-01T00:00:00.123Z text` into the timestamp and the text.
fn split_timestamp(line: &str) -> (Option<Timestamp>, &str) {
    let (head, rest) = line.split_once(' ').unwrap_or((line, ""));
    match head.parse::<Timestamp>() {
        Ok(timestamp) => (Some(timestamp), rest),
        Err(_) => (None, line),
    }
}

/// Turns a byte stream into lines: splits on `\n`, strips `\r`, keeps partial
/// lines until they are completed, decodes UTF-8 lossily and cuts lines
/// longer than [`MAX_LINE_BYTES`] (a cut line is returned as soon as it is
/// cut; the rest of it is skipped).
#[derive(Debug, Default)]
pub struct LineSplitter {
    partial: Vec<u8>,
    /// Inside the rest of a cut line.
    skipping: bool,
}

impl LineSplitter {
    /// Feeds a chunk and returns the lines it completed.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        let mut lines = Vec::new();
        let mut rest = chunk;
        loop {
            let end = rest.iter().position(|&b| b == b'\n');
            let piece = &rest[..end.unwrap_or(rest.len())];
            if !self.skipping {
                let room = MAX_LINE_BYTES + 1 - self.partial.len();
                self.partial
                    .extend_from_slice(&piece[..piece.len().min(room)]);
                if self.partial.len() > MAX_LINE_BYTES {
                    self.partial.truncate(MAX_LINE_BYTES);
                    lines.push(Self::decode_cut(&std::mem::take(&mut self.partial)));
                    self.skipping = true;
                } else if end.is_some() {
                    lines.push(Self::decode(&std::mem::take(&mut self.partial)));
                }
            }
            let Some(end) = end else {
                break;
            };
            self.skipping = false;
            rest = &rest[end + 1..];
        }
        lines
    }

    /// The unterminated last line, if any.
    pub fn finish(&mut self) -> Option<String> {
        self.skipping = false;
        (!self.partial.is_empty()).then(|| Self::decode(&std::mem::take(&mut self.partial)))
    }

    fn decode(bytes: &[u8]) -> String {
        let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
        String::from_utf8_lossy(bytes).into_owned()
    }

    /// A line cut at [`MAX_LINE_BYTES`], without a character cut in half.
    fn decode_cut(bytes: &[u8]) -> String {
        let end = match std::str::from_utf8(bytes) {
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            _ => bytes.len(),
        };
        let mut text = String::from_utf8_lossy(&bytes[..end]).into_owned();
        text.push_str(CUT_LINE_MARK);
        text
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use k8s_openapi::api::core::v1::{
        ContainerState, ContainerStateRunning, ContainerStateTerminated, ContainerStateWaiting,
        PodSpec, PodStatus,
    };

    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn source(pod: &str) -> Arc<LogSource> {
        Arc::new(LogSource::new("ns", pod, "app"))
    }

    fn texts(event: &LogEvent) -> Vec<&str> {
        match event {
            LogEvent::Lines { lines, .. } => lines.iter().map(|l| l.text.as_str()).collect(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn splits_lines_across_chunks() {
        let mut splitter = LineSplitter::default();
        assert_eq!(splitter.push(b"one\ntw"), ["one"]);
        assert_eq!(splitter.push(b"o\r\nthree"), ["two"]);
        assert!(splitter.push(b"").is_empty());
        assert_eq!(splitter.push(b"\n\n"), ["three", ""]);
        assert_eq!(splitter.finish(), None);
        assert_eq!(splitter.push(b"tail"), Vec::<String>::new());
        assert_eq!(splitter.finish().as_deref(), Some("tail"));
    }

    #[test]
    fn decodes_invalid_utf8_lossily_even_when_split() {
        let mut splitter = LineSplitter::default();
        // "é" split between chunks stays intact, a stray byte becomes U+FFFD.
        assert!(splitter.push(&[b'a', 0xC3]).is_empty());
        assert_eq!(splitter.push(&[0xA9, 0xFF, b'\n']), ["aé\u{FFFD}"]);
    }

    #[test]
    fn cuts_overlong_lines() {
        let mut splitter = LineSplitter::default();
        let cut = format!("{}{CUT_LINE_MARK}", "x".repeat(MAX_LINE_BYTES));
        // A line of exactly the limit is kept whole.
        let exact = splitter.push(&[vec![b'y'; MAX_LINE_BYTES], b"\n".to_vec()].concat());
        assert_eq!(exact, ["y".repeat(MAX_LINE_BYTES)]);
        // A longer one comes out as soon as it is cut, its rest is skipped
        // (even across chunks) up to the end of the line.
        let lines = splitter.push(&vec![b'x'; MAX_LINE_BYTES * 2 + 3]);
        assert_eq!(lines, std::slice::from_ref(&cut));
        assert!(splitter.push(&vec![b'x'; MAX_LINE_BYTES]).is_empty());
        assert_eq!(splitter.push(b"xx\nnext\n"), ["next"]);
        assert_eq!(splitter.finish(), None);
        // A megabyte line in one chunk is one cut line.
        let mut big = vec![b'x'; 1024 * 1024];
        big.extend_from_slice(b"\nafter");
        assert_eq!(splitter.push(&big), [cut]);
        assert_eq!(splitter.finish().as_deref(), Some("after"));
    }

    #[test]
    fn cut_lines_keep_whole_characters() {
        let mut splitter = LineSplitter::default();
        // "é" (two bytes) straddles the limit.
        let mut line = vec![b'a'; MAX_LINE_BYTES - 1];
        line.extend_from_slice("é tail\n".as_bytes());
        let lines = splitter.push(&line);
        assert_eq!(
            lines,
            [format!("{}{CUT_LINE_MARK}", "a".repeat(MAX_LINE_BYTES - 1))]
        );
    }

    #[tokio::test]
    async fn fetched_lines_stop_at_the_byte_limit() {
        let source = Arc::new(LogSource::new("ns", "pod", "c"));
        let log =
            b"2024-05-01T10:00:00Z one\n2024-05-01T10:00:01Z two\n2024-05-01T10:00:02Z three\n";
        let whole = read_lines(&log[..], &source, 1024).await.unwrap();
        assert!(!whole.truncated);
        assert_eq!(whole.lines.len(), 3);
        // Cut inside the second line: only the complete first one is kept.
        let cut = read_lines(&log[..], &source, 30).await.unwrap();
        assert!(cut.truncated);
        let texts: Vec<&str> = cut.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["one"]);
        // A server ignoring `limitBytes` is cut here.
        let endless = futures::io::repeat(b'x');
        let cut = read_lines(endless, &source, 1024 * 1024).await.unwrap();
        assert!(cut.truncated);
        assert_eq!(cut.lines.len(), 1);
    }

    #[test]
    fn splits_timestamps() {
        let (time, text) = split_timestamp("2024-05-01T10:00:00.123456789Z hello world");
        assert_eq!(time, Some(ts("2024-05-01T10:00:00.123456789Z")));
        assert_eq!(text, "hello world");
        assert_eq!(split_timestamp("no timestamp"), (None, "no timestamp"));
        let (time, text) = split_timestamp("2024-05-01T10:00:00Z");
        assert_eq!((time.is_some(), text), (true, ""));
    }

    #[test]
    fn maps_options_to_params() {
        let options = LogOptions {
            follow: false,
            previous: true,
            tail_lines: Some(100),
            since: Some(LogSince::Seconds(60)),
            timestamps: false,
            limit_bytes: Some(4096),
            ..Default::default()
        };
        let params = options.to_params("web");
        assert_eq!(params.container.as_deref(), Some("web"));
        assert!(!params.follow && params.previous && params.timestamps);
        assert_eq!(params.tail_lines, Some(100));
        assert_eq!(params.since_seconds, Some(60));
        assert_eq!(params.since_time, None);
        assert_eq!(params.limit_bytes, Some(4096));

        let time = ts("2024-05-01T10:00:00Z");
        let options = LogOptions {
            since: Some(LogSince::Time(time)),
            ..Default::default()
        };
        let params = options.to_params("web");
        assert!(params.follow);
        assert_eq!(
            (params.since_seconds, params.since_time),
            (None, Some(time))
        );
    }

    #[test]
    fn resume_params_start_at_last_line() {
        let options = LogOptions {
            tail_lines: Some(10),
            since: Some(LogSince::Seconds(5)),
            ..Default::default()
        };
        let last = ts("2024-05-01T10:00:00Z");
        let params = options.resume_params("app", Some(last));
        assert_eq!(params.tail_lines, None);
        assert_eq!(params.since_seconds, None);
        assert_eq!(params.since_time, Some(last));
        assert_eq!(options.resume_params("app", None).tail_lines, Some(10));
    }

    #[test]
    fn resume_filter_skips_already_seen_lines() {
        let a = ts("2024-05-01T10:00:00Z");
        let b = ts("2024-05-01T10:00:01Z");
        let c = ts("2024-05-01T10:00:02Z");
        let mut filter = ResumeFilter::default();
        assert!(filter.accept(Some(a)));
        assert!(filter.accept(Some(b)));
        assert!(filter.accept(Some(b)));
        assert_eq!(filter.begin_resume(), Some(b));

        // The server resends from `b` (and maybe earlier in the same second).
        assert!(!filter.accept(Some(a)));
        assert!(!filter.accept(Some(b)));
        assert!(!filter.accept(Some(b)));
        assert!(filter.accept(Some(b)), "a third line at `b` is new");
        assert!(filter.accept(Some(c)));
        assert!(filter.accept(None));
    }

    #[tokio::test]
    async fn reads_fetched_lines_with_timestamps() {
        let input: &[u8] = b"2024-05-01T10:00:00Z one\n2024-05-01T10:00:01Z two\r\nthree";
        let lines = read_lines(futures::io::Cursor::new(input), &source("a"), 1024)
            .await
            .unwrap()
            .lines;
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["one", "two", "three"]);
        assert_eq!(lines[1].timestamp, Some(ts("2024-05-01T10:00:01Z")));
        assert_eq!(lines[2].timestamp, None);
    }

    async fn run_pump(input: &'static [u8], source: Arc<LogSource>, tx: mpsc::Sender<LogEvent>) {
        let mut outbox = Outbox::new(tx, source, 100);
        let mut resume = ResumeFilter::default();
        let reader = futures::io::Cursor::new(input);
        let result = pump(reader, &mut outbox, &mut resume, true).await.unwrap();
        assert!(matches!(result, Pumped::Eof { .. }));
    }

    #[tokio::test]
    async fn merges_and_tags_sources() {
        let (tx, rx) = mpsc::channel(4);
        let a = source("a");
        let b = source("b");
        run_pump(
            b"2024-05-01T10:00:00Z a1\n2024-05-01T10:00:01Z a2",
            a.clone(),
            tx.clone(),
        )
        .await;
        run_pump(b"2024-05-01T10:00:00Z b1\n", b.clone(), tx).await;

        let events: Vec<LogEvent> = rx.collect().await;
        let lines: Vec<(String, String)> = events
            .iter()
            .flat_map(|event| match event {
                LogEvent::Lines { lines, .. } => lines.clone(),
                _ => Vec::new(),
            })
            .map(|line| (line.source.label(), line.text))
            .collect();
        assert_eq!(
            lines,
            [
                ("a/app".to_owned(), "a1".to_owned()),
                ("a/app".to_owned(), "a2".to_owned()),
                ("b/app".to_owned(), "b1".to_owned()),
            ]
        );
        let LogEvent::Lines { lines, .. } = &events[0] else {
            panic!("expected lines");
        };
        assert_eq!(lines[0].timestamp, Some(ts("2024-05-01T10:00:00Z")));
    }

    #[test]
    fn outbox_drops_oldest_and_reports_count() {
        let (tx, _rx) = mpsc::channel(1);
        let src = source("a");
        let mut outbox = Outbox::new(tx, src.clone(), 3);
        for i in 0..5 {
            outbox.push(LogLine {
                source: src.clone(),
                timestamp: None,
                text: i.to_string(),
            });
        }
        let batch = outbox.take_batch();
        assert_eq!(texts(&batch), ["2", "3", "4"]);
        assert!(matches!(batch, LogEvent::Lines { dropped: 2, .. }));
        assert!(!outbox.has_pending());
    }

    /// A consumer that never reads still lets the producer finish reading,
    /// and the backlog reports what was lost.
    #[tokio::test]
    async fn slow_consumer_does_not_block_reading() {
        let (tx, mut rx) = mpsc::channel(0);
        let src = source("a");
        let input: Vec<u8> = (0..1000)
            .flat_map(|i| format!("{i}\n").into_bytes())
            .collect();
        let reader = futures::io::Cursor::new(input);
        let mut outbox = Outbox::new(tx, src, 10);
        let mut resume = ResumeFilter::default();
        let pumped = tokio::spawn(async move {
            let result = pump(reader, &mut outbox, &mut resume, true).await;
            drop(outbox);
            result
        });
        // Let the producer run ahead before draining.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut received = Vec::new();
        let mut dropped = 0;
        while let Some(event) = rx.next().await {
            if let LogEvent::Lines {
                lines, dropped: d, ..
            } = event
            {
                dropped += d;
                received.extend(lines.into_iter().map(|l| l.text));
            }
        }
        assert!(matches!(
            pumped.await.unwrap(),
            Ok(Pumped::Eof { lines: 1000 })
        ));
        assert_eq!(received.len() as u64 + dropped, 1000);
        assert!(dropped > 0);
        assert_eq!(received.last().map(String::as_str), Some("999"));
    }

    fn pod(phase: &str, state: ContainerState) -> Pod {
        Pod {
            status: Some(PodStatus {
                phase: Some(phase.to_owned()),
                container_statuses: Some(vec![ContainerStatus {
                    name: "app".to_owned(),
                    state: Some(state),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn container_liveness_follows_restart_policy() {
        let running = ContainerState {
            running: Some(ContainerStateRunning::default()),
            ..Default::default()
        };
        let terminated = ContainerState {
            terminated: Some(ContainerStateTerminated::default()),
            ..Default::default()
        };
        let waiting = ContainerState {
            waiting: Some(ContainerStateWaiting::default()),
            ..Default::default()
        };
        let live = |pod: &Pod| container_liveness(pod, "app");
        assert_eq!(live(&pod("Running", running.clone())), Liveness::Running);
        assert_eq!(live(&pod("Running", waiting)), Liveness::Restarting);
        assert_eq!(
            container_liveness(&pod("Running", running.clone()), "other"),
            Liveness::Gone
        );
        assert_eq!(live(&pod("Succeeded", running)), Liveness::Gone);

        // A terminated container is restarted by the default `Always` policy.
        let mut restarting = pod("Running", terminated.clone());
        assert_eq!(live(&restarting), Liveness::Restarting);
        restarting.spec = Some(PodSpec {
            restart_policy: Some("OnFailure".into()),
            ..Default::default()
        });
        assert_eq!(live(&restarting), Liveness::Gone, "exit code 0");
        restarting.spec = Some(PodSpec {
            restart_policy: Some("Never".into()),
            ..Default::default()
        });
        assert_eq!(live(&restarting), Liveness::Gone);
    }
}
