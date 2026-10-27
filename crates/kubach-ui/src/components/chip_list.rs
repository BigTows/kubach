//! `key=value` chips (labels, annotations, selectors) that fold after a few
//! and copy themselves on click.

use std::rc::Rc;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_i18n::t;

/// Chips shown while folded, unless [`ChipList::folded`] says otherwise.
const FOLDED: usize = 4;
/// Longest value shown on a chip; the copy has all of it.
const VALUE_MAX: usize = 48;
/// Longest value in a chip's tooltip.
const TOOLTIP_MAX: usize = 400;

type Toggle = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

/// Chips with a fold toggle and a "copy all" button. The owner keeps the
/// folded state and flips it in [`on_toggle`](Self::on_toggle).
#[derive(IntoElement)]
pub struct ChipList {
    id: SharedString,
    chips: Vec<(SharedString, SharedString)>,
    expanded: bool,
    folded: usize,
    on_toggle: Option<Toggle>,
}

impl ChipList {
    pub fn new(
        id: impl Into<SharedString>,
        chips: impl IntoIterator<Item = (impl Into<SharedString>, impl Into<SharedString>)>,
    ) -> Self {
        Self {
            id: id.into(),
            chips: chips
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
            expanded: false,
            folded: FOLDED,
            on_toggle: None,
        }
    }

    /// Shows every chip instead of the first few.
    pub fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        self
    }

    /// Chips shown while folded; 0 hides them all behind "Show N".
    pub fn folded(mut self, shown: usize) -> Self {
        self.folded = shown;
        self
    }

    /// Called by the fold toggle. Without it every chip is shown.
    pub fn on_toggle(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_toggle = Some(Rc::new(handler));
        self
    }

    /// All chips as `key=value` lines, as "copy all" copies them.
    pub fn text(&self) -> String {
        self.chips
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl RenderOnce for ChipList {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let all = self.text();
        let total = self.chips.len();
        let foldable = self.on_toggle.is_some() && total > self.folded;
        let shown = if foldable && !self.expanded {
            self.folded
        } else {
            total
        };
        let hides_all = self.folded == 0;
        let id = self.id.clone();
        let chips = self
            .chips
            .into_iter()
            .take(shown)
            .enumerate()
            .map(|(ix, (key, value))| {
                let copy = format!("{key}={value}");
                let tip = format!(
                    "{key}={}\n{}",
                    clip(&value, TOOLTIP_MAX),
                    t!("ui-chip-click-to-copy")
                );
                div()
                    .id((id.clone(), ix))
                    .flex()
                    .flex_none()
                    .max_w_full()
                    .h(px(22.))
                    .items_center()
                    .px_2()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.muted)
                    .font_family(theme.mono_font_family.clone())
                    .text_xs()
                    .cursor_pointer()
                    .hover(|chip| chip.border_color(theme.muted_foreground))
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                    })
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.muted_foreground)
                            .child(key),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.muted_foreground)
                            .child("="),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.foreground)
                            .child(clip(&value, VALUE_MAX)),
                    )
            });
        let toggle = self.on_toggle.filter(|_| foldable).map(|handler| {
            let label = match (self.expanded, hides_all) {
                (true, true) => t!("ui-chip-hide"),
                (true, false) => t!("ui-chip-show-less"),
                (false, true) => t!("ui-chip-show-all", count = total),
                (false, false) => t!("ui-chip-more", count = total - shown),
            };
            div()
                .id((id.clone(), usize::MAX))
                .flex_none()
                .h(px(22.))
                .px_1()
                .flex()
                .items_center()
                .text_xs()
                .text_color(theme.muted_foreground)
                .cursor_pointer()
                .hover(|toggle| toggle.text_color(theme.foreground))
                .on_click(move |event, window, cx| handler(event, window, cx))
                .child(label)
        });
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .children(chips)
            .children(toggle)
            .when(total > 1, |list| {
                list.child(
                    Clipboard::new((id.clone(), usize::MAX - 1))
                        .value(all)
                        .tooltip(t!("ui-chip-copy-all")),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{ChipList, clip};

    #[test]
    fn copies_all_as_lines_and_clips_long_values() {
        let list = ChipList::new("chips", [("app", "web"), ("tier", "front")]);
        assert_eq!(list.text(), "app=web\ntier=front");
        assert_eq!(clip("abcdef", 3), "abc…");
        assert_eq!(clip("abc", 3), "abc");
    }
}
