//! Where the Google Cloud CLI comes from (#38).
//!
//! Google publishes the SHA-256 of the current archive of every platform
//! on its "versioned archives" page ([`PAGE_URL`]); Kubach reads the page,
//! takes the archive's address and hash from the row of this platform and
//! the version from the name of the Windows bundle, then downloads the
//! archive from `dl.google.com` and checks it against that hash. Nothing
//! is pinned: the page always describes the current release. If Google
//! updates the archive before the page, the hashes differ and the download
//! fails with a mismatch (try again later).
//!
//! On Windows the bundle with Python is used, so no Python is needed; the
//! Linux archives bundle Python too. The macOS archive has none: gcloud
//! needs Python 3.10+ on the machine (python.org or Homebrew), otherwise
//! it does not start.

use std::path::PathBuf;

use super::cli::{Integrity, Layout, Release};
use super::download::{self, Arch, DownloadError, Os, Platform};

/// The page with the archives and their SHA-256.
pub const PAGE_URL: &str = "https://docs.cloud.google.com/sdk/docs/downloads-versioned-archives";
/// Archives are served from here (the page links them).
const DOWNLOAD_PREFIX: &str = "https://dl.google.com/dl/cloudsdk/channels/rapid/downloads/";
/// The page is about 200 KB.
const MAX_PAGE_BYTES: u64 = 4 * 1024 * 1024;
/// The GKE exec plugin installed into the SDK.
pub const GKE_PLUGIN: &str = "gke-gcloud-auth-plugin";

pub(super) fn supported(platform: Platform) -> bool {
    archive_name(platform, "0").is_some()
}

/// `google-cloud-sdk/bin/gcloud` (`gcloud.cmd` on Windows).
pub(super) fn entry(platform: Platform) -> PathBuf {
    let name = if platform.os == Os::Windows {
        "gcloud.cmd"
    } else {
        "gcloud"
    };
    ["google-cloud-sdk", "bin", name].iter().collect()
}

/// The archive of `platform` as named on the page; `version` is only
/// part of the Windows bundle's name. Windows on Arm has no archive with
/// a published checksum.
pub fn archive_name(platform: Platform, version: &str) -> Option<String> {
    let name = match (platform.os, platform.arch) {
        (Os::MacOs, Arch::Arm64) => "google-cloud-cli-darwin-arm.tar.gz".to_owned(),
        (Os::MacOs, Arch::Amd64) => "google-cloud-cli-darwin-x86_64.tar.gz".to_owned(),
        (Os::Linux, Arch::Arm64) => "google-cloud-cli-linux-arm.tar.gz".to_owned(),
        (Os::Linux, Arch::Amd64) => "google-cloud-cli-linux-x86_64.tar.gz".to_owned(),
        (Os::Windows, Arch::Amd64) => {
            format!("google-cloud-sdk-{version}-windows-x86_64-bundled-python.zip")
        }
        (Os::Windows, Arch::Arm64) => return None,
    };
    Some(name)
}

/// The current release for `platform` on the page `html`.
pub fn parse_page(html: &str, platform: Platform) -> Option<Release> {
    let version = page_version(html)?;
    let name = archive_name(platform, &version)?;
    for row in html.split("<tr").skip(1) {
        let row = row.split("</tr>").next().unwrap_or(row);
        let Some(href) = row
            .split("href=\"")
            .skip(1)
            .filter_map(|rest| rest.split('"').next())
            .find(|href| href.rsplit('/').next() == Some(name.as_str()))
        else {
            continue;
        };
        // Only Google's download server, whatever the page links.
        let url = format!("{DOWNLOAD_PREFIX}{name}");
        if href != url {
            return None;
        }
        let expected = hashes(row).next()?;
        return Some(Release {
            version: Some(version),
            url,
            integrity: Integrity::Sha256 {
                expected,
                source: PAGE_URL.to_owned(),
            },
            layout: Layout::Tree,
        });
    }
    None
}

/// 64-digit hex words of `text`, lowercase.
fn hashes(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_ascii_hexdigit())
        .filter(|word| word.len() == 64)
        .map(str::to_ascii_lowercase)
}

/// The version in the name of the Windows bundle
/// (`google-cloud-sdk-591.0.0-windows-x86_64-bundled-python.zip`).
fn page_version(html: &str) -> Option<String> {
    const PREFIX: &str = "google-cloud-sdk-";
    const SUFFIX: &str = "-windows-x86_64-bundled-python.zip";
    html.match_indices(PREFIX).find_map(|(ix, _)| {
        let rest = &html[ix + PREFIX.len()..];
        let version = &rest[..rest.find(SUFFIX)?];
        let parts: Vec<&str> = version.split('.').collect();
        let plain = parts.len() == 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.len() <= 6 && p.bytes().all(|b| b.is_ascii_digit()));
        plain.then(|| version.to_owned())
    })
}

/// Reads the page and picks the release for `platform`.
pub(super) async fn resolve(platform: Platform) -> Result<Release, DownloadError> {
    let mut page = Vec::new();
    download::fetch(PAGE_URL, &mut page, MAX_PAGE_BYTES, |_| {}).await?;
    let name = archive_name(platform, "<version>").unwrap_or_default();
    parse_page(&String::from_utf8_lossy(&page), platform).ok_or(DownloadError::ChecksumMissing {
        file: name,
        url: PAGE_URL.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table of the page, October 2026 (Cloud SDK 591.0.0). The
    /// darwin-arm hash was checked against the downloaded archive.
    const PAGE: &str = include_str!("fixtures/gcloud-versioned-archives.html");

    fn sha(release: &Release) -> &str {
        match &release.integrity {
            Integrity::Sha256 { expected, .. } => expected,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn release_per_platform_from_the_page() {
        let mac = parse_page(PAGE, Platform::new(Os::MacOs, Arch::Arm64)).unwrap();
        assert_eq!(mac.version.as_deref(), Some("591.0.0"));
        assert_eq!(
            mac.url,
            "https://dl.google.com/dl/cloudsdk/channels/rapid/downloads/\
             google-cloud-cli-darwin-arm.tar.gz"
        );
        assert_eq!(
            mac.integrity,
            Integrity::Sha256 {
                expected: "3b7bbaaf2dd0287fab5320359fb61c678cbd6e8d6d58b884470def1bf96589a1".into(),
                source: PAGE_URL.into(),
            }
        );
        assert_eq!(mac.layout, Layout::Tree);

        let linux = parse_page(PAGE, Platform::new(Os::Linux, Arch::Amd64)).unwrap();
        assert!(linux.url.ends_with("google-cloud-cli-linux-x86_64.tar.gz"));
        assert_eq!(
            sha(&linux),
            "8b8c1be16284f7072359de05d31fe5ce52b6052fb295404a62a54b7d03ec4326"
        );
        let arm = parse_page(PAGE, Platform::new(Os::Linux, Arch::Arm64)).unwrap();
        assert_eq!(
            sha(&arm),
            "c134c741f7232131e2556d77991bc8eaeb70054cfad426e06c533b3586ab69e6"
        );
        let intel = parse_page(PAGE, Platform::new(Os::MacOs, Arch::Amd64)).unwrap();
        assert_eq!(
            sha(&intel),
            "c2381ccfdeb7ed1c0a7186a8d030c317375092232d8135331d826eece6bc48f5"
        );

        let windows = parse_page(PAGE, Platform::new(Os::Windows, Arch::Amd64)).unwrap();
        assert!(
            windows
                .url
                .ends_with("google-cloud-sdk-591.0.0-windows-x86_64-bundled-python.zip")
        );
        assert_eq!(
            sha(&windows),
            "f011188d735a63ee5278ce41e6f1a9388dbd17232238be31a1c2cfc39933bada"
        );
        assert_eq!(
            parse_page(PAGE, Platform::new(Os::Windows, Arch::Arm64)),
            None
        );
        assert!(!supported(Platform::new(Os::Windows, Arch::Arm64)));
    }

    #[test]
    fn foreign_links_and_garbage_are_refused() {
        let mac = Platform::new(Os::MacOs, Arch::Arm64);
        let evil = PAGE.replace(
            "https://dl.google.com/dl/cloudsdk/channels/rapid/downloads/\
             google-cloud-cli-darwin-arm.tar.gz",
            "https://evil.example.com/google-cloud-cli-darwin-arm.tar.gz",
        );
        assert_eq!(parse_page(&evil, mac), None);
        assert_eq!(parse_page("<html>moved</html>", mac), None);
        let no_hash = PAGE.replace(
            "3b7bbaaf2dd0287fab5320359fb61c678cbd6e8d6d58b884470def1bf96589a1",
            "",
        );
        assert_eq!(parse_page(&no_hash, mac), None);
    }
}
