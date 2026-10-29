use std::cmp::Ordering;
use std::collections::HashSet;
use std::rc::Rc;

use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableEvent, TableState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::age::{AgeTicker, format_since, unix_now};
use crate::components::{StatusBadge, StatusCube, StatusCubes, TruncatedText};
use crate::measure::{TEXT_XS, text_width};
use crate::tokens::Tone;
use kubach_i18n::t;

actions!(resource_table, [CheckAll]);

const CONTEXT: &str = "ResourceTable";
const CHECK_COLUMN_WIDTH: Pixels = px(36.);
/// Header width besides the title: the sort icon with its gap and the cell
/// paddings.
const HEADER_CHROME: Pixels = px(36.);
/// Status labels longer than this may be clipped in a default-width Status
/// column, so their badge gets the full label as a tooltip.
const STATUS_TOOLTIP_CHARS: usize = 11;

/// Binds `cmd-a` / `ctrl-a` (check every row of a focused checkable table).
pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-a", CheckAll, Some(CONTEXT)),
        KeyBinding::new("ctrl-a", CheckAll, Some(CONTEXT)),
    ]);
}

/// Horizontal alignment of a column's header and cells.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CellAlign {
    #[default]
    Start,
    End,
}

/// A column of a [`ResourceTable`].
#[derive(Clone, Debug)]
pub struct ResourceColumn {
    key: SharedString,
    title: SharedString,
    width: Pixels,
    align: CellAlign,
    sortable: bool,
    /// Rows already come sorted ascending by this column.
    natural: bool,
}

impl ResourceColumn {
    /// `key` identifies the column, `title` is shown in the header.
    pub fn new(key: impl Into<SharedString>, title: impl Into<SharedString>) -> Self {
        Self {
            key: key.into(),
            title: title.into(),
            width: px(120.),
            align: CellAlign::Start,
            sortable: false,
            natural: false,
        }
    }

    /// The key the column was created with.
    pub fn key(&self) -> &SharedString {
        &self.key
    }

    /// Initial width, default 120 px. Columns stay resizable by dragging.
    pub fn width(mut self, width: impl Into<Pixels>) -> Self {
        self.width = width.into();
        self
    }

    /// Aligns the header and cells to the right edge, for numbers.
    pub fn align_end(mut self) -> Self {
        self.align = CellAlign::End;
        self
    }

    /// Lets a header click sort the rows by this column.
    pub fn sortable(mut self) -> Self {
        self.sortable = true;
        self
    }

    /// Sortable, and the rows already come sorted ascending by this column:
    /// unsorted, the header shows it ascending, and a click sorts descending.
    pub fn sorted(mut self) -> Self {
        self.sortable = true;
        self.natural = true;
        self
    }
}

/// Value a cell is sorted by. Numbers sort before text.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SortKey {
    Number(i64),
    Text(SharedString),
}

impl Ord for SortKey {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (SortKey::Number(a), SortKey::Number(b)) => a.cmp(b),
            (SortKey::Text(a), SortKey::Text(b)) => a.as_ref().cmp(b.as_ref()),
            (SortKey::Number(_), SortKey::Text(_)) => Ordering::Less,
            (SortKey::Text(_), SortKey::Number(_)) => Ordering::Greater,
        }
    }
}

impl PartialOrd for SortKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug, PartialEq)]
enum CellContent {
    Text(SharedString),
    Mono(SharedString),
    Status(SharedString, Tone),
    /// Time since a moment (Unix seconds), formatted when drawn.
    Age(i64),
    /// A count and how long ago the latest one was: `12 (3h ago)`.
    CountAge(i64, i64),
    /// Status cubes (init ones first, dimmed) and the text they stand for.
    Cubes {
        init: Rc<[StatusCube]>,
        cubes: Rc<[StatusCube]>,
        text: SharedString,
    },
}

impl CellContent {
    /// The drawn text at `now` (Unix seconds).
    fn text_at(&self, now: i64) -> SharedString {
        match self {
            CellContent::Text(text)
            | CellContent::Mono(text)
            | CellContent::Status(text, _)
            | CellContent::Cubes { text, .. } => text.clone(),
            CellContent::Age(since) => format_since(*since, now).into(),
            CellContent::CountAge(count, since) => t!(
                "ui-table-count-age",
                count = *count,
                age = format_since(*since, now)
            )
            .into(),
        }
    }
}

/// Draws what goes before a cell's content, e.g. an icon.
type LeadingFn = Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;

/// An element drawn before a cell's content; see [`ResourceCell::leading`].
#[derive(Clone)]
struct CellLeading(LeadingFn);

/// Same closure: rows rebuilt from the same data share none, so a cell with
/// one never equals its rebuilt self and is redrawn.
impl PartialEq for CellLeading {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for CellLeading {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CellLeading")
    }
}

/// One cell of a [`ResourceRow`]: what is drawn and what it sorts by.
#[derive(Clone, Debug, PartialEq)]
pub struct ResourceCell {
    content: CellContent,
    sort_key: SortKey,
    muted: bool,
    leading: Option<CellLeading>,
    trailing: Option<CellLeading>,
}

impl ResourceCell {
    fn with(content: CellContent, sort_key: SortKey) -> Self {
        Self {
            content,
            sort_key,
            muted: false,
            leading: None,
            trailing: None,
        }
    }

    /// Plain text in the UI font.
    pub fn text(text: impl Into<SharedString>) -> Self {
        let text = text.into();
        Self::with(CellContent::Text(text.clone()), SortKey::Text(text))
    }

    /// Text in the monospace font, e.g. `1/1` or an IP.
    pub fn mono(text: impl Into<SharedString>) -> Self {
        let text = text.into();
        Self::with(CellContent::Mono(text.clone()), SortKey::Text(text))
    }

    /// Monospace number, sorted numerically.
    pub fn number(value: i64) -> Self {
        Self::with(
            CellContent::Mono(value.to_string().into()),
            SortKey::Number(value),
        )
    }

    /// Time since `since` (Unix seconds) like `kubectl` prints ages (`45s`,
    /// `5h3m`, `12d`). It stays current: the table re-renders it as time
    /// passes. Sorted by age, i.e. by `since`, so the order never goes stale.
    pub fn age(since: i64) -> Self {
        Self::with(
            CellContent::Age(since),
            SortKey::Number(since.saturating_neg()),
        )
    }

    /// A count with the time of the latest occurrence (Unix seconds), like
    /// restarts: `12 (3h ago)`. Live like [`age`](Self::age); sorted by count.
    pub fn count_age(count: i64, since: i64) -> Self {
        Self::with(CellContent::CountAge(count, since), SortKey::Number(count))
    }

    /// A [`StatusBadge`], sorted by label.
    pub fn status(label: impl Into<SharedString>, tone: Tone) -> Self {
        let label = label.into();
        Self::with(
            CellContent::Status(label.clone(), tone),
            SortKey::Text(label),
        )
    }

    /// A row of [`StatusCubes`], `init` ones first and dimmed, e.g. one per
    /// container of a pod. Its text is the first tooltip line of every cube
    /// (the container names); it sorts by the number of cubes unless
    /// [`sort_by`](Self::sort_by) says otherwise.
    pub fn cubes(
        init: impl IntoIterator<Item = StatusCube>,
        cubes: impl IntoIterator<Item = StatusCube>,
    ) -> Self {
        let init: Rc<[StatusCube]> = init.into_iter().collect();
        let cubes: Rc<[StatusCube]> = cubes.into_iter().collect();
        let text = init
            .iter()
            .chain(cubes.iter())
            .map(|cube| cube.tooltip.lines().next().unwrap_or_default())
            .collect::<Vec<_>>()
            .join(", ");
        let count = (init.len() + cubes.len()) as i64;
        Self::with(
            CellContent::Cubes {
                init,
                cubes,
                text: text.into(),
            },
            SortKey::Number(count),
        )
    }

    /// Draws the cell in the muted text color.
    pub fn muted(mut self) -> Self {
        self.muted = true;
        self
    }

    /// Draws `leading` before the content, e.g. a [`ChartIcon`](crate::ChartIcon).
    /// It runs only when the cell is drawn, i.e. for visible rows, so it
    /// may start loading what it shows.
    pub fn leading(
        mut self,
        leading: impl Fn(&mut Window, &mut App) -> AnyElement + 'static,
    ) -> Self {
        self.leading = Some(CellLeading(Rc::new(leading)));
        self
    }

    /// Draws `trailing` after the content, e.g. a badge. Like
    /// [`leading`](Self::leading), it runs only for visible rows.
    pub fn trailing(
        mut self,
        trailing: impl Fn(&mut Window, &mut App) -> AnyElement + 'static,
    ) -> Self {
        self.trailing = Some(CellLeading(Rc::new(trailing)));
        self
    }

    /// Sorts the cell by `value` instead of its text.
    pub fn sort_by(mut self, value: i64) -> Self {
        self.sort_key = SortKey::Number(value);
        self
    }

    /// The text the cell shows now.
    pub fn label(&self) -> SharedString {
        self.content.text_at(unix_now())
    }

    /// The moment a live cell counts from (Unix seconds).
    fn since(&self) -> Option<i64> {
        match self.content {
            CellContent::Age(since) | CellContent::CountAge(_, since) => Some(since),
            _ => None,
        }
    }
}

/// A row of a [`ResourceTable`], one cell per column.
#[derive(Clone, Debug, PartialEq)]
pub struct ResourceRow {
    /// Stable identity (e.g. `namespace/name` or a UID) that keeps the
    /// selection across sorting and [`ResourceTable::set_rows`].
    pub key: SharedString,
    pub cells: Vec<ResourceCell>,
}

impl ResourceRow {
    pub fn new(
        key: impl Into<SharedString>,
        cells: impl IntoIterator<Item = ResourceCell>,
    ) -> Self {
        Self {
            key: key.into(),
            cells: cells.into_iter().collect(),
        }
    }
}

/// Direction a [`ResourceTable`] is sorted in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortDirection {
    Ascending,
    Descending,
}

/// What a [`ResourceTable`] is sorted by: a column [`key`](ResourceColumn::key)
/// and a direction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnSort {
    pub column: SharedString,
    pub direction: SortDirection,
}

/// State of a checkbox, or of the header checkbox over all rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckState {
    Unchecked,
    /// Some rows, not all (header only).
    Partial,
    Checked,
}

/// Checked rows, by key, independent of their order in the view.
#[derive(Default, Debug)]
struct CheckSet {
    keys: HashSet<SharedString>,
    /// Where a shift-click range starts: the last row toggled.
    anchor: Option<SharedString>,
}

impl CheckSet {
    fn contains(&self, key: &SharedString) -> bool {
        self.keys.contains(key)
    }

    fn toggle(&mut self, key: &SharedString) {
        if !self.keys.remove(key) {
            self.keys.insert(key.clone());
        }
        self.anchor = Some(key.clone());
    }

    /// Checks every row between the anchor and `view[to]`, both included.
    fn check_range(&mut self, view: &[SharedString], to: usize) {
        let Some(target) = view.get(to) else {
            return;
        };
        let from = self
            .anchor
            .as_ref()
            .and_then(|anchor| view.iter().position(|key| key == anchor))
            .unwrap_or(to);
        let (start, end) = (from.min(to), from.max(to));
        self.keys.extend(view[start..=end].iter().cloned());
        if from == to {
            self.anchor = Some(target.clone());
        }
    }

    fn check_all(&mut self, view: &[SharedString]) {
        self.keys = view.iter().cloned().collect();
    }

    fn clear(&mut self) -> bool {
        self.anchor = None;
        let changed = !self.keys.is_empty();
        self.keys.clear();
        changed
    }

    /// Drops keys that are no longer in `view`; returns whether any were.
    fn retain(&mut self, view: &[SharedString]) -> bool {
        let present: HashSet<&SharedString> = view.iter().collect();
        let before = self.keys.len();
        self.keys.retain(|key| present.contains(key));
        if self.anchor.as_ref().is_some_and(|a| !present.contains(a)) {
            self.anchor = None;
        }
        self.keys.len() != before
    }

    /// Header state for a view of `total` rows (the set is a subset of it).
    fn state(&self, total: usize) -> CheckState {
        match self.keys.len() {
            0 => CheckState::Unchecked,
            n if n >= total => CheckState::Checked,
            _ => CheckState::Partial,
        }
    }
}

type CheckedHandler = Rc<dyn Fn(&mut App)>;
type ResetHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// Table data: rows in caller order plus the sorted view over them.
///
/// Columns and cells stay in caller order; `display` maps the shown order
/// onto them, so moving a column moves its cells, sort, width and kind along.
struct ResourceTableDelegate {
    columns: Vec<ResourceColumn>,
    /// Indices into `columns` in display order.
    display: Vec<usize>,
    /// Columns can be dragged by their header.
    movable: bool,
    /// Header menu "Reset column order".
    on_reset: Option<ResetHandler>,
    rows: Vec<ResourceRow>,
    /// Indices into `rows` in display order.
    order: Vec<usize>,
    sort: Option<(usize, SortDirection)>,
    /// Adds the checkbox column in front of `columns`.
    checkable: bool,
    checks: CheckSet,
    /// Tells the owning table that the checked rows changed.
    on_checked: Option<CheckedHandler>,
    /// Tells the owning table that a header click changed the sort.
    on_sorted: Option<CheckedHandler>,
    /// Re-renders the table while it shows ages.
    ages: AgeTicker,
}

impl ResourceTableDelegate {
    fn new(columns: Vec<ResourceColumn>) -> Self {
        Self {
            display: (0..columns.len()).collect(),
            columns,
            movable: false,
            on_reset: None,
            rows: Vec::new(),
            order: Vec::new(),
            sort: None,
            checkable: false,
            checks: CheckSet::default(),
            on_checked: None,
            on_sorted: None,
            ages: AgeTicker::default(),
        }
    }

    /// The most recent moment a live cell counts from.
    fn youngest(&self) -> Option<i64> {
        self.rows
            .iter()
            .flat_map(|row| &row.cells)
            .filter_map(ResourceCell::since)
            .max()
    }

    /// Replaces the rows; returns whether checked rows disappeared.
    fn set_rows(&mut self, rows: Vec<ResourceRow>) -> bool {
        self.rows = rows;
        self.apply_sort();
        let view = self.view_keys();
        self.checks.retain(&view)
    }

    /// Row keys in display order.
    fn view_keys(&self) -> Vec<SharedString> {
        self.order
            .iter()
            .map(|&ix| self.rows[ix].key.clone())
            .collect()
    }

    /// Checked keys in display order.
    fn checked_keys(&self) -> Vec<SharedString> {
        self.order
            .iter()
            .map(|&ix| &self.rows[ix].key)
            .filter(|key| self.checks.contains(key))
            .cloned()
            .collect()
    }

    fn is_checked(&self, view_ix: usize) -> bool {
        self.row(view_ix)
            .is_some_and(|row| self.checks.contains(&row.key))
    }

    /// Toggles the row, or with `range` checks everything from the anchor.
    fn check_row(&mut self, view_ix: usize, range: bool) {
        if range {
            let view = self.view_keys();
            self.checks.check_range(&view, view_ix);
        } else if let Some(key) = self.row(view_ix).map(|row| row.key.clone()) {
            self.checks.toggle(&key);
        }
    }

    fn check_all(&mut self) {
        let view = self.view_keys();
        self.checks.check_all(&view);
    }

    /// Header checkbox: checks all rows unless all are checked already.
    fn toggle_all(&mut self) {
        if self.check_state() == CheckState::Checked {
            self.checks.clear();
        } else {
            self.check_all();
        }
    }

    fn check_state(&self) -> CheckState {
        self.checks.state(self.order.len())
    }

    /// Index into `columns` of a table column, `None` for the checkbox one.
    fn data_col(&self, col_ix: usize) -> Option<usize> {
        self.display_pos(col_ix)
            .and_then(|pos| self.display.get(pos).copied())
    }

    /// Position in `display` of a table column, `None` for the checkbox one.
    fn display_pos(&self, col_ix: usize) -> Option<usize> {
        if self.checkable {
            col_ix.checked_sub(1)
        } else {
            Some(col_ix)
        }
    }

    /// Moves the table column `col_ix` to `to_ix`, as a header drag does.
    fn move_display(&mut self, col_ix: usize, to_ix: usize) {
        // The table keeps a moved column within its region, so the fixed
        // checkbox column never takes part.
        let (Some(from), Some(to)) = (self.display_pos(col_ix), self.display_pos(to_ix)) else {
            return;
        };
        if from < self.display.len() && to < self.display.len() {
            let ix = self.display.remove(from);
            self.display.insert(to, ix);
        }
    }

    /// Column keys in display order.
    fn order_keys(&self) -> Vec<SharedString> {
        self.display
            .iter()
            .map(|&ix| self.columns[ix].key.clone())
            .collect()
    }

    /// Display order for `keys`: known keys first, once each, then the
    /// columns `keys` misses in caller order.
    fn display_for(&self, keys: &[impl AsRef<str>]) -> Vec<usize> {
        let mut display = Vec::with_capacity(self.columns.len());
        let known = keys.iter().filter_map(|key| {
            self.columns
                .iter()
                .position(|column| column.key.as_ref() == key.as_ref())
        });
        for ix in known.chain(0..self.columns.len()) {
            if !display.contains(&ix) {
                display.push(ix);
            }
        }
        display
    }

    fn checked_changed(&self, cx: &mut Context<TableState<Self>>) {
        cx.notify();
        if let Some(handler) = self.on_checked.clone() {
            cx.defer(move |cx| handler(cx));
        }
    }

    /// Cycles the column through ascending, descending and unsorted.
    /// A column the rows come sorted by skips ascending: unsorted is that.
    fn toggle_sort(&mut self, col_ix: usize) {
        let natural = self.columns[col_ix].natural;
        self.sort = match self.sort {
            Some((ix, SortDirection::Ascending)) if ix == col_ix => {
                Some((col_ix, SortDirection::Descending))
            }
            Some((ix, SortDirection::Descending)) if ix == col_ix => None,
            None if natural => Some((col_ix, SortDirection::Descending)),
            _ if natural => None,
            _ => Some((col_ix, SortDirection::Ascending)),
        };
        self.apply_sort();
    }

    /// The sort as a column key; `None` in the caller's order.
    fn column_sort(&self) -> Option<ColumnSort> {
        let (ix, direction) = self.sort?;
        Some(ColumnSort {
            column: self.columns.get(ix)?.key.clone(),
            direction,
        })
    }

    /// Sorts by the column `key`. A column that is gone or not sortable
    /// leaves the caller's order, as does `None`. Ascending by the column
    /// the rows come sorted by is that order too.
    fn set_column_sort(&mut self, sort: Option<(&str, SortDirection)>) {
        self.sort = sort.and_then(|(key, direction)| {
            let ix = self
                .columns
                .iter()
                .position(|column| column.sortable && column.key.as_ref() == key)?;
            let natural = self.columns[ix].natural && direction == SortDirection::Ascending;
            (!natural).then_some((ix, direction))
        });
        self.apply_sort();
    }

    fn apply_sort(&mut self) {
        self.order = (0..self.rows.len()).collect();
        let Some((col_ix, direction)) = self.sort else {
            return;
        };
        let key = |row: usize| self.rows[row].cells.get(col_ix).map(|cell| &cell.sort_key);
        // Stable, so equal keys keep the caller's order.
        self.order.sort_by(|&a, &b| {
            let ordering = key(a).cmp(&key(b));
            match direction {
                SortDirection::Ascending => ordering,
                SortDirection::Descending => ordering.reverse(),
            }
        });
    }

    fn row(&self, view_ix: usize) -> Option<&ResourceRow> {
        self.order.get(view_ix).map(|&ix| &self.rows[ix])
    }

    fn view_index(&self, key: &SharedString) -> Option<usize> {
        self.order.iter().position(|&ix| &self.rows[ix].key == key)
    }

    fn cell(&self, view_ix: usize, col_ix: usize) -> Option<&ResourceCell> {
        self.row(view_ix).and_then(|row| row.cells.get(col_ix))
    }
}

/// Reselects the row with `key` after the rows moved, or clears the selection.
fn restore_selection(
    state: &mut TableState<ResourceTableDelegate>,
    key: Option<SharedString>,
    cx: &mut Context<TableState<ResourceTableDelegate>>,
) {
    let Some(key) = key else {
        return;
    };
    match state.delegate().view_index(&key) {
        // Setting the row scrolls to it, so leave a row that did not move.
        Some(ix) if state.selected_row() == Some(ix) => {}
        Some(ix) => state.set_selected_row(ix, cx),
        None => state.clear_selection(cx),
    }
}

/// Shows the columns in `display` order (indices into `columns`), keeping the
/// widths they were resized to; returns whether the order changed.
fn reorder(
    state: &mut TableState<ResourceTableDelegate>,
    display: Vec<usize>,
    cx: &mut Context<TableState<ResourceTableDelegate>>,
) -> bool {
    if state.delegate().display == display {
        return false;
    }
    // The table rebuilds its columns from the delegate on refresh, so carry
    // the current widths over first.
    let offset = usize::from(state.delegate().checkable);
    let widths: Vec<Option<Pixels>> = (0..display.len())
        .map(|pos| state.col_width(pos + offset))
        .collect();
    let delegate = state.delegate_mut();
    for (pos, width) in widths.into_iter().enumerate() {
        if let (Some(width), Some(&ix)) = (width, delegate.display.get(pos)) {
            delegate.columns[ix].width = width;
        }
    }
    delegate.display = display;
    state.refresh(cx);
    cx.notify();
    true
}

fn selected_key(state: &TableState<ResourceTableDelegate>) -> Option<SharedString> {
    state
        .selected_row()
        .and_then(|ix| state.delegate().row(ix))
        .map(|row| row.key.clone())
}

/// A small checkbox with a tri-state look; the caller handles clicks.
fn check_box(state: CheckState, cx: &App) -> Div {
    let theme = cx.theme();
    let filled = state != CheckState::Unchecked;
    let icon = match state {
        CheckState::Unchecked => None,
        CheckState::Partial => Some(IconName::Minus),
        CheckState::Checked => Some(IconName::Check),
    };
    div()
        .size(px(14.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.))
        .border_1()
        .map(|this| {
            if filled {
                this.bg(theme.primary).border_color(theme.primary)
            } else {
                this.bg(theme.background).border_color(theme.input)
            }
        })
        .children(icon.map(|icon| {
            Icon::new(icon)
                .size(px(10.))
                .text_color(theme.primary_foreground)
        }))
}

impl TableDelegate for ResourceTableDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len() + usize::from(self.checkable)
    }

    fn rows_count(&self, _: &App) -> usize {
        self.order.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        let Some(col_ix) = self.data_col(col_ix) else {
            return Column::new("__checked", "")
                .width(CHECK_COLUMN_WIDTH)
                .fixed_left()
                .resizable(false)
                .movable(false)
                .selectable(false)
                .p_0();
        };
        let column = &self.columns[col_ix];
        // Sorting is handled here rather than by the table, so a click
        // anywhere on the header sorts and the selection follows its row.
        Column::new(column.key.clone(), column.title.clone())
            .width(column.width)
            .movable(self.movable)
            .selectable(false)
            .when(column.align == CellAlign::End, |column| column.text_right())
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(col_ix) = self.data_col(col_ix) else {
            // Fixed width rather than `size_full`: the table narrows the
            // header cell for a sort icon, which would shift the checkbox
            // off the row checkboxes below.
            return div()
                .id("resource-th-check")
                .w(CHECK_COLUMN_WIDTH)
                .h_full()
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .child(check_box(self.check_state(), cx))
                .on_click(cx.listener(|state, _: &ClickEvent, _, cx| {
                    state.delegate_mut().toggle_all();
                    state.delegate().checked_changed(cx);
                }))
                .into_any_element();
        };
        let column = &self.columns[col_ix];
        let theme = cx.theme();
        let icon = column.sortable.then_some(match self.sort {
            Some((ix, SortDirection::Ascending)) if ix == col_ix => (IconName::SortAscending, true),
            Some((ix, SortDirection::Descending)) if ix == col_ix => {
                (IconName::SortDescending, true)
            }
            None if column.natural => (IconName::SortAscending, true),
            _ => (IconName::ChevronsUpDown, false),
        });
        let th = div()
            .id(("resource-th", col_ix))
            .size_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap_1()
            .when(column.align == CellAlign::End, |th| th.justify_end())
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.muted_foreground)
            .child(TruncatedText::new("title", column.title.clone()))
            .when_some(icon, |th, (icon, active)| {
                let color = if active {
                    theme.foreground
                } else {
                    theme.muted_foreground.opacity(0.5)
                };
                th.child(Icon::new(icon).size_3().flex_shrink_0().text_color(color))
                    .cursor_pointer()
                    .on_click(cx.listener(move |state, _: &ClickEvent, _, cx| {
                        let selected = selected_key(state);
                        state.delegate_mut().toggle_sort(col_ix);
                        restore_selection(state, selected, cx);
                        if let Some(handler) = state.delegate().on_sorted.clone() {
                            cx.defer(move |cx| handler(cx));
                        }
                        cx.notify();
                    }))
            });
        match self.on_reset.clone().filter(|_| self.movable) {
            Some(reset) => th
                .context_menu(move |menu, _, _| {
                    let reset = reset.clone();
                    menu.item(
                        PopupMenuItem::new(t!("ui-table-reset-columns"))
                            .on_click(move |_, window, cx| reset(window, cx)),
                    )
                })
                .into_any_element(),
            None => th.into_any_element(),
        }
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let tr = div().id(("row", row_ix));
        if !self.checkable || row_ix >= self.order.len() {
            return tr;
        }
        // Checked rows get a faint tint; the row open in details keeps the
        // table's own (stronger) selection background on top of it.
        let tint = cx.theme().primary.opacity(0.08);
        tr.when(self.is_checked(row_ix), |tr| tr.bg(tint))
            .on_click(cx.listener(move |state, event: &ClickEvent, _, cx| {
                let modifiers = event.modifiers();
                if !(modifiers.shift || modifiers.secondary()) {
                    return;
                }
                state.delegate_mut().check_row(row_ix, modifiers.shift);
                state.delegate().checked_changed(cx);
            }))
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(col_ix) = self.data_col(col_ix) else {
            let state = if self.is_checked(row_ix) {
                CheckState::Checked
            } else {
                CheckState::Unchecked
            };
            return div()
                .id(("resource-check", row_ix))
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .child(check_box(state, cx))
                .on_click(cx.listener(move |state, event: &ClickEvent, _, cx| {
                    // Keep the row from opening in details.
                    cx.stop_propagation();
                    state
                        .delegate_mut()
                        .check_row(row_ix, event.modifiers().shift);
                    state.delegate().checked_changed(cx);
                }))
                .into_any_element();
        };
        let theme = cx.theme();
        let align = self.columns[col_ix].align;
        let td = div()
            .size_full()
            .min_w_0()
            .flex()
            .items_center()
            .when(align == CellAlign::End, |td| td.justify_end());
        let Some(cell) = self.cell(row_ix, col_ix) else {
            return td.into_any_element();
        };
        let content = match &cell.content {
            CellContent::Text(text) => div().min_w_0().truncate().child(text.clone()),
            CellContent::Mono(text) => div()
                .min_w_0()
                .truncate()
                .font_family(theme.mono_font_family.clone())
                .child(text.clone()),
            CellContent::Status(label, tone) => {
                let tooltip = (label.chars().count() > STATUS_TOOLTIP_CHARS).then(|| label.clone());
                let badge = div()
                    .id(SharedString::from(format!(
                        "resource-status-{row_ix}-{col_ix}"
                    )))
                    .min_w_0()
                    .flex()
                    .overflow_hidden()
                    .child(StatusBadge::new(label.clone(), *tone).shrink())
                    .when_some(tooltip, |badge, tooltip| {
                        badge.tooltip(move |window, cx| {
                            Tooltip::new(tooltip.clone()).build(window, cx)
                        })
                    });
                div().min_w_0().flex().child(badge)
            }
            CellContent::Cubes { init, cubes, .. } => div().min_w_0().overflow_hidden().child(
                StatusCubes::new(
                    SharedString::from(format!("resource-cubes-{row_ix}-{col_ix}")),
                    cubes.iter().cloned(),
                )
                .init(init.iter().cloned())
                .single_line(),
            ),
            live @ (CellContent::Age(_) | CellContent::CountAge(..)) => div()
                .min_w_0()
                .truncate()
                .font_family(theme.mono_font_family.clone())
                .child(live.text_at(unix_now())),
        };
        let muted = cell.muted.then_some(theme.muted_foreground);
        let leading = cell.leading.clone();
        let trailing = cell.trailing.clone();
        td.when_some(muted, |td, color| td.text_color(color))
            .when_some(leading, |td, leading| {
                td.gap_2().child((leading.0)(window, cx))
            })
            .child(content)
            .when_some(trailing, |td, trailing| {
                td.gap_2().child((trailing.0)(window, cx))
            })
            .into_any_element()
    }

    fn move_column(
        &mut self,
        col_ix: usize,
        to_ix: usize,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) {
        self.move_display(col_ix, to_ix);
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        self.data_col(col_ix)
            .and_then(|col_ix| self.cell(row_ix, col_ix))
            .map(|cell| cell.label().to_string())
            .unwrap_or_default()
    }
}

type SelectHandler = Rc<dyn Fn(&ResourceRow, &mut Window, &mut App)>;

/// Dense, virtualized resource table on gpui-component's `DataTable`.
///
/// Columns and rows come from the caller and the table knows nothing about
/// Kubernetes. Sortable columns sort on a header click (ascending, descending,
/// then back to the caller's order); only visible rows are rendered, so
/// thousands of rows stay smooth. Give it a bounded height.
///
/// With [`checkable`](Self::checkable) rows also get checkboxes, separate
/// from the selected (clicked) row: a header checkbox checks all, shift-click
/// checks a range, cmd/ctrl-click toggles a row and `cmd-a` / `ctrl-a` checks
/// every row. Checks follow row keys across sorting and
/// [`set_rows`](Self::set_rows); rows that disappear are unchecked.
///
/// With [`movable_columns`](Self::movable_columns) columns can be dragged by
/// their header (see there).
pub struct ResourceTable {
    state: Entity<TableState<ResourceTableDelegate>>,
    on_select: Option<SelectHandler>,
    selected: Option<SharedString>,
    _subscriptions: [Subscription; 2],
}

/// Events of a [`ResourceTable`].
#[derive(Clone, Debug)]
pub enum ResourceTableEvent {
    /// The checked rows changed; holds their keys in display order.
    CheckedChanged(Vec<SharedString>),
}

impl EventEmitter<ResourceTableEvent> for ResourceTable {}

/// The user rearranged the columns of a table with
/// [`movable_columns`](ResourceTable::movable_columns).
#[derive(Clone, Debug)]
pub enum ColumnOrderEvent {
    /// A column was dragged; holds every column key in the new order.
    Moved(Vec<SharedString>),
    /// "Reset column order" in the header menu: back to the caller's order.
    Reset,
}

impl EventEmitter<ColumnOrderEvent> for ResourceTable {}

/// The user changed the sort of a [`ResourceTable`] by a header click.
#[derive(Clone, Debug)]
pub enum TableSortEvent {
    /// Holds the new sort; `None` is the caller's (default) order.
    Changed(Option<ColumnSort>),
}

impl EventEmitter<TableSortEvent> for ResourceTable {}

impl ResourceTable {
    pub fn new(
        mut columns: Vec<ResourceColumn>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // A column starts at least as wide as its header, so a translated
        // title isn't cut (#444); dragging can still narrow it.
        for column in &mut columns {
            let title = text_width(&column.title, TEXT_XS, FontWeight::MEDIUM, window, cx);
            column.width = column
                .width
                .max(px(f32::from(title).ceil()) + HEADER_CHROME);
        }
        let mut delegate = ResourceTableDelegate::new(columns);
        let owner = cx.weak_entity();
        delegate.on_checked = Some(Rc::new(move |cx: &mut App| {
            owner.update(cx, |table, cx| table.emit_checked(cx)).ok();
        }));
        let owner = cx.weak_entity();
        delegate.on_sorted = Some(Rc::new(move |cx: &mut App| {
            owner
                .update(cx, |table, cx| {
                    let sort = table.sort(cx);
                    cx.emit(TableSortEvent::Changed(sort));
                })
                .ok();
        }));
        let owner = cx.weak_entity();
        delegate.on_reset = Some(Rc::new(move |_: &mut Window, cx: &mut App| {
            owner
                .update(cx, |table, cx| table.reset_column_order(cx))
                .ok();
        }));
        // Columns opt in to moving (`movable_columns`); the checkbox column
        // never does.
        let state = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .col_movable(true)
                .col_selectable(false)
        });
        let subscriptions = [
            cx.subscribe_in(&state, window, Self::on_table_event),
            // Rows, ages, scrolling and sorting change the state: the table
            // redraws with it, so an owner can embed it cached (#363).
            cx.observe(&state, |_, _, cx| cx.notify()),
        ];
        Self {
            state,
            on_select: None,
            selected: None,
            _subscriptions: subscriptions,
        }
    }

    /// Called when the user selects a row, by click or keyboard.
    pub fn on_select(
        mut self,
        handler: impl Fn(&ResourceRow, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }

    /// Adds the checkbox column (see the type docs).
    pub fn checkable(self, cx: &mut App) -> Self {
        self.state.update(cx, |state, cx| {
            state.delegate_mut().checkable = true;
            state.refresh(cx);
        });
        self
    }

    /// Lets the user drag columns by their header, and reset their order
    /// from the header's context menu. Cells, sort and widths follow their
    /// column; the checkbox column stays first. Changes are reported as
    /// [`ColumnOrderEvent`]s, so the caller can remember the order and pass
    /// it back with [`set_column_order`](Self::set_column_order).
    pub fn movable_columns(self, cx: &mut App) -> Self {
        self.state.update(cx, |state, cx| {
            state.delegate_mut().movable = true;
            state.refresh(cx);
        });
        self
    }

    /// Shows the columns in the order of `keys`; unknown keys are ignored,
    /// columns `keys` misses follow in the order given to [`new`](Self::new).
    /// Emits nothing.
    pub fn set_column_order(&mut self, keys: &[impl AsRef<str>], cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            let display = state.delegate().display_for(keys);
            reorder(state, display, cx);
        });
    }

    /// Column keys in display order.
    pub fn column_order(&self, cx: &App) -> Vec<SharedString> {
        self.state.read(cx).delegate().order_keys()
    }

    /// Shows the columns in the order given to [`new`](Self::new) and emits
    /// [`ColumnOrderEvent::Reset`].
    pub fn reset_column_order(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            let display = (0..state.delegate().columns.len()).collect();
            reorder(state, display, cx);
        });
        cx.emit(ColumnOrderEvent::Reset);
        cx.notify();
    }

    /// What the rows are sorted by; `None` in the caller's order.
    pub fn sort(&self, cx: &App) -> Option<ColumnSort> {
        self.state.read(cx).delegate().column_sort()
    }

    /// Sorts by the column with key `column`, e.g. a remembered sort. A
    /// column that is gone or not sortable, or `None`, gives the caller's
    /// order. Keeps the selected row; emits nothing.
    pub fn set_sort(&mut self, sort: Option<(&str, SortDirection)>, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            let selected = selected_key(state);
            state.delegate_mut().set_column_sort(sort);
            restore_selection(state, selected, cx);
            cx.notify();
        });
    }

    /// Replaces the rows, keeping the sort, the selected row and the checked
    /// rows (by key).
    ///
    /// Returns whether anything changed: the same rows again (a poll or
    /// timer with nothing new) leave the table as it is, without a redraw.
    pub fn set_rows(&mut self, rows: Vec<ResourceRow>, cx: &mut Context<Self>) -> bool {
        if self.state.read(cx).delegate().rows == rows {
            return false;
        }
        let unchecked = self.state.update(cx, |state, cx| {
            let selected = selected_key(state);
            let unchecked = state.delegate_mut().set_rows(rows);
            let youngest = state.delegate().youngest();
            state.delegate_mut().ages.track(youngest, cx);
            restore_selection(state, selected, cx);
            cx.notify();
            unchecked
        });
        if unchecked {
            self.emit_checked(cx);
        }
        true
    }

    /// Keys of the checked rows, in display order.
    pub fn checked(&self, cx: &App) -> Vec<SharedString> {
        self.state.read(cx).delegate().checked_keys()
    }

    /// Checks every row.
    pub fn check_all(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.delegate_mut().check_all();
            cx.notify();
        });
        self.emit_checked(cx);
    }

    /// Unchecks every row.
    pub fn clear_checked(&mut self, cx: &mut Context<Self>) {
        let changed = self.state.update(cx, |state, cx| {
            cx.notify();
            state.delegate_mut().checks.clear()
        });
        if changed {
            self.emit_checked(cx);
        }
    }

    fn emit_checked(&mut self, cx: &mut Context<Self>) {
        let keys = self.checked(cx);
        cx.emit(ResourceTableEvent::CheckedChanged(keys));
        cx.notify();
    }

    /// Deselects the row, e.g. when its details panel is closed, so clicking
    /// it again counts as a new selection.
    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.state.update(cx, |state, cx| state.clear_selection(cx));
    }

    /// Key of the selected row.
    pub fn selected(&self) -> Option<&SharedString> {
        self.selected.as_ref()
    }

    fn on_table_event(
        &mut self,
        state: &Entity<TableState<ResourceTableDelegate>>,
        event: &TableEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TableEvent::SelectRow(ix) => {
                let Some(row) = state.read(cx).delegate().row(*ix).cloned() else {
                    return;
                };
                // Re-selecting the same row after a sort is not a new choice.
                if self.selected.as_ref() == Some(&row.key) {
                    return;
                }
                self.selected = Some(row.key.clone());
                if let Some(handler) = self.on_select.clone() {
                    handler(&row, window, cx);
                }
            }
            TableEvent::ClearSelection => self.selected = None,
            TableEvent::MoveColumn(..) => {
                let order = self.column_order(cx);
                cx.emit(ColumnOrderEvent::Moved(order));
            }
            _ => {}
        }
    }
}

impl Render for ResourceTable {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let checkable = self.state.read(cx).delegate().checkable;
        div()
            .size_full()
            .when(checkable, |this| {
                this.key_context(CONTEXT)
                    .on_action(cx.listener(|this, _: &CheckAll, _, cx| this.check_all(cx)))
            })
            .child(DataTable::new(&self.state))
    }
}

#[cfg(test)]
mod tests {
    // Not a glob: `gpui_kit::*` exports a `test` attribute that would shadow std's.
    use super::{
        CheckState, ColumnSort, ResourceCell, ResourceColumn, ResourceRow, ResourceTableDelegate,
        SortDirection,
    };

    fn delegate() -> ResourceTableDelegate {
        let mut delegate = ResourceTableDelegate::new(vec![
            ResourceColumn::new("name", "Name").sortable(),
            ResourceColumn::new("restarts", "Restarts").sortable(),
        ]);
        delegate.set_rows(vec![
            ResourceRow::new("b", [ResourceCell::text("b"), ResourceCell::number(10)]),
            ResourceRow::new("a", [ResourceCell::text("a"), ResourceCell::number(2)]),
            ResourceRow::new("c", [ResourceCell::text("c"), ResourceCell::number(2)]),
        ]);
        delegate
    }

    fn keys(delegate: &ResourceTableDelegate) -> Vec<&str> {
        (0..delegate.order.len())
            .map(|ix| delegate.row(ix).unwrap().key.as_ref())
            .collect()
    }

    #[test]
    fn rebuilt_rows_equal_until_something_shown_changes() {
        use gpui_kit::{Empty, IntoElement as _};

        let row = |since, restarts| {
            ResourceRow::new(
                "web/app",
                [
                    ResourceCell::text("app"),
                    ResourceCell::age(since),
                    ResourceCell::count_age(restarts, since),
                ],
            )
        };
        // A timer or poll rebuilding the same rows: nothing to redraw.
        assert_eq!(row(1_000, 0), row(1_000, 0));
        assert_ne!(row(1_000, 0), row(1_000, 1));
        assert_ne!(row(1_000, 0), row(2_000, 0));
        // Leading elements are closures: only the same one compares equal.
        let leading = ResourceCell::text("x").leading(|_, _| Empty.into_any_element());
        assert_eq!(leading, leading.clone());
        assert_ne!(
            leading,
            ResourceCell::text("x").leading(|_, _| Empty.into_any_element())
        );
    }

    #[test]
    fn header_clicks_cycle_through_sort_orders() {
        let mut delegate = delegate();
        assert_eq!(keys(&delegate), ["b", "a", "c"]);
        delegate.toggle_sort(0);
        assert_eq!(keys(&delegate), ["a", "b", "c"]);
        delegate.toggle_sort(0);
        assert_eq!(keys(&delegate), ["c", "b", "a"]);
        delegate.toggle_sort(0);
        assert_eq!(keys(&delegate), ["b", "a", "c"]);
    }

    #[test]
    fn sort_is_set_and_read_by_column_key() {
        let mut delegate = delegate();
        assert_eq!(delegate.column_sort(), None);
        delegate.set_column_sort(Some(("restarts", SortDirection::Descending)));
        assert_eq!(keys(&delegate), ["b", "a", "c"]);
        assert_eq!(
            delegate.column_sort(),
            Some(ColumnSort {
                column: "restarts".into(),
                direction: SortDirection::Descending,
            })
        );
        delegate.toggle_sort(0);
        assert_eq!(delegate.column_sort().unwrap().column.as_ref(), "name");
        delegate.set_column_sort(None);
        assert_eq!(delegate.column_sort(), None);
        assert_eq!(keys(&delegate), ["b", "a", "c"]);
    }

    #[test]
    fn missing_or_unsortable_columns_give_the_default_order() {
        let mut delegate = ResourceTableDelegate::new(vec![
            ResourceColumn::new("name", "Name").sortable(),
            ResourceColumn::new("note", "Note"),
            ResourceColumn::new("age", "Age").sorted(),
        ]);
        delegate.set_rows(vec![
            ResourceRow::new("b", [ResourceCell::text("b"), ResourceCell::text("y")]),
            ResourceRow::new("a", [ResourceCell::text("a"), ResourceCell::text("x")]),
        ]);
        delegate.set_column_sort(Some(("name", SortDirection::Ascending)));
        assert_eq!(keys(&delegate), ["a", "b"]);
        // A column of another version, a hidden one or one not sortable.
        for key in ["restarts", "note", ""] {
            delegate.set_column_sort(Some(("name", SortDirection::Ascending)));
            delegate.set_column_sort(Some((key, SortDirection::Descending)));
            assert_eq!(delegate.column_sort(), None, "{key}");
            assert_eq!(keys(&delegate), ["b", "a"]);
        }
        // Ascending by the column the rows come sorted by is the default.
        delegate.set_column_sort(Some(("age", SortDirection::Ascending)));
        assert_eq!(delegate.column_sort(), None);
        delegate.set_column_sort(Some(("age", SortDirection::Descending)));
        assert_eq!(delegate.column_sort().unwrap().column.as_ref(), "age");
    }

    #[test]
    fn numbers_sort_numerically_and_stably() {
        let mut delegate = delegate();
        delegate.toggle_sort(1);
        assert_eq!(keys(&delegate), ["a", "c", "b"]);
        assert_eq!(delegate.view_index(&"b".into()), Some(2));
    }

    #[test]
    fn new_rows_keep_the_sort() {
        let mut delegate = delegate();
        delegate.toggle_sort(0);
        delegate.set_rows(vec![
            ResourceRow::new("z", [ResourceCell::text("z")]),
            ResourceRow::new("y", [ResourceCell::text("y")]),
        ]);
        assert_eq!(keys(&delegate), ["y", "z"]);
    }

    fn checked(delegate: &ResourceTableDelegate) -> Vec<String> {
        delegate
            .checked_keys()
            .iter()
            .map(|key| key.to_string())
            .collect()
    }

    fn letter_rows(keys: &[&str]) -> Vec<ResourceRow> {
        keys.iter()
            .map(|&key| ResourceRow::new(key.to_owned(), [ResourceCell::text(key.to_owned())]))
            .collect()
    }

    fn four_rows() -> ResourceTableDelegate {
        let mut delegate = ResourceTableDelegate::new(vec![ResourceColumn::new("name", "Name")]);
        delegate.checkable = true;
        delegate.set_rows(letter_rows(&["a", "b", "c", "d"]));
        delegate
    }

    #[test]
    fn toggle_and_range_follow_the_view() {
        let mut delegate = four_rows();
        delegate.check_row(1, false);
        assert_eq!(checked(&delegate), ["b"]);
        // Shift-click extends from the last toggled row, in either direction.
        delegate.check_row(3, true);
        assert_eq!(checked(&delegate), ["b", "c", "d"]);
        delegate.check_row(2, false);
        assert_eq!(checked(&delegate), ["b", "d"]);
        delegate.check_row(0, true);
        assert_eq!(checked(&delegate), ["a", "b", "c", "d"]);
        // A range without an anchor checks just the clicked row.
        delegate.checks.clear();
        delegate.check_row(2, true);
        assert_eq!(checked(&delegate), ["c"]);
    }

    #[test]
    fn header_checks_all_then_none_with_partial_state() {
        let mut delegate = four_rows();
        assert_eq!(delegate.check_state(), CheckState::Unchecked);
        delegate.check_row(0, false);
        assert_eq!(delegate.check_state(), CheckState::Partial);
        delegate.toggle_all();
        assert_eq!(delegate.check_state(), CheckState::Checked);
        assert_eq!(checked(&delegate), ["a", "b", "c", "d"]);
        delegate.toggle_all();
        assert_eq!(delegate.check_state(), CheckState::Unchecked);
        delegate.check_all();
        assert_eq!(delegate.checked_keys().len(), 4);
    }

    #[test]
    fn checks_survive_sorting_and_updates_but_not_removal() {
        let mut delegate = four_rows();
        delegate.check_row(0, false);
        delegate.check_row(2, false);
        delegate.toggle_sort(0);
        delegate.toggle_sort(0);
        // Descending: d c b a; the checks stay on their keys.
        assert_eq!(checked(&delegate), ["c", "a"]);
        assert!(delegate.is_checked(1));
        assert!(delegate.set_rows(letter_rows(&["a", "b", "d"])));
        assert_eq!(checked(&delegate), ["a"]);
        assert!(!delegate.set_rows(letter_rows(&["a", "d"])));
        assert_eq!(delegate.check_state(), CheckState::Partial);
    }

    #[test]
    fn checkbox_column_shifts_data_columns() {
        let mut delegate = four_rows();
        assert_eq!(delegate.data_col(0), None);
        assert_eq!(delegate.data_col(1), Some(0));
        delegate.checkable = false;
        assert_eq!(delegate.data_col(0), Some(0));
    }

    #[test]
    fn ages_are_formatted_from_their_moment_and_sorted_by_it() {
        let now = 1_000_000;
        let mut delegate = ResourceTableDelegate::new(vec![
            ResourceColumn::new("age", "Age").sortable(),
            ResourceColumn::new("restarts", "Restarts").sortable(),
        ]);
        delegate.set_rows(vec![
            ResourceRow::new(
                "day",
                [
                    ResourceCell::age(now - 86_400),
                    ResourceCell::count_age(1, now - 300),
                ],
            ),
            ResourceRow::new(
                "fresh",
                [ResourceCell::age(now - 5), ResourceCell::number(0)],
            ),
            ResourceRow::new(
                "hour",
                [
                    ResourceCell::age(now - 3_600),
                    ResourceCell::count_age(12, now - 60),
                ],
            ),
        ]);
        let cell = |key: &str, col: usize| {
            let row = delegate.rows.iter().find(|row| row.key.as_ref() == key);
            row.unwrap().cells[col].content.text_at(now)
        };
        assert_eq!(cell("fresh", 0).as_ref(), "5s");
        assert_eq!(cell("hour", 0).as_ref(), "60m");
        assert_eq!(cell("day", 0).as_ref(), "24h");
        assert_eq!(cell("hour", 1).as_ref(), "12 (60s ago)");
        // The youngest moment drives the ticks.
        assert_eq!(delegate.youngest(), Some(now - 5));
        // Ascending age: youngest first.
        delegate.toggle_sort(0);
        assert_eq!(keys(&delegate), ["fresh", "hour", "day"]);
        delegate.toggle_sort(1);
        assert_eq!(keys(&delegate), ["fresh", "day", "hour"]);
    }

    fn order(delegate: &ResourceTableDelegate) -> Vec<String> {
        delegate
            .order_keys()
            .iter()
            .map(|key| key.to_string())
            .collect()
    }

    #[test]
    fn moved_columns_take_their_cells_and_sort_along() {
        let mut delegate = delegate();
        delegate.checkable = true;
        delegate.toggle_sort(1);
        // Drag Restarts (table column 2, after the checkboxes) in front of Name.
        delegate.move_display(2, 1);
        assert_eq!(order(&delegate), ["restarts", "name"]);
        assert_eq!(delegate.data_col(0), None);
        assert_eq!(delegate.data_col(1), Some(1));
        assert_eq!(delegate.data_col(2), Some(0));
        // The sort stays on Restarts and cells stay with their column.
        assert_eq!(keys(&delegate), ["a", "c", "b"]);
        let label = |col_ix: usize| {
            let col = delegate.data_col(col_ix).unwrap();
            delegate.cell(0, col).unwrap().label().to_string()
        };
        assert_eq!(label(1), "2");
        assert_eq!(label(2), "a");
        // The checkbox column never moves.
        delegate.move_display(0, 2);
        delegate.move_display(1, 0);
        assert_eq!(order(&delegate), ["restarts", "name"]);
    }

    #[test]
    fn column_order_by_keys_skips_unknown_and_appends_missing() {
        let mut delegate = ResourceTableDelegate::new(vec![
            ResourceColumn::new("name", "Name"),
            ResourceColumn::new("status", "Status"),
            ResourceColumn::new("age", "Age"),
        ]);
        delegate.display = delegate.display_for(&["age", "gone", "name", "age"]);
        assert_eq!(order(&delegate), ["age", "name", "status"]);
        delegate.display = delegate.display_for(&[] as &[&str]);
        assert_eq!(order(&delegate), ["name", "status", "age"]);
    }

    #[test]
    fn tables_without_ages_do_not_tick() {
        assert_eq!(delegate().youngest(), None);
    }
}
