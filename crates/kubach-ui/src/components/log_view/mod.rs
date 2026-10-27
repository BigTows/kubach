//! Virtualized log list: hundreds of thousands of lines, follow mode,
//! search highlighting, optional source tags and timestamps, program output
//! colors and mouse selection ([`LineSelector`]).

mod buffer;

use std::ops::Range;

use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, Theme};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::line_selection::{self, LineSelector, SelectableLines};
use crate::{layer_highlights, span_highlights, theme};
use kubach_i18n::t;

pub use buffer::{
    FollowState, LogBuffer, LogEntry, LogMatch, LogSearch, LogTag, PushResult, find_matches,
};

/// Lines kept by default before the oldest are dropped.
pub const DEFAULT_LOG_CAPACITY: usize = 500_000;
const LINE_HEIGHT: Pixels = px(18.);

/// Color of tag `index` (cycles through the theme's accent colors).
pub fn tag_color(index: usize, theme: &Theme) -> Hsla {
    let palette = [
        theme.blue,
        theme.green,
        theme.yellow,
        theme.magenta,
        theme.cyan,
        theme.red,
        theme.blue_light,
        theme.green_light,
        theme.yellow_light,
        theme.magenta_light,
        theme.cyan_light,
        theme.red_light,
    ];
    palette[index % palette.len()]
}

/// What a [`LogView`] reports to its owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogViewEvent {
    /// The first line came into view while not following: a chance to
    /// [`prepend`](LogView::prepend) earlier lines. Sent once until lines are
    /// prepended or the top is scrolled out of view.
    ReachedTop,
}

/// A note pinned above the first line (loading earlier lines, beginning of
/// the log, limits).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogTopNotice {
    /// Shown with a spinner, even when the first line is not in view.
    Loading(SharedString),
    Info(SharedString),
    Warning(SharedString),
}

/// A scrolling list of log lines that sticks to the end while following.
///
/// Scrolling up pauses following, scrolling back to the end resumes it.
pub struct LogView {
    buffer: LogBuffer,
    search: LogSearch,
    list: ListState,
    follow: FollowState,
    wrap: bool,
    show_timestamps: bool,
    /// `None`: "No lines yet" in the current language.
    placeholder: Option<SharedString>,
    selector: LineSelector,
    /// Following was paused because text got selected; it resumes when the
    /// selection goes away.
    resume_follow: bool,
    /// [`LogViewEvent::ReachedTop`] may be sent.
    top_armed: bool,
    top_notice: Option<LogTopNotice>,
    /// Lines were dropped from the front since the last clear.
    evicted: bool,
}

impl EventEmitter<LogViewEvent> for LogView {}

impl LogView {
    pub fn new(capacity: usize, cx: &mut Context<Self>) -> Self {
        let list =
            ListState::new(0, ListAlignment::Top, px(200.)).with_uniform_item_height(LINE_HEIGHT);
        list.set_follow_mode(FollowMode::Tail);
        let view = cx.entity().downgrade();
        list.set_scroll_handler(move |event, _, cx| {
            let following = event.is_following_tail;
            view.update(cx, |view, cx| view.sync_follow(following, cx))
                .ok();
        });
        Self {
            buffer: LogBuffer::new(capacity),
            search: LogSearch::default(),
            list,
            follow: FollowState::Following,
            wrap: false,
            show_timestamps: false,
            placeholder: None,
            selector: LineSelector::new(cx),
            resume_follow: false,
            top_armed: true,
            top_notice: None,
            evicted: false,
        }
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    pub fn buffer(&self) -> &LogBuffer {
        &self.buffer
    }

    /// Text shown while there are no lines.
    pub fn set_placeholder(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.placeholder = Some(text.into());
        cx.notify();
    }

    /// Appends lines, dropping the oldest beyond capacity.
    pub fn push(&mut self, entries: Vec<LogEntry>, cx: &mut Context<Self>) {
        if entries.is_empty() {
            return;
        }
        let old_len = self.buffer.len();
        let from = self.buffer.end_id();
        let pushed = self.buffer.push(entries);
        self.list.splice(old_len..old_len, pushed.added);
        if pushed.evicted > 0 {
            self.evicted = true;
            self.list.splice(0..pushed.evicted, 0);
            self.search.evict(&self.buffer);
            self.selector.retain_from(self.buffer.first_id());
        }
        self.search.extend(&self.buffer, from);
        self.follow.appended(pushed.added);
        cx.notify();
    }

    /// Inserts earlier lines (oldest first) above the first one, keeping the
    /// visible lines in place, the selection, search matches and following.
    /// Lines beyond capacity are not inserted. Always allows the next
    /// [`LogViewEvent::ReachedTop`], so an empty batch asks for more.
    pub fn prepend(&mut self, entries: Vec<LogEntry>, cx: &mut Context<Self>) {
        self.top_armed = true;
        let added = self.buffer.prepend(entries);
        if added > 0 {
            let following = self.list.is_following_tail();
            let top = self.list.logical_scroll_top();
            self.list.splice(0..0, added);
            if !following {
                // Anchor the line that was at the top of the viewport.
                self.list.scroll_to(ListOffset {
                    item_ix: top.item_ix + added,
                    offset_in_item: top.offset_in_item,
                });
            }
            self.search.prepend(&self.buffer, added);
        }
        cx.notify();
    }

    /// Whether lines were dropped from the front (beyond capacity) since the
    /// last clear, so the first line is no longer the oldest one received.
    pub fn has_evicted(&self) -> bool {
        self.evicted
    }

    pub fn set_top_notice(&mut self, notice: Option<LogTopNotice>, cx: &mut Context<Self>) {
        if self.top_notice != notice {
            self.top_notice = notice;
            cx.notify();
        }
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.evicted = false;
        self.top_armed = true;
        self.selector.clear();
        self.resume_follow = false;
        self.buffer.clear();
        self.list.reset(0);
        self.list.set_follow_mode(FollowMode::Tail);
        self.follow = FollowState::Following;
        let query = self.search.query().to_owned();
        self.search.set_query(&query, &self.buffer);
        cx.notify();
    }

    pub fn wrap(&self) -> bool {
        self.wrap
    }

    pub fn set_wrap(&mut self, wrap: bool, cx: &mut Context<Self>) {
        if self.wrap != wrap {
            self.wrap = wrap;
            self.list.remeasure();
            cx.notify();
        }
    }

    pub fn show_timestamps(&self) -> bool {
        self.show_timestamps
    }

    pub fn set_show_timestamps(&mut self, show: bool, cx: &mut Context<Self>) {
        if self.show_timestamps != show {
            self.show_timestamps = show;
            // Offsets index the displayed line, which just changed.
            self.selector.clear();
            self.resume_follow = false;
            self.list.remeasure();
            cx.notify();
        }
    }

    pub fn follow_state(&self) -> FollowState {
        self.follow
    }

    /// Jumps to the end and keeps following new lines; drops the
    /// selection, which would pause following again.
    pub fn follow(&mut self, cx: &mut Context<Self>) {
        self.selector.clear();
        self.resume_follow = false;
        self.list.set_follow_mode(FollowMode::Tail);
        self.follow = FollowState::Following;
        cx.notify();
    }

    /// Stops at the current position while lines keep arriving.
    pub fn pause(&mut self, cx: &mut Context<Self>) {
        self.list.pause_following_tail();
        self.follow.sync(false);
        cx.notify();
    }

    fn sync_follow(&mut self, following: bool, cx: &mut Context<Self>) {
        if following && self.selector.has_selection() {
            // Scrolled back to the end with text selected: stay put. The list
            // is mid-update here, so pause it afterwards.
            let view = cx.entity().downgrade();
            cx.defer(move |cx| {
                view.update(cx, |view, cx| view.pause_for_selection(cx))
                    .ok();
            });
            return;
        }
        let before = self.follow;
        self.follow.sync(following);
        if before != self.follow {
            cx.notify();
        }
    }

    fn pause_for_selection(&mut self, cx: &mut Context<Self>) {
        if self.selector.has_selection() {
            self.list.pause_following_tail();
            self.follow.sync(false);
            cx.notify();
        }
    }

    /// Whether some text is selected.
    pub fn has_selection(&self) -> bool {
        self.selector.has_selection()
    }

    /// The selected text, if any.
    pub fn selected_text(&self) -> Option<String> {
        line_selection::selected_text(self)
    }

    /// Selects every line.
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        line_selection::select_all(self, cx);
    }

    pub fn search_query(&self) -> &str {
        self.search.query()
    }

    /// Highlights `query` (ASCII ignores case) in every line.
    pub fn set_search(&mut self, query: &str, cx: &mut Context<Self>) {
        if self.search.query() != query {
            self.search.set_query(query, &self.buffer);
            cx.notify();
        }
    }

    /// `(current, total)` with `current` counted from 1, 0 when none.
    pub fn match_position(&self) -> (usize, usize) {
        let current = self.search.current_index().map_or(0, |ix| ix + 1);
        (current, self.search.len())
    }

    pub fn next_match(&mut self, cx: &mut Context<Self>) {
        let line = self.search.next_match().map(|m| m.line);
        self.reveal(line, cx);
    }

    pub fn prev_match(&mut self, cx: &mut Context<Self>) {
        let line = self.search.prev_match().map(|m| m.line);
        self.reveal(line, cx);
    }

    fn reveal(&mut self, line: Option<u64>, cx: &mut Context<Self>) {
        let Some(index) = line.and_then(|id| self.buffer.index_of(id)) else {
            return;
        };
        self.list.pause_following_tail();
        self.follow.sync(false);
        self.list.scroll_to_reveal_item(index);
        cx.notify();
    }

    /// All buffered lines as shown (with tags and timestamps when visible).
    pub fn text(&self) -> String {
        let mut text = String::new();
        for entry in self.buffer.iter() {
            text.push_str(&self.display_line(entry).0);
            text.push('\n');
        }
        text
    }

    /// The displayed line and where its log text starts.
    fn display_line(&self, entry: &LogEntry) -> (String, usize) {
        let mut line = String::with_capacity(entry.text.len() + 48);
        if let Some(tag) = &entry.tag {
            line.push_str(&tag.label);
            line.push(' ');
        }
        if self.show_timestamps
            && let Some(timestamp) = &entry.timestamp
        {
            line.push_str(timestamp);
            line.push(' ');
        }
        let offset = line.len();
        line.push_str(&entry.text);
        (line, offset)
    }

    fn render_line(&self, index: usize, cx: &App) -> AnyElement {
        let Some(entry) = self.buffer.get(index) else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let id = self.buffer.first_id() + index as u64;
        let (line, offset) = self.display_line(entry);

        // Lowest priority first: program colors, prefixes, search, selection.
        let palette = theme::ansi_palette(cx);
        let mut highlights: Vec<(Range<usize>, HighlightStyle)> =
            span_highlights(&entry.spans, offset, &palette).collect();
        let mut at = 0;
        if let Some(tag) = &entry.tag {
            highlights.push((
                0..tag.label.len(),
                HighlightStyle {
                    color: Some(tag_color(tag.color, theme)),
                    ..Default::default()
                },
            ));
            at = tag.label.len() + 1;
        }
        if offset > at {
            highlights.push((
                at..offset - 1,
                HighlightStyle {
                    color: Some(theme.muted_foreground),
                    ..Default::default()
                },
            ));
        }
        let current = self.search.current();
        for found in self.search.in_line(id) {
            let is_current = current == Some(found);
            highlights.push((
                found.range.start + offset..found.range.end + offset,
                HighlightStyle {
                    background_color: Some(if is_current {
                        theme.warning.opacity(0.85)
                    } else {
                        theme.warning.opacity(0.3)
                    }),
                    color: is_current.then_some(theme.warning_foreground),
                    ..Default::default()
                },
            ));
        }
        highlights.extend(self.selector.highlight(id, line.len(), cx));

        let text = StyledText::new(line).with_highlights(layer_highlights(highlights));
        self.selector
            .row(id, text)
            .w_full()
            .px_3()
            .line_height(LINE_HEIGHT)
            .when(!self.wrap, |line| {
                line.whitespace_nowrap().overflow_hidden()
            })
            .into_any_element()
    }

    /// Whether the first line is (partly) in view.
    fn top_in_view(&self) -> bool {
        self.list.item_is_above_viewport(0) == Some(false)
    }

    /// Sends [`LogViewEvent::ReachedTop`] once the first line is in view.
    fn check_top(&mut self, cx: &mut Context<Self>) {
        let at_top =
            !self.buffer.is_empty() && !self.list.is_following_tail() && self.top_in_view();
        if !at_top {
            if !self.top_in_view() {
                self.top_armed = true;
            }
            return;
        }
        if self.top_armed {
            self.top_armed = false;
            cx.emit(LogViewEvent::ReachedTop);
        }
    }

    fn render_top_notice(&self, cx: &App) -> Option<impl IntoElement + use<>> {
        let notice = self.top_notice.as_ref()?;
        let loading = matches!(notice, LogTopNotice::Loading(_));
        if !loading && !self.top_in_view() {
            return None;
        }
        let theme = cx.theme();
        let (icon, text) = match notice {
            LogTopNotice::Loading(text) => (
                crate::spinner()
                    .icon(Icon::new(IconName::LoaderCircle))
                    .with_size(px(12.))
                    .color(theme.muted_foreground)
                    .into_any_element(),
                text,
            ),
            LogTopNotice::Info(text) => (
                Icon::new(IconName::Info)
                    .size_3()
                    .text_color(theme.muted_foreground)
                    .into_any_element(),
                text,
            ),
            LogTopNotice::Warning(text) => (
                Icon::new(IconName::TriangleAlert)
                    .size_3()
                    .text_color(theme.warning)
                    .into_any_element(),
                text,
            ),
        };
        Some(
            div()
                .absolute()
                .top_2()
                .w_full()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("log-top-notice")
                        .occlude()
                        .flex()
                        .items_center()
                        .gap_1()
                        .px_3()
                        .py_1()
                        .rounded_full()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.popover)
                        .text_xs()
                        .text_color(theme.popover_foreground)
                        .child(icon)
                        .child(text.clone()),
                ),
        )
    }

    fn render_unseen(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let unseen = self.follow.unseen();
        if unseen == 0 {
            return None;
        }
        let theme = cx.theme();
        let label = t!("ui-log-new-lines", count = unseen);
        Some(
            div()
                .absolute()
                .bottom_3()
                .w_full()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("log-unseen")
                        .occlude()
                        .flex()
                        .items_center()
                        .gap_1()
                        .px_3()
                        .py_1()
                        .rounded_full()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.popover)
                        .text_xs()
                        .text_color(theme.popover_foreground)
                        .cursor_pointer()
                        .hover(|style| style.bg(theme.secondary_hover))
                        .child(Icon::new(IconName::ArrowDown).size_3())
                        .child(label)
                        .on_click(cx.listener(|this, _, _, cx| this.follow(cx))),
                ),
        )
    }
}

impl Render for LogView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.selector.begin_frame();
        let theme = cx.theme();
        let root = div()
            .relative()
            .size_full()
            .font_family(theme.mono_font_family.clone())
            .text_xs()
            .text_color(theme.foreground);
        if self.buffer.is_empty() {
            return root
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child(
                    self.placeholder
                        .clone()
                        .unwrap_or_else(|| t!("ui-log-no-lines").into()),
                );
        }
        self.check_top(cx);
        let render_line =
            cx.processor(|this: &mut Self, index: usize, _, cx| this.render_line(index, cx));
        let root = line_selection::selectable(root, &self.selector, cx);
        root.child(
            list(self.list.clone(), move |index, window, cx| {
                render_line(index, window, cx)
            })
            .size_full()
            .py_1(),
        )
        .child(Scrollbar::vertical(&self.list))
        .children(self.render_top_notice(cx))
        .children(self.render_unseen(cx))
    }
}

impl SelectableLines for LogView {
    fn line_selector(&self) -> &LineSelector {
        &self.selector
    }

    fn line_selector_mut(&mut self) -> &mut LineSelector {
        &mut self.selector
    }

    fn line_ids(&self) -> Range<u64> {
        self.buffer.first_id()..self.buffer.end_id()
    }

    fn line_text(&self, line: u64) -> Option<String> {
        let entry = self.buffer.get(self.buffer.index_of(line)?)?;
        Some(self.display_line(entry).0)
    }

    fn scroll_by(&mut self, delta: Pixels, cx: &mut Context<Self>) {
        self.list.scroll_by(delta);
        cx.notify();
    }

    fn selection_changed(&mut self, cx: &mut Context<Self>) {
        if self.selector.has_selection() {
            if self.follow.is_following() {
                self.list.pause_following_tail();
                self.follow.sync(false);
                self.resume_follow = true;
            }
        } else if !self.selector.is_dragging() && self.resume_follow {
            self.resume_follow = false;
            self.list.set_follow_mode(FollowMode::Tail);
            self.follow = FollowState::Following;
        }
        cx.notify();
    }
}
