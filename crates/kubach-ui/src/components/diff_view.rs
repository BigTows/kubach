use std::sync::Arc;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use crate::tokens::TINT_OPACITY;
use kubach_i18n::t;

/// Unchanged lines kept around each change.
const DEFAULT_CONTEXT: usize = 3;
const ROW_HEIGHT: Pixels = px(20.);
const GUTTER_WIDTH: Pixels = px(44.);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffLineKind {
    Unchanged,
    Removed,
    Added,
}

/// One line of a line diff, with 0-based numbers on each side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffViewLine {
    pub kind: DiffLineKind,
    pub old: Option<usize>,
    pub new: Option<usize>,
    pub text: SharedString,
}

/// A displayed row: a line, or a run of hidden unchanged lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffRow {
    Line(DiffViewLine),
    Folded { hidden: usize },
}

/// Collapses unchanged runs to `context` lines around each change. Without
/// any change, everything is one fold.
pub fn fold_unchanged(lines: &[DiffViewLine], context: usize) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if lines[index].kind != DiffLineKind::Unchanged {
            rows.push(DiffRow::Line(lines[index].clone()));
            index += 1;
            continue;
        }
        let start = index;
        while index < lines.len() && lines[index].kind == DiffLineKind::Unchanged {
            index += 1;
        }
        let run = &lines[start..index];
        let head = if start == 0 { 0 } else { context };
        let tail = if index == lines.len() { 0 } else { context };
        if run.len() > head + tail {
            rows.extend(run[..head].iter().cloned().map(DiffRow::Line));
            rows.push(DiffRow::Folded {
                hidden: run.len() - head - tail,
            });
            rows.extend(run[run.len() - tail..].iter().cloned().map(DiffRow::Line));
        } else {
            rows.extend(run.iter().cloned().map(DiffRow::Line));
        }
    }
    rows
}

/// Unified line diff with old/new line numbers, tinted additions and
/// removals, and folded unchanged runs.
///
/// Rows are virtualized (`uniform_list`): only the visible ones are laid
/// out, so a manifest of thousands of lines scrolls smoothly. Screens that
/// re-render often pass rows folded once ([`DiffView::from_rows`]) instead
/// of folding the lines on every frame.
#[derive(IntoElement)]
pub struct DiffView {
    id: ElementId,
    source: Source,
}

enum Source {
    Lines {
        lines: Vec<DiffViewLine>,
        context: usize,
    },
    Rows(Arc<[DiffRow]>),
}

impl DiffView {
    pub fn new(id: impl Into<ElementId>, lines: Vec<DiffViewLine>) -> Self {
        Self {
            id: id.into(),
            source: Source::Lines {
                lines,
                context: DEFAULT_CONTEXT,
            },
        }
    }

    /// Rows already folded with [`fold_rows`] (or [`fold_unchanged`]).
    pub fn from_rows(id: impl Into<ElementId>, rows: Arc<[DiffRow]>) -> Self {
        Self {
            id: id.into(),
            source: Source::Rows(rows),
        }
    }

    /// Unchanged lines kept around each change (default 3); only for
    /// [`DiffView::new`].
    pub fn context(mut self, lines: usize) -> Self {
        if let Source::Lines { context, .. } = &mut self.source {
            *context = lines;
        }
        self
    }
}

/// [`fold_unchanged`] with the default context, folded once and shared
/// for [`DiffView::from_rows`].
pub fn fold_rows(lines: &[DiffViewLine]) -> Arc<[DiffRow]> {
    fold_unchanged(lines, DEFAULT_CONTEXT).into()
}

fn render_row(row: &DiffRow, cx: &App) -> Div {
    let theme = cx.theme();
    let number = |n: Option<usize>| {
        div()
            .w(GUTTER_WIDTH)
            .flex_shrink_0()
            .pr_2()
            .text_right()
            .text_color(theme.muted_foreground)
            .child(n.map(|n| (n + 1).to_string()).unwrap_or_default())
    };
    match row {
        DiffRow::Folded { hidden } => div()
            .h(ROW_HEIGHT)
            .flex()
            .items_center()
            .pl(GUTTER_WIDTH * 2.)
            .bg(theme.muted)
            .text_color(theme.muted_foreground)
            .child(t!("ui-diff-unchanged-lines", count = *hidden)),
        DiffRow::Line(line) => {
            let (sign, tone) = match line.kind {
                DiffLineKind::Unchanged => (" ", None),
                DiffLineKind::Removed => ("-", Some(theme.danger)),
                DiffLineKind::Added => ("+", Some(theme.success)),
            };
            let mut row = div()
                .h(ROW_HEIGHT)
                .flex()
                .items_center()
                .whitespace_nowrap()
                .child(number(line.old))
                .child(number(line.new))
                .child(
                    div()
                        .w(px(16.))
                        .flex_shrink_0()
                        .text_color(tone.unwrap_or(theme.muted_foreground))
                        .child(sign),
                )
                .child(div().flex_1().min_w_0().child(line.text.clone()));
            if let Some(tone) = tone {
                row = row.bg(tone.opacity(TINT_OPACITY));
            }
            row
        }
    }
}

impl RenderOnce for DiffView {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let rows: Arc<[DiffRow]> = match self.source {
            Source::Lines { lines, context } => fold_unchanged(&lines, context).into(),
            Source::Rows(rows) => rows,
        };
        let count = rows.len();
        div()
            .size_full()
            .py_1()
            .font_family(theme.mono_font_family.clone())
            .text_xs()
            .text_color(theme.foreground)
            .child(
                uniform_list(self.id, count, move |range, _, cx| {
                    range.map(|ix| render_row(&rows[ix], cx)).collect()
                })
                .size_full(),
            )
    }
}

#[cfg(test)]
mod tests {
    // Not a glob: `gpui_kit::*` exports a `test` attribute that would shadow std's.
    use super::{DiffLineKind, DiffRow, DiffViewLine, fold_unchanged};
    use gpui_kit::SharedString;

    fn lines(kinds: &str) -> Vec<DiffViewLine> {
        kinds
            .chars()
            .enumerate()
            .map(|(i, c)| DiffViewLine {
                kind: match c {
                    '-' => DiffLineKind::Removed,
                    '+' => DiffLineKind::Added,
                    _ => DiffLineKind::Unchanged,
                },
                old: Some(i),
                new: Some(i),
                text: SharedString::from(i.to_string()),
            })
            .collect()
    }

    /// `=` for an unchanged line, `-`/`+` for changes, a number for a fold.
    fn shape(rows: &[DiffRow]) -> String {
        rows.iter()
            .map(|row| match row {
                DiffRow::Folded { hidden } => format!("[{hidden}]"),
                DiffRow::Line(line) => match line.kind {
                    DiffLineKind::Unchanged => "=".into(),
                    DiffLineKind::Removed => "-".into(),
                    DiffLineKind::Added => "+".into(),
                },
            })
            .collect()
    }

    #[test]
    fn folds_long_unchanged_runs() {
        let rows = fold_unchanged(&lines("==========-+=========="), 3);
        assert_eq!(shape(&rows), "[7]===-+===[7]");
        // The kept lines are the ones next to the change.
        let DiffRow::Line(first) = &rows[1] else {
            panic!()
        };
        assert_eq!(first.old, Some(7));
    }

    #[test]
    fn keeps_short_runs_between_changes() {
        assert_eq!(shape(&fold_unchanged(&lines("-======+"), 3)), "-======+");
        assert_eq!(
            shape(&fold_unchanged(&lines("-=======+"), 3)),
            "-===[1]===+"
        );
    }

    #[test]
    fn folds_everything_without_changes() {
        assert_eq!(shape(&fold_unchanged(&lines("====="), 3)), "[5]");
        assert!(fold_unchanged(&[], 3).is_empty());
        assert_eq!(shape(&fold_unchanged(&lines("++"), 3)), "++");
    }

    #[test]
    fn folded_rows_are_counted_in_each_language() {
        use kubach_i18n::{Locale, msg};
        let text =
            |count: usize, locale| msg!("ui-diff-unchanged-lines", count = count).translate(locale);
        assert_eq!(text(1, Locale::En), "⋯ 1 unchanged line");
        assert_eq!(text(4, Locale::En), "⋯ 4 unchanged lines");
        assert_eq!(text(1, Locale::Ru), "⋯ 1 строка без изменений");
        assert_eq!(text(3, Locale::Ru), "⋯ 3 строки без изменений");
        assert_eq!(text(5, Locale::Ru), "⋯ 5 строк без изменений");
        assert_eq!(text(21, Locale::Ru), "⋯ 21 строка без изменений");
        assert_eq!(text(5, Locale::ZhHans), "⋯ 5 行未更改");
    }
}
