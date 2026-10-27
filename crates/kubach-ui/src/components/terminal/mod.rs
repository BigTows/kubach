//! Terminal emulator view: an `alacritty_terminal` grid painted with GPUI
//! text runs. It knows nothing about where bytes come from: feed output with
//! [`TerminalView::feed`] and forward [`TerminalEvent`]s to the process.
//! Box drawing, blocks, braille and powerline symbols are drawn as shapes
//! over the whole cell ([`builtin_glyphs`]) so they join across lines.
//!
//! Keys reach the process two ways: control keys and combinations are
//! encoded in `on_key_down`, plain text comes through the view's
//! [`InputHandler`], so IME compositions (pinyin, kana, hangul) and dead keys
//! work; [`route_keystroke`] decides which, so nothing is sent twice. Preedit
//! text is drawn over the cursor and sent only when committed.

mod builtin_glyphs;
mod ime;
mod input;
mod palette;

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Osc52, Term, TermMode};
use alacritty_terminal::vte::ansi::{CursorShape, NamedColor, Processor};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use ime::Composition;
pub use input::{
    InputMode, KeyAction, encode_keystroke, encode_paste, encode_text, route_keystroke,
};
pub use palette::TerminalPalette;

/// Key context of a focused terminal.
pub const TERMINAL_CONTEXT: &str = "Terminal";
/// Lines kept above the screen.
pub const SCROLLBACK_LINES: usize = 10_000;
const FONT_SIZE: Pixels = px(13.);
const LINE_HEIGHT: f32 = 1.35;
const PADDING: Pixels = px(8.);

/// Keeps keys the app binds globally (focus cycling, copy on Linux and
/// Windows) inside a focused terminal. Call once at startup.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", NoAction, Some(TERMINAL_CONTEXT)),
        KeyBinding::new("shift-tab", NoAction, Some(TERMINAL_CONTEXT)),
        KeyBinding::new("ctrl-c", NoAction, Some(TERMINAL_CONTEXT)),
        KeyBinding::new("escape", NoAction, Some(TERMINAL_CONTEXT)),
    ]);
}

/// Collects what the emulator wants from the outside (replies to queries,
/// title changes, clipboard writes) until the view handles it.
#[derive(Clone, Default)]
struct Listener(Rc<RefCell<Vec<TermEvent>>>);

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        self.0.borrow_mut().push(event);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GridSize {
    cols: usize,
    rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

/// What a program in the terminal may do with the system clipboard through
/// OSC 52 (`ESC ] 52 ; c ; <base64> BEL`). A program, or a file it prints,
/// could otherwise put a command there for the user to paste, so writes are
/// denied unless the owner allows them. Reading the clipboard is never
/// allowed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClipboardPolicy {
    /// OSC 52 writes are ignored.
    #[default]
    Deny,
    /// OSC 52 writes replace the clipboard.
    AllowWrite,
}

impl ClipboardPolicy {
    /// Text to put on the clipboard for `event`, if the policy lets it.
    /// Clipboard reads (`ClipboardLoad`) are never answered.
    fn store(self, event: &TermEvent) -> Option<&str> {
        match (self, event) {
            (ClipboardPolicy::AllowWrite, TermEvent::ClipboardStore(_, text)) => Some(text),
            _ => None,
        }
    }
}

/// Emulator config. OSC 52 reads are refused by the emulator itself; writes
/// reach [`ClipboardPolicy`].
fn term_config() -> Config {
    Config {
        scrolling_history: SCROLLBACK_LINES,
        osc52: Osc52::OnlyCopy,
        ..Config::default()
    }
}

/// What a [`TerminalView`] asks of its owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalEvent {
    /// Bytes to write to the process: keys, pastes and answers to terminal
    /// queries such as the cursor position report (`ESC[6n`).
    Input(Vec<u8>),
    /// The visible grid changed size.
    Resize {
        cols: u16,
        rows: u16,
    },
    Title(SharedString),
}

/// An interactive terminal: grid, scrollback, selection, keyboard and
/// clipboard handling. Colors follow the theme.
pub struct TerminalView {
    term: Term<Listener>,
    parser: Processor,
    events: Listener,
    focus: FocusHandle,
    size: GridSize,
    cell: Size<Pixels>,
    origin: Point<Pixels>,
    selecting: bool,
    scroll_remainder: Pixels,
    title: Option<SharedString>,
    clipboard: ClipboardPolicy,
    ime: Composition,
    _focus_subscriptions: [Subscription; 2],
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TerminalView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let events = Listener::default();
        let size = GridSize { cols: 80, rows: 24 };
        let focus = cx.focus_handle();
        let subscriptions = [
            cx.on_focus(&focus, window, |this, _, cx| {
                this.term.is_focused = true;
                cx.notify();
            }),
            cx.on_blur(&focus, window, |this, _, cx| {
                this.term.is_focused = false;
                this.ime.cancel();
                cx.notify();
            }),
        ];
        Self {
            term: Term::new(term_config(), &size, events.clone()),
            parser: Processor::new(),
            events,
            focus,
            size,
            cell: size_of_cell(FONT_SIZE * 0.6),
            origin: Point::default(),
            selecting: false,
            scroll_remainder: px(0.),
            title: None,
            clipboard: ClipboardPolicy::default(),
            ime: Composition::default(),
            _focus_subscriptions: subscriptions,
        }
    }

    /// `(cols, rows)` of the visible grid.
    pub fn size(&self) -> (u16, u16) {
        (clamp_u16(self.size.cols), clamp_u16(self.size.rows))
    }

    /// Title set by the program (OSC 0/2), if any.
    pub fn title(&self) -> Option<&SharedString> {
        self.title.as_ref()
    }

    /// What programs may do with the clipboard through OSC 52; denied by
    /// default.
    pub fn set_clipboard_policy(&mut self, policy: ClipboardPolicy) {
        self.clipboard = policy;
    }

    pub fn clipboard_policy(&self) -> ClipboardPolicy {
        self.clipboard
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.focus.focus(window, cx);
    }

    /// Processes program output.
    pub fn feed(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        self.parser.advance(&mut self.term, bytes);
        self.flush_events(cx);
        cx.notify();
    }

    /// The visible screen as text, trailing blanks trimmed (for tests and
    /// diagnostics).
    pub fn screen_text(&self) -> String {
        let grid = self.term.grid();
        let mut lines = Vec::with_capacity(self.size.rows);
        for row in 0..self.size.rows as i32 {
            let line = &grid[Line(row)];
            let text: String = (0..self.size.cols).map(|col| line[Column(col)].c).collect();
            lines.push(text.trim_end().to_owned());
        }
        while lines.last().is_some_and(|line| line.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }

    pub fn copy_selection(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.term.selection_to_string().filter(|t| !t.is_empty()) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    pub fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        self.paste_text(&text, cx);
    }

    fn paste_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let bytes = encode_paste(text, self.input_mode());
        self.send(bytes, cx);
    }

    /// IME preedit text shown over the cursor, if a composition is going on.
    pub fn marked_text(&self) -> Option<&str> {
        self.ime.marked()
    }

    /// Sends text committed by the IME, a dead key or plain typing.
    fn send_text(&mut self, text: Option<String>, cx: &mut Context<Self>) {
        if let Some(bytes) = text.as_deref().and_then(encode_text) {
            self.send(bytes, cx);
        }
        cx.notify();
    }

    /// Screen bounds of the cursor cell, shifted right by `x`.
    fn cursor_bounds(&self, x: Pixels) -> Bounds<Pixels> {
        let grid = self.term.grid();
        let cursor = grid.cursor.point;
        let row = (cursor.line.0 + grid.display_offset() as i32)
            .clamp(0, self.size.rows.saturating_sub(1) as i32) as f32;
        let origin = point(
            self.origin.x + self.cell.width * cursor.column.0 as f32 + x,
            self.origin.y + self.cell.height * row,
        );
        Bounds::new(origin, self.cell)
    }

    fn input_mode(&self) -> InputMode {
        let mode = self.term.mode();
        InputMode {
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
        }
    }

    /// Sends user input, scrolling back to the prompt.
    fn send(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        self.term.selection = None;
        self.term.scroll_display(Scroll::Bottom);
        cx.emit(TerminalEvent::Input(bytes));
        cx.notify();
    }

    fn flush_events(&mut self, cx: &mut Context<Self>) {
        let events = std::mem::take(&mut *self.events.0.borrow_mut());
        for event in events {
            match event {
                TermEvent::PtyWrite(text) => cx.emit(TerminalEvent::Input(text.into_bytes())),
                TermEvent::Title(title) => {
                    let title: SharedString = title.into();
                    self.title = Some(title.clone());
                    cx.emit(TerminalEvent::Title(title));
                }
                TermEvent::ResetTitle => self.title = None,
                TermEvent::ClipboardStore(..) | TermEvent::ClipboardLoad(..) => {
                    if let Some(text) = self.clipboard.store(&event) {
                        cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
                    }
                }
                TermEvent::ColorRequest(index, format) => {
                    let palette = TerminalPalette::from_theme(cx.theme());
                    let color = match index {
                        0..=255 => palette.indexed(index as u8),
                        i if i == NamedColor::Background as usize => palette.background,
                        i if i == NamedColor::Cursor as usize => palette.cursor,
                        _ => palette.foreground,
                    };
                    let reply = format(TerminalPalette::to_rgb(color));
                    cx.emit(TerminalEvent::Input(reply.into_bytes()));
                }
                TermEvent::TextAreaSizeRequest(format) => {
                    let reply = format(self.window_size());
                    cx.emit(TerminalEvent::Input(reply.into_bytes()));
                }
                _ => {}
            }
        }
    }

    fn window_size(&self) -> WindowSize {
        WindowSize {
            num_lines: clamp_u16(self.size.rows),
            num_cols: clamp_u16(self.size.cols),
            cell_width: self.cell.width.as_f32() as u16,
            cell_height: self.cell.height.as_f32() as u16,
        }
    }

    /// Fits the grid into `bounds`; resizes the emulator when it changed.
    fn layout(&mut self, bounds: Bounds<Pixels>, cell: Size<Pixels>, cx: &mut Context<Self>) {
        self.origin = bounds.origin + point(PADDING, PADDING);
        self.cell = cell;
        let fit = |space: Pixels, unit: Pixels, min: usize| {
            ((space - PADDING * 2.).as_f32() / unit.as_f32())
                .floor()
                .max(min as f32) as usize
        };
        let size = GridSize {
            cols: fit(bounds.size.width, cell.width, 2),
            rows: fit(bounds.size.height, cell.height, 1),
        };
        if size != self.size {
            self.size = size;
            self.term.resize(size);
            cx.emit(TerminalEvent::Resize {
                cols: clamp_u16(size.cols),
                rows: clamp_u16(size.rows),
            });
        }
    }

    fn grid_point(&self, position: Point<Pixels>) -> (GridPoint, Side) {
        let x = (position.x - self.origin.x).as_f32() / self.cell.width.as_f32();
        let y = (position.y - self.origin.y).as_f32() / self.cell.height.as_f32();
        let col = x.floor().clamp(0.0, (self.size.cols - 1) as f32) as usize;
        let row = y.floor().clamp(0.0, (self.size.rows - 1) as f32) as i32;
        let side = if x - x.floor() > 0.5 {
            Side::Right
        } else {
            Side::Left
        };
        let line = row - self.term.grid().display_offset() as i32;
        (GridPoint::new(Line(line), Column(col)), side)
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = &keystroke.modifiers;
        let clipboard = if cfg!(target_os = "macos") {
            modifiers.platform && !modifiers.control && !modifiers.alt
        } else {
            modifiers.control && modifiers.shift
        };
        if clipboard && keystroke.key == "c" {
            self.copy_selection(cx);
            cx.stop_propagation();
            return;
        }
        if clipboard && keystroke.key == "v" {
            self.paste(cx);
            cx.stop_propagation();
            return;
        }
        let action = route_keystroke(
            keystroke,
            event.prefer_character_input,
            self.ime.is_composing(),
            self.input_mode(),
        );
        if let KeyAction::Send(bytes) = action {
            cx.stop_propagation();
            self.send(bytes, cx);
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus.focus(window, cx);
        let (point, side) = self.grid_point(event.position);
        let kind = match event.click_count {
            2 => SelectionType::Semantic,
            3 => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        self.term.selection = Some(Selection::new(kind, point, side));
        self.selecting = true;
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let (point, side) = self.grid_point(event.position);
        if let Some(selection) = &mut self.term.selection {
            selection.update(point, side);
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.selecting = false;
        if self.term.selection.as_ref().is_some_and(|s| s.is_empty()) {
            self.term.selection = None;
            cx.notify();
        }
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_remainder += event.delta.pixel_delta(self.cell.height).y;
        let lines = (self.scroll_remainder.as_f32() / self.cell.height.as_f32()).trunc() as i32;
        if lines == 0 {
            return;
        }
        self.scroll_remainder -= self.cell.height * lines as f32;
        let mode = *self.term.mode();
        if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
            // Full-screen programs (less, vim) get arrow keys instead.
            let arrow: &[u8] = match (lines > 0, mode.contains(TermMode::APP_CURSOR)) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            };
            cx.emit(TerminalEvent::Input(
                arrow.repeat(lines.unsigned_abs() as usize),
            ));
        } else {
            self.term.scroll_display(Scroll::Delta(lines));
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Everything [`paint`](Frame::paint) needs, taken from the grid.
    fn frame(&self, palette: &TerminalPalette, focused: bool) -> Frame {
        let content = self.term.renderable_content();
        let offset = content.display_offset as i32;
        let cursor = content.cursor;
        let selection = content.selection;
        let block_cursor = focused && cursor.shape == CursorShape::Block;
        let mut rows: Vec<FrameRow> = (0..self.size.rows).map(|_| FrameRow::default()).collect();

        for indexed in content.display_iter {
            let point = indexed.point;
            let Some(row) = usize::try_from(point.line.0 + offset)
                .ok()
                .and_then(|row| rows.get_mut(row))
            else {
                continue;
            };
            let cell = indexed.cell;
            let flags = cell.flags;
            if flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let col = point.column.0;
            let width = if flags.contains(Flags::WIDE_CHAR) {
                2
            } else {
                1
            };
            let fg_color = if flags.contains(Flags::BOLD) {
                TerminalPalette::brighten(cell.fg)
            } else {
                cell.fg
            };
            let mut fg = palette.foreground(fg_color, content.colors);
            let mut bg = palette.background(cell.bg, content.colors);
            if flags.contains(Flags::DIM) {
                fg = fg.opacity(0.7);
            }
            if flags.contains(Flags::INVERSE) {
                (fg, bg) = (bg.unwrap_or(palette.background), Some(fg));
            }
            if selection.is_some_and(|selection| selection.contains(point)) {
                bg = Some(palette.selection);
            }
            if block_cursor && point == cursor.point {
                bg = Some(palette.cursor);
                fg = palette.background;
            }
            if let Some(bg) = bg {
                row.background(col, col + width, bg);
            }

            let underline = flags.intersects(Flags::ALL_UNDERLINES);
            let strike = flags.contains(Flags::STRIKEOUT);
            let zerowidth = cell.zerowidth().unwrap_or_default();
            if flags.contains(Flags::HIDDEN)
                || (cell.c == ' ' && zerowidth.is_empty() && !underline && !strike)
            {
                continue;
            }
            if zerowidth.is_empty() && builtin_glyphs::is_builtin(cell.c) {
                row.glyphs.push(FrameGlyph {
                    col,
                    width,
                    c: cell.c,
                    color: fg,
                });
                continue;
            }
            let style = RunStyle {
                color: fg,
                bold: flags.contains(Flags::BOLD),
                italic: flags.contains(Flags::ITALIC),
                underline,
                strike,
            };
            row.text(col, width, cell.c, zerowidth, style);
        }

        let cursor_row = usize::try_from(cursor.point.line.0 + offset)
            .ok()
            .filter(|row| *row < self.size.rows);
        let cursor = match (cursor.shape, cursor_row) {
            (CursorShape::Hidden, _) | (_, None) => None,
            (CursorShape::Block, Some(_)) if focused => None,
            (shape, Some(row)) => Some(FrameCursor {
                row,
                col: cursor.point.column.0,
                shape: if focused {
                    shape
                } else {
                    CursorShape::HollowBlock
                },
            }),
        };

        let preedit = self
            .ime
            .marked()
            .zip(cursor_row)
            .map(|(text, row)| FramePreedit {
                row,
                col: content.cursor.point.column.0,
                caret: ime::byte_offset(text, self.ime.selected_range().end),
                text: text.to_owned(),
                color: palette.foreground,
                background: palette.background,
            });
        let cursor = if preedit.is_some() { None } else { cursor };

        Frame {
            rows,
            cursor,
            preedit,
            cell: self.cell,
            origin: self.origin,
            cursor_color: palette.cursor,
        }
    }
}

fn clamp_u16(value: usize) -> u16 {
    value.min(u16::MAX as usize) as u16
}

fn size_of_cell(width: Pixels) -> Size<Pixels> {
    size(width, (FONT_SIZE * LINE_HEIGHT).round())
}

/// Cell size of the monospace font.
fn measure_cell(font: &Font, window: &Window) -> Size<Pixels> {
    let text_system = window.text_system();
    let font_id = text_system.resolve_font(font);
    let width = text_system
        .advance(font_id, FONT_SIZE, 'm')
        .map(|advance| advance.width)
        .unwrap_or(FONT_SIZE * 0.6);
    size_of_cell(width)
}

#[derive(Clone, Copy, PartialEq)]
struct RunStyle {
    color: Hsla,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
}

/// Text of equal style starting at `col`.
struct FrameRun {
    col: usize,
    end: usize,
    text: String,
    style: RunStyle,
    /// Wide characters get a run of their own so the next one starts on its
    /// cell even if the glyph is not exactly two cells wide.
    wide: bool,
}

/// A symbol drawn as shapes instead of a font glyph.
struct FrameGlyph {
    col: usize,
    width: usize,
    c: char,
    color: Hsla,
}

#[derive(Default)]
struct FrameRow {
    backgrounds: Vec<(usize, usize, Hsla)>,
    runs: Vec<FrameRun>,
    glyphs: Vec<FrameGlyph>,
}

impl FrameRow {
    fn background(&mut self, start: usize, end: usize, color: Hsla) {
        match self.backgrounds.last_mut() {
            Some((_, last_end, last)) if *last_end == start && *last == color => *last_end = end,
            _ => self.backgrounds.push((start, end, color)),
        }
    }

    fn text(&mut self, col: usize, width: usize, c: char, zerowidth: &[char], style: RunStyle) {
        let wide = width > 1;
        match self.runs.last_mut() {
            Some(run) if !wide && !run.wide && run.end == col && run.style == style => {
                run.text.push(c);
                run.text.extend(zerowidth);
                run.end = col + width;
            }
            _ => {
                let mut text = String::from(c);
                text.extend(zerowidth);
                self.runs.push(FrameRun {
                    col,
                    end: col + width,
                    text,
                    style,
                    wide,
                });
            }
        }
    }
}

struct FrameCursor {
    row: usize,
    col: usize,
    shape: CursorShape,
}

/// IME preedit drawn over the cursor: underlined, with its own caret.
struct FramePreedit {
    row: usize,
    col: usize,
    text: String,
    /// Caret position in `text`, in bytes.
    caret: usize,
    color: Hsla,
    background: Hsla,
}

struct Frame {
    rows: Vec<FrameRow>,
    cursor: Option<FrameCursor>,
    preedit: Option<FramePreedit>,
    cell: Size<Pixels>,
    origin: Point<Pixels>,
    cursor_color: Hsla,
}

impl Frame {
    fn cell_origin(&self, row: usize, col: usize) -> Point<Pixels> {
        point(
            self.origin.x + self.cell.width * col as f32,
            self.origin.y + self.cell.height * row as f32,
        )
    }

    /// Draws built-in symbols on cell edges snapped to device pixels, so
    /// neighbouring cells share edges exactly.
    fn paint_glyphs(&self, rows: &[FrameRow], window: &mut Window) {
        let scale = window.scale_factor();
        let snap = |value: Pixels| (value.as_f32() * scale).round();
        for (y, row) in rows.iter().enumerate() {
            if row.glyphs.is_empty() {
                continue;
            }
            let top = snap(self.cell_origin(y, 0).y);
            let bottom = snap(self.cell_origin(y + 1, 0).y);
            for glyph in &row.glyphs {
                let left = snap(self.cell_origin(y, glyph.col).x);
                let right = snap(self.cell_origin(y, glyph.col + glyph.width).x);
                let cell = builtin_glyphs::Rect::new(left, top, right - left, bottom - top);
                let mut painter = GlyphPainter {
                    window,
                    color: glyph.color,
                    scale,
                };
                builtin_glyphs::draw(glyph.c, cell, &mut painter);
            }
        }
    }

    fn paint(mut self, font: &Font, window: &mut Window, cx: &mut App) {
        let rows = std::mem::take(&mut self.rows);
        for (y, row) in rows.iter().enumerate() {
            for &(start, end, color) in &row.backgrounds {
                let bounds = Bounds::new(
                    self.cell_origin(y, start),
                    size(self.cell.width * (end - start) as f32, self.cell.height),
                );
                window.paint_quad(fill(bounds, color));
            }
        }
        self.paint_glyphs(&rows, window);
        for (y, row) in rows.into_iter().enumerate() {
            for run in row.runs {
                let style = run.style;
                let font = Font {
                    weight: if style.bold {
                        FontWeight::BOLD
                    } else {
                        FontWeight::NORMAL
                    },
                    style: if style.italic {
                        FontStyle::Italic
                    } else {
                        FontStyle::Normal
                    },
                    ..font.clone()
                };
                let text_run = TextRun {
                    len: run.text.len(),
                    font,
                    color: style.color,
                    background_color: None,
                    underline: style.underline.then_some(UnderlineStyle {
                        thickness: px(1.),
                        color: Some(style.color),
                        wavy: false,
                    }),
                    strikethrough: style.strike.then_some(StrikethroughStyle {
                        thickness: px(1.),
                        color: Some(style.color),
                    }),
                };
                let origin = point(
                    self.origin.x + self.cell.width * run.col as f32,
                    self.origin.y + self.cell.height * y as f32,
                );
                let line =
                    window
                        .text_system()
                        .shape_line(run.text.into(), FONT_SIZE, &[text_run], None);
                line.paint(origin, self.cell.height, TextAlign::Left, None, window, cx)
                    .ok();
            }
        }
        if let Some(cursor) = &self.cursor {
            let origin = self.cell_origin(cursor.row, cursor.col);
            let bar = px(2.);
            let quad = match cursor.shape {
                CursorShape::Beam => fill(
                    Bounds::new(origin, size(bar, self.cell.height)),
                    self.cursor_color,
                ),
                CursorShape::Underline => fill(
                    Bounds::new(
                        point(origin.x, origin.y + self.cell.height - bar),
                        size(self.cell.width, bar),
                    ),
                    self.cursor_color,
                ),
                _ => outline(
                    Bounds::new(origin, self.cell),
                    self.cursor_color,
                    BorderStyle::Solid,
                ),
            };
            window.paint_quad(quad);
        }
        if let Some(preedit) = self.preedit.take() {
            self.paint_preedit(preedit, font, window, cx);
        }
    }

    fn paint_preedit(&self, preedit: FramePreedit, font: &Font, window: &mut Window, cx: &mut App) {
        let origin = self.cell_origin(preedit.row, preedit.col);
        let line = shape_preedit(&preedit.text, font, preedit.color, window);
        let width = line.width().max(self.cell.width);
        window.paint_quad(fill(
            Bounds::new(origin, size(width, self.cell.height)),
            preedit.background,
        ));
        let caret = line.x_for_index(preedit.caret);
        line.paint(origin, self.cell.height, TextAlign::Left, None, window, cx)
            .ok();
        window.paint_quad(fill(
            Bounds::new(
                point(origin.x + caret, origin.y),
                size(px(2.), self.cell.height),
            ),
            self.cursor_color,
        ));
    }
}

/// Preedit text as one underlined line.
fn shape_preedit(text: &str, font: &Font, color: Hsla, window: &Window) -> ShapedLine {
    let run = TextRun {
        len: text.len(),
        font: font.clone(),
        color,
        background_color: None,
        underline: Some(UnderlineStyle {
            thickness: px(1.),
            color: Some(color),
            wavy: false,
        }),
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line(text.to_owned().into(), FONT_SIZE, &[run], None)
}

/// Paints built-in symbol shapes, given in device pixels, into the window.
struct GlyphPainter<'a> {
    window: &'a mut Window,
    color: Hsla,
    scale: f32,
}

impl GlyphPainter<'_> {
    fn point(&self, [x, y]: builtin_glyphs::Pt) -> Point<Pixels> {
        point(px(x / self.scale), px(y / self.scale))
    }
}

impl builtin_glyphs::Canvas for GlyphPainter<'_> {
    fn rect(&mut self, rect: builtin_glyphs::Rect, alpha: f32) {
        let bounds = Bounds::new(
            self.point([rect.x, rect.y]),
            size(px(rect.w / self.scale), px(rect.h / self.scale)),
        );
        let color = Hsla {
            a: self.color.a * alpha,
            ..self.color
        };
        self.window.paint_quad(fill(bounds, color));
    }

    fn polygon(&mut self, points: &[builtin_glyphs::Pt]) {
        let Some((&first, rest)) = points.split_first() else {
            return;
        };
        let mut builder = PathBuilder::fill();
        builder.move_to(self.point(first));
        for &p in rest {
            builder.line_to(self.point(p));
        }
        builder.close();
        if let Ok(path) = builder.build() {
            self.window.paint_path(path, self.color);
        }
    }
}

/// Text input from the platform: IME compositions, dead keys and plain
/// typing. The document is only the preedit (see [`Composition`]).
impl TerminalView {
    fn ime_selected_range(&self) -> UTF16Selection {
        UTF16Selection {
            range: self.ime.selected_range(),
            reversed: false,
        }
    }

    fn ime_replace(&mut self, range: Option<Range<usize>>, text: &str, cx: &mut Context<Self>) {
        let text = self.ime.commit(range, text);
        self.send_text(text, cx);
    }

    fn ime_mark(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ime.mark(range, text, selected);
        if self.ime.is_composing() {
            // Keep the preedit next to the prompt.
            self.term.selection = None;
            self.term.scroll_display(Scroll::Bottom);
        }
        window.invalidate_character_coordinates();
        cx.notify();
    }

    fn ime_unmark(&mut self, cx: &mut Context<Self>) {
        let text = self.ime.unmark();
        self.send_text(text, cx);
    }

    /// Where the IME candidate window goes: under the preedit at `range`,
    /// or the cursor cell.
    fn ime_bounds(&self, range: Range<usize>, font: &Font, window: &Window) -> Bounds<Pixels> {
        let x = self.ime.marked().map_or(px(0.), |text| {
            let line = shape_preedit(text, font, Hsla::default(), window);
            line.x_for_index(ime::byte_offset(text, range.start))
        });
        self.cursor_bounds(x)
    }
}

/// The terminal's [`InputHandler`]. Unlike `ElementInputHandler` it turns
/// off macOS press-and-hold, so a held key repeats as in other terminals
/// instead of opening the accent menu.
struct TerminalInputHandler {
    view: Entity<TerminalView>,
    font: Font,
    element_bounds: Bounds<Pixels>,
}

impl InputHandler for TerminalInputHandler {
    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<UTF16Selection> {
        Some(self.view.read(cx).ime_selected_range())
    }

    fn marked_text_range(&mut self, _: &mut Window, cx: &mut App) -> Option<Range<usize>> {
        self.view.read(cx).ime.marked_range()
    }

    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<String> {
        let (text, range) = self.view.read(cx).ime.text_for_range(range_utf16);
        *adjusted_range = Some(range);
        Some(text)
    }

    fn replace_text_in_range(
        &mut self,
        replacement_range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut App,
    ) {
        self.view
            .update(cx, |view, cx| view.ime_replace(replacement_range, text, cx));
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.view.update(cx, |view, cx| {
            view.ime_mark(range_utf16, new_text, new_selected_range, window, cx)
        });
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut App) {
        self.view.update(cx, |view, cx| view.ime_unmark(cx));
    }

    /// Pastes from the platform (macOS Services, the Edit menu) are
    /// bracketed like `cmd-v`.
    fn paste(&mut self, item: ClipboardItem, _: &mut Window, cx: &mut App) {
        if let Some(text) = item.text() {
            self.view.update(cx, |view, cx| view.paste_text(&text, cx));
        }
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        Some(
            self.view
                .read(cx)
                .ime_bounds(range_utf16, &self.font, window),
        )
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<usize> {
        None
    }

    fn element_bounds(&mut self, _: &mut Window, _: &mut App) -> Option<Bounds<Pixels>> {
        Some(self.element_bounds)
    }

    fn text_length_utf16(&mut self, _: &mut Window, cx: &mut App) -> Option<usize> {
        Some(self.view.read(cx).ime.len_utf16())
    }

    fn apple_press_and_hold_enabled(&mut self) -> bool {
        false
    }

    fn accepts_text_input(&mut self, _: &mut Window, _: &mut App) -> bool {
        true
    }

    /// With an IME input source active, printable keys go to the IME first
    /// so it can compose them; keys it does not want come back as key-downs.
    fn prefers_ime_for_printable_keys(&mut self, _: &mut Window, _: &mut App) -> bool {
        true
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let palette = TerminalPalette::from_theme(theme);
        let font = font(theme.mono_font_family.clone());
        let focused = self.focus.is_focused(window);
        let view = cx.entity();
        let input_view = view.clone();
        let focus = self.focus.clone();

        div()
            .id("terminal")
            .key_context(TERMINAL_CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .overflow_hidden()
            .cursor(CursorStyle::IBeam)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .child(
                canvas(
                    {
                        let font = font.clone();
                        move |bounds, window, cx| {
                            let cell = measure_cell(&font, window);
                            view.update(cx, |view, cx| {
                                view.layout(bounds, cell, cx);
                                view.frame(&palette, focused)
                            })
                        }
                    },
                    move |bounds, frame, window, cx| {
                        window.handle_input(
                            &focus,
                            TerminalInputHandler {
                                view: input_view,
                                font: font.clone(),
                                element_bounds: bounds,
                            },
                            cx,
                        );
                        frame.paint(&font, window, cx)
                    },
                )
                .size_full(),
            )
    }
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::event::Event as TermEvent;
    use alacritty_terminal::grid::Dimensions as _;
    use std::sync::Arc;

    use alacritty_terminal::term::{ClipboardType, Config, Term};
    use alacritty_terminal::vte::ansi::Processor;
    use gpui_kit::hsla;

    use super::{ClipboardPolicy, FrameRow, GridSize, Listener, RunStyle, term_config};

    fn term(cols: usize, rows: usize) -> (Term<Listener>, Listener) {
        let events = Listener::default();
        let term = Term::new(Config::default(), &GridSize { cols, rows }, events.clone());
        (term, events)
    }

    /// Clipboard writes that `output` gets through `policy` in the view's
    /// emulator config, and whether it made the emulator ask to read the
    /// clipboard.
    fn osc52(output: &[u8], policy: ClipboardPolicy) -> (Vec<String>, bool) {
        let events = Listener::default();
        let size = GridSize { cols: 40, rows: 5 };
        let mut term = Term::new(term_config(), &size, events.clone());
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, output);
        let events = events.0.borrow();
        let stores = events
            .iter()
            .filter_map(|event| policy.store(event).map(str::to_owned))
            .collect();
        let load = events
            .iter()
            .any(|event| matches!(event, TermEvent::ClipboardLoad(..)));
        (stores, load)
    }

    /// `printf '\033]52;c;%s\a' "$(printf 'echo pwned' | base64)"`
    const STORE: &[u8] = b"\x1b]52;c;ZWNobyBwd25lZA==\x07";

    #[test]
    fn osc52_writes_are_denied_by_default() {
        assert_eq!(ClipboardPolicy::default(), ClipboardPolicy::Deny);
        let (stores, _) = osc52(STORE, ClipboardPolicy::default());
        assert!(stores.is_empty());
    }

    #[test]
    fn osc52_writes_pass_when_allowed() {
        let (stores, _) = osc52(STORE, ClipboardPolicy::AllowWrite);
        assert_eq!(stores, vec!["echo pwned".to_owned()]);
    }

    #[test]
    fn osc52_reads_are_never_answered() {
        for policy in [ClipboardPolicy::Deny, ClipboardPolicy::AllowWrite] {
            let (stores, load) = osc52(b"\x1b]52;c;?\x07", policy);
            assert!(stores.is_empty());
            assert!(!load);
            let load = TermEvent::ClipboardLoad(
                ClipboardType::Clipboard,
                Arc::new(|text: &str| text.to_owned()),
            );
            assert_eq!(policy.store(&load), None);
        }
    }

    #[test]
    fn answers_cursor_position_query() {
        let (mut term, events) = term(40, 10);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"ab\r\ncd\x1b[6n");
        let replies: Vec<String> = events
            .0
            .borrow()
            .iter()
            .filter_map(|event| match event {
                TermEvent::PtyWrite(text) => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(replies, vec!["\x1b[2;3R".to_owned()]);
    }

    #[test]
    fn resize_reflows_grid() {
        let (mut term, _) = term(10, 5);
        term.resize(GridSize { cols: 20, rows: 3 });
        assert_eq!(term.columns(), 20);
        assert_eq!(term.screen_lines(), 3);
    }

    #[test]
    fn rows_merge_equal_styles() {
        let style = RunStyle {
            color: hsla(0., 0., 1., 1.),
            bold: false,
            italic: false,
            underline: false,
            strike: false,
        };
        let mut row = FrameRow::default();
        row.text(0, 1, 'a', &[], style);
        row.text(1, 1, 'b', &[], style);
        row.text(3, 1, 'c', &[], style);
        row.text(4, 2, '漢', &[], style);
        row.text(6, 1, 'd', &[], style);
        let runs: Vec<(usize, &str)> = row.runs.iter().map(|r| (r.col, r.text.as_str())).collect();
        assert_eq!(runs, vec![(0, "ab"), (3, "c"), (4, "漢"), (6, "d")]);

        row.background(0, 1, style.color);
        row.background(1, 3, style.color);
        row.background(5, 6, style.color);
        assert_eq!(row.backgrounds.len(), 2);
        assert_eq!(row.backgrounds[0].1, 3);
    }
}
