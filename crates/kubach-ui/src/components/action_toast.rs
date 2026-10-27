//! Progress and outcome of an action as a toast over the window, so that
//! nothing in the layout moves while it runs or when it goes away.
//!
//! Built on the gpui-kit notification stack: toasts of several actions stack
//! in the corner and expand on hover instead of covering each other.
//!
//! Every toast is also an operation in [`Operations`]: hiding the toast
//! keeps it in the status bar's list, with the time it has run, its phase
//! and its output (#382).

use std::cell::Cell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::*;

use super::bulk_progress::{BulkProgress, BulkResult};
use super::confirm_dialog::ConfirmDialog;
use super::operations::Operations;
use crate::age::format_age;
use kubach_i18n::t;

/// How long a successful outcome stays up.
pub const ACTION_TOAST_SUCCESS_TIMEOUT: Duration = Duration::from_secs(4);

/// Height above which the toast body scrolls.
const BODY_MAX_HEIGHT: Pixels = px(360.);

/// Output lines kept per operation; older ones are dropped.
const LOG_LINES: usize = 200;

static NEXT_KEY: AtomicU64 = AtomicU64::new(0);

type CancelHandler = Rc<dyn Fn(&mut App)>;

/// Handle to a shown action toast. Updates re-render it in place; a
/// success hides it after [`ACTION_TOAST_SUCCESS_TIMEOUT`], a failure stays
/// until closed.
#[derive(Clone)]
pub struct ActionToast {
    pub(super) state: Entity<ActionToastState>,
}

/// Asked before a Cancel goes through: what stopping leaves behind.
#[derive(Clone)]
struct CancelConfirm {
    title: SharedString,
    message: SharedString,
}

/// What the toast shows; see [`BulkProgress`].
pub(super) struct ActionToastState {
    /// Unique per toast: scopes element ids.
    pub(super) key: u64,
    title: SharedString,
    done: usize,
    total: usize,
    detail: Option<SharedString>,
    result: Option<BulkResult>,
    /// Stops the action; the Cancel button shows while it is set and the
    /// action runs.
    cancel: Option<CancelHandler>,
    /// Shown before `cancel` runs.
    cancel_confirm: Option<CancelConfirm>,
    /// Cancel was pressed (and confirmed).
    cancelled: bool,
    started: Instant,
    ended: Option<Instant>,
    phase: Option<SharedString>,
    log: VecDeque<SharedString>,
    log_open: bool,
}

impl ActionToastState {
    fn succeeded(&self) -> bool {
        self.result
            .as_ref()
            .is_some_and(|result| result.failures.is_empty())
    }

    pub(super) fn running(&self) -> bool {
        self.result.is_none()
    }

    /// Time since the start, or how long it took.
    fn elapsed(&self) -> Duration {
        self.ended
            .unwrap_or_else(Instant::now)
            .saturating_duration_since(self.started)
    }
}

impl ActionToast {
    /// Shows a toast for an action over `total` items in `window`.
    pub fn show(
        title: impl Into<SharedString>,
        total: usize,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let toast = Self::new(title, total, cx);
        window.push_notification(toast.notification(), cx);
        toast
    }

    /// Like [`Self::show`], for callers without a window at hand: shows the
    /// toast in the active window once the current update is over.
    pub fn show_in_active_window(
        title: impl Into<SharedString>,
        total: usize,
        cx: &mut App,
    ) -> Self {
        let toast = Self::new(title, total, cx);
        let notification = toast.notification();
        cx.defer(move |cx| {
            let Some(handle) = cx
                .active_window()
                .or_else(|| cx.windows().into_iter().next())
            else {
                return;
            };
            handle
                .update(cx, |_, window, cx| {
                    window.push_notification(notification, cx)
                })
                .ok();
        });
        toast
    }

    fn new(title: impl Into<SharedString>, total: usize, cx: &mut App) -> Self {
        let state = cx.new(|_| ActionToastState {
            key: NEXT_KEY.fetch_add(1, Ordering::Relaxed),
            title: title.into(),
            done: 0,
            total,
            detail: None,
            result: None,
            cancel: None,
            cancel_confirm: None,
            cancelled: false,
            started: Instant::now(),
            ended: None,
            phase: None,
            log: VecDeque::new(),
            log_open: false,
        });
        let toast = Self { state };
        Operations::register(&toast, cx);
        toast
    }

    /// The notification rendering this toast's state.
    fn notification(&self) -> Notification {
        let state = self.state.clone();
        let key = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
        let observing = Rc::new(Cell::new(false));
        let closing = Rc::new(Cell::new(false));
        Notification::new()
            .id1::<ActionToast>(ElementId::Integer(key))
            .autohide(false)
            .content(move |_, window, cx| {
                if !observing.replace(true) {
                    let closing = closing.clone();
                    cx.observe_in(&state, window, move |_, state, window, cx| {
                        cx.notify();
                        if state.read(cx).succeeded() {
                            close_later(&closing, window, cx);
                        }
                    })
                    .detach();
                    tick(&state, window, cx);
                }
                // Finished before the first frame.
                if state.read(cx).succeeded() {
                    close_later(&closing, window, cx);
                }
                let hide = cx.listener(|note, _, window, cx| note.dismiss(window, cx));
                div()
                    .id(("action-toast-body", key))
                    .max_h(BODY_MAX_HEIGHT)
                    .overflow_y_scroll()
                    // Room for the close button the notification shows on hover.
                    .pr_4()
                    .child(card(
                        &state,
                        "action-toast-progress",
                        Some(Rc::new(hide)),
                        cx,
                    ))
                    .into_any_element()
            })
    }

    /// Items finished so far.
    pub fn set_done(&self, done: usize, cx: &mut App) {
        self.state.update(cx, |state, cx| {
            state.done = done;
            cx.notify();
        });
    }

    /// Changes the number of items, once the action knows it (a drain
    /// learns its pods after cordoning).
    pub fn set_total(&self, total: usize, cx: &mut App) {
        self.state.update(cx, |state, cx| {
            state.total = total;
            cx.notify();
        });
    }

    /// Shows a Cancel button running `cancel` (once) while the action runs.
    pub fn set_cancel(&self, cancel: impl Fn(&mut App) + 'static, cx: &mut App) {
        self.state.update(cx, |state, cx| {
            state.cancel = Some(Rc::new(cancel));
            state.cancel_confirm = None;
            cx.notify();
        });
    }

    /// Like [`Self::set_cancel`], asking first with `title` and `message`
    /// (what stopping halfway leaves behind).
    pub fn set_cancel_confirmed(
        &self,
        title: impl Into<SharedString>,
        message: impl Into<SharedString>,
        cancel: impl Fn(&mut App) + 'static,
        cx: &mut App,
    ) {
        let confirm = CancelConfirm {
            title: title.into(),
            message: message.into(),
        };
        self.state.update(cx, |state, cx| {
            state.cancel = Some(Rc::new(cancel));
            state.cancel_confirm = Some(confirm);
            cx.notify();
        });
    }

    /// Secondary text under the title (monospace): a command, its output.
    pub fn set_detail(&self, detail: impl Into<SharedString>, cx: &mut App) {
        let detail = detail.into();
        self.state.update(cx, |state, cx| {
            state.detail = (!detail.is_empty()).then_some(detail);
            cx.notify();
        });
    }

    /// What the action is doing now ("Waiting for Job web-check", "3 pods
    /// left"); shown under the title while it runs.
    pub fn set_phase(&self, phase: impl Into<SharedString>, cx: &mut App) {
        let phase = phase.into();
        self.state.update(cx, |state, cx| {
            state.phase = (!phase.is_empty()).then_some(phase);
            cx.notify();
        });
    }

    /// Appends lines to the output (under "Show output"); the last one
    /// becomes the phase.
    pub fn push_output(&self, lines: impl IntoIterator<Item = SharedString>, cx: &mut App) {
        self.state.update(cx, |state, cx| {
            for line in lines {
                state.phase = Some(line.clone());
                state.log.push_back(line);
            }
            while state.log.len() > LOG_LINES {
                state.log.pop_front();
            }
            cx.notify();
        });
    }

    /// Marks the action finished.
    pub fn finish(&self, result: BulkResult, cx: &mut App) {
        self.state.update(cx, |state, cx| {
            state.done = state.total;
            state.result = Some(result);
            state.ended = Some(Instant::now());
            state.cancel = None;
            cx.notify();
        });
    }

    /// Whether the action still runs.
    pub fn running(&self, cx: &App) -> bool {
        self.state.read(cx).running()
    }
}

/// Re-renders the notification every second while the action runs, for
/// its elapsed time; nothing else redraws for it.
fn tick(state: &Entity<ActionToastState>, window: &mut Window, cx: &mut Context<Notification>) {
    let state = state.clone();
    cx.spawn_in(window, async move |note, cx| {
        loop {
            let delay = next_second(&state, cx);
            cx.background_executor().timer(delay).await;
            let running = cx.update(|_, cx| state.read(cx).running()).unwrap_or(false);
            if note.update(cx, |_, cx| cx.notify()).is_err() || !running {
                break;
            }
        }
    })
    .detach();
}

/// Time until the elapsed seconds of `state` turn over.
fn next_second(state: &Entity<ActionToastState>, cx: &mut AsyncWindowContext) -> Duration {
    let elapsed = cx
        .update(|_, cx| state.read(cx).elapsed())
        .unwrap_or_default();
    Duration::from_millis(1000 - u64::from(elapsed.subsec_millis()) + 5)
}

/// Starts the countdown that closes a successful toast, once.
fn close_later(closing: &Cell<bool>, window: &mut Window, cx: &mut Context<Notification>) {
    if closing.replace(true) {
        return;
    }
    cx.spawn_in(window, async move |note, cx| {
        cx.background_executor()
            .timer(ACTION_TOAST_SUCCESS_TIMEOUT)
            .await;
        note.update_in(cx, |note, window, cx| note.dismiss(window, cx))
            .ok();
    })
    .detach();
}

type HideHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// The operation as a [`BulkProgress`]: in the toast (`hide` closes it)
/// and in the status bar's list. `id` must differ between the two.
pub(super) fn card(
    entity: &Entity<ActionToastState>,
    id: &'static str,
    hide: Option<HideHandler>,
    cx: &App,
) -> BulkProgress {
    let state = entity.read(cx);
    let mut progress = BulkProgress::new(
        (id, state.key),
        state.title.clone(),
        state.done,
        state.total,
    )
    .bare()
    .elapsed(format_age(state.elapsed().as_secs()))
    .cancelled(state.cancelled);
    if let Some(phase) = &state.phase {
        progress = progress.phase(phase.clone());
    }
    if let Some(detail) = &state.detail {
        progress = progress.detail(detail.clone());
    }
    let toggle = entity.clone();
    progress = progress.log(
        state.log.iter().cloned().collect(),
        state.log_open,
        move |_, _, cx| {
            toggle.update(cx, |state, cx| {
                state.log_open = !state.log_open;
                cx.notify();
            })
        },
    );
    if let Some(result) = &state.result {
        return progress.finished(result.clone());
    }
    if let Some(hide) = hide {
        progress = progress.on_hide(move |event, window, cx| hide(event, window, cx));
    }
    if state.cancel.is_some() {
        let entity = entity.clone();
        progress = progress.on_cancel(move |_, window, cx| request_cancel(&entity, window, cx));
    }
    progress
}

/// Runs the cancel handler, after a confirmation when one is set.
fn request_cancel(entity: &Entity<ActionToastState>, window: &mut Window, cx: &mut App) {
    let Some(confirm) = entity.read(cx).cancel_confirm.clone() else {
        cancel_now(entity, cx);
        return;
    };
    let entity = entity.clone();
    ConfirmDialog::open(window, cx, move |_, _| {
        let entity = entity.clone();
        ConfirmDialog::new(confirm.title.clone())
            .message(confirm.message.clone())
            .confirm_label(t!("ui-action-stop"))
            .danger()
            .on_confirm(move |_, cx| cancel_now(&entity, cx))
    });
}

fn cancel_now(entity: &Entity<ActionToastState>, cx: &mut App) {
    let cancel = entity.update(cx, |state, cx| {
        if state.result.is_some() {
            return None;
        }
        cx.notify();
        let cancel = state.cancel.take();
        state.cancelled |= cancel.is_some();
        if cancel.is_some() {
            state.phase = Some(t!("ui-action-stopping").into());
        }
        cancel
    });
    if let Some(cancel) = cancel {
        cancel(cx);
    }
}
