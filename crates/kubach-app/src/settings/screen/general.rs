//! The "General" page: appearance, language, the terminal and the disk
//! cache (#177).

use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Disableable as _, Sizable as _};
use gpui_kit::*;
use kubach_core::disk_cache;
use kubach_core::settings::{Language, Settings, ThemePreference};
use kubach_core::streams::node_shell;
use kubach_i18n::t;
use kubach_ui::{SelectRow, SettingRow, ToggleRow, format_bytes};

use super::{Entry, Group, SettingsScreen, index_of, labels, searchable, setter};
use crate::images::RemoteImages;

const THEMES: [(ThemePreference, &str); 3] = [
    (ThemePreference::Dark, "Dark"),
    (ThemePreference::Light, "Light"),
    (ThemePreference::System, "System"),
];

/// Name of `theme` in the interface language.
fn theme_label(theme: ThemePreference) -> String {
    match theme {
        ThemePreference::Dark => t!("settings-theme-dark"),
        ThemePreference::Light => t!("settings-theme-light"),
        ThemePreference::System => t!("settings-theme-system"),
    }
}

/// Languages under their own names, whatever the interface language.
const LANGUAGES: [(Language, &str); 3] = [
    (Language::En, "English"),
    (Language::Ru, "Русский"),
    (Language::ZhHans, "简体中文"),
];

/// Image and namespace inputs of node shells (#318); empty takes the
/// default.
pub(super) struct NodeShellForm {
    image: Entity<InputState>,
    namespace: Entity<InputState>,
    _subscriptions: [Subscription; 2],
}

impl NodeShellForm {
    pub(super) fn new(
        settings: &Settings,
        window: &mut Window,
        cx: &mut Context<SettingsScreen>,
    ) -> Self {
        let general = &settings.general;
        let input = |placeholder: &str,
                     value: &Option<String>,
                     window: &mut Window,
                     cx: &mut Context<SettingsScreen>| {
            let value = value.clone().unwrap_or_default();
            let placeholder = placeholder.to_owned();
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        let image = input(
            node_shell::DEFAULT_IMAGE,
            &general.node_shell_image,
            window,
            cx,
        );
        let namespace = input(
            node_shell::DEFAULT_NAMESPACE,
            &general.node_shell_namespace,
            window,
            cx,
        );
        let save = |apply: fn(&mut Settings, Option<String>)| {
            move |this: &mut SettingsScreen,
                  input: &Entity<InputState>,
                  event: &InputEvent,
                  _: &mut Window,
                  cx: &mut Context<SettingsScreen>| {
                if let InputEvent::Change = event {
                    let value = input.read(cx).value().trim().to_owned();
                    let value = (!value.is_empty()).then_some(value);
                    this.settings.update(cx, |settings, cx| {
                        settings.edit(cx, |settings| apply(settings, value))
                    });
                }
            }
        };
        let subscriptions = [
            cx.subscribe_in(&image, window, save(|s, v| s.general.node_shell_image = v)),
            cx.subscribe_in(
                &namespace,
                window,
                save(|s, v| s.general.node_shell_namespace = v),
            ),
        ];
        Self {
            image,
            namespace,
            _subscriptions: subscriptions,
        }
    }

    /// Shows values changed elsewhere (reset, the settings file).
    pub(super) fn sync(&self, settings: &Settings, window: &mut Window, cx: &mut App) {
        let general = &settings.general;
        for (input, stored) in [
            (&self.image, &general.node_shell_image),
            (&self.namespace, &general.node_shell_namespace),
        ] {
            let stored = stored.as_deref().unwrap_or("");
            // Trimmed on save: leave spaces being typed alone.
            if input.read(cx).value().trim() != stored {
                input.update(cx, |input, cx| {
                    input.set_value(stored.to_owned(), window, cx)
                });
            }
        }
    }
}

impl SettingsScreen {
    /// Counts the disk cache off the main thread.
    pub(super) fn count_cache(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = disk_cache::dir() else {
            self.cache_size = Some(0);
            return;
        };
        let size = cx.background_spawn(async move { disk_cache::size(&dir) });
        cx.spawn(async move |this, cx| {
            let size = size.await;
            this.update(cx, |this, cx| {
                this.cache_size = Some(size);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Removes the whole disk cache; images are fetched again when drawn.
    fn clear_cache(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = disk_cache::dir() else {
            return;
        };
        self.cache_size = None;
        let cleared = cx.background_spawn(async move { disk_cache::clear(&dir) });
        cx.spawn(async move |this, cx| {
            let result = cleared.await;
            cx.update(|cx| {
                if let Some(images) = RemoteImages::global(cx) {
                    images.update(cx, |images, cx| images.forget_all(cx));
                }
            });
            this.update(cx, |this, cx| {
                this.cache_error = result.err().map(|error| error.to_string());
                this.count_cache(cx);
            })
            .ok();
        })
        .detach();
    }

    fn storage(&self, cx: &mut Context<Self>) -> Group {
        let description = match (self.cache_size, disk_cache::dir()) {
            (None, _) => t!("settings-cache-counting"),
            (Some(size), Some(dir)) => t!(
                "settings-cache-description-in",
                size = format_bytes(size),
                dir = dir.display().to_string()
            ),
            (Some(size), None) => t!("settings-cache-description", size = format_bytes(size)),
        };
        let description = match &self.cache_error {
            Some(error) => format!(
                "{description} {}",
                t!("settings-cache-clear-failed", error = error)
            ),
            None => description,
        };
        let row = SettingRow::new("clear-cache", t!("settings-cache"))
            .description(description)
            .control(
                Button::new("clear-cache-button")
                    .outline()
                    .small()
                    .label(t!("settings-clear-cache"))
                    .disabled(self.cache_size.is_none_or(|size| size == 0))
                    .on_click(cx.listener(|this, _, _, cx| this.clear_cache(cx))),
            );
        Group::new(t!("settings-group-storage")).entry(Entry::new(
            searchable(
                "cache clear clean storage disk images icons readme",
                &[&t!("settings-cache"), &t!("settings-clear-cache")],
            ),
            row,
        ))
    }

    pub(super) fn general(&self, settings: &Settings, cx: &mut Context<Self>) -> Vec<Group> {
        let theme = ThemePreference::default();
        let language = Language::default();
        let (theme_title, theme_description) =
            (t!("settings-theme"), t!("settings-theme-description"));
        let (language_title, language_description) =
            (t!("settings-language"), t!("settings-language-description"));
        let (namespace_title, namespace_description) = (
            t!("settings-node-shell-namespace"),
            t!("settings-node-shell-namespace-description"),
        );
        let (image_title, image_description) = (
            t!("settings-node-shell-image"),
            t!("settings-node-shell-image-description"),
        );
        let (clipboard_title, clipboard_description) = (
            t!("settings-terminal-clipboard"),
            t!("settings-terminal-clipboard-description"),
        );
        let (debug_title, debug_description) = (
            t!("settings-confirm-debug"),
            t!("settings-confirm-debug-description"),
        );
        vec![
            Group::new(t!("settings-group-appearance")).entry(Entry::new(
                searchable(
                    "theme dark light system appearance",
                    &[&theme_title, &theme_description],
                ),
                SelectRow::new(
                    "theme",
                    theme_title,
                    THEMES.map(|(theme, _)| theme_label(theme)),
                )
                .description(theme_description)
                .selected(index_of(&THEMES, settings.general.theme))
                .default_index(index_of(&THEMES, theme))
                .on_change(setter(&self.settings, |s, ix: usize| {
                    s.general.theme = THEMES[ix].0
                })),
            )),
            Group::new(t!("settings-group-language")).entry(Entry::new(
                searchable(
                    "language interface english russian chinese язык 语言 中文",
                    &[&language_title],
                ),
                SelectRow::new("language", language_title, labels(&LANGUAGES))
                    .description(language_description)
                    .selected(index_of(&LANGUAGES, settings.general.language))
                    .default_index(index_of(&LANGUAGES, language))
                    .on_change(setter(&self.settings, |s, ix: usize| {
                        s.general.language = LANGUAGES[ix].0
                    })),
            )),
            Group::new(t!("settings-group-terminal"))
                .entry(Entry::new(
                    searchable(
                        "node shell namespace pod privileged nsenter",
                        &[&namespace_title, &namespace_description],
                    ),
                    SettingRow::new("node-shell-namespace", namespace_title)
                        .description(namespace_description)
                        .reset(settings.general.node_shell_namespace.is_some(), {
                            let reset = setter(&self.settings, |s, ()| {
                                s.general.node_shell_namespace = None
                            });
                            move |window, cx| reset((), window, cx)
                        })
                        .control(
                            div()
                                .w(px(200.))
                                .child(Input::new(&self.node_shell.namespace).small()),
                        ),
                ))
                .entry(Entry::new(
                    searchable(
                        "node shell image busybox alpine nsenter",
                        &[&image_title, &image_description],
                    ),
                    SettingRow::new("node-shell-image", image_title)
                        .description(image_description)
                        .reset(settings.general.node_shell_image.is_some(), {
                            let reset =
                                setter(&self.settings, |s, ()| s.general.node_shell_image = None);
                            move |window, cx| reset((), window, cx)
                        })
                        .control(
                            div()
                                .w(px(200.))
                                .child(Input::new(&self.node_shell.image).small()),
                        ),
                ))
                .entry(Entry::new(
                    searchable(
                        "terminal shell debug clipboard osc 52 copy буфер обмена",
                        &[&clipboard_title, &clipboard_description],
                    ),
                    ToggleRow::new("terminal-clipboard-write", clipboard_title)
                        .description(clipboard_description)
                        .checked(settings.general.terminal_clipboard_write)
                        .default_value(false)
                        .on_change(setter(&self.settings, |s, on: bool| {
                            s.general.terminal_clipboard_write = on
                        })),
                ))
                .entry(Entry::new(
                    searchable(
                        "debug ephemeral container confirm ask подтверждение отладка",
                        &[&debug_title, &debug_description],
                    ),
                    ToggleRow::new("confirm-debug", debug_title)
                        .description(debug_description)
                        .checked(!settings.general.skip_debug_confirm)
                        .default_value(true)
                        .on_change(setter(&self.settings, |s, on: bool| {
                            s.general.skip_debug_confirm = !on
                        })),
                )),
            self.storage(cx),
        ]
    }
}
