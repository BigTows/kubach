//! Operations of this session (#382): every [`ActionToast`] registers here,
//! so a long helm install, a drain or a bulk delete stays reachable after
//! its toast is hidden. The status bar shows how many run (its label is
//! translated by the application) and opens an [`OperationsList`] on click.
//!
//! An operation may also [`announce`](Operations::announce) its start and
//! its outcome: a short notice the status bar shows for a few seconds
//! (#459).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, WindowExt as _};
use gpui_kit::*;

use super::action_toast::{ActionToast, ActionToastState, card};
use super::empty_state::EmptyState;
use super::status_bar::StatusBarItem;
use crate::tokens::Tone;
use kubach_i18n::t;

/// Operations kept for the list; older finished ones are dropped.
const KEPT: usize = 20;

/// How long a notice stays in the status bar.
pub const NOTICE_TIMEOUT: Duration = Duration::from_secs(6);

/// How long a failure notice stays: longer, it has more to read.
pub const NOTICE_FAILURE_TIMEOUT: Duration = Duration::from_secs(12);

static NEXT_NOTICE: AtomicU64 = AtomicU64::new(0);

/// A short line about an operation for the status bar: it started
/// ([`Tone::Info`]), succeeded ([`Tone::Success`]) or failed
/// ([`Tone::Danger`]).
#[derive(Clone, Debug, PartialEq)]
pub struct OperationNotice {
    /// Unique per notice: a newer one with the same text still redraws.
    pub key: u64,
    pub text: SharedString,
    pub tone: Tone,
}

impl OperationNotice {
    /// The notice as a status bar item: tinted by its tone, with an icon
    /// for it; opens the operations on click.
    pub fn item(&self, id: impl Into<ElementId>) -> StatusBarItem {
        let icon = match self.tone {
            Tone::Success => IconName::CircleCheck,
            Tone::Danger | Tone::Warning => IconName::CircleX,
            Tone::Info | Tone::Neutral => IconName::Play,
        };
        StatusBarItem::new(id, self.text.clone())
            .icon(icon)
            .tone(self.tone)
            .tooltip(t!("ui-operations-notice-tooltip"))
            .on_click(|_, window, cx| OperationsList::open(window, cx))
    }
}

/// The operations of the session, newest first. Notifies whenever one of
/// them changes; observers that only need the count compare
/// [`running`](Self::running) (a phase changes several times a second).
pub struct Operations {
    list: Vec<(ActionToast, Subscription)>,
    notice: Option<OperationNotice>,
    /// Clears `notice` once it has been up long enough.
    notice_timer: Option<Task<()>>,
}

struct GlobalOperations(Entity<Operations>);

impl Global for GlobalOperations {}

impl Operations {
    /// The session's operations, created on first use.
    pub fn global(cx: &mut App) -> Entity<Self> {
        if let Some(global) = cx.try_global::<GlobalOperations>() {
            return global.0.clone();
        }
        let entity = cx.new(|_| Self {
            list: Vec::new(),
            notice: None,
            notice_timer: None,
        });
        cx.set_global(GlobalOperations(entity.clone()));
        entity
    }

    /// Shows `text` in the status bar for a few seconds, replacing the
    /// notice shown before.
    pub fn announce(text: impl Into<SharedString>, tone: Tone, cx: &mut App) {
        let notice = OperationNotice {
            key: NEXT_NOTICE.fetch_add(1, Ordering::Relaxed),
            text: text.into(),
            tone,
        };
        let timeout = if tone == Tone::Danger {
            NOTICE_FAILURE_TIMEOUT
        } else {
            NOTICE_TIMEOUT
        };
        let key = notice.key;
        Self::global(cx).update(cx, |this, cx| {
            this.notice = Some(notice);
            this.notice_timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(timeout).await;
                this.update(cx, |this, cx| {
                    if this.notice.as_ref().is_some_and(|notice| notice.key == key) {
                        this.notice = None;
                        cx.notify();
                    }
                })
                .ok();
            }));
            cx.notify();
        });
    }

    /// The notice the status bar shows now.
    pub fn notice(&self) -> Option<&OperationNotice> {
        self.notice.as_ref()
    }

    pub(super) fn register(toast: &ActionToast, cx: &mut App) {
        let operations = Self::global(cx);
        let toast = toast.clone();
        operations.update(cx, |this, cx| {
            let subscription = cx.observe(&toast.state, |_, _, cx| cx.notify());
            this.list.insert(0, (toast, subscription));
            // Drop the oldest finished ones; running ones always stay.
            while this.list.len() > KEPT {
                let Some(at) = this
                    .list
                    .iter()
                    .rposition(|(toast, _)| !toast.state.read(cx).running())
                else {
                    break;
                };
                drop(this.list.remove(at));
            }
            cx.notify();
        });
    }

    /// How many operations still run.
    pub fn running(&self, cx: &App) -> usize {
        self.list
            .iter()
            .filter(|(toast, _)| toast.state.read(cx).running())
            .count()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Forgets the finished operations.
    pub fn clear_finished(&mut self, cx: &mut Context<Self>) {
        self.list
            .retain(|(toast, _)| toast.state.read(cx).running());
        cx.notify();
    }

    fn states(&self) -> Vec<Entity<ActionToastState>> {
        self.list
            .iter()
            .map(|(toast, _)| toast.state.clone())
            .collect()
    }
}

/// The operations of the session with their time, phase and output, newest
/// first. Ticks every second while one runs, for the elapsed times.
pub struct OperationsList {
    operations: Entity<Operations>,
    ticker: Option<Task<()>>,
    _subscription: Subscription,
}

/// Width of the sheet the list opens in.
const SHEET_WIDTH: Pixels = px(440.);

impl OperationsList {
    /// Opens the list in a sheet on the right of `window`.
    pub fn open(window: &mut Window, cx: &mut App) {
        let list = cx.new(Self::new);
        window.open_sheet(cx, move |sheet, _, _| {
            sheet
                .title(t!("ui-operations-title"))
                .size(SHEET_WIDTH)
                .child(list.clone())
        });
    }

    pub fn new(cx: &mut Context<Self>) -> Self {
        let operations = Operations::global(cx);
        let subscription = cx.observe(&operations, |this, _, cx| {
            this.track(cx);
            cx.notify();
        });
        let mut this = Self {
            operations,
            ticker: None,
            _subscription: subscription,
        };
        this.track(cx);
        this
    }

    /// Starts the ticks when an operation runs and none tick yet.
    fn track(&mut self, cx: &mut Context<Self>) {
        if self.operations.read(cx).running(cx) == 0 {
            self.ticker = None;
            return;
        }
        if self.ticker.is_some() {
            return;
        }
        self.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let running = this.update(cx, |this, cx| {
                    cx.notify();
                    let running = this.operations.read(cx).running(cx) > 0;
                    if !running {
                        this.ticker = None;
                    }
                    running
                });
                if !running.unwrap_or(false) {
                    break;
                }
            }
        }));
    }
}

impl Render for OperationsList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let operations = self.operations.read(cx);
        if operations.is_empty() {
            return EmptyState::new(IconName::CircleCheck, t!("ui-operations-empty"))
                .description(t!("ui-operations-empty-description"))
                .into_any_element();
        }
        let finished = operations.list.len() > operations.running(cx);
        let registry = self.operations.clone();
        let clear = finished.then(|| {
            div().flex().justify_end().child(
                Button::new("operations-clear")
                    .ghost()
                    .xsmall()
                    .label(t!("ui-operations-clear-finished"))
                    .on_click(move |_, _, cx| {
                        registry.update(cx, |operations, cx| operations.clear_finished(cx))
                    }),
            )
        });
        let theme = cx.theme();
        let cards: Vec<_> = operations
            .states()
            .iter()
            .map(|state| {
                div()
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.group_box)
                    .child(card(state, "operation", None, cx))
            })
            .collect();
        div()
            .id("operations-list")
            .flex()
            .flex_col()
            .gap_2()
            .children(clear)
            .children(cards)
            .into_any_element()
    }
}
