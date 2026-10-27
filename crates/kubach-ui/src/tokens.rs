//! Layout constants and semantic tones. Colors come from the theme, not from here.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{App, Hsla, Pixels, px};

/// Custom titlebar height; leaves room for the macOS traffic lights.
pub const TITLEBAR_HEIGHT: Pixels = px(40.);
pub const SIDEBAR_WIDTH: Pixels = px(240.);
/// Cluster hotbar left of the sidebar.
pub const HOTBAR_WIDTH: Pixels = px(56.);
/// Dense row height for resource tables and navigation.
pub const ROW_HEIGHT: Pixels = px(32.);
/// Status bar along the bottom of the main window.
pub const STATUS_BAR_HEIGHT: Pixels = px(26.);

/// Background opacity of a tinted badge relative to its tone color.
pub const TINT_OPACITY: f32 = 0.14;

/// Semantic color of a status, mapped onto the active theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Success,
    Warning,
    Danger,
    Info,
}

impl Tone {
    pub fn color(self, cx: &App) -> Hsla {
        let theme = cx.theme();
        match self {
            Tone::Neutral => theme.muted_foreground,
            Tone::Success => theme.success,
            Tone::Warning => theme.warning,
            Tone::Danger => theme.danger,
            Tone::Info => theme.info,
        }
    }
}
