//! Taking one file out of a release archive (`.tar.gz` or `.zip`).
//!
//! Only the named member is written, to a path the caller picks, so
//! entries with `..` or absolute paths in the archive can't escape.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

/// Archive formats of tool releases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveKind {
    TarGz,
    Zip,
}

impl ArchiveKind {
    /// From the file name of the archive: `.zip`, else `.tar.gz`/`.tgz`.
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.to_ascii_lowercase();
        if name.ends_with(".zip") {
            Some(Self::Zip)
        } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            Some(Self::TarGz)
        } else {
            None
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("{0} is not in the archive")]
    MissingMember(String),
    #[error("{0} in the archive is not a regular file")]
    NotAFile(String),
    #[error("{member} is larger than {limit} bytes")]
    TooLarge { member: String, limit: u64 },
    #[error("can't read the archive: {0}")]
    Corrupt(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Copies `member` (a `/`-separated path, `./` prefixes ignored) of the
/// archive at `archive` to the new file `dest`, at most `limit` bytes.
pub fn extract_member(
    archive: &Path,
    kind: ArchiveKind,
    member: &str,
    dest: &Path,
    limit: u64,
) -> Result<(), ArchiveError> {
    let wanted = normalize(member);
    match kind {
        ArchiveKind::TarGz => extract_tar_gz(archive, &wanted, dest, limit),
        ArchiveKind::Zip => extract_zip(archive, &wanted, dest, limit),
    }
}

fn normalize(path: &str) -> String {
    path.replace('\\', "/")
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>()
        .join("/")
}

fn extract_tar_gz(
    archive: &Path,
    wanted: &str,
    dest: &Path,
    limit: u64,
) -> Result<(), ArchiveError> {
    let decoder = flate2::read::GzDecoder::new(File::open(archive)?);
    let mut tar = tar::Archive::new(decoder);
    let corrupt = |e: io::Error| ArchiveError::Corrupt(e.to_string());
    for entry in tar.entries().map_err(corrupt)? {
        let entry = entry.map_err(corrupt)?;
        let path = entry.path().map_err(corrupt)?;
        if normalize(&path.to_string_lossy()) != wanted {
            continue;
        }
        if !entry.header().entry_type().is_file() {
            return Err(ArchiveError::NotAFile(wanted.to_owned()));
        }
        return copy_limited(entry, dest, wanted, limit);
    }
    Err(ArchiveError::MissingMember(wanted.to_owned()))
}

fn extract_zip(archive: &Path, wanted: &str, dest: &Path, limit: u64) -> Result<(), ArchiveError> {
    let corrupt = |e: zip::result::ZipError| ArchiveError::Corrupt(e.to_string());
    let mut zip = zip::ZipArchive::new(File::open(archive)?).map_err(corrupt)?;
    let index = (0..zip.len()).find(|&ix| {
        zip.name_for_index(ix)
            .and_then(Result::ok)
            .is_some_and(|name| normalize(&name) == wanted)
    });
    let Some(index) = index else {
        return Err(ArchiveError::MissingMember(wanted.to_owned()));
    };
    let file = zip.by_index(index).map_err(corrupt)?;
    if !file.is_file() {
        return Err(ArchiveError::NotAFile(wanted.to_owned()));
    }
    copy_limited(file, dest, wanted, limit)
}

fn copy_limited(
    reader: impl Read,
    dest: &Path,
    member: &str,
    limit: u64,
) -> Result<(), ArchiveError> {
    let mut out = File::create_new(dest)?;
    // One byte over the limit tells a too large member from one that fits.
    let copied = io::copy(&mut reader.take(limit + 1), &mut out)?;
    if copied > limit {
        drop(out);
        std::fs::remove_file(dest).ok();
        return Err(ArchiveError::TooLarge {
            member: member.to_owned(),
            limit,
        });
    }
    out.sync_all()?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod fixtures {
    use std::io::Write as _;
    use std::path::Path;

    /// A `.tar.gz` at `path` with `(name, contents)` regular files.
    pub fn tar_gz(path: &Path, files: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut tar = tar::Builder::new(encoder);
        for (name, contents) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            tar.append_data(&mut header, name, *contents).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();
    }

    /// A `.zip` at `path` with `(name, contents)` deflated files.
    pub fn zip(path: &Path, files: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, contents) in files {
            zip.start_file(*name, options).unwrap();
            zip.write_all(contents).unwrap();
        }
        zip.finish().unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures;
    use super::*;

    #[test]
    fn kind_from_name() {
        assert_eq!(
            ArchiveKind::from_name("helm-v3-linux-amd64.tar.gz"),
            Some(ArchiveKind::TarGz)
        );
        assert_eq!(ArchiveKind::from_name("x.TGZ"), Some(ArchiveKind::TarGz));
        assert_eq!(
            ArchiveKind::from_name("helm-v3-windows-amd64.zip"),
            Some(ArchiveKind::Zip)
        );
        assert_eq!(ArchiveKind::from_name("helm.exe"), None);
    }

    #[test]
    fn extracts_member_from_tar_gz() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.tar.gz");
        fixtures::tar_gz(
            &archive,
            &[
                ("linux-amd64/LICENSE", b"license"),
                ("linux-amd64/helm", b"#!/bin/sh\necho helm\n"),
            ],
        );
        let dest = dir.path().join("helm");
        extract_member(
            &archive,
            ArchiveKind::TarGz,
            "./linux-amd64/helm",
            &dest,
            1024,
        )
        .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"#!/bin/sh\necho helm\n");
    }

    #[test]
    fn extracts_member_from_zip() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.zip");
        fixtures::zip(
            &archive,
            &[
                ("windows-arm64/README.md", b"readme"),
                ("windows-arm64/helm.exe", b"MZ fake exe"),
            ],
        );
        let dest = dir.path().join("helm.exe");
        extract_member(
            &archive,
            ArchiveKind::Zip,
            "windows-arm64/helm.exe",
            &dest,
            1024,
        )
        .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"MZ fake exe");
    }

    #[test]
    fn missing_member_and_limit() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.tar.gz");
        fixtures::tar_gz(&archive, &[("helm", b"0123456789")]);
        let dest = dir.path().join("out");
        assert!(matches!(
            extract_member(&archive, ArchiveKind::TarGz, "other", &dest, 1024),
            Err(ArchiveError::MissingMember(_))
        ));
        assert!(matches!(
            extract_member(&archive, ArchiveKind::TarGz, "helm", &dest, 5),
            Err(ArchiveError::TooLarge { .. })
        ));
        assert!(!dest.exists());
    }

    #[test]
    fn garbage_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.zip");
        std::fs::write(&archive, b"not an archive").unwrap();
        let dest = dir.path().join("out");
        assert!(matches!(
            extract_member(&archive, ArchiveKind::Zip, "helm", &dest, 1024),
            Err(ArchiveError::Corrupt(_))
        ));
        let archive = dir.path().join("a.tar.gz");
        std::fs::write(&archive, b"not an archive").unwrap();
        assert!(matches!(
            extract_member(&archive, ArchiveKind::TarGz, "helm", &dest, 1024),
            Err(ArchiveError::Corrupt(_))
        ));
    }
}
