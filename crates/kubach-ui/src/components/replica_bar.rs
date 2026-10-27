//! Mini bar of a workload's replicas: one cell per pod, filled for ready
//! pods, tinted for pods that exist but are not ready, hollow for pods
//! still missing.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;
use kubach_i18n::t;

/// Up to this many pods get a cell each; more are drawn as proportions.
const MAX_CELLS: u32 = 24;

/// Replicas as a bar and a `ready/desired` label.
#[derive(IntoElement, Clone, Copy, Debug)]
pub struct ReplicaBar {
    desired: u32,
    current: u32,
    ready: u32,
}

/// What each cell of a [`ReplicaBar`] shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplicaCell {
    Ready,
    NotReady,
    Missing,
}

impl ReplicaBar {
    pub fn new(desired: u32, current: u32, ready: u32) -> Self {
        Self {
            desired,
            current,
            ready: ready.min(current.max(ready)),
        }
    }

    /// Cells in order: ready, then not ready, then missing. Surge pods
    /// (more than desired) get cells too.
    pub fn cells(&self) -> Vec<ReplicaCell> {
        let total = self.desired.max(self.current).max(self.ready);
        (0..total)
            .map(|ix| {
                if ix < self.ready {
                    ReplicaCell::Ready
                } else if ix < self.current {
                    ReplicaCell::NotReady
                } else {
                    ReplicaCell::Missing
                }
            })
            .collect()
    }
}

impl RenderOnce for ReplicaBar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let cells = self.cells();
        let color = |cell: ReplicaCell| match cell {
            ReplicaCell::Ready => theme.success,
            ReplicaCell::NotReady => theme.warning,
            ReplicaCell::Missing => theme.muted,
        };
        let bar = if cells.len() as u32 <= MAX_CELLS {
            div()
                .flex()
                .gap(px(2.))
                .children(cells.into_iter().map(|cell| {
                    div()
                        .w(px(10.))
                        .h(px(8.))
                        .rounded_sm()
                        .bg(color(cell))
                        .border_1()
                        .border_color(match cell {
                            ReplicaCell::Missing => theme.border,
                            other => color(other),
                        })
                }))
                .into_any_element()
        } else {
            let total = cells.len() as f32;
            let share = |cell| cells.iter().filter(|c| **c == cell).count() as f32 / total;
            div()
                .w(px(160.))
                .h(px(8.))
                .flex()
                .rounded_sm()
                .overflow_hidden()
                .bg(theme.muted)
                .child(
                    div()
                        .h_full()
                        .w(relative(share(ReplicaCell::Ready)))
                        .bg(theme.success),
                )
                .child(
                    div()
                        .h_full()
                        .w(relative(share(ReplicaCell::NotReady)))
                        .bg(theme.warning),
                )
                .into_any_element()
        };
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(bar)
            .child(div().text_xs().text_color(theme.muted_foreground).child(t!(
                "ui-replicas-ready",
                ready = self.ready,
                desired = self.desired
            )))
    }
}

#[cfg(test)]
mod tests {
    use super::{ReplicaBar, ReplicaCell};

    #[test]
    fn cells_cover_surge_and_missing_pods() {
        use ReplicaCell::*;
        assert_eq!(ReplicaBar::new(3, 2, 1).cells(), [Ready, NotReady, Missing]);
        assert_eq!(ReplicaBar::new(2, 3, 2).cells(), [Ready, Ready, NotReady]);
        assert!(ReplicaBar::new(0, 0, 0).cells().is_empty());
    }
}
