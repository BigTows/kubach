//! ANSI escape sequences in log lines: SGR styles are kept as spans over the
//! plain text, every other control sequence is dropped.
//!
//! Programs that think they write to a terminal (gitlab-runner, npm, cargo)
//! color their logs with `ESC[…m` and redraw progress lines with `\r`.
//! [`parse`] turns such a line into the text a person reads plus the styled
//! ranges of that text, so search, filtering and copying never see escape
//! codes.

use std::ops::Range;

/// A color set by SGR.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AnsiColor {
    /// One of the 16 palette colors: 0–7 normal, 8–15 bright.
    Named(u8),
    /// An entry of the 256-color table above the first 16 (16–255).
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl AnsiColor {
    /// Color `index` of the 256-color table; the first 16 are palette colors.
    pub fn indexed(index: u8) -> Self {
        if index < 16 {
            AnsiColor::Named(index)
        } else {
            AnsiColor::Indexed(index)
        }
    }
}

/// Text attributes set by SGR. The default value is the terminal default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct AnsiStyle {
    pub foreground: Option<AnsiColor>,
    pub background: Option<AnsiColor>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
}

impl AnsiStyle {
    pub fn is_default(&self) -> bool {
        *self == AnsiStyle::default()
    }
}

/// A styled byte range of [`AnsiText::text`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnsiSpan {
    pub range: Range<usize>,
    pub style: AnsiStyle,
}

/// A line without escape sequences and the styles of its parts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnsiText {
    pub text: String,
    /// Non-overlapping, in order; text outside every span has the default
    /// style.
    pub spans: Vec<AnsiSpan>,
}

/// Whether `line` needs [`parse`]: it has escape sequences, carriage returns
/// or other control characters.
pub fn has_controls(line: &str) -> bool {
    line.bytes()
        .any(|byte| byte < 0x20 && byte != b'\t' || byte == 0x7f)
}

/// The text of `line` without escape sequences.
pub fn strip(line: &str) -> String {
    if has_controls(line) {
        parse(line).text
    } else {
        line.to_owned()
    }
}

/// Splits `line` into plain text and SGR-styled spans.
///
/// - SGR (`ESC[…m`): 16 colors and their bright variants, 256 colors and
///   true color (`;` and `:` forms), bold, dim, italic, underline,
///   strikethrough and their resets; `ESC[m`, `ESC[0m` and `ESC[0;m` reset
///   everything. Unknown codes are ignored.
/// - Other CSI sequences (cursor moves, erase line), OSC (window title,
///   hyperlinks), DCS/SOS/PM/APC strings and two-byte escapes are dropped.
/// - `\r` starts the line over, as a terminal redrawing a progress bar
///   would: only what follows the last `\r` that is followed by text stays.
///   The style carries over.
/// - Other control characters except tab are dropped.
pub fn parse(line: &str) -> AnsiText {
    let mut parser = Parser::default();
    let mut chars = line.char_indices().peekable();
    while let Some((at, ch)) = chars.next() {
        match ch {
            '\x1b' => match chars.peek().map(|&(_, next)| next) {
                Some('[') => {
                    chars.next();
                    let start = at + 2;
                    let mut end = None;
                    for (ix, ch) in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&ch) {
                            end = Some((ix, ch));
                            break;
                        }
                    }
                    // An unterminated sequence at the end of the line is
                    // dropped like any other.
                    if let Some((ix, 'm')) = end {
                        parser.sgr(&line[start..ix]);
                    }
                }
                Some(']' | 'P' | 'X' | '^' | '_') => {
                    chars.next();
                    // String sequences end with BEL or ST (`ESC \`).
                    while let Some((_, ch)) = chars.next() {
                        if ch == '\x07' {
                            break;
                        }
                        if ch == '\x1b' {
                            if chars.peek().map(|&(_, next)| next) == Some('\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                Some(_) => {
                    // `ESC` + intermediates (0x20–0x2f) + final byte, e.g.
                    // `ESC ( B` or `ESC =`.
                    for (_, ch) in chars.by_ref() {
                        if !('\x20'..='\x2f').contains(&ch) {
                            break;
                        }
                    }
                }
                None => {}
            },
            '\r' => parser.carriage_return(),
            '\t' => parser.push(ch),
            ch if ch.is_control() => {}
            ch => parser.push(ch),
        }
    }
    parser.finish()
}

#[derive(Default)]
struct Parser {
    out: AnsiText,
    style: AnsiStyle,
    /// Start of the text written with `style`.
    run_start: usize,
    /// A `\r` was seen; the next character starts the line over.
    restart: bool,
}

impl Parser {
    fn push(&mut self, ch: char) {
        if self.restart {
            self.restart = false;
            self.out.text.clear();
            self.out.spans.clear();
            self.run_start = 0;
        }
        self.out.text.push(ch);
    }

    fn carriage_return(&mut self) {
        self.restart = true;
    }

    /// Closes the run written with the current style.
    fn close_run(&mut self) {
        let end = self.out.text.len();
        if end > self.run_start && !self.style.is_default() {
            self.out.spans.push(AnsiSpan {
                range: self.run_start..end,
                style: self.style,
            });
        }
        self.run_start = end;
    }

    fn set_style(&mut self, style: AnsiStyle) {
        if style != self.style {
            self.close_run();
            self.style = style;
        }
    }

    fn sgr(&mut self, params: &str) {
        // Private sequences (`ESC[?…m`, `ESC[>…m`) are not SGR.
        if params.starts_with(['<', '=', '>', '?']) {
            return;
        }
        let mut style = self.style;
        let groups: Vec<Vec<Option<u32>>> = params
            .split(';')
            .map(|group| group.split(':').map(|n| n.parse().ok()).collect())
            .collect();
        let mut groups = groups.into_iter();
        while let Some(group) = groups.next() {
            let code = group.first().copied().flatten().unwrap_or(0);
            match code {
                0 => style = AnsiStyle::default(),
                1 => style.bold = true,
                2 => style.dim = true,
                3 => style.italic = true,
                4 => style.underline = group.get(1).copied().flatten() != Some(0),
                9 => style.strikethrough = true,
                21 => style.underline = true,
                22 => {
                    style.bold = false;
                    style.dim = false;
                }
                23 => style.italic = false,
                24 => style.underline = false,
                29 => style.strikethrough = false,
                30..=37 => style.foreground = Some(AnsiColor::Named((code - 30) as u8)),
                39 => style.foreground = None,
                40..=47 => style.background = Some(AnsiColor::Named((code - 40) as u8)),
                49 => style.background = None,
                90..=97 => style.foreground = Some(AnsiColor::Named((code - 90 + 8) as u8)),
                100..=107 => style.background = Some(AnsiColor::Named((code - 100 + 8) as u8)),
                38 | 48 => {
                    let color = if group.len() > 1 {
                        extended_color(&group[1..])
                    } else {
                        extended_color_from(&mut groups)
                    };
                    if let Some(color) = color {
                        if code == 38 {
                            style.foreground = Some(color);
                        } else {
                            style.background = Some(color);
                        }
                    }
                }
                _ => {}
            }
        }
        self.set_style(style);
    }

    fn finish(mut self) -> AnsiText {
        self.close_run();
        self.out
    }
}

/// `5:n` or `2:[colorspace:]r:g:b` after `38:` / `48:`.
fn extended_color(params: &[Option<u32>]) -> Option<AnsiColor> {
    match params.first().copied().flatten()? {
        5 => Some(AnsiColor::indexed(byte(params.get(1).copied().flatten()?))),
        2 => {
            // `38:2::r:g:b` has an (empty) color space id before the
            // channels; `38:2:r:g:b` does not.
            let channels = if params.len() >= 5 {
                &params[2..5]
            } else {
                params.get(1..4)?
            };
            Some(AnsiColor::Rgb(
                byte(channels[0].unwrap_or(0)),
                byte(channels[1].unwrap_or(0)),
                byte(channels[2].unwrap_or(0)),
            ))
        }
        _ => None,
    }
}

/// `;5;n` or `;2;r;g;b` after `38` / `48`, taken from the following groups.
fn extended_color_from(groups: &mut impl Iterator<Item = Vec<Option<u32>>>) -> Option<AnsiColor> {
    let mut next = || {
        groups
            .next()
            .and_then(|group| group.first().copied().flatten())
    };
    match next()? {
        5 => Some(AnsiColor::indexed(byte(next()?))),
        2 => {
            let r = next().unwrap_or(0);
            let g = next().unwrap_or(0);
            let b = next().unwrap_or(0);
            Some(AnsiColor::Rgb(byte(r), byte(g), byte(b)))
        }
        _ => None,
    }
}

fn byte(value: u32) -> u8 {
    value.min(255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fg(color: AnsiColor) -> AnsiStyle {
        AnsiStyle {
            foreground: Some(color),
            ..AnsiStyle::default()
        }
    }

    fn span(range: Range<usize>, style: AnsiStyle) -> AnsiSpan {
        AnsiSpan { range, style }
    }

    #[test]
    fn plain_text_passes_through() {
        let parsed = parse("hello\tworld");
        assert_eq!(parsed.text, "hello\tworld");
        assert!(parsed.spans.is_empty());
        assert!(!has_controls("hello\tworld"));
        assert!(has_controls("a\x1b[0m"));
    }

    #[test]
    fn gitlab_runner_lines() {
        let parsed = parse("\x1b[0;33mWARNING: no cache\x1b[0;m");
        assert_eq!(parsed.text, "WARNING: no cache");
        assert_eq!(parsed.spans, vec![span(0..17, fg(AnsiColor::Named(3)))]);

        let parsed = parse("\x1b[31;1mPANIC\x1b[0;m: boom");
        assert_eq!(parsed.text, "PANIC: boom");
        assert_eq!(
            parsed.spans,
            vec![span(
                0..5,
                AnsiStyle {
                    bold: true,
                    ..fg(AnsiColor::Named(1))
                }
            )]
        );
    }

    #[test]
    fn resets() {
        for reset in ["\x1b[0m", "\x1b[m", "\x1b[0;m", "\x1b[;m"] {
            let parsed = parse(&format!("\x1b[32ma{reset}b"));
            assert_eq!(parsed.text, "ab", "{reset:?}");
            assert_eq!(
                parsed.spans,
                vec![span(0..1, fg(AnsiColor::Named(2)))],
                "{reset:?}"
            );
        }
    }

    #[test]
    fn bright_and_background_colors() {
        let parsed = parse("\x1b[91;104mx\x1b[39;49my");
        assert_eq!(
            parsed.spans,
            vec![span(
                0..1,
                AnsiStyle {
                    background: Some(AnsiColor::Named(12)),
                    ..fg(AnsiColor::Named(9))
                }
            )]
        );
        assert_eq!(parsed.text, "xy");
    }

    #[test]
    fn extended_colors() {
        let cases = [
            ("\x1b[38;5;196mx", AnsiColor::Indexed(196)),
            ("\x1b[38;5;9mx", AnsiColor::Named(9)),
            ("\x1b[38;2;10;20;30mx", AnsiColor::Rgb(10, 20, 30)),
            ("\x1b[38:5:202mx", AnsiColor::Indexed(202)),
            ("\x1b[38:2:1:2:3mx", AnsiColor::Rgb(1, 2, 3)),
            ("\x1b[38:2::4:5:6mx", AnsiColor::Rgb(4, 5, 6)),
        ];
        for (line, color) in cases {
            let parsed = parse(line);
            assert_eq!(parsed.text, "x", "{line:?}");
            assert_eq!(parsed.spans, vec![span(0..1, fg(color))], "{line:?}");
        }
        let parsed = parse("\x1b[48;2;1;2;3;1mx");
        assert_eq!(
            parsed.spans[0].style,
            AnsiStyle {
                background: Some(AnsiColor::Rgb(1, 2, 3)),
                bold: true,
                ..AnsiStyle::default()
            }
        );
    }

    #[test]
    fn attributes_and_their_resets() {
        let parsed = parse("\x1b[1;2;3;4;9ma\x1b[22mb\x1b[23;24;29mc");
        assert_eq!(parsed.text, "abc");
        let all = AnsiStyle {
            bold: true,
            dim: true,
            italic: true,
            underline: true,
            strikethrough: true,
            ..AnsiStyle::default()
        };
        assert_eq!(
            parsed.spans,
            vec![
                span(0..1, all),
                span(
                    1..2,
                    AnsiStyle {
                        bold: false,
                        dim: false,
                        ..all
                    }
                ),
            ]
        );
        assert!(parse("\x1b[4:0mx").spans.is_empty());
    }

    #[test]
    fn same_style_merges_into_one_span() {
        let parsed = parse("\x1b[33ma\x1b[33mb\x1b[0m");
        assert_eq!(parsed.spans, vec![span(0..2, fg(AnsiColor::Named(3)))]);
    }

    #[test]
    fn other_sequences_are_dropped() {
        let parsed =
            parse("\x1b[2K\x1b[1Ga\x1b]0;title\x07b\x1b]8;;http://x\x1b\\c\x1b(Bd\x1b[?25le");
        assert_eq!(parsed.text, "abcde");
        assert!(parsed.spans.is_empty());
        assert_eq!(parse("a\x1b[").text, "a");
        assert_eq!(parse("a\x1b").text, "a");
        assert_eq!(parse("a\x07b\x08c").text, "abc");
    }

    #[test]
    fn carriage_return_keeps_last_segment() {
        assert_eq!(parse("10%\r50%\r100%").text, "100%");
        assert_eq!(parse("done\r").text, "done");
        assert_eq!(parse("done\r\n").text, "done");
        let parsed = parse("\x1b[32m10%\r\x1b[1m100%");
        assert_eq!(parsed.text, "100%");
        assert_eq!(
            parsed.spans,
            vec![span(
                0..4,
                AnsiStyle {
                    bold: true,
                    ..fg(AnsiColor::Named(2))
                }
            )]
        );
    }

    #[test]
    fn unicode_offsets_are_bytes() {
        let parsed = parse("привет \x1b[36mмир\x1b[0m!");
        assert_eq!(parsed.text, "привет мир!");
        assert_eq!(&parsed.text[parsed.spans[0].range.clone()], "мир");
        assert_eq!(strip("\x1b[1mж\x1b[0m"), "ж");
    }
}
