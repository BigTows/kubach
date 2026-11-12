//! Kubach's own disk cache, `<config dir>/cache`: downloaded chart icons,
//! README images and anything else that can be fetched or rebuilt again.
//! "Clear cache" removes the folder whole, so caches belong in it and
//! nothing else does (settings, state and the vault live beside it).
//!
//! Some caches hold what a cluster returned, per context: the lists of
//! [`list_cache`](crate::list_cache) and the API catalogs of
//! [`discovery::cache`](crate::discovery::cache). Their file names begin
//! with [`context_tag`], so the files of a context can be found by its name
//! alone, and the contexts in [`uncached`] have none: nothing of them is
//! written or read ([ADR 0008](../../../docs/adr/0008-kubeconfig-vault.md),
//! [ADR 0012](../../../docs/adr/0012-list-cache.md)).

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, PoisonError, RwLock};
use std::time::{Duration, SystemTime};

use sha2::{Digest as _, Sha256};

/// Folders of the disk cache whose files belong to a context: every file
/// name in them begins with the [`context_tag`] of its context.
pub const CONTEXT_FOLDERS: [&str; 2] = ["lists", "discovery"];

/// A temporary file older than this was left by a write that never ended.
const STALE_TEMP: Duration = Duration::from_secs(60 * 60);

const TAG_LEN: usize = 32;

/// `cache` in the config directory (`KUBACH_CONFIG_DIR` overrides it).
pub fn dir() -> Option<PathBuf> {
    crate::settings::config_dir().map(|dir| dir.join("cache"))
}

/// Bytes of the files under `dir`; unreadable entries count as nothing.
pub fn size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => size(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map_or(0, |meta| meta.len()),
            _ => 0,
        })
        .sum()
}

/// Removes `dir` with everything in it; a missing one is already clear.
pub fn clear(dir: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(dir) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Contexts nothing of which is kept in the per-context caches, neither
/// written nor read: the app lists the contexts of the vault here. Clones
/// share the list.
#[derive(Clone, Debug, Default)]
pub struct Uncached(Arc<RwLock<Names>>);

#[derive(Debug, Default)]
struct Names {
    known: bool,
    names: BTreeSet<String>,
}

impl Uncached {
    /// Replaces the list. True if it changed, and the first time it is set:
    /// the files of these contexts are then to be removed ([`sweep`]).
    pub fn set(&self, names: BTreeSet<String>) -> bool {
        let mut state = self.0.write().unwrap_or_else(PoisonError::into_inner);
        let changed = !state.known || state.names != names;
        state.known = true;
        state.names = names;
        changed
    }

    pub fn contains(&self, context: &str) -> bool {
        let state = self.0.read().unwrap_or_else(PoisonError::into_inner);
        state.names.contains(context)
    }
}

/// The list the caches in the config directory go by.
pub fn uncached() -> &'static Uncached {
    static UNCACHED: OnceLock<Uncached> = OnceLock::new();
    UNCACHED.get_or_init(Uncached::default)
}

/// What the cache files of `context` begin with: a hash of its name, so
/// any name is a valid file name and the files of a context are found
/// without reading them.
pub fn context_tag(context: &str) -> String {
    Sha256::digest(context.as_bytes())[..TAG_LEN / 2]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Name of a cache file of `context`: its tag, then `rest`.
pub fn file_name(context: &str, rest: &str) -> String {
    format!("{}-{rest}", context_tag(context))
}

/// The context tag a file name begins with (after the dot of a temporary
/// file), if it is a name made by [`file_name`].
fn tag_of(name: &str) -> Option<&str> {
    let (tag, _) = name.strip_prefix('.').unwrap_or(name).split_once('-')?;
    let hex = tag.len() == TAG_LEN && tag.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    hex.then_some(tag)
}

/// Cleans the per-context folders of the cache in `dir`: removes the files
/// of the `uncached` contexts, files not named by [`file_name`] (earlier
/// versions named them by a hash that says nothing of the context) and
/// temporary files of writes that never ended. Returns how many went.
/// Blocking.
pub fn sweep(dir: &Path, uncached: &BTreeSet<String>) -> usize {
    let tags: BTreeSet<String> = uncached.iter().map(|name| context_tag(name)).collect();
    let mut removed = 0;
    for folder in CONTEXT_FOLDERS {
        let Ok(entries) = std::fs::read_dir(dir.join(folder)) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let stale = || {
                let modified = entry.metadata().and_then(|meta| meta.modified());
                modified.is_ok_and(|at| at + STALE_TEMP < SystemTime::now())
            };
            let keep = tag_of(&name)
                .is_some_and(|tag| !tags.contains(tag) && !(name.starts_with('.') && stale()));
            if !keep && std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::time::{Duration, SystemTime};

    use super::{Uncached, clear, context_tag, file_name, size, sweep, tag_of};

    #[test]
    fn uncached_contexts_are_replaced_as_a_whole() {
        let uncached = Uncached::default();
        assert!(!uncached.contains("vault"));
        // The first list counts as a change even when empty.
        assert!(uncached.set(BTreeSet::new()));
        assert!(!uncached.set(BTreeSet::new()));
        assert!(uncached.set(BTreeSet::from(["vault".to_owned()])));
        assert!(uncached.clone().contains("vault"));
        assert!(!uncached.contains("file"));
        assert!(!uncached.set(BTreeSet::from(["vault".to_owned()])));
        assert!(uncached.set(BTreeSet::from(["other".to_owned()])));
        assert!(!uncached.contains("vault"));
    }

    #[test]
    fn file_names_begin_with_the_tag_of_the_context() {
        let name = file_name("arn:aws:eks:eu-west-1:1:cluster/prod", "0a1b.json");
        let tag = context_tag("arn:aws:eks:eu-west-1:1:cluster/prod");
        assert_eq!(name, format!("{tag}-0a1b.json"));
        assert_eq!(tag_of(&name), Some(tag.as_str()));
        assert_eq!(tag_of(&format!(".{name}.tmp-1")), Some(tag.as_str()));
        assert_ne!(tag, context_tag("prod"));
        // Names of earlier versions: one hash, no tag.
        assert_eq!(tag_of("00112233445566778899aabbccddeeff.json.gz"), None);
        assert_eq!(tag_of("short-name.json"), None);
    }

    #[test]
    fn sweep_removes_uncached_contexts_and_old_names_only() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        let lists = cache.join("lists");
        let discovery = cache.join("discovery");
        let images = cache.join("images");
        for dir in [&lists, &discovery, &images] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let file = |dir: &std::path::Path, name: String| {
            let path = dir.join(name);
            std::fs::write(&path, b"x").unwrap();
            path
        };
        let vault_list = file(&lists, file_name("from-vault", "aa.json.gz"));
        let vault_catalog = file(&discovery, file_name("from-vault", "bb.json"));
        let file_list = file(&lists, file_name("from-file", "aa.json.gz"));
        let file_catalog = file(&discovery, file_name("from-file", "bb.json"));
        let old_list = file(&lists, "00112233445566778899aabbccddeeff.json.gz".into());
        let old_catalog = file(&discovery, "00112233445566778899aabbccddeeff.json".into());
        let image = file(&images, "00112233445566778899aabbccddeeff".into());
        // A write of another process under way, and one that never ended.
        let writing = file(&lists, format!(".{}.tmp-1", file_name("from-file", "cc")));
        let abandoned = file(&lists, format!(".{}.tmp-2", file_name("from-file", "dd")));
        std::fs::File::options()
            .append(true)
            .open(&abandoned)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(2 * 60 * 60))
            .unwrap();

        let uncached = BTreeSet::from(["from-vault".to_owned()]);
        assert_eq!(sweep(&cache, &uncached), 5);
        for gone in [
            &vault_list,
            &vault_catalog,
            &old_list,
            &old_catalog,
            &abandoned,
        ] {
            assert!(!gone.exists(), "{gone:?}");
        }
        for kept in [&file_list, &file_catalog, &image, &writing] {
            assert!(kept.exists(), "{kept:?}");
        }
        assert_eq!(sweep(&cache, &uncached), 0);
        // A cache that does not exist yet has nothing to sweep.
        assert_eq!(sweep(&root.path().join("none"), &uncached), 0);
    }

    #[test]
    fn sizes_nested_files_and_clears_the_folder() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        std::fs::create_dir_all(cache.join("images")).unwrap();
        std::fs::write(cache.join("images").join("a"), [0u8; 10]).unwrap();
        std::fs::write(cache.join("b"), [0u8; 5]).unwrap();
        std::fs::write(root.path().join("settings.toml"), "x").unwrap();

        assert_eq!(size(&cache), 15);
        clear(&cache).unwrap();
        assert!(!cache.exists());
        assert!(root.path().join("settings.toml").exists());
        assert_eq!(size(&cache), 0);
        clear(&cache).unwrap();
    }
}
