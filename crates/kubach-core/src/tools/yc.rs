//! Where the Yandex Cloud CLI comes from (#38).
//!
//! The current version is in `release/stable` of the `yandexcloud-yc`
//! bucket (Yandex's own `install.sh` reads it there); each release has a
//! `.tar.gz` (`.zip` on Windows) with the single `yc` binary per platform.
//!
//! Yandex publishes **no checksum or signature** for `yc` (neither next to
//! the files nor in the bucket listing, checked October 2026; `install.sh`
//! checks nothing either). The download is therefore protected only by
//! HTTPS from `storage.yandexcloud.net`, and Kubach records and shows that
//! it was not checked ([`Integrity::None`]). There is no Windows Arm build.

use super::cli::{Integrity, Layout, Release};
use super::download::{self, Arch, DownloadError, Os, Platform};

const BUCKET: &str = "https://storage.yandexcloud.net/yandexcloud-yc";
/// "1.42.0\n".
const MAX_STABLE_BYTES: u64 = 64;

pub(super) fn supported(platform: Platform) -> bool {
    !matches!(
        platform,
        Platform {
            os: Os::Windows,
            arch: Arch::Arm64
        }
    )
}

/// `<bucket>/release/stable`.
pub fn stable_url() -> String {
    format!("{BUCKET}/release/stable")
}

/// The version in `release/stable`, if it is a plain `X.Y.Z`.
pub fn parse_stable(text: &str) -> Option<String> {
    let version = text.trim();
    let parts: Vec<&str> = version.split('.').collect();
    let plain = parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 6 && p.bytes().all(|b| b.is_ascii_digit()));
    plain.then(|| version.to_owned())
}

/// The release archive of `version` for `platform`.
pub fn release(version: &str, platform: Platform) -> Release {
    let (os, arch) = (platform.go_os(), platform.go_arch());
    let ext = if platform.os == Os::Windows {
        "zip"
    } else {
        "tar.gz"
    };
    Release {
        version: Some(version.to_owned()),
        url: format!("{BUCKET}/release/{version}/{os}/{arch}/yc_{version}_{os}_{arch}.{ext}"),
        integrity: Integrity::None,
        layout: Layout::Single {
            member: format!("yc{}", platform.exe_suffix()),
        },
    }
}

pub(super) async fn resolve(platform: Platform) -> Result<Release, DownloadError> {
    let url = stable_url();
    let mut body = Vec::new();
    download::fetch(&url, &mut body, MAX_STABLE_BYTES, |_| {}).await?;
    let version = parse_stable(&String::from_utf8_lossy(&body))
        .ok_or_else(|| DownloadError::InvalidUrl(format!("{url}: no version")))?;
    Ok(release(&version, platform))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_version() {
        assert_eq!(parse_stable("1.42.0\n").as_deref(), Some("1.42.0"));
        assert_eq!(parse_stable("<Error>NoSuchKey</Error>"), None);
        assert_eq!(parse_stable("1.42"), None);
        assert_eq!(parse_stable("../1.2.3"), None);
    }

    #[test]
    fn archives_per_platform() {
        let mac = release("1.42.0", Platform::new(Os::MacOs, Arch::Arm64));
        assert_eq!(
            mac.url,
            "https://storage.yandexcloud.net/yandexcloud-yc/release/1.42.0/darwin/arm64/\
             yc_1.42.0_darwin_arm64.tar.gz"
        );
        assert_eq!(mac.integrity, Integrity::None);
        assert_eq!(
            mac.layout,
            Layout::Single {
                member: "yc".into()
            }
        );
        let windows = release("1.42.0", Platform::new(Os::Windows, Arch::Amd64));
        assert_eq!(
            windows.url,
            "https://storage.yandexcloud.net/yandexcloud-yc/release/1.42.0/windows/amd64/\
             yc_1.42.0_windows_amd64.zip"
        );
        assert_eq!(
            windows.layout,
            Layout::Single {
                member: "yc.exe".into()
            }
        );
        let linux = release("1.42.0", Platform::new(Os::Linux, Arch::Arm64));
        assert!(
            linux
                .url
                .ends_with("/linux/arm64/yc_1.42.0_linux_arm64.tar.gz")
        );
        assert!(!supported(Platform::new(Os::Windows, Arch::Arm64)));
    }
}
