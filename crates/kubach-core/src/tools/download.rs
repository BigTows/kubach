//! Downloading a tool release, checking it and unpacking it into Kubach's
//! data directory.
//!
//! A release is trusted by the SHA-256 pinned in Kubach (the version in
//! the [`ToolSpec`]) or, for any other version, by a detached OpenPGP
//! signature from a key Kubach ships ([`SignatureSpec`], #403).
//!
//! Layout: `<root>/<name>/<version>/<binary>` plus [`VERIFICATION_FILE`],
//! where `root` is
//! [`tools_dir`](super::tools_dir). A download goes to a temporary
//! `<root>/<name>/.download-*` directory, which is removed when the future
//! finishes or is dropped (cancel), and only a verified, unpacked binary is
//! renamed into place.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::StreamExt as _;
use http_body_util::BodyExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::archive::{self, ArchiveError, ArchiveKind};
use super::signature::{self, Keyring, SignatureError, Signer};

/// Archives larger than this are refused (helm is about 17 MB).
pub const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
/// Unpacked binaries larger than this are refused.
pub const MAX_BINARY_BYTES: u64 = 512 * 1024 * 1024;
/// Checksum files larger than this are refused.
const MAX_CHECKSUM_BYTES: u64 = 64 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest wait for the next chunk of a response.
const READ_TIMEOUT: Duration = Duration::from_secs(60);
const TEMP_PREFIX: &str = ".download-";
/// Redirects followed per request (GitHub release assets redirect once).
const MAX_REDIRECTS: usize = 5;
/// How a downloaded copy was checked, next to its binary.
pub const VERIFICATION_FILE: &str = "verification.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Os {
    MacOs,
    Linux,
    Windows,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Arch {
    Amd64,
    Arm64,
}

/// An OS and CPU a tool release is built for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Platform {
    pub os: Os,
    pub arch: Arch,
}

impl Platform {
    pub const fn new(os: Os, arch: Arch) -> Self {
        Self { os, arch }
    }

    /// The platform Kubach runs on, if releases exist for it.
    pub fn current() -> Option<Self> {
        Self::from_names(std::env::consts::OS, std::env::consts::ARCH)
    }

    /// From Rust's `std::env::consts::{OS, ARCH}` names.
    pub fn from_names(os: &str, arch: &str) -> Option<Self> {
        let os = match os {
            "macos" => Os::MacOs,
            "linux" => Os::Linux,
            "windows" => Os::Windows,
            _ => return None,
        };
        let arch = match arch {
            "x86_64" => Arch::Amd64,
            "aarch64" => Arch::Arm64,
            _ => return None,
        };
        Some(Self { os, arch })
    }

    /// Go's `GOOS`: `darwin`, `linux`, `windows`.
    pub fn go_os(self) -> &'static str {
        match self.os {
            Os::MacOs => "darwin",
            Os::Linux => "linux",
            Os::Windows => "windows",
        }
    }

    /// Go's `GOARCH`: `amd64`, `arm64`.
    pub fn go_arch(self) -> &'static str {
        match self.arch {
            Arch::Amd64 => "amd64",
            Arch::Arm64 => "arm64",
        }
    }

    /// `.exe` on Windows, else empty.
    pub fn exe_suffix(self) -> &'static str {
        if self.os == Os::Windows { ".exe" } else { "" }
    }
}

/// The release archive of one platform and its pinned SHA-256.
#[derive(Clone, Copy, Debug)]
pub struct Artifact {
    pub platform: Platform,
    /// Lowercase hex SHA-256 of the archive, checked into Kubach.
    pub sha256: &'static str,
}

/// Where the detached OpenPGP signatures of a tool's releases are and the
/// keys trusted to make them.
#[derive(Debug)]
pub struct SignatureSpec {
    /// URL of the signature of the archive for a version and platform.
    pub archive_url: fn(&str, Platform) -> String,
    /// URL of the signature of the published checksum file.
    pub checksum_url: fn(&str, Platform) -> String,
    /// The trusted keys.
    pub keyring: fn() -> &'static Keyring,
}

/// How to get a tool: a pinned version, its archives and where the binary
/// is inside them. A new tool is one more `ToolSpec` value.
#[derive(Debug)]
pub struct ToolSpec {
    /// Binary name without `.exe`; also the directory under the tools root.
    pub name: &'static str,
    /// Display name, e.g. "Helm".
    pub title: &'static str,
    /// Pinned version, without a leading `v`.
    pub version: &'static str,
    /// One per supported platform.
    pub artifacts: &'static [Artifact],
    /// HTTPS URL of the archive for a version and platform.
    pub archive_url: fn(&str, Platform) -> String,
    /// URL of the published checksum file next to the archive.
    pub checksum_url: fn(&str, Platform) -> String,
    /// Path of the binary inside the archive.
    pub member: fn(Platform) -> String,
    /// Release signatures; without them only the pinned version can be
    /// downloaded.
    pub signature: Option<SignatureSpec>,
}

impl ToolSpec {
    pub fn artifact(&self, platform: Platform) -> Option<&Artifact> {
        self.artifacts.iter().find(|a| a.platform == platform)
    }

    /// Archive URL of the pinned version, when `platform` is supported.
    pub fn url(&self, platform: Platform) -> Option<String> {
        self.artifact(platform)
            .map(|_| (self.archive_url)(self.version, platform))
    }

    /// `name` with `.exe` on Windows.
    pub fn binary_name(&self, platform: Platform) -> String {
        format!("{}{}", self.name, platform.exe_suffix())
    }

    /// `<root>/<name>`.
    pub fn dir(&self, root: &Path) -> PathBuf {
        root.join(self.name)
    }
}

/// A downloaded copy of a tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    pub version: String,
    pub path: PathBuf,
}

impl Installed {
    /// How this copy was checked when it was downloaded; `None` for a copy
    /// from before #403 or when the file is unreadable.
    pub fn verification(&self) -> Option<Verification> {
        let text = fs::read_to_string(self.path.parent()?.join(VERIFICATION_FILE)).ok()?;
        serde_json::from_str(&text).ok()
    }
}

/// How a downloaded archive was checked; kept as [`VERIFICATION_FILE`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verification {
    /// SHA-256 of the archive (it matched the published checksum).
    pub sha256: String,
    /// The archive matched the SHA-256 pinned in Kubach.
    pub pinned: bool,
    /// Who signed the archive, when the signature was valid.
    pub signer: Option<Signer>,
    /// Why the signature of a pinned version was not checked or did not
    /// verify (the pinned SHA-256 was enough to install it).
    pub signature_note: Option<String>,
}

/// Bytes received so far; `total` from `Content-Length` when sent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub downloaded: u64,
    pub total: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("{tool} can't be downloaded for this OS and CPU")]
    UnsupportedPlatform { tool: &'static str },
    #[error("no data directory to download into")]
    NoDataDir,
    #[error("invalid URL {0}")]
    InvalidUrl(String),
    #[error("can't reach {host}: {message}")]
    Network { host: String, message: String },
    #[error("{url} answered {status}")]
    Http { url: String, status: u16 },
    #[error("download is larger than {limit} bytes")]
    TooLarge { limit: u64 },
    #[error("no checksum for {file} in {url}")]
    ChecksumMissing { file: String, url: String },
    #[error(
        "SHA-256 of {file} does not match {against}: expected {expected}, got {actual}; \
         the file was deleted"
    )]
    ChecksumMismatch {
        file: String,
        /// "the published checksum" or "the checksum pinned in Kubach".
        against: &'static str,
        expected: String,
        actual: String,
    },
    #[error("the OpenPGP signature of {file} is not valid: {source}")]
    Signature {
        file: String,
        source: SignatureError,
    },
    #[error("{tool} {version} can't be checked: no signature keys and no pinned checksum")]
    Untrusted { tool: &'static str, version: String },
    #[error("too many redirects from {0}")]
    TooManyRedirects(String),
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    #[error("{0}")]
    Io(#[from] io::Error),
}

impl DownloadError {
    /// Whether the release could not be fetched or trusted (network, HTTP,
    /// checksum or signature), rather than a local failure: then another
    /// release (the pinned one) may still work.
    pub fn is_release_problem(&self) -> bool {
        matches!(
            self,
            Self::Network { .. }
                | Self::Http { .. }
                | Self::TooLarge { .. }
                | Self::ChecksumMissing { .. }
                | Self::ChecksumMismatch { .. }
                | Self::Signature { .. }
                | Self::Untrusted { .. }
                | Self::TooManyRedirects(_)
                | Self::InvalidUrl(_)
                | Self::Archive(_)
        )
    }
}

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The hash for `file` in a checksum file: a lone hash, or `<hash>
/// [*]<file>` lines as written by `sha256sum`.
pub fn parse_checksum(text: &str, file: &str) -> Option<String> {
    let is_hash = |s: &str| s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit());
    let lines: Vec<Vec<&str>> = text
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .filter(|parts| !parts.is_empty())
        .collect();
    let named = lines.iter().find_map(|parts| match parts.as_slice() {
        [hash, name] if is_hash(hash) && name.trim_start_matches('*') == file => Some(*hash),
        _ => None,
    });
    let lone = match lines.as_slice() {
        [parts] if parts.len() == 1 && is_hash(parts[0]) => Some(parts[0]),
        _ => None,
    };
    named.or(lone).map(str::to_ascii_lowercase)
}

/// Checks `actual` against the published and, when given, the pinned hash.
pub fn verify(
    file: &str,
    actual: &str,
    published: &str,
    pinned: Option<&str>,
) -> Result<(), DownloadError> {
    let mismatch = |against, expected: &str| DownloadError::ChecksumMismatch {
        file: file.to_owned(),
        against,
        expected: expected.to_owned(),
        actual: actual.to_owned(),
    };
    if !actual.eq_ignore_ascii_case(published) {
        return Err(mismatch("the published checksum", published));
    }
    if let Some(pinned) = pinned
        && !actual.eq_ignore_ascii_case(pinned)
    {
        return Err(mismatch("the checksum pinned in Kubach", pinned));
    }
    Ok(())
}

/// Numeric parts of a version, for ordering (`3.22.1` → `[3, 22, 1]`).
pub fn version_key(version: &str) -> Vec<u64> {
    version
        .trim_start_matches('v')
        .split(['.', '-', '+'])
        .map_while(|part| part.parse().ok())
        .collect()
}

/// Every downloaded version of `spec` under `root`, newest first.
pub fn installed_versions(spec: &ToolSpec, root: &Path) -> Vec<Installed> {
    let Some(platform) = Platform::current() else {
        return Vec::new();
    };
    installed_versions_for(spec, root, platform)
}

fn installed_versions_for(spec: &ToolSpec, root: &Path, platform: Platform) -> Vec<Installed> {
    let binary = spec.binary_name(platform);
    let Ok(entries) = fs::read_dir(spec.dir(root)) else {
        return Vec::new();
    };
    let mut found: Vec<Installed> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let version = entry.file_name().to_str()?.to_owned();
            if version.starts_with('.') {
                return None;
            }
            let path = entry.path().join(&binary);
            path.is_file().then_some(Installed { version, path })
        })
        .collect();
    found.sort_by_key(|installed| std::cmp::Reverse(version_key(&installed.version)));
    found
}

/// The downloaded copy of `spec` to use: the pinned version, else the
/// newest one found.
pub fn installed(spec: &ToolSpec, root: &Path) -> Option<Installed> {
    let versions = installed_versions(spec, root);
    let pinned = versions.iter().position(|i| i.version == spec.version);
    let mut versions = versions;
    match pinned {
        Some(ix) => Some(versions.swap_remove(ix)),
        None => versions.into_iter().next(),
    }
}

/// Deletes every downloaded version of `spec`.
pub fn remove_all(spec: &ToolSpec, root: &Path) -> io::Result<()> {
    match fs::remove_dir_all(spec.dir(root)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Deletes the downloaded versions of `spec` other than `keep`, and
/// leftovers of interrupted downloads.
pub fn remove_other_versions(spec: &ToolSpec, root: &Path, keep: &str) -> io::Result<()> {
    let entries = match fs::read_dir(spec.dir(root)) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries.filter_map(Result::ok) {
        if entry.file_name() != keep && entry.path().is_dir() {
            fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}

/// Downloads the pinned version of `spec` for the current platform into
/// `root`: see [`download_version`].
pub async fn download(
    spec: &'static ToolSpec,
    root: &Path,
    progress: impl FnMut(Progress) + Send,
) -> Result<Installed, DownloadError> {
    download_version(spec, spec.version, root, progress)
        .await
        .map(|(installed, _)| installed)
}

/// Downloads `version` of `spec` for the current platform into `root`,
/// checks it, unpacks the binary and returns it with how it was checked.
///
/// The archive's SHA-256 must match the published checksum file. The
/// pinned version must also match the SHA-256 pinned in Kubach; its
/// signature is checked when the tool has one, but a failure there is only
/// noted. Any other version must carry valid OpenPGP signatures of both
/// the checksum file and the archive by a key in the tool's keyring.
/// Reports bytes received to `progress`. Dropping the future cancels and
/// cleans up. Must run on a tokio runtime.
pub async fn download_version(
    spec: &'static ToolSpec,
    version: &str,
    root: &Path,
    progress: impl FnMut(Progress) + Send,
) -> Result<(Installed, Verification), DownloadError> {
    let platform =
        Platform::current().ok_or(DownloadError::UnsupportedPlatform { tool: spec.name })?;
    download_for(spec, version, platform, root, progress).await
}

async fn download_for(
    spec: &'static ToolSpec,
    version: &str,
    platform: Platform,
    root: &Path,
    mut progress: impl FnMut(Progress) + Send,
) -> Result<(Installed, Verification), DownloadError> {
    let artifact = spec
        .artifact(platform)
        .ok_or(DownloadError::UnsupportedPlatform { tool: spec.name })?;
    let pinned = (version == spec.version).then_some(artifact.sha256);
    if pinned.is_none() && spec.signature.is_none() {
        return Err(DownloadError::Untrusted {
            tool: spec.name,
            version: version.to_owned(),
        });
    }
    let url = (spec.archive_url)(version, platform);
    let file_name = file_name(&url)?;
    let tool_dir = spec.dir(root);
    fs::create_dir_all(&tool_dir)?;
    let temp = tempfile::Builder::new()
        .prefix(TEMP_PREFIX)
        .tempdir_in(&tool_dir)?;

    let checksum_url = (spec.checksum_url)(version, platform);
    let mut checksum = Vec::new();
    fetch(&checksum_url, &mut checksum, MAX_CHECKSUM_BYTES, |_| {}).await?;
    let published =
        parse_checksum(&String::from_utf8_lossy(&checksum), &file_name).ok_or_else(|| {
            DownloadError::ChecksumMissing {
                file: file_name.clone(),
                url: checksum_url.clone(),
            }
        })?;

    // The checksum file's signature is checked now, the archive's once the
    // archive is here.
    let mut signature_note = None;
    let mut archive_signature = None;
    if let Some(signing) = &spec.signature {
        match fetch_signatures(signing, version, platform, &checksum, &checksum_url).await {
            Ok(signature) => archive_signature = Some(signature),
            Err(error) if pinned.is_some() => signature_note = Some(error.to_string()),
            Err(error) => return Err(error),
        }
    }

    let archive_path = temp.path().join(&file_name);
    let mut sink = HashingFile::create(&archive_path)?;
    fetch(&url, &mut sink, MAX_ARCHIVE_BYTES, &mut progress).await?;
    let actual = sink.finish()?;
    if let Err(error) = verify(&file_name, &actual, &published, pinned) {
        fs::remove_file(&archive_path).ok();
        return Err(error);
    }

    let mut signer = None;
    if let (Some(signing), Some(signature)) = (&spec.signature, archive_signature) {
        match verify_file_signature((signing.keyring)(), &signature, &archive_path, &file_name) {
            Ok(by) => signer = Some(by),
            Err(error) if pinned.is_some() => signature_note = Some(error.to_string()),
            Err(error) => {
                fs::remove_file(&archive_path).ok();
                return Err(error);
            }
        }
    }
    let verification = Verification {
        sha256: actual,
        pinned: pinned.is_some(),
        signer,
        signature_note,
    };

    let installed = install_archive(
        spec,
        version,
        platform,
        &tool_dir,
        temp.path(),
        &archive_path,
        &verification,
    )?;
    Ok((installed, verification))
}

/// Fetches the signature of the checksum file and checks it against
/// `checksum`, then fetches the signature of the archive, to check once
/// the archive is downloaded.
async fn fetch_signatures(
    signing: &SignatureSpec,
    version: &str,
    platform: Platform,
    checksum: &[u8],
    checksum_url: &str,
) -> Result<Vec<u8>, DownloadError> {
    let mut checksum_signature = Vec::new();
    fetch(
        &(signing.checksum_url)(version, platform),
        &mut checksum_signature,
        signature::MAX_SIGNATURE_BYTES,
        |_| {},
    )
    .await?;
    (signing.keyring)()
        .verify(&checksum_signature, checksum)
        .map_err(|source| DownloadError::Signature {
            file: file_name(checksum_url).unwrap_or_default(),
            source,
        })?;
    let mut archive_signature = Vec::new();
    fetch(
        &(signing.archive_url)(version, platform),
        &mut archive_signature,
        signature::MAX_SIGNATURE_BYTES,
        |_| {},
    )
    .await?;
    Ok(archive_signature)
}

/// Checks the detached `signature` of the file at `path`.
fn verify_file_signature(
    keyring: &Keyring,
    signature: &[u8],
    path: &Path,
    file_name: &str,
) -> Result<Signer, DownloadError> {
    let file = fs::File::open(path)?;
    keyring
        .verify(signature, io::BufReader::new(file))
        .map_err(|source| DownloadError::Signature {
            file: file_name.to_owned(),
            source,
        })
}

/// Unpacks the verified archive at `archive_path` into `<tool_dir>/<version>`
/// through the temporary directory `temp` (on the same file system).
fn install_archive(
    spec: &ToolSpec,
    version: &str,
    platform: Platform,
    tool_dir: &Path,
    temp: &Path,
    archive_path: &Path,
    verification: &Verification,
) -> Result<Installed, DownloadError> {
    let name = archive_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let kind = ArchiveKind::from_name(name)
        .ok_or_else(|| ArchiveError::Corrupt(format!("{name}: unknown archive type")))?;
    // The version names a directory: it came over the network.
    if version.is_empty() || version.starts_with('.') || version.contains(['/', '\\']) {
        return Err(DownloadError::InvalidUrl(version.to_owned()));
    }
    let staged = temp.join(version);
    fs::create_dir(&staged)?;
    let binary = staged.join(spec.binary_name(platform));
    archive::extract_member(
        archive_path,
        kind,
        &(spec.member)(platform),
        &binary,
        MAX_BINARY_BYTES,
    )?;
    make_executable(&binary)?;
    clear_quarantine(&binary);
    let record = serde_json::to_vec_pretty(verification).map_err(io::Error::other)?;
    fs::write(staged.join(VERIFICATION_FILE), record)?;

    let target = tool_dir.join(version);
    if target.exists() {
        // A broken or partial copy of the same version: swap it out.
        fs::rename(&target, temp.join("previous"))?;
    }
    fs::rename(&staged, &target)?;
    Ok(Installed {
        version: version.to_owned(),
        path: target.join(spec.binary_name(platform)),
    })
}

fn file_name(url: &str) -> Result<String, DownloadError> {
    url.rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| DownloadError::InvalidUrl(url.to_owned()))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn make_executable(_: &Path) -> io::Result<()> {
    Ok(())
}

/// Removes `com.apple.quarantine` if the system added it, so Gatekeeper
/// does not block the binary. Needs no extra rights: the file is ours.
#[cfg(target_os = "macos")]
fn clear_quarantine(path: &Path) {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return;
    };
    // SAFETY: both pointers are valid NUL-terminated strings for the call.
    unsafe {
        libc::removexattr(
            path.as_ptr(),
            c"com.apple.quarantine".as_ptr(),
            libc::XATTR_NOFOLLOW,
        );
    }
}

#[cfg(not(target_os = "macos"))]
fn clear_quarantine(_: &Path) {}

/// A file being written that hashes what goes through it.
struct HashingFile {
    file: fs::File,
    hasher: Sha256,
}

impl HashingFile {
    fn create(path: &Path) -> io::Result<Self> {
        Ok(Self {
            file: fs::File::create_new(path)?,
            hasher: Sha256::new(),
        })
    }

    /// Flushes to disk and returns the hex SHA-256.
    fn finish(self) -> io::Result<String> {
        self.file.sync_all()?;
        Ok(hex(&self.hasher.finalize()))
    }
}

impl io::Write for HashingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.file.write(buf)?;
        self.hasher.update(&buf[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// GETs `url` into `sink`, at most `limit` bytes, through the proxy from
/// the environment ([`super::proxy::from_env`]). Follows up to
/// [`MAX_REDIRECTS`] redirects, never from `https` to `http`.
pub(super) async fn fetch(
    url: &str,
    sink: &mut impl io::Write,
    limit: u64,
    mut progress: impl FnMut(Progress),
) -> Result<(), DownloadError> {
    let mut uri: http::Uri = url
        .parse()
        .map_err(|_| DownloadError::InvalidUrl(url.to_owned()))?;
    let mut redirects = 0;
    let (response, host) = loop {
        let (response, host) = send(&uri, url).await?;
        let status = response.status();
        if !status.is_redirection() {
            break (response, host);
        }
        let location = response
            .headers()
            .get(http::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| DownloadError::Http {
                url: url.to_owned(),
                status: status.as_u16(),
            })?;
        redirects += 1;
        if redirects > MAX_REDIRECTS {
            return Err(DownloadError::TooManyRedirects(url.to_owned()));
        }
        uri = redirect_target(&uri, location)
            .ok_or_else(|| DownloadError::InvalidUrl(location.to_owned()))?;
    };
    let network = |message: String| DownloadError::Network {
        host: host.clone(),
        message,
    };
    let status = response.status();
    if !status.is_success() {
        return Err(DownloadError::Http {
            url: url.to_owned(),
            status: status.as_u16(),
        });
    }
    let total = response
        .headers()
        .get(http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok()?.parse::<u64>().ok());
    if total.is_some_and(|total| total > limit) {
        return Err(DownloadError::TooLarge { limit });
    }
    let mut downloaded = 0u64;
    progress(Progress { downloaded, total });
    let mut body = response.into_body().into_data_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|e| network(e.to_string()))?;
        downloaded += chunk.len() as u64;
        if downloaded > limit {
            return Err(DownloadError::TooLarge { limit });
        }
        sink.write_all(&chunk)?;
        progress(Progress { downloaded, total });
    }
    if total.is_some_and(|total| total != downloaded) {
        return Err(network("the connection closed before the end".into()));
    }
    Ok(())
}

/// One `GET` of `uri` (`url` is the address asked for, for errors);
/// returns the response and the host it came from.
async fn send(
    uri: &http::Uri,
    url: &str,
) -> Result<(http::Response<kube::client::Body>, String), DownloadError> {
    let (Some(scheme), Some(authority)) = (uri.scheme(), uri.authority()) else {
        return Err(DownloadError::InvalidUrl(url.to_owned()));
    };
    let host = authority.host().to_owned();
    let network = |message: String| DownloadError::Network {
        host: host.clone(),
        message,
    };
    let base: http::Uri = format!("{scheme}://{authority}")
        .parse()
        .map_err(|_| DownloadError::InvalidUrl(url.to_owned()))?;
    let mut config = kube::Config::new(base);
    config.connect_timeout = Some(CONNECT_TIMEOUT);
    config.read_timeout = Some(READ_TIMEOUT);
    config.proxy_url = super::proxy::from_env(uri);
    let client = kube::Client::try_from(config).map_err(|e| network(e.to_string()))?;
    let path = uri.path_and_query().map_or("/", |p| p.as_str());
    let request = http::Request::get(path)
        .body(kube::client::Body::empty())
        .map_err(|_| DownloadError::InvalidUrl(url.to_owned()))?;
    let response = client
        .send(request)
        .await
        .map_err(|e| network(e.to_string()))?;
    Ok((response, host))
}

/// Where a redirect from `from` to `location` leads: an absolute URL or a
/// path on the same host. `None` for anything else, and for a step down
/// from `https` to `http`.
fn redirect_target(from: &http::Uri, location: &str) -> Option<http::Uri> {
    let target: http::Uri = if location.starts_with('/') && !location.starts_with("//") {
        format!(
            "{}://{}{location}",
            from.scheme_str()?,
            from.authority()?.as_str()
        )
        .parse()
        .ok()?
    } else {
        location.parse().ok()?
    };
    let scheme = target.scheme_str()?;
    target.authority()?;
    let allowed = match from.scheme_str()? {
        "https" => scheme == "https",
        _ => matches!(scheme, "http" | "https"),
    };
    allowed.then_some(target)
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::super::archive::fixtures;
    use super::*;

    const LINUX: Platform = Platform::new(Os::Linux, Arch::Amd64);
    const WINDOWS: Platform = Platform::new(Os::Windows, Arch::Arm64);

    fn test_url(version: &str, p: Platform) -> String {
        let ext = if p.os == Os::Windows { "zip" } else { "tar.gz" };
        format!(
            "https://example.com/tool-v{version}-{}-{}.{ext}",
            p.go_os(),
            p.go_arch()
        )
    }

    fn test_checksum_url(version: &str, p: Platform) -> String {
        format!("{}.sha256sum", test_url(version, p))
    }

    fn test_member(p: Platform) -> String {
        format!("{}-{}/tool{}", p.go_os(), p.go_arch(), p.exe_suffix())
    }

    static TOOL: ToolSpec = ToolSpec {
        name: "tool",
        title: "Tool",
        version: "1.2.3",
        artifacts: &[
            Artifact {
                platform: LINUX,
                sha256: "aa",
            },
            Artifact {
                platform: WINDOWS,
                sha256: "bb",
            },
        ],
        archive_url: test_url,
        checksum_url: test_checksum_url,
        member: test_member,
        signature: None,
    };

    #[test]
    fn platform_from_rust_names() {
        assert_eq!(
            Platform::from_names("macos", "aarch64"),
            Some(Platform::new(Os::MacOs, Arch::Arm64))
        );
        assert_eq!(
            Platform::from_names("linux", "x86_64"),
            Some(Platform::new(Os::Linux, Arch::Amd64))
        );
        assert_eq!(
            Platform::from_names("windows", "aarch64"),
            Some(Platform::new(Os::Windows, Arch::Arm64))
        );
        assert_eq!(Platform::from_names("freebsd", "x86_64"), None);
        assert_eq!(Platform::from_names("linux", "riscv64"), None);
    }

    #[test]
    fn url_only_for_described_platforms() {
        assert_eq!(
            TOOL.url(LINUX).as_deref(),
            Some("https://example.com/tool-v1.2.3-linux-amd64.tar.gz")
        );
        assert_eq!(TOOL.url(Platform::new(Os::MacOs, Arch::Arm64)), None);
        assert_eq!(TOOL.binary_name(WINDOWS), "tool.exe");
        assert_eq!(TOOL.binary_name(LINUX), "tool");
    }

    #[test]
    fn checksum_file_formats() {
        let hash = "E73DA8A0B0D5193774FDFF17CE6319C7436BB22EE229B45FD64F3C40368C753A";
        let lower = hash.to_ascii_lowercase();
        assert_eq!(
            parse_checksum(&format!("{hash}  helm.tar.gz\n"), "helm.tar.gz").as_deref(),
            Some(lower.as_str())
        );
        assert_eq!(
            parse_checksum(&format!("{hash}\n"), "helm.tar.gz").as_deref(),
            Some(lower.as_str())
        );
        let many = format!("{}  other.tar.gz\n{hash} *helm.tar.gz\n", "0".repeat(64));
        assert_eq!(
            parse_checksum(&many, "helm.tar.gz").as_deref(),
            Some(lower.as_str())
        );
        assert_eq!(parse_checksum(&many, "missing.zip"), None);
        assert_eq!(
            parse_checksum("<html>not found</html>", "helm.tar.gz"),
            None
        );
    }

    #[test]
    fn verify_needs_both_hashes() {
        let actual = sha256_hex(b"archive");
        assert_eq!(
            actual,
            "0eb3e36bfb24dcd9bb1d1bece1531216b59539a8fde17ee80224af0653c92aa3"
        );
        verify("a", &actual, &actual.to_ascii_uppercase(), Some(&actual)).unwrap();
        // A signed version has no pinned hash: the published one decides.
        verify("a", &actual, &actual, None).unwrap();
        let published = verify("a", &actual, &"0".repeat(64), Some(&actual)).unwrap_err();
        assert!(matches!(
            published,
            DownloadError::ChecksumMismatch {
                against: "the published checksum",
                ..
            }
        ));
        let pinned = verify("a", &actual, &actual, Some(&"0".repeat(64))).unwrap_err();
        assert!(matches!(
            pinned,
            DownloadError::ChecksumMismatch {
                against: "the checksum pinned in Kubach",
                ..
            }
        ));
    }

    #[test]
    fn versions_order_numerically() {
        assert!(version_key("3.22.1") > version_key("3.9.10"));
        assert!(version_key("v3.10.0") > version_key("3.9.0"));
    }

    fn fake_install(root: &Path, version: &str, platform: Platform) {
        let dir = TOOL.dir(root).join(version);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(TOOL.binary_name(platform)), b"bin").unwrap();
    }

    #[test]
    fn finds_pinned_then_newest_and_removes() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let platform = Platform::current().unwrap();
        assert_eq!(installed(&TOOL, root), None);
        fake_install(root, "1.0.0", platform);
        fake_install(root, "1.10.0", platform);
        // A leftover download and a version without the binary are skipped.
        fs::create_dir_all(TOOL.dir(root).join(".download-x/1.2.3")).unwrap();
        fs::create_dir_all(TOOL.dir(root).join("9.9.9")).unwrap();
        let newest = installed(&TOOL, root).unwrap();
        assert_eq!(newest.version, "1.10.0");
        fake_install(root, "1.2.3", platform);
        let pinned = installed(&TOOL, root).unwrap();
        assert_eq!(pinned.version, "1.2.3");
        assert_eq!(
            pinned.path,
            TOOL.dir(root)
                .join("1.2.3")
                .join(TOOL.binary_name(platform))
        );
        assert_eq!(installed_versions(&TOOL, root).len(), 3);

        remove_other_versions(&TOOL, root, "1.2.3").unwrap();
        let left: Vec<String> = installed_versions(&TOOL, root)
            .into_iter()
            .map(|i| i.version)
            .collect();
        assert_eq!(left, ["1.2.3"]);
        assert!(!TOOL.dir(root).join(".download-x").exists());

        remove_all(&TOOL, root).unwrap();
        assert_eq!(installed(&TOOL, root), None);
        remove_all(&TOOL, root).unwrap();
    }

    fn install_fixture(
        platform: Platform,
        build: impl Fn(&Path),
    ) -> (tempfile::TempDir, Installed) {
        install_fixture_version(platform, TOOL.version, build)
    }

    fn install_fixture_version(
        platform: Platform,
        version: &str,
        build: impl Fn(&Path),
    ) -> (tempfile::TempDir, Installed) {
        let root = tempfile::tempdir().unwrap();
        let tool_dir = TOOL.dir(root.path());
        fs::create_dir_all(&tool_dir).unwrap();
        let temp = tempfile::Builder::new()
            .prefix(TEMP_PREFIX)
            .tempdir_in(&tool_dir)
            .unwrap();
        let url = test_url(version, platform);
        let archive = temp.path().join(file_name(&url).unwrap());
        build(&archive);
        let verification = Verification {
            sha256: "ab".repeat(32),
            pinned: version == TOOL.version,
            signer: None,
            signature_note: None,
        };
        let installed = install_archive(
            &TOOL,
            version,
            platform,
            &tool_dir,
            temp.path(),
            &archive,
            &verification,
        )
        .unwrap();
        drop(temp);
        (root, installed)
    }

    #[test]
    fn installs_from_tar_gz_atomically() {
        let (root, installed) = install_fixture(LINUX, |archive| {
            fixtures::tar_gz(
                archive,
                &[
                    ("linux-amd64/LICENSE", b"l"),
                    ("linux-amd64/tool", b"#!/bin/sh\n"),
                ],
            )
        });
        assert_eq!(installed.version, "1.2.3");
        assert_eq!(installed.path, TOOL.dir(root.path()).join("1.2.3/tool"));
        assert_eq!(fs::read(&installed.path).unwrap(), b"#!/bin/sh\n");
        #[cfg(unix)]
        assert!(crate::which::is_executable(&installed.path));
        // Only the version directory is left: the temporary one is gone.
        let names: Vec<_> = fs::read_dir(TOOL.dir(root.path()))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["1.2.3"]);
    }

    #[test]
    fn installs_from_zip_and_replaces_a_broken_copy() {
        let (root, installed) = install_fixture(WINDOWS, |archive| {
            let tool_dir = archive.parent().unwrap().parent().unwrap();
            // A previous copy of the same version, without the binary.
            fs::create_dir_all(tool_dir.join("1.2.3")).unwrap();
            fixtures::zip(archive, &[("windows-arm64/tool.exe", b"MZ")]);
        });
        assert_eq!(installed.path, TOOL.dir(root.path()).join("1.2.3/tool.exe"));
        assert_eq!(fs::read(&installed.path).unwrap(), b"MZ");
    }

    #[test]
    fn hashing_file_hashes_what_it_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        let mut file = HashingFile::create(&path).unwrap();
        file.write_all(b"arch").unwrap();
        file.write_all(b"ive").unwrap();
        assert_eq!(file.finish().unwrap(), sha256_hex(b"archive"));
        assert_eq!(fs::read(&path).unwrap(), b"archive");
    }

    #[tokio::test]
    async fn unsupported_platform_is_an_error() {
        let root = tempfile::tempdir().unwrap();
        let error = download_for(
            &TOOL,
            TOOL.version,
            Platform::new(Os::MacOs, Arch::Amd64),
            root.path(),
            |_| {},
        )
        .await
        .unwrap_err();
        assert!(matches!(error, DownloadError::UnsupportedPlatform { .. }));
    }

    #[tokio::test]
    async fn other_versions_need_signatures() {
        let root = tempfile::tempdir().unwrap();
        let error = download_for(&TOOL, "9.9.9", LINUX, root.path(), |_| {})
            .await
            .unwrap_err();
        assert!(matches!(error, DownloadError::Untrusted { .. }), "{error}");
        assert!(error.is_release_problem());
        assert!(!DownloadError::NoDataDir.is_release_problem());
    }

    #[test]
    fn installs_another_version_with_its_verification() {
        let (root, installed) = install_fixture_version(LINUX, "2.0.0", |archive| {
            fixtures::tar_gz(archive, &[("linux-amd64/tool", b"v2")])
        });
        assert_eq!(installed.version, "2.0.0");
        assert_eq!(installed.path, TOOL.dir(root.path()).join("2.0.0/tool"));
        let verification = installed.verification().unwrap();
        assert!(!verification.pinned);
        assert_eq!(verification.sha256, "ab".repeat(32));
        // The record does not make the directory look like a broken copy.
        assert_eq!(
            installed_versions_for(&TOOL, root.path(), LINUX),
            [installed]
        );
    }

    #[test]
    fn refuses_version_names_that_are_not_one_directory() {
        let root = tempfile::tempdir().unwrap();
        let tool_dir = TOOL.dir(root.path());
        fs::create_dir_all(&tool_dir).unwrap();
        let archive = tool_dir.join("a.tar.gz");
        fixtures::tar_gz(&archive, &[("linux-amd64/tool", b"x")]);
        for bad in ["", "../x", "a/b", ".hidden", "a\\b"] {
            let error = install_archive(
                &TOOL,
                bad,
                LINUX,
                &tool_dir,
                &tool_dir,
                &archive,
                &Verification::default(),
            )
            .unwrap_err();
            assert!(matches!(error, DownloadError::InvalidUrl(_)), "{bad}");
        }
    }

    #[test]
    fn redirects_stay_on_https() {
        let from: http::Uri = "https://github.com/helm/helm/releases/download/v4.3.1/x.asc"
            .parse()
            .unwrap();
        let absolute =
            redirect_target(&from, "https://release-assets.example.com/a?sig=1").unwrap();
        assert_eq!(absolute.host(), Some("release-assets.example.com"));
        assert_eq!(absolute.path_and_query().unwrap().as_str(), "/a?sig=1");
        let relative = redirect_target(&from, "/other/path").unwrap();
        assert_eq!(relative.to_string(), "https://github.com/other/path");
        assert_eq!(redirect_target(&from, "http://example.com/a"), None);
        assert_eq!(redirect_target(&from, "//example.com/a"), None);
        assert_eq!(redirect_target(&from, "file:///etc/passwd"), None);
        let plain: http::Uri = "http://example.com/a".parse().unwrap();
        assert!(redirect_target(&plain, "https://example.com/b").is_some());
    }
}
