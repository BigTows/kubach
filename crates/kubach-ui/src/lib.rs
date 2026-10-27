//! Kubach design system.
//!
//! Colors live in one place: the bundled `themes/kubach.json` (Kubach Dark /
//! Kubach Light), loaded into the gpui-component theme. Every component here
//! reads them through [`ActiveTheme`](gpui_kit::component::ActiveTheme), so the
//! stock gpui-component widgets and the Kubernetes-specific ones below always
//! match and follow light/dark switching.

pub mod a11y_check;
pub mod age;
pub mod assets;
mod components;
pub mod measure;
pub mod theme;
pub mod tokens;
pub mod window;

pub use age::{AgeTicker, format_age, format_since, unix_now};
pub use components::*;
pub use tokens::Tone;

/// Registers the components' key bindings (e.g. `cmd-a` in checkable
/// tables). Call once at startup, after `gpui_kit::init`.
pub fn init(cx: &mut gpui_kit::App) {
    components::init(cx);
}
