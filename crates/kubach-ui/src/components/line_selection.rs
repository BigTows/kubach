//! Mouse text selection over the lines of a (virtualized) text view: logs,
//! YAML, describe output.
//!
//! gpui-component's window-wide text selection follows rendered elements
//! (the anchor lives in a painted participant), which a virtualized list
//! drops as soon as a row scrolls away and which cannot copy rows that are
//! not on screen. Here the selection is a model instead: two positions given
//! as (stable line id, byte offset in the displayed line). Rows report their
//! text layout when painted, so the mouse maps to positions; copying reads
//! the lines from the view's own storage, so any number of lines works, and
//! lines appended or evicted around the selection never move it.
//!
//! A view adopts it by owning a [`LineSelector`], implementing
//! [`SelectableLines`], wrapping its root with [`selectable`] and each row's
//! text with [`LineSelector::row`], and drawing
//! [`LineSelector::highlight`] over the row text.

use std::cell::{Cell, RefCell};
use std::cmp::{max, min};
use std::ops::Range;
use std::rc::Rc;

use gpui_kit::base::AutoScroll;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

actions!(
    line_selection,
    [
        /// Copies the selected text.
        CopySelection,
        /// Selects every line.
        SelectAllLines,
        /// Drops the selection.
        ClearSelection
    ]
);

const CONTEXT: &str = "SelectableLines";

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("secondary-c", CopySelection, Some(CONTEXT)),
        KeyBinding::new("secondary-a", SelectAllLines, Some(CONTEXT)),
        KeyBinding::new("escape", ClearSelection, Some(CONTEXT)),
    ]);
}

/// A position in a line view: a line id and a byte offset into the line as
/// displayed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TextPoint {
    pub line: u64,
    pub offset: usize,
}

impl TextPoint {
    pub fn new(line: u64, offset: usize) -> Self {
        Self { line, offset }
    }
}

/// What a press selects and a drag extends by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SelectionUnit {
    #[default]
    Char,
    Word,
    Line,
}

impl SelectionUnit {
    /// Single click: characters, double: words, triple: lines.
    pub fn from_click_count(count: usize) -> Self {
        match count {
            0 | 1 => SelectionUnit::Char,
            2 => SelectionUnit::Word,
            _ => SelectionUnit::Line,
        }
    }
}

/// A selected range of lines, from the press (anchor) to the pointer
/// (head). Both are whole units (a word for a double click), so dragging
/// after a double click extends word by word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineSelection {
    anchor: Range<TextPoint>,
    head: Range<TextPoint>,
    unit: SelectionUnit,
}

impl LineSelection {
    /// Starts a selection at `at` in a line whose text is `text`.
    pub fn new(at: TextPoint, unit: SelectionUnit, text: &str) -> Self {
        let range = unit_range(at, unit, text);
        Self {
            anchor: range.clone(),
            head: range,
            unit,
        }
    }

    /// Everything from the start of line `first` to the end of line `last`
    /// (`last_len` bytes long).
    pub fn all(first: u64, last: u64, last_len: usize) -> Self {
        Self {
            anchor: TextPoint::new(first, 0)..TextPoint::new(first, 0),
            head: TextPoint::new(last, last_len)..TextPoint::new(last, last_len),
            unit: SelectionUnit::Char,
        }
    }

    /// Moves the head to `to` in a line whose text is `text`.
    pub fn extend(&mut self, to: TextPoint, text: &str) {
        self.head = unit_range(to, self.unit, text);
    }

    pub fn unit(&self) -> SelectionUnit {
        self.unit
    }

    pub fn start(&self) -> TextPoint {
        min(self.anchor.start, self.head.start)
    }

    pub fn end(&self) -> TextPoint {
        max(self.anchor.end, self.head.end)
    }

    pub fn is_empty(&self) -> bool {
        self.start() == self.end()
    }

    /// Selected bytes of line `line`, `len` bytes long; `None` when the line
    /// is outside the selection.
    pub fn columns(&self, line: u64, len: usize) -> Option<Range<usize>> {
        let (start, end) = (self.start(), self.end());
        if start == end || line < start.line || line > end.line {
            return None;
        }
        let from = if line == start.line { start.offset } else { 0 };
        let to = if line == end.line { end.offset } else { len };
        Some(from.min(len)..to.min(len))
    }

    /// The selected text, lines joined with `\n`. `text` returns a line's
    /// displayed text, `None` for lines no longer there.
    pub fn text<S: AsRef<str>>(&self, mut text: impl FnMut(u64) -> Option<S>) -> String {
        let (start, end) = (self.start(), self.end());
        let mut out = String::new();
        if start == end {
            return out;
        }
        for line in start.line..=end.line {
            let Some(content) = text(line) else {
                continue;
            };
            let content = content.as_ref();
            if line > start.line {
                out.push('\n');
            }
            if let Some(columns) = self.columns(line, content.len()) {
                let from = floor_char_boundary(content, columns.start);
                let to = floor_char_boundary(content, columns.end).max(from);
                out.push_str(&content[from..to]);
            }
        }
        out
    }

    /// Clips the selection to lines from `first` on (older ones were
    /// dropped). Returns `false` when nothing selected is left.
    pub fn retain_from(&mut self, first: u64) -> bool {
        if self.end().line < first {
            return false;
        }
        let clip = |point: &mut TextPoint| {
            if point.line < first {
                *point = TextPoint::new(first, 0);
            }
        };
        clip(&mut self.anchor.start);
        clip(&mut self.anchor.end);
        clip(&mut self.head.start);
        clip(&mut self.head.end);
        true
    }
}

/// The unit around `at` (an empty range for [`SelectionUnit::Char`]).
fn unit_range(at: TextPoint, unit: SelectionUnit, text: &str) -> Range<TextPoint> {
    let offset = floor_char_boundary(text, at.offset);
    let columns = match unit {
        SelectionUnit::Char => offset..offset,
        SelectionUnit::Word => word_range(text, offset),
        SelectionUnit::Line => 0..text.len(),
    };
    TextPoint::new(at.line, columns.start)..TextPoint::new(at.line, columns.end)
}

#[derive(PartialEq)]
enum CharClass {
    Word,
    Space,
    Other,
}

fn class(ch: char) -> CharClass {
    if ch.is_alphanumeric() || ch == '_' {
        CharClass::Word
    } else if ch.is_whitespace() {
        CharClass::Space
    } else {
        CharClass::Other
    }
}

/// The word (letters, digits, `_`), run of spaces or single other character
/// at byte `offset`; at the end of the line, the one before it.
pub fn word_range(text: &str, offset: usize) -> Range<usize> {
    let offset = floor_char_boundary(text, offset);
    let Some((at, ch)) = text[offset..]
        .chars()
        .next()
        .map(|ch| (offset, ch))
        .or_else(|| text[..offset].char_indices().next_back())
    else {
        return offset..offset;
    };
    let kind = class(ch);
    if kind == CharClass::Other {
        return at..at + ch.len_utf8();
    }
    let start = text[..at]
        .char_indices()
        .rev()
        .take_while(|&(_, ch)| class(ch) == kind)
        .last()
        .map_or(at, |(ix, _)| ix);
    let end = text[at..]
        .char_indices()
        .find(|&(_, ch)| class(ch) != kind)
        .map_or(text.len(), |(ix, _)| at + ix);
    start..end
}

fn floor_char_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// A row painted in the last frame.
struct PaintedRow {
    line: u64,
    layout: TextLayout,
    bounds: Bounds<Pixels>,
}

/// Selection state of a line view: the selection, the rows painted last
/// frame (to map the mouse to text) and the drag in progress.
pub struct LineSelector {
    selection: Option<LineSelection>,
    rows: Rc<RefCell<Vec<PaintedRow>>>,
    viewport: Rc<Cell<Bounds<Pixels>>>,
    focus: FocusHandle,
    dragging: bool,
    auto_scroll: AutoScroll,
}

impl LineSelector {
    pub fn new(cx: &mut App) -> Self {
        Self {
            selection: None,
            rows: Rc::default(),
            viewport: Rc::default(),
            focus: cx.focus_handle(),
            dragging: false,
            auto_scroll: AutoScroll::default(),
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// The selection, if it covers any text.
    pub fn selection(&self) -> Option<&LineSelection> {
        self.selection.as_ref().filter(|s| !s.is_empty())
    }

    pub fn has_selection(&self) -> bool {
        self.selection().is_some()
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging
    }

    pub fn set_selection(&mut self, selection: Option<LineSelection>) {
        self.selection = selection;
    }

    pub fn clear(&mut self) {
        self.selection = None;
        self.dragging = false;
        self.auto_scroll.stop();
    }

    /// Clips the selection to lines from `first` on; see
    /// [`LineSelection::retain_from`].
    pub fn retain_from(&mut self, first: u64) {
        if let Some(selection) = &mut self.selection
            && !selection.retain_from(first)
        {
            self.selection = None;
        }
    }

    /// Wraps the text of line `line` so the mouse can select in it. Call it
    /// for every row rendered this frame.
    pub fn row(&self, line: u64, text: StyledText) -> Div {
        let layout = text.layout().clone();
        let rows = self.rows.clone();
        div()
            .child(text)
            .on_children_prepainted(move |bounds, _, _| {
                let Some(bounds) = bounds.first().copied() else {
                    return;
                };
                let mut rows = rows.borrow_mut();
                rows.retain(|row| row.line != line);
                rows.push(PaintedRow {
                    line,
                    layout: layout.clone(),
                    bounds,
                });
            })
    }

    /// The selection highlight over line `line` (`len` bytes long), drawn
    /// with the theme's selection color.
    pub fn highlight(
        &self,
        line: u64,
        len: usize,
        cx: &App,
    ) -> Option<(Range<usize>, HighlightStyle)> {
        let columns = self.selection()?.columns(line, len)?;
        (!columns.is_empty()).then(|| {
            (
                columns,
                HighlightStyle {
                    background_color: Some(cx.theme().selection),
                    ..HighlightStyle::default()
                },
            )
        })
    }

    /// Forgets the rows of the previous frame; call at the start of the
    /// view's `render`.
    pub fn begin_frame(&self) {
        self.rows.borrow_mut().clear();
    }

    /// The text position under `position` (window coordinates); above or
    /// below the painted rows it is the start of the first or the end of the
    /// last one.
    pub fn hit(&self, position: Point<Pixels>) -> Option<TextPoint> {
        let rows = self.rows.borrow();
        let first = rows.iter().min_by_key(|row| row.line)?;
        let last = rows.iter().max_by_key(|row| row.line)?;
        if position.y < first.bounds.top() {
            return Some(TextPoint::new(first.line, 0));
        }
        if position.y >= last.bounds.bottom() {
            return Some(TextPoint::new(last.line, last.layout.len()));
        }
        let row = rows
            .iter()
            .find(|row| position.y >= row.bounds.top() && position.y < row.bounds.bottom())
            .or_else(|| {
                // In a gap between rows: the nearest one.
                rows.iter().min_by(|a, b| {
                    let distance = |row: &PaintedRow| (row.bounds.center().y - position.y).abs();
                    f32::from(distance(a)).total_cmp(&f32::from(distance(b)))
                })
            })?;
        let y = position
            .y
            .clamp(row.bounds.top(), row.bounds.bottom() - px(1.));
        let offset = match row.layout.index_for_position(point(position.x, y)) {
            Ok(offset) | Err(offset) => offset,
        };
        Some(TextPoint::new(row.line, offset.min(row.layout.len())))
    }
}

/// A view with selectable lines; see the [module docs](self).
pub trait SelectableLines: Render + Sized {
    fn line_selector(&self) -> &LineSelector;
    fn line_selector_mut(&mut self) -> &mut LineSelector;
    /// Ids of the lines, in order.
    fn line_ids(&self) -> Range<u64>;
    /// The text of line `line` as displayed (what offsets index into).
    fn line_text(&self, line: u64) -> Option<String>;
    /// Scrolls by `delta` (positive: down) while a drag is past an edge.
    fn scroll_by(&mut self, delta: Pixels, cx: &mut Context<Self>);
    /// Called after the selection changed.
    fn selection_changed(&mut self, _cx: &mut Context<Self>) {}
}

/// Makes `root` (the view's outermost element) handle selection: mouse
/// presses and drags, `secondary-c` / `secondary-a` / `escape`.
pub fn selectable<V: SelectableLines>(
    root: Div,
    selector: &LineSelector,
    cx: &mut Context<V>,
) -> Div {
    let focus = selector.focus.clone();
    let view = cx.entity().downgrade();
    let viewport = selector.viewport.clone();
    root.key_context(CONTEXT)
        .track_focus(&focus)
        .on_action(cx.listener(|this: &mut V, _: &CopySelection, _, cx| {
            if let Some(text) = selected_text(this) {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
        }))
        .on_action(cx.listener(|this: &mut V, _: &SelectAllLines, _, cx| select_all(this, cx)))
        .on_action(cx.listener(|this: &mut V, _: &ClearSelection, _, cx| {
            if this.line_selector().selection.is_some() {
                this.line_selector_mut().clear();
                this.selection_changed(cx);
                cx.notify();
            } else {
                cx.propagate();
            }
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this: &mut V, event: &MouseDownEvent, window, cx| {
                press(this, event, window, cx);
            }),
        )
        .child(
            canvas(
                move |bounds, _, _| viewport.set(bounds),
                move |_, _, window, _| {
                    // Window-wide so a drag keeps selecting outside the view.
                    let moves = view.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                        if phase == DispatchPhase::Bubble {
                            moves.update(cx, |this, cx| drag(this, event, cx)).ok();
                        }
                    });
                    let ups = view.clone();
                    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                        if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                            ups.update(cx, |this, cx| release(this, cx)).ok();
                        }
                    });
                },
            )
            .absolute()
            .size_full(),
        )
}

/// The selected text, if any.
pub fn selected_text<V: SelectableLines>(view: &V) -> Option<String> {
    let selection = view.line_selector().selection()?;
    Some(selection.text(|line| view.line_text(line)))
}

/// Selects every line.
pub fn select_all<V: SelectableLines>(view: &mut V, cx: &mut Context<V>) {
    let ids = view.line_ids();
    if ids.is_empty() {
        return;
    }
    let last = ids.end - 1;
    let len = view.line_text(last).map_or(0, |text| text.len());
    view.line_selector_mut()
        .set_selection(Some(LineSelection::all(ids.start, last, len)));
    view.selection_changed(cx);
    cx.notify();
}

fn press<V: SelectableLines>(
    view: &mut V,
    event: &MouseDownEvent,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    let focus = view.line_selector().focus.clone();
    window.focus(&focus, cx);
    let Some(at) = view.line_selector().hit(event.position) else {
        return;
    };
    let Some(text) = view.line_text(at.line) else {
        return;
    };
    let selector = view.line_selector_mut();
    match &mut selector.selection {
        Some(selection) if event.modifiers.shift => selection.extend(at, &text),
        _ => {
            let unit = SelectionUnit::from_click_count(event.click_count);
            selector.selection = Some(LineSelection::new(at, unit, &text));
        }
    }
    selector.dragging = true;
    view.selection_changed(cx);
    cx.notify();
}

fn drag<V: SelectableLines>(view: &mut V, event: &MouseMoveEvent, cx: &mut Context<V>) {
    if !view.line_selector().dragging {
        return;
    }
    if event.pressed_button != Some(MouseButton::Left) {
        // The release happened where we could not see it.
        release(view, cx);
        return;
    }
    let viewport = view.line_selector().viewport.get();
    let selector = view.line_selector_mut();
    selector.auto_scroll.last_drag_position = Some(event.position);
    let delta = AutoScroll::compute_delta(event.position.y, viewport);
    selector
        .auto_scroll
        .set(delta, cx, |delta, view: &mut V, cx| {
            view.scroll_by(delta, cx);
            if let Some(position) = view.line_selector().auto_scroll.last_drag_position {
                extend_to(view, position, cx);
            }
        });
    extend_to(view, event.position, cx);
}

fn extend_to<V: SelectableLines>(view: &mut V, position: Point<Pixels>, cx: &mut Context<V>) {
    let Some(at) = view.line_selector().hit(position) else {
        return;
    };
    let Some(text) = view.line_text(at.line) else {
        return;
    };
    let Some(selection) = &mut view.line_selector_mut().selection else {
        return;
    };
    let before = selection.clone();
    selection.extend(at, &text);
    if *selection != before {
        view.selection_changed(cx);
        cx.notify();
    }
}

fn release<V: SelectableLines>(view: &mut V, cx: &mut Context<V>) {
    let selector = view.line_selector_mut();
    if !selector.dragging {
        return;
    }
    selector.dragging = false;
    selector.auto_scroll.stop();
    // A click without a drag only places nothing; drop the empty selection.
    if selector
        .selection
        .as_ref()
        .is_some_and(LineSelection::is_empty)
    {
        selector.selection = None;
    }
    view.selection_changed(cx);
    cx.notify();
}

#[cfg(test)]
mod tests {
    use super::{LineSelection, SelectionUnit, TextPoint, word_range};

    fn at(line: u64, offset: usize) -> TextPoint {
        TextPoint::new(line, offset)
    }

    #[test]
    fn words_spaces_and_punctuation() {
        let text = "pod/web-1  ready_now ok";
        assert_eq!(word_range(text, 0), 0..3);
        assert_eq!(word_range(text, 2), 0..3);
        assert_eq!(word_range(text, 3), 3..4);
        assert_eq!(word_range(text, 4), 4..7);
        assert_eq!(word_range(text, 10), 9..11);
        assert_eq!(word_range(text, 13), 11..20);
        assert_eq!(word_range(text, text.len()), 21..23);
        assert_eq!(word_range("", 0), 0..0);
        let text = "ошибка: диск";
        assert_eq!(&text[word_range(text, 2)], "ошибка");
        assert_eq!(&text[word_range(text, 3)], "ошибка");
    }

    #[test]
    fn drag_selects_across_lines_in_either_direction() {
        let lines = ["first line", "second", "third line"];
        let text = |line: u64| lines.get(line as usize).copied();
        let mut selection = LineSelection::new(at(0, 6), SelectionUnit::Char, lines[0]);
        assert!(selection.is_empty());
        selection.extend(at(2, 5), lines[2]);
        assert_eq!(selection.text(text), "line\nsecond\nthird");
        assert_eq!(selection.columns(0, 10), Some(6..10));
        assert_eq!(selection.columns(1, 6), Some(0..6));
        assert_eq!(selection.columns(2, 10), Some(0..5));
        assert_eq!(selection.columns(3, 4), None);

        selection.extend(at(0, 2), lines[0]);
        assert_eq!(selection.text(text), "rst ");
    }

    #[test]
    fn word_and_line_units_extend_by_whole_units() {
        let lines = ["alpha beta gamma", "delta epsilon"];
        let text = |line: u64| lines.get(line as usize).copied();
        let mut selection = LineSelection::new(at(0, 7), SelectionUnit::Word, lines[0]);
        assert_eq!(selection.text(text), "beta");
        selection.extend(at(1, 8), lines[1]);
        assert_eq!(selection.text(text), "beta gamma\ndelta epsilon");
        selection.extend(at(0, 1), lines[0]);
        assert_eq!(selection.text(text), "alpha beta");

        let mut selection = LineSelection::new(at(1, 3), SelectionUnit::Line, lines[1]);
        assert_eq!(selection.text(text), "delta epsilon");
        selection.extend(at(0, 0), lines[0]);
        assert_eq!(selection.text(text), "alpha beta gamma\ndelta epsilon");
    }

    #[test]
    fn select_all_and_eviction() {
        let lines = ["a", "bb", "ccc"];
        let text = |line: u64| lines.get(line as usize).copied();
        let mut selection = LineSelection::all(0, 2, 3);
        assert_eq!(selection.text(text), "a\nbb\nccc");
        assert!(selection.retain_from(1));
        assert_eq!(selection.start(), at(1, 0));
        assert_eq!(selection.text(text), "bb\nccc");
        assert!(!selection.retain_from(3));
    }

    #[test]
    fn offsets_past_the_text_are_clamped() {
        let selection = LineSelection {
            anchor: at(0, 1)..at(0, 1),
            head: at(0, 99)..at(0, 99),
            unit: SelectionUnit::Char,
        };
        // Offset 1 splits `ж`: it rounds down to the char start.
        assert_eq!(selection.text(|_| Some("жук")), "жук");
        assert_eq!(selection.columns(0, 6), Some(1..6));
    }

    #[test]
    fn click_counts_pick_units() {
        assert_eq!(SelectionUnit::from_click_count(1), SelectionUnit::Char);
        assert_eq!(SelectionUnit::from_click_count(2), SelectionUnit::Word);
        assert_eq!(SelectionUnit::from_click_count(3), SelectionUnit::Line);
        assert_eq!(SelectionUnit::from_click_count(4), SelectionUnit::Line);
    }
}
