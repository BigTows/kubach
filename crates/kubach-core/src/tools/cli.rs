//! Cloud CLIs Kubach can download for "Import from cloud" (#38): the
//! Google Cloud CLI (`gcloud`), the Yandex Cloud CLI (`yc`) and the AWS
//! CLI v2 (`aws`).
//!
//! Unlike Helm ([`super::ToolSpec`]), these tools release every week or
//! so and ship as whole directory trees (gcloud, aws), so Kubach pins no
//! version: it asks the vendor for the current release ([`CliTool::resolve`])
//! and trusts it by what the vendor publishes ([`Integrity`]):
//!
//! - gcloud: the SHA-256 Google lists on its "versioned archives" page;
//! - aws: the detached OpenPGP signature (`.sig`) by the AWS CLI Team key
//!   shipped with Kubach (Linux), the Apple Developer ID signature of
//!   `AWSCLIV2.pkg` (macOS);
//! - yc: Yandex publishes no checksum or signature, so only HTTPS from
//!   `storage.yandexcloud.net` protects the download; this is recorded and
//!   shown.
//!
//! The release is downloaded only on the user's request, into
//! `<data dir>/tools/<name>/<version>/`, through a temporary directory that
//! is removed on failure or cancel (as for Helm, ADR 0006); nothing is
//! installed system-wide and no administrator rights are needed. Where the
//! vendor offers no such package (AWS CLI on Windows: an MSI only), the
//! UI shows an install hint instead.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::archive::{self, ArchiveError, ArchiveKind};
use super::download::{
    self, DownloadError, HashingFile, Installed, Os, Platform, Progress, TEMP_PREFIX,
    VERIFICATION_FILE, Verification,
};
use super::signature::{self, Keyring, SignatureError};
use super::{aws, gcloud, yc};

/// Unpacked trees larger than this are refused (gcloud with its bundled
/// Python is about 700 MB on Linux).
pub const MAX_TREE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// `gcloud components install` downloads and unpacks the plugin.
const COMPONENT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const VERSION_TIMEOUT: Duration = Duration::from_secs(30);
const PKGUTIL: &str = "/usr/sbin/pkgutil";

/// A cloud CLI Kubach can find or download.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CliTool {
    /// Google Cloud CLI.
    Gcloud,
    /// Yandex Cloud CLI.
    Yc,
    /// AWS CLI v2.
    Aws,
}

impl CliTool {
    pub const ALL: [Self; 3] = [Self::Gcloud, Self::Yc, Self::Aws];

    /// Directory under the tools root.
    pub fn name(self) -> &'static str {
        match self {
            Self::Gcloud => "gcloud",
            Self::Yc => "yc",
            Self::Aws => "aws",
        }
    }

    /// Product name, e.g. "Google Cloud CLI".
    pub fn title(self) -> &'static str {
        match self {
            Self::Gcloud => "Google Cloud CLI",
            Self::Yc => "Yandex Cloud CLI",
            Self::Aws => "AWS CLI",
        }
    }

    /// The program, as typed in a terminal.
    pub fn command(self) -> &'static str {
        self.name()
    }

    /// The vendor's install instructions, for the hint where Kubach can't
    /// download the CLI.
    pub fn install_page(self) -> &'static str {
        match self {
            Self::Gcloud => "https://cloud.google.com/sdk/docs/install",
            Self::Yc => "https://yandex.cloud/docs/cli/quickstart",
            Self::Aws => {
                "https://docs.aws.amazon.com/cli/latest/userguide/getting-started-install.html"
            }
        }
    }

    /// Whether Kubach can download the CLI for `platform`.
    pub fn downloadable(self, platform: Platform) -> bool {
        match self {
            Self::Gcloud => gcloud::supported(platform),
            Self::Yc => yc::supported(platform),
            Self::Aws => aws::supported(platform),
        }
    }

    /// Path of the program inside a downloaded version directory.
    pub fn entry(self, platform: Platform) -> PathBuf {
        match self {
            Self::Gcloud => gcloud::entry(platform),
            Self::Yc => PathBuf::from(format!("yc{}", platform.exe_suffix())),
            Self::Aws => aws::entry(platform),
        }
    }

    /// File names to look for in `PATH`.
    fn path_names(self, platform: Platform) -> Vec<String> {
        match (self, platform.os) {
            // The SDK starts gcloud through a batch file on Windows.
            (Self::Gcloud, Os::Windows) => vec!["gcloud.cmd".into(), "gcloud.exe".into()],
            (tool, _) => vec![format!("{}{}", tool.command(), platform.exe_suffix())],
        }
    }

    /// Asks the vendor for the current release for `platform`.
    pub async fn resolve(self, platform: Platform) -> Result<Release, DownloadError> {
        if !self.downloadable(platform) {
            return Err(DownloadError::UnsupportedPlatform { tool: self.name() });
        }
        match self {
            Self::Gcloud => gcloud::resolve(platform).await,
            Self::Yc => yc::resolve(platform).await,
            Self::Aws => aws::resolve(platform),
        }
    }

    /// The version in the output of [`version_args`](Self::version_args).
    pub fn parse_version(self, output: &str) -> Option<String> {
        let prefix = match self {
            Self::Gcloud => "Google Cloud SDK ",
            Self::Yc => "Yandex Cloud CLI ",
            Self::Aws => "aws-cli/",
        };
        output.lines().find_map(|line| {
            let rest = line.trim().strip_prefix(prefix)?;
            let version: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            (!version.is_empty() && !version.starts_with('.')).then_some(version)
        })
    }

    /// Arguments printing the version.
    pub fn version_args(self) -> &'static [&'static str] {
        match self {
            Self::Gcloud => &["version"],
            Self::Yc => &["version"],
            Self::Aws => &["--version"],
        }
    }
}

/// How a release is trusted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Integrity {
    /// The SHA-256 the vendor publishes at `source`.
    Sha256 { expected: String, source: String },
    /// A detached OpenPGP signature at `url` by a key of `keys`.
    Signature { url: String, keys: TrustedKeys },
    /// A macOS installer package signed with the Apple Developer ID of
    /// `team` (checked with `pkgutil --check-signature`).
    MacPackage { team: &'static str },
    /// Nothing is published; only HTTPS protects the download.
    None,
}

/// Keys shipped with Kubach that may sign a cloud CLI release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustedKeys {
    /// The AWS CLI Team key ([`aws::AWS_KEY_FINGERPRINT`]).
    AwsCli,
}

impl TrustedKeys {
    pub fn keyring(self) -> &'static Keyring {
        match self {
            Self::AwsCli => aws::keyring(),
        }
    }
}

/// What the downloaded file is and what to keep of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Layout {
    /// An archive with one binary at `member`.
    Single { member: String },
    /// An archive unpacked as a whole.
    Tree,
    /// A macOS installer package: expanded with `pkgutil --expand-full`
    /// and the directory `payload` kept under the name `keep_as`.
    MacPackage {
        payload: &'static str,
        keep_as: &'static str,
    },
}

/// The current release of a CLI for one platform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// `None` when the vendor does not say up front (AWS): the program is
    /// asked after unpacking.
    pub version: Option<String>,
    pub url: String,
    pub integrity: Integrity,
    pub layout: Layout,
}

/// A finished download.
#[derive(Clone, Debug)]
pub struct CliInstall {
    pub installed: Installed,
    pub verification: Verification,
    /// gcloud: why `gke-gcloud-auth-plugin` could not be installed into
    /// the new SDK (it is needed to connect to GKE clusters).
    pub plugin_error: Option<String>,
}

/// Where a CLI in use comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CliSource {
    /// Found in `PATH` (the login shell's, ADR 0013).
    Path,
    /// Downloaded by Kubach.
    Downloaded,
}

/// A CLI found by [`locate`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliBinary {
    pub tool: CliTool,
    pub path: PathBuf,
    pub source: CliSource,
}

/// The CLI to use: the one in `PATH` (the user's own, with their updates),
/// else the copy Kubach downloaded.
pub fn locate(tool: CliTool) -> Option<CliBinary> {
    let platform = Platform::current()?;
    let found = |path, source| CliBinary { tool, path, source };
    find_in_path(tool, platform, &crate::env::path())
        .map(|path| found(path, CliSource::Path))
        .or_else(|| downloaded(tool).map(|i| found(i.path, CliSource::Downloaded)))
}

fn find_in_path(tool: CliTool, platform: Platform, path: &std::ffi::OsStr) -> Option<PathBuf> {
    let names = tool.path_names(platform);
    crate::which::search_dirs(path)
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| crate::which::is_executable(candidate))
}

/// The copy of `tool` Kubach downloaded, if any.
pub fn downloaded(tool: CliTool) -> Option<Installed> {
    downloaded_in(tool, &super::tools_dir()?)
}

/// The newest downloaded version of `tool` under the tools root `root`.
pub fn downloaded_in(tool: CliTool, root: &Path) -> Option<Installed> {
    let platform = Platform::current()?;
    downloaded_for(tool, root, platform).into_iter().next()
}

fn downloaded_for(tool: CliTool, root: &Path, platform: Platform) -> Vec<Installed> {
    let Ok(entries) = fs::read_dir(root.join(tool.name())) else {
        return Vec::new();
    };
    let entry = tool.entry(platform);
    let mut found: Vec<Installed> = entries
        .filter_map(Result::ok)
        .filter_map(|dir| {
            let version = dir.file_name().to_str()?.to_owned();
            if version.starts_with('.') {
                return None;
            }
            let path = dir.path().join(&entry);
            path.is_file().then_some(Installed { version, path })
        })
        .collect();
    found.sort_by_key(|i| std::cmp::Reverse(download::version_key(&i.version)));
    found
}

/// Deletes every downloaded version of `tool`.
pub fn remove(tool: CliTool, root: &Path) -> io::Result<()> {
    match fs::remove_dir_all(root.join(tool.name())) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Deletes the downloaded versions of `tool` other than `keep`.
fn remove_others(tool: CliTool, root: &Path, keep: &str) {
    let Ok(entries) = fs::read_dir(root.join(tool.name())) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        if entry.file_name() != keep && entry.path().is_dir() {
            fs::remove_dir_all(entry.path()).ok();
        }
    }
}

/// Downloads the current release of `tool` into the tools root `root`,
/// checks it as the vendor allows ([`Integrity`]) and unpacks it. On
/// success older versions are removed; for gcloud, `gke-gcloud-auth-plugin`
/// is installed into the new SDK. Dropping the future cancels and cleans
/// up. Must run on a tokio runtime.
pub async fn download(
    tool: CliTool,
    root: &Path,
    progress: impl FnMut(Progress) + Send,
) -> Result<CliInstall, DownloadError> {
    let platform =
        Platform::current().ok_or(DownloadError::UnsupportedPlatform { tool: tool.name() })?;
    let release = tool.resolve(platform).await?;
    let (installed, verification) = install(tool, &release, platform, root, progress).await?;
    remove_others(tool, root, &installed.version);
    let plugin_error = match tool {
        CliTool::Gcloud => install_gke_plugin(&installed.path).await.err(),
        _ => None,
    };
    Ok(CliInstall {
        installed,
        verification,
        plugin_error,
    })
}

/// Fetches, checks and unpacks `release` into `<root>/<name>/<version>`.
async fn install(
    tool: CliTool,
    release: &Release,
    platform: Platform,
    root: &Path,
    mut progress: impl FnMut(Progress) + Send,
) -> Result<(Installed, Verification), DownloadError> {
    let file_name = download::file_name(&release.url)?;
    let tool_dir = root.join(tool.name());
    fs::create_dir_all(&tool_dir)?;
    let temp = tempfile::Builder::new()
        .prefix(TEMP_PREFIX)
        .tempdir_in(&tool_dir)?;

    let signature = match &release.integrity {
        Integrity::Signature { url, .. } => {
            let mut bytes = Vec::new();
            download::fetch(url, &mut bytes, signature::MAX_SIGNATURE_BYTES, |_| {}).await?;
            Some(bytes)
        }
        _ => None,
    };

    let archive_path = temp.path().join(&file_name);
    let mut sink = HashingFile::create(&archive_path)?;
    download::fetch(
        &release.url,
        &mut sink,
        download::MAX_ARCHIVE_BYTES,
        &mut progress,
    )
    .await?;
    let actual = sink.finish()?;
    let verification = match check(
        &release.integrity,
        &archive_path,
        &file_name,
        &actual,
        signature,
    )
    .await
    {
        Ok(verification) => verification,
        Err(error) => {
            fs::remove_file(&archive_path).ok();
            return Err(error);
        }
    };

    let staged = temp.path().join("staged");
    fs::create_dir(&staged)?;
    let entry_rel = tool.entry(platform);
    unpack(
        &release.layout,
        &archive_path,
        &file_name,
        temp.path(),
        &staged,
        &entry_rel,
    )
    .await?;
    fs::remove_file(&archive_path).ok();
    let entry = staged.join(&entry_rel);
    if !entry.is_file() {
        return Err(ArchiveError::MissingMember(entry_rel.display().to_string()).into());
    }
    download::make_executable(&entry)?;
    download::clear_quarantine(&entry);

    let version = match &release.version {
        Some(version) => version.clone(),
        None => binary_version(tool, &entry).await.ok_or_else(|| {
            io::Error::other(format!("{} does not answer --version", tool.command()))
        })?,
    };
    if !is_directory_name(&version) {
        return Err(DownloadError::InvalidUrl(version));
    }
    let record = serde_json::to_vec_pretty(&verification).map_err(io::Error::other)?;
    fs::write(staged.join(VERIFICATION_FILE), record)?;
    let target = tool_dir.join(&version);
    if target.exists() {
        fs::rename(&target, temp.path().join("previous"))?;
    }
    fs::rename(&staged, &target)?;
    Ok((
        Installed {
            version,
            path: target.join(entry_rel),
        },
        verification,
    ))
}

/// A version names a directory: it came over the network.
fn is_directory_name(version: &str) -> bool {
    !version.is_empty()
        && !version.starts_with('.')
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
}

/// Checks the downloaded archive (SHA-256 `actual`) as `integrity` says.
async fn check(
    integrity: &Integrity,
    archive: &Path,
    file_name: &str,
    actual: &str,
    signature: Option<Vec<u8>>,
) -> Result<Verification, DownloadError> {
    let mut verification = Verification {
        sha256: actual.to_owned(),
        ..Verification::default()
    };
    match integrity {
        Integrity::Sha256 { expected, source } => {
            download::verify(file_name, actual, expected, None)?;
            verification.published_at = Some(source.clone());
        }
        Integrity::Signature { keys, .. } => {
            let signature = signature.unwrap_or_default();
            verification.signer = Some(download::verify_file_signature(
                keys.keyring(),
                &signature,
                archive,
                file_name,
            )?);
        }
        Integrity::MacPackage { team } => {
            verification.package_signer = Some(check_package_signature(archive, team).await?);
        }
        Integrity::None => verification.unchecked = true,
    }
    Ok(verification)
}

/// Unpacks the checked archive into `staged` (`temp` is scratch space on
/// the same file system).
async fn unpack(
    layout: &Layout,
    archive: &Path,
    file_name: &str,
    temp: &Path,
    staged: &Path,
    entry: &Path,
) -> Result<(), DownloadError> {
    let kind = || {
        ArchiveKind::from_name(file_name)
            .ok_or_else(|| ArchiveError::Corrupt(format!("{file_name}: unknown archive type")))
    };
    match layout {
        Layout::Single { member } => {
            archive::extract_member(
                archive,
                kind()?,
                member,
                &staged.join(entry),
                download::MAX_BINARY_BYTES,
            )?;
        }
        Layout::Tree => {
            let (archive, staged) = (archive.to_owned(), staged.to_owned());
            let kind = kind()?;
            // Tens of thousands of files: off the async workers.
            tokio::task::spawn_blocking(move || {
                archive::extract_all(&archive, kind, &staged, MAX_TREE_BYTES)
            })
            .await
            .map_err(io::Error::other)??;
        }
        Layout::MacPackage { payload, keep_as } => {
            let expanded = temp.join("expanded");
            let mut command = tokio::process::Command::new(PKGUTIL);
            command.arg("--expand-full").arg(archive).arg(&expanded);
            crate::process::run(command, COMPONENT_TIMEOUT)
                .await
                .map_err(|e| io::Error::other(format!("pkgutil --expand-full {e}")))?;
            let payload = expanded.join(payload);
            if !payload.is_dir() {
                return Err(ArchiveError::MissingMember(payload.display().to_string()).into());
            }
            fs::rename(payload, staged.join(keep_as))?;
        }
    }
    Ok(())
}

/// `pkgutil --check-signature` of the package at `path`: the signer
/// (`Developer ID Installer: … (TEAM)`) when it is signed and notarized
/// for distribution by `team`.
async fn check_package_signature(path: &Path, team: &str) -> Result<String, DownloadError> {
    let file = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut command = tokio::process::Command::new(PKGUTIL);
    command.arg("--check-signature").arg(path);
    let output = match crate::process::run(command, VERSION_TIMEOUT).await {
        Ok(output) => output.stdout_text(),
        Err(crate::process::ProcessError::Failed { stdout, .. }) => {
            String::from_utf8_lossy(&stdout).into_owned()
        }
        Err(error) => return Err(io::Error::other(format!("pkgutil {error}")).into()),
    };
    package_signer(&output, team).map_err(|source| DownloadError::Signature { file, source })
}

/// The signer in `pkgutil --check-signature` output, if the package is
/// signed with a Developer ID certificate of `team`.
pub fn package_signer(output: &str, team: &str) -> Result<String, SignatureError> {
    let signed = output.lines().any(|line| {
        line.trim()
            .starts_with("Status: signed by a developer certificate issued by Apple")
    });
    let signer = output.lines().map(str::trim).find_map(|line| {
        line.strip_prefix("1. ")
            .filter(|rest| rest.starts_with("Developer ID Installer:"))
    });
    match (signed, signer) {
        (true, Some(signer)) if signer.ends_with(&format!("({team})")) => Ok(signer.to_owned()),
        (true, Some(signer)) => Err(SignatureError::UnknownKey {
            issuer: signer.to_owned(),
        }),
        _ => Err(SignatureError::Malformed(
            "the package is not signed with an Apple Developer ID".into(),
        )),
    }
}

/// The version `tool` at `path` reports; `None` when it does not run or
/// answer in time.
pub async fn binary_version(tool: CliTool, path: &Path) -> Option<String> {
    let mut command = tokio::process::Command::new(path);
    command.args(tool.version_args());
    if tool == CliTool::Gcloud {
        command.env("CLOUDSDK_CORE_DISABLE_PROMPTS", "1");
    }
    let output = crate::process::run(command, VERSION_TIMEOUT).await.ok()?;
    tool.parse_version(&output.stdout_text())
}

/// `gke-gcloud-auth-plugin[.exe]`.
pub fn gke_plugin_name() -> String {
    let exe = if cfg!(windows) { ".exe" } else { "" };
    format!("{}{exe}", gcloud::GKE_PLUGIN)
}

/// The GKE exec plugin that goes with `gcloud`: next to it (the SDK's
/// `bin`, also behind a symlink such as Homebrew's), else in `PATH`.
pub fn gke_plugin(gcloud: &Path) -> Option<PathBuf> {
    gke_plugin_beside(gcloud)
        .or_else(|| crate::which::which(gcloud::GKE_PLUGIN, &crate::env::path()))
}

/// The GKE exec plugin in the folder of `gcloud` or of the file its
/// symlink points to (the SDK's `bin`).
fn gke_plugin_beside(gcloud: &Path) -> Option<PathBuf> {
    let name = gke_plugin_name();
    let mut dirs: Vec<PathBuf> = gcloud.parent().map(Path::to_owned).into_iter().collect();
    if let Ok(real) = fs::canonicalize(gcloud)
        && let Some(dir) = real.parent()
    {
        dirs.push(dir.to_owned());
    }
    dirs.into_iter()
        .map(|dir| dir.join(&name))
        .find(|path| crate::which::is_executable(path))
}

/// The SDK folder holding `gke-gcloud-auth-plugin` beside the gcloud in
/// `PATH` or the one Kubach downloaded. Homebrew links only `gcloud` into
/// its `bin`, so the plugin `gcloud components install` adds is not in
/// `PATH`; contexts naming it bare still start it from here
/// ([`crate::env::apply_to_exec`]).
pub fn gke_plugin_dir() -> Option<PathBuf> {
    let platform = Platform::current()?;
    let in_path = find_in_path(CliTool::Gcloud, platform, &crate::env::path());
    let downloaded = downloaded(CliTool::Gcloud).map(|installed| installed.path);
    in_path
        .into_iter()
        .chain(downloaded)
        .find_map(|gcloud| gke_plugin_beside(&gcloud))
        .and_then(|plugin| plugin.parent().map(Path::to_owned))
}

/// Whether the exec plugin `command` is `gke-gcloud-auth-plugin`, by name
/// or by path.
pub fn is_gke_plugin(command: &str) -> bool {
    Path::new(command)
        .file_stem()
        .is_some_and(|stem| stem == gcloud::GKE_PLUGIN)
}

/// Runs `gcloud components install gke-gcloud-auth-plugin --quiet` with
/// the gcloud at `gcloud`; the error is what gcloud said.
pub async fn install_gke_plugin(gcloud: &Path) -> Result<(), String> {
    let mut command = tokio::process::Command::new(gcloud);
    command
        .args(["components", "install", gcloud::GKE_PLUGIN, "--quiet"])
        .env("CLOUDSDK_CORE_DISABLE_PROMPTS", "1");
    match crate::process::run(command, COMPONENT_TIMEOUT).await {
        Ok(_) => Ok(()),
        Err(crate::process::ProcessError::Failed { stderr, .. }) => {
            Err(last_lines(&String::from_utf8_lossy(&stderr)))
        }
        Err(error) => Err(error.to_string()),
    }
}

/// The last few non-empty lines of a program's error output.
pub fn last_lines(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    lines[lines.len().saturating_sub(4)..].join("\n")
}

/// Environment for running a downloaded CLI: nothing extra, but kept in
/// one place for the cloud listing (`OsString` pairs).
pub fn quiet_env(tool: CliTool) -> Vec<(OsString, OsString)> {
    match tool {
        CliTool::Gcloud => vec![("CLOUDSDK_CORE_DISABLE_PROMPTS".into(), "1".into())],
        CliTool::Yc => Vec::new(),
        CliTool::Aws => vec![("AWS_PAGER".into(), "".into())],
    }
}

#[cfg(test)]
mod tests {
    use super::super::archive::fixtures;
    use super::super::download::Arch;
    use super::*;

    const MAC: Platform = Platform::new(Os::MacOs, Arch::Arm64);
    const LINUX: Platform = Platform::new(Os::Linux, Arch::Amd64);
    const WINDOWS: Platform = Platform::new(Os::Windows, Arch::Amd64);
    const WINDOWS_ARM: Platform = Platform::new(Os::Windows, Arch::Arm64);

    #[test]
    fn what_can_be_downloaded_where() {
        for platform in [MAC, LINUX, Platform::new(Os::Linux, Arch::Arm64)] {
            for tool in CliTool::ALL {
                assert!(tool.downloadable(platform), "{tool:?} {platform:?}");
            }
        }
        assert!(CliTool::Gcloud.downloadable(WINDOWS));
        assert!(CliTool::Yc.downloadable(WINDOWS));
        // An MSI only: needs msiexec, so a hint instead.
        assert!(!CliTool::Aws.downloadable(WINDOWS));
        for tool in CliTool::ALL {
            assert!(!tool.downloadable(WINDOWS_ARM));
        }
    }

    #[test]
    fn entries_per_platform() {
        assert_eq!(
            CliTool::Gcloud.entry(MAC),
            Path::new("google-cloud-sdk").join("bin").join("gcloud")
        );
        assert_eq!(
            CliTool::Gcloud.entry(WINDOWS),
            Path::new("google-cloud-sdk").join("bin").join("gcloud.cmd")
        );
        assert_eq!(CliTool::Yc.entry(WINDOWS), Path::new("yc.exe"));
        assert_eq!(CliTool::Aws.entry(MAC), Path::new("aws-cli").join("aws"));
        assert_eq!(
            CliTool::Aws.entry(LINUX),
            Path::new("aws").join("dist").join("aws")
        );
        assert_eq!(
            CliTool::Gcloud.path_names(WINDOWS),
            ["gcloud.cmd", "gcloud.exe"]
        );
        assert_eq!(CliTool::Aws.path_names(LINUX), ["aws"]);
    }

    #[test]
    fn versions_from_cli_output() {
        let gcloud = "Google Cloud SDK 591.0.0\nbq 2.1.25\ncore 2026.10.26\n";
        assert_eq!(
            CliTool::Gcloud.parse_version(gcloud).as_deref(),
            Some("591.0.0")
        );
        assert_eq!(
            CliTool::Yc
                .parse_version("Yandex Cloud CLI 1.42.0 darwin/arm64\n")
                .as_deref(),
            Some("1.42.0")
        );
        assert_eq!(
            CliTool::Aws
                .parse_version("aws-cli/2.39.1 Python/3.14.8 Darwin/25.6.0 exe/arm64\n")
                .as_deref(),
            Some("2.39.1")
        );
        assert_eq!(CliTool::Aws.parse_version("command not found"), None);
    }

    #[test]
    fn version_names_one_directory() {
        assert!(is_directory_name("2.39.1"));
        assert!(!is_directory_name(""));
        assert!(!is_directory_name("../x"));
        assert!(!is_directory_name("a/b"));
        assert!(!is_directory_name(".hidden"));
    }

    /// `pkgutil --check-signature AWSCLIV2.pkg`, October 2026.
    const PKGUTIL_AWS: &str = r#"Package "AWSCLIV2.pkg":
   Status: signed by a developer certificate issued by Apple for distribution
   Notarization: trusted by the Apple notary service
   Signed with a trusted timestamp on: 2026-10-28 18:15:11 +0000
   Certificate Chain:
    1. Developer ID Installer: AMZN Mobile LLC (94KV3E626L)
       Expires: 2030-09-26 00:18:06 +0000
    2. Developer ID Certification Authority
    3. Apple Root CA
"#;

    #[test]
    fn package_signature_must_be_the_vendors() {
        assert_eq!(
            package_signer(PKGUTIL_AWS, "94KV3E626L").unwrap(),
            "Developer ID Installer: AMZN Mobile LLC (94KV3E626L)"
        );
        assert!(matches!(
            package_signer(PKGUTIL_AWS, "ABCDE12345"),
            Err(SignatureError::UnknownKey { .. })
        ));
        assert!(matches!(
            package_signer(
                "Package \"x.pkg\":\n   Status: no signature\n",
                "94KV3E626L"
            ),
            Err(SignatureError::Malformed(_))
        ));
    }

    fn fake(root: &Path, tool: CliTool, version: &str, platform: Platform) {
        let path = root
            .join(tool.name())
            .join(version)
            .join(tool.entry(platform));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"bin").unwrap();
    }

    #[test]
    fn finds_the_newest_download_and_removes() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        assert!(downloaded_for(CliTool::Gcloud, root, LINUX).is_empty());
        fake(root, CliTool::Gcloud, "590.0.0", LINUX);
        fake(root, CliTool::Gcloud, "591.0.0", LINUX);
        fs::create_dir_all(root.join("gcloud/.download-x")).unwrap();
        fs::create_dir_all(root.join("gcloud/999.0.0")).unwrap();
        let found = downloaded_for(CliTool::Gcloud, root, LINUX);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].version, "591.0.0");
        assert!(found[0].path.ends_with("google-cloud-sdk/bin/gcloud"));
        remove_others(CliTool::Gcloud, root, "591.0.0");
        assert_eq!(downloaded_for(CliTool::Gcloud, root, LINUX).len(), 1);
        remove(CliTool::Gcloud, root).unwrap();
        assert!(downloaded_for(CliTool::Gcloud, root, LINUX).is_empty());
        remove(CliTool::Gcloud, root).unwrap();
    }

    #[test]
    fn finds_cli_in_path_before_download() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        assert_eq!(find_in_path(CliTool::Yc, LINUX, bin.as_os_str()), None);
        let yc = bin.join("yc");
        fs::write(&yc, b"#!/bin/sh\n").unwrap();
        download::make_executable(&yc).unwrap();
        #[cfg(unix)]
        assert_eq!(find_in_path(CliTool::Yc, LINUX, bin.as_os_str()), Some(yc));
        // Relative PATH entries are never searched (#202).
        assert_eq!(find_in_path(CliTool::Yc, LINUX, "bin".as_ref()), None);
    }

    #[test]
    fn gke_plugin_next_to_gcloud() {
        let dir = tempfile::tempdir().unwrap();
        let gcloud = dir.path().join("gcloud");
        fs::write(&gcloud, b"").unwrap();
        let plugin = dir.path().join(gke_plugin_name());
        fs::write(&plugin, b"").unwrap();
        download::make_executable(&plugin).unwrap();
        #[cfg(unix)]
        assert_eq!(gke_plugin(&gcloud), Some(plugin));
    }

    #[test]
    fn gke_plugin_by_name_or_path() {
        assert!(is_gke_plugin("gke-gcloud-auth-plugin"));
        assert!(is_gke_plugin("gke-gcloud-auth-plugin.exe"));
        assert!(is_gke_plugin("/opt/sdk/bin/gke-gcloud-auth-plugin"));
        assert!(!is_gke_plugin("aws"));
        assert!(!is_gke_plugin("gcloud"));
    }

    #[tokio::test]
    async fn installs_a_single_binary_release() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let archive = source.path().join("yc_1.42.0_linux_amd64.tar.gz");
        fixtures::tar_gz(&archive, &[("yc", b"#!/bin/sh\n")]);
        // The network step is replaced: unpack and check directly.
        let temp = root.path().join("t");
        let staged = temp.join("staged");
        fs::create_dir_all(&staged).unwrap();
        unpack(
            &Layout::Single {
                member: "yc".into(),
            },
            &archive,
            "yc_1.42.0_linux_amd64.tar.gz",
            &temp,
            &staged,
            &CliTool::Yc.entry(LINUX),
        )
        .await
        .unwrap();
        assert_eq!(fs::read(staged.join("yc")).unwrap(), b"#!/bin/sh\n");

        let verification = check(
            &Integrity::None,
            &archive,
            "yc.tar.gz",
            &"ab".repeat(32),
            None,
        )
        .await
        .unwrap();
        assert!(verification.unchecked);
        assert!(!verification.pinned);
    }

    #[tokio::test]
    async fn published_checksum_must_match() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.tar.gz");
        fs::write(&archive, b"archive").unwrap();
        let actual = download::sha256_hex(b"archive");
        let ok = check(
            &Integrity::Sha256 {
                expected: actual.clone(),
                source: "https://example.com/page".into(),
            },
            &archive,
            "a.tar.gz",
            &actual,
            None,
        )
        .await
        .unwrap();
        assert_eq!(ok.published_at.as_deref(), Some("https://example.com/page"));
        let bad = check(
            &Integrity::Sha256 {
                expected: "0".repeat(64),
                source: "https://example.com/page".into(),
            },
            &archive,
            "a.tar.gz",
            &actual,
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(bad, DownloadError::ChecksumMismatch { .. }));
    }

    #[test]
    fn last_lines_of_errors() {
        assert_eq!(last_lines("a\n\nb\nc\nd\ne\n"), "b\nc\nd\ne");
        assert_eq!(last_lines(""), "");
    }
}
