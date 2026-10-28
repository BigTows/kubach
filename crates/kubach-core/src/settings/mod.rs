//! Application settings.
//!
//! A typed [`Settings`] model stored as TOML in the OS config directory
//! ([`default_path`]), with a schema version and migrations, atomic writes,
//! tolerant loading and a debounced file watcher. Free of UI dependencies, so
//! the MCP server (#12) and other parts read the same model.

mod file;
mod hotbar;
mod migrate;
mod model;
mod presets;
pub mod security;
mod watch;

pub(crate) use file::write_atomic;
pub use file::{
    CONFIG_DIR_ENV, Loaded, SETTINGS_FILE, config_dir, create_private_dir, default_path, load, save,
};
pub use hotbar::{ColorPreset, HotbarPrefs, Rgb, TileColorChoice};
pub use model::{
    Agent, Clusters, DEFAULT_KEEP_CONNECTED, DEFAULT_MCP_PORT, DEFAULT_VAULT_LOCK_MINUTES, General,
    HelmBinary, HelmPrefs, HelmSource, Language, PrometheusSource, SCHEMA_VERSION, Settings,
    ThemePreference, Tools, expand_home, find_on_path,
};
pub use presets::{Presets, ResourcePreset};
pub use security::{
    AccessRaise, AgentAccess, AgentLevel, ContextKey, ContextPolicy, ContextSecurity,
    MAX_ACCESS_RAISES, SECRET_VALUES_HIDDEN, SecretAction, SecuritySettings,
};
pub(crate) use watch::watch_names;
pub use watch::{SettingsWatcher, watch};
