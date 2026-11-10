//! Bounded line storage and search for [`LogView`](super::LogView).

use std::collections::VecDeque;
use std::ops::Range;

use gpui_kit::SharedString;
use unicode_width::UnicodeWidthStr as _;

use crate::StyledSpan;

/// Source label shown before a line when several sources are merged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogTag {
    pub label: SharedString,
    /// Index into the tag palette; equal indices get equal colors.
    pub color: usize,
}

/// One line of a [`LogView`](super::LogView).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    pub tag: Option<LogTag>,
    pub timestamp: Option<SharedString>,
    /// Plain text: no escape sequences, so search and copy see what is shown.
    pub text: SharedString,
    /// Styled ranges of `text` (program output colors), in order.
    pub spans: Vec<StyledSpan>,
}

impl LogEntry {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            tag: None,
            timestamp: None,
            text: text.into(),
            spans: Vec::new(),
        }
    }

    /// Colors parts of the text; ranges are bytes of `text`.
    pub fn spans(mut self, spans: Vec<StyledSpan>) -> Self {
        self.spans = spans;
        self
    }

    pub fn tag(mut self, label: impl Into<SharedString>, color: usize) -> Self {
        self.tag = Some(LogTag {
            label: label.into(),
            color,
        });
        self
    }

    pub fn timestamp(mut self, timestamp: impl Into<SharedString>) -> Self {
        self.timestamp = Some(timestamp.into());
        self
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

/// Lines in arrival order, dropping the oldest beyond `capacity`.
///
/// Every line gets an id that never changes (`first_id() + index`), so search
/// matches and selections survive eviction from the front and lines inserted
/// before the first one ([`prepend`](Self::prepend) counts ids down from a
/// large start, so they stay unique and ordered).
#[derive(Debug)]
pub struct LogBuffer {
    lines: VecDeque<LogEntry>,
    capacity: usize,
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
    /// Old lines dropped from the front to stay within capacity.
    pub evicted: usize,
}

impl LogBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            capacity: capacity.max(1),
            first_id: FIRST_ID,
            widest: VecDeque::new(),
        }
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

    /// Appends lines, evicting the oldest ones beyond capacity.
    pub fn push(&mut self, entries: impl IntoIterator<Item = LogEntry>) -> PushResult {
        let mut added = 0;
        for entry in entries {
            let (id, cells) = (self.end_id(), entry.cells());
            while self.widest.back().is_some_and(|&(_, back)| back <= cells) {
                self.widest.pop_back();
            }
            self.widest.push_back((id, cells));
            self.lines.push_back(entry);
            added += 1;
        }
        let evicted = self.lines.len().saturating_sub(self.capacity);
        self.lines.drain(..evicted);
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
    /// within capacity (the newest of them); returns how many were added.
    pub fn prepend(&mut self, entries: Vec<LogEntry>) -> usize {
        let room = self
            .capacity
            .saturating_sub(self.lines.len())
            .min(self.first_id as usize);
        let skip = entries.len().saturating_sub(room);
        let added = entries.len() - skip;
        for entry in entries.into_iter().skip(skip).rev() {
            self.first_id -= 1;
            let cells = entry.cells();
            // Only a line wider than everything after it matters.
            if self.widest.front().is_none_or(|&(_, front)| front < cells) {
                self.widest.push_front((self.first_id, cells));
            }
            self.lines.push_front(entry);
        }
        added
    }

    /// Removes every line; ids keep counting up.
    pub fn clear(&mut self) {
        self.first_id = self.end_id();
        self.lines.clear();
        self.widest.clear();
    }
}

/// A search hit: a byte range in the text of line `line` (an id).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogMatch {
    pub line: u64,
    pub range: Range<usize>,
}

/// Matches of a query over a [`LogBuffer`], kept in line order, with a
/// current match for next/previous navigation.
#[derive(Debug, Default)]
pub struct LogSearch {
    query: String,
    matches: Vec<LogMatch>,
    current: Option<usize>,
}

impl LogSearch {
    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn len(&self) -> usize {
        self.matches.len()
    }

    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }

    /// Position of the current match among all matches.
    pub fn current_index(&self) -> Option<usize> {
        self.current
    }

    pub fn current(&self) -> Option<&LogMatch> {
        self.matches.get(self.current?)
    }

    /// Replaces the query and searches the whole buffer.
    pub fn set_query(&mut self, query: &str, buffer: &LogBuffer) {
        self.query = query.to_owned();
        self.matches.clear();
        self.current = None;
        self.scan(buffer, buffer.first_id());
    }

    /// Searches lines appended since `from_id`.
    pub fn extend(&mut self, buffer: &LogBuffer, from_id: u64) {
        self.scan(buffer, from_id.max(buffer.first_id()));
    }

    /// Searches the `count` lines just prepended to the buffer; the current
    /// match stays the same.
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

    /// Forgets matches in lines evicted from the buffer.
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

    /// Moves to the next match (wrapping) and returns it.
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

    /// Moves to the previous match (wrapping) and returns it.
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

    /// Matches in line `id`, in order.
    pub fn in_line(&self, id: u64) -> &[LogMatch] {
        let start = self.matches.partition_point(|m| m.line < id);
        let end = self.matches.partition_point(|m| m.line <= id);
        &self.matches[start..end]
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

/// Non-overlapping occurrences of `needle` in `haystack`. ASCII queries
/// ignore case; others match exactly.
pub fn find_matches(haystack: &str, needle: &str) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    if needle.is_empty() || needle.len() > haystack.len() {
        return found;
    }
    if needle.is_ascii() {
        // An ASCII needle can only match at char boundaries, so byte offsets
        // are valid string indices.
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
        let texts: Vec<_> = buffer.iter().map(|e| e.text.to_string()).collect();
        assert_eq!(texts, ["z", "a", "b", "c", "d"]);
        let ids: Vec<u64> = (0..buffer.len())
            .map(|ix| buffer.first_id() + ix as u64)
            .collect();
        assert!(ids.windows(2).all(|w| w[0] < w[1]));
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
    fn widest_line_matches_a_scan_on_mixed_input() {
        let mut buffer = LogBuffer::new(7);
        let mut seed = 17usize;
        for round in 0..200 {
            seed = seed * 31 % 1009;
            let text = "x".repeat(seed % 9);
            if round % 5 == 4 {
                buffer.prepend(vec![LogEntry::new(text)]);
            } else {
                buffer.push([LogEntry::new(text)]);
            }
            assert_eq!(buffer.widest(), widest_by_scan(&buffer), "round {round}");
        }
    }

    #[test]
    fn cells_count_the_tag_and_wide_characters() {
        assert_eq!(LogEntry::new("abc").cells(), 3);
        assert_eq!(LogEntry::new("abc").tag("pod", 0).cells(), 7);
        assert_eq!(LogEntry::new("日本").cells(), 4);
    }

    #[test]
    fn search_covers_prepended_lines() {
        let mut buffer = LogBuffer::new(100);
        lines(&mut buffer, &["x", "y x"]);
        let mut search = LogSearch::default();
        search.set_query("x", &buffer);
        search.next_match();
        search.next_match();
        let current = search.current().cloned();
        let added = buffer.prepend(entries(&["x x", "none"]));
        search.prepend(&buffer, added);
        assert_eq!(search.len(), 4);
        assert_eq!(search.current().cloned(), current);
        assert_eq!(search.current_index(), Some(3));
        assert_eq!(search.in_line(buffer.first_id()).len(), 2);
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
        let mut search = LogSearch::default();
        search.set_query("x", &buffer);
        assert_eq!(search.len(), 3);
        assert_eq!(search.current(), None);
        let first = buffer.first_id();
        assert_eq!(search.next_match().unwrap().line, first);
        assert_eq!(search.next_match().unwrap().range, 0..1);
        assert_eq!(search.next_match().unwrap().range, 2..3);
        assert_eq!(search.next_match().unwrap().line, first);
        assert_eq!(search.prev_match().unwrap().line, first + 2);
        assert_eq!(search.in_line(first + 2).len(), 2);
        assert!(search.in_line(first + 1).is_empty());
    }

    #[test]
    fn search_extends_and_evicts() {
        let mut buffer = LogBuffer::new(3);
        lines(&mut buffer, &["x", "y", "x"]);
        let mut search = LogSearch::default();
        search.set_query("x", &buffer);
        search.next_match();
        search.next_match();
        assert_eq!(search.current().unwrap().line, FIRST_ID + 2);

        let from = buffer.end_id();
        lines(&mut buffer, &["x", "x"]);
        search.evict(&buffer);
        search.extend(&buffer, from);
        assert_eq!(buffer.first_id(), FIRST_ID + 2);
        assert_eq!(search.len(), 3);
        assert_eq!(search.current().unwrap().line, FIRST_ID + 2);
        assert_eq!(search.current_index(), Some(0));
    }

    #[test]
    fn evicting_current_match_moves_to_first() {
        let mut buffer = LogBuffer::new(2);
        lines(&mut buffer, &["x", "y"]);
        let mut search = LogSearch::default();
        search.set_query("x", &buffer);
        search.next_match();
        let from = buffer.end_id();
        lines(&mut buffer, &["x", "x"]);
        search.evict(&buffer);
        assert_eq!(search.current(), None);
        search.extend(&buffer, from);
        assert_eq!(search.len(), 2);
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
