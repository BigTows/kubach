use std::path::PathBuf;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

/// A small square icon of a chart or an app: an image when there is one,
/// else (and while it loads or when it fails) the first letter of the name
/// on a muted tile. The caller decides where the image comes from; the
/// component never fetches anything itself.
#[derive(IntoElement)]
pub struct ChartIcon {
    name: SharedString,
    image: Option<ImageSource>,
    size: Pixels,
}

impl ChartIcon {
    pub fn new(name: impl Into<SharedString>) -> Self {
        Self {
            name: name.into(),
            image: None,
            size: px(18.),
        }
    }

    /// The image to draw instead of the letter.
    pub fn image(mut self, image: impl Into<ImageSource>) -> Self {
        self.image = Some(image.into());
        self
    }

    /// The image file to draw (PNG, JPEG, GIF, WebP or SVG), if any.
    pub fn path(mut self, path: Option<PathBuf>) -> Self {
        self.image = path.map(ImageSource::from);
        self
    }

    /// Side of the square, 18 px by default.
    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.size = size.into();
        self
    }

    /// The letter shown without an image: the first alphanumeric character
    /// of the name's last path segment, upper-cased (`bitnami/nginx` → `N`).
    pub fn letter(name: &str) -> String {
        name.rsplit('/')
            .next()
            .unwrap_or(name)
            .chars()
            .find(|c| c.is_alphanumeric())
            .map(|c| c.to_uppercase().collect())
            .unwrap_or_else(|| "?".to_owned())
    }
}

impl RenderOnce for ChartIcon {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let size = self.size;
        let letter = Self::letter(&self.name);
        let (bg, border, fg) = (theme.muted, theme.border, theme.muted_foreground);
        let placeholder = move || {
            div()
                .size(size)
                .flex()
                .items_center()
                .justify_center()
                .rounded(size * 0.25)
                .border_1()
                .border_color(border)
                .bg(bg)
                .text_color(fg)
                .text_size(size * 0.55)
                .font_weight(FontWeight::SEMIBOLD)
                .child(letter.clone())
                .into_any_element()
        };
        let body = match self.image {
            Some(image) => img(image)
                .size(size)
                .object_fit(ObjectFit::Contain)
                .with_fallback(placeholder.clone())
                .with_loading(placeholder.clone())
                .into_any_element(),
            None => placeholder(),
        };
        div()
            .flex_shrink_0()
            .size(size)
            .flex()
            .items_center()
            .justify_center()
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::ChartIcon;

    #[test]
    fn letter_of_the_chart_name() {
        assert_eq!(ChartIcon::letter("bitnami/nginx"), "N");
        assert_eq!(ChartIcon::letter("ingress-nginx"), "I");
        assert_eq!(ChartIcon::letter("_x"), "X");
        assert_eq!(ChartIcon::letter("ёлка"), "Ё");
        assert_eq!(ChartIcon::letter(""), "?");
    }
}
