use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::components::graph::TrafficLevel;
use crate::theme::traffic_palette;
use kubach_i18n::t;

/// Key of the traffic layer of a [`GraphView`](crate::GraphView) (#326):
/// the active source, then a line sample per [`TrafficLevel`] (thicker for
/// more load) and the long-dashed line of traffic the structure doesn't
/// explain. Colors come from the theme's `graph.traffic.*` tokens.
#[derive(IntoElement, Default)]
pub struct TrafficLegend {
    source: Option<SharedString>,
    unmapped: bool,
}

impl TrafficLegend {
    pub fn new() -> Self {
        Self::default()
    }

    /// `Istio · 5m`: where the numbers come from, shown first.
    pub fn source(mut self, source: impl Into<SharedString>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Adds the sample of traffic without a Service.
    pub fn unmapped(mut self, shown: bool) -> Self {
        self.unmapped = shown;
        self
    }
}

/// Line thickness of a level's sample: what the canvas draws for its load.
fn sample_width(level: TrafficLevel) -> f32 {
    match level {
        TrafficLevel::Low => 1.5,
        TrafficLevel::Medium => 2.5,
        TrafficLevel::High | TrafficLevel::Degraded | TrafficLevel::Failing => 3.5,
    }
}

impl RenderOnce for TrafficLegend {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let palette = traffic_palette(cx);
        let item = |sample: Div, label: String| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_1p5()
                .child(sample)
                .child(label)
        };
        let line = |color: Hsla, width: f32| div().w(px(18.)).h(px(width)).rounded_full().bg(color);
        let dashed = |color: Hsla| {
            div()
                .flex()
                .gap(px(2.))
                .children((0..3).map(move |_| div().w(px(5.)).h(px(2.)).bg(color)))
        };
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_3()
            .text_xs()
            .text_color(muted)
            .children(self.source.map(|source| {
                div()
                    .flex_none()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child(source)
            }))
            .children(TrafficLevel::ALL.into_iter().map(|level| {
                item(
                    line(palette[level.index()], sample_width(level)),
                    level.label(),
                )
            }))
            .when(self.unmapped, |this| {
                this.child(item(
                    dashed(palette[TrafficLevel::Medium.index()]),
                    t!("ui-traffic-no-service"),
                ))
            })
    }
}
