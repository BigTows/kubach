//! Window titlebar that also works as the window frame under client-side
//! decorations (Linux without a server frame, e.g. GNOME on Wayland).
//!
//! The resize border and shadow of such windows come from the gpui-kit window
//! root (`gpui_kit::open_window`), which wraps every window in
//! gpui-component's `window_border`; the titlebar adds what only it can:
//! moving, double-click maximize, the window menu and the window buttons.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{IconName, Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// Whether the app draws this window's frame itself: GPUI gave it client-side
/// decorations. Always `false` on macOS and Windows, which keep the system
/// frame (traffic lights, caption buttons).
pub fn is_client_decorated(window: &Window) -> bool {
    cfg!(target_os = "linux") && matches!(window.window_decorations(), Decorations::Client { .. })
}

/// A window's titlebar. Styled and filled by the caller; under client-side
/// decorations it drags the window, maximizes or restores it on double
/// click, opens the window menu on right click and shows [`WindowControls`]
/// on the right. Elsewhere it is a plain container.
#[derive(IntoElement)]
pub struct WindowTitlebar {
    id: SharedString,
    style: StyleRefinement,
    children: Vec<AnyElement>,
}

impl WindowTitlebar {
    pub fn new(id: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            style: StyleRefinement::default(),
            children: Vec::new(),
        }
    }
}

impl Styled for WindowTitlebar {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for WindowTitlebar {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

/// Pressed on the titlebar and not yet moved: the next move starts a window
/// move. Deferring the move to the first drag keeps double clicks reaching
/// the titlebar, which a compositor grab on mouse down would swallow.
#[derive(Default)]
struct DragState {
    pressed: bool,
}

fn set_pressed(drag: &Entity<DragState>, pressed: bool, cx: &mut App) {
    drag.update(cx, |drag, _| drag.pressed = pressed);
}

impl RenderOnce for WindowTitlebar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let mut bar = div().id(self.id.clone()).relative();
        bar.style().refine(&self.style);
        let bar = bar.children(self.children);
        if !is_client_decorated(window) {
            return bar;
        }

        let key = ElementId::Name(format!("{}-drag", self.id).into());
        let drag = window.use_keyed_state(key, cx, |_, _| DragState::default());
        let window_menu = window.window_controls().window_menu;
        bar.on_mouse_down(MouseButton::Left, {
            let drag = drag.clone();
            move |_, _, cx| set_pressed(&drag, true, cx)
        })
        .on_mouse_up(MouseButton::Left, {
            let drag = drag.clone();
            move |_, _, cx| set_pressed(&drag, false, cx)
        })
        .on_mouse_down_out({
            let drag = drag.clone();
            move |_, _, cx| set_pressed(&drag, false, cx)
        })
        .on_mouse_move(move |_, window, cx| {
            let pressed = drag.update(cx, |drag, _| std::mem::take(&mut drag.pressed));
            if pressed {
                window.start_window_move();
            }
        })
        .on_click(|event, window, _| {
            if event.click_count() == 2 {
                window.zoom_window();
            }
        })
        .when(window_menu, |bar| {
            bar.on_mouse_down(MouseButton::Right, |event, window, _| {
                window.show_window_menu(event.position)
            })
        })
        .child(
            div()
                .absolute()
                .top_0()
                .right_0()
                .h_full()
                .child(WindowControls::new()),
        )
    }
}

/// Minimize, maximize/restore and close buttons for client-side decorated
/// windows, limited to what the window manager supports. Renders nothing
/// otherwise unless [`WindowControls::always`] is set.
#[derive(IntoElement, Default)]
pub struct WindowControls {
    always: bool,
}

impl WindowControls {
    pub fn new() -> Self {
        Self::default()
    }

    /// Shows the buttons under any decorations (the gallery on macOS).
    pub fn always(mut self) -> Self {
        self.always = true;
        self
    }
}

fn control(id: &'static str, icon: IconName, action: fn(&mut Window)) -> Button {
    Button::new(id)
        .ghost()
        .xsmall()
        .icon(icon)
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            action(window);
        })
}

impl RenderOnce for WindowControls {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        let visible = self.always || is_client_decorated(window);
        let supported = window.window_controls();
        let (maximize_icon, maximize_id) = if window.is_maximized() {
            (IconName::WindowRestore, "window-restore")
        } else {
            (IconName::WindowMaximize, "window-maximize")
        };
        h_flex()
            .h_full()
            .items_center()
            .gap_1()
            .when(visible, |row| {
                row.px_2()
                    // Presses on the buttons must not start a titlebar drag.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .when(supported.minimize, |row| {
                        row.child(control(
                            "window-minimize",
                            IconName::WindowMinimize,
                            |window| window.minimize_window(),
                        ))
                    })
                    .when(supported.maximize, |row| {
                        row.child(control(maximize_id, maximize_icon, |window| {
                            window.zoom_window()
                        }))
                    })
                    .child(control("window-close", IconName::WindowClose, |window| {
                        window.remove_window()
                    }))
            })
    }
}
