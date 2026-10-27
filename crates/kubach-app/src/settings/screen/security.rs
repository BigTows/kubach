//! Security of the selected context on the Clusters page (#278): what an
//! AI agent connected over MCP may do.

use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::Button;
use gpui_kit::component::{Selectable as _, Sizable as _};
use gpui_kit::*;
use kubach_core::settings::security::{MAX_SESSION_TTL_MINUTES, MIN_SESSION_TTL_MINUTES};
use kubach_core::settings::{
    AgentSecrets, ContextKey, ContextPolicy, DeployMode, ExecMode, NamespaceLimit, Preset, Profile,
    Settings,
};
use kubach_i18n::t;
use kubach_ui::{NumberRow, SelectRow, SettingRow, ToggleRow};

use super::{
    Anchor, Choice, Entry, Group, SettingsScreen, format_minutes, index_of, searchable, setter,
    translated,
};
use crate::settings::AppSettings;
use crate::workspace::hotbar::loosened_label;

const PROFILES: [Choice<Profile>; 3] = [
    (Profile::ReadOnly, || t!("settings-profile-read-only")),
    (Profile::Debug, || t!("settings-profile-debug")),
    (Profile::Deploy, || t!("settings-profile-deploy")),
];
const DEPLOY_MODES: [Choice<DeployMode>; 2] = [
    (DeployMode::DryRun, || t!("settings-deploy-dry-run")),
    (DeployMode::ApplyWithConfirmation, || {
        t!("settings-deploy-apply")
    }),
];
const EXEC_MODES: [Choice<ExecMode>; 3] = [
    (ExecMode::Disabled, || t!("settings-exec-never")),
    (ExecMode::ConfirmEach, || t!("settings-exec-single")),
    (ExecMode::InteractiveShell, || t!("settings-exec-shell")),
];
const NAMESPACE_LIMITS: [Choice<NamespaceLimit>; 2] = [
    (NamespaceLimit::Single, || t!("settings-namespaces-one")),
    (NamespaceLimit::Multiple, || {
        t!("settings-namespaces-several")
    }),
];
const AGENT_SECRETS: [Choice<AgentSecrets>; 3] = [
    (AgentSecrets::None, || t!("settings-agent-secrets-none")),
    (AgentSecrets::MetadataAndChecks, || {
        t!("settings-agent-secrets-metadata")
    }),
    (AgentSecrets::RevealOnRequest, || {
        t!("settings-agent-secrets-reveal")
    }),
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
    /// Security policy of the context `name` with API server `server`: the
    /// preset, Secrets, the AI agent over MCP and its sessions.
    pub(super) fn security_groups(
        &self,
        name: &str,
        server: &str,
        settings: &Settings,
        _cx: &mut Context<Self>,
    ) -> Vec<Group> {
        let key = ContextKey::new(name.to_owned(), server);
        let policy = settings.security.policy(&key);
        let strict = ContextPolicy::strict();
        let keywords = format!("security {name} {server}");

        let preset_button = |id: &'static str, label: String, preset: Preset| {
            let apply = policy_setter(&self.settings, &key, move |p, ()| {
                *p = ContextPolicy::preset(preset)
            });
            Button::new(id)
                .outline()
                .small()
                .label(label)
                .selected(policy.matching_preset() == Some(preset))
                .on_click(move |_, window, cx| apply((), window, cx))
        };
        let (strict_label, development_label) = (
            t!("settings-preset-strict"),
            t!("settings-preset-development"),
        );
        let presets_text = searchable(
            "preset strict development local",
            &[
                &t!("settings-security-preset"),
                &strict_label,
                &development_label,
            ],
        );
        let presets = SettingRow::new("preset", t!("settings-security-preset"))
            .description(match policy.matching_preset() {
                Some(Preset::Strict) => t!("settings-security-preset-strict"),
                Some(Preset::Development) => t!("settings-security-preset-development"),
                None => t!("settings-security-preset-custom"),
            })
            .control(
                div()
                    .flex()
                    .gap_1()
                    .child(preset_button("preset-strict", strict_label, Preset::Strict))
                    .child(preset_button(
                        "preset-development",
                        development_label,
                        Preset::Development,
                    )),
            );

        let mut context_group = Group::new(t!("settings-group-security"))
            .description(if server.is_empty() {
                t!("settings-security-description-no-server")
            } else {
                t!("settings-security-description", server = server)
            })
            .keywords(&keywords)
            .anchor(Anchor::Security);
        let loosened = policy.loosened();
        if !loosened.is_empty() {
            let list: Vec<_> = loosened.iter().map(|l| loosened_label(*l)).collect();
            context_group = context_group.notice(
                Alert::warning(
                    "security-loosened",
                    t!(
                        "settings-security-loosened",
                        name = name,
                        list = list.join(", ")
                    ),
                )
                .title(t!("settings-security-relaxed"))
                .small(),
            );
        }
        if key.suggested_preset() == Some(Preset::Development)
            && policy.matching_preset() != Some(Preset::Development)
        {
            context_group = context_group.notice(
                Alert::info(
                    "security-suggest",
                    t!("settings-security-suggest-development"),
                )
                .small(),
            );
        }
        context_group = context_group.entry(Entry::new(presets_text, presets));

        let (title, description) = (
            t!("settings-hide-secret-values"),
            t!("settings-hide-secret-values-description"),
        );
        let secrets = Group::new(t!("settings-group-secrets"))
            .keywords(&keywords)
            .entry(Entry::new(
                searchable(
                    "secrets hide values show copy load reveal helm kustomize production strict",
                    &[&title, &description],
                ),
                ToggleRow::new("hide-secret-values", title)
                    .description(description)
                    .checked(policy.hide_secret_values)
                    .default_value(strict.hide_secret_values)
                    .on_change(policy_setter(&self.settings, &key, |p, on| {
                        p.hide_secret_values = on
                    })),
            ));

        let profile_ok = |min: Profile| policy.max_profile >= min;
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
                    .default_value(strict.mcp_enabled)
                    .on_change(policy_setter(&self.settings, &key, |p, on| {
                        p.mcp_enabled = on
                    })),
            ));
        if !policy.mcp_enabled {
            // The rest only matters with access on; the values are kept.
            return vec![context_group, secrets, agent];
        }
        // Raised for good from an agent's request (#417): what, when, from
        // which session.
        let raises = settings.security.raises_of(&key);
        if let Some(last) = raises.first() {
            let (time, changes) = (crate::agent::clock(&last.time), last.changes.join("; "));
            let mut text = if last.client.is_empty() {
                t!(
                    "settings-ceiling-raised",
                    time = time,
                    session = last.session.as_str(),
                    changes = changes
                )
            } else {
                t!(
                    "settings-ceiling-raised-client",
                    time = time,
                    session = last.session.as_str(),
                    client = last.client.as_str(),
                    changes = changes
                )
            };
            if raises.len() > 1 {
                let more = t!("settings-ceiling-raised-earlier", count = raises.len() - 1);
                text = format!("{text} {more}");
            }
            agent = agent.notice(
                Alert::info("security-ceiling-raised", text)
                    .title(t!("settings-ceiling-raised-title"))
                    .small(),
            );
        }
        let (protected_title, protected_description) = (
            t!("settings-protected"),
            t!("settings-protected-description"),
        );
        let (profile_title, profile_description) = (
            t!("settings-max-profile"),
            t!("settings-max-profile-description"),
        );
        let (deploy_title, deploy_description) = (
            t!("settings-deploy-mode"),
            t!("settings-deploy-mode-description"),
        );
        let (exec_title, exec_description) = (
            t!("settings-exec-mode"),
            t!("settings-exec-mode-description"),
        );
        let (secrets_title, secrets_description) = (
            t!("settings-agent-secrets"),
            if policy.hide_secret_values {
                t!("settings-agent-secrets-hidden-description")
            } else {
                t!("settings-agent-secrets-description")
            },
        );
        let (kinds_title, kinds_description) = (
            t!("settings-agent-other-kinds"),
            t!("settings-agent-other-kinds-description"),
        );
        let (namespaces_title, namespaces_description) = (
            t!("settings-namespaces"),
            t!("settings-namespaces-description"),
        );
        let (ttl_title, ttl_description) = (
            t!("settings-session-ttl"),
            t!("settings-session-ttl-description"),
        );
        agent = agent
            .entry(Entry::new(
                searchable(
                    "protected cluster production prod raise ceiling request never",
                    &[&protected_title, &protected_description],
                ),
                ToggleRow::new("protected-cluster", protected_title)
                    .description(protected_description)
                    .checked(policy.protected)
                    .default_value(strict.protected)
                    .on_change(policy_setter(&self.settings, &key, |p, on| {
                        p.protected = on
                    })),
            ))
            .entry(Entry::new(
                select_text(
                    "maximum profile read-only debug deploy",
                    &profile_title,
                    &profile_description,
                    &PROFILES,
                ),
                SelectRow::new("max-profile", profile_title, translated(&PROFILES))
                    .description(profile_description)
                    .selected(index_of(&PROFILES, policy.max_profile))
                    .default_index(index_of(&PROFILES, strict.max_profile))
                    .on_change(policy_setter(&self.settings, &key, |p, ix: usize| {
                        p.max_profile = PROFILES[ix].0
                    })),
            ))
            .entry(Entry::new(
                select_text(
                    "changes deploy apply dry run diff secret write",
                    &deploy_title,
                    &deploy_description,
                    &DEPLOY_MODES,
                ),
                SelectRow::new("deploy-mode", deploy_title, translated(&DEPLOY_MODES))
                    .description(deploy_description)
                    .selected(index_of(&DEPLOY_MODES, policy.deploy))
                    .default_index(index_of(&DEPLOY_MODES, strict.deploy))
                    .disabled(!profile_ok(Profile::Deploy))
                    .on_change(policy_setter(&self.settings, &key, |p, ix: usize| {
                        p.deploy = DEPLOY_MODES[ix].0
                    })),
            ))
            .entry(Entry::new(
                select_text(
                    "exec shell command",
                    &exec_title,
                    &exec_description,
                    &EXEC_MODES,
                ),
                SelectRow::new("exec-mode", exec_title, translated(&EXEC_MODES))
                    .description(exec_description)
                    .selected(index_of(&EXEC_MODES, policy.exec))
                    .default_index(index_of(&EXEC_MODES, strict.exec))
                    .disabled(!profile_ok(Profile::Debug))
                    .on_change(policy_setter(&self.settings, &key, |p, ix: usize| {
                        p.exec = EXEC_MODES[ix].0
                    })),
            ))
            .entry(Entry::new(
                select_text(
                    "secrets agent metadata checks reveal values masked",
                    &secrets_title,
                    &secrets_description,
                    &AGENT_SECRETS,
                ),
                SelectRow::new("agent-secrets", secrets_title, translated(&AGENT_SECRETS))
                    .description(secrets_description)
                    .selected(index_of(&AGENT_SECRETS, policy.effective_agent_secrets()))
                    .default_index(index_of(&AGENT_SECRETS, strict.agent_secrets))
                    .on_change(policy_setter(&self.settings, &key, |p, ix: usize| {
                        p.agent_secrets = AGENT_SECRETS[ix].0
                    })),
            ))
            .entry(Entry::new(
                searchable(
                    "allow other kinds crd custom resources flux argo allowlist",
                    &[&kinds_title, &kinds_description],
                ),
                ToggleRow::new("agent-other-kinds", kinds_title)
                    .description(kinds_description)
                    .checked(policy.agent_other_kinds)
                    .default_value(strict.agent_other_kinds)
                    .on_change(policy_setter(&self.settings, &key, |p, on| {
                        p.agent_other_kinds = on
                    })),
            ))
            .entry(Entry::new(
                select_text(
                    "namespaces session several one",
                    &namespaces_title,
                    &namespaces_description,
                    &NAMESPACE_LIMITS,
                ),
                SelectRow::new(
                    "namespaces",
                    namespaces_title,
                    translated(&NAMESPACE_LIMITS),
                )
                .description(namespaces_description)
                .selected(index_of(&NAMESPACE_LIMITS, policy.namespaces))
                .default_index(index_of(&NAMESPACE_LIMITS, strict.namespaces))
                .on_change(policy_setter(&self.settings, &key, |p, ix: usize| {
                    p.namespaces = NAMESPACE_LIMITS[ix].0
                })),
            ))
            .entry(Entry::new(
                searchable(
                    "session ttl time limit duration length",
                    &[&ttl_title, &ttl_description],
                ),
                NumberRow::new("session-ttl", ttl_title)
                    .description(ttl_description)
                    .value(policy.session_ttl_minutes)
                    .range(MIN_SESSION_TTL_MINUTES..=MAX_SESSION_TTL_MINUTES)
                    .step(15)
                    .format(format_minutes)
                    .default_value(strict.session_ttl_minutes)
                    .on_change(policy_setter(&self.settings, &key, |p, minutes| {
                        p.session_ttl_minutes = minutes
                    })),
            ));

        let (identity_title, identity_description) = (
            t!("settings-temporary-objects"),
            t!("settings-temporary-objects-description"),
        );
        let (require_title, require_description) = (
            t!("settings-require-server-enforced"),
            t!("settings-require-server-enforced-description"),
        );
        let identity = Group::new(t!("settings-group-enforcement"))
            .description(t!("settings-enforcement-description"))
            .keywords(&keywords)
            .entry(Entry::new(
                searchable(
                    "temporary objects serviceaccount role rolebinding cluster identity",
                    &[&identity_title, &identity_description],
                ),
                ToggleRow::new("temporary-objects", identity_title)
                    .description(identity_description)
                    .checked(policy.temporary_objects)
                    .default_value(strict.temporary_objects)
                    .on_change(policy_setter(&self.settings, &key, |p, on| {
                        p.temporary_objects = on
                    })),
            ))
            .entry(Entry::new(
                searchable(
                    "require server-side limits enforced impersonation serviceaccount fallback",
                    &[&require_title, &require_description],
                ),
                ToggleRow::new("require-server-enforced", require_title)
                    .description(require_description)
                    .checked(policy.require_server_enforced)
                    .default_value(strict.require_server_enforced)
                    .on_change(policy_setter(&self.settings, &key, |p, on| {
                        p.require_server_enforced = on
                    })),
            ));
        vec![context_group, secrets, agent, identity]
    }
}
