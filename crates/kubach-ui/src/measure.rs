//! Widths of text, for layouts sized to their content (#444): a Russian
//! label is often half as long again as the English one, so columns of
//! labels take the width of the widest instead of a constant.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

/// Size of `text_xs` text.
pub const TEXT_XS: Rems = Rems(0.75);
/// Size of `text_sm` text.
pub const TEXT_SM: Rems = Rems(0.875);

/// Width of `text` on one line in the UI font at `size` and `weight`,
/// with the CJK fallbacks the windows use.
pub fn text_width(text: &str, size: Rems, weight: FontWeight, window: &Window, cx: &App) -> Pixels {
    let family = cx.theme().font_family.clone();
    width_in(family, text, size, weight, window, cx)
}

/// Width of `text` on one line in the monospaced font at `size`, as
/// numbers on a chart's axis are drawn.
pub fn mono_text_width(text: &str, size: Rems, window: &Window, cx: &App) -> Pixels {
    let family = cx.theme().mono_font_family.clone();
    width_in(family, text, size, FontWeight::NORMAL, window, cx)
}

fn width_in(
    family: SharedString,
    text: &str,
    size: Rems,
    weight: FontWeight,
    window: &Window,
    cx: &App,
) -> Pixels {
    if text.is_empty() {
        return px(0.);
    }
    let text: SharedString = text.replace(['\n', '\r'], " ").into();
    let run = TextRun {
        len: text.len(),
        font: Font {
            weight,
            fallbacks: crate::theme::text_fallbacks(cx),
            ..font(family)
        },
        color: Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line(text, size.to_pixels(window.rem_size()), &[run], None)
        .width
}

/// Width of a column of labels: the widest of `labels` at `size`, but at
/// least `min` (the column stays put in English) and at most `max` (longer
/// labels wrap).
pub fn label_column_width<'a>(
    labels: impl IntoIterator<Item = &'a str>,
    size: Rems,
    min: Pixels,
    max: Pixels,
    window: &Window,
    cx: &App,
) -> Pixels {
    let widest = labels
        .into_iter()
        .map(|label| text_width(label, size, FontWeight::NORMAL, window, cx))
        .fold(px(0.), Pixels::max);
    // Rounded up: a column one subpixel too narrow wraps its widest label.
    px(f32::from(widest).ceil()).clamp(min, max)
}
