//! Loading log lines older than the ones already streamed.
//!
//! A log tab starts with the last [`INITIAL_TAIL_LINES`] lines of every source
//! and follows them. The API cannot fetch "the lines before line N", so earlier
//! lines are fetched with a larger `tailLines` (no follow) and only the lines
//! older than the oldest known one are kept: they are matched by timestamp,
//! and lines sharing the oldest timestamp by their text and order.
//!
//! With several sources, lines older than what every other source has been
//! loaded to are held back until the others catch up, so the lines shown
//! above the already shown ones stay merged by time.
//!
//! Loading stops at [`MAX_HISTORY_LINES`] lines or [`MAX_HISTORY_BYTES`] of
//! text, whichever comes first (#195). Each fetch is limited in bytes too;
//! the server counts them from the oldest line of the tail, so a cut answer
//! still has the lines being loaded unless they alone do not fit.

use std::sync::Arc;

use k8s_openapi::jiff::Timestamp;
use kube::Client;

use super::StreamError;
use super::logs::{FetchedLines, LogLine, LogSource, fetch_log_lines};

/// Lines per source requested when a log stream starts.
pub const INITIAL_TAIL_LINES: i64 = 200;
/// No earlier lines are loaded once this many lines are known.
pub const MAX_HISTORY_LINES: usize = 50_000;
/// No earlier lines are loaded once the known lines take this many bytes.
pub const MAX_HISTORY_BYTES: usize = 32 * 1024 * 1024;
/// Earlier lines requested per load (per source), growing with each load.
const STEPS: [usize; 3] = [1_000, 5_000, 20_000];
/// Bytes a line takes in the server's answer beside its text: the RFC 3339
/// timestamp with nanoseconds, a space and the newline.
const LINE_OVERHEAD: usize = 32;
/// Extra bytes asked for per fetch, as the known lines' size is estimated.
const FETCH_MARGIN: usize = 256 * 1024;

/// Whether earlier lines can be loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryState {
    /// Some source has earlier lines to load.
    More,
    /// Every source is shown from its first line.
    Beginning,
    /// [`MAX_HISTORY_LINES`] reached; older lines are not loaded.
    Capped,
    /// Loading is turned off (lines were cleared or evicted).
    Off,
}

/// One fetch of earlier lines: the last `tail_lines` lines of `source`, at
/// most `limit_bytes` of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryRequest {
    pub source: Arc<LogSource>,
    pub tail_lines: i64,
    pub limit_bytes: usize,
}

#[derive(Debug)]
struct SourceHistory {
    source: Arc<LogSource>,
    /// Lines known from the oldest one to now (shown, held back or dropped
    /// by the stream): the `tailLines` that reaches back to the oldest line.
    known: usize,
    /// Estimated size of the known lines in the server's answer.
    known_bytes: usize,
    /// Timestamp of the oldest known line.
    oldest_at: Option<Timestamp>,
    /// Texts of the oldest known lines sharing `oldest_at`, in order.
    oldest: Vec<String>,
    /// Every known line shares `oldest_at`, so the group may still grow.
    oldest_open: bool,
    /// Loaded lines not shown yet, oldest first.
    held_back: Vec<LogLine>,
    /// Loaded back to the first line.
    exhausted: bool,
}

impl SourceHistory {
    fn new(source: Arc<LogSource>) -> Self {
        Self {
            source,
            known: 0,
            known_bytes: 0,
            oldest_at: None,
            oldest: Vec::new(),
            oldest_open: true,
            held_back: Vec::new(),
            exhausted: false,
        }
    }

    /// Nothing more to fetch: loaded to the start, the first lines did not
    /// fill the initial tail (so they began the log), or no timestamp to
    /// compare against.
    fn done(&self, initial_tail: usize) -> bool {
        self.exhausted || self.oldest_at.is_none() || self.known < initial_tail
    }

    /// Records a line newer than every known one.
    fn observe(&mut self, line: &LogLine) {
        if self.oldest.is_empty() {
            self.oldest_at = line.timestamp;
            self.oldest.push(line.text.clone());
            self.oldest_open = true;
        } else if self.oldest_open {
            if line.timestamp.is_none() || line.timestamp == self.oldest_at {
                self.oldest.push(line.text.clone());
            } else {
                self.oldest_open = false;
            }
        }
    }

    /// Records `older` (oldest first, all older than every known line).
    fn prepend(&mut self, older: Vec<LogLine>) {
        if older.is_empty() {
            return;
        }
        let keys = keys(&older);
        let first = keys[0];
        let group = keys.iter().take_while(|key| **key == first).count();
        let texts = older[..group].iter().map(|line| line.text.clone());
        if group == older.len() && first == self.oldest_at {
            // Still inside the oldest timestamp.
            self.oldest.splice(0..0, texts);
        } else {
            self.oldest = texts.collect();
            self.oldest_at = first;
            self.oldest_open = false;
        }
        self.known += older.len();
        self.known_bytes += size(&older);
        self.held_back.splice(0..0, older);
    }
}

/// Earlier-lines state of one log tab.
#[derive(Debug)]
pub struct LogHistory {
    sources: Vec<SourceHistory>,
    initial_tail: usize,
    loads: usize,
    capped: bool,
    off: bool,
}

impl LogHistory {
    /// For streams started with `tailLines = initial_tail`.
    pub fn new(sources: &[Arc<LogSource>], initial_tail: i64) -> Self {
        Self {
            sources: sources.iter().cloned().map(SourceHistory::new).collect(),
            initial_tail: initial_tail.max(0) as usize,
            loads: 0,
            capped: false,
            off: false,
        }
    }

    fn find(&mut self, source: &LogSource) -> Option<&mut SourceHistory> {
        self.sources.iter_mut().find(|s| *s.source == *source)
    }

    /// Records streamed lines of `source` and the lines the stream dropped
    /// before them.
    pub fn observe(&mut self, source: &LogSource, lines: &[LogLine], dropped: u64) {
        let Some(history) = self.find(source) else {
            return;
        };
        history.known += lines.len() + dropped as usize;
        history.known_bytes += size(lines);
        for line in lines {
            history.observe(line);
        }
    }

    /// Stops loading (the shown lines no longer reach back to the oldest
    /// known line).
    pub fn turn_off(&mut self) {
        self.off = true;
    }

    /// Lines known over all sources.
    pub fn known(&self) -> usize {
        self.sources.iter().map(|s| s.known).sum()
    }

    /// Estimated bytes of the known lines over all sources.
    pub fn known_bytes(&self) -> usize {
        self.sources.iter().map(|s| s.known_bytes).sum()
    }

    fn over_limits(&self) -> bool {
        self.known() >= MAX_HISTORY_LINES || self.known_bytes() >= MAX_HISTORY_BYTES
    }

    pub fn state(&self) -> HistoryState {
        if self.off {
            HistoryState::Off
        } else if self.sources.iter().all(|s| s.done(self.initial_tail)) {
            HistoryState::Beginning
        } else if self.capped {
            HistoryState::Capped
        } else {
            HistoryState::More
        }
    }

    /// Requests for the next load, one per source with earlier lines; empty
    /// when there is nothing to load. Each load asks for more lines.
    pub fn plan(&mut self) -> Vec<HistoryRequest> {
        if self.state() != HistoryState::More {
            return Vec::new();
        }
        if self.over_limits() {
            self.capped = true;
            return Vec::new();
        }
        let known = self.known();
        let step = STEPS[self.loads.min(STEPS.len() - 1)].min(MAX_HISTORY_LINES - known);
        self.loads += 1;
        let initial_tail = self.initial_tail;
        let fetching = self
            .sources
            .iter()
            .filter(|s| !s.done(initial_tail))
            .count()
            .max(1);
        // The earlier lines of all sources share what is left of the byte
        // budget; the known lines (the end of each answer) come on top.
        let share = (MAX_HISTORY_BYTES - self.known_bytes()) / fetching;
        self.sources
            .iter()
            .filter(|s| !s.done(initial_tail))
            .map(|s| HistoryRequest {
                source: s.source.clone(),
                tail_lines: (s.known + step) as i64,
                limit_bytes: share + s.known_bytes + FETCH_MARGIN,
            })
            .collect()
    }

    /// Takes the fetched lines (each the answer to a [`plan`](Self::plan)
    /// request) and returns the lines to show above the shown ones, oldest
    /// first, merged by time.
    pub fn apply(&mut self, fetched: Vec<(HistoryRequest, FetchedLines)>) -> Vec<LogLine> {
        if self.off {
            return Vec::new();
        }
        let mut gap = false;
        for (request, FetchedLines { lines, truncated }) in fetched {
            let Some(history) = self.find(&request.source) else {
                continue;
            };
            if history.exhausted {
                continue;
            }
            if truncated {
                // Cut by the byte limit: usable only if the answer still
                // reaches past the oldest known lines, else there would be
                // a gap between what it has and what is shown.
                let reaches = match history.oldest_at {
                    Some(at) => keys(&lines).iter().any(|key| *key > Some(at)),
                    None => false,
                };
                if !reaches {
                    gap = true;
                    continue;
                }
            } else if (lines.len() as i64) < request.tail_lines {
                // Fewer lines than asked for: the server sent the whole log.
                history.exhausted = true;
            }
            let older = older_lines(lines, history.oldest_at, &history.oldest);
            history.prepend(older);
        }
        if gap || self.over_limits() {
            self.capped = true;
        }
        let cutoff = self.cutoff();
        let released: Vec<Vec<LogLine>> = self
            .sources
            .iter_mut()
            .map(|history| {
                let keys = keys(&history.held_back);
                let start = match cutoff {
                    Some(cutoff) => keys.partition_point(|key| *key < Some(cutoff)),
                    None => 0,
                };
                history.held_back.split_off(start)
            })
            .collect();
        if self.capped {
            // Not loaded any further: what is held back would end up out of
            // order.
            for history in &mut self.sources {
                history.held_back.clear();
            }
        }
        merge_by_time(released)
    }

    /// Lines older than this are held back: some source with earlier lines
    /// has not been loaded that far back yet. `None` releases everything.
    fn cutoff(&self) -> Option<Timestamp> {
        self.sources
            .iter()
            .filter(|s| !s.done(self.initial_tail))
            .filter_map(|s| s.oldest_at)
            .max()
    }
}

/// Runs `requests` concurrently (previous container instances when
/// `previous`), for [`LogHistory::apply`]. Must run on the
/// [`runtime`](crate::runtime).
pub async fn fetch_history(
    client: &Client,
    requests: Vec<HistoryRequest>,
    previous: bool,
) -> Vec<(HistoryRequest, Result<FetchedLines, StreamError>)> {
    let fetches = requests.into_iter().map(|request| async move {
        let lines = fetch_log_lines(
            client,
            request.source.clone(),
            previous,
            request.tail_lines,
            request.limit_bytes,
        )
        .await;
        (request, lines)
    });
    futures::future::join_all(fetches).await
}

/// Estimated size of `lines` in the server's answer.
fn size(lines: &[LogLine]) -> usize {
    lines
        .iter()
        .map(|line| line.text.len() + LINE_OVERHEAD)
        .sum()
}

/// The time each line sorts by: its timestamp, or the previous line's (the
/// next one's for leading lines without one).
fn keys(lines: &[LogLine]) -> Vec<Option<Timestamp>> {
    let mut last = lines.iter().find_map(|line| line.timestamp);
    lines
        .iter()
        .map(|line| {
            if line.timestamp.is_some() {
                last = line.timestamp;
            }
            last
        })
        .collect()
}

/// The lines of `fetched` (the tail of a log, oldest first) older than the
/// oldest known line. `oldest` are the texts of the known lines at
/// `oldest_at`, in order: fetched lines at that time count as older only when
/// they come before a run of exactly these texts. Without that run only the
/// strictly older lines are kept, so nothing is ever duplicated.
pub fn older_lines(
    mut fetched: Vec<LogLine>,
    oldest_at: Option<Timestamp>,
    oldest: &[String],
) -> Vec<LogLine> {
    let Some(at) = oldest_at else {
        return Vec::new();
    };
    let keys = keys(&fetched);
    let before = keys
        .iter()
        .position(|key| *key >= Some(at))
        .unwrap_or(keys.len());
    let group = keys[before..]
        .iter()
        .take_while(|key| **key == Some(at))
        .count();
    let candidates = &fetched[before..before + group];
    let known = oldest.len();
    let start = (0..=candidates.len().saturating_sub(known))
        .rev()
        .find(|&start| {
            start + known <= candidates.len()
                && candidates[start..start + known]
                    .iter()
                    .map(|line| &line.text)
                    .eq(oldest.iter())
        })
        .unwrap_or(0);
    fetched.truncate(before + start);
    fetched
}

/// Merges per-source runs (each oldest first) by time; equal times keep the
/// source order.
pub fn merge_by_time(runs: Vec<Vec<LogLine>>) -> Vec<LogLine> {
    let total = runs.iter().map(Vec::len).sum();
    let keyed: Vec<(Vec<Option<Timestamp>>, std::vec::IntoIter<LogLine>)> = runs
        .into_iter()
        .map(|run| (keys(&run), run.into_iter()))
        .collect();
    let mut runs = keyed;
    let mut at = vec![0usize; runs.len()];
    let mut merged = Vec::with_capacity(total);
    loop {
        let next = (0..runs.len())
            .filter(|&ix| at[ix] < runs[ix].0.len())
            .min_by_key(|&ix| (runs[ix].0[at[ix]], ix));
        let Some(ix) = next else {
            break;
        };
        at[ix] += 1;
        merged.extend(runs[ix].1.next());
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(container: &str) -> Arc<LogSource> {
        Arc::new(LogSource::new("ns", "pod", container))
    }

    /// Second `s` of the day as a timestamp.
    fn at(s: i64) -> Timestamp {
        Timestamp::from_second(1_700_000_000 + s).unwrap()
    }

    fn line(source: &Arc<LogSource>, s: i64, text: &str) -> LogLine {
        LogLine {
            source: source.clone(),
            timestamp: Some(at(s)),
            text: text.to_owned(),
        }
    }

    fn texts(lines: &[LogLine]) -> Vec<&str> {
        lines.iter().map(|l| l.text.as_str()).collect()
    }

    #[test]
    fn keeps_strictly_older_lines() {
        let a = source("a");
        let fetched = vec![line(&a, 1, "x"), line(&a, 2, "y"), line(&a, 3, "z")];
        let older = older_lines(fetched, Some(at(3)), &["z".into()]);
        assert_eq!(texts(&older), ["x", "y"]);
    }

    #[test]
    fn splits_a_shared_timestamp_by_text_and_order() {
        let a = source("a");
        // Known lines start in the middle of the lines at second 2.
        let fetched = vec![
            line(&a, 1, "x"),
            line(&a, 2, "p"),
            line(&a, 2, "q"),
            line(&a, 2, "p"),
            line(&a, 2, "q"),
            line(&a, 3, "z"),
        ];
        let older = older_lines(fetched.clone(), Some(at(2)), &["p".into(), "q".into()]);
        assert_eq!(texts(&older), ["x", "p", "q"]);
        let older = older_lines(
            fetched.clone(),
            Some(at(2)),
            &["q".into(), "p".into(), "q".into()],
        );
        assert_eq!(texts(&older), ["x", "p"]);
        // No matching run: only the strictly older lines.
        let older = older_lines(fetched, Some(at(2)), &["other".into()]);
        assert_eq!(texts(&older), ["x"]);
    }

    #[test]
    fn lines_without_timestamps_follow_their_line() {
        let a = source("a");
        let mut continuation = line(&a, 0, "rest of x");
        continuation.timestamp = None;
        let fetched = vec![line(&a, 1, "x"), continuation, line(&a, 2, "y")];
        let older = older_lines(fetched, Some(at(2)), &["y".into()]);
        assert_eq!(texts(&older), ["x", "rest of x"]);
        assert!(older_lines(vec![line(&a, 1, "x")], None, &[]).is_empty());
    }

    #[test]
    fn merges_runs_by_time() {
        let a = source("a");
        let b = source("b");
        let merged = merge_by_time(vec![
            vec![line(&a, 1, "a1"), line(&a, 3, "a3")],
            vec![line(&b, 1, "b1"), line(&b, 2, "b2"), line(&b, 4, "b4")],
        ]);
        assert_eq!(texts(&merged), ["a1", "b1", "b2", "a3", "b4"]);
    }

    #[test]
    fn short_first_tail_is_the_beginning() {
        let a = source("a");
        let mut history = LogHistory::new(std::slice::from_ref(&a), 200);
        history.observe(&a, &[line(&a, 1, "only")], 0);
        assert_eq!(history.state(), HistoryState::Beginning);
        assert!(history.plan().is_empty());
    }

    /// A fake log of `count` lines, several per second.
    fn log(source: &Arc<LogSource>, count: usize) -> Vec<LogLine> {
        (0..count)
            .map(|n| line(source, n as i64 / 3, &format!("line {}", n % 2)))
            .collect()
    }

    fn tail(log: &[LogLine], lines: i64) -> Vec<LogLine> {
        log[log.len().saturating_sub(lines as usize)..].to_vec()
    }

    #[test]
    fn loads_back_to_the_first_line_without_gaps_or_duplicates() {
        let a = source("a");
        let mut full = log(&a, 7_000);
        let mut history = LogHistory::new(std::slice::from_ref(&a), 200);
        let mut shown = tail(&full, 200);
        history.observe(&a, &shown, 0);
        let mut tails = Vec::new();
        while history.state() == HistoryState::More {
            let requests = history.plan();
            assert_eq!(requests.len(), 1);
            tails.push(requests[0].tail_lines);
            // Lines keep arriving while the request runs.
            let live: Vec<LogLine> = (0..7)
                .map(|n| line(&a, 10_000 + full.len() as i64, &format!("live {n}")))
                .collect();
            full.extend(live.iter().cloned());
            let fetched = tail(&full, requests[0].tail_lines);
            history.observe(&a, &live, 0);
            shown.extend(live);
            let older = history.apply(vec![(requests[0].clone(), fetched.into())]);
            shown.splice(0..0, older);
        }
        assert_eq!(history.state(), HistoryState::Beginning);
        assert_eq!(shown, full);
        assert_eq!(tails, [1_200, 6_200, 26_200]);
    }

    #[test]
    fn merges_sources_loaded_to_the_beginning() {
        let a = source("a");
        let b = source("b");
        let mut history = LogHistory::new(&[a.clone(), b.clone()], 2);
        // `a` logs every second, `b` every 10 seconds.
        let log_a: Vec<LogLine> = (0..100).map(|s| line(&a, s, &format!("a{s}"))).collect();
        let log_b: Vec<LogLine> = (0..10)
            .map(|n| line(&b, n * 10, &format!("b{}", n * 10)))
            .collect();
        history.observe(&a, &tail(&log_a, 2), 0);
        history.observe(&b, &tail(&log_b, 2), 0);

        let requests = history.plan();
        assert_eq!(requests.len(), 2);
        let answer = |requests: Vec<HistoryRequest>| {
            requests
                .into_iter()
                .map(|request| {
                    let log = if *request.source == *a {
                        &log_a
                    } else {
                        &log_b
                    };
                    let lines = tail(log, request.tail_lines);
                    (request, lines.into())
                })
                .collect::<Vec<_>>()
        };
        let older = history.apply(answer(requests));
        // Both answered with fewer lines than asked: fully loaded, so
        // everything is released, merged by time.
        assert_eq!(history.state(), HistoryState::Beginning);
        assert_eq!(older.len(), 98 + 8);
        let times: Vec<Timestamp> = older.iter().map(|l| l.timestamp.unwrap()).collect();
        assert!(times.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn lagging_source_holds_back_the_others() {
        let a = source("a");
        let b = source("b");
        let mut history = LogHistory::new(&[a.clone(), b.clone()], 2);
        history.observe(&a, &[line(&a, 100, "a100"), line(&a, 101, "a101")], 0);
        history.observe(&b, &[line(&b, 100, "b100"), line(&b, 101, "b101")], 0);
        let requests = history.plan();
        let request =
            |s: &Arc<LogSource>| requests.iter().find(|r| *r.source == **s).unwrap().clone();
        let (ra, rb) = (request(&a), request(&b));
        // Full answers: `a` reaches back to 50, `b` only to 90.
        let spread = ra.tail_lines - 2;
        let mut fetched_a: Vec<LogLine> = (0..spread)
            .map(|n| line(&a, 50 + n * 50 / spread, "a"))
            .collect();
        fetched_a.extend([line(&a, 100, "a100"), line(&a, 101, "a101")]);
        let mut fetched_b = vec![line(&b, 90, "b90"); rb.tail_lines as usize - 2];
        fetched_b.extend([line(&b, 100, "b100"), line(&b, 101, "b101")]);
        let older = history.apply(vec![(ra, fetched_a.into()), (rb, fetched_b.into())]);
        assert_eq!(history.state(), HistoryState::More);
        assert!(older.iter().all(|l| l.timestamp.unwrap() >= at(90)));
        assert!(older.iter().any(|l| *l.source == *a));
        assert!(older.iter().any(|l| *l.source == *b));
        assert!(!history.sources[0].held_back.is_empty());
    }

    #[test]
    fn stops_at_the_cap() {
        let a = source("a");
        let full = log(&a, MAX_HISTORY_LINES * 2);
        let mut history = LogHistory::new(std::slice::from_ref(&a), 200);
        let mut shown = tail(&full, 200);
        history.observe(&a, &shown, 0);
        let mut loads = 0;
        while history.state() == HistoryState::More {
            let requests = history.plan();
            if requests.is_empty() {
                break;
            }
            let fetched = tail(&full, requests[0].tail_lines);
            shown.splice(
                0..0,
                history.apply(vec![(requests[0].clone(), fetched.into())]),
            );
            loads += 1;
        }
        assert_eq!(history.state(), HistoryState::Capped);
        assert!(loads > 2);
        assert_eq!(shown.len(), MAX_HISTORY_LINES);
        assert_eq!(shown[..], full[full.len() - MAX_HISTORY_LINES..]);
    }

    /// The answer to `request` from `log` as a server with `limitBytes`
    /// gives it: the tail, cut after `limit_bytes` from its start.
    fn answer(log: &[LogLine], request: &HistoryRequest) -> FetchedLines {
        let mut lines = Vec::new();
        let mut bytes = 0;
        for line in tail(log, request.tail_lines) {
            bytes += size(std::slice::from_ref(&line));
            if bytes > request.limit_bytes {
                return FetchedLines {
                    lines,
                    truncated: true,
                };
            }
            lines.push(line);
        }
        lines.into()
    }

    #[test]
    fn stops_at_the_byte_cap() {
        let a = source("a");
        // 64 KiB lines: the byte budget runs out long before the line cap.
        let long = "x".repeat(64 * 1024);
        let full: Vec<LogLine> = (0..2_000)
            .map(|n| line(&a, n as i64 / 3, &format!("{n} {long}")))
            .collect();
        let mut history = LogHistory::new(std::slice::from_ref(&a), 200);
        let mut shown = tail(&full, 200);
        history.observe(&a, &shown, 0);
        while history.state() == HistoryState::More {
            let requests = history.plan();
            if requests.is_empty() {
                break;
            }
            assert!(requests[0].limit_bytes <= 2 * MAX_HISTORY_BYTES);
            let fetched = answer(&full, &requests[0]);
            shown.splice(0..0, history.apply(vec![(requests[0].clone(), fetched)]));
        }
        assert_eq!(history.state(), HistoryState::Capped);
        assert!(history.known_bytes() <= MAX_HISTORY_BYTES + 64 * 1024 + 1024);
        // What is shown is the end of the log, without gaps.
        assert_eq!(shown[..], full[full.len() - shown.len()..]);
    }

    #[test]
    fn cut_answer_without_the_known_lines_is_not_used() {
        let a = source("a");
        let full = log(&a, 5_000);
        let mut history = LogHistory::new(std::slice::from_ref(&a), 200);
        history.observe(&a, &tail(&full, 200), 0);
        let requests = history.plan();
        // Cut long before the oldest known line: using it would leave a gap.
        let fetched = FetchedLines {
            lines: tail(&full, requests[0].tail_lines)[..10].to_vec(),
            truncated: true,
        };
        assert!(
            history
                .apply(vec![(requests[0].clone(), fetched)])
                .is_empty()
        );
        assert_eq!(history.state(), HistoryState::Capped);
    }

    #[test]
    fn cut_answer_reaching_the_known_lines_is_used() {
        let a = source("a");
        let full = log(&a, 5_000);
        let mut history = LogHistory::new(std::slice::from_ref(&a), 200);
        history.observe(&a, &tail(&full, 200), 0);
        let requests = history.plan();
        // Cut inside the known lines: the earlier ones are all there.
        let all = tail(&full, requests[0].tail_lines);
        let fetched = FetchedLines {
            lines: all[..all.len() - 100].to_vec(),
            truncated: true,
        };
        let older = history.apply(vec![(requests[0].clone(), fetched)]);
        assert_eq!(older[..], all[..all.len() - 200]);
        // Not taken for the beginning of the log.
        assert_eq!(history.state(), HistoryState::More);
    }

    #[test]
    fn turned_off_loads_nothing() {
        let a = source("a");
        let mut history = LogHistory::new(std::slice::from_ref(&a), 2);
        history.observe(&a, &[line(&a, 1, "x"), line(&a, 2, "y")], 0);
        history.turn_off();
        assert_eq!(history.state(), HistoryState::Off);
        assert!(history.plan().is_empty());
    }
}
