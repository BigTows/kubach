//! Bounded line storage and search for [`LogView`](super::LogView).

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::ops::Range;

use gpui_kit::SharedString;
use jiff::Timestamp;
use unicode_width::UnicodeWidthStr as _;

use crate::StyledSpan;

/// Source label shown before a line when several sources are merged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogTag {
    /// Lines of one source should share one label (a clone of the same
    /// `SharedString`), so a line does not pay for its own copy: a label
    /// longer than 23 bytes is an allocation (shorter ones are kept inline).
    pub label: SharedString,
    /// Index into the tag palette; equal indices get equal colors.
    pub color: usize,
}

/// One line of a [`LogView`](super::LogView).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    pub tag: Option<LogTag>,
    /// When the line was written, in milliseconds since the Unix epoch
    /// ([`NO_TIME`]: unknown). Eight bytes instead of the text of the
    /// timestamp: it is formatted for the lines that are drawn.
    millis: i64,
    /// Plain text: no escape sequences, so search and copy see what is shown.
    pub text: SharedString,
    /// Styled ranges of `text` (program output colors), in order.
    pub spans: Box<[StyledSpan]>,
}

/// [`LogEntry::millis`] of a line without a timestamp.
const NO_TIME: i64 = i64::MIN;

/// What the text of a line takes beside its bytes: the counters of its
/// shared allocation. (Texts of up to 23 bytes are kept inline and take
/// nothing; they are counted the same, which only errs on the safe side.)
const TEXT_HEADER: usize = 2 * size_of::<usize>();

impl LogEntry {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            tag: None,
            millis: NO_TIME,
            text: text.into(),
            spans: Box::default(),
        }
    }

    /// Colors parts of the text; ranges are bytes of `text`.
    pub fn spans(mut self, spans: Vec<StyledSpan>) -> Self {
        self.spans = spans.into_boxed_slice();
        self
    }

    pub fn tag(mut self, label: impl Into<SharedString>, color: usize) -> Self {
        self.tag = Some(LogTag {
            label: label.into(),
            color,
        });
        self
    }

    /// When the line was written; kept to the millisecond, as it is shown.
    pub fn timestamp(mut self, timestamp: Timestamp) -> Self {
        self.millis = timestamp.as_millisecond();
        self
    }

    /// When the line was written, to the millisecond.
    pub fn time(&self) -> Option<Timestamp> {
        if self.millis == NO_TIME {
            return None;
        }
        Timestamp::from_millisecond(self.millis).ok()
    }

    /// Bytes the line holds in a buffer: the entry, its text and its spans.
    /// The tag label is not counted: lines of a source share it.
    pub fn bytes(&self) -> usize {
        size_of::<Self>() + TEXT_HEADER + self.text.len() + size_of_val(&*self.spans)
    }

    /// Terminal cells the tag and the text take: an estimate of how wide the
    /// line is drawn, to compare lines without shaping them.
    fn cells(&self) -> usize {
        let tag = self.tag.as_ref().map_or(0, |tag| cells(&tag.label) + 1);
        tag + cells(&self.text)
    }
}

fn cells(text: &str) -> usize {
    if text.is_ascii() {
        text.len()
    } else {
        text.width()
    }
}

/// Appends a timestamp the way lines show it: UTC with milliseconds.
pub(super) fn write_timestamp(line: &mut String, timestamp: Timestamp) {
    // Writing into a `String` does not fail.
    let _ = write!(line, "{}", timestamp.strftime("%Y-%m-%dT%H:%M:%S%.3fZ"));
}

/// Lines in arrival order, dropping the oldest beyond `capacity` lines or
/// beyond a limit of bytes ([`with_max_bytes`](Self::with_max_bytes)).
///
/// Every line gets an id that never changes (`first_id() + index`), so search
/// matches and selections survive eviction from the front and lines inserted
/// before the first one ([`prepend`](Self::prepend) counts ids down from a
/// large start, so they stay unique and ordered).
#[derive(Debug)]
pub struct LogBuffer {
    lines: VecDeque<LogEntry>,
    capacity: usize,
    max_bytes: usize,
    /// Sum of [`LogEntry::bytes`] of the lines.
    bytes: usize,
    first_id: u64,
    /// `(id, cells)` of every line wider than all lines after it, in line
    /// order: the front is the widest buffered line. A line that is not
    /// here has a later line at least as wide, which outlives it, so both
    /// appending and evicting keep the maximum in constant time.
    widest: VecDeque<(u64, usize)>,
}

/// Id of the first line pushed into an empty buffer; prepended lines count
/// down from it.
const FIRST_ID: u64 = 1 << 48;

/// What [`LogBuffer::push`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PushResult {
    pub added: usize,
    /// Old lines dropped from the front to stay within the limits.
    pub evicted: usize,
}

impl LogBuffer {
    /// A buffer of at most `capacity` lines, of any size in bytes.
    pub fn new(capacity: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            capacity: capacity.max(1),
            max_bytes: usize::MAX,
            bytes: 0,
            first_id: FIRST_ID,
            widest: VecDeque::new(),
        }
    }

    /// Also drops the oldest lines while the buffered ones hold more than
    /// `max_bytes` ([`LogEntry::bytes`]). The newest line is always kept,
    /// even when it alone is larger.
    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Bytes the buffered lines hold ([`LogEntry::bytes`]).
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Id of the line at index 0.
    pub fn first_id(&self) -> u64 {
        self.first_id
    }

    /// Id the next pushed line will get.
    pub fn end_id(&self) -> u64 {
        self.first_id + self.lines.len() as u64
    }

    pub fn get(&self, index: usize) -> Option<&LogEntry> {
        self.lines.get(index)
    }

    /// Index of the line with `id`, if it is still buffered.
    pub fn index_of(&self, id: u64) -> Option<usize> {
        let index = id.checked_sub(self.first_id)?;
        (index < self.lines.len() as u64).then_some(index as usize)
    }

    pub fn iter(&self) -> impl Iterator<Item = &LogEntry> {
        self.lines.iter()
    }

    /// Id of the buffered line that takes the most cells (tag and text; the
    /// latest of equally wide ones).
    pub fn widest(&self) -> Option<u64> {
        self.widest.front().map(|&(id, _)| id)
    }

    /// Appends lines, then evicts the oldest ones until the buffer is within
    /// both limits.
    pub fn push(&mut self, entries: impl IntoIterator<Item = LogEntry>) -> PushResult {
        let mut added = 0;
        for entry in entries {
            let (id, cells) = (self.end_id(), entry.cells());
            while self.widest.back().is_some_and(|&(_, back)| back <= cells) {
                self.widest.pop_back();
            }
            self.widest.push_back((id, cells));
            self.bytes += entry.bytes();
            self.lines.push_back(entry);
            added += 1;
        }
        let mut evicted = 0;
        while self.lines.len() > self.capacity
            || (self.bytes > self.max_bytes && self.lines.len() > 1)
        {
            let Some(entry) = self.lines.pop_front() else {
                break;
            };
            self.bytes -= entry.bytes();
            evicted += 1;
        }
        self.first_id += evicted as u64;
        while self
            .widest
            .front()
            .is_some_and(|&(id, _)| id < self.first_id)
        {
            self.widest.pop_front();
        }
        PushResult { added, evicted }
    }

    /// Inserts lines (oldest first) before the first one, as many as fit
    /// within the limits (the newest of them); returns how many were added.
    pub fn prepend(&mut self, entries: Vec<LogEntry>) -> usize {
        let mut added = 0;
        for entry in entries.into_iter().rev() {
            let bytes = entry.bytes();
            let fits = self.lines.len() < self.capacity
                && self.first_id > 0
                && (self.lines.is_empty() || self.bytes + bytes <= self.max_bytes);
            if !fits {
                break;
            }
            self.first_id -= 1;
            let cells = entry.cells();
            // Only a line wider than everything after it matters.
            if self.widest.front().is_none_or(|&(_, front)| front < cells) {
                self.widest.push_front((self.first_id, cells));
            }
            self.bytes += bytes;
            self.lines.push_front(entry);
            added += 1;
        }
        added
    }

    /// Removes every line; ids keep counting up.
    pub fn clear(&mut self) {
        self.first_id = self.end_id();
        self.lines.clear();
        self.widest.clear();
        self.bytes = 0;
    }
}

/// A search hit: a byte range in the text of line `line` (an id).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogMatch {
    pub line: u64,
    pub range: Range<usize>,
}

/// A line with matches and how many it has.
#[derive(Clone, Copy, Debug)]
struct Hit {
    line: u64,
    count: u32,
}

/// The current match: the `nth` one of `line`, `index` among all matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Current {
    line: u64,
    nth: u32,
    index: usize,
}

/// A step that needs lines not searched yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wanted {
    Next,
    Prev,
}

/// What [`LogSearch::step`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanStep {
    /// Every buffered line is searched.
    pub complete: bool,
    /// A step to the next or previous match that was waiting for more lines
    /// was made: the current match changed.
    pub moved: bool,
}

/// What a searched line costs beside its text, in the units of the budget
/// of [`LogSearch::step`].
const LINE_COST: usize = 64;

/// Matches of a query over a [`LogBuffer`], with a current match for
/// next/previous navigation.
///
/// The buffer is searched in pieces ([`step`](Self::step)), oldest line
/// first, so a keystroke on a full buffer does not hold the UI thread
/// (#171): [`set_query`](Self::set_query) only forgets the old matches. The
/// lines up to some id are searched and the matches in them are all known,
/// so the position of the current match is exact at any moment; the total
/// grows until the search is complete. A step past the last match found so
/// far (or back from the first one) waits for the rest of the search, as
/// the answer depends on it.
///
/// Per line with matches only their number is kept (16 bytes; the ranges
/// are found again in the few lines that are drawn or jumped to), so the
/// memory is bounded by the lines of the buffer, not by the matches.
#[derive(Debug, Default)]
pub struct LogSearch {
    query: String,
    /// Lines with matches among the searched ones, in line order.
    hits: VecDeque<Hit>,
    /// Sum of the counts of `hits`.
    total: usize,
    /// Lines with smaller ids are searched.
    scanned_to: u64,
    current: Option<Current>,
    wanted: Option<Wanted>,
}

impl LogSearch {
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Matches found so far; all of them once the search
    /// [is complete](Self::is_complete).
    pub fn len(&self) -> usize {
        self.total
    }

    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// Whether every buffered line is searched.
    pub fn is_complete(&self, buffer: &LogBuffer) -> bool {
        self.query.is_empty() || self.scanned_to >= buffer.end_id()
    }

    /// Position of the current match among all matches.
    pub fn current_index(&self) -> Option<usize> {
        self.current.map(|current| current.index)
    }

    /// The current match.
    pub fn current_match(&self, buffer: &LogBuffer) -> Option<LogMatch> {
        let current = self.current?;
        let entry = buffer.get(buffer.index_of(current.line)?)?;
        let range = matches(&entry.text, &self.query).nth(current.nth as usize)?;
        Some(LogMatch {
            line: current.line,
            range,
        })
    }

    /// Which of the matches of line `id` (found by [`find_matches`]) is the
    /// current one.
    pub fn current_in_line(&self, id: u64) -> Option<usize> {
        self.current
            .filter(|current| current.line == id)
            .map(|current| current.nth as usize)
    }

    /// Replaces the query and forgets the matches; the buffer is searched
    /// by [`step`](Self::step) or [`finish`](Self::finish).
    pub fn set_query(&mut self, query: &str, buffer: &LogBuffer) {
        self.query = query.to_owned();
        // Not `clear`: the matches of a wide query take megabytes.
        self.hits = VecDeque::new();
        self.total = 0;
        self.current = None;
        self.wanted = None;
        self.scanned_to = buffer.first_id();
    }

    /// Searches the next lines not searched yet: about `budget` bytes of
    /// text (each line also counts as [`LINE_COST`] bytes), at least one
    /// line.
    pub fn step(&mut self, buffer: &LogBuffer, budget: usize) -> ScanStep {
        if !self.query.is_empty() {
            self.scanned_to = self.scanned_to.max(buffer.first_id());
            if let Some(start) = buffer.index_of(self.scanned_to) {
                let mut spent = 0usize;
                for entry in buffer.lines.range(start..) {
                    if spent >= budget {
                        break;
                    }
                    self.count(self.scanned_to, entry, false);
                    self.scanned_to += 1;
                    spent = spent.saturating_add(entry.text.len() + LINE_COST);
                }
            }
        }
        let complete = self.is_complete(buffer);
        ScanStep {
            complete,
            moved: self.arrive(complete),
        }
    }

    /// Searches every line not searched yet.
    pub fn finish(&mut self, buffer: &LogBuffer) -> ScanStep {
        self.step(buffer, usize::MAX)
    }

    /// Searches the `count` lines just prepended to the buffer; the current
    /// match stays the same.
    pub fn prepend(&mut self, buffer: &LogBuffer, count: usize) {
        if self.query.is_empty() || count == 0 {
            return;
        }
        let first = buffer.first_id();
        let before = self.total;
        let prepended = buffer.lines.range(..count.min(buffer.len()));
        for (offset, entry) in prepended.enumerate().rev() {
            self.count(first + offset as u64, entry, true);
        }
        if let Some(current) = &mut self.current {
            current.index += self.total - before;
        }
    }

    /// Forgets matches in lines evicted from the buffer.
    pub fn evict(&mut self, buffer: &LogBuffer) {
        let first = buffer.first_id();
        let mut gone = 0;
        while let Some(hit) = self.hits.front()
            && hit.line < first
        {
            gone += hit.count as usize;
            self.hits.pop_front();
        }
        if gone == 0 {
            return;
        }
        self.total -= gone;
        self.current = match self.current {
            Some(current) if current.line >= first => Some(Current {
                index: current.index - gone,
                ..current
            }),
            Some(_) => self.first(),
            None => None,
        };
    }

    /// Moves to the next match (wrapping) and returns it. `None` also when
    /// the next one is not known yet: [`step`](Self::step) makes the move
    /// then and says so.
    pub fn next_match(&mut self, buffer: &LogBuffer) -> Option<LogMatch> {
        self.wanted = None;
        match self.following() {
            Some(next) => self.current = Some(next),
            None if self.is_complete(buffer) => self.current = self.first(),
            None => {
                self.wanted = Some(Wanted::Next);
                return None;
            }
        }
        self.current_match(buffer)
    }

    /// Moves to the previous match (wrapping) and returns it. `None` also
    /// when it is the last match and the search is not complete:
    /// [`step`](Self::step) makes the move then and says so.
    pub fn prev_match(&mut self, buffer: &LogBuffer) -> Option<LogMatch> {
        self.wanted = None;
        match self.preceding() {
            Some(prev) => self.current = Some(prev),
            None if self.is_complete(buffer) => self.current = self.last(),
            None => {
                self.wanted = Some(Wanted::Prev);
                return None;
            }
        }
        self.current_match(buffer)
    }

    /// Counts the matches of a line; `front`: the line is before all known
    /// ones.
    fn count(&mut self, line: u64, entry: &LogEntry, front: bool) {
        let count = matches(&entry.text, &self.query).count();
        if count == 0 {
            return;
        }
        // Lines are far shorter than 4 GiB.
        let hit = Hit {
            line,
            count: u32::try_from(count).unwrap_or(u32::MAX),
        };
        self.total += hit.count as usize;
        if front {
            self.hits.push_front(hit);
        } else {
            self.hits.push_back(hit);
        }
    }

    /// Makes the step that was waiting for more lines, if it can be made.
    fn arrive(&mut self, complete: bool) -> bool {
        let target = match self.wanted {
            None => return false,
            Some(Wanted::Next) => match self.following() {
                Some(next) => Some(next),
                None if complete => self.first(),
                None => return false,
            },
            Some(Wanted::Prev) if complete => self.last(),
            Some(Wanted::Prev) => return false,
        };
        self.wanted = None;
        let moved = target.is_some() && target != self.current;
        if target.is_some() {
            self.current = target;
        }
        moved
    }

    fn first(&self) -> Option<Current> {
        self.hits.front().map(|hit| Current {
            line: hit.line,
            nth: 0,
            index: 0,
        })
    }

    fn last(&self) -> Option<Current> {
        self.hits.back().map(|hit| Current {
            line: hit.line,
            nth: hit.count - 1,
            index: self.total - 1,
        })
    }

    /// Where the current match is among the lines with matches.
    fn slot(&self, current: Current) -> Option<usize> {
        let slot = self.hits.partition_point(|hit| hit.line < current.line);
        (self.hits.get(slot)?.line == current.line).then_some(slot)
    }

    /// The match after the current one among those found so far (the first
    /// one when there is no current match).
    fn following(&self) -> Option<Current> {
        let Some(current) = self.current else {
            return self.first();
        };
        let slot = self.slot(current)?;
        if current.nth + 1 < self.hits[slot].count {
            return Some(Current {
                nth: current.nth + 1,
                index: current.index + 1,
                ..current
            });
        }
        self.hits.get(slot + 1).map(|hit| Current {
            line: hit.line,
            nth: 0,
            index: current.index + 1,
        })
    }

    /// The match before the current one.
    fn preceding(&self) -> Option<Current> {
        let current = self.current?;
        if current.nth > 0 {
            return Some(Current {
                nth: current.nth - 1,
                index: current.index - 1,
                ..current
            });
        }
        let hit = self.hits.get(self.slot(current)?.checked_sub(1)?)?;
        Some(Current {
            line: hit.line,
            nth: hit.count - 1,
            index: current.index - 1,
        })
    }
}

/// Non-overlapping occurrences of `needle` in `haystack`. ASCII queries
/// ignore case; others match exactly.
pub fn find_matches(haystack: &str, needle: &str) -> Vec<Range<usize>> {
    matches(haystack, needle).collect()
}

/// The occurrences [`find_matches`] collects.
fn matches<'a>(haystack: &'a str, needle: &'a str) -> Matches<'a> {
    let pattern = needle.as_bytes();
    let fold = needle.is_ascii();
    let first = pattern.first().copied().unwrap_or_default();
    Matches {
        hay: haystack.as_bytes(),
        pattern,
        fold,
        first: if fold {
            [first.to_ascii_lowercase(), first.to_ascii_uppercase()]
        } else {
            [first; 2]
        },
        at: 0,
    }
}

/// Matches are compared as bytes. An ASCII needle can only match at char
/// boundaries, and so can any other needle, being whole characters itself:
/// the ranges are valid string indices.
struct Matches<'a> {
    hay: &'a [u8],
    pattern: &'a [u8],
    /// Ignore ASCII case.
    fold: bool,
    /// The bytes a match can start with.
    first: [u8; 2],
    at: usize,
}

impl Iterator for Matches<'_> {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Range<usize>> {
        let len = self.pattern.len();
        if len == 0 {
            return None;
        }
        // The last index a match can start at.
        let last = self.hay.len().checked_sub(len)?;
        let [lower, upper] = self.first;
        while self.at <= last {
            let ahead = &self.hay[self.at..=last];
            let Some(skip) = ahead.iter().position(|&b| b == lower || b == upper) else {
                break;
            };
            let start = self.at + skip;
            let found = &self.hay[start..start + len];
            let equal = if self.fold {
                found.eq_ignore_ascii_case(self.pattern)
            } else {
                found == self.pattern
            };
            if equal {
                self.at = start + len;
                return Some(start..start + len);
            }
            self.at = start + 1;
        }
        self.at = last + 1;
        None
    }
}

/// Whether a log view keeps scrolling to new lines, and how many arrived
/// while it did not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FollowState {
    Following,
    Paused { unseen: usize },
}

impl FollowState {
    pub fn is_following(self) -> bool {
        self == FollowState::Following
    }

    /// Lines appended while paused.
    pub fn unseen(self) -> usize {
        match self {
            FollowState::Following => 0,
            FollowState::Paused { unseen } => unseen,
        }
    }

    pub fn appended(&mut self, lines: usize) {
        if let FollowState::Paused { unseen } = self {
            *unseen = unseen.saturating_add(lines);
        }
    }

    /// Syncs with whether the list currently sticks to its end (it stops when
    /// the user scrolls up and resumes when they scroll back down).
    pub fn sync(&mut self, following: bool) {
        match (*self, following) {
            (FollowState::Paused { .. }, true) => *self = FollowState::Following,
            (FollowState::Following, false) => *self = FollowState::Paused { unseen: 0 },
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(buffer: &mut LogBuffer, texts: &[&str]) -> PushResult {
        buffer.push(texts.iter().map(|text| LogEntry::new(text.to_string())))
    }

    #[test]
    fn buffer_evicts_oldest_and_keeps_ids() {
        let mut buffer = LogBuffer::new(3);
        assert_eq!(
            lines(&mut buffer, &["a", "b"]),
            PushResult {
                added: 2,
                evicted: 0
            }
        );
        assert_eq!(
            lines(&mut buffer, &["c", "d", "e"]),
            PushResult {
                added: 3,
                evicted: 2
            }
        );
        let base = FIRST_ID;
        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.first_id(), base + 2);
        assert_eq!(buffer.end_id(), base + 5);
        assert_eq!(buffer.get(0).unwrap().text, "c");
        assert_eq!(buffer.index_of(base + 4), Some(2));
        assert_eq!(buffer.index_of(base + 1), None);
        assert_eq!(buffer.index_of(base + 5), None);
    }

    #[test]
    fn clear_keeps_counting() {
        let mut buffer = LogBuffer::new(10);
        lines(&mut buffer, &["a", "b"]);
        buffer.clear();
        assert!(buffer.is_empty());
        assert_eq!(buffer.bytes(), 0);
        assert_eq!(buffer.first_id(), FIRST_ID + 2);
        lines(&mut buffer, &["c"]);
        assert_eq!(buffer.index_of(FIRST_ID + 2), Some(0));
    }

    fn entries(texts: &[&str]) -> Vec<LogEntry> {
        texts
            .iter()
            .map(|text| LogEntry::new(text.to_string()))
            .collect()
    }

    fn texts(buffer: &LogBuffer) -> Vec<String> {
        buffer.iter().map(|e| e.text.to_string()).collect()
    }

    /// Bytes of a line without spans whose text is `text` bytes long.
    fn line_bytes(text: usize) -> usize {
        LogEntry::new("x".repeat(text)).bytes()
    }

    #[test]
    fn prepend_keeps_ids_of_existing_lines() {
        let mut buffer = LogBuffer::new(5);
        lines(&mut buffer, &["c", "d"]);
        let d = buffer.first_id() + 1;
        assert_eq!(buffer.prepend(entries(&["a", "b"])), 2);
        assert_eq!(buffer.get(buffer.index_of(d).unwrap()).unwrap().text, "d");
        assert_eq!(buffer.get(0).unwrap().text, "a");
        assert_eq!(buffer.first_id(), d - 3);
        // Only the newest prepended lines that fit.
        assert_eq!(buffer.prepend(entries(&["x", "y", "z"])), 1);
        assert_eq!(texts(&buffer), ["z", "a", "b", "c", "d"]);
        let ids: Vec<u64> = (0..buffer.len())
            .map(|ix| buffer.first_id() + ix as u64)
            .collect();
        assert!(ids.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn a_line_counts_its_text_and_spans() {
        let plain = LogEntry::new("hello");
        assert_eq!(plain.bytes(), size_of::<LogEntry>() + TEXT_HEADER + 5);
        let span = StyledSpan {
            range: 0..2,
            style: Default::default(),
        };
        let colored = LogEntry::new("hello").spans(vec![span.clone(), span]);
        assert_eq!(colored.bytes(), plain.bytes() + 2 * size_of::<StyledSpan>());
        // The label is shared between the lines of a source.
        assert_eq!(LogEntry::new("hello").tag("pod", 0).bytes(), plain.bytes());
    }

    #[test]
    fn byte_limit_evicts_the_oldest_lines() {
        // Room for three lines of 100 bytes of text, not four.
        let mut buffer = LogBuffer::new(1000).with_max_bytes(line_bytes(100) * 3 + 10);
        let line = |fill: &str| LogEntry::new(fill.repeat(100));
        assert_eq!(buffer.push([line("a"), line("b"), line("c")]).evicted, 0);
        assert_eq!(buffer.bytes(), line_bytes(100) * 3);
        assert_eq!(
            buffer.push([line("d")]),
            PushResult {
                added: 1,
                evicted: 1
            }
        );
        assert_eq!(buffer.first_id(), FIRST_ID + 1);
        assert_eq!(buffer.get(0).unwrap().text, "b".repeat(100));
        // A larger line pushes out as many as it takes.
        let pushed = buffer.push([LogEntry::new("e".repeat(350))]);
        assert_eq!(pushed.evicted, 3);
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.bytes(), line_bytes(350));
        assert!(buffer.bytes() <= buffer.max_bytes());
        // Short lines fit beside it again.
        assert_eq!(buffer.push([LogEntry::new("f")]).evicted, 0);
        assert_eq!(buffer.len(), 2);
    }

    #[test]
    fn both_limits_hold_together() {
        let mut buffer = LogBuffer::new(4).with_max_bytes(line_bytes(10) * 6);
        // Short lines: the line limit is the tighter one.
        buffer.push((0..10).map(|_| LogEntry::new("x".repeat(10))));
        assert_eq!(buffer.len(), 4);
        // Long lines: the byte limit is.
        buffer.push((0..10).map(|_| LogEntry::new("x".repeat(10 + line_bytes(10)))));
        assert_eq!(buffer.len(), 3);
        assert!(buffer.bytes() <= buffer.max_bytes());
    }

    #[test]
    fn a_line_larger_than_the_byte_limit_is_kept_alone() {
        let mut buffer = LogBuffer::new(1000).with_max_bytes(line_bytes(100) * 2);
        lines(&mut buffer, &["a", "b"]);
        let huge = "x".repeat(5000);
        let pushed = buffer.push([LogEntry::new(huge.clone())]);
        assert_eq!(pushed.evicted, 2);
        assert_eq!(texts(&buffer), std::slice::from_ref(&huge));
        assert_eq!(buffer.widest(), Some(FIRST_ID + 2));
        // The next line takes its place: the newest one always stays.
        assert_eq!(lines(&mut buffer, &["c"]).evicted, 1);
        assert_eq!(texts(&buffer), ["c"]);
        assert_eq!(buffer.bytes(), line_bytes(1));
        // Of two huge lines in one batch only the newer one stays.
        let pushed = buffer.push([LogEntry::new(huge.clone()), LogEntry::new(huge.clone())]);
        assert_eq!(pushed.evicted, 2);
        assert_eq!(buffer.len(), 1);
        // Nothing is prepended to a buffer over its limit.
        assert_eq!(buffer.prepend(entries(&["early"])), 0);
        // An empty buffer takes a huge line from the history too.
        let mut buffer = LogBuffer::new(1000).with_max_bytes(line_bytes(100));
        assert_eq!(
            buffer.prepend(vec![LogEntry::new("old"), LogEntry::new(huge)]),
            1
        );
        assert_eq!(buffer.len(), 1);
    }

    #[test]
    fn prepend_stops_at_the_byte_limit() {
        let mut buffer = LogBuffer::new(1000).with_max_bytes(line_bytes(10) * 3);
        buffer.push([LogEntry::new("n".repeat(10))]);
        let early: Vec<LogEntry> = ["a", "b", "c", "d"]
            .iter()
            .map(|fill| LogEntry::new(fill.repeat(10)))
            .collect();
        // The newest two fit.
        assert_eq!(buffer.prepend(early), 2);
        assert_eq!(
            texts(&buffer),
            ["c".repeat(10), "d".repeat(10), "n".repeat(10)]
        );
        assert_eq!(buffer.bytes(), line_bytes(10) * 3);
        assert_eq!(buffer.first_id(), FIRST_ID - 2);
    }

    /// The widest line by a full scan (the latest of equals).
    fn widest_by_scan(buffer: &LogBuffer) -> Option<u64> {
        let mut best: Option<(u64, usize)> = None;
        for (index, entry) in buffer.iter().enumerate() {
            let cells = entry.cells();
            if best.is_none_or(|(_, widest)| widest <= cells) {
                best = Some((buffer.first_id() + index as u64, cells));
            }
        }
        best.map(|(id, _)| id)
    }

    #[test]
    fn widest_line_follows_pushes_evictions_and_prepends() {
        let mut buffer = LogBuffer::new(4);
        assert_eq!(buffer.widest(), None);
        lines(&mut buffer, &["aaaa", "bb", "ccc"]);
        assert_eq!(buffer.widest(), Some(FIRST_ID));
        // Equal width: the later line, which lives longer.
        lines(&mut buffer, &["dddd"]);
        assert_eq!(buffer.widest(), Some(FIRST_ID + 3));
        // Evicts "aaaa" and "bb".
        lines(&mut buffer, &["e", "f"]);
        assert_eq!(buffer.widest(), Some(FIRST_ID + 3));
        // Evicts "ccc" and "dddd": the widest is now one of the short ones.
        lines(&mut buffer, &["g", "hh"]);
        assert_eq!(buffer.widest(), Some(FIRST_ID + 7));
        assert_eq!(buffer.widest(), widest_by_scan(&buffer));

        let mut buffer = LogBuffer::new(10);
        lines(&mut buffer, &["bbb", "c"]);
        buffer.prepend(entries(&["x", "aaaaa", "yy"]));
        assert_eq!(buffer.widest(), Some(FIRST_ID - 2));
        assert_eq!(buffer.widest(), widest_by_scan(&buffer));
        buffer.prepend(entries(&["zzzzz"]));
        assert_eq!(buffer.widest(), Some(FIRST_ID - 2));
        buffer.clear();
        assert_eq!(buffer.widest(), None);
    }

    #[test]
    fn widest_line_and_bytes_match_a_scan_on_mixed_input() {
        let mut buffer = LogBuffer::new(7).with_max_bytes(line_bytes(4) * 6);
        let mut seed = 17usize;
        for round in 0..300 {
            seed = seed * 31 % 1009;
            let text = "x".repeat(seed % 9);
            if round % 5 == 4 {
                buffer.prepend(vec![LogEntry::new(text)]);
            } else {
                buffer.push([LogEntry::new(text)]);
            }
            assert_eq!(buffer.widest(), widest_by_scan(&buffer), "round {round}");
            let bytes: usize = buffer.iter().map(LogEntry::bytes).sum();
            assert_eq!(buffer.bytes(), bytes, "round {round}");
            assert!(bytes <= buffer.max_bytes() || buffer.len() == 1);
            assert!(buffer.len() <= buffer.capacity());
        }
    }

    #[test]
    fn cells_count_the_tag_and_wide_characters() {
        assert_eq!(LogEntry::new("abc").cells(), 3);
        assert_eq!(LogEntry::new("abc").tag("pod", 0).cells(), 7);
        assert_eq!(LogEntry::new("日本").cells(), 4);
    }

    #[test]
    fn timestamps_show_milliseconds_in_utc() {
        let mut line = String::from("pod ");
        write_timestamp(&mut line, "2026-10-22T10:11:12.345678Z".parse().unwrap());
        assert_eq!(line, "pod 2026-10-22T10:11:12.345Z");
        let mut line = String::new();
        write_timestamp(&mut line, "2026-01-02T03:04:05+03:00".parse().unwrap());
        assert_eq!(line, "2026-01-02T00:04:05.000Z");
        // A line keeps it to the millisecond, in eight bytes.
        let at: Timestamp = "2026-10-22T10:11:12.345678Z".parse().unwrap();
        let entry = LogEntry::new("x").timestamp(at);
        assert_eq!(
            entry.time(),
            Some("2026-10-22T10:11:12.345Z".parse().unwrap())
        );
        assert_eq!(LogEntry::new("x").time(), None);
        assert!(size_of::<LogEntry>() <= 80);
    }

    /// A search with every line searched, as after the old `set_query`.
    fn searched(query: &str, buffer: &LogBuffer) -> LogSearch {
        let mut search = LogSearch::default();
        search.set_query(query, buffer);
        assert!(search.finish(buffer).complete);
        search
    }

    /// What `LogView::push` does.
    fn push(buffer: &mut LogBuffer, search: &mut LogSearch, entries: Vec<LogEntry>) {
        let complete = search.is_complete(buffer);
        let pushed = buffer.push(entries);
        if pushed.evicted > 0 {
            search.evict(buffer);
        }
        if complete {
            search.finish(buffer);
        }
    }

    /// Matches of `id` as the rows draw them.
    fn in_line(search: &LogSearch, buffer: &LogBuffer, id: u64) -> Vec<Range<usize>> {
        let entry = buffer.get(buffer.index_of(id).unwrap()).unwrap();
        find_matches(&entry.text, search.query())
    }

    #[test]
    fn search_covers_prepended_lines() {
        let mut buffer = LogBuffer::new(100);
        lines(&mut buffer, &["x", "y x"]);
        let mut search = searched("x", &buffer);
        search.next_match(&buffer);
        search.next_match(&buffer);
        let current = search.current_match(&buffer);
        let added = buffer.prepend(entries(&["x x", "none"]));
        search.prepend(&buffer, added);
        assert_eq!(search.len(), 4);
        assert_eq!(search.current_match(&buffer), current);
        assert_eq!(search.current_index(), Some(3));
        assert_eq!(in_line(&search, &buffer, buffer.first_id()).len(), 2);
    }

    #[test]
    fn finds_ascii_case_insensitively() {
        assert_eq!(
            find_matches("Error: error ERROR", "error"),
            vec![0..5, 7..12, 13..18]
        );
        assert_eq!(find_matches("aaaa", "aa"), vec![0..2, 2..4]);
        assert!(find_matches("abc", "").is_empty());
        assert!(find_matches("ab", "abc").is_empty());
    }

    #[test]
    fn finds_in_unicode_text() {
        let text = "привет error мир";
        let found = find_matches(text, "ERROR");
        assert_eq!(found.len(), 1);
        assert_eq!(&text[found[0].clone()], "error");
        assert_eq!(find_matches(text, "мир"), vec![19..25]);
    }

    #[test]
    fn search_navigates_and_wraps() {
        let mut buffer = LogBuffer::new(100);
        lines(&mut buffer, &["one x", "two", "x x"]);
        let mut search = searched("x", &buffer);
        assert_eq!(search.len(), 3);
        assert_eq!(search.current_match(&buffer), None);
        let first = buffer.first_id();
        assert_eq!(search.next_match(&buffer).unwrap().line, first);
        assert_eq!(search.current_in_line(first), Some(0));
        assert_eq!(search.next_match(&buffer).unwrap().range, 0..1);
        assert_eq!(search.current_in_line(first), None);
        assert_eq!(search.next_match(&buffer).unwrap().range, 2..3);
        assert_eq!(search.current_in_line(first + 2), Some(1));
        assert_eq!(search.current_index(), Some(2));
        assert_eq!(search.next_match(&buffer).unwrap().line, first);
        assert_eq!(search.current_index(), Some(0));
        assert_eq!(search.prev_match(&buffer).unwrap().line, first + 2);
        assert_eq!(search.current_index(), Some(2));
        assert_eq!(in_line(&search, &buffer, first + 2).len(), 2);
        assert!(in_line(&search, &buffer, first + 1).is_empty());
    }

    #[test]
    fn search_extends_and_evicts() {
        let mut buffer = LogBuffer::new(3);
        lines(&mut buffer, &["x", "y", "x"]);
        let mut search = searched("x", &buffer);
        search.next_match(&buffer);
        search.next_match(&buffer);
        assert_eq!(search.current_match(&buffer).unwrap().line, FIRST_ID + 2);

        push(&mut buffer, &mut search, entries(&["x", "x"]));
        assert_eq!(buffer.first_id(), FIRST_ID + 2);
        assert_eq!(search.len(), 3);
        assert_eq!(search.current_match(&buffer).unwrap().line, FIRST_ID + 2);
        assert_eq!(search.current_index(), Some(0));
    }

    #[test]
    fn evicting_current_match_moves_to_first() {
        let mut buffer = LogBuffer::new(2);
        lines(&mut buffer, &["x", "y"]);
        let mut search = searched("x", &buffer);
        search.next_match(&buffer);
        let pushed = buffer.push(entries(&["x", "x"]));
        assert_eq!(pushed.evicted, 2);
        search.evict(&buffer);
        assert_eq!(search.current_match(&buffer), None);
        search.finish(&buffer);
        assert_eq!(search.len(), 2);
    }

    #[test]
    fn a_step_searches_a_piece_of_the_buffer() {
        let mut buffer = LogBuffer::new(100);
        lines(&mut buffer, &["x", "y", "x x", "y", "x"]);
        let mut search = LogSearch::default();
        search.set_query("x", &buffer);
        assert_eq!(search.len(), 0);
        assert!(!search.is_complete(&buffer));
        // The smallest budget still searches a line.
        for found in [1, 1, 3, 3] {
            let step = search.step(&buffer, 1);
            assert!(!step.complete && !step.moved);
            assert_eq!(search.len(), found);
        }
        assert!(search.step(&buffer, 1).complete);
        assert_eq!(search.len(), 4);
        // A budget of two lines.
        search.set_query("x", &buffer);
        search.step(&buffer, LINE_COST + 2);
        assert_eq!(search.scanned_to, FIRST_ID + 2);
        // No query: nothing to search.
        search.set_query("", &buffer);
        assert!(search.is_complete(&buffer));
        assert!(search.step(&buffer, 1).complete);
        assert_eq!(search.len(), 0);
    }

    #[test]
    fn lines_appended_during_the_search_are_searched_in_turn() {
        let mut buffer = LogBuffer::new(4);
        lines(&mut buffer, &["x", "y", "x"]);
        let mut search = LogSearch::default();
        search.set_query("x", &buffer);
        search.step(&buffer, 1);
        // Evicts the searched line and one not searched yet.
        push(&mut buffer, &mut search, entries(&["x x", "y", "x"]));
        assert_eq!(search.len(), 0);
        assert!(!search.is_complete(&buffer));
        assert!(search.finish(&buffer).complete);
        assert_eq!(search.len(), 4);
        // Complete: new lines are searched as they come.
        push(&mut buffer, &mut search, entries(&["x"]));
        assert!(search.is_complete(&buffer));
        assert_eq!(search.len(), 4);
        assert_eq!(search.hits.len(), 3);
    }

    #[test]
    fn a_step_past_the_found_matches_waits_for_the_search() {
        let mut buffer = LogBuffer::new(100);
        lines(&mut buffer, &["x", "y", "y", "x", "y", "x"]);
        let first = buffer.first_id();
        let mut search = LogSearch::default();
        search.set_query("x", &buffer);
        // Nothing found yet: the first match, once there is one.
        assert_eq!(search.next_match(&buffer), None);
        let step = search.step(&buffer, 1);
        assert!(step.moved && !step.complete);
        assert_eq!(search.current_match(&buffer).unwrap().line, first);
        assert_eq!(search.current_index(), Some(0));
        // The next one is two lines further.
        assert_eq!(search.next_match(&buffer), None);
        assert_eq!(search.current_index(), Some(0));
        assert!(!search.step(&buffer, 1).moved);
        assert!(!search.step(&buffer, 1).moved);
        assert!(search.step(&buffer, 1).moved);
        assert_eq!(search.current_match(&buffer).unwrap().line, first + 3);
        assert_eq!(search.current_index(), Some(1));
        // A known one is taken at once.
        assert_eq!(search.prev_match(&buffer).unwrap().line, first);
        // Back from the first one is the last one, known at the end only.
        assert_eq!(search.prev_match(&buffer), None);
        assert!(!search.step(&buffer, 1).moved);
        let step = search.step(&buffer, 1);
        assert!(step.moved && step.complete);
        assert_eq!(search.current_match(&buffer).unwrap().line, first + 5);
        assert_eq!(search.current_index(), Some(2));
        assert_eq!(search.len(), 3);
    }

    #[test]
    fn a_waiting_step_wraps_when_nothing_more_is_found() {
        let mut buffer = LogBuffer::new(100);
        lines(&mut buffer, &["x", "x", "y", "y"]);
        let first = buffer.first_id();
        let mut search = LogSearch::default();
        search.set_query("x", &buffer);
        search.step(&buffer, LINE_COST + 2);
        search.next_match(&buffer);
        assert_eq!(search.next_match(&buffer).unwrap().line, first + 1);
        assert_eq!(search.next_match(&buffer), None);
        let step = search.finish(&buffer);
        assert!(step.moved && step.complete);
        assert_eq!(search.current_match(&buffer).unwrap().line, first);
        assert_eq!(search.current_index(), Some(0));
        // Nothing is waiting any more.
        assert!(!search.finish(&buffer).moved);

        // A single match: wrapping lands on it again, nothing moved.
        let mut buffer = LogBuffer::new(100);
        lines(&mut buffer, &["x", "y"]);
        search.set_query("x", &buffer);
        search.step(&buffer, 1);
        search.next_match(&buffer);
        assert_eq!(search.next_match(&buffer), None);
        assert!(!search.finish(&buffer).moved);
        assert_eq!(search.current_index(), Some(0));

        // No matches at all, and a new query forgets the waiting step.
        search.set_query("z", &buffer);
        assert_eq!(search.prev_match(&buffer), None);
        assert!(!search.finish(&buffer).moved);
        assert_eq!(search.current_index(), None);
        search.set_query("x", &buffer);
        assert_eq!(search.next_match(&buffer), None);
        search.set_query("y", &buffer);
        assert!(!search.finish(&buffer).moved);
        assert_eq!(search.current_index(), None);
    }

    #[test]
    fn matches_take_memory_per_line_not_per_match() {
        let mut buffer = LogBuffer::new(50);
        let mut search = searched("a", &buffer);
        for _ in 0..40 {
            let batch = (0..7).map(|_| LogEntry::new("a".repeat(1000))).collect();
            push(&mut buffer, &mut search, batch);
        }
        assert_eq!(search.len(), 50_000);
        assert_eq!(search.hits.len(), 50);
        assert_eq!(size_of::<Hit>(), 16);
    }

    /// The search before #171, kept to compare with: every match is stored
    /// and a query searches the whole buffer at once.
    mod reference {
        use super::super::{LogBuffer, LogMatch};
        use std::ops::Range;

        #[derive(Debug, Default)]
        pub struct Search {
            pub query: String,
            pub matches: Vec<LogMatch>,
            pub current: Option<usize>,
        }

        impl Search {
            pub fn current(&self) -> Option<&LogMatch> {
                self.matches.get(self.current?)
            }

            pub fn set_query(&mut self, query: &str, buffer: &LogBuffer) {
                self.query = query.to_owned();
                self.matches.clear();
                self.current = None;
                self.scan(buffer, buffer.first_id());
            }

            pub fn extend(&mut self, buffer: &LogBuffer, from_id: u64) {
                self.scan(buffer, from_id.max(buffer.first_id()));
            }

            pub fn prepend(&mut self, buffer: &LogBuffer, count: usize) {
                if self.query.is_empty() || count == 0 {
                    return;
                }
                let first = buffer.first_id();
                let mut found = Vec::new();
                for (offset, entry) in buffer.lines.range(..count.min(buffer.len())).enumerate() {
                    let line = first + offset as u64;
                    found.extend(
                        find_matches(&entry.text, &self.query)
                            .into_iter()
                            .map(|range| LogMatch { line, range }),
                    );
                }
                if let Some(current) = &mut self.current {
                    *current += found.len();
                }
                self.matches.splice(0..0, found);
            }

            pub fn evict(&mut self, buffer: &LogBuffer) {
                let gone = self.matches.partition_point(|m| m.line < buffer.first_id());
                if gone == 0 {
                    return;
                }
                self.matches.drain(..gone);
                self.current = match self.current {
                    Some(current) if current >= gone => Some(current - gone),
                    Some(_) if !self.matches.is_empty() => Some(0),
                    _ => None,
                };
            }

            pub fn next_match(&mut self) -> Option<&LogMatch> {
                if self.matches.is_empty() {
                    return None;
                }
                let next = match self.current {
                    Some(current) => (current + 1) % self.matches.len(),
                    None => 0,
                };
                self.current = Some(next);
                self.matches.get(next)
            }

            pub fn prev_match(&mut self) -> Option<&LogMatch> {
                if self.matches.is_empty() {
                    return None;
                }
                let prev = match self.current {
                    Some(0) | None => self.matches.len() - 1,
                    Some(current) => current - 1,
                };
                self.current = Some(prev);
                self.matches.get(prev)
            }

            fn scan(&mut self, buffer: &LogBuffer, from_id: u64) {
                if self.query.is_empty() {
                    return;
                }
                let Some(start) = buffer.index_of(from_id) else {
                    return;
                };
                for (offset, entry) in buffer.lines.range(start..).enumerate() {
                    let line = from_id + offset as u64;
                    self.matches.extend(
                        find_matches(&entry.text, &self.query)
                            .into_iter()
                            .map(|range| LogMatch { line, range }),
                    );
                }
            }
        }

        pub fn find_matches(haystack: &str, needle: &str) -> Vec<Range<usize>> {
            let mut found = Vec::new();
            if needle.is_empty() || needle.len() > haystack.len() {
                return found;
            }
            if needle.is_ascii() {
                let hay = haystack.as_bytes();
                let pattern = needle.as_bytes();
                let mut at = 0;
                while at + pattern.len() <= hay.len() {
                    if hay[at..at + pattern.len()].eq_ignore_ascii_case(pattern) {
                        found.push(at..at + pattern.len());
                        at += pattern.len();
                    } else {
                        at += 1;
                    }
                }
            } else {
                found.extend(
                    haystack
                        .match_indices(needle)
                        .map(|(at, text)| at..at + text.len()),
                );
            }
            found
        }
    }

    /// A small deterministic generator.
    struct Random(u64);

    impl Random {
        fn below(&mut self, bound: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % bound as u64) as usize
        }

        /// A short line of a few letters, so that queries match often.
        fn text(&mut self) -> String {
            const LETTERS: [&str; 6] = ["a", "A", "b", "é", "É", " "];
            (0..self.below(9))
                .map(|_| LETTERS[self.below(LETTERS.len())])
                .collect()
        }

        fn entries(&mut self, most: usize) -> Vec<LogEntry> {
            (0..self.below(most + 1))
                .map(|_| LogEntry::new(self.text()))
                .collect()
        }

        fn query(&mut self) -> &'static str {
            const QUERIES: [&str; 12] = [
                "a", "A", "ab", "aa", "b a", " ", "é", "aé", "éé", "É", "", "abba",
            ];
            QUERIES[self.below(QUERIES.len())]
        }
    }

    /// Every match the search knows, with the ranges found again.
    fn all_matches(search: &LogSearch, buffer: &LogBuffer) -> Vec<LogMatch> {
        let mut all = Vec::new();
        let mut before = None;
        for hit in &search.hits {
            assert!(before < Some(hit.line), "lines in order, once each");
            before = Some(hit.line);
            let found = in_line(search, buffer, hit.line);
            assert_eq!(found.len(), hit.count as usize);
            let line = hit.line;
            all.extend(found.into_iter().map(|range| LogMatch { line, range }));
        }
        assert_eq!(all.len(), search.len());
        all
    }

    #[test]
    fn matches_equal_the_reference_on_random_text() {
        let mut random = Random(0x9e37_79b9_7f4a_7c15);
        for _ in 0..20_000 {
            let (text, query) = (random.text(), random.query());
            assert_eq!(
                find_matches(&text, query),
                reference::find_matches(&text, query),
                "{query:?} in {text:?}"
            );
        }
        for (text, query) in [
            ("ERROR error Error", "eRRoR"),
            ("aaaaa", "aa"),
            ("ééé", "éé"),
            ("xé", "é"),
            ("é", "éé"),
            ("Éé", "é"),
            ("日本語の日本", "日本"),
            ("", "a"),
        ] {
            assert_eq!(
                find_matches(text, query),
                reference::find_matches(text, query),
                "{query:?} in {text:?}"
            );
        }
    }

    /// Random appends (with evictions by lines and by bytes), prepends,
    /// clears, query changes and steps between matches; every line is
    /// searched after each of them, as the old search did. The state must
    /// equal the old search at every step.
    #[test]
    fn search_equals_a_full_rescan_after_random_changes() {
        for seed in 1..=40u64 {
            let mut random = Random(seed.wrapping_mul(0x2545_f491_4f6c_dd1d));
            let mut buffer = LogBuffer::new(12);
            if seed % 2 == 0 {
                buffer = buffer.with_max_bytes(line_bytes(4) * 8);
            }
            let mut search = LogSearch::default();
            let mut model = reference::Search::default();
            for round in 0..400 {
                let at = format!("seed {seed}, round {round}");
                match random.below(12) {
                    0..=4 => {
                        let from = buffer.end_id();
                        let pushed = buffer.push(random.entries(5));
                        if pushed.evicted > 0 {
                            search.evict(&buffer);
                            model.evict(&buffer);
                        }
                        search.finish(&buffer);
                        model.extend(&buffer, from);
                    }
                    5 => {
                        let added = buffer.prepend(random.entries(3));
                        search.prepend(&buffer, added);
                        model.prepend(&buffer, added);
                    }
                    6 | 7 => {
                        let query = random.query();
                        search.set_query(query, &buffer);
                        search.finish(&buffer);
                        model.set_query(query, &buffer);
                    }
                    8 | 9 => {
                        let found = search.next_match(&buffer);
                        assert_eq!(found.as_ref(), model.next_match(), "{at}");
                    }
                    10 => {
                        let found = search.prev_match(&buffer);
                        assert_eq!(found.as_ref(), model.prev_match(), "{at}");
                    }
                    _ => {
                        buffer.clear();
                        let query = search.query().to_owned();
                        search.set_query(&query, &buffer);
                        model.set_query(&query, &buffer);
                    }
                }
                assert!(search.is_complete(&buffer), "{at}");
                assert_eq!(search.len(), model.matches.len(), "{at}");
                assert_eq!(search.current_index(), model.current, "{at}");
                assert_eq!(
                    search.current_match(&buffer).as_ref(),
                    model.current(),
                    "{at}"
                );
                assert_eq!(all_matches(&search, &buffer), model.matches, "{at}");
                assert!(search.hits.len() <= buffer.len(), "{at}");
            }
        }
    }

    /// The same changes while the buffer is searched in small pieces, as
    /// the view does it: lines arrive, leave and are prepended between the
    /// pieces. What is found so far is always the matches of the searched
    /// lines, and the result is the full rescan.
    #[test]
    fn a_search_in_pieces_equals_a_full_rescan() {
        for seed in 1..=40u64 {
            let mut random = Random(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
            let mut buffer = LogBuffer::new(30);
            if seed % 2 == 0 {
                buffer = buffer.with_max_bytes(line_bytes(4) * 20);
            }
            let mut search = LogSearch::default();
            let mut model = reference::Search::default();
            let mut completed = 0;
            for round in 0..600 {
                let at = format!("seed {seed}, round {round}");
                match random.below(12) {
                    0..=3 => push(&mut buffer, &mut search, random.entries(6)),
                    4 => {
                        let added = buffer.prepend(random.entries(3));
                        search.prepend(&buffer, added);
                    }
                    5 => search.set_query(random.query(), &buffer),
                    6 => {
                        buffer.clear();
                        let query = search.query().to_owned();
                        search.set_query(&query, &buffer);
                    }
                    _ => {
                        let budget = 1 + random.below(3 * LINE_COST);
                        search.step(&buffer, budget);
                    }
                }
                // The searched part: the old search over those lines only.
                model.set_query(search.query(), &buffer);
                let complete = search.is_complete(&buffer);
                if !complete {
                    model.matches.retain(|m| m.line < search.scanned_to);
                }
                completed += usize::from(complete);
                assert_eq!(all_matches(&search, &buffer), model.matches, "{at}");
                assert_eq!(search.current_index(), None, "{at}");
                assert!(search.hits.len() <= buffer.len(), "{at}");
            }
            assert!(
                completed > 50 && completed < 550,
                "seed {seed}: {completed}"
            );
            search.finish(&buffer);
            model.set_query(search.query(), &buffer);
            assert_eq!(all_matches(&search, &buffer), model.matches);
        }
    }

    /// Steps between matches made while the search is still running end
    /// where they would on a searched buffer.
    #[test]
    fn steps_during_the_search_end_where_a_full_search_puts_them() {
        for seed in 1..=60u64 {
            let mut random = Random(seed.wrapping_mul(0x2545_f491_4f6c_dd1d));
            let mut buffer = LogBuffer::new(200);
            for _ in 0..20 {
                buffer.push(random.entries(6));
            }
            let query = ["a", "ab", "é", "abba"][random.below(4)];
            let mut search = LogSearch::default();
            search.set_query(query, &buffer);
            let mut model = reference::Search::default();
            model.set_query(query, &buffer);
            for round in 0..40 {
                let at = format!("seed {seed}, round {round}");
                let forward = random.below(3) > 0;
                let found = if forward {
                    model.next_match();
                    search.next_match(&buffer)
                } else {
                    model.prev_match();
                    search.prev_match(&buffer)
                };
                if found.is_none() {
                    // Waiting (or no matches): the search goes on until the
                    // step is made.
                    while !search.is_complete(&buffer) {
                        if search.step(&buffer, 1 + random.below(200)).moved {
                            break;
                        }
                    }
                } else if random.below(2) == 0 {
                    search.step(&buffer, 1 + random.below(200));
                }
                assert_eq!(search.current_index(), model.current, "{at}");
                assert_eq!(
                    search.current_match(&buffer).as_ref(),
                    model.current(),
                    "{at}"
                );
            }
        }
    }

    #[test]
    fn follow_counts_unseen_while_paused() {
        let mut follow = FollowState::Following;
        follow.appended(5);
        assert_eq!(follow.unseen(), 0);
        follow.sync(false);
        assert!(!follow.is_following());
        follow.appended(3);
        follow.appended(4);
        assert_eq!(follow.unseen(), 7);
        follow.sync(false);
        assert_eq!(follow.unseen(), 7);
        follow.sync(true);
        assert_eq!(follow, FollowState::Following);
    }
}
