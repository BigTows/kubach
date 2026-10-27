//! The "AI agent" page (#86): the MCP HTTP server, how to connect an
//! agent, the session for HTTP agents, active sessions with the rights
//! their agents were granted (#227) and the audit log.
//!
//! The HTTP session is one button (#401): it starts without namespaces,
//! and its ceiling (profile) and length come from the context's security
//! settings, so there is nothing else to pick here.

use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::settings::{DEFAULT_MCP_PORT, Settings};
use kubach_i18n::t;
use kubach_mcp::{BoundaryInfo, Grant, GrantState, Profile, SessionMode};
use kubach_ui::{NumberRow, SettingRow, ToggleRow};

use super::{Entry, Group, SettingsScreen, searchable, setter};
use crate::agent::{self, AgentState, ServerStatus, SessionRow, install};
use crate::secret_clipboard;

/// Audit entries shown; the file has them all.
const AUDIT_SHOWN: usize = 50;

/// How long a read of the clients' configs is shown before it is read
/// again (the page re-renders every second while sessions run).
const STATUS_TTL: std::time::Duration = std::time::Duration::from_secs(3);

/// Each client with its status.
type Statuses = Vec<(install::Client, install::Status)>;

/// What the page keeps between renders.
pub(super) struct AgentForm {
    /// "Manual setup" is open.
    manual: bool,
    /// The clients' connection status and when it was read (#400).
    statuses: std::cell::RefCell<Option<(std::time::Instant, Statuses)>>,
}

impl AgentForm {
    pub(super) fn new(_window: &mut Window, _cx: &mut Context<SettingsScreen>) -> Self {
        Self {
            manual: false,
            statuses: std::cell::RefCell::new(None),
        }
    }

    /// Every client's status, read again when older than [`STATUS_TTL`].
    fn statuses(
        &self,
        env: &install::Env,
        exe: &std::path::Path,
    ) -> Vec<(install::Client, install::Status)> {
        let mut cache = self.statuses.borrow_mut();
        if let Some((at, statuses)) = cache.as_ref()
            && at.elapsed() < STATUS_TTL
        {
            return statuses.clone();
        }
        let statuses: Vec<_> = install::Client::ALL
            .into_iter()
            .map(|client| (client, install::status(env, client, exe)))
            .collect();
        *cache = Some((std::time::Instant::now(), statuses.clone()));
        statuses
    }

    /// Reads the statuses again on the next render.
    fn forget_statuses(&mut self) {
        *self.statuses.get_mut() = None;
    }
}

/// How a namespace's boundary reads on the sessions screen.
fn boundary_label(boundary: &BoundaryInfo) -> String {
    let who = match boundary.mode {
        SessionMode::ClientOnly => t!("settings-boundary-client-only"),
        SessionMode::Impersonation => t!("settings-boundary-impersonation"),
        SessionMode::TemporaryServiceAccount => t!("settings-boundary-temporary-sa"),
    };
    let namespace = boundary.namespace.as_str();
    // A configured server-side identity that failed is never silent (#197).
    match &boundary.warning {
        Some(warning) => t!(
            "settings-boundary-warning",
            namespace = namespace,
            who = who,
            warning = warning.to_string()
        ),
        None => t!("settings-boundary", namespace = namespace, who = who),
    }
}

/// How the sessions screen names a right.
fn grant_label(grant: &Grant) -> String {
    match grant {
        Grant::Read { namespace } => t!("settings-grant-read", namespace = namespace.as_str()),
        Grant::Secret { namespace, name } => t!(
            "settings-grant-secret",
            namespace = namespace.as_str(),
            name = name.as_str()
        ),
        Grant::Modify { namespace } => t!("settings-grant-modify", namespace = namespace.as_str()),
    }
}

/// A session's ceiling, named like the choices of the security settings.
fn profile_label(profile: Profile) -> String {
    match profile {
        Profile::ReadOnly => t!("settings-profile-read-only"),
        Profile::Debug => t!("settings-profile-debug"),
        Profile::Deploy => t!("settings-profile-deploy"),
    }
}

/// [`install::Status::label`] in the interface language.
fn client_status(status: &install::Status) -> String {
    use install::Status;
    match status {
        Status::NotFound => t!("settings-client-not-installed"),
        Status::NotConnected => t!("settings-client-not-connected"),
        Status::Connected => t!("settings-client-connected"),
        Status::Stale { command } => t!("settings-client-stale", command = command.as_str()),
        Status::Broken(error) => t!("settings-client-broken", error = error.as_str()),
    }
}

/// [`install::Plan::summary`] in the interface language.
fn plan_summary(plan: &install::Plan) -> String {
    use install::{Change, SERVER_NAME};
    let path = plan.path.display().to_string();
    match &plan.change {
        Change::Add if plan.before.is_none() => {
            t!("settings-plan-create", path = path, server = SERVER_NAME)
        }
        Change::Add => t!("settings-plan-add", path = path, server = SERVER_NAME),
        Change::Update { command } => t!(
            "settings-plan-update",
            path = path,
            server = SERVER_NAME,
            command = command.as_str()
        ),
        Change::Remove => t!("settings-plan-remove", path = path, server = SERVER_NAME),
        Change::Nothing => t!("settings-plan-nothing", path = path),
    }
}

/// [`install::Client::restart_hint`] in the interface language.
fn restart_hint(client: install::Client) -> String {
    use install::Client;
    match client {
        Client::ClaudeCode => t!("settings-restart-claude-code"),
        Client::Codex => t!("settings-restart-codex"),
        Client::OpenCode => t!("settings-restart-opencode"),
        Client::Cursor => t!("settings-restart-cursor"),
        Client::ClaudeDesktop => t!("settings-restart-claude-desktop"),
    }
}

/// Copies `text` and says so. Text with the MCP token goes through
/// [`secret_clipboard`]: marked concealed and cleared after a while.
fn copy_button(id: impl Into<ElementId>, text: String, secret: bool) -> Button {
    Button::new(id)
        .ghost()
        .xsmall()
        .icon(IconName::Copy)
        .tooltip(t!("settings-copy"))
        .on_click(move |_, window, cx| {
            use gpui_kit::component::WindowExt as _;
            use gpui_kit::component::notification::Notification;
            if secret {
                secret_clipboard::copy(&text, cx);
            } else {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
            }
            window.push_notification(Notification::info(t!("settings-copied")), cx);
        })
}

/// A block of code with a copy button; `shown` may hide the token, and then
/// the copy is treated as a secret.
fn snippet(id: &'static str, shown: String, copied: String, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let secret = shown != copied;
    div()
        .flex()
        .items_start()
        .gap_2()
        .px_2()
        .py_1p5()
        .rounded_md()
        .bg(theme.muted)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .font_family(theme.mono_font_family.clone())
                .text_xs()
                .whitespace_normal()
                .children(shown.lines().map(|line| div().child(line.to_owned()))),
        )
        .child(copy_button(id, copied, secret))
}

/// Quotes `text` for a POSIX shell when needed.
fn shell_quote(text: &str) -> String {
    if !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@".contains(c))
    {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

fn masked(token: &str) -> String {
    let tail: String = token
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("••••••••{tail}")
}

impl SettingsScreen {
    pub(super) fn agent(&self, settings: &Settings, cx: &mut Context<Self>) -> Vec<Group> {
        let Some(state) = agent::global(cx) else {
            return Vec::new();
        };
        let mut groups = vec![self.agent_server(settings, &state, cx)];
        groups.push(self.agent_connect(settings, &state, cx));
        groups.push(Self::agent_sessions(&state, cx));
        groups.push(Self::agent_audit(&state, cx));
        groups
    }

    fn agent_server(
        &self,
        settings: &Settings,
        state: &Entity<AgentState>,
        cx: &mut Context<Self>,
    ) -> Group {
        let agent = state.read(cx);
        let mut group = Group::new(t!("settings-group-http-server"))
            .description(t!("settings-http-server-description"));
        let status: SharedString = match agent.status() {
            ServerStatus::Off => t!("settings-server-off").into(),
            ServerStatus::Starting => t!("settings-server-starting").into(),
            ServerStatus::Running { url } => {
                t!("settings-server-listening", url = url.to_string()).into()
            }
            ServerStatus::Failed(error) => {
                group = group.notice(Alert::error("mcp-server-error", error.clone()).small());
                t!("settings-server-not-running").into()
            }
        };
        let title = t!("settings-mcp-http");
        group = group.entry(Entry::new(
            searchable("http server mcp enable listen localhost", &[&title]),
            ToggleRow::new("mcp-http", title)
                .description(status)
                .checked(settings.agent.http_enabled)
                .default_value(false)
                .on_change(setter(&self.settings, |s, on| s.agent.http_enabled = on)),
        ));
        let (title, description) = (t!("settings-mcp-port"), t!("settings-mcp-port-description"));
        group = group.entry(Entry::new(
            searchable("http port", &[&title, &description]),
            NumberRow::new("mcp-port", title)
                .description(description)
                .value(u32::from(settings.agent.http_port))
                .range(1024..=65535)
                .step(1)
                .default_value(u32::from(DEFAULT_MCP_PORT))
                .on_change(setter(&self.settings, |s, port: u32| {
                    s.agent.http_port = u16::try_from(port).unwrap_or(DEFAULT_MCP_PORT)
                })),
        ));
        let url = format!("http://127.0.0.1:{}/mcp", settings.agent.http_port);
        group = group.entry(Entry::new(
            "url address endpoint copy",
            SettingRow::new("mcp-url", "URL")
                .description(url.clone())
                .control(copy_button("mcp-url-copy", url, false)),
        ));
        let token = agent.token().map(str::to_owned);
        let regenerate = state.downgrade();
        let title = t!("settings-mcp-token");
        group.entry(Entry::new(
            searchable(
                "token bearer authorization copy regenerate",
                &[&title, &t!("settings-mcp-token-regenerate")],
            ),
            SettingRow::new("mcp-token", title)
                .description(match &token {
                    Some(token) => t!("settings-mcp-token-description", token = masked(token)),
                    None => t!("settings-mcp-token-none"),
                })
                .control(
                    div()
                        .flex()
                        .gap_1()
                        .when_some(token, |this, token| {
                            this.child(copy_button("mcp-token-copy", token, true))
                        })
                        .child(
                            Button::new("mcp-token-regenerate")
                                .outline()
                                .small()
                                .label(t!("settings-mcp-token-regenerate"))
                                .tooltip(t!("settings-mcp-token-regenerate-tooltip"))
                                .on_click(move |_, window, cx| {
                                    let result = regenerate
                                        .update(cx, |agent, cx| agent.regenerate_token(cx));
                                    if let Ok(Err(error)) = result {
                                        use gpui_kit::component::WindowExt as _;
                                        use gpui_kit::component::notification::Notification;
                                        window.push_notification(Notification::error(error), cx);
                                    }
                                }),
                        ),
                ),
        ))
    }

    fn agent_connect(
        &self,
        settings: &Settings,
        state: &Entity<AgentState>,
        cx: &mut Context<Self>,
    ) -> Group {
        let mut group = Group::new(t!("settings-group-connect"))
            .description(t!("settings-connect-description"));
        let exe = std::env::current_exe().ok();
        let env = install::Env::current();
        if let (Some(exe), Some(env)) = (exe.clone(), env.clone()) {
            for (client, status) in self.agent.statuses(&env, &exe) {
                group = group.entry(Entry::new(
                    format!(
                        "connect agent client {} {} {} {}",
                        client.name(),
                        client.id(),
                        t!("settings-client-connect"),
                        client_status(&status)
                    ),
                    client_row(client, &status, &env, &exe, cx),
                ));
            }
        } else {
            group = group.notice(
                Alert::warning(
                    "agent-connect-unavailable",
                    t!("settings-connect-unavailable"),
                )
                .small(),
            );
        }

        let manual = self.agent.manual;
        let entity = cx.entity().downgrade();
        let (title, description) = (
            t!("settings-manual-setup"),
            t!("settings-manual-setup-description"),
        );
        group = group.entry(Entry::new(
            searchable(
                "manual setup snippet http stdio other clients",
                &[&title, &description],
            ),
            SettingRow::new("agent-manual", title)
                .description(description)
                .control(
                    Button::new("agent-manual-toggle")
                        .ghost()
                        .small()
                        .label(if manual {
                            t!("settings-manual-hide")
                        } else {
                            t!("settings-manual-show")
                        })
                        .on_click(move |_, _, cx| {
                            entity
                                .update(cx, |this, cx| {
                                    this.agent.manual = !this.agent.manual;
                                    cx.notify();
                                })
                                .ok();
                        }),
                ),
        ));
        if !manual {
            return group;
        }

        let agent = state.read(cx);
        let url = format!("http://127.0.0.1:{}/mcp", settings.agent.http_port);
        // The command carries no token (#191): Claude Code reads it from the
        // environment, the shell from the token file.
        let token_file = agent
            .token_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<settings folder>/mcp-token".to_owned());
        let exe = exe
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "kubach".to_owned());
        let http = format!(
            "export KUBACH_MCP_TOKEN=\"$(cat {})\"\n\
             claude mcp add --transport http --scope user kubach {url} \\\n  \
             --header 'Authorization: Bearer ${{KUBACH_MCP_TOKEN}}'",
            shell_quote(&token_file)
        );
        let stdio = install::snippet(install::Client::ClaudeDesktop, &exe);
        let (stdio_title, stdio_description) = (
            t!("settings-snippet-stdio"),
            t!("settings-snippet-stdio-description"),
        );
        let (http_title, http_description) = (
            t!("settings-snippet-claude-http"),
            t!("settings-snippet-claude-http-description"),
        );
        group
            .entry(Entry::new(
                searchable(
                    "stdio json snippet other client",
                    &[&stdio_title, &stdio_description],
                ),
                SettingRow::new("snippet-stdio", stdio_title)
                    .description(stdio_description)
                    .below(snippet("snippet-stdio-copy", stdio.clone(), stdio, cx)),
            ))
            .entry(Entry::new(
                searchable(
                    "claude code http snippet connect",
                    &[&http_title, &http_description],
                ),
                SettingRow::new("snippet-claude-http", http_title)
                    .description(http_description)
                    .below(snippet("snippet-claude-http-copy", http.clone(), http, cx)),
            ))
    }

    fn agent_sessions(state: &Entity<AgentState>, cx: &mut Context<Self>) -> Group {
        let agent = state.read(cx);
        let rows = agent.sessions();
        let mut group = Group::new(t!("settings-group-sessions"))
            .description(t!("settings-sessions-description"));
        if let Some(error) = agent.grant_error() {
            group = group.notice(Alert::error("agent-grant-error", error.clone()).small());
        }
        if let Some(error) = agent.session_error() {
            group = group.notice(Alert::error("agent-session-error", error.clone()).small());
        }
        // HTTP agents choose their context themselves (#421).
        if agent.http_waiting_for_context() {
            group = group.notice(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("settings-sessions-http-waiting")),
            );
        }
        if rows.is_empty() {
            return group.notice(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("settings-sessions-none")),
            );
        }
        for (ix, row) in rows.into_iter().enumerate() {
            let text = format!(
                "session {} {} {} {} {}",
                row.context,
                row.namespaces.join(" "),
                row.profile,
                profile_label(row.profile),
                row.clients.join(" ")
            );
            group = group.entry(Entry::new(text, session_row(ix, row, state, cx)));
        }
        group
    }

    fn agent_audit(state: &Entity<AgentState>, cx: &mut Context<Self>) -> Group {
        let agent = state.read(cx);
        let theme = cx.theme();
        let mut group =
            Group::new(t!("settings-group-audit")).description(t!("settings-audit-description"));
        if let Some(error) = agent.audit_error() {
            group = group.notice(Alert::error("audit-error", error.clone()).small());
        }
        let path = agent.audit_path();
        let rows: Vec<_> = agent
            .audit_rows()
            .iter()
            .rev()
            .take(AUDIT_SHOWN)
            .cloned()
            .collect();
        let list = div()
            .flex()
            .flex_col()
            .gap_0p5()
            .children(rows.into_iter().map(|row| {
                let color = if row.allowed && row.result == "ok" {
                    theme.foreground
                } else if row.allowed {
                    theme.warning
                } else {
                    theme.danger
                };
                let detail = row.reason.clone().unwrap_or_else(|| row.args.clone());
                div()
                    .flex()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(theme.muted)
                    .text_xs()
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_color(theme.muted_foreground)
                            .child(agent::clock(&row.time)),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .w(px(120.))
                            .truncate()
                            .child(row.client),
                    )
                    .child(div().flex_shrink_0().w(px(130.)).truncate().child(row.tool))
                    .child(
                        div()
                            .flex_shrink_0()
                            .w(px(110.))
                            .text_color(color)
                            .child(format!("{} · {}", row.result, row.decided_by)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.muted_foreground)
                            .child(detail),
                    )
            }));
        let empty = agent.audit_rows().is_empty();
        let reveal = path.clone();
        let title = t!("settings-audit-recent");
        group.entry(Entry::new(
            searchable("audit log journal calls history", &[&title]),
            SettingRow::new("audit-log", title)
                .description(match &path {
                    Some(path) => t!("settings-audit-file", path = path.display().to_string()),
                    None => t!("settings-audit-no-folder"),
                })
                .when_some(reveal, |row, path| {
                    row.control(
                        Button::new("audit-reveal")
                            .outline()
                            .small()
                            .label(t!("settings-audit-show-file"))
                            .disabled(!path.exists())
                            .on_click(move |_, _, cx| cx.reveal_path(&path)),
                    )
                })
                .when(!empty, |row| row.below(list)),
        ))
    }
}

/// One client on the "Connect an agent" list: status and its buttons.
fn client_row(
    client: install::Client,
    status: &install::Status,
    env: &install::Env,
    exe: &std::path::Path,
    cx: &mut Context<SettingsScreen>,
) -> SettingRow {
    use install::Status;
    let path = env.config_path(client);
    let description = format!("{} · {}", client_status(status), path.display());
    let button = |id: &'static str, label: String, install: bool, primary: bool| {
        let (env, exe, screen) = (env.clone(), exe.to_owned(), cx.entity().downgrade());
        let button = Button::new((id, client as usize)).small().label(label);
        let button = if primary {
            button.primary()
        } else {
            button.outline()
        };
        button.on_click(move |_, window, cx| {
            open_client_change(client, install, &env, &exe, screen.clone(), window, cx)
        })
    };
    let controls = div().flex().gap_1();
    let controls = match status {
        Status::Connected => controls.child(button(
            "agent-disconnect",
            t!("settings-client-disconnect"),
            false,
            false,
        )),
        Status::Stale { .. } => controls
            .child(button(
                "agent-reconnect",
                t!("settings-client-reconnect"),
                true,
                true,
            ))
            .child(button(
                "agent-disconnect",
                t!("settings-client-disconnect"),
                false,
                false,
            )),
        Status::NotConnected | Status::NotFound => controls.child(button(
            "agent-connect",
            t!("settings-client-connect"),
            true,
            true,
        )),
        Status::Broken(_) => controls,
    };
    SettingRow::new(("agent-client", client as usize), client.name())
        .description(description)
        .control(controls)
}

/// Previews connecting (or disconnecting) `client` and asks: the file, what
/// changes and the diff. Nothing is written before Apply.
fn open_client_change(
    client: install::Client,
    connect: bool,
    env: &install::Env,
    exe: &std::path::Path,
    screen: WeakEntity<SettingsScreen>,
    window: &mut Window,
    cx: &mut App,
) {
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::component::notification::Notification;
    let plan = if connect {
        install::plan_install(env, client, exe)
    } else {
        install::plan_uninstall(env, client)
    };
    let plan = match plan {
        Ok(plan) => plan,
        Err(error) => {
            window.push_notification(Notification::error(error.to_string()), cx);
            return;
        }
    };
    if plan.change == install::Change::Nothing {
        window.push_notification(Notification::info(plan_summary(&plan)), cx);
        return;
    }
    let diff = plan.diff();
    window.open_alert_dialog(cx, move |dialog, _, cx| {
        let theme = cx.theme();
        let plan = plan.clone();
        let screen = screen.clone();
        let lines = div()
            .id("agent-client-diff")
            .max_h(px(280.))
            .overflow_y_scroll()
            .p_2()
            .rounded_md()
            .bg(theme.muted)
            .font_family(theme.mono_font_family.clone())
            .text_xs()
            .children(diff.lines().map(|line| {
                let color = if line.starts_with('+') && !line.starts_with("+++") {
                    theme.success
                } else if line.starts_with('-') && !line.starts_with("---") {
                    theme.danger
                } else {
                    theme.foreground
                };
                div().text_color(color).child(line.to_owned())
            }));
        dialog
            .confirm()
            .title(if connect {
                t!("settings-client-connect-title", client = client.name())
            } else {
                t!("settings-client-disconnect-title", client = client.name())
            })
            .width(px(560.))
            .description(t!(
                "settings-client-change-description",
                summary = plan_summary(&plan),
                client = client.name(),
                suffix = install::BACKUP_SUFFIX
            ))
            .child(lines)
            .ok_text(t!("settings-client-apply"))
            .on_ok(move |_, window, cx| {
                match install::apply(&plan) {
                    Ok(_) => window.push_notification(
                        Notification::success(if connect {
                            t!(
                                "settings-client-connected-done",
                                hint = restart_hint(plan.client)
                            )
                        } else {
                            t!(
                                "settings-client-disconnected-done",
                                hint = restart_hint(plan.client)
                            )
                        }),
                        cx,
                    ),
                    Err(error) => {
                        window.push_notification(Notification::error(error.to_string()), cx)
                    }
                }
                screen
                    .update(cx, |screen, cx| {
                        screen.agent.forget_statuses();
                        cx.notify();
                    })
                    .ok();
                true
            })
    });
}

fn session_row(
    ix: usize,
    row: SessionRow,
    state: &Entity<AgentState>,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let title = t!(
        "settings-session-title",
        context = row.context.as_str(),
        profile = profile_label(row.profile)
    );
    let clients = if row.clients.is_empty() {
        t!("settings-session-no-calls")
    } else {
        row.clients.join(", ")
    };
    let mut origin = match row.pid {
        None => "HTTP".to_owned(),
        Some(pid) => t!("settings-session-pid", pid = pid.to_string()),
    };
    if row.vault {
        origin = format!("{origin} · {}", t!("settings-session-vault"));
    }
    // Per namespace: who holds the boundary (#401). Namespaces pinned at
    // launch but not read yet have none.
    let mut namespaces: Vec<(String, bool)> = row
        .boundaries
        .iter()
        .map(|boundary| (boundary_label(boundary), boundary.server_enforced()))
        .collect();
    for namespace in &row.namespaces {
        if !row.boundaries.iter().any(|b| &b.namespace == namespace) {
            namespaces.push((
                t!(
                    "settings-session-namespace-not-read",
                    namespace = namespace.as_str()
                ),
                true,
            ));
        }
    }
    let no_namespaces = namespaces.is_empty();
    let details = t!(
        "settings-session-details",
        origin = origin,
        clients = clients,
        calls = row.calls,
        since = agent::clock(&row.started_at),
        left = agent::remaining(&row.expires_at)
    );
    let revoke = state.downgrade();
    let id = row.id.clone();
    let grants = div()
        .flex()
        .flex_col()
        .gap_0p5()
        .children(row.grants.iter().enumerate().map(|(gx, entry)| {
            let granted = entry.state == GrantState::Granted;
            let take_back = state.downgrade();
            let session = row.id.clone();
            let grant = entry.grant.clone();
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(match (&entry.grant, granted) {
                            (_, false) => theme.muted_foreground,
                            (Grant::Secret { .. }, true) => theme.warning,
                            _ => theme.foreground,
                        })
                        .child(format!(
                            "{} · {}",
                            grant_label(&entry.grant),
                            match (granted, entry.at_launch) {
                                (true, true) => t!("settings-grant-allowed-at-launch"),
                                (true, false) => t!("settings-grant-allowed"),
                                (false, _) => t!("settings-grant-denied"),
                            }
                        )),
                )
                .when(granted, |this| {
                    this.child(
                        Button::new(("agent-revoke-grant", ix * 1000 + gx))
                            .ghost()
                            .xsmall()
                            .label(t!("settings-revoke"))
                            .on_click(move |_, _, cx| {
                                let grant = grant.clone();
                                take_back
                                    .update(cx, |agent, cx| agent.revoke_grant(&session, grant, cx))
                                    .ok();
                            }),
                    )
                })
        }));
    let no_grants = row.grants.is_empty();
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_2()
        .rounded_md()
        .bg(theme.muted)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().text_sm().truncate().child(title))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .truncate()
                        .child(details),
                )
                .when(no_namespaces, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(t!("settings-session-no-namespace")),
                    )
                })
                .children(namespaces.into_iter().map(|(label, enforced)| {
                    div()
                        .text_xs()
                        .whitespace_normal()
                        .text_color(if enforced {
                            theme.muted_foreground
                        } else {
                            theme.warning
                        })
                        .child(label)
                }))
                .when_some(row.raised, |this, raised| {
                    this.child(
                        div()
                            .text_xs()
                            .whitespace_normal()
                            .text_color(theme.warning)
                            .child(t!("settings-session-raised", raised = raised.to_string())),
                    )
                })
                .when(no_grants, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(t!("settings-session-no-grants")),
                    )
                })
                .when(!no_grants, |this| this.child(grants)),
        )
        .child(
            Button::new(("agent-revoke", ix))
                .outline()
                .small()
                .label(if row.revoking {
                    t!("settings-revoking")
                } else {
                    t!("settings-revoke")
                })
                .disabled(row.revoking)
                .on_click(move |_, _, cx| {
                    revoke.update(cx, |agent, cx| agent.revoke(&id, cx)).ok();
                }),
        )
}

#[cfg(test)]
mod tests {
    use super::{masked, shell_quote};

    #[test]
    fn quoting_and_masking() {
        assert_eq!(shell_quote("minikube"), "minikube");
        assert_eq!(
            shell_quote("/Applications/My Kubach.app"),
            "'/Applications/My Kubach.app'"
        );
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(masked("abcdef123456"), "••••••••3456");
    }
}
