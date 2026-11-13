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
use crate::disk_cache::Uncached;

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

/// File of the catalog of `context` at `server` in `dir`. The name is the
/// [tag](crate::disk_cache::context_tag) of the context and a hash of the
/// server, so a context pointed at another cluster starts afresh.
pub fn path(dir: &Path, context: &str, server: &str) -> PathBuf {
    let server: String = Sha256::digest(server.as_bytes())[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    dir.join(crate::disk_cache::file_name(
        context,
        &format!("{server}.json"),
    ))
}

/// Where the catalog of `context` at `server` is kept in the config
/// directory; `None` for a context that is not cached
/// ([`disk_cache::uncached`](crate::disk_cache::uncached)) or without a
/// config directory.
pub fn file(context: &str, server: &str) -> Option<PathBuf> {
    file_in(&dir()?, crate::disk_cache::uncached(), context, server)
}

fn file_in(dir: &Path, uncached: &Uncached, context: &str, server: &str) -> Option<PathBuf> {
    (!uncached.contains(context)).then(|| path(dir, context, server))
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
    let stored = Stored {
        format: FORMAT,
        catalog: catalog.clone(),
    };
    let json = serde_json::to_vec(&stored).map_err(io::Error::other)?;
    crate::settings::write_atomic(path, &json)
}

#[cfg(test)]
mod tests {
    use kube::api::ApiResource;
    use kube::discovery::{ApiCapabilities, Scope};

    use std::collections::BTreeSet;

    use super::{ApiCatalog, file_in, load, path, store};
    use crate::disk_cache::{Uncached, context_tag};

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
    fn a_context_that_is_not_cached_has_no_file() {
        let dir = std::path::Path::new("/cache");
        let uncached = Uncached::default();
        uncached.set(BTreeSet::from(["from-vault".to_owned()]));
        assert_eq!(file_in(dir, &uncached, "from-vault", "https://a"), None);
        let file = file_in(dir, &uncached, "from-file", "https://a").unwrap();
        assert_eq!(file, path(dir, "from-file", "https://a"));
        // The files of a context are found by its name alone.
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(&format!("{}-", context_tag("from-file"))));
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
