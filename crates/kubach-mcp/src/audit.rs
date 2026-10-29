//! Local audit log: one JSON line per tool call, in every session mode.
//! In client-only mode it is the only record that the agent made a call.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use serde_json::Value;

/// Who made the final decision about a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DecidedBy {
    /// Rejected by the client-side policy; no API call was made.
    Policy,
    /// Approved or rejected by the user.
    User,
    /// Rejected by the API server (RBAC, admission, validation).
    Api,
    /// Allowed without a decision point (reads).
    None,
    /// Granted by the launch arguments (`kubach mcp --grant-read`).
    LaunchArgs,
    /// Granted within the session's access without a question (#15).
    Access,
    /// Approved without a window: the user said not to ask again about
    /// this kind of change in this namespace in this session (#30).
    Remembered,
}

/// One audit record.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AuditEntry {
    /// RFC 3339.
    pub time: String,
    pub session: String,
    pub client: String,
    pub tool: String,
    /// Arguments with credentials masked.
    pub args: Value,
    /// What a change did beyond its arguments: for a Secret written through
    /// the user (#230) the operation, namespace, name and keys. Never
    /// values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change: Option<Value>,
    pub decided_by: DecidedBy,
    /// "Don't ask again this session" (#30): `used` when the change ran
    /// without a window, `saved` when the user ticked it for this call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remembered: Option<String>,
    pub allowed: bool,
    /// Denial or error message, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// `user`, `impersonated:<name>` or `serviceaccount:<ns>/<name>`.
    pub identity: String,
    /// `ok`, `denied`, `rejected`, `error` or an HTTP status code.
    pub result: String,
    pub duration_ms: u64,
}

/// Size limits of the audit log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuditLimits {
    /// The file is rotated before a line would make it larger than this.
    pub max_file_bytes: u64,
    /// Rotated files kept: `<name>.1` (newest) to `<name>.<keep>`.
    pub keep: usize,
    /// Serialized `args` longer than this are replaced with a preview.
    pub max_args_bytes: usize,
    /// `reason` longer than this is cut.
    pub max_reason_bytes: usize,
}

impl Default for AuditLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 10 * 1024 * 1024,
            keep: 2,
            max_args_bytes: 16 * 1024,
            max_reason_bytes: 4 * 1024,
        }
    }
}

/// Appends [`AuditEntry`] lines to a file chosen by the caller.
///
/// The file and its rotated copies are readable by the owner only (`0600`,
/// on Windows a DACL with only the current user); the folder is created
/// `0700`. Long arguments are cut (see [`AuditLimits`]), and the file is
/// rotated by size, so an agent in a loop can't fill the disk.
#[derive(Debug)]
pub struct AuditLog {
    path: PathBuf,
    limits: AuditLimits,
    file: Mutex<File>,
}

impl AuditLog {
    /// Opens `path` for appending with [`AuditLimits::default`].
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::open_with(path, AuditLimits::default())
    }

    /// Opens `path` for appending, creating it and its folder if needed,
    /// and narrows the permissions of an existing file and folder.
    pub fn open_with(path: impl AsRef<Path>, limits: AuditLimits) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            kubach_core::settings::create_private_dir(dir)?;
        }
        let file = open_private(&path)?;
        Ok(Self {
            path,
            limits,
            file: Mutex::new(file),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Writes one line. A full line is written with a single `write_all`
    /// under the lock, so concurrent calls never interleave.
    pub fn record(&self, entry: &AuditEntry) -> io::Result<()> {
        let mut line = serde_json::to_vec(&self.limited(entry)).map_err(io::Error::other)?;
        line.push(b'\n');
        let mut file = self
            .file
            .lock()
            .map_err(|_| io::Error::other("audit lock poisoned"))?;
        self.rotate_if_full(&mut file, line.len() as u64)?;
        file.write_all(&line)?;
        file.flush()
    }

    /// `entry` with long `args` and `reason` cut to the limits.
    fn limited(&self, entry: &AuditEntry) -> AuditEntry {
        let mut entry = entry.clone();
        let args = serde_json::to_string(&entry.args).unwrap_or_default();
        if args.len() > self.limits.max_args_bytes {
            entry.args = serde_json::json!({
                "truncated": true,
                "bytes": args.len(),
                "preview": truncate(&args, self.limits.max_args_bytes),
            });
        }
        if let Some(reason) = &mut entry.reason
            && reason.len() > self.limits.max_reason_bytes
        {
            let cut = truncate(reason, self.limits.max_reason_bytes).len();
            reason.truncate(cut);
            reason.push_str(" [truncated]");
        }
        entry
    }

    /// Before a line of `next` bytes would push the file over the limit,
    /// moves `<name>` to `<name>.1` (shifting older copies) and starts a new
    /// file. If another process (the app and `kubach mcp` share the file)
    /// already rotated it, just reopens the new file.
    fn rotate_if_full(&self, file: &mut File, next: u64) -> io::Result<()> {
        let max = self.limits.max_file_bytes;
        let len = file.metadata()?.len();
        if len == 0 || len + next <= max {
            return Ok(());
        }
        let current = std::fs::metadata(&self.path).map_or(0, |meta| meta.len());
        if current >= len || current + next > max {
            self.rotate()?;
        }
        *file = open_private(&self.path)?;
        Ok(())
    }

    fn rotate(&self) -> io::Result<()> {
        let keep = self.limits.keep;
        if keep == 0 {
            return remove_if_exists(&self.path);
        }
        remove_if_exists(&rotated(&self.path, keep))?;
        for n in (1..keep).rev() {
            let from = rotated(&self.path, n);
            if from.exists() {
                std::fs::rename(&from, rotated(&self.path, n + 1))?;
            }
        }
        match std::fs::rename(&self.path, rotated(&self.path, 1)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }
}

/// Opens `path` for appending, owner-only; an existing file is narrowed too.
fn open_private(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let file = options.open(path)?;
    kubach_core::kubeconfig::restrict_permissions(path)?;
    Ok(file)
}

/// `<path>.<n>`.
fn rotated(path: &Path, n: usize) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{n}"));
    PathBuf::from(name)
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// The longest prefix of `text` of at most `max` bytes on a char boundary.
fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn entry(tool: &str) -> AuditEntry {
        AuditEntry {
            time: "2026-10-22T10:00:00Z".into(),
            session: "abc".into(),
            client: "claude-code".into(),
            tool: tool.into(),
            args: json!({"kind": "Pod"}),
            change: None,
            decided_by: DecidedBy::Policy,
            remembered: None,
            allowed: false,
            reason: Some("namespace".into()),
            identity: "user".into(),
            result: "denied".into(),
            duration_ms: 3,
        }
    }

    #[test]
    fn appends_json_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let log = AuditLog::open(&path).unwrap();
        log.record(&entry("list_resources")).unwrap();
        log.record(&entry("get_resource")).unwrap();
        drop(log);
        // Reopening appends.
        AuditLog::open(&path)
            .unwrap()
            .record(&entry("logs"))
            .unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["tool"], "list_resources");
        assert_eq!(lines[0]["decided_by"], "policy");
        assert_eq!(lines[2]["tool"], "logs");
        assert_eq!(lines[0]["duration_ms"], 3);
    }

    fn read_lines(path: &Path) -> Vec<Value> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn tools(path: &Path) -> Vec<String> {
        read_lines(path)
            .iter()
            .map(|l| l["tool"].as_str().unwrap().to_owned())
            .collect()
    }

    /// Bytes of one line written for `entry("t")`.
    fn line_len() -> u64 {
        serde_json::to_vec(&entry("t")).unwrap().len() as u64 + 1
    }

    #[test]
    fn cuts_long_args_and_reason() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let limits = AuditLimits {
            max_args_bytes: 100,
            max_reason_bytes: 10,
            ..AuditLimits::default()
        };
        let log = AuditLog::open_with(&path, limits).unwrap();
        let mut long = entry("apply");
        long.args = json!({"manifest": "ж".repeat(1000)});
        long.reason = Some("x".repeat(50));
        log.record(&long).unwrap();
        log.record(&entry("get_resource")).unwrap();

        let lines = read_lines(&path);
        let args = &lines[0]["args"];
        assert_eq!(args["truncated"], true);
        assert!(args["bytes"].as_u64().unwrap() > 2000);
        let preview = args["preview"].as_str().unwrap();
        assert!(preview.len() <= 100);
        assert!(preview.starts_with("{\"manifest\":\"жж"));
        assert_eq!(
            lines[0]["reason"],
            format!("{} [truncated]", "x".repeat(10))
        );
        // Short entries are untouched.
        assert_eq!(lines[1]["args"], json!({"kind": "Pod"}));
        assert_eq!(lines[1]["reason"], "namespace");
    }

    #[test]
    fn rotates_by_size_and_keeps_two_copies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let limits = AuditLimits {
            // Three lines per file.
            max_file_bytes: line_len() * 3,
            keep: 2,
            ..AuditLimits::default()
        };
        let log = AuditLog::open_with(&path, limits).unwrap();
        for n in 0..10 {
            log.record(&entry(&n.to_string())).unwrap();
        }
        assert_eq!(tools(&path), ["9"]);
        assert_eq!(tools(&rotated(&path, 1)), ["6", "7", "8"]);
        assert_eq!(tools(&rotated(&path, 2)), ["3", "4", "5"]);
        assert!(!rotated(&path, 3).exists());
    }

    #[test]
    fn reopens_a_file_rotated_by_another_process() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let limits = AuditLimits {
            max_file_bytes: line_len() * 2,
            ..AuditLimits::default()
        };
        let a = AuditLog::open_with(&path, limits).unwrap();
        let b = AuditLog::open_with(&path, limits).unwrap();
        a.record(&entry("1")).unwrap();
        a.record(&entry("2")).unwrap();
        // `a` rotates; `b` still holds the old file, now `.1`.
        a.record(&entry("3")).unwrap();
        // `b` sees its file full, finds the new one with room and reopens it
        // instead of rotating again.
        b.record(&entry("4")).unwrap();
        assert_eq!(tools(&path), ["3", "4"]);
        assert_eq!(tools(&rotated(&path, 1)), ["1", "2"]);
        assert!(!rotated(&path, 2).exists());
    }

    #[cfg(unix)]
    #[test]
    fn file_and_folder_are_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("kubach");
        let path = dir.join("audit.jsonl");
        AuditLog::open(&path).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&path), 0o600);

        // An existing file and folder with open permissions are narrowed.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        AuditLog::open(&path).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&path), 0o600);

        // The rotated copy and the new file are owner-only too.
        let limits = AuditLimits {
            max_file_bytes: line_len(),
            ..AuditLimits::default()
        };
        std::fs::remove_file(&path).unwrap();
        let log = AuditLog::open_with(&path, limits).unwrap();
        log.record(&entry("1")).unwrap();
        log.record(&entry("2")).unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&rotated(&path, 1)), 0o600);
    }
}
