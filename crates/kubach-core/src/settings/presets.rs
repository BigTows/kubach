//! Resource presets saved by the user (#320): YAML with the `${name}` and
//! `${namespace}` placeholders of [`crate::templates`], offered when new
//! resources are created.

use serde::{Deserialize, Serialize};

use crate::templates;

/// The user's presets, in the order they were saved.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Presets {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<ResourcePreset>,
}

/// One saved preset.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ResourcePreset {
    pub name: String,
    /// `apiVersion` of the first document.
    pub api_version: String,
    /// `kind` of the first document; presets are grouped by it.
    pub kind: String,
    /// YAML with placeholders.
    pub text: String,
    /// Opened by "+" on the page of its kind, instead of the built-in
    /// template. At most one per kind.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub default: bool,
}

impl ResourcePreset {
    /// API group of [`kind`](Self::kind) (`""` for the core group).
    pub fn group(&self) -> &str {
        templates::group_of(&self.api_version)
    }

    pub fn is_kind(&self, group: &str, kind: &str) -> bool {
        self.group() == group && self.kind == kind
    }
}

impl Presets {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Adds `preset`, replacing one of the same kind and name; returns its
    /// index. A replaced preset keeps being the default of its kind.
    pub fn save(&mut self, mut preset: ResourcePreset) -> usize {
        let same = self.items.iter().position(|p| {
            p.is_kind(preset.group(), &preset.kind) && p.name.trim() == preset.name.trim()
        });
        match same {
            Some(ix) => {
                preset.default |= self.items[ix].default;
                self.items[ix] = preset;
                self.normalize_default(ix);
                ix
            }
            None => {
                self.items.push(preset);
                let ix = self.items.len() - 1;
                self.normalize_default(ix);
                ix
            }
        }
    }

    /// Renames the preset at `ix`; blank names are ignored.
    pub fn rename(&mut self, ix: usize, name: &str) {
        let name = name.trim();
        if let Some(preset) = self.items.get_mut(ix)
            && !name.is_empty()
        {
            preset.name = name.to_owned();
        }
    }

    pub fn remove(&mut self, ix: usize) {
        if ix < self.items.len() {
            self.items.remove(ix);
        }
    }

    /// Makes the preset at `ix` the default of its kind (or not).
    pub fn set_default(&mut self, ix: usize, on: bool) {
        if let Some(preset) = self.items.get_mut(ix) {
            preset.default = on;
            self.normalize_default(ix);
        }
    }

    /// The default preset of a kind.
    pub fn default_for(&self, group: &str, kind: &str) -> Option<&ResourcePreset> {
        self.items
            .iter()
            .find(|p| p.default && p.is_kind(group, kind))
    }

    /// When the preset at `ix` is a default, no other of its kind is.
    fn normalize_default(&mut self, ix: usize) {
        let Some(preset) = self.items.get(ix) else {
            return;
        };
        if !preset.default {
            return;
        }
        let (group, kind) = (preset.group().to_owned(), preset.kind.clone());
        for (other, p) in self.items.iter_mut().enumerate() {
            if other != ix && p.is_kind(&group, &kind) {
                p.default = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(name: &str, api_version: &str, kind: &str) -> ResourcePreset {
        ResourcePreset {
            name: name.into(),
            api_version: api_version.into(),
            kind: kind.into(),
            text: format!("apiVersion: {api_version}\nkind: {kind}\n"),
            default: false,
        }
    }

    #[test]
    fn one_default_per_kind() {
        let mut presets = Presets::default();
        let a = presets.save(preset("a", "apps/v1", "Deployment"));
        let b = presets.save(preset("b", "apps/v1", "Deployment"));
        let c = presets.save(preset("c", "v1", "ConfigMap"));
        presets.set_default(a, true);
        presets.set_default(c, true);
        assert_eq!(presets.default_for("apps", "Deployment").unwrap().name, "a");
        presets.set_default(b, true);
        assert_eq!(presets.default_for("apps", "Deployment").unwrap().name, "b");
        assert!(!presets.items[a].default);
        assert_eq!(presets.default_for("", "ConfigMap").unwrap().name, "c");
        presets.set_default(b, false);
        assert!(presets.default_for("apps", "Deployment").is_none());
    }

    #[test]
    fn save_replaces_same_name_and_kind() {
        let mut presets = Presets::default();
        presets.save(preset("web", "apps/v1", "Deployment"));
        presets.set_default(0, true);
        let mut changed = preset("web", "apps/v1", "Deployment");
        changed.text = "changed".into();
        assert_eq!(presets.save(changed), 0);
        assert_eq!(presets.items.len(), 1);
        assert_eq!(presets.items[0].text, "changed");
        assert!(presets.items[0].default);
        presets.save(preset("web", "v1", "Service"));
        assert_eq!(presets.items.len(), 2);
    }

    #[test]
    fn rename_and_remove() {
        let mut presets = Presets::default();
        presets.save(preset("a", "v1", "Service"));
        presets.rename(0, "  b ");
        assert_eq!(presets.items[0].name, "b");
        presets.rename(0, "   ");
        assert_eq!(presets.items[0].name, "b");
        presets.remove(5);
        presets.remove(0);
        assert!(presets.is_empty());
    }

    #[test]
    fn round_trips_through_toml() {
        let mut settings = crate::settings::Settings::default();
        let mut p = preset("web", "apps/v1", "Deployment");
        p.text = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: ${name}\n".into();
        p.default = true;
        settings.presets.save(p);
        let text = toml::to_string(&settings).unwrap();
        let back: crate::settings::Settings = toml::from_str(&text).unwrap();
        assert_eq!(back, settings);
        let empty = toml::to_string(&crate::settings::Settings::default()).unwrap();
        assert!(!empty.contains("presets"));
    }
}
