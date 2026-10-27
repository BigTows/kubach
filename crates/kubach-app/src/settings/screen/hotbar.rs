//! The hotbar tile of the selected context on the per-cluster page (#173,
//! #278, #402): its label and color. Presets come from the theme; a custom
//! color is picked with a color picker and stored as `#rrggbb` next to the
//! preset names.

use gpui_kit::component::button::Button;
use gpui_kit::component::color_picker::{ColorPickerEvent, ColorPickerState, ColorSelect};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Sizable as _};
use gpui_kit::*;
use kubach_core::settings::{ColorPreset, Rgb, Settings, TileColorChoice};
use kubach_i18n::t;
use kubach_ui::{SettingRow, TilePreview};

use super::{Anchor, Entry, Group, SettingsScreen, searchable, setter};
use crate::hotbar::{ALIAS_MAX, default_color, preset_color, tile_color, tile_label};

/// The picker for the custom color of the selected context's tile and the
/// input of its label.
pub(super) struct TileColorForm {
    picker: Entity<ColorPickerState>,
    /// Alias shown on the tile instead of the initials.
    label: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl TileColorForm {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsScreen>) -> Self {
        let picker = cx.new(|cx| ColorPickerState::new(window, cx));
        // Only user edits emit: `sync_tile_color` sets the value silently.
        let subscription = cx.subscribe(&picker, |this, _, event: &ColorPickerEvent, cx| {
            let ColorPickerEvent::Change(Some(color)) = event else {
                return;
            };
            let Some(name) = this.selected_context() else {
                return;
            };
            let choice = TileColorChoice::Custom(to_rgb(*color));
            this.settings.update(cx, |settings, cx| {
                settings.edit(cx, |s| {
                    s.clusters.hotbar.set_tile_color(&name, Some(choice))
                })
            });
        });
        let label = cx.new(|cx| InputState::new(window, cx));
        let placeholder = crate::i18n::localized_placeholder(
            &label,
            || t!("settings-tile-initials-placeholder"),
            window,
            cx,
        );
        let label_subscription = cx.subscribe(&label, |this, input, event: &InputEvent, cx| {
            let InputEvent::Change = event else {
                return;
            };
            let Some(name) = this.selected_context() else {
                return;
            };
            let alias = input.read(cx).value().to_string();
            this.settings.update(cx, |settings, cx| {
                settings.edit(cx, |s| s.clusters.hotbar.set_alias(&name, &alias))
            });
        });
        let mut subscriptions = vec![subscription, label_subscription];
        subscriptions.extend(placeholder);
        Self {
            picker,
            label,
            _subscriptions: subscriptions,
        }
    }
}

/// The opaque sRGB color of `color`; alpha is dropped.
fn to_rgb(color: Hsla) -> Rgb {
    let rgba = color.to_rgb();
    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;
    Rgb::new(channel(rgba.r), channel(rgba.g), channel(rgba.b))
}

impl SettingsScreen {
    /// Shows the selected context's color in the picker without emitting a
    /// change. Skipped while it already shows that color, so a drag in the
    /// picker is not fought by the save it causes.
    pub(super) fn sync_tile_color(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.selected_context() else {
            return;
        };
        let prefs = &self.settings.read(cx).get().clusters.hotbar;
        let color = tile_color(prefs, &name).color(cx);
        // Trimmed on save: leave spaces being typed alone.
        let alias = prefs.alias(&name).unwrap_or("").to_owned();
        let label = self.tile_color.label.clone();
        if label.read(cx).value().trim() != alias {
            label.update(cx, |label, cx| label.set_value(alias, window, cx));
        }
        let picker = self.tile_color.picker.clone();
        if picker.read(cx).value().map(to_rgb) == Some(to_rgb(color)) {
            return;
        }
        picker.update(cx, |picker, cx| picker.set_value(color, window, cx));
    }

    /// Color of the tile of `name` in the hotbar.
    pub(super) fn tile_color_group(&self, name: &str, settings: &Settings) -> Group {
        let prefs = &settings.clusters.hotbar;
        let stored = prefs.tile_color(name);
        let color = tile_color(prefs, name);
        let default = default_color(name);
        let label = tile_label(prefs, name);

        let set = |choice: Option<TileColorChoice>| {
            let name = name.to_owned();
            setter(&self.settings, move |s, ()| {
                s.clusters.hotbar.set_tile_color(&name, choice)
            })
        };
        let reset = set(None);
        let reset_button = {
            let reset = set(None);
            Button::new("hotbar-color-reset")
                .outline()
                .small()
                .label(t!("settings-tile-reset"))
                .disabled(stored.is_none())
                .on_click(move |_, window, cx| reset((), window, cx))
        };
        let preview_title = t!("settings-tile-preview");
        let preview = SettingRow::new("hotbar-preview", preview_title.clone())
            .description(match stored {
                None => t!("settings-tile-default-color", color = default.label()),
                Some(TileColorChoice::Preset(preset)) => {
                    t!(
                        "settings-tile-preset-color",
                        color = preset_color(preset).label()
                    )
                }
                Some(TileColorChoice::Custom(rgb)) => {
                    t!("settings-tile-custom-color-value", rgb = rgb.to_string())
                }
            })
            .reset(stored.is_some(), move |window, cx| reset((), window, cx))
            .control(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(TilePreview::new(label.clone(), color))
                    .child(TilePreview::new(label.clone(), color).active(true))
                    .child(reset_button),
            );

        let presets_title = t!("settings-tile-presets");
        let presets =
            SettingRow::new("hotbar-presets", presets_title.clone())
                .description(t!("settings-tile-presets-description"))
                .control(div().flex().flex_wrap().gap_2().children(
                    ColorPreset::ALL.into_iter().map(|preset| {
                        let option = preset_color(preset);
                        let apply = set(Some(TileColorChoice::Preset(preset)));
                        let tooltip: SharedString = option.label().into();
                        div()
                            .id(SharedString::from(format!(
                                "hotbar-preset-{}",
                                preset.name()
                            )))
                            .rounded_lg()
                            .cursor_pointer()
                            .tooltip(move |window, cx| {
                                Tooltip::new(tooltip.clone()).build(window, cx)
                            })
                            .on_click(move |_, window, cx| apply((), window, cx))
                            .child(TilePreview::new(label.clone(), option).active(option == color))
                    }),
                ));

        let (custom_title, custom_description) = (
            t!("settings-tile-custom"),
            t!("settings-tile-custom-description"),
        );
        let custom = SettingRow::new("hotbar-custom", custom_title.clone())
            .description(custom_description.clone())
            .control(
                div()
                    .w(px(180.))
                    .child(ColorSelect::new(&self.tile_color.picker).small()),
            );

        let (label_title, label_description) = (
            t!("settings-tile-label"),
            t!("settings-tile-label-description", max = ALIAS_MAX),
        );
        let label_row = SettingRow::new("hotbar-label", label_title.clone())
            .description(label_description.clone())
            .control(
                div()
                    .w(px(120.))
                    .child(Input::new(&self.tile_color.label).small()),
            );

        Group::new(t!("settings-group-hotbar-tile"))
            .description(t!("settings-hotbar-tile-description"))
            .keywords(format!("hotbar {name}"))
            .anchor(Anchor::TileColor)
            .entry(Entry::new(
                searchable(
                    "tile label alias rename name initials short",
                    &[&label_title, &label_description],
                ),
                label_row,
            ))
            .entry(Entry::new(
                searchable("tile color preview reset default", &[&preview_title]),
                preview,
            ))
            .entry(Entry::new(
                searchable(
                    "tile color presets blue teal orange purple pink gray theme",
                    &[&presets_title],
                ),
                presets,
            ))
            .entry(Entry::new(
                searchable(
                    "tile color custom picker hex rgb",
                    &[&custom_title, &custom_description],
                ),
                custom,
            ))
    }
}
