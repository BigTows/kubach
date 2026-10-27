//! Guards against duplicate accessibility node ids (#354).
//!
//! `gallery_a11y_tree_has_unique_ids` (ignored: it opens a window, macOS
//! only) runs the gallery with the accessibility tree on and fails if it
//! panics, once per interface language:
//! `cargo test -p kubach-ui --test a11y_ids -- --ignored` (debug profile:
//! the release one has no such panic).
//!
//! GPUI builds an element's accessibility node id from its whole id path,
//! and a stateless (`RenderOnce`) component adds only its type name to that
//! path. So a button with a literal id inside such a component gets the same
//! node id in every instance under one parent, and a debug build panics with
//! "Duplicate a11y node id" as soon as two of them are on screen (release
//! silently drops the node). GPUI's test window never turns accessibility
//! on, so the always-on test checks the rule in the sources: a `RenderOnce`
//! component that gives a
//! role-bearing element a literal id must take its own id and put it on its
//! root (`.id(self.id)`), which scopes the literal per instance.

use std::fs;
use std::path::{Path, PathBuf};

/// Constructors of gpui-kit elements that get an accessibility node.
const ROLE_ELEMENTS: &[&str] = &[
    "Button",
    "Progress",
    "Clipboard",
    "Switch",
    "Checkbox",
    "Radio",
    "Slider",
    "Toggle",
    "Link",
    "Tab",
];

/// Files allowed to keep a literal id without a root id, with the reason.
const ALLOWED: &[(&str, &str)] = &[(
    "components/filter_bar.rs",
    "`no_matches` is a list placeholder: one per screen view, which scopes it",
)];

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

/// Literal ids given to role-bearing elements, as `Button::new("…")`.
fn literal_role_ids(source: &str) -> Vec<String> {
    let mut found = Vec::new();
    for name in ROLE_ELEMENTS {
        let needle = format!("{name}::new(\"");
        let mut rest = source;
        while let Some(at) = rest.find(&needle) {
            let preceded_by_ident = rest[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
            let tail = &rest[at + needle.len()..];
            if !preceded_by_ident {
                let end = tail.find('"').unwrap_or(tail.len());
                found.push(format!("{name}::new(\"{}\")", &tail[..end]));
            }
            rest = tail;
        }
    }
    found
}

#[test]
fn stateless_components_scope_literal_ids() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(!files.is_empty());

    let mut offenders = Vec::new();
    for file in files {
        let source = fs::read_to_string(&file).unwrap();
        // Unit tests may build components however they like.
        let source = source.split("#[cfg(test)]").next().unwrap_or_default();
        let relative = file
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if !source.contains("impl RenderOnce for")
            || source.contains(".id(self.id")
            || ALLOWED.iter().any(|(path, _)| *path == relative)
        {
            continue;
        }
        let ids = literal_role_ids(source);
        if !ids.is_empty() {
            offenders.push(format!("{relative}: {}", ids.join(", ")));
        }
    }
    assert!(
        offenders.is_empty(),
        "literal ids in RenderOnce components clash between instances; take an id \
         and put it on the root with `.id(self.id)`:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn finds_literal_ids() {
    let source =
        r#"Button::new("a").child(Progress::new("b")); MyButton::new("c"); Button::new(id)"#;
    assert_eq!(
        literal_role_ids(source),
        ["Button::new(\"a\")", "Progress::new(\"b\")"]
    );
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "opens the gallery window"]
fn gallery_a11y_tree_has_unique_ids() {
    // `cargo test --test a11y_ids` doesn't build examples: build the gallery
    // into the same target dir, next to the test binary's `deps`.
    let exe = std::env::current_exe().unwrap();
    let profile_dir = exe.parent().and_then(Path::parent).unwrap();
    let target_dir = profile_dir.parent().unwrap();
    let built = std::process::Command::new(env!("CARGO"))
        .args(["build", "-p", "kubach-ui", "--example", "gallery"])
        .env("CARGO_TARGET_DIR", target_dir)
        .status()
        .unwrap();
    assert!(built.success(), "gallery build failed");
    let gallery = profile_dir.join("examples").join("gallery");
    // Every interface language (#444): translated text changes layout, and
    // what is cut, wrapped or shown.
    for lang in ["en", "ru", "zh-Hans"] {
        let mut command = std::process::Command::new(&gallery);
        command.env("KUBACH_UI_LANG", lang);
        if let Err(err) = kubach_ui::a11y_check::run(&mut command) {
            panic!("{lang}: {err}");
        }
    }
}
