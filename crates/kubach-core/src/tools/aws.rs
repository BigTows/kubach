//! Where the AWS CLI v2 comes from (#38).
//!
//! - Linux: `awscli-exe-linux-<arch>.zip` from `awscli.amazonaws.com`
//!   with its detached OpenPGP signature (`.sig`), checked against the AWS
//!   CLI Team key published in the AWS CLI user guide and shipped with
//!   Kubach ([`AWS_KEY_FINGERPRINT`]). The whole archive is unpacked and
//!   `aws/dist/aws` is run directly (the bundled `install` script, which
//!   would write to `/usr/local`, is not used).
//! - macOS: `AWSCLIV2.pkg` (universal). Its Apple Developer ID signature
//!   is checked with `pkgutil --check-signature` (team `94KV3E626L`), then
//!   the package is expanded with `pkgutil --expand-full` and the
//!   `aws-cli` directory of its payload is kept: no installer, no
//!   administrator rights. AWS publishes no PGP signature or checksum for
//!   the package.
//! - Windows: only an MSI is published; unpacking it needs `msiexec`, so
//!   Kubach shows an install hint instead (the AWS CLI is then found in
//!   `PATH`).
//!
//! The version is not known up front (the file names carry none): it is
//! read from `aws --version` after unpacking.

use std::path::PathBuf;
use std::sync::OnceLock;

use super::cli::{Integrity, Layout, Release, TrustedKeys};
use super::download::{Arch, DownloadError, Os, Platform};
use super::signature::Keyring;

/// The AWS CLI Team public key, as published in the AWS CLI user guide
/// ("Verify the integrity and authenticity of the downloaded zip file").
const AWS_KEY: &str = include_str!("aws-cli-KEY.asc");
/// Its fingerprint as the user guide lists it (`FB5D B77F D5C1 18B8 0511
/// ADA8 A631 0ACC 4672 475C`, key ID `A6310ACC4672475C`, expires
/// 2027-07-01; a renewed key comes with a Kubach update).
pub const AWS_KEY_FINGERPRINT: &str = "FB5DB77FD5C118B80511ADA8A6310ACC4672475C";
/// Apple Developer ID team of `AWSCLIV2.pkg`.
pub const AWS_PACKAGE_TEAM: &str = "94KV3E626L";

/// The AWS CLI Team key.
pub fn keyring() -> &'static Keyring {
    static KEYRING: OnceLock<Keyring> = OnceLock::new();
    KEYRING.get_or_init(|| Keyring::from_armored(AWS_KEY))
}

pub(super) fn supported(platform: Platform) -> bool {
    platform.os != Os::Windows
}

/// `aws-cli/aws` on macOS, `aws/dist/aws` on Linux.
pub(super) fn entry(platform: Platform) -> PathBuf {
    let parts: &[&str] = match platform.os {
        Os::MacOs => &["aws-cli", "aws"],
        _ => &["aws", "dist", "aws"],
    };
    parts.iter().collect()
}

/// The current release for `platform` (no network needed).
pub fn release(platform: Platform) -> Option<Release> {
    match platform.os {
        Os::Linux => {
            let arch = match platform.arch {
                Arch::Amd64 => "x86_64",
                Arch::Arm64 => "aarch64",
            };
            let url = format!("https://awscli.amazonaws.com/awscli-exe-linux-{arch}.zip");
            Some(Release {
                version: None,
                integrity: Integrity::Signature {
                    url: format!("{url}.sig"),
                    keys: TrustedKeys::AwsCli,
                },
                url,
                layout: Layout::Tree,
            })
        }
        Os::MacOs => Some(Release {
            version: None,
            url: "https://awscli.amazonaws.com/AWSCLIV2.pkg".into(),
            integrity: Integrity::MacPackage {
                team: AWS_PACKAGE_TEAM,
            },
            layout: Layout::MacPackage {
                payload: "aws-cli.pkg/Payload/aws-cli",
                keep_as: "aws-cli",
            },
        }),
        Os::Windows => None,
    }
}

pub(super) fn resolve(platform: Platform) -> Result<Release, DownloadError> {
    release(platform).ok_or(DownloadError::UnsupportedPlatform { tool: "aws" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_key_is_the_aws_cli_team_key() {
        assert_eq!(keyring().fingerprints(), [AWS_KEY_FINGERPRINT]);
    }

    #[test]
    fn releases_per_platform() {
        let linux = release(Platform::new(Os::Linux, Arch::Arm64)).unwrap();
        assert_eq!(
            linux.url,
            "https://awscli.amazonaws.com/awscli-exe-linux-aarch64.zip"
        );
        assert!(matches!(
            linux.integrity,
            Integrity::Signature { ref url, .. }
                if url == "https://awscli.amazonaws.com/awscli-exe-linux-aarch64.zip.sig"
        ));
        assert_eq!(linux.layout, Layout::Tree);
        let x86 = release(Platform::new(Os::Linux, Arch::Amd64)).unwrap();
        assert!(x86.url.ends_with("awscli-exe-linux-x86_64.zip"));
        let mac = release(Platform::new(Os::MacOs, Arch::Amd64)).unwrap();
        assert_eq!(mac.url, "https://awscli.amazonaws.com/AWSCLIV2.pkg");
        assert_eq!(mac.integrity, Integrity::MacPackage { team: "94KV3E626L" });
        assert_eq!(release(Platform::new(Os::Windows, Arch::Amd64)), None);
    }
}
