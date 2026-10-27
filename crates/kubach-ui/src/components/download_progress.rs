use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_i18n::t;

type CancelHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// A running download: what is fetched, bytes received of the total (a
/// bar when the total is known) and a cancel button.
#[derive(IntoElement)]
pub struct DownloadProgress {
    id: ElementId,
    title: SharedString,
    downloaded: u64,
    total: Option<u64>,
    on_cancel: Option<CancelHandler>,
}

impl DownloadProgress {
    /// `title` like "Downloading Helm 3.22.1".
    pub fn new(id: impl Into<ElementId>, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            downloaded: 0,
            total: None,
            on_cancel: None,
        }
    }

    /// Bytes received and the expected size, when known.
    pub fn bytes(mut self, downloaded: u64, total: Option<u64>) -> Self {
        self.downloaded = downloaded;
        self.total = total;
        self
    }

    pub fn on_cancel(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_cancel = Some(Rc::new(handler));
        self
    }
}

/// `12.3 MB`, `640 KB`, `12 B`.
pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.;
    let value = bytes as f64;
    if value >= KB * KB {
        format!("{:.1} MB", value / (KB * KB))
    } else if value >= KB {
        format!("{:.0} KB", value / KB)
    } else {
        format!("{bytes} B")
    }
}

impl RenderOnce for DownloadProgress {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let status = match self.total {
            Some(total) => format!(
                "{} / {}",
                format_bytes(self.downloaded),
                format_bytes(total)
            ),
            None => format_bytes(self.downloaded),
        };
        let fraction = self
            .total
            .filter(|total| *total > 0)
            .map(|total| (self.downloaded as f32 / total as f32).clamp(0., 1.));
        let cancel_id = ElementId::NamedChild(Arc::new(self.id.clone()), "cancel".into());
        let bar_id = ElementId::NamedChild(Arc::new(self.id.clone()), "bar".into());
        div()
            .id(self.id)
            .flex()
            .flex_col()
            .gap_1p5()
            .min_w(px(220.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .child(
                        crate::spinner()
                            .icon(Icon::new(IconName::LoaderCircle))
                            .with_size(px(14.))
                            .color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.foreground)
                            .child(self.title),
                    )
                    .child(div().text_color(theme.muted_foreground).child(status))
                    .when_some(self.on_cancel, |row, handler| {
                        row.child(
                            Button::new(cancel_id)
                                .ghost()
                                .xsmall()
                                .label(t!("ui-cancel"))
                                .on_click(move |event, window, cx| handler(event, window, cx)),
                        )
                    }),
            )
            .when_some(fraction, |this, fraction| {
                this.child(Progress::new(bar_id).value(fraction * 100.))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::format_bytes;

    #[test]
    fn bytes_are_human_readable() {
        assert_eq!(format_bytes(12), "12 B");
        assert_eq!(format_bytes(640 * 1024), "640 KB");
        assert_eq!(format_bytes(17_058_141), "16.3 MB");
    }
}
