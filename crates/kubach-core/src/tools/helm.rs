//! The Helm release Kubach downloads when `helm` is not installed (#131),
//! its latest version and the signature check (#403).
//!
//! Two release lines can be downloaded: Helm 4 (the default) and Helm 3.
//! The version comes from `get.helm.sh/helm-latest-version` (or
//! `helm3-latest-version`), checked at most once a day ([`LATEST_TTL`])
//! and on request. A version other than the one pinned here must carry an
//! OpenPGP signature by a Helm maintainer key shipped with Kubach; when the
//! latest version can't be learned, fetched or verified, the pinned
//! version with its pinned SHA-256 is installed instead and the reason is
//! reported ([`HelmInstall::fallback`]).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::download::{
    self, Arch, Artifact, DownloadError, Installed, Os, Platform, Progress, SignatureSpec,
    ToolSpec, Verification,
};
use super::signature::Keyring;

/// The latest version is asked for at most this often, unless the user
/// asks.
pub const LATEST_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Where the latest known versions are kept, in the tools directory.
const LATEST_FILE: &str = "helm-latest.json";
/// A `*-latest-version` answer is a few bytes.
const MAX_LATEST_BYTES: u64 = 256;

/// A Helm release line Kubach can download.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HelmLine {
    /// Helm 4, the current line.
    #[default]
    #[serde(rename = "4")]
    Helm4,
    /// Helm 3, supported with fixes only (no more minor releases).
    #[serde(rename = "3")]
    Helm3,
}

impl HelmLine {
    pub const ALL: [HelmLine; 2] = [HelmLine::Helm4, HelmLine::Helm3];

    pub fn major(self) -> u64 {
        match self {
            Self::Helm4 => 4,
            Self::Helm3 => 3,
        }
    }

    /// "Helm 4".
    pub fn title(self) -> &'static str {
        match self {
            Self::Helm4 => "Helm 4",
            Self::Helm3 => "Helm 3",
        }
    }

    /// The release pinned in Kubach for this line: the fallback.
    pub fn pinned(self) -> &'static ToolSpec {
        match self {
            Self::Helm4 => &HELM4,
            Self::Helm3 => &HELM3,
        }
    }

    /// Where get.helm.sh names the latest release of the line.
    pub fn latest_url(self) -> &'static str {
        match self {
            Self::Helm4 => "https://get.helm.sh/helm-latest-version",
            Self::Helm3 => "https://get.helm.sh/helm3-latest-version",
        }
    }

    /// The line of a version such as `4.3.1` or `v3.22.1+g…`.
    pub fn of_version(version: &str) -> Option<Self> {
        match download::version_key(version).first()? {
            4 => Some(Self::Helm4),
            3 => Some(Self::Helm3),
            _ => None,
        }
    }
}

/// Helm 4 releases from get.helm.sh. The pinned version is the fallback
/// when the latest one can't be fetched or verified; it is raised with
/// Kubach releases. The hashes are those published with the release,
/// checked against both get.helm.sh and the GitHub release notes.
pub static HELM4: ToolSpec = ToolSpec {
    name: "helm",
    title: "Helm",
    version: "4.3.1",
    artifacts: &[
        Artifact {
            platform: Platform::new(Os::MacOs, Arch::Amd64),
            sha256: "34bec4eba761ba5c16d1965ee6b8f5b0abe5993974a802274e2a4f0418893d89",
        },
        Artifact {
            platform: Platform::new(Os::MacOs, Arch::Arm64),
            sha256: "6b4e61c8cd27d99e5a801e63ff25dd5c70b36875c2cd659d3e9ce6b76fc23198",
        },
        Artifact {
            platform: Platform::new(Os::Linux, Arch::Amd64),
            sha256: "0403a88ae20913d11b10a2467b50087721fea7581736da6b9d3992f5357fe54b",
        },
        Artifact {
            platform: Platform::new(Os::Linux, Arch::Arm64),
            sha256: "8cf8f66ecbc252ac8f7f06cca68aba88508ddfde3a6a34feb1678ad1694b4f88",
        },
        Artifact {
            platform: Platform::new(Os::Windows, Arch::Amd64),
            sha256: "fcfd7b796dcf0f225abc40b5636a3b01f55f3de2fb1dfb18475df94e62c14112",
        },
        Artifact {
            platform: Platform::new(Os::Windows, Arch::Arm64),
            sha256: "373edaae86db70342ef39b014e2e7764ea8f9cff08714843daa5e55b5ee492f2",
        },
    ],
    archive_url,
    checksum_url,
    member,
    signature: Some(SIGNATURES),
};

/// Helm 3 releases from get.helm.sh; Helm ≥ 3.13 is needed for
/// `--dry-run=server` (ADR 0006). Hashes as for [`HELM4`].
pub static HELM3: ToolSpec = ToolSpec {
    name: "helm",
    title: "Helm",
    version: "3.22.1",
    artifacts: &[
        Artifact {
            platform: Platform::new(Os::MacOs, Arch::Amd64),
            sha256: "cf850a81b908f8eef0331e6b18776da2767bcac639ca84472311dad20008495c",
        },
        Artifact {
            platform: Platform::new(Os::MacOs, Arch::Arm64),
            sha256: "e73da8a0b0d5193774fdff17ce6319c7436bb22ee229b45fd64f3c40368c753a",
        },
        Artifact {
            platform: Platform::new(Os::Linux, Arch::Amd64),
            sha256: "896cf63684a42a723d61c980369d6e588330927deb13676ca0d85006dd6da0ea",
        },
        Artifact {
            platform: Platform::new(Os::Linux, Arch::Arm64),
            sha256: "4c29e286b279d170fa048ddfc3e4e83dcd069ca9c52102766ba672d7149e694d",
        },
        Artifact {
            platform: Platform::new(Os::Windows, Arch::Amd64),
            sha256: "32ce9f4509b81782c9740898c43946e1f97c4d93ec5d10222c9b6eeec21ea043",
        },
        Artifact {
            platform: Platform::new(Os::Windows, Arch::Arm64),
            sha256: "10104b049309d4c751bca88254843e16edd84141411f63503f10845ea33cbd2f",
        },
    ],
    archive_url,
    checksum_url,
    member,
    signature: Some(SIGNATURES),
};

/// Helm signs every release file with a maintainer's key; the `.asc`
/// files are attached to the GitHub release (get.helm.sh has none).
const SIGNATURES: SignatureSpec = SignatureSpec {
    archive_url: signature_url,
    checksum_url: checksum_signature_url,
    keyring,
};

/// `KEYS` from github.com/helm/helm, verbatim, at commit
/// 5638c35399464b6432ba81b92a341218991efa5c (the latest change to the
/// file when this was written).
const HELM_KEYS: &str = include_str!("helm-KEYS");

/// Fingerprints of the primary keys in [`HELM_KEYS`] that load (a test
/// keeps the two in step). Releases so far are signed with Scott Rigby's
/// key `208D D36E D5BB 3745 A167 43A4 C7C6 FBB5 B91C 1155`: the release
/// notes on GitHub name it, his Keybase account (r6by) publishes it and
/// keys.openpgp.org serves it under that fingerprint.
pub const HELM_KEY_FINGERPRINTS: &[&str] = &[
    "672C657BE06B4B30969C4A57461449C25E36B98E", // Matthew Farina
    "F1261BDE929012C8FF2E501D6EA5D7598529A53E", // Martin Hickey
    "967F8AC5E2216F9F4FD270AD92AA783CBAAE8E3B", // Matthew Fisher
    "76939899B137D575D3274E756DCCB9D752D35BA8", // Taylor Thomas (expired 2022)
    "49D09C86C3DC8DA3F0A076221EF612347F8A9958", // Adam Reese
    "ABA2529598F6626C420D335B62F49E747D911B60", // Matt Butcher
    "4AB45F1CB0D292975C6371436E2A23D806B6E6DD", // Matt Butcher
    "208DD36ED5BB3745A16743A4C7C6FBB5B91C1155", // Scott Rigby
    "7FEC81FACC7FFB2A010ADD13C2D40F4D8196E874", // Robert Sirchia
    "BF888333D96A1C18E2682AAED79D67C9EC016739", // George Jenkins
];

/// The Helm maintainer keys Kubach trusts.
pub fn keyring() -> &'static Keyring {
    static KEYRING: OnceLock<Keyring> = OnceLock::new();
    KEYRING.get_or_init(|| Keyring::from_armored(HELM_KEYS))
}

/// `https://get.helm.sh/helm-v<ver>-<os>-<arch>.tar.gz` (`.zip` on Windows).
fn archive_url(version: &str, platform: Platform) -> String {
    format!("https://get.helm.sh/{}", archive_name(version, platform))
}

fn archive_name(version: &str, platform: Platform) -> String {
    let ext = if platform.os == Os::Windows {
        "zip"
    } else {
        "tar.gz"
    };
    format!(
        "helm-v{version}-{}-{}.{ext}",
        platform.go_os(),
        platform.go_arch()
    )
}

fn checksum_url(version: &str, platform: Platform) -> String {
    format!("{}.sha256sum", archive_url(version, platform))
}

/// `https://github.com/helm/helm/releases/download/v<ver>/<archive>.asc`.
fn signature_url(version: &str, platform: Platform) -> String {
    format!(
        "https://github.com/helm/helm/releases/download/v{version}/{}.asc",
        archive_name(version, platform)
    )
}

fn checksum_signature_url(version: &str, platform: Platform) -> String {
    format!(
        "https://github.com/helm/helm/releases/download/v{version}/{}.sha256sum.asc",
        archive_name(version, platform)
    )
}

/// `<os>-<arch>/helm[.exe]`.
fn member(platform: Platform) -> String {
    format!(
        "{}-{}/helm{}",
        platform.go_os(),
        platform.go_arch(),
        platform.exe_suffix()
    )
}

/// The version in a `*-latest-version` answer (`v4.3.1\n` → `4.3.1`), if
/// it is a plain `X.Y.Z` release of `line`.
pub fn parse_latest(text: &str, line: HelmLine) -> Option<String> {
    let version = text.trim().strip_prefix('v')?;
    let parts: Vec<&str> = version.split('.').collect();
    let plain = parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty() && part.len() <= 6 && part.bytes().all(|b| b.is_ascii_digit())
        });
    (plain && HelmLine::of_version(version) == Some(line)).then(|| version.to_owned())
}

/// The latest versions of both lines and when they were asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LatestVersions {
    /// Unix seconds of the check.
    pub checked_at: u64,
    pub helm4: Option<String>,
    pub helm3: Option<String>,
}

impl LatestVersions {
    pub fn get(&self, line: HelmLine) -> Option<&str> {
        match line {
            HelmLine::Helm4 => self.helm4.as_deref(),
            HelmLine::Helm3 => self.helm3.as_deref(),
        }
    }

    /// Checked less than [`LATEST_TTL`] before `now` (and not in the
    /// future: a clock set back makes it stale).
    pub fn is_fresh(&self, now: u64) -> bool {
        self.checked_at <= now.saturating_add(60)
            && now.saturating_sub(self.checked_at) < LATEST_TTL.as_secs()
    }

    /// The saved check in the tools directory `root`, if any.
    pub fn load(root: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(root.join(LATEST_FILE)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Saves the check into `root`, atomically.
    pub fn save(&self, root: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(root)?;
        let mut file = tempfile::NamedTempFile::new_in(root)?;
        serde_json::to_writer(&mut file, self).map_err(std::io::Error::other)?;
        file.persist(root.join(LATEST_FILE))
            .map(|_| ())
            .map_err(|error| error.error)
    }
}

/// Now in Unix seconds.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Asks get.helm.sh for the latest version of both lines. An error only
/// when neither answered; a line that did not answer (or answered
/// nonsense) stays `None`.
pub async fn check_latest() -> Result<LatestVersions, DownloadError> {
    let (helm4, helm3) =
        futures::join!(fetch_latest(HelmLine::Helm4), fetch_latest(HelmLine::Helm3));
    if let (Err(error), Err(_)) = (&helm4, &helm3) {
        return Err(clone_error(error));
    }
    Ok(LatestVersions {
        checked_at: unix_now(),
        helm4: helm4.ok().flatten(),
        helm3: helm3.ok().flatten(),
    })
}

async fn fetch_latest(line: HelmLine) -> Result<Option<String>, DownloadError> {
    let mut body = Vec::new();
    download::fetch(line.latest_url(), &mut body, MAX_LATEST_BYTES, |_| {}).await?;
    Ok(parse_latest(&String::from_utf8_lossy(&body), line))
}

/// `DownloadError` is not `Clone` (it holds `io::Error`); the message is
/// what matters here.
fn clone_error(error: &DownloadError) -> DownloadError {
    match error {
        DownloadError::Network { host, message } => DownloadError::Network {
            host: host.clone(),
            message: message.clone(),
        },
        DownloadError::Http { url, status } => DownloadError::Http {
            url: url.clone(),
            status: *status,
        },
        other => DownloadError::Io(std::io::Error::other(other.to_string())),
    }
}

/// The version of `line` to download: the latest known one, unless it is
/// older than the pinned one (a stale or rolled-back answer).
pub fn available(line: HelmLine, latest: Option<&LatestVersions>) -> String {
    let pinned = line.pinned().version;
    match latest.and_then(|latest| latest.get(line)) {
        Some(latest) if download::version_key(latest) > download::version_key(pinned) => {
            latest.to_owned()
        }
        _ => pinned.to_owned(),
    }
}

/// A finished Helm download.
#[derive(Clone, Debug)]
pub struct HelmInstall {
    pub installed: Installed,
    pub verification: Verification,
    /// Why the pinned version was installed instead of the latest one.
    pub fallback: Option<String>,
    /// The latest versions, when they were checked for this download.
    pub latest: Option<LatestVersions>,
}

/// Downloads the latest release of `line` into the tools directory
/// `root`, or the pinned one when that fails (see the module docs).
/// `known` is the last check; it is reused while fresh.
pub async fn download_line(
    line: HelmLine,
    root: &Path,
    known: Option<LatestVersions>,
    mut progress: impl FnMut(Progress) + Send,
) -> Result<HelmInstall, DownloadError> {
    let known = known.filter(|known| known.is_fresh(unix_now()));
    let steps = Network {
        root,
        progress: &mut progress,
    };
    download_line_with(line, known, steps).await
}

/// The network side of [`download_line`], replaced in tests.
trait LineSteps {
    /// Asks for the latest versions (and keeps the answer).
    async fn check(&mut self) -> Result<LatestVersions, DownloadError>;
    /// Downloads and checks `version` of `spec`.
    async fn download(
        &mut self,
        spec: &'static ToolSpec,
        version: &str,
    ) -> Result<(Installed, Verification), DownloadError>;
}

struct Network<'a, P> {
    root: &'a Path,
    progress: &'a mut P,
}

impl<P: FnMut(Progress) + Send> LineSteps for Network<'_, P> {
    async fn check(&mut self) -> Result<LatestVersions, DownloadError> {
        let latest = check_latest().await?;
        latest.save(self.root).ok();
        Ok(latest)
    }

    async fn download(
        &mut self,
        spec: &'static ToolSpec,
        version: &str,
    ) -> Result<(Installed, Verification), DownloadError> {
        download::download_version(spec, version, self.root, &mut *self.progress).await
    }
}

/// [`download_line`] with its network steps passed in. `known` is a fresh
/// earlier check or `None` to ask with [`LineSteps::check`].
async fn download_line_with(
    line: HelmLine,
    known: Option<LatestVersions>,
    mut steps: impl LineSteps,
) -> Result<HelmInstall, DownloadError> {
    let spec = line.pinned();
    let mut checked = None;
    let mut problem = None;
    let latest = match known {
        Some(known) => Some(known),
        None => match steps.check().await {
            Ok(latest) => {
                checked = Some(latest.clone());
                Some(latest)
            }
            Err(error) => {
                problem = Some(format!("Can't check the latest {}: {error}", line.title()));
                None
            }
        },
    };
    let target = available(line, latest.as_ref());
    if target != spec.version {
        match steps.download(spec, &target).await {
            Ok((installed, verification)) => {
                return Ok(HelmInstall {
                    installed,
                    verification,
                    fallback: None,
                    latest: checked,
                });
            }
            Err(error) if error.is_release_problem() => {
                problem = Some(format!("Helm {target} was not installed: {error}"));
            }
            Err(error) => return Err(error),
        }
    }
    let (installed, verification) = steps.download(spec, spec.version).await?;
    let fallback = problem.map(|problem| {
        format!(
            "{problem}. Installed Helm {} pinned in Kubach instead, checked by its SHA-256.",
            spec.version
        )
    });
    Ok(HelmInstall {
        installed,
        verification,
        fallback,
        latest: checked,
    })
}

/// The `helm` Kubach downloaded, if any (in [`super::tools_dir`]): the
/// newest version there.
pub fn downloaded() -> Option<Installed> {
    downloaded_in(&super::tools_dir()?)
}

/// The newest downloaded `helm` under the tools directory `root`.
pub fn downloaded_in(root: &Path) -> Option<Installed> {
    super::installed_versions(&HELM4, root).into_iter().next()
}

/// Path of the downloaded `helm`, for the lookup in
/// [`Tools::helm`](crate::settings::Tools::helm).
pub fn downloaded_path() -> Option<PathBuf> {
    downloaded().map(|installed| installed.path)
}

/// `helm version --short` of the binary at `path`, e.g. `v3.22.1+g…`;
/// `None` when it does not run or answer in time.
pub async fn binary_version(path: &Path) -> Option<String> {
    let mut command = tokio::process::Command::new(path);
    command.args(["version", "--short"]);
    let output = crate::process::run(command, Duration::from_secs(10))
        .await
        .ok()?;
    let text = output.stdout_text().trim().to_owned();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::super::signature::SignatureError;
    use super::*;

    const ALL: [Platform; 6] = [
        Platform::new(Os::MacOs, Arch::Amd64),
        Platform::new(Os::MacOs, Arch::Arm64),
        Platform::new(Os::Linux, Arch::Amd64),
        Platform::new(Os::Linux, Arch::Arm64),
        Platform::new(Os::Windows, Arch::Amd64),
        Platform::new(Os::Windows, Arch::Arm64),
    ];

    #[test]
    fn every_platform_has_an_artifact() {
        for spec in [&HELM4, &HELM3] {
            for platform in ALL {
                let artifact = spec.artifact(platform).expect("artifact");
                assert_eq!(artifact.sha256.len(), 64);
                assert!(artifact.sha256.chars().all(|c| c.is_ascii_hexdigit()));
                assert_eq!(artifact.sha256, artifact.sha256.to_ascii_lowercase());
            }
            assert!(Platform::current().is_none_or(|p| spec.artifact(p).is_some()));
        }
    }

    #[test]
    fn urls_and_members_per_platform() {
        let mac = Platform::new(Os::MacOs, Arch::Arm64);
        assert_eq!(
            HELM3.url(mac).unwrap(),
            "https://get.helm.sh/helm-v3.22.1-darwin-arm64.tar.gz"
        );
        assert_eq!(
            (HELM3.checksum_url)(HELM3.version, mac),
            "https://get.helm.sh/helm-v3.22.1-darwin-arm64.tar.gz.sha256sum"
        );
        assert_eq!((HELM3.member)(mac), "darwin-arm64/helm");
        let signatures = HELM4.signature.as_ref().unwrap();
        assert_eq!(
            (signatures.archive_url)("4.3.1", mac),
            "https://github.com/helm/helm/releases/download/v4.3.1/helm-v4.3.1-darwin-arm64.tar.gz.asc"
        );
        assert_eq!(
            (signatures.checksum_url)("4.3.1", mac),
            "https://github.com/helm/helm/releases/download/v4.3.1/\
             helm-v4.3.1-darwin-arm64.tar.gz.sha256sum.asc"
        );

        let linux = Platform::new(Os::Linux, Arch::Amd64);
        assert_eq!(
            HELM4.url(linux).unwrap(),
            "https://get.helm.sh/helm-v4.3.1-linux-amd64.tar.gz"
        );
        assert_eq!((HELM4.member)(linux), "linux-amd64/helm");

        let windows = Platform::new(Os::Windows, Arch::Arm64);
        assert_eq!(
            HELM4.url(windows).unwrap(),
            "https://get.helm.sh/helm-v4.3.1-windows-arm64.zip"
        );
        assert_eq!((HELM4.member)(windows), "windows-arm64/helm.exe");
        assert_eq!(HELM4.binary_name(windows), "helm.exe");
    }

    #[test]
    fn pinned_versions_match_their_lines() {
        assert_eq!(HelmLine::of_version(HELM4.version), Some(HelmLine::Helm4));
        assert_eq!(HelmLine::of_version(HELM3.version), Some(HelmLine::Helm3));
        // Helm 3 needs 3.13 for `--dry-run=server`.
        assert!(download::version_key(HELM3.version) >= vec![3, 13]);
        assert_eq!(HelmLine::default(), HelmLine::Helm4);
        assert_eq!(HelmLine::Helm3.pinned().version, "3.22.1");
        assert_eq!(
            HelmLine::of_version("v3.22.1+g1a2b3c"),
            Some(HelmLine::Helm3)
        );
        assert_eq!(HelmLine::of_version("2.17.0"), None);
    }

    #[test]
    fn parses_latest_version_answers() {
        assert_eq!(
            parse_latest("v4.3.1\n", HelmLine::Helm4).as_deref(),
            Some("4.3.1")
        );
        assert_eq!(
            parse_latest("v3.22.1", HelmLine::Helm3).as_deref(),
            Some("3.22.1")
        );
        // The wrong line, pre-releases, garbage and path tricks are refused.
        for (text, line) in [
            ("v4.3.1", HelmLine::Helm3),
            ("v3.22.1", HelmLine::Helm4),
            ("v4.4.0-rc.1", HelmLine::Helm4),
            ("4.3.1", HelmLine::Helm4),
            ("v4.3", HelmLine::Helm4),
            ("v4.3.1.2", HelmLine::Helm4),
            ("v4..1", HelmLine::Helm4),
            ("v4.3.1/../x", HelmLine::Helm4),
            ("<html>Not found</html>", HelmLine::Helm4),
            ("", HelmLine::Helm4),
        ] {
            assert_eq!(parse_latest(text, line), None, "{text}");
        }
    }

    #[test]
    fn available_never_goes_below_the_pinned_version() {
        let latest = LatestVersions {
            checked_at: 0,
            helm4: Some("4.10.0".into()),
            helm3: Some("3.9.0".into()),
        };
        assert_eq!(available(HelmLine::Helm4, Some(&latest)), "4.10.0");
        assert_eq!(available(HelmLine::Helm3, Some(&latest)), HELM3.version);
        assert_eq!(available(HelmLine::Helm4, None), HELM4.version);
        let unknown = LatestVersions::default();
        assert_eq!(available(HelmLine::Helm3, Some(&unknown)), HELM3.version);
    }

    #[test]
    fn latest_check_is_cached_for_a_day() {
        let day = LATEST_TTL.as_secs();
        let latest = LatestVersions {
            checked_at: 1_000_000,
            ..Default::default()
        };
        assert!(latest.is_fresh(1_000_000));
        assert!(latest.is_fresh(1_000_000 + day - 1));
        assert!(!latest.is_fresh(1_000_000 + day));
        // A check "from the future" (the clock went back) is stale.
        assert!(!latest.is_fresh(1_000_000 - 3600));

        let root = tempfile::tempdir().unwrap();
        assert_eq!(LatestVersions::load(root.path()), None);
        let saved = LatestVersions {
            checked_at: 42,
            helm4: Some("4.3.1".into()),
            helm3: None,
        };
        saved.save(root.path()).unwrap();
        assert_eq!(LatestVersions::load(root.path()), Some(saved));
        // The file does not look like a downloaded version.
        assert_eq!(downloaded_in(root.path()), None);
    }

    #[test]
    fn line_setting_names() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct S {
            line: HelmLine,
        }
        let text = toml::to_string(&S {
            line: HelmLine::Helm3,
        })
        .unwrap();
        assert_eq!(text.trim(), "line = \"3\"");
        let parsed: S = toml::from_str("line = \"4\"").unwrap();
        assert_eq!(parsed.line, HelmLine::Helm4);
        assert!(toml::from_str::<S>("line = \"2\"").is_err());
    }

    #[test]
    fn keyring_holds_the_helm_maintainer_keys() {
        assert_eq!(keyring().fingerprints(), HELM_KEY_FINGERPRINTS);
    }

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/tools/fixtures");

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{FIXTURES}/{name}")).unwrap()
    }

    /// The real `.sha256sum` files of three releases and their real
    /// signatures from the GitHub releases (4.2.3 was signed by another
    /// maintainer).
    #[test]
    fn verifies_real_release_signatures() {
        let scott = (
            "208DD36ED5BB3745A16743A4C7C6FBB5B91C1155",
            "Scott Rigby <scott@r6by.com>",
        );
        let matt = (
            "672C657BE06B4B30969C4A57461449C25E36B98E",
            "Matthew Farina <matt@mattfarina.com>",
        );
        for (version, (fingerprint, user)) in [("4.3.1", scott), ("3.22.1", scott), ("4.2.3", matt)]
        {
            let name = format!("helm-v{version}-darwin-arm64.tar.gz.sha256sum");
            let checksum = fixture(&name);
            let signature = fixture(&format!("{name}.asc"));
            let signer = keyring().verify(&signature, &checksum[..]).unwrap();
            assert_eq!(signer.fingerprint, fingerprint, "{version}");
            assert_eq!(signer.user_id, user, "{version}");
            assert_eq!(
                download::parse_checksum(
                    &String::from_utf8_lossy(&checksum),
                    &archive_name(version, Platform::new(Os::MacOs, Arch::Arm64))
                )
                .map(|hash| hash.len()),
                Some(64)
            );
        }
        // The pinned Helm 4 hash is the signed one.
        let checksum = fixture("helm-v4.3.1-darwin-arm64.tar.gz.sha256sum");
        let pinned = HELM4
            .artifact(Platform::new(Os::MacOs, Arch::Arm64))
            .unwrap();
        assert!(String::from_utf8_lossy(&checksum).starts_with(pinned.sha256));
    }

    #[test]
    fn tampered_or_swapped_checksum_is_refused() {
        let mut checksum = fixture("helm-v4.3.1-darwin-arm64.tar.gz.sha256sum");
        checksum[0] = if checksum[0] == b'0' { b'1' } else { b'0' };
        let signature = fixture("helm-v4.3.1-darwin-arm64.tar.gz.sha256sum.asc");
        assert!(matches!(
            keyring().verify(&signature, &checksum[..]),
            Err(SignatureError::Invalid { .. })
        ));
        // The signature of another release does not cover this file.
        let other = fixture("helm-v3.22.1-darwin-arm64.tar.gz.sha256sum.asc");
        let checksum = fixture("helm-v4.3.1-darwin-arm64.tar.gz.sha256sum");
        assert!(matches!(
            keyring().verify(&other, &checksum[..]),
            Err(SignatureError::Invalid { .. })
        ));
    }

    #[test]
    fn real_signature_needs_the_helm_key() {
        let checksum = fixture("helm-v4.3.1-darwin-arm64.tar.gz.sha256sum");
        let signature = fixture("helm-v4.3.1-darwin-arm64.tar.gz.sha256sum.asc");
        let error = Keyring::default()
            .verify(&signature, &checksum[..])
            .unwrap_err();
        assert_eq!(
            error,
            SignatureError::UnknownKey {
                issuer: "208D D36E D5BB 3745 A167 43A4 C7C6 FBB5 B91C 1155".into()
            }
        );
    }

    /// A version whose download fails, and how.
    type Failing = (&'static str, fn() -> DownloadError);

    fn fake_install(version: &str) -> (Installed, Verification) {
        let installed = Installed {
            version: version.to_owned(),
            path: PathBuf::from(format!("/tools/helm/{version}/helm")),
        };
        (installed, Verification::default())
    }

    fn latest(helm4: &str) -> LatestVersions {
        LatestVersions {
            checked_at: unix_now(),
            helm4: Some(helm4.into()),
            helm3: Some("3.22.1".into()),
        }
    }

    /// Runs [`download_line_with`] with a canned check and downloads that
    /// fail for the versions in `failing`; returns the result and the
    /// versions asked for.
    fn run_line(
        line: HelmLine,
        known: Option<LatestVersions>,
        check: Result<LatestVersions, DownloadError>,
        failing: &[Failing],
    ) -> (Result<HelmInstall, DownloadError>, Vec<String>) {
        struct Fake<'a> {
            check: Option<Result<LatestVersions, DownloadError>>,
            failing: &'a [Failing],
            asked: Vec<String>,
        }
        impl LineSteps for &mut Fake<'_> {
            async fn check(&mut self) -> Result<LatestVersions, DownloadError> {
                self.check.take().unwrap()
            }
            async fn download(
                &mut self,
                _: &'static ToolSpec,
                version: &str,
            ) -> Result<(Installed, Verification), DownloadError> {
                self.asked.push(version.to_owned());
                match self.failing.iter().find(|(v, _)| *v == version) {
                    Some((_, error)) => Err(error()),
                    None => Ok(fake_install(version)),
                }
            }
        }
        let mut fake = Fake {
            check: Some(check),
            failing,
            asked: Vec::new(),
        };
        let result = futures::executor::block_on(download_line_with(line, known, &mut fake));
        (result, fake.asked)
    }

    fn bad_signature() -> DownloadError {
        DownloadError::Signature {
            file: "helm-v4.9.0-darwin-arm64.tar.gz".into(),
            source: SignatureError::UnknownKey {
                issuer: "0000".into(),
            },
        }
    }

    fn offline() -> DownloadError {
        DownloadError::Network {
            host: "get.helm.sh".into(),
            message: "dns error".into(),
        }
    }

    #[test]
    fn installs_the_latest_signed_version() {
        let (result, asked) = run_line(HelmLine::Helm4, None, Ok(latest("4.9.0")), &[]);
        let result = result.unwrap();
        assert_eq!(asked, ["4.9.0"]);
        assert_eq!(result.installed.version, "4.9.0");
        assert_eq!(result.fallback, None);
        assert_eq!(
            result.latest,
            Some(latest("4.9.0")).map(|mut l| {
                l.checked_at = result.latest.as_ref().unwrap().checked_at;
                l
            })
        );
        // A fresh earlier check is reused: nothing new to save.
        let (result, _) = run_line(HelmLine::Helm4, Some(latest("4.9.0")), Err(offline()), &[]);
        assert_eq!(result.unwrap().latest, None);
    }

    #[test]
    fn falls_back_to_the_pinned_version_when_the_signature_fails() {
        let (result, asked) = run_line(
            HelmLine::Helm4,
            None,
            Ok(latest("4.9.0")),
            &[("4.9.0", bad_signature)],
        );
        let result = result.unwrap();
        assert_eq!(asked, ["4.9.0", HELM4.version]);
        assert_eq!(result.installed.version, HELM4.version);
        let reason = result.fallback.unwrap();
        assert!(
            reason.starts_with("Helm 4.9.0 was not installed: "),
            "{reason}"
        );
        assert!(reason.contains("does not trust"), "{reason}");
        assert!(
            reason.contains("Installed Helm 4.3.1 pinned in Kubach"),
            "{reason}"
        );
    }

    #[test]
    fn falls_back_when_the_latest_version_is_unknown() {
        let (result, asked) = run_line(HelmLine::Helm3, None, Err(offline()), &[]);
        let result = result.unwrap();
        assert_eq!(asked, [HELM3.version]);
        let reason = result.fallback.unwrap();
        assert!(
            reason.starts_with("Can't check the latest Helm 3: can't reach get.helm.sh"),
            "{reason}"
        );
        assert_eq!(result.latest, None);
    }

    #[test]
    fn pinned_latest_needs_no_fallback_and_local_errors_stop() {
        // The latest is the pinned version: one download, no note.
        let (result, asked) = run_line(HelmLine::Helm3, None, Ok(latest("4.3.1")), &[]);
        assert_eq!(asked, ["3.22.1"]);
        assert_eq!(result.unwrap().fallback, None);
        // A full disk is not the release's fault: no second try.
        let (result, asked) = run_line(
            HelmLine::Helm4,
            None,
            Ok(latest("4.9.0")),
            &[("4.9.0", || {
                DownloadError::Io(std::io::Error::other("disk full"))
            })],
        );
        assert!(matches!(result, Err(DownloadError::Io(_))));
        assert_eq!(asked, ["4.9.0"]);
        // No network at all: the pinned download fails too, with its error.
        let (result, asked) = run_line(
            HelmLine::Helm4,
            None,
            Err(offline()),
            &[(HELM4.version, offline)],
        );
        assert!(matches!(result, Err(DownloadError::Network { .. })));
        assert_eq!(asked, [HELM4.version]);
    }

    /// `cargo test -p kubach-core --lib -- --ignored downloads_real_helm`
    #[tokio::test]
    #[ignore = "downloads from get.helm.sh and github.com"]
    async fn downloads_real_helm() {
        for line in HelmLine::ALL {
            let root = tempfile::tempdir().unwrap();
            let mut last = Progress::default();
            let result = download_line(line, root.path(), None, |p| last = p)
                .await
                .unwrap();
            assert_eq!(result.fallback, None);
            let latest = result.latest.as_ref().unwrap();
            assert_eq!(
                result.installed.version,
                available(line, Some(latest)),
                "{line:?}"
            );
            assert!(last.downloaded > 1_000_000);
            assert_eq!(last.total, Some(last.downloaded));
            let signer = result.verification.signer.as_ref().unwrap();
            assert!(
                HELM_KEY_FINGERPRINTS.contains(&signer.fingerprint.as_str()),
                "{signer}"
            );
            assert_eq!(result.installed.verification(), Some(result.verification));
            let version = binary_version(&result.installed.path).await.unwrap();
            assert!(
                version.starts_with(&format!("v{}", result.installed.version)),
                "{version}"
            );
            assert_eq!(downloaded_in(root.path()), Some(result.installed));
            assert_eq!(LatestVersions::load(root.path()).as_ref(), Some(latest));
        }
    }

    /// The pinned version alone, with its signature checked too.
    #[tokio::test]
    #[ignore = "downloads from get.helm.sh and github.com"]
    async fn downloads_pinned_helm_with_signature() {
        let root = tempfile::tempdir().unwrap();
        let (installed, verification) =
            download::download_version(&HELM3, HELM3.version, root.path(), |_| {})
                .await
                .unwrap();
        assert!(verification.pinned);
        assert_eq!(verification.signature_note, None);
        assert!(verification.signer.is_some());
        assert_eq!(installed.version, "3.22.1");
    }
}
