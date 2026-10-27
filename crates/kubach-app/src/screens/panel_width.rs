//! Widths of resizable side panels, remembered per panel in the application
//! state (`state.json`, [`kubach_core::state::AppState::panel_widths`]).

use gpui_kit::*;
use kubach_ui::{SidePanelEvent, SidePanelState};

use crate::state::StateStore;

/// Width of the side panel `key` (`browser.details`): the remembered one or
/// `default`. Widths the user drags are remembered when the drag ends, a
/// double click on the edge forgets them. The owner redraws when the width
/// changes, so a screen embedded cached (#363) follows a reset too.
pub fn remembered<T: 'static>(
    key: &'static str,
    default: Pixels,
    cx: &mut Context<T>,
) -> Entity<SidePanelState> {
    let saved = StateStore::try_global(cx)
        .and_then(|store| store.read(cx).get().panel_width(key))
        .map(|width| px(width as f32));
    let panel = cx.new(|_| SidePanelState::new(default, saved));
    cx.observe(&panel, |_, _, cx| cx.notify()).detach();
    cx.subscribe(&panel, move |_, _, event: &SidePanelEvent, cx| {
        let Some(store) = StateStore::try_global(cx) else {
            return;
        };
        let width = match *event {
            SidePanelEvent::Resized(width) if width != default => {
                Some(f32::from(width).round() as u32)
            }
            SidePanelEvent::Resized(_) | SidePanelEvent::Reset => None,
        };
        store.update(cx, |store, cx| {
            store.edit(cx, |state| state.remember_panel_width(key, width))
        });
    })
    .detach();
    panel
}
