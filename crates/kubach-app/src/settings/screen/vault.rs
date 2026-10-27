//! The encrypted vault of imported kubeconfigs on the "Clusters" page
//! (ADR 0008).

use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::settings::{DEFAULT_VAULT_LOCK_MINUTES, Settings};
use kubach_i18n::t;
use kubach_ui::{NumberRow, SettingRow, ToggleRow};

use super::{Entry, Group, SettingsScreen, searchable, setter};
use crate::vault::{self, Support, VaultStatus};

impl SettingsScreen {
    /// The encrypted vault of imported kubeconfigs (ADR 0008).
    pub(super) fn vault_group(&self, settings: &Settings, cx: &mut Context<Self>) -> Group {
        let mut group = Group::new(t!("settings-group-vault"))
            .description(t!("settings-vault-group-description"));
        let Some(state) = vault::global(cx) else {
            return group;
        };
        let state = state.read(cx);
        let unlocked = state.is_unlocked();
        let busy = state.busy();
        let (status, broken) = match state.status() {
            VaultStatus::Missing => (t!("settings-vault-missing"), false),
            VaultStatus::Locked(_) => (t!("settings-vault-locked"), false),
            VaultStatus::Unlocked(_) => (t!("settings-vault-unlocked"), false),
            VaultStatus::Broken(error) => (error.to_string(), true),
            VaultStatus::Unavailable => (t!("settings-vault-unavailable"), true),
        };
        if broken {
            group = group.notice(Alert::error("vault-broken", status.clone()).small());
        }
        let has_vault = matches!(
            state.status(),
            VaultStatus::Locked(_) | VaultStatus::Unlocked(_)
        );
        let lock_button = match state.status() {
            VaultStatus::Unlocked(_) => Some(
                Button::new("vault-lock")
                    .outline()
                    .small()
                    .label(t!("settings-vault-lock-now"))
                    .on_click(|_, _, cx| vault::lock(cx)),
            ),
            VaultStatus::Locked(_) => Some(
                Button::new("vault-unlock")
                    .outline()
                    .small()
                    .label(t!("settings-vault-unlock"))
                    .on_click(|_, window, cx| vault::prompt_unlock(None, window, cx)),
            ),
            _ => None,
        };
        let theme = cx.theme();
        let rows = state.entries().iter().enumerate().map(|(ix, entry)| {
            let id = entry.id.clone();
            let actions =
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .flex_shrink_0()
                    .when(unlocked, |actions| {
                        let (update, rename, export, remove) =
                            (id.clone(), id.clone(), id.clone(), id.clone());
                        actions
                            .child(
                                Button::new(("vault-update", ix))
                                    .ghost()
                                    .xsmall()
                                    .label(t!("settings-vault-update"))
                                    .on_click(move |_, window, cx| {
                                        vault::update_from_file(update.clone(), window, cx)
                                    }),
                            )
                            .child(
                                Button::new(("vault-rename", ix))
                                    .ghost()
                                    .xsmall()
                                    .label(t!("settings-vault-rename"))
                                    .on_click(move |_, window, cx| {
                                        vault::rename(rename.clone(), window, cx)
                                    }),
                            )
                            .child(
                                Button::new(("vault-export", ix))
                                    .ghost()
                                    .xsmall()
                                    .label(t!("settings-vault-export"))
                                    .on_click(move |_, window, cx| {
                                        vault::export(export.clone(), window, cx)
                                    }),
                            )
                            .child(
                                Button::new(("vault-remove", ix))
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Close)
                                    .tooltip(t!("settings-vault-remove"))
                                    .on_click(move |_, window, cx| {
                                        vault::confirm_remove(remove.clone(), window, cx)
                                    }),
                            )
                    });
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1p5()
                .rounded_md()
                .bg(theme.muted)
                .text_xs()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(div().truncate().child(entry.name.clone()))
                        .child(
                            div()
                                .truncate()
                                .text_color(theme.muted_foreground)
                                .child(entry.contexts.join(", ")),
                        ),
                )
                .child(actions)
        });
        let list = div().flex().flex_col().gap_1().children(rows);
        let has_entries = !state.entries().is_empty();
        let title = t!("settings-vault");
        group = group.entry(Entry::new(
            searchable(
                "vault imported encrypted kubeconfig import paste clipboard lock unlock update \
                 export rename remove",
                &[
                    &title,
                    &t!("settings-vault-import-file"),
                    &t!("settings-vault-paste"),
                ],
            ),
            SettingRow::new("vault-status", title)
                .description(status)
                .control(
                    div()
                        .flex()
                        .gap_1()
                        .children(lock_button)
                        .child(
                            Button::new("vault-import")
                                .outline()
                                .small()
                                .icon(IconName::Plus)
                                .label(t!("settings-vault-import-file"))
                                .disabled(broken)
                                .on_click(|_, window, cx| vault::import_file(window, cx)),
                        )
                        .child(
                            Button::new("vault-paste")
                                .outline()
                                .small()
                                .label(t!("settings-vault-paste"))
                                .tooltip(t!("settings-vault-paste-tooltip"))
                                .disabled(broken)
                                .on_click(|_, window, cx| vault::import_clipboard(window, cx)),
                        ),
                )
                .when(has_entries, |row| row.below(list)),
        ));

        let key_mode = state.key_mode();
        let (keychain_ok, keychain_note) = match state.keychain() {
            Support::Available => (true, None),
            Support::Checking => (false, Some(t!("settings-vault-keychain-checking"))),
            Support::Unavailable(reason) => (false, Some(reason.to_string())),
        };
        let remembered = key_mode.is_some_and(|mode| mode.keychain);
        let mut description = t!("settings-vault-keychain-description");
        if let Some(note) = keychain_note {
            description = format!("{description} {note}");
        } else if !unlocked && has_vault {
            description = format!("{description} {}", t!("settings-vault-asks-password"));
        }
        let keychain_busy = busy == Some(vault::Busy::Keychain);
        let title = t!("settings-vault-keychain");
        group = group.entry(Entry::new(
            searchable(
                "vault keychain remember credential manager secret service password",
                &[&title, &description],
            ),
            ToggleRow::new(
                "vault-keychain",
                if keychain_busy {
                    t!("settings-vault-keychain-busy")
                } else {
                    title
                },
            )
            .description(description)
            .checked(remembered)
            .default_value(false)
            .disabled(!keychain_ok || busy.is_some())
            .on_change(unlocking(vault::set_keychain)),
        ));
        let method = kubach_core::vault::biometric::METHOD_NAME;
        let has_password = key_mode.is_some_and(|mode| mode.password);
        let (biometric_ok, biometric_note) = match state.biometric() {
            Support::Available => (true, None),
            Support::Checking => (
                false,
                Some(t!("settings-vault-biometric-checking", method = method)),
            ),
            Support::Unavailable(reason) => {
                (false, Some(format!("{}.", reason.trim_end_matches('.'))))
            }
        };
        let mut description = t!("settings-vault-biometric-description", method = method);
        if let Some(note) = biometric_note {
            description = format!("{description} {note}");
        } else if has_vault && !has_password {
            description = format!("{description} {}", t!("settings-vault-set-password-first"));
        } else if !unlocked && has_vault {
            description = format!("{description} {}", t!("settings-vault-asks-password"));
        }
        let title = t!("settings-vault-biometric", method = method);
        group = group.entry(Entry::new(
            searchable(
                "vault biometric touch id windows hello fingerprint face unlock",
                &[&title, &description],
            ),
            ToggleRow::new(
                "vault-biometric",
                if busy == Some(vault::Busy::Biometric) {
                    t!("settings-vault-biometric-busy", method = method)
                } else {
                    title
                },
            )
            .description(description)
            .checked(key_mode.is_some_and(|mode| mode.biometric))
            .default_value(false)
            .disabled(!biometric_ok || !has_password || busy.is_some())
            .on_change(unlocking(vault::set_biometric)),
        ));
        let (title, description) = (
            t!("settings-vault-lock-screen"),
            t!("settings-vault-lock-screen-description"),
        );
        group = group.entry(Entry::new(
            searchable("vault lock screen locks sleep", &[&title, &description]),
            ToggleRow::new("vault-lock-screen", title)
                .description(description)
                .checked(settings.clusters.vault_lock_on_screen_lock)
                .default_value(true)
                .on_change(setter(&self.settings, |s, on: bool| {
                    s.clusters.vault_lock_on_screen_lock = on
                })),
        ));
        let (title, description) = (
            t!("settings-vault-lock-idle"),
            t!("settings-vault-lock-idle-description"),
        );
        group = group.entry(Entry::new(
            searchable(
                "vault lock inactivity idle timeout minutes",
                &[&title, &description],
            ),
            NumberRow::new("vault-lock-minutes", title)
                .description(description)
                .value(settings.clusters.vault_lock_minutes)
                .range(0..=240)
                .step(5)
                .unit(t!("settings-unit-minutes"))
                .default_value(DEFAULT_VAULT_LOCK_MINUTES)
                .on_change(setter(&self.settings, |s, minutes: u32| {
                    s.clusters.vault_lock_minutes = minutes
                })),
        ));
        if has_vault {
            let title = t!("settings-vault-password");
            group = group.entry(Entry::new(
                searchable("vault password change", &[&title]),
                SettingRow::new("vault-password", title)
                    .description(if key_mode.is_some_and(|mode| mode.password) {
                        t!("settings-vault-password-description")
                    } else {
                        t!("settings-vault-no-password-description")
                    })
                    .control(
                        Button::new("vault-change-password")
                            .outline()
                            .small()
                            .label(match busy {
                                Some(vault::Busy::Password) => t!("settings-vault-encrypting"),
                                _ if key_mode.is_some_and(|mode| mode.password) => {
                                    t!("settings-vault-change-password")
                                }
                                _ => t!("settings-vault-set-password"),
                            })
                            .disabled(busy.is_some())
                            .on_click(|_, window, cx| vault::change_password(window, cx)),
                    ),
            ));
        }
        if has_vault || broken {
            let (title, description) = (
                t!("settings-vault-reset"),
                t!("settings-vault-reset-description"),
            );
            group = group.entry(Entry::new(
                searchable(
                    "vault reset delete forgot password damaged",
                    &[&title, &description],
                ),
                SettingRow::new("vault-reset", title)
                    .description(description)
                    .control(
                        Button::new("vault-reset")
                            .danger()
                            .small()
                            .label(if busy == Some(vault::Busy::Reset) {
                                t!("settings-vault-deleting")
                            } else {
                                t!("settings-vault-reset-button")
                            })
                            .disabled(busy.is_some())
                            .on_click(|_, window, cx| vault::confirm_reset(window, cx)),
                    ),
            ));
        }
        group
    }
}

/// A vault toggle that works while the vault is locked: it asks for the
/// password first and then makes the change (the vault key is needed for it).
fn unlocking(
    change: fn(bool, &mut Window, &mut App),
) -> impl Fn(bool, &mut Window, &mut App) + 'static {
    move |on: bool, window: &mut Window, cx: &mut App| {
        let locked = vault::global(cx)
            .is_some_and(|vault| matches!(vault.read(cx).status(), VaultStatus::Locked(_)));
        if !locked {
            change(on, window, cx);
            return;
        }
        vault::prompt_unlock(
            Some(std::rc::Rc::new(
                move |window: &mut Window, cx: &mut App| change(on, window, cx),
            )),
            window,
            cx,
        );
    }
}
