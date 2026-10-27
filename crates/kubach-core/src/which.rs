//! Finding programs in `PATH` (kustomize, exec credential plugins).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The directories of `path` (a `PATH` value) that are safe to search:
/// empty and relative entries are skipped, because they resolve against the
/// current directory, and a `./helm` from somebody else's repository must
/// not be picked up (#202).
pub fn search_dirs(path: &OsStr) -> impl Iterator<Item = PathBuf> + '_ {
    std::env::split_paths(path).filter(|dir| dir.is_absolute())
}

/// The first executable `name` in the absolute directories of `path` (a
/// `PATH` value, see [`search_dirs`]); on Windows `name.exe` is tried first.
pub fn which(name: &str, path: &OsStr) -> Option<PathBuf> {
    let names: Vec<String> = if cfg!(windows) {
        vec![format!("{name}.exe"), name.to_owned()]
    } else {
        vec![name.to_owned()]
    };
    search_dirs(path)
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|candidate| is_executable(candidate))
}

/// Whether `command` can be started: a path (absolute or with a directory)
/// must be an executable file, a bare name must be found in `path`.
pub fn command_exists(command: &str, path: &OsStr) -> bool {
    let as_path = Path::new(command);
    if as_path.is_absolute() || as_path.components().count() > 1 {
        is_executable(as_path) || (cfg!(windows) && is_executable(&as_path.with_extension("exe")))
    } else {
        which(command, path).is_some()
    }
}

#[cfg(unix)]
pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
pub fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn tool(dir: &Path, name: &str) -> PathBuf {
        let file = dir.join(name);
        std::fs::write(&file, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        file
    }

    #[test]
    fn search_dirs_skips_empty_and_relative_entries() {
        let dirs: Vec<PathBuf> = search_dirs(OsStr::new("/usr/bin::.:bin:./x:/opt/bin")).collect();
        assert_eq!(dirs, [PathBuf::from("/usr/bin"), PathBuf::from("/opt/bin")]);
    }

    #[test]
    fn which_finds_tool_in_absolute_dir() {
        let dir = tempfile::tempdir().unwrap();
        let file = tool(dir.path(), "i202-tool");
        let path = std::env::join_paths([dir.path()]).unwrap();
        assert_eq!(which("i202-tool", &path), Some(file));
    }

    #[test]
    fn which_ignores_relative_dirs() {
        let dir = tempfile::tempdir().unwrap();
        tool(dir.path(), "i202-tool");
        // A relative entry naming the directory must not be searched, even
        // though it would resolve from the right working directory.
        let relative = dir.path().strip_prefix("/").unwrap();
        let path = std::env::join_paths([Path::new(""), Path::new("."), relative]).unwrap();
        assert_eq!(which("i202-tool", &path), None);
        assert!(!command_exists("i202-tool", &path));
    }
}
