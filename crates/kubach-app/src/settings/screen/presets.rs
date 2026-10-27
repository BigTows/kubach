//! The "Presets" page (#320): resource presets saved from the YAML editor,
//! grouped by kind; rename, make the default of a kind, delete.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{IconName, Sizable as _};
use gpui_kit::*;
use kubach_core::settings::{ResourcePreset, Settings};
use kubach_i18n::t;
use kubach_ui::SettingRow;

use super::{Entry, Group, SettingsScreen, searchable, setter};

/// Name inputs of the presets, one per preset in settings order.
#[derive(Default)]
pub(super) struct PresetsForm {
    names: Vec<Entity<InputState>>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsScreen {
    /// Rebuilds the name inputs when presets were added or removed, and
    /// shows renames made elsewhere.
    pub(super) fn sync_presets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let names: Vec<String> = self
            .settings
            .read(cx)
            .get()
            .presets
            .items
            .iter()
            .map(|preset| preset.name.clone())
            .collect();
        if names.len() != self.presets.names.len() {
            self.presets._subscriptions.clear();
            self.presets.names = names
                .iter()
                .enumerate()
                .map(|(ix, name)| {
                    let input = cx.new(|cx| {
                        InputState::new(window, cx)
                            .placeholder(t!("settings-preset-name-placeholder"))
                            .default_value(name.clone())
                    });
                    let subscription =
                        cx.subscribe_in(&input, window, move |this, input, event, _, cx| {
                            if let InputEvent::Change = event {
                                let value = input.read(cx).value().to_string();
                                this.settings.update(cx, |settings, cx| {
                                    settings.edit(cx, |s| s.presets.rename(ix, &value))
                                });
                            }
                        });
                    self.presets._subscriptions.push(subscription);
                    input
                })
                .collect();
            return;
        }
        for (input, name) in self.presets.names.iter().zip(&names) {
            let typed = input.read(cx).value();
            // A cleared field is being retyped: blank names are not saved.
            if typed.trim() != name && !typed.trim().is_empty() {
                let name = name.clone();
                input.update(cx, |input, cx| input.set_value(name, window, cx));
            }
        }
    }

    pub(super) fn presets_page(&self, settings: &Settings) -> Vec<Group> {
        let items = &settings.presets.items;
        if items.is_empty() {
            let (title, description) = (
                t!("settings-presets-none"),
                t!("settings-presets-none-description"),
            );
            return vec![Group::new(t!("settings-section-presets")).entry(Entry::new(
                searchable("presets templates none empty save", &[&title, &description]),
                SettingRow::new("presets-none", title).description(description),
            ))];
        }
        let mut kinds: Vec<(&str, &str)> = Vec::new();
        for preset in items {
            if !kinds.contains(&(preset.api_version.as_str(), preset.kind.as_str())) {
                kinds.push((preset.api_version.as_str(), preset.kind.as_str()));
            }
        }
        kinds
            .into_iter()
            .map(|(api_version, kind)| {
                let mut group = Group::new(kind.to_owned())
                    .description(t!(
                        "settings-presets-kind-description",
                        api_version = api_version,
                        kind = kind
                    ))
                    .keywords(format!("presets templates {api_version}"));
                for (ix, preset) in items.iter().enumerate() {
                    if preset.api_version != api_version || preset.kind != kind {
                        continue;
                    }
                    group = group.entry(Entry::new(
                        format!(
                            "{} {kind} preset rename delete default {} {}",
                            preset.name,
                            t!("settings-preset-default"),
                            t!("settings-preset-delete")
                        ),
                        self.preset_row(ix, preset),
                    ));
                }
                group
            })
            .collect()
    }

    fn preset_row(&self, ix: usize, preset: &ResourcePreset) -> impl IntoElement {
        let default = preset.default;
        let name = self.presets.names.get(ix).cloned();
        let set_default = setter(&self.settings, move |s, on: bool| {
            s.presets.set_default(ix, on)
        });
        let remove = setter(&self.settings, move |s, ()| s.presets.remove(ix));
        let control = div()
            .flex()
            .items_center()
            .gap_3()
            .children(name.map(|input| div().w(px(220.)).child(Input::new(&input).small())))
            .child(
                Checkbox::new(("preset-default", ix))
                    .label(t!("settings-preset-default"))
                    .checked(default)
                    .on_click(move |checked: &bool, window, cx| set_default(*checked, window, cx)),
            )
            .child(
                Button::new(("preset-delete", ix))
                    .ghost()
                    .small()
                    .icon(IconName::Delete)
                    .tooltip(t!("settings-preset-delete"))
                    .on_click(move |_, window, cx| remove((), window, cx)),
            );
        let lines = preset.text.lines().count();
        SettingRow::new(ElementId::from(("preset", ix)), preset.name.clone())
            .description(if default {
                t!(
                    "settings-preset-lines-default",
                    lines = lines,
                    kind = preset.kind.as_str()
                )
            } else {
                t!("settings-preset-lines", lines = lines)
            })
            .control(control)
    }
}
