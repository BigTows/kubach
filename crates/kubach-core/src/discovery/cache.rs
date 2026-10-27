//! The API catalog of each context on disk, `<cache dir>/discovery`.
//!
//! Discovery of a big cluster (many CRDs, a remote API server) takes from a
//! fraction of a second to several seconds. The cached catalog is shown at
//! once on connect, and discovery refreshes it in the background (#299).

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::ApiCatalog;

/// Bumped when the stored format changes; older files are ignored.
const FORMAT: u32 = 1;

#[derive(Serialize, Deserialize)]
struct Stored {
    format: u32,
    catalog: ApiCatalog,
}

/// `discovery` in Kubach's [disk cache](crate::disk_cache).
pub fn dir() -> Option<PathBuf> {
    crate::disk_cache::dir().map(|dir| dir.join("discovery"))
}

/// File of the catalog of `context` at `server` in `dir`. The name is a
/// hash: context names may hold any character, and the server is part of
/// the key so a context pointed at another cluster starts afresh.
pub fn path(dir: &Path, context: &str, server: &str) -> PathBuf {
    let mut hash = Sha256::new();
    hash.update(context.as_bytes());
    hash.update([0]);
    hash.update(server.as_bytes());
    let name: String = hash.finalize()[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    dir.join(format!("{name}.json"))
}

/// The stored catalog, or `None` if there is none or it can't be used.
pub fn load(path: &Path) -> Option<ApiCatalog> {
    let bytes = std::fs::read(path).ok()?;
    let stored: Stored = serde_json::from_slice(&bytes).ok()?;
    (stored.format == FORMAT).then_some(stored.catalog)
}

/// Writes `catalog` to `path` through a temporary file, so a reader never
/// sees half of it.
pub fn store(path: &Path, catalog: &ApiCatalog) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("no cache directory"))?;
    std::fs::create_dir_all(dir)?;
    let stored = Stored {
        format: FORMAT,
        catalog: catalog.clone(),
    };
    let json = serde_json::to_vec(&stored).map_err(io::Error::other)?;
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use kube::api::ApiResource;
    use kube::discovery::{ApiCapabilities, Scope};

    use super::{ApiCatalog, load, path, store};

    fn catalog() -> ApiCatalog {
        let pods = ApiResource {
            group: String::new(),
            version: "v1".into(),
            api_version: "v1".into(),
            kind: "Pod".into(),
            plural: "pods".into(),
        };
        let capabilities = ApiCapabilities {
            scope: Scope::Namespaced,
            subresources: Vec::new(),
            operations: vec!["list".into(), "watch".into()],
        };
        ApiCatalog::from_resources([(pods, capabilities)])
    }

    #[test]
    fn stores_and_loads_the_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let file = path(&dir.path().join("discovery"), "minikube", "https://1.2.3.4");
        assert!(load(&file).is_none());

        store(&file, &catalog()).unwrap();
        let loaded = load(&file).unwrap();
        assert_eq!(loaded, catalog());
        assert!(loaded.find("", "Pod").unwrap().namespaced);
        // Nothing but the catalog is left behind.
        assert_eq!(
            std::fs::read_dir(file.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn keys_by_context_and_server() {
        let dir = std::path::Path::new("/cache");
        let a = path(dir, "prod", "https://a");
        assert_eq!(a, path(dir, "prod", "https://a"));
        assert_ne!(a, path(dir, "prod", "https://b"));
        assert_ne!(a, path(dir, "staging", "https://a"));
        // Separator: ("ab", "c") and ("a", "bc") differ.
        assert_ne!(path(dir, "ab", "c"), path(dir, "a", "bc"));
        assert_eq!(a.parent(), Some(dir));
    }

    #[test]
    fn ignores_broken_and_foreign_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.json");
        std::fs::write(&file, b"{not json").unwrap();
        assert!(load(&file).is_none());
        std::fs::write(&file, br#"{"format":999,"catalog":{"kinds":[]}}"#).unwrap();
        assert!(load(&file).is_none());
    }
}
