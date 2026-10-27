//! Cluster hotbar: tile labels and colors from the user's
//! [`HotbarPrefs`] (stored in settings), and local-cluster detection.

use std::path::Path;

use gpui_kit::{App, SharedString};
use kubach_core::ContextInfo;
use kubach_core::kubeconfig::{FileState, Source};
use kubach_core::settings::security::is_local_cluster;
use kubach_core::settings::{ColorPreset, HotbarPrefs, TileColorChoice};
use kubach_i18n::t;
use kubach_ui::TileColor;

use crate::settings::AppSettings;

/// Longest alias shown on a tile, in characters.
pub const ALIAS_MAX: usize = 3;

/// Id prefix of tiles standing for an unreadable linked kubeconfig file
/// whose contexts are unknown. Other tile ids are context names.
const FILE_TILE_PREFIX: &str = "kubeconfig-file:";

/// Tile id for the unreadable linked file `path`.
pub fn file_tile_id(path: &Path) -> String {
    format!("{FILE_TILE_PREFIX}{}", path.display())
}

/// Path of a file tile, `None` for context tiles.
pub fn file_tile_path(id: &str) -> Option<&Path> {
    id.strip_prefix(FILE_TILE_PREFIX).map(Path::new)
}

/// Label of a file tile: initials of the file name (`eks-prod.yaml` → `ep`).
pub fn file_tile_label(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy())
        .unwrap_or_default();
    initials(&stem)
}

/// Tooltip row telling where a context comes from.
pub fn source_row(source: &Source) -> (SharedString, SharedString) {
    (t!("hotbar-row-source").into(), source.to_string().into())
}

/// Tooltip rows of a context or file that can't be read now.
pub fn problem_rows(file: &FileState) -> Vec<(SharedString, SharedString)> {
    let problem = file
        .problem
        .as_ref()
        .map(|problem| problem.message().to_string())
        .unwrap_or_default();
    let mut rows = vec![
        (t!("hotbar-row-problem").into(), problem.into()),
        (
            t!("hotbar-row-file").into(),
            file.path().display().to_string().into(),
        ),
    ];
    if file.source.is_linked() {
        rows.push((t!("hotbar-row-fix").into(), t!("hotbar-fix-linked").into()));
    }
    rows
}

/// Text on the tile of `name`: the alias (clipped), or the initials.
pub fn tile_label(prefs: &HotbarPrefs, name: &str) -> String {
    match prefs.alias(name) {
        Some(alias) => alias.chars().take(ALIAS_MAX).collect(),
        None => initials(name),
    }
}

/// Hotbar preferences of the current settings (defaults without them).
pub fn prefs(cx: &App) -> HotbarPrefs {
    AppSettings::try_global(cx)
        .map(|settings| settings.read(cx).get().clusters.hotbar.clone())
        .unwrap_or_default()
}

/// Chosen color of `name`, or one derived from the name.
pub fn tile_color(prefs: &HotbarPrefs, name: &str) -> TileColor {
    prefs
        .tile_color(name)
        .map(choice_color)
        .unwrap_or_else(|| default_color(name))
}

/// How a stored color choice is drawn: presets from the theme, custom
/// colors as given.
pub fn choice_color(choice: TileColorChoice) -> TileColor {
    match choice {
        TileColorChoice::Preset(preset) => preset_color(preset),
        TileColorChoice::Custom(rgb) => TileColor::Custom(rgb.to_u32()),
    }
}

/// The theme color of a stored preset.
pub fn preset_color(preset: ColorPreset) -> TileColor {
    match preset {
        ColorPreset::Blue => TileColor::Blue,
        ColorPreset::Teal => TileColor::Teal,
        ColorPreset::Orange => TileColor::Orange,
        ColorPreset::Purple => TileColor::Purple,
        ColorPreset::Pink => TileColor::Pink,
        ColorPreset::Gray => TileColor::Gray,
    }
}

/// Two lowercase letters for a context name: `prod` → `pr`,
/// `kind-dev` → `kd`, `minikube` → `mk`. EKS ARNs and GKE names use the
/// cluster part (`arn:…:cluster/prod` → `pr`, `gke_proj_zone_prod` → `pr`).
pub fn initials(name: &str) -> String {
    let mut base = name.rsplit('/').next().unwrap_or(name);
    if base.starts_with("gke_") {
        base = base.rsplit('_').next().unwrap_or(base);
    }
    if base.eq_ignore_ascii_case("minikube") {
        return "mk".into();
    }
    let parts: Vec<&str> = base
        .split(|c: char| !c.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect();
    let initials: String = match parts.as_slice() {
        [] => return "?".into(),
        [word] => word.chars().take(2).collect(),
        [first, second, ..] => first
            .chars()
            .take(1)
            .chain(second.chars().take(1))
            .collect(),
    };
    initials.to_lowercase()
}

/// Local clusters (minikube, kind, k3d, Docker Desktop, or an API server on
/// loopback), by context or cluster name. Same rule as the security presets.
pub fn is_local(context: &ContextInfo) -> bool {
    let server = context.server.as_deref().unwrap_or_default();
    is_local_cluster(&context.name, server) || is_local_cluster(&context.cluster, server)
}

/// A stable color picked from the name, so tiles differ without setup.
pub fn default_color(name: &str) -> TileColor {
    // FNV-1a: stable across runs and platforms, unlike `DefaultHasher`.
    let hash = name.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    // Gray stays for explicit choice.
    let palette = &TileColor::ALL[..TileColor::ALL.len() - 1];
    palette[hash as usize % palette.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(name: &str, cluster: &str, server: Option<&str>) -> ContextInfo {
        ContextInfo {
            name: name.into(),
            cluster: cluster.into(),
            user: None,
            namespace: None,
            server: server.map(Into::into),
        }
    }

    #[test]
    fn initials_of_context_names() {
        assert_eq!(initials("prod"), "pr");
        assert_eq!(initials("staging"), "st");
        assert_eq!(initials("minikube"), "mk");
        assert_eq!(initials("kind-dev"), "kd");
        assert_eq!(initials("docker-desktop"), "dd");
        assert_eq!(initials("Prod_EU"), "pe");
        assert_eq!(
            initials("arn:aws:eks:eu-central-1:123456789012:cluster/payments"),
            "pa"
        );
        assert_eq!(initials("gke_my-project_europe-west1_prod"), "pr");
        assert_eq!(initials("x"), "x");
        assert_eq!(initials("--"), "?");
    }

    #[test]
    fn file_tiles_are_told_from_context_tiles() {
        let path = Path::new("/home/me/.kube/eks-prod.yaml");
        let id = file_tile_id(path);
        assert_eq!(file_tile_path(&id), Some(path));
        assert_eq!(file_tile_path("prod"), None);
        assert_eq!(file_tile_label(path), "ep");
    }

    #[test]
    fn problem_rows_offer_a_fix_for_linked_files() {
        use kubach_core::kubeconfig::FileProblem;
        let file = |source| FileState {
            source,
            problem: Some(FileProblem::Missing),
            loose_mode: None,
            contexts: Vec::new(),
        };
        let linked = problem_rows(&file(Source::Linked("/x/eks.yaml".into())));
        assert_eq!(linked[0].1.as_ref(), "File not found");
        assert_eq!(linked[1].1.as_ref(), "/x/eks.yaml");
        assert_eq!(linked.len(), 3);
        let default = problem_rows(&file(Source::Env("/x/config".into())));
        assert_eq!(default.len(), 2);
        assert_eq!(
            source_row(&Source::Linked("/x/eks.yaml".into())).1.as_ref(),
            "Linked file: /x/eks.yaml"
        );
    }

    #[test]
    fn alias_overrides_initials() {
        let mut prefs = HotbarPrefs::default();
        assert_eq!(tile_label(&prefs, "staging"), "st");
        prefs.set_alias("staging", "STG1");
        assert_eq!(tile_label(&prefs, "staging"), "STG");
        prefs.set_alias("staging", "");
        assert_eq!(tile_label(&prefs, "staging"), "st");
    }

    #[test]
    fn detects_local_clusters() {
        assert!(is_local(&context("minikube", "minikube", None)));
        assert!(is_local(&context("kind-dev", "kind-dev", None)));
        assert!(is_local(&context("k3d-test", "k3d-test", None)));
        assert!(is_local(&context("docker-desktop", "docker-desktop", None)));
        // Renamed context of a local cluster.
        assert!(is_local(&context("dev", "kind-dev", None)));
        assert!(is_local(&context(
            "dev",
            "lab",
            Some("https://127.0.0.1:6443")
        )));
        assert!(is_local(&context("dev", "lab", Some("https://localhost"))));
        assert!(is_local(&context(
            "dev",
            "lab",
            Some("https://[::1]:6443/")
        )));
        assert!(!is_local(&context(
            "prod",
            "prod",
            Some("https://prod.example.com")
        )));
        assert!(!is_local(&context("kindly", "prod", None)));
        assert!(!is_local(&context(
            "prod",
            "prod",
            Some("https://10.0.0.1:443")
        )));
    }

    #[test]
    fn colors_default_by_name_and_can_be_set() {
        let mut prefs = HotbarPrefs::default();
        assert_eq!(tile_color(&prefs, "prod"), default_color("prod"));
        assert_ne!(default_color("prod"), TileColor::Gray);
        prefs.set_color("prod", TileColor::Gray.name());
        assert_eq!(tile_color(&prefs, "prod"), TileColor::Gray);
        // Unknown names fall back to the derived color.
        prefs.set_color("prod", "ultraviolet");
        assert_eq!(tile_color(&prefs, "prod"), default_color("prod"));
        // Custom colors are drawn as stored.
        prefs.set_color("prod", "#2fb5a0");
        assert_eq!(tile_color(&prefs, "prod"), TileColor::Custom(0x2f_b5_a0));
        prefs.set_tile_color("prod", None);
        assert_eq!(tile_color(&prefs, "prod"), default_color("prod"));
    }

    #[test]
    fn every_preset_has_its_theme_color() {
        for preset in ColorPreset::ALL {
            assert_eq!(preset_color(preset).name(), preset.name());
        }
        assert_eq!(ColorPreset::ALL.len(), TileColor::ALL.len());
    }
}
