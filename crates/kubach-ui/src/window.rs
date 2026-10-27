use gpui_kit::*;

use crate::theme;

/// Options for a main window: a transparent titlebar that the app draws
/// itself ([`crate::WindowTitlebar`]) and a blurred translucent background
/// where the platform blurs ([`theme::BLURS_BEHIND`]), opaque otherwise.
///
/// Linux decorations stay GPUI's default: server-side where the compositor
/// offers them, client-side otherwise (GNOME on Wayland). With client-side
/// decorations the titlebar moves the window and draws its buttons, and the
/// gpui-kit window root adds the resize border and shadow.
pub fn main_window_options(title: impl Into<SharedString>, cx: &App) -> WindowOptions {
    let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(800.), px(500.))),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(14.), px(13.))),
        }),
        window_background: if theme::BLURS_BEHIND {
            WindowBackgroundAppearance::Blurred
        } else {
            WindowBackgroundAppearance::Opaque
        },
        app_id: Some("dev.kubach.app".into()),
        ..Default::default()
    }
}
