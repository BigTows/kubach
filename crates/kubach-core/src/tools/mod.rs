//! External programs Kubach can download on request (ADR 0006, #131).
//!
//! A tool is described by a [`ToolSpec`]: pinned version, where its
//! release archives live, the SHA-256 of each archive and the path of the
//! binary inside. [`download`] fetches, verifies and unpacks it into
//! `<data dir>/tools/<name>/<version>/`; nothing is installed system-wide.
//! A release other than the pinned one must carry a detached OpenPGP
//! signature by a key Kubach ships ([`signature`], #403). Only Helm
//! ([`helm::HELM4`], [`helm::HELM3`]) is described so far.

pub mod archive;
pub mod download;
pub mod helm;
pub mod proxy;
pub mod signature;

use std::path::PathBuf;

pub use download::{
    Arch, Artifact, DownloadError, Installed, Os, Platform, Progress, SignatureSpec, ToolSpec,
    Verification, download, download_version, installed, installed_versions, remove_all,
    remove_other_versions,
};

/// Overrides the data directory (tests, portable setups).
pub const DATA_DIR_ENV: &str = "KUBACH_DATA_DIR";

/// `$KUBACH_DATA_DIR`; else `$KUBACH_CONFIG_DIR` (a portable or test
/// setup keeps everything in one place); else `kubach` in the OS data
/// directory: `~/Library/Application Support/kubach`,
/// `%APPDATA%\kubach` or `$XDG_DATA_HOME/kubach` (`~/.local/share/kubach`).
pub fn data_dir() -> Option<PathBuf> {
    let env = |name: &str| std::env::var_os(name).filter(|dir| !dir.is_empty());
    if let Some(dir) = env(DATA_DIR_ENV) {
        return Some(PathBuf::from(dir));
    }
    if let Some(dir) = env(crate::settings::CONFIG_DIR_ENV) {
        return Some(PathBuf::from(dir));
    }
    dirs::data_dir().map(|dir| dir.join("kubach"))
}

/// Where downloaded tools live: `tools` in [`data_dir`].
pub fn tools_dir() -> Option<PathBuf> {
    data_dir().map(|dir| dir.join("tools"))
}
