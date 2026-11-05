//! `KeyBinding::new` panics at startup on a `secondary-` keystroke, and the
//! compiler does not see it: the app dies before its first window. The rule
//! is checked in the sources: `kubach_ui::secondary!` spells such a shortcut.

use std::fs;
use std::path::{Path, PathBuf};

use gpui_kit::{KeyBinding, NoAction};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_secondary_prefix_in_sources() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut files = Vec::new();
    for dir in ["kubach-ui/src", "kubach-ui/examples", "kubach-app/src"] {
        rust_files(&crates.join(dir), &mut files);
    }
    assert!(!files.is_empty());
    let offenders: Vec<_> = files
        .iter()
        .filter(|path| fs::read_to_string(path).unwrap().contains("\"secondary-"))
        .collect();
    assert!(offenders.is_empty(), "`secondary-` in {offenders:?}");
}

#[test]
fn secondary_is_a_valid_keystroke() {
    let binding = KeyBinding::new(kubach_ui::secondary!("shift-n"), NoAction, None);
    let keystroke = binding.keystrokes()[0].inner();
    assert_eq!(keystroke.key, "n");
    assert!(keystroke.modifiers.shift);
    assert_eq!(keystroke.modifiers.platform, cfg!(target_os = "macos"));
    assert_eq!(keystroke.modifiers.control, !cfg!(target_os = "macos"));
}
