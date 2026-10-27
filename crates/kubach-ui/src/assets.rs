//! Asset source: the default `gpui-kit` icon bundle plus the Lucide icons
//! the app and gallery use beyond it. `IconName` lists all of Lucide, but the default
//! bundle embeds only ~100 SVGs, so any other icon renders empty (#338).

use std::borrow::Cow;

use gpui_kit::assets::icon_assets;
use gpui_kit::{AssetSource, Result, SharedString};

// Icons used in Kubach that are missing from `default-icons.txt` in `gpui-kit-assets`.
icon_assets!(
    ExtraIcons,
    [
        ArrowRightLeft,
        Box,
        Braces,
        Clock,
        Database,
        Layers,
        Lock,
        LogIn,
        PencilLine,
        Route,
        Server,
        Shield,
        ShieldBan,
        ShieldCheck,
        SlidersHorizontal,
        Trash,
        Waypoints,
        Workflow,
        Wrench,
    ]
);

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::assets::IconName;

    #[test]
    fn loads_default_and_extra_icons() {
        for icon in [
            IconName::ArrowRightLeft,
            IconName::Waypoints,
            IconName::Search,
        ] {
            let bytes = Assets.load(&icon.path()).unwrap();
            assert!(bytes.is_some_and(|b| !b.is_empty()), "{}", icon.path());
        }
    }
}
