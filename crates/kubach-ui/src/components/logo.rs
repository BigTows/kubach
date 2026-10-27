//! The Kubach logo: three isometric cubes, the app icon redrawn in the
//! theme's foreground so it fits Kubach Dark and Light.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

/// Faces of the cubes in the 512 × 512 app icon
/// (`crates/kubach-app/assets/icon/kubach.svg`): shade and corners.
const FACES: [(Shade, [(f32, f32); 4]); 9] = [
    (
        Shade::Top,
        [(256., 54.), (366., 116.), (256., 179.), (146., 116.)],
    ),
    (
        Shade::Left,
        [(146., 116.), (256., 179.), (256., 306.), (146., 243.)],
    ),
    (
        Shade::Right,
        [(366., 116.), (256., 179.), (256., 306.), (366., 243.)],
    ),
    (
        Shade::Top,
        [(146., 243.), (256., 306.), (146., 369.), (36., 306.)],
    ),
    (
        Shade::Left,
        [(36., 306.), (146., 369.), (146., 496.), (36., 433.)],
    ),
    (
        Shade::Right,
        [(256., 306.), (146., 369.), (146., 496.), (256., 433.)],
    ),
    (
        Shade::Top,
        [(366., 243.), (476., 306.), (366., 369.), (256., 306.)],
    ),
    (
        Shade::Left,
        [(256., 306.), (366., 369.), (366., 496.), (256., 433.)],
    ),
    (
        Shade::Right,
        [(476., 306.), (366., 369.), (366., 496.), (476., 433.)],
    ),
];
/// Bounds of [`FACES`]: the logo is drawn without the icon's margins.
const MIN: (f32, f32) = (36., 54.);
const SPAN: (f32, f32) = (440., 442.);

#[derive(Clone, Copy)]
enum Shade {
    Top,
    Left,
    Right,
}

impl Shade {
    /// Opacity of the foreground: lit top, darker sides.
    fn opacity(self) -> f32 {
        match self {
            Shade::Top => 1.,
            Shade::Left => 0.62,
            Shade::Right => 0.3,
        }
    }
}

/// The logo in a square of [`Logo::size`], 16 px by default.
#[derive(IntoElement)]
pub struct Logo {
    size: Pixels,
}

impl Logo {
    pub fn new() -> Self {
        Self { size: px(16.) }
    }

    /// Side of the square.
    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.size = size.into();
        self
    }
}

impl Default for Logo {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderOnce for Logo {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = cx.theme().foreground;
        canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let side = bounds.size.width.min(bounds.size.height);
                let scale = f32::from(side) / SPAN.0.max(SPAN.1);
                let origin = bounds.origin
                    + point(
                        (bounds.size.width - px(SPAN.0 * scale)) / 2.,
                        (bounds.size.height - px(SPAN.1 * scale)) / 2.,
                    );
                let at = |(x, y): (f32, f32)| {
                    origin + point(px((x - MIN.0) * scale), px((y - MIN.1) * scale))
                };
                for (shade, corners) in FACES {
                    let mut path = PathBuilder::fill();
                    path.move_to(at(corners[0]));
                    for corner in &corners[1..] {
                        path.line_to(at(*corner));
                    }
                    path.close();
                    if let Ok(path) = path.build() {
                        window.paint_path(path, color.opacity(shade.opacity()));
                    }
                }
            },
        )
        .size(self.size)
        .flex_none()
    }
}
