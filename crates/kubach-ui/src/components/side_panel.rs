//! Side panel on the right of a screen whose width the user drags by its
//! left edge (#309).

use std::rc::Rc;

use gpui_kit::base::{
    InteractiveElementExt as _, ResizeHandleRenderer, ResizeHandleState, resize_handle,
};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// Narrowest a side panel gets, by dragging or when the window is small.
pub const SIDE_PANEL_MIN_WIDTH: Pixels = px(280.);

/// Narrowest the content on the left of a side panel gets: give it
/// `.min_w(SIDE_PANEL_CONTENT_MIN)` and the panel shrinks first.
pub const SIDE_PANEL_CONTENT_MIN: Pixels = px(360.);

/// Widest share of the window a side panel may take.
const MAX_SHARE: f32 = 0.7;

/// Distance from the panel's left edge to the middle of the gap before it
/// (`gap_4`), where the handle sits.
const HANDLE_OFFSET: Pixels = px(8.);

/// Width changes the owner may remember.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SidePanelEvent {
    /// A drag ended at this width.
    Resized(Pixels),
    /// A double click went back to the default width.
    Reset,
}

/// Width of a [`SidePanel`]; one per screen, kept across renders.
pub struct SidePanelState {
    width: Pixels,
    default: Pixels,
    dragging: bool,
}

impl EventEmitter<SidePanelEvent> for SidePanelState {}

impl SidePanelState {
    /// A panel `default` wide, or `saved` wide when the user resized it.
    pub fn new(default: Pixels, saved: Option<Pixels>) -> Self {
        Self {
            width: saved.unwrap_or(default).max(SIDE_PANEL_MIN_WIDTH),
            default,
            dragging: false,
        }
    }

    pub fn width(&self) -> Pixels {
        self.width
    }

    pub fn default_width(&self) -> Pixels {
        self.default
    }

    /// Goes back to the default width.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.dragging = false;
        self.width = self.default;
        cx.emit(SidePanelEvent::Reset);
        cx.notify();
    }

    fn drag_to(&mut self, width: Pixels, cx: &mut Context<Self>) {
        self.dragging = true;
        if self.width != width {
            self.width = width;
            cx.notify();
        }
    }

    fn finish_drag(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.dragging) {
            cx.emit(SidePanelEvent::Resized(self.width));
        }
    }
}

/// Width for a drag of the left edge to `x`: the panel's right edge stays,
/// `shown` is the width it has on screen now (the layout gives less than
/// `current` when the content beside it is at its minimum).
fn drag_width(
    right: Pixels,
    x: Pixels,
    current: Pixels,
    shown: Pixels,
    viewport: Pixels,
) -> Pixels {
    let max = (viewport * MAX_SHARE).max(SIDE_PANEL_MIN_WIDTH);
    let target = (right - x).clamp(SIDE_PANEL_MIN_WIDTH, max);
    // Already capped by the layout: growing further would only add a dead
    // zone to drag back through.
    if target > shown && current > shown + px(1.) {
        shown.max(SIDE_PANEL_MIN_WIDTH)
    } else {
        target
    }
}

/// Payload of the edge drag; carries the panel so two panels never move
/// each other.
#[derive(Clone)]
struct EdgeDrag(EntityId);

impl Render for EdgeDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// A right-hand panel `state.width()` wide with a drag handle on its left
/// edge: hovering it shows the resize cursor, dragging changes the width,
/// a double click restores the default. The children fill the panel.
///
/// The panel shrinks below its width (down to [`SIDE_PANEL_MIN_WIDTH`]) when
/// the content beside it has `min_w(SIDE_PANEL_CONTENT_MIN)` and no room.
#[derive(IntoElement)]
pub struct SidePanel {
    id: SharedString,
    state: Entity<SidePanelState>,
    children: Vec<AnyElement>,
}

impl SidePanel {
    pub fn new(id: impl Into<SharedString>, state: &Entity<SidePanelState>) -> Self {
        Self {
            id: id.into(),
            state: state.clone(),
            children: Vec::new(),
        }
    }
}

impl ParentElement for SidePanel {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for SidePanel {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let width = self.state.read(cx).width();
        let entity = self.state.entity_id();
        let handle_id = ElementId::Name(format!("{}-handle", self.id).into());
        let appearance: ResizeHandleRenderer = Rc::new(|handle, _, cx| {
            let theme = cx.theme();
            let color = match handle.state() {
                ResizeHandleState::Idle => None,
                ResizeHandleState::Hovered => Some(theme.border),
                ResizeHandleState::Pressed | ResizeHandleState::Dragging => Some(theme.drag_border),
            };
            Some(
                div()
                    .flex_none()
                    .h_full()
                    .w(px(2.))
                    .when_some(color, |this, color| this.bg(color))
                    .into_any_element(),
            )
        });
        let finish = {
            let state = self.state.clone();
            move |_: &MouseUpEvent, _: &mut Window, cx: &mut App| {
                state.update(cx, |state, cx| state.finish_drag(cx));
            }
        };
        div()
            .id(ElementId::Name(self.id))
            .relative()
            .h_full()
            .min_h_0()
            .w(width)
            .min_w(SIDE_PANEL_MIN_WIDTH)
            .flex_shrink(1.)
            .on_drag_move({
                let state = self.state.clone();
                move |event: &DragMoveEvent<EdgeDrag>, window, cx| {
                    if event.drag(cx).0 != entity {
                        return;
                    }
                    let bounds = event.bounds;
                    let viewport = window.viewport_size().width;
                    state.update(cx, |state, cx| {
                        let width = drag_width(
                            bounds.right(),
                            event.event.position.x,
                            state.width,
                            bounds.size.width,
                            viewport,
                        );
                        state.drag_to(width, cx);
                    });
                }
            })
            .on_mouse_up(MouseButton::Left, finish.clone())
            .on_mouse_up_out(MouseButton::Left, finish)
            // The content is laid out at the panel's size, never measured
            // for it: as a shrinkable flex item the panel would otherwise
            // have its whole subtree sized several times over on every
            // frame (#355, ~45 ms for a pod's details).
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .flex()
                    .flex_col()
                    .children(self.children),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(-HANDLE_OFFSET)
                    .w(px(1.))
                    .child(
                        resize_handle(handle_id, Axis::Horizontal)
                            .with_appearance(appearance)
                            .on_drag(EdgeDrag(entity), |drag, _, _, cx| cx.new(|_| drag.clone()))
                            .on_double_click({
                                let state = self.state.clone();
                                move |_, _, cx| state.update(cx, |state, cx| state.reset(cx))
                            }),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::{Pixels, px};

    use super::{SIDE_PANEL_MIN_WIDTH, drag_width};

    const VIEWPORT: Pixels = px(1600.);

    #[test]
    fn dragging_the_left_edge_keeps_the_right_one() {
        // Right edge at 1500: the edge dragged to 1000 makes it 500 wide.
        assert_eq!(
            drag_width(px(1500.), px(1000.), px(380.), px(380.), VIEWPORT),
            px(500.)
        );
        assert_eq!(
            drag_width(px(1500.), px(1200.), px(500.), px(500.), VIEWPORT),
            px(300.)
        );
    }

    #[test]
    fn width_stays_within_the_limits() {
        assert_eq!(
            drag_width(px(1500.), px(1400.), px(380.), px(380.), VIEWPORT),
            SIDE_PANEL_MIN_WIDTH
        );
        // At most 70% of the window.
        assert_eq!(
            drag_width(px(1500.), px(0.), px(380.), px(380.), VIEWPORT),
            px(1120.)
        );
    }

    #[test]
    fn a_width_capped_by_the_layout_does_not_grow() {
        // The content beside is at its minimum and the panel got 600 of 700.
        assert_eq!(
            drag_width(px(1500.), px(800.), px(700.), px(600.), VIEWPORT),
            px(600.)
        );
        // Narrowing works at once.
        assert_eq!(
            drag_width(px(1500.), px(1000.), px(700.), px(600.), VIEWPORT),
            px(500.)
        );
    }
}
