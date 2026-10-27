//! Arrangement of the cluster hotbar: order, aliases, colors, hidden contexts.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

/// Order, aliases, colors and hidden contexts of the cluster hotbar, keyed
/// by context name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotbarPrefs {
    /// Contexts in the user's order; ones not listed follow in kubeconfig order.
    pub order: Vec<String>,
    /// Short tile labels.
    pub aliases: BTreeMap<String, String>,
    /// Tile color names (the UI's palette); others get a color from the name.
    pub colors: BTreeMap<String, String>,
    pub hidden: BTreeSet<String>,
}

impl HotbarPrefs {
    /// All of `names` (kubeconfig order) in the user's order.
    pub fn ordered(&self, names: &[String]) -> Vec<String> {
        let mut ordered: Vec<String> = self
            .order
            .iter()
            .filter(|name| names.contains(name))
            .cloned()
            .collect();
        for name in names {
            if !ordered.contains(name) {
                ordered.push(name.clone());
            }
        }
        ordered
    }

    /// Contexts shown as tiles, in order.
    pub fn visible(&self, names: &[String]) -> Vec<String> {
        let mut ordered = self.ordered(names);
        ordered.retain(|name| !self.hidden.contains(name));
        ordered
    }

    /// Hidden contexts that still exist, in order.
    pub fn hidden_of(&self, names: &[String]) -> Vec<String> {
        let mut ordered = self.ordered(names);
        ordered.retain(|name| self.hidden.contains(name));
        ordered
    }

    /// Puts `dragged` where `target` is; the rest shift.
    pub fn move_to(&mut self, names: &[String], dragged: &str, target: &str) {
        let mut ordered = self.ordered(names);
        let (Some(from), Some(to)) = (
            ordered.iter().position(|name| name == dragged),
            ordered.iter().position(|name| name == target),
        ) else {
            return;
        };
        let name = ordered.remove(from);
        ordered.insert(to, name);
        self.order = ordered;
    }

    pub fn set_hidden(&mut self, name: &str, hidden: bool) {
        if hidden {
            self.hidden.insert(name.to_owned());
        } else {
            self.hidden.remove(name);
        }
    }

    pub fn alias(&self, name: &str) -> Option<&str> {
        self.aliases.get(name).map(String::as_str)
    }

    /// Sets the alias; blank clears it.
    pub fn set_alias(&mut self, name: &str, alias: &str) {
        let alias = alias.trim();
        if alias.is_empty() {
            self.aliases.remove(name);
        } else {
            self.aliases.insert(name.to_owned(), alias.to_owned());
        }
    }

    pub fn color(&self, name: &str) -> Option<&str> {
        self.colors.get(name).map(String::as_str)
    }

    pub fn set_color(&mut self, name: &str, color: &str) {
        self.colors.insert(name.to_owned(), color.to_owned());
    }

    /// Chosen tile color of `name`; `None` when none is set or the stored
    /// value is not a color (the tile then gets its default color).
    pub fn tile_color(&self, name: &str) -> Option<TileColorChoice> {
        self.color(name).and_then(TileColorChoice::parse)
    }

    /// Sets the tile color of `name`; `None` goes back to the default.
    pub fn set_tile_color(&mut self, name: &str, color: Option<TileColorChoice>) {
        match color {
            Some(color) => self.set_color(name, &color.to_string()),
            None => {
                self.colors.remove(name);
            }
        }
    }
}

/// Named tile colors. The UI draws them with colors of the current theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColorPreset {
    Blue,
    Teal,
    Orange,
    Purple,
    Pink,
    Gray,
}

impl ColorPreset {
    pub const ALL: [ColorPreset; 6] = [
        ColorPreset::Blue,
        ColorPreset::Teal,
        ColorPreset::Orange,
        ColorPreset::Purple,
        ColorPreset::Pink,
        ColorPreset::Gray,
    ];

    /// Name stored in the settings file.
    pub fn name(self) -> &'static str {
        match self {
            ColorPreset::Blue => "blue",
            ColorPreset::Teal => "teal",
            ColorPreset::Orange => "orange",
            ColorPreset::Purple => "purple",
            ColorPreset::Pink => "pink",
            ColorPreset::Gray => "gray",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|preset| preset.name() == name)
    }
}

/// An opaque sRGB color, written as `#rrggbb`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Parses `#rrggbb` (any case); anything else is `None`.
    pub fn parse_hex(text: &str) -> Option<Self> {
        let hex = text.strip_prefix('#')?;
        if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        u32::from_str_radix(hex, 16).ok().map(Self::from_u32)
    }

    /// From `0xRRGGBB`; higher bits are ignored.
    pub const fn from_u32(value: u32) -> Self {
        Self::new((value >> 16) as u8, (value >> 8) as u8, value as u8)
    }

    /// As `0xRRGGBB`.
    pub const fn to_u32(self) -> u32 {
        (self.r as u32) << 16 | (self.g as u32) << 8 | self.b as u32
    }
}

impl fmt::Display for Rgb {
    /// `#rrggbb`, lowercase.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

/// Tile color chosen by the user: a preset (drawn with the theme's color)
/// or a custom color (user data, drawn as given).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TileColorChoice {
    Preset(ColorPreset),
    Custom(Rgb),
}

impl TileColorChoice {
    /// Reads a stored value: a preset name (`"teal"`) or `#rrggbb`.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        ColorPreset::from_name(text)
            .map(Self::Preset)
            .or_else(|| Rgb::parse_hex(text).map(Self::Custom))
    }
}

impl fmt::Display for TileColorChoice {
    /// The stored value: the preset name or `#rrggbb`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Preset(preset) => f.write_str(preset.name()),
            Self::Custom(rgb) => rgb.fmt(f),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|&name| name.to_owned()).collect()
    }

    #[test]
    fn order_follows_prefs_then_kubeconfig() {
        let all = names(&["a", "b", "c", "d"]);
        let prefs = HotbarPrefs {
            order: names(&["c", "gone", "a"]),
            ..Default::default()
        };
        assert_eq!(prefs.ordered(&all), names(&["c", "a", "b", "d"]));
    }

    #[test]
    fn drag_moves_to_the_target_slot() {
        let all = names(&["a", "b", "c", "d"]);
        let mut prefs = HotbarPrefs::default();
        prefs.move_to(&all, "a", "c");
        assert_eq!(prefs.ordered(&all), names(&["b", "c", "a", "d"]));
        prefs.move_to(&all, "d", "b");
        assert_eq!(prefs.ordered(&all), names(&["d", "b", "c", "a"]));
        // Unknown names change nothing.
        prefs.move_to(&all, "x", "b");
        assert_eq!(prefs.ordered(&all), names(&["d", "b", "c", "a"]));
    }

    #[test]
    fn hidden_contexts_leave_the_bar() {
        let all = names(&["a", "b", "c"]);
        let mut prefs = HotbarPrefs::default();
        prefs.set_hidden("b", true);
        prefs.set_hidden("gone", true);
        assert_eq!(prefs.visible(&all), names(&["a", "c"]));
        assert_eq!(prefs.hidden_of(&all), names(&["b"]));
        prefs.set_hidden("b", false);
        assert_eq!(prefs.visible(&all), all);
        assert!(prefs.hidden_of(&all).is_empty());
    }

    #[test]
    fn stored_under_clusters_with_defaults_for_older_files() {
        use crate::settings::{DEFAULT_KEEP_CONNECTED, Settings};

        let mut settings = Settings::default();
        assert_eq!(settings.clusters.keep_connected, DEFAULT_KEEP_CONNECTED);
        settings.clusters.keep_connected = 5;
        let hotbar = &mut settings.clusters.hotbar;
        hotbar.move_to(&names(&["a", "b"]), "b", "a");
        hotbar.set_alias("a", "dev");
        hotbar.set_color("b", "pink");
        hotbar.set_hidden("c", true);
        let text = toml::to_string(&settings).unwrap();
        assert_eq!(toml::from_str::<Settings>(&text).unwrap(), settings);

        // A file written before these keys existed.
        let old: Settings =
            toml::from_str("version = 1\n[clusters]\nstartup_context = \"a\"").unwrap();
        assert_eq!(old.clusters.keep_connected, DEFAULT_KEEP_CONNECTED);
        assert_eq!(old.clusters.hotbar, HotbarPrefs::default());

        settings.clusters.keep_connected = 0;
        assert_eq!(settings.clusters.keep_connected(), 1);
    }

    #[test]
    fn tile_colors_are_presets_or_hex() {
        assert_eq!(
            TileColorChoice::parse("teal"),
            Some(TileColorChoice::Preset(ColorPreset::Teal))
        );
        assert_eq!(
            TileColorChoice::parse("#FF8800"),
            Some(TileColorChoice::Custom(Rgb::new(0xff, 0x88, 0x00)))
        );
        for garbage in [
            "",
            "Teal",
            "ultraviolet",
            "#ff88",
            "#ff88001",
            "ff8800",
            "#gg8800",
            "#+f8800",
        ] {
            assert_eq!(TileColorChoice::parse(garbage), None, "{garbage:?}");
        }
        for preset in ColorPreset::ALL {
            let choice = TileColorChoice::Preset(preset);
            assert_eq!(TileColorChoice::parse(&choice.to_string()), Some(choice));
        }
        let custom = TileColorChoice::Custom(Rgb::from_u32(0x0a_1b_2c));
        assert_eq!(custom.to_string(), "#0a1b2c");
        assert_eq!(TileColorChoice::parse(&custom.to_string()), Some(custom));
        assert_eq!(Rgb::from_u32(0x0a_1b_2c).to_u32(), 0x0a_1b_2c);
    }

    #[test]
    fn tile_color_is_stored_next_to_preset_names() {
        let mut prefs = HotbarPrefs::default();
        assert_eq!(prefs.tile_color("a"), None);
        // Files written before custom colors keep their preset names.
        prefs.set_color("a", "pink");
        assert_eq!(
            prefs.tile_color("a"),
            Some(TileColorChoice::Preset(ColorPreset::Pink))
        );
        prefs.set_tile_color("b", Some(TileColorChoice::Custom(Rgb::new(1, 2, 3))));
        assert_eq!(prefs.color("b"), Some("#010203"));
        // Garbage reads as no color.
        prefs.set_color("c", "ultraviolet");
        assert_eq!(prefs.tile_color("c"), None);

        let text = toml::to_string(&prefs).unwrap();
        assert!(text.contains("a = \"pink\""), "{text}");
        assert!(text.contains("b = \"#010203\""), "{text}");
        assert_eq!(toml::from_str::<HotbarPrefs>(&text).unwrap(), prefs);

        prefs.set_tile_color("b", None);
        assert_eq!(prefs.color("b"), None);
    }

    #[test]
    fn blank_alias_clears_it() {
        let mut prefs = HotbarPrefs::default();
        prefs.set_alias("staging", "  stg ");
        assert_eq!(prefs.alias("staging"), Some("stg"));
        prefs.set_alias("staging", " ");
        assert_eq!(prefs.alias("staging"), None);
    }
}
