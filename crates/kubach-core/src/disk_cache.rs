//! Kubach's own disk cache, `<config dir>/cache`: downloaded chart icons,
//! README images and anything else that can be fetched or rebuilt again.
//! "Clear cache" removes the folder whole, so caches belong in it and
//! nothing else does (settings, state and the vault live beside it).

use std::io;
use std::path::{Path, PathBuf};

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

#[cfg(test)]
mod tests {
    use super::{clear, size};

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
