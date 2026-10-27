use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

/// A small read-only table for the details panel (RBAC rules, version
/// lists, recommendations): muted headers, monospace cells that wrap, a
/// thin line between rows. Columns share the width equally.
#[derive(IntoElement)]
pub struct CompactTable {
    columns: Vec<SharedString>,
    rows: Vec<Vec<SharedString>>,
}

impl CompactTable {
    pub fn new<S: Into<SharedString>>(columns: impl IntoIterator<Item = S>) -> Self {
        Self {
            columns: columns.into_iter().map(Into::into).collect(),
            rows: Vec::new(),
        }
    }

    /// Adds a row; missing cells are left empty, extra ones are dropped.
    pub fn row<S: Into<SharedString>>(mut self, cells: impl IntoIterator<Item = S>) -> Self {
        self.rows.push(cells.into_iter().map(Into::into).collect());
        self
    }
}

impl RenderOnce for CompactTable {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let width = self.columns.len();
        let cell =
            |content: SharedString| div().flex_1().min_w_0().whitespace_normal().child(content);
        let header = div()
            .flex()
            .gap_3()
            .pb_1()
            .text_xs()
            .text_color(theme.muted_foreground)
            .children(self.columns.into_iter().map(cell));
        div()
            .flex()
            .flex_col()
            .text_sm()
            .child(header)
            .children(self.rows.into_iter().map(|row| {
                let mut cells = row;
                cells.resize(width, SharedString::default());
                div()
                    .flex()
                    .gap_3()
                    .py_1()
                    .border_t_1()
                    .border_color(theme.border)
                    .font_family(theme.mono_font_family.clone())
                    .text_color(theme.foreground)
                    .children(cells.into_iter().map(cell))
            }))
    }
}
