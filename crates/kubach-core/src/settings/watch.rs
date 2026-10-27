//! Watching the settings file for external edits.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use notify::{EventKind, RecursiveMode, Watcher as _};

/// Stops watching when dropped.
pub struct SettingsWatcher {
    _watcher: notify::RecommendedWatcher,
}

/// Calls `on_change` (from a background thread) after `path` is created,
/// changed or removed and no further events arrived for `debounce`.
///
/// Watches the parent directory, creating it if missing, so atomic
/// replacements (rename over the file) are seen too. The caller's own
/// writes also trigger `on_change`; compare the reloaded settings to tell.
pub fn watch(
    path: &Path,
    debounce: Duration,
    on_change: impl Fn() + Send + 'static,
) -> notify::Result<SettingsWatcher> {
    let dir = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    super::create_private_dir(dir)?;
    let name: OsString = path.file_name().unwrap_or_default().to_owned();
    let watcher = watch_names(
        &[dir.to_owned()],
        HashSet::from([name]),
        debounce,
        "kubach-settings-watch",
        on_change,
    )?;
    Ok(SettingsWatcher { _watcher: watcher })
}

/// Watches `dirs` (non-recursively) and calls `on_change` (from a
/// background thread) once files named in `names` were created, changed or
/// removed there and no further events arrived for `debounce`.
///
/// Directories that can't be watched (e.g. missing ones) are skipped; fails
/// only when none of them can be.
pub(crate) fn watch_names(
    dirs: &[PathBuf],
    names: HashSet<OsString>,
    debounce: Duration,
    thread: &str,
    on_change: impl Fn() + Send + 'static,
) -> notify::Result<notify::RecommendedWatcher> {
    let (tx, rx) = mpsc::channel::<()>();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        // Compare names only: FSEvents may report canonicalized paths.
        if event
            .paths
            .iter()
            .any(|changed| changed.file_name().is_some_and(|name| names.contains(name)))
        {
            tx.send(()).ok();
        }
    })?;
    let mut first_error = None;
    let mut watched = 0;
    for dir in dirs {
        match watcher.watch(dir, RecursiveMode::NonRecursive) {
            Ok(()) => watched += 1,
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    if let (0, Some(error)) = (watched, first_error) {
        return Err(error);
    }

    // Exits once the watcher (and with it the sender) is dropped.
    std::thread::Builder::new()
        .name(thread.into())
        .spawn(move || {
            while rx.recv().is_ok() {
                loop {
                    match rx.recv_timeout(debounce) {
                        Ok(()) => continue,
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                on_change();
            }
        })?;
    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    use super::*;
    use crate::settings::{SETTINGS_FILE, Settings, ThemePreference, load, save};

    #[test]
    fn reports_debounced_external_edits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kubach").join(SETTINGS_FILE);
        let changes = Arc::new(AtomicUsize::new(0));
        let counter = changes.clone();
        let watcher = watch(&path, Duration::from_millis(200), move || {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
        // Let the backend start (FSEvents has a short latency).
        std::thread::sleep(Duration::from_millis(300));

        let mut settings = Settings::default();
        for theme in [ThemePreference::Light, ThemePreference::System] {
            settings.general.theme = theme;
            save(&path, &settings).unwrap();
        }
        // Unrelated files in the directory are ignored.
        std::fs::write(path.with_file_name("other.txt"), "x").unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        while changes.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(changes.load(Ordering::SeqCst) >= 1);
        assert_eq!(load(&path).settings.general.theme, ThemePreference::System);
        drop(watcher);
    }
}
