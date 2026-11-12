//! Local audit log: one JSON line per tool call, in every session mode.
//! In client-only mode it is the only record that the agent made a call.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use k8s_openapi::jiff::Timestamp;
use serde::Serialize;
use serde_json::{Value, json};

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
    /// `session`, `client`, `tool`, `identity`, `result` and `remembered`
    /// longer than this are cut: the caller writes some of them.
    pub max_field_bytes: usize,
    /// `args` and `reason` of a call the policy refused are cut to this:
    /// nothing ran, and its arguments may be anything.
    pub max_refused_bytes: usize,
    /// Bytes of lines about calls the policy refused that one session may
    /// write in a row. Beyond them such calls are counted and summed up in
    /// one line, until the session has earned room again.
    pub refused_burst_bytes: u64,
    /// Bytes of such lines a session earns back each minute, up to
    /// [`refused_burst_bytes`](Self::refused_burst_bytes).
    pub refused_bytes_per_minute: u64,
}

/// How long refused calls are counted before the log says how many it
/// left out.
pub const REFUSALS_WINDOW: Duration = Duration::from_secs(60);

/// The `tool` of a line the log writes about itself.
pub const REFUSALS_TOOL: &str = "audit:refusals";

impl Default for AuditLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 10 * 1024 * 1024,
            keep: 2,
            max_args_bytes: 16 * 1024,
            max_reason_bytes: 4 * 1024,
            max_field_bytes: 256,
            max_refused_bytes: 512,
            // A few hundred refused calls in a row are recorded in full
            // (an agent finding its way). Over the longest session, 8
            // hours, one session's refused calls then take 256 KiB and 32
            // KiB a minute, 16 MiB in all: less than the file and its
            // copies hold (30 MiB), so they alone do not rotate the other
            // lines away.
            refused_burst_bytes: 256 * 1024,
            refused_bytes_per_minute: 32 * 1024,
        }
    }
}

/// Appends [`AuditEntry`] lines to a file chosen by the caller.
///
/// The file and its rotated copies are readable by the owner only (`0600`,
/// on Windows a DACL with only the current user); the folder is created
/// `0700`. Every field is cut to a length (see [`AuditLimits`]), and the
/// file is rotated by size, so an agent in a loop can't fill the disk.
/// Calls the policy refused are recorded within a size each session may
/// write and counted beyond it, so a run of them does not rotate the other
/// lines away.
#[derive(Debug)]
pub struct AuditLog {
    path: PathBuf,
    limits: AuditLimits,
    state: Mutex<State>,
}

#[derive(Debug)]
struct State {
    file: File,
    /// What each session may still write about refused calls.
    refusals: HashMap<String, Refusals>,
}

/// The room one session has for lines about refused calls.
#[derive(Debug)]
struct Refusals {
    /// Bytes it may write now.
    room: u64,
    /// When `room` last grew.
    earned_at: Instant,
    /// Counted, not recorded, since `first_dropped`.
    dropped: u64,
    /// The first call that was not recorded, and when.
    first_dropped: Option<(Instant, Box<AuditEntry>)>,
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
            state: Mutex::new(State {
                file,
                refusals: HashMap::new(),
            }),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Writes one line. A full line is written with a single `write_all`
    /// under the lock, so concurrent calls never interleave.
    pub fn record(&self, entry: &AuditEntry) -> io::Result<()> {
        self.record_at(entry, Instant::now())
    }

    /// [`record`](Self::record) at `now`.
    fn record_at(&self, entry: &AuditEntry, now: Instant) -> io::Result<()> {
        let entry = self.limited(entry);
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("audit lock poisoned"))?;
        self.sum_up_refusals(&mut state, Some(now))?;
        if refused_by_policy(&entry) {
            let size = serde_json::to_vec(&entry).map_or(0, |line| line.len() as u64 + 1);
            let limits = &self.limits;
            let refusals = state
                .refusals
                .entry(entry.session.clone())
                .or_insert(Refusals {
                    room: limits.refused_burst_bytes,
                    earned_at: now,
                    dropped: 0,
                    first_dropped: None,
                });
            let waited = u64::try_from(now.duration_since(refusals.earned_at).as_millis())
                .unwrap_or(u64::MAX);
            let earned = waited.saturating_mul(limits.refused_bytes_per_minute) / 60_000;
            if earned > 0 {
                refusals.room = refusals
                    .room
                    .saturating_add(earned)
                    .min(limits.refused_burst_bytes);
                refusals.earned_at = now;
            }
            if size > refusals.room {
                refusals.dropped += 1;
                if refusals.first_dropped.is_some() {
                    return Ok(());
                }
                // Said at once: the sum comes with a later line, which a
                // process that ends now never writes.
                refusals.first_dropped = Some((now, Box::new(entry.clone())));
                let notice = self.refusals_line(
                    &entry,
                    json!({"firstTool": entry.tool}),
                    "this session's calls refused by the policy took the room the log keeps \
                     for them; further ones are counted, not recorded, until there is room \
                     again"
                        .into(),
                );
                return self.write(&mut state.file, &notice);
            }
            refusals.room -= size;
        }
        self.write(&mut state.file, &entry)
    }

    fn write(&self, file: &mut File, entry: &AuditEntry) -> io::Result<()> {
        let mut line = serde_json::to_vec(entry).map_err(io::Error::other)?;
        line.push(b'\n');
        self.rotate_if_full(file, line.len() as u64)?;
        file.write_all(&line)?;
        file.flush()
    }

    /// Writes how many refused calls each session left unrecorded, for
    /// those counting for [`REFUSALS_WINDOW`] at `now` (for all of them
    /// without a time), and forgets sessions with all their room back.
    fn sum_up_refusals(&self, state: &mut State, now: Option<Instant>) -> io::Result<()> {
        let limits = &self.limits;
        let mut sums = Vec::new();
        state.refusals.retain(|_, refusals| {
            let due = refusals.first_dropped.as_ref().is_some_and(|(since, _)| {
                now.is_none_or(|now| now.duration_since(*since) >= REFUSALS_WINDOW)
            });
            if due && let Some((_, first)) = refusals.first_dropped.take() {
                sums.push((std::mem::take(&mut refusals.dropped), first));
            }
            let Some(now) = now else {
                return false;
            };
            // Kept while it counts or has room to earn back.
            let idle = now.duration_since(refusals.earned_at);
            let full_after = limits
                .refused_burst_bytes
                .checked_div(limits.refused_bytes_per_minute)
                .map_or(Duration::MAX, |minutes| {
                    Duration::from_secs(minutes.saturating_add(1).saturating_mul(60))
                });
            refusals.first_dropped.is_some() || idle < full_after
        });
        for (dropped, first) in sums {
            let line = self.refusals_line(
                &first,
                json!({"notRecorded": dropped, "firstAt": first.time, "firstTool": first.tool}),
                format!("{dropped} calls of this session refused by the policy were not recorded"),
            );
            self.write(&mut state.file, &line)?;
        }
        Ok(())
    }

    /// A line of the log about the refused calls of the session of `like`.
    fn refusals_line(&self, like: &AuditEntry, args: Value, reason: String) -> AuditEntry {
        AuditEntry {
            time: Timestamp::now().to_string(),
            session: like.session.clone(),
            client: like.client.clone(),
            tool: REFUSALS_TOOL.into(),
            args,
            change: None,
            decided_by: DecidedBy::Policy,
            remembered: None,
            allowed: false,
            reason: Some(reason),
            identity: like.identity.clone(),
            result: "denied".into(),
            duration_ms: 0,
        }
    }

    /// `entry` with every field cut to the limits.
    fn limited(&self, entry: &AuditEntry) -> AuditEntry {
        let mut entry = entry.clone();
        let limits = &self.limits;
        let refused = refused_by_policy(&entry);
        let within = |max: usize| {
            if refused {
                max.min(limits.max_refused_bytes)
            } else {
                max
            }
        };
        cut_value(&mut entry.args, within(limits.max_args_bytes));
        if let Some(change) = &mut entry.change {
            cut_value(change, limits.max_args_bytes);
        }
        if let Some(reason) = &mut entry.reason {
            cut_text(reason, within(limits.max_reason_bytes));
        }
        for field in [
            &mut entry.time,
            &mut entry.session,
            &mut entry.client,
            &mut entry.tool,
            &mut entry.identity,
            &mut entry.result,
        ] {
            cut_text(field, limits.max_field_bytes);
        }
        if let Some(remembered) = &mut entry.remembered {
            cut_text(remembered, limits.max_field_bytes);
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

impl Drop for AuditLog {
    /// Refused calls counted and not summed up yet are, before the log
    /// goes.
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            let _ = self.sum_up_refusals(&mut state, None);
        }
    }
}

/// A call the policy refused: nothing ran, nobody was asked.
fn refused_by_policy(entry: &AuditEntry) -> bool {
    entry.decided_by == DecidedBy::Policy && !entry.allowed && entry.tool != REFUSALS_TOOL
}

/// Replaces `value` with a preview when its JSON is longer than `max`.
fn cut_value(value: &mut Value, max: usize) {
    let text = serde_json::to_string(value).unwrap_or_default();
    if text.len() > max {
        *value = json!({
            "truncated": true,
            "bytes": text.len(),
            "preview": truncate(&text, max),
        });
    }
}

/// Cuts `text` to `max` bytes and says so.
fn cut_text(text: &mut String, max: usize) {
    if text.len() > max {
        let cut = truncate(text, max).len();
        text.truncate(cut);
        text.push_str(" [truncated]");
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
    fn cuts_every_field_the_caller_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let limits = AuditLimits::default();
        let log = AuditLog::open_with(&path, limits).unwrap();
        let long = "ж".repeat(100_000);
        let mut entry = entry(&long);
        entry.client = long.clone();
        entry.session = long.clone();
        entry.identity = long.clone();
        entry.result = long.clone();
        entry.remembered = Some(long.clone());
        entry.reason = Some(long.clone());
        entry.args = json!({"name": long});
        entry.change = Some(json!({"keys": [long]}));
        // A call that ran, and one the policy refused.
        entry.decided_by = DecidedBy::None;
        entry.allowed = true;
        log.record(&entry).unwrap();
        entry.decided_by = DecidedBy::Policy;
        entry.allowed = false;
        log.record(&entry).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let sizes: Vec<usize> = text.lines().map(str::len).collect();
        let fields = 6 * (limits.max_field_bytes + 16);
        // A preview of JSON is escaped once more when it is written.
        let ran = 2 * (2 * limits.max_args_bytes + 64) + limits.max_reason_bytes + fields + 512;
        let refused =
            2 * (limits.max_refused_bytes + limits.max_args_bytes + 64) + fields + 1024 + 512;
        assert!(sizes[0] <= ran, "{} > {ran}", sizes[0]);
        assert!(sizes[1] <= refused, "{} > {refused}", sizes[1]);
        let lines = read_lines(&path);
        for line in &lines {
            for field in [
                "tool",
                "client",
                "session",
                "identity",
                "result",
                "remembered",
            ] {
                let value = line[field].as_str().unwrap();
                assert!(value.ends_with(" [truncated]"), "{field}: {value}");
                assert!(value.len() <= limits.max_field_bytes + 12, "{field}");
            }
            assert_eq!(line["change"]["truncated"], true);
        }
        let preview = |line: &Value| line["args"]["preview"].as_str().unwrap().len();
        assert!(preview(&lines[0]) > limits.max_refused_bytes);
        assert!(preview(&lines[1]) <= limits.max_refused_bytes);
        assert!(lines[1]["reason"].as_str().unwrap().len() <= limits.max_refused_bytes + 12);
    }

    /// A call of `session` the policy refused.
    fn refusal(session: &str, tool: &str) -> AuditEntry {
        AuditEntry {
            session: session.into(),
            ..entry(tool)
        }
    }

    /// Bytes of the line of `refusal("abc", "refused-0")`.
    fn refusal_len() -> u64 {
        serde_json::to_vec(&refusal("abc", "refused-0"))
            .unwrap()
            .len() as u64
            + 1
    }

    #[test]
    fn refused_calls_beyond_the_limit_are_counted_and_summed_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let limits = AuditLimits {
            refused_burst_bytes: 3 * refusal_len(),
            refused_bytes_per_minute: refusal_len(),
            ..AuditLimits::default()
        };
        let log = AuditLog::open_with(&path, limits).unwrap();
        let start = Instant::now();
        let mut ran = entry("get_resource");
        ran.decided_by = DecidedBy::None;
        ran.allowed = true;
        for n in 0..10 {
            log.record_at(&refusal("abc", &format!("refused-{n}")), start)
                .unwrap();
        }
        // Calls that ran, and another session, are recorded as before.
        log.record_at(&ran, start).unwrap();
        log.record_at(&refusal("other", "refused-other"), start)
            .unwrap();
        assert_eq!(
            tools(&path),
            [
                "refused-0",
                "refused-1",
                "refused-2",
                REFUSALS_TOOL,
                "get_resource",
                "refused-other"
            ]
        );
        let notice = &read_lines(&path)[3];
        assert_eq!(notice["session"], "abc");
        assert_eq!(notice["result"], "denied");
        assert_eq!(notice["args"]["firstTool"], "refused-3");
        assert!(notice["reason"].as_str().unwrap().contains("not recorded"));

        // The first line after the minute says how many were left out. The
        // session earned room for one line a minute.
        let later = start + REFUSALS_WINDOW;
        log.record_at(&refusal("abc", "refused-a"), later).unwrap();
        log.record_at(&refusal("abc", "refused-b"), later).unwrap();
        let lines = read_lines(&path);
        let sum = &lines[6];
        assert_eq!(sum["tool"], REFUSALS_TOOL);
        assert_eq!(sum["session"], "abc");
        assert_eq!(sum["args"]["notRecorded"], 7);
        assert_eq!(sum["args"]["firstTool"], "refused-3");
        assert_eq!(lines[7]["tool"], "refused-a");
        assert_eq!(lines[8]["tool"], REFUSALS_TOOL);
        assert_eq!(lines[8]["args"]["firstTool"], "refused-b");
        assert_eq!(lines.len(), 9);
    }

    #[test]
    fn a_run_of_refused_calls_fits_the_default_room() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let log = AuditLog::open(&path).unwrap();
        for n in 0..500 {
            log.record(&refusal("abc", &format!("refused-{n}")))
                .unwrap();
        }
        assert_eq!(read_lines(&path).len(), 500);
    }

    #[test]
    fn refused_calls_do_not_rotate_other_lines_away() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let limits = AuditLimits {
            // Room for a dozen lines in all.
            max_file_bytes: line_len() * 4,
            keep: 2,
            refused_burst_bytes: 2 * line_len(),
            refused_bytes_per_minute: 0,
            ..AuditLimits::default()
        };
        let log = AuditLog::open_with(&path, limits).unwrap();
        let mut ran = entry("applied");
        ran.decided_by = DecidedBy::User;
        ran.allowed = true;
        let start = Instant::now();
        log.record_at(&ran, start).unwrap();
        for n in 0..10_000 {
            log.record_at(&refusal("abc", &n.to_string()), start)
                .unwrap();
        }
        let kept: Vec<String> = [rotated(&path, 2), rotated(&path, 1), path.clone()]
            .iter()
            .filter(|path| path.exists())
            .flat_map(|path| tools(path))
            .collect();
        assert_eq!(kept, ["applied", "0", "1", REFUSALS_TOOL]);
    }

    #[test]
    fn a_closing_log_sums_up_what_it_counted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let limits = AuditLimits {
            refused_burst_bytes: line_len(),
            ..AuditLimits::default()
        };
        let log = AuditLog::open_with(&path, limits).unwrap();
        for n in 0..5 {
            log.record(&refusal("abc", &n.to_string())).unwrap();
        }
        drop(log);
        let lines = read_lines(&path);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[2]["tool"], REFUSALS_TOOL);
        assert_eq!(lines[2]["args"]["notRecorded"], 4);
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
