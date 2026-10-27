use std::collections::HashMap;
use std::sync::Arc;

use kube::api::{ApiResource, GroupVersionKind};

use super::{GenericPlugin, ResourcePlugin};

/// Plugin lookup by GVK. Unknown kinds (including CRDs) fall back to the
/// generic plugin, so every discovered resource is always viewable.
pub struct PluginRegistry {
    by_kind: HashMap<(String, String), Arc<dyn ResourcePlugin>>,
    generic: Arc<dyn ResourcePlugin>,
}

impl PluginRegistry {
    /// Registry with the generic plugin only.
    pub fn new() -> Self {
        Self {
            by_kind: HashMap::new(),
            generic: Arc::new(GenericPlugin),
        }
    }

    /// Registry with the generic plugin and all built-in plugins.
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        super::builtin::register_all(&mut registry);
        registry
    }

    /// Registers a plugin for all its supported GVKs; later registrations win.
    pub fn register(&mut self, plugin: Arc<dyn ResourcePlugin>) {
        for gvk in plugin.supported() {
            self.by_kind
                .insert((gvk.group, gvk.kind), Arc::clone(&plugin));
        }
    }

    /// Plugin for a group and kind (version is ignored), or the generic one.
    pub fn plugin_for(&self, group: &str, kind: &str) -> Arc<dyn ResourcePlugin> {
        self.by_kind
            .get(&(group.to_owned(), kind.to_owned()))
            .unwrap_or(&self.generic)
            .clone()
    }

    pub fn plugin_for_gvk(&self, gvk: &GroupVersionKind) -> Arc<dyn ResourcePlugin> {
        self.plugin_for(&gvk.group, &gvk.kind)
    }

    /// Every registered plugin (once per GVK it serves) and the generic one.
    pub fn plugins(&self) -> impl Iterator<Item = &Arc<dyn ResourcePlugin>> {
        self.by_kind.values().chain(std::iter::once(&self.generic))
    }

    pub fn plugin_for_resource(&self, resource: &ApiResource) -> Arc<dyn ResourcePlugin> {
        self.plugin_for(&resource.group, &resource.kind)
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}
