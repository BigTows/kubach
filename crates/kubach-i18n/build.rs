//! Embeds `locales/<tag>/*.ftl` and lists the English messages with their
//! arguments, so `t!` and `msg!` check ids and argument names at compile
//! time (ADR 0018).

#[path = "src/ftl.rs"]
mod ftl;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Directories under `locales/`, in the order of `Locale`.
const LOCALES: [&str; 3] = ["en", "ru", "zh-Hans"];

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("locales");
    println!("cargo:rerun-if-changed={}", root.display());

    let mut out = String::from("pub(crate) static RESOURCES: [&[(&str, &str)]; 3] = [\n");
    let mut english = ftl::Messages::new();
    for locale in LOCALES {
        let dir = root.join(locale);
        println!("cargo:rerun-if-changed={}", dir.display());
        out.push_str("    &[\n");
        for file in ftl_files(&dir) {
            println!("cargo:rerun-if-changed={}", file.display());
            let name = file.file_name().unwrap().to_str().unwrap();
            writeln!(
                out,
                "        ({name:?}, include_str!({:?})),",
                file.display().to_string()
            )
            .unwrap();
            if locale == "en" {
                let source = std::fs::read_to_string(&file).unwrap();
                let messages = ftl::messages(&source)
                    .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
                for (id, args) in messages {
                    assert!(
                        english.insert(id.clone(), args).is_none(),
                        "message `{id}` is defined in two English files"
                    );
                }
            }
        }
        out.push_str("    ],\n");
    }
    out.push_str("];\n\n");

    out.push_str(
        "/// English message ids with their argument names, sorted by id.\n\
         pub(crate) static KEYS: &[(&str, &[&str])] = &[\n",
    );
    for (id, args) in &english {
        let args: Vec<String> = args.iter().map(|arg| format!("{arg:?}")).collect();
        writeln!(out, "    ({id:?}, &[{}]),", args.join(", ")).unwrap();
    }
    out.push_str("];\n");

    let dest = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("resources.rs");
    std::fs::write(dest, out).unwrap();
}

/// `.ftl` files of `dir`, sorted by name.
fn ftl_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "ftl"))
        .collect();
    files.sort();
    files
}
