use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::rc::Rc;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::StatusDot;
use crate::age::{format_since, unix_now};
use crate::tokens::{TINT_OPACITY, Tone};

/// One row of an [`EventList`]: a Kubernetes event (or a group of repeats).
#[derive(Clone, Debug, PartialEq)]
pub struct EventEntry {
    warning: bool,
    reason: SharedString,
    message: SharedString,
    object: Option<SharedString>,
    /// Last occurrence, Unix seconds.
    since: Option<i64>,
    count: u32,
    /// Identity across updates, see [`key`](Self::key).
    key: Option<u64>,
}

impl EventEntry {
    pub fn new(reason: impl Into<SharedString>, message: impl Into<SharedString>) -> Self {
        Self {
            warning: false,
            reason: reason.into(),
            message: message.into(),
            object: None,
            since: None,
            count: 1,
            key: None,
        }
    }

    /// Highlights the entry as a `Warning` event.
    pub fn warning(mut self, warning: bool) -> Self {
        self.warning = warning;
        self
    }

    /// The object the event is about, e.g. `Pod/api-1`.
    pub fn object(mut self, object: impl Into<SharedString>) -> Self {
        self.object = Some(object.into());
        self
    }

    /// Time of the last occurrence (Unix seconds), shown as its age (`5m`)
    /// at render time. The owning view keeps it current with an
    /// [`AgeTicker`](crate::AgeTicker).
    pub fn since(mut self, since: i64) -> Self {
        self.since = Some(since);
        self
    }

    /// Occurrences; shown as `×N` above one.
    pub fn count(mut self, count: u32) -> Self {
        self.count = count;
        self
    }

    /// What makes the entry the same row across updates of an
    /// [`EventTimeline`] (e.g. the key of the event group). Without one,
    /// the type, reason, message and object do.
    pub fn key(mut self, key: impl Hash) -> Self {
        self.key = Some(hash_of(key));
        self
    }

    fn identity(&self) -> u64 {
        self.key
            .unwrap_or_else(|| hash_of((self.warning, &self.reason, &self.message, &self.object)))
    }

    /// The row: status dot, reason, repeats, object and age over the message.
    fn render(&self, now: i64, cx: &App) -> Div {
        let theme = cx.theme();
        let (warning, muted, foreground) =
            (theme.warning, theme.muted_foreground, theme.foreground);
        let tone = if self.warning {
            Tone::Warning
        } else {
            Tone::Neutral
        };
        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .text_sm()
            .child(StatusDot::new(tone))
            .child(
                div()
                    .flex_none()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if self.warning { warning } else { foreground })
                    .child(self.reason.clone()),
            )
            .when(self.count > 1, |row| {
                row.child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("×{}", self.count)),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .text_color(muted)
                    .children(self.object.clone()),
            )
            .children(self.since.map(|since| {
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(muted)
                    .child(format_since(since, now))
            }));
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .px_2()
            .py_1p5()
            .rounded_md()
            .when(self.warning, |row| row.bg(warning.opacity(TINT_OPACITY)))
            .child(header)
            .child(
                div()
                    .pl_4()
                    .text_xs()
                    .whitespace_normal()
                    .text_color(if self.warning { foreground } else { muted })
                    .child(self.message.clone()),
            )
    }
}

fn hash_of(value: impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// Timeline of events, newest first as given. Warnings are tinted with the
/// theme's warning color.
///
/// Lays out every row: for short lists inside a scrolling panel (the
/// events in the details of a resource). Long timelines use an
/// [`EventTimeline`], which draws only the rows in view.
#[derive(IntoElement)]
pub struct EventList {
    entries: Vec<EventEntry>,
}

impl EventList {
    pub fn new(entries: impl IntoIterator<Item = EventEntry>) -> Self {
        Self {
            entries: entries.into_iter().collect(),
        }
    }
}

impl RenderOnce for EventList {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let now = unix_now();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .children(self.entries.iter().map(|entry| entry.render(now, cx)))
    }
}

/// Space between the rows of a timeline, as `gap_1` in an [`EventList`].
const ROW_GAP: Pixels = px(4.);
/// Height assumed for a row (with its gap) until it is laid out: reason
/// over a one-line message. Sizes the scrollbar for rows never shown.
const ROW_HEIGHT_HINT: Pixels = px(58.);
/// Rows laid out beyond the visible ones, so scrolling does not pop.
const OVERDRAW: Pixels = px(200.);
/// Runs of changed rows remeasured one by one up to this many; more are
/// remeasured as one range.
const REMEASURE_SPANS: usize = 32;

/// A virtualized [`EventList`]: only the rows in view are laid out and
/// drawn, whatever the length of the timeline (#368).
///
/// Owned by the view that shows it, which hands it the entries whenever
/// they change ([`set_entries`](Self::set_entries)) and draws it with
/// [`render`](Self::render). Rows are matched by [`EventEntry::key`]:
/// rows that stay keep their measured height and the scroll position stays
/// on the same row; only rows whose content changed are measured again.
/// Scrolling re-renders the owning view.
pub struct EventTimeline {
    list: ListState,
    entries: Rc<Vec<EventEntry>>,
}

impl Default for EventTimeline {
    fn default() -> Self {
        Self::new()
    }
}

impl EventTimeline {
    pub fn new() -> Self {
        Self {
            list: ListState::new(0, ListAlignment::Top, OVERDRAW),
            entries: Rc::default(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Scrolled more than `slack` away from the first row.
    pub fn is_scrolled(&self, slack: Pixels) -> bool {
        let top = self.list.logical_scroll_top();
        top.item_ix > 0 || top.offset_in_item > slack
    }

    pub fn scroll_to_top(&self) {
        self.list.scroll_to(ListOffset::default());
    }

    /// Replaces the rows. With `to_top` the timeline then shows its first
    /// row; otherwise it keeps showing the rows it showed: rows added above
    /// push nothing down on screen.
    pub fn set_entries(&mut self, entries: Vec<EventEntry>, to_top: bool) {
        let added = self.splice(&entries).unwrap_or_else(|| {
            self.list.reset(entries.len());
            true
        });
        if added {
            // New rows get the hint; laid-out ones keep their height as it.
            self.list.clone().with_uniform_item_height(ROW_HEIGHT_HINT);
        }
        self.entries = Rc::new(entries);
        if to_top {
            self.scroll_to_top();
        }
    }

    /// Applies the change from the current entries to `new` to the list
    /// state row by row: removed and added rows are spliced, changed ones
    /// remeasured. Whether rows were added, or `None` when kept rows moved
    /// (or keys repeat), which takes a reset.
    fn splice(&self, new: &[EventEntry]) -> Option<bool> {
        let old = &self.entries;
        let old_keys: Vec<u64> = old.iter().map(EventEntry::identity).collect();
        let new_keys: Vec<u64> = new.iter().map(EventEntry::identity).collect();
        let ops = diff(&old_keys, &new_keys)?;
        let mut changed = Vec::new();
        let mut added = false;
        let mut ix = 0;
        for op in ops {
            match op {
                Op::Remove => self.list.splice(ix..ix + 1, 0),
                Op::Insert => {
                    self.list.splice(ix..ix, 1);
                    added = true;
                    ix += 1;
                }
                Op::Keep(old_ix, new_ix) => {
                    if old[old_ix] != new[new_ix] {
                        changed.push(ix);
                    }
                    ix += 1;
                }
            }
        }
        for range in spans(&changed) {
            self.list.remeasure_items(range);
        }
        Some(added)
    }

    /// The timeline, filling its parent (padded inside), with a scrollbar.
    pub fn render(&self) -> impl IntoElement + use<> {
        let entries = self.entries.clone();
        let now = unix_now();
        div()
            .relative()
            .size_full()
            .child(
                list(self.list.clone(), move |ix, _, cx| match entries.get(ix) {
                    Some(entry) => div()
                        .pb(ROW_GAP)
                        .child(entry.render(now, cx))
                        .into_any_element(),
                    None => Empty.into_any_element(),
                })
                .size_full()
                .p_2(),
            )
            .child(Scrollbar::vertical(&self.list))
    }
}

/// A step from the old rows to the new ones.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    /// The row at the cursor is gone.
    Remove,
    /// A new row at the cursor.
    Insert,
    /// Old row `.0` is new row `.1`.
    Keep(usize, usize),
}

/// Steps turning the `old` keys into the `new` ones by removals and
/// insertions only; `None` when keys repeat or kept rows changed order.
fn diff(old: &[u64], new: &[u64]) -> Option<Vec<Op>> {
    let old_set: HashSet<u64> = old.iter().copied().collect();
    let new_set: HashSet<u64> = new.iter().copied().collect();
    if old_set.len() != old.len() || new_set.len() != new.len() {
        return None;
    }
    let mut ops = Vec::with_capacity(old.len().max(new.len()));
    let (mut i, mut j) = (0, 0);
    while i < old.len() || j < new.len() {
        if i < old.len() && !new_set.contains(&old[i]) {
            ops.push(Op::Remove);
            i += 1;
        } else if j < new.len() && !old_set.contains(&new[j]) {
            ops.push(Op::Insert);
            j += 1;
        } else if i < old.len() && j < new.len() && old[i] == new[j] {
            ops.push(Op::Keep(i, j));
            i += 1;
            j += 1;
        } else {
            return None;
        }
    }
    Some(ops)
}

/// Sorted indices as ranges to remeasure: a range per run, or one range
/// over all of them when there are many runs.
fn spans(indices: &[usize]) -> Vec<Range<usize>> {
    let mut spans: Vec<Range<usize>> = Vec::new();
    for &ix in indices {
        match spans.last_mut() {
            Some(span) if span.end == ix => span.end = ix + 1,
            _ => spans.push(ix..ix + 1),
        }
    }
    if let (true, Some(first), Some(last)) =
        (spans.len() > REMEASURE_SPANS, spans.first(), spans.last())
    {
        return std::iter::once(first.start..last.end).collect();
    }
    spans
}

/// A resource usage bar: label, text and how full it is (`0..=1`).
/// Turns warning from 75 % and danger from 90 %. An empty label is left
/// out (a row of a key/value list already names the value).
#[derive(IntoElement)]
pub struct UsageMeter {
    label: SharedString,
    value: SharedString,
    fill: Option<f32>,
}

impl UsageMeter {
    pub fn new(label: impl Into<SharedString>, value: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            fill: None,
        }
    }

    /// Share used; no bar is drawn without one.
    pub fn fill(mut self, fill: Option<f32>) -> Self {
        self.fill = fill.map(|f| f.clamp(0., 1.));
        self
    }

    fn tone(fill: f32) -> Tone {
        if fill >= 0.9 {
            Tone::Danger
        } else if fill >= 0.75 {
            Tone::Warning
        } else {
            Tone::Success
        }
    }
}

impl RenderOnce for UsageMeter {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let bar_color = self.fill.map(|fill| Self::tone(fill).color(cx));
        let theme = cx.theme();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .text_sm()
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap_3()
                    .when(!self.label.is_empty(), |row| {
                        row.child(
                            div()
                                .flex_none()
                                .min_w(px(64.))
                                .text_color(theme.muted_foreground)
                                .child(self.label),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_normal()
                            .font_family(theme.mono_font_family.clone())
                            .text_xs()
                            .text_color(theme.foreground)
                            .child(self.value),
                    ),
            )
            .when_some(self.fill.zip(bar_color), |meter, (fill, color)| {
                meter.child(
                    div()
                        .h(px(4.))
                        .w_full()
                        .rounded_full()
                        .bg(theme.muted)
                        .child(div().h_full().w(relative(fill)).rounded_full().bg(color)),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{Op, UsageMeter, diff, spans};
    use crate::tokens::Tone;

    #[test]
    fn meter_tone_follows_fill() {
        assert_eq!(UsageMeter::tone(0.2), Tone::Success);
        assert_eq!(UsageMeter::tone(0.8), Tone::Warning);
        assert_eq!(UsageMeter::tone(1.0), Tone::Danger);
    }

    #[test]
    fn diff_inserts_on_top_and_removes() {
        assert_eq!(
            diff(&[1, 2, 3], &[9, 1, 3]),
            Some(vec![Op::Insert, Op::Keep(0, 1), Op::Remove, Op::Keep(2, 2)])
        );
        assert_eq!(diff(&[], &[1]), Some(vec![Op::Insert]));
        assert_eq!(diff(&[1], &[]), Some(vec![Op::Remove]));
    }

    #[test]
    fn diff_gives_up_on_moved_or_repeated_rows() {
        assert_eq!(diff(&[1, 2], &[2, 1]), None);
        assert_eq!(diff(&[1], &[1, 1]), None);
    }

    #[test]
    fn spans_join_runs_and_cap_their_number() {
        assert_eq!(spans(&[1, 2, 5]), [1..3, 5..6]);
        let many: Vec<usize> = (0..100).map(|i| i * 2).collect();
        assert_eq!(spans(&many), std::iter::once(0..199).collect::<Vec<_>>());
    }
}
