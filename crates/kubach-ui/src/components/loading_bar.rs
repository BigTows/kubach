//! A thin indeterminate progress bar: a short segment gliding across a
//! quiet track, for work of unknown length such as a list refreshing in
//! the background. Sits along an edge (the status bar's top) and takes no
//! layout space there.

use std::time::Duration;

use gpui_kit::*;

use crate::theme::loading_bar_colors;

/// Height of the bar.
pub const LOADING_BAR_HEIGHT: Pixels = px(2.);

/// One pass of the segment across the track.
const PASS: Duration = Duration::from_millis(1400);

/// Width of the segment, a share of the track.
const SEGMENT: f32 = 0.3;

/// Left edge of the segment, as a share of the track, at `t` of a pass:
/// from just off the left end to just off the right end.
pub fn segment_offset(t: f32) -> f32 {
    -SEGMENT + t.clamp(0., 1.) * (1. + SEGMENT)
}

/// Smooth start and stop of each pass.
fn ease_in_out(t: f32) -> f32 {
    if t < 0.5 {
        2. * t * t
    } else {
        1. - (-2. * t + 2.).powi(2) / 2.
    }
}

/// Indeterminate progress bar spanning its parent's width.
#[derive(IntoElement)]
pub struct LoadingBar {
    id: ElementId,
}

impl LoadingBar {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self { id: id.into() }
    }
}

impl RenderOnce for LoadingBar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let (bar, track) = loading_bar_colors(cx);
        div()
            .id(self.id.clone())
            .relative()
            .w_full()
            .h(LOADING_BAR_HEIGHT)
            .overflow_hidden()
            .bg(track)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .h_full()
                    .w(relative(SEGMENT))
                    .rounded_full()
                    .bg(bar)
                    .with_animation(
                        self.id,
                        Animation::new(PASS).repeat().with_easing(ease_in_out),
                        |segment, t| segment.left(relative(segment_offset(t))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{SEGMENT, ease_in_out, segment_offset};

    #[test]
    fn the_segment_enters_and_leaves_the_track() {
        assert_eq!(segment_offset(0.), -SEGMENT);
        assert!((segment_offset(1.) - 1.).abs() < 1e-6);
        assert!(segment_offset(0.5) > 0. && segment_offset(0.5) < 1.);
        // Out-of-range easing output stays on the track's path.
        assert_eq!(segment_offset(-1.), -SEGMENT);
    }

    #[test]
    fn passes_ease_in_and_out() {
        assert_eq!(ease_in_out(0.), 0.);
        assert_eq!(ease_in_out(1.), 1.);
        assert!((ease_in_out(0.5) - 0.5).abs() < 1e-6);
        assert!(ease_in_out(0.1) < 0.1);
    }
}
