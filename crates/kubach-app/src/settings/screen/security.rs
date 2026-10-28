//! Security of the selected context on the Clusters page (#278): whether
//! the user sees Secret values, and what an AI agent connected over MCP
//! gets without asking (#15).

use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::Button;
use gpui_kit::component::{Disableable as _, Sizable as _};
use gpui_kit::*;
use kubach_core::settings::{AgentAccess, AgentLevel, ContextKey, ContextPolicy, Settings};
use kubach_i18n::t;
use kubach_ui::{SelectRow, SettingRow, ToggleRow};

use super::{
    Anchor, Choice, Entry, Group, SettingsScreen, index_of, searchable, setter, translated,
};
use crate::settings::AppSettings;

const LEVELS: [Choice<AgentLevel>; 3] = [
    (AgentLevel::Read, || t!("settings-minimum-read")),
    (AgentLevel::Debug, || t!("settings-minimum-debug")),
    (AgentLevel::Changes, || t!("settings-minimum-changes")),
];

/// Search text of a select: its keywords, title, description and choices.
fn select_text<T>(keywords: &str, title: &str, description: &str, options: &[Choice<T>]) -> String {
    let choices = translated(options).join(" ");
    searchable(keywords, &[title, description, &choices])
}

/// [`setter`] for the security policy of `key`.
fn policy_setter<T: 'static>(
    settings: &Entity<AppSettings>,
    key: &ContextKey,
    apply: impl Fn(&mut ContextPolicy, T) + 'static,
) -> impl Fn(T, &mut Window, &mut App) + 'static {
    let key = key.clone();
    setter(settings, move |settings, value| {
        let mut policy = settings.security.policy(&key);
        apply(&mut policy, value);
        settings.security.set_policy(&key, policy);
    })
}

impl SettingsScreen {
    /// Security policy of the context `name` with API server `server`:
    /// Secret values in the UI, the AI agent's access and the history of
    /// raises.
    pub(super) fn security_groups(
        &self,
        name: &str,
        server: &str,
        settings: &Settings,
        _cx: &mut Context<Self>,
    ) -> Vec<Group> {
        let key = ContextKey::new(name.to_owned(), server);
        let policy = settings.security.policy(&key);
        let defaults = ContextPolicy::default();
        let keywords = format!("security {name} {server}");

        let (title, description) = (
            t!("settings-hide-secret-values"),
            t!("settings-hide-secret-values-description"),
        );
        let context_group = Group::new(t!("settings-group-security"))
            .description(if server.is_empty() {
                t!("settings-security-description-no-server")
            } else {
                t!("settings-security-description", server = server)
            })
            .keywords(&keywords)
            .anchor(Anchor::Security)
            .entry(Entry::new(
                searchable(
                    "secrets hide values show copy load reveal helm kustomize production",
                    &[&title, &description],
                ),
                ToggleRow::new("hide-secret-values", title)
                    .description(description)
                    .checked(policy.hide_secret_values)
                    .default_value(defaults.hide_secret_values)
                    .on_change(policy_setter(&self.settings, &key, |p, on| {
                        p.hide_secret_values = on
                    })),
            ));

        let (title, description) = (
            t!("settings-mcp-enabled"),
            t!("settings-mcp-enabled-description"),
        );
        let mut agent = Group::new(t!("settings-group-agent-mcp"))
            .description(t!("settings-agent-mcp-description"))
            .keywords(&keywords)
            .entry(Entry::new(
                searchable("mcp ai agent access enable", &[&title, &description]),
                ToggleRow::new("mcp-enabled", title)
                    .description(description)
                    .checked(policy.mcp_enabled)
                    .default_value(defaults.mcp_enabled)
                    .on_change(policy_setter(&self.settings, &key, |p, on| {
                        p.mcp_enabled = on
                    })),
            ));
        if !policy.mcp_enabled {
            // The rest only matters with access on; the values are kept.
            return vec![context_group, agent];
        }
        // Raised by default from an agent's request (#417, #15): what,
        // when, from which session.
        let raises = settings.security.raises_of(&key);
        if let Some(last) = raises.first() {
            let (time, changes) = (crate::agent::clock(&last.time), last.changes.join("; "));
            let mut text = if last.client.is_empty() {
                t!(
                    "settings-access-raised",
                    time = time,
                    session = last.session.as_str(),
                    changes = changes
                )
            } else {
                t!(
                    "settings-access-raised-client",
                    time = time,
                    session = last.session.as_str(),
                    client = last.client.as_str(),
                    changes = changes
                )
            };
            if raises.len() > 1 {
                let more = t!("settings-access-raised-earlier", count = raises.len() - 1);
                text = format!("{text} {more}");
            }
            agent = agent.notice(
                Alert::info("security-access-raised", text)
                    .title(t!("settings-access-raised-title"))
                    .small(),
            );
        }
        let (level_title, level_description) = (
            t!("settings-minimum-access"),
            t!("settings-minimum-access-description"),
        );
        let (secrets_title, secrets_description) = (
            t!("settings-minimum-secrets"),
            t!("settings-minimum-secrets-description"),
        );
        let (crds_title, crds_description) = (
            t!("settings-minimum-crds"),
            t!("settings-minimum-crds-description"),
        );
        let (reset_title, reset_description) = (
            t!("settings-minimum-reset"),
            t!("settings-minimum-reset-description"),
        );
        let reset = policy_setter(&self.settings, &key, |p, ()| {
            p.set_minimum(AgentAccess::default())
        });
        agent = agent
            .entry(Entry::new(
                select_text(
                    "minimal minimum access level read debug changes logs exec apply delete helm",
                    &level_title,
                    &level_description,
                    &LEVELS,
                ),
                SelectRow::new("minimum-access", level_title, translated(&LEVELS))
                    .description(level_description)
                    .selected(index_of(&LEVELS, policy.minimum_access))
                    .default_index(index_of(&LEVELS, defaults.minimum_access))
                    .on_change(policy_setter(&self.settings, &key, |p, ix: usize| {
                        p.minimum_access = LEVELS[ix].0
                    })),
            ))
            .entry(Entry::new(
                searchable(
                    "minimal minimum secrets metadata checks keys default",
                    &[&secrets_title, &secrets_description],
                ),
                ToggleRow::new("minimum-secrets", secrets_title)
                    .description(secrets_description)
                    .checked(policy.minimum_secrets)
                    .default_value(defaults.minimum_secrets)
                    .on_change(policy_setter(&self.settings, &key, |p, on| {
                        p.minimum_secrets = on
                    })),
            ))
            .entry(Entry::new(
                searchable(
                    "minimal minimum custom resources crd flux argo kinds default",
                    &[&crds_title, &crds_description],
                ),
                ToggleRow::new("minimum-crds", crds_title)
                    .description(crds_description)
                    .checked(policy.minimum_crds)
                    .default_value(defaults.minimum_crds)
                    .on_change(policy_setter(&self.settings, &key, |p, on| {
                        p.minimum_crds = on
                    })),
            ))
            .entry(Entry::new(
                searchable(
                    "reset read minimal minimum access default",
                    &[&reset_title, &reset_description],
                ),
                SettingRow::new("minimum-reset", reset_title.clone())
                    .description(reset_description)
                    .control(
                        Button::new("minimum-reset-button")
                            .outline()
                            .small()
                            .label(reset_title)
                            .disabled(!policy.minimum_raised())
                            .on_click(move |_, window, cx| reset((), window, cx)),
                    ),
            ));
        vec![context_group, agent]
    }
}
