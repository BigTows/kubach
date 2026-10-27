use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_i18n::t;

/// Failures listed before "and N more".
const FAILURES_SHOWN: usize = 20;

type DismissHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// Progress of an operation over many items, then its summary: how many
/// succeeded and which failed and why.
///
/// `id` scopes the inner buttons and bar: two instances side by side need
/// different ids, or their accessibility nodes clash.
#[derive(IntoElement)]
pub struct BulkProgress {
    id: ElementId,
    title: SharedString,
    total: usize,
    done: usize,
    /// Set once every item is done.
    result: Option<BulkResult>,
    /// Secondary text under the header, e.g. the command or its output.
    detail: Option<SharedString>,
    /// Drawn without its own frame, inside a toast.
    bare: bool,
    on_dismiss: Option<DismissHandler>,
    /// Shows a Cancel button while running.
    on_cancel: Option<DismissHandler>,
    /// Time since the start (running) or how long it took (finished).
    elapsed: Option<SharedString>,
    /// What it is doing right now, under the header while running.
    phase: Option<SharedString>,
    /// Stopped by the user: a failed result reads "Cancelled".
    cancelled: bool,
    /// Shows a button hiding the view while running (the operation goes on).
    on_hide: Option<DismissHandler>,
    /// Lines of output under a toggle, unfolded or not.
    log: Option<OutputLog>,
}

/// Output lines of a [`BulkProgress`] under a "Show output" toggle.
struct OutputLog {
    lines: Vec<SharedString>,
    open: bool,
    on_toggle: DismissHandler,
}

/// Height above which the output scrolls.
const LOG_MAX_HEIGHT: Pixels = px(180.);

/// Outcome shown by a finished [`BulkProgress`].
#[derive(Clone, Debug, Default)]
pub struct BulkResult {
    pub succeeded: usize,
    /// `(item, reason)`.
    pub failures: Vec<(SharedString, SharedString)>,
    /// Extra advice under the failures, e.g. how to get a permission.
    pub hint: Option<SharedString>,
}

impl BulkProgress {
    /// `title` like "Deleting 12 Pods"; `done` of `total` items finished.
    pub fn new(
        id: impl Into<ElementId>,
        title: impl Into<SharedString>,
        done: usize,
        total: usize,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            total,
            done,
            result: None,
            detail: None,
            bare: false,
            on_dismiss: None,
            on_cancel: None,
            elapsed: None,
            phase: None,
            cancelled: false,
            on_hide: None,
            log: None,
        }
    }

    /// Time since the start while running, the duration once finished
    /// (`12s`, `2m5s`), after the status.
    pub fn elapsed(mut self, elapsed: impl Into<SharedString>) -> Self {
        self.elapsed = Some(elapsed.into());
        self
    }

    /// The current phase while running, e.g. "Waiting for Job web-check".
    pub fn phase(mut self, phase: impl Into<SharedString>) -> Self {
        let phase = phase.into();
        self.phase = (!phase.is_empty()).then_some(phase);
        self
    }

    /// Marks a failed result as cancelled by the user.
    pub fn cancelled(mut self, cancelled: bool) -> Self {
        self.cancelled = cancelled;
        self
    }

    /// Shows a button that hides the view while the operation runs on.
    pub fn on_hide(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_hide = Some(Rc::new(handler));
        self
    }

    /// Output lines under a toggle; `open` unfolds them, `on_toggle` flips it.
    pub fn log(
        mut self,
        lines: Vec<SharedString>,
        open: bool,
        on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        if !lines.is_empty() {
            self.log = Some(OutputLog {
                lines,
                open,
                on_toggle: Rc::new(on_toggle),
            });
        }
        self
    }

    /// Secondary text under the header (monospace), e.g. the command while
    /// it runs and its output once done.
    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        let detail = detail.into();
        self.detail = (!detail.is_empty()).then_some(detail);
        self
    }

    /// Drops the frame, for use inside a container that draws its own
    /// (e.g. [`crate::ActionToast`]).
    pub fn bare(mut self) -> Self {
        self.bare = true;
        self
    }

    /// Marks the operation finished with `result`.
    pub fn finished(mut self, result: BulkResult) -> Self {
        self.result = Some(result);
        self
    }

    /// Shows a Cancel button while the operation runs.
    pub fn on_cancel(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_cancel = Some(Rc::new(handler));
        self
    }

    /// Shows a close button once finished.
    pub fn on_dismiss(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_dismiss = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for BulkProgress {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let single = self.total <= 1;
        let (icon, color, status) = match &self.result {
            None => (
                IconName::LoaderCircle,
                theme.muted_foreground,
                if single {
                    t!("ui-bulk-running")
                } else {
                    format!("{} / {}", self.done, self.total)
                },
            ),
            Some(result) if result.failures.is_empty() => (
                IconName::CircleCheck,
                theme.success,
                if single {
                    t!("ui-bulk-done")
                } else {
                    t!("ui-bulk-succeeded", succeeded = result.succeeded)
                },
            ),
            Some(_) if self.cancelled => {
                (IconName::CircleX, theme.warning, t!("ui-bulk-cancelled"))
            }
            Some(result) => (
                IconName::TriangleAlert,
                theme.danger,
                if single {
                    t!("ui-bulk-failed")
                } else {
                    t!(
                        "ui-bulk-succeeded-failed",
                        succeeded = result.succeeded,
                        failed = result.failures.len()
                    )
                },
            ),
        };
        let status = match &self.elapsed {
            Some(elapsed) => format!("{status} · {elapsed}"),
            None => status,
        };
        let fraction = if self.total == 0 {
            1.
        } else {
            self.done as f32 / self.total as f32
        };
        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .text_sm()
            .child(if self.result.is_none() {
                // Still running: a static loader reads as stuck.
                crate::spinner()
                    .icon(Icon::new(icon))
                    .with_size(px(16.))
                    .color(color)
                    .into_any_element()
            } else {
                Icon::new(icon)
                    .size_4()
                    .text_color(color)
                    .into_any_element()
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child(self.title),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(status),
            )
            .when_some(
                self.on_hide.filter(|_| self.result.is_none()),
                |header, handler| {
                    header.child(
                        Button::new("bulk-progress-hide")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Minus)
                            .tooltip(t!("ui-bulk-hide-tooltip"))
                            .on_click(move |event, window, cx| handler(event, window, cx)),
                    )
                },
            )
            .when_some(
                self.on_cancel.filter(|_| self.result.is_none()),
                |header, handler| {
                    header.child(
                        Button::new("bulk-progress-cancel")
                            .ghost()
                            .xsmall()
                            .label(t!("ui-cancel"))
                            .on_click(move |event, window, cx| handler(event, window, cx)),
                    )
                },
            )
            .when_some(
                self.on_dismiss.filter(|_| self.result.is_some()),
                |header, handler| {
                    header.child(
                        Button::new("bulk-progress-dismiss")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .on_click(move |event, window, cx| handler(event, window, cx)),
                    )
                },
            );
        let details = self.result.as_ref().map(|result| {
            let shown = result.failures.len().min(FAILURES_SHOWN);
            let more = result.failures.len() - shown;
            div()
                .flex()
                .flex_col()
                .gap_1()
                .text_xs()
                // The item above its reason: side by side, a long name
                // squeezed the reason into a column of letters (#316).
                .children(result.failures[..shown].iter().map(|(item, reason)| {
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .when(!item.is_empty(), |row| {
                            row.child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_color(theme.foreground)
                                    .child(item.clone()),
                            )
                        })
                        .child(
                            div()
                                .min_w_0()
                                .whitespace_normal()
                                .text_color(theme.danger)
                                .child(reason.clone()),
                        )
                }))
                .when(more > 0, |list| {
                    list.child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child(t!("ui-and-more", count = more)),
                    )
                })
                .when_some(result.hint.clone(), |list, hint| {
                    list.child(
                        div()
                            .pt_1()
                            .whitespace_normal()
                            .text_color(theme.muted_foreground)
                            .child(hint),
                    )
                })
        });
        let detail = self.detail.map(|detail| {
            div()
                .text_xs()
                .whitespace_normal()
                .font_family(theme.mono_font_family.clone())
                .text_color(theme.muted_foreground)
                .child(detail)
        });
        let phase = self.phase.filter(|_| self.result.is_none()).map(|phase| {
            div()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(theme.foreground)
                .child(phase)
        });
        let log = self.log.map(|log| {
            let count = log.lines.len();
            let on_toggle = log.on_toggle.clone();
            let toggle = Button::new("bulk-progress-output")
                .ghost()
                .xsmall()
                .icon(if log.open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .label(if log.open {
                    t!("ui-bulk-hide-output")
                } else {
                    t!("ui-bulk-show-output", count = count)
                })
                .on_click(move |event, window, cx| on_toggle(event, window, cx));
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().flex().child(toggle))
                .when(log.open, |this| {
                    this.child(
                        div()
                            .id("bulk-progress-output-lines")
                            .max_h(LOG_MAX_HEIGHT)
                            .overflow_y_scroll()
                            .p_2()
                            .rounded(theme.radius)
                            .bg(theme.muted.opacity(0.4))
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .children(
                                log.lines
                                    .into_iter()
                                    .map(|line| div().whitespace_normal().child(line)),
                            ),
                    )
                })
        });
        div()
            .id(self.id)
            .flex()
            .flex_col()
            .gap_2()
            .when(!self.bare, |this| {
                this.px_3()
                    .py_2()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.group_box)
            })
            .child(header)
            .children(phase)
            .children(detail)
            .when(self.result.is_none() && !single, |this| {
                this.child(Progress::new("bulk-progress").value(fraction * 100.))
            })
            .children(details)
            .children(log)
    }
}
