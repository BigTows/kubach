//! Saving logs to a file (#214): the whole log of one or several containers
//! streamed from the API server, or a text that is already in memory.
//!
//! The file is written next to its target under a temporary name and renamed
//! onto the target only when everything is in it ([`PartialFile`]): a
//! download that failed or was cancelled (its task dropped) leaves neither a
//! cut file under the chosen name nor the temporary one.

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use futures::{AsyncRead, AsyncReadExt};
use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::jiff::Zoned;
use kube::Client;
use kube::api::{Api, LogParams};
use tokio::io::AsyncWriteExt;

use super::StreamError;
use super::logs::LogSource;
use crate::api_status;

/// Bytes read from the server per step.
const READ_CHUNK: usize = 64 * 1024;
/// Bytes collected before they go to the disk.
const WRITE_BUFFER: usize = 256 * 1024;

/// `<pod>_<container>_<time>.log`, the name offered for a saved log; without
/// `container` (every container of a pod, the pods of a workload) it is
/// `<pod>_<time>.log`. `at` is written as `20261128-101112`. Characters a
/// file name may not have (`Deployment/web`) become `-`.
pub fn log_file_name(pod: &str, container: Option<&str>, at: &Zoned) -> String {
    let part = |text: &str| -> String {
        text.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                    c
                } else {
                    '-'
                }
            })
            .collect()
    };
    let mut name = part(pod);
    if name.is_empty() {
        name.push_str("logs");
    }
    if let Some(container) = container.filter(|container| !container.is_empty()) {
        name.push('_');
        name.push_str(&part(container));
    }
    format!("{name}_{}.log", at.strftime("%Y%m%d-%H%M%S"))
}

/// [`log_file_name`] for the current moment in the system time zone.
pub fn log_file_name_now(pod: &str, container: Option<&str>) -> String {
    log_file_name(pod, container, &Zoned::now())
}

/// A file being written next to `target` under a temporary name.
/// [`commit`](Self::commit) renames it onto the target; dropped before that
/// (an error, a cancelled task) it is removed.
#[derive(Debug)]
pub struct PartialFile {
    temp: PathBuf,
    target: PathBuf,
    committed: bool,
}

impl PartialFile {
    /// Chooses the temporary name; the file itself is created by the caller
    /// at [`path`](Self::path).
    pub fn new(target: &Path) -> Self {
        let name = target
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let unique = uuid::Uuid::new_v4().simple().to_string();
        let temp = target.with_file_name(format!("{name}.{}.part", &unique[..8]));
        Self {
            temp,
            target: target.to_owned(),
            committed: false,
        }
    }

    /// Where to write.
    pub fn path(&self) -> &Path {
        &self.temp
    }

    /// The file is complete: it takes the place of the target.
    pub fn commit(mut self) -> io::Result<()> {
        std::fs::rename(&self.temp, &self.target)?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for PartialFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.temp);
        }
    }
}

/// Writes `text` to `target` through a [`PartialFile`]. Blocking.
pub fn save_log_text(target: &Path, text: &str) -> io::Result<()> {
    use std::io::Write as _;
    let partial = PartialFile::new(target);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(partial.path())?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    drop(file);
    partial.commit()
}

/// What a download asks the server for. Always the whole log: no tail, no
/// byte limit, no follow.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LogDownloadOptions {
    /// Logs of the previous (terminated) container instance.
    pub previous: bool,
    /// Every line starts with the time the server recorded for it.
    pub timestamps: bool,
}

impl LogDownloadOptions {
    /// Request parameters for `container`.
    pub fn to_params(&self, container: &str) -> LogParams {
        LogParams {
            container: Some(container.to_owned()),
            follow: false,
            limit_bytes: None,
            pretty: false,
            previous: self.previous,
            since_seconds: None,
            since_time: None,
            tail_lines: None,
            timestamps: self.timestamps,
        }
    }
}

/// Why the log of a source could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceFailure {
    /// HTTP 403 or the reason `Forbidden`.
    Forbidden,
    /// HTTP 404 or the reason `NotFound`: the pod is gone.
    NotFound,
    /// Anything else, in the words of the server or of the connection.
    Other(String),
}

impl std::fmt::Display for SourceFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceFailure::Forbidden => f.write_str("forbidden"),
            SourceFailure::NotFound => f.write_str("not found"),
            SourceFailure::Other(message) => f.write_str(message),
        }
    }
}

impl From<&StreamError> for SourceFailure {
    fn from(error: &StreamError) -> Self {
        match error {
            StreamError::Api(error) => match &**error {
                kube::Error::Api(status) if api_status::is_forbidden(status) => Self::Forbidden,
                kube::Error::Api(status) if api_status::is_not_found(status) => Self::NotFound,
                kube::Error::Api(status) if !status.message.is_empty() => {
                    Self::Other(status.message.clone())
                }
                other => Self::Other(other.to_string()),
            },
            other => Self::Other(other.to_string()),
        }
    }
}

/// A download that saved nothing.
#[derive(Debug, thiserror::Error)]
pub enum LogDownloadError {
    /// The log of `log` could not be read (the only source, every source,
    /// or a connection that broke in the middle of a log).
    #[error("{}: {reason}", log.label())]
    Source {
        log: LogSource,
        reason: SourceFailure,
    },
    /// The file could not be created, written or renamed.
    #[error("{0}")]
    File(#[source] io::Error),
}

/// A finished download.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogDownloadReport {
    /// Bytes in the file.
    pub bytes: u64,
    /// Sources the server refused to give the log of (a container that has
    /// not started, no previous instance, a deleted pod) while others were
    /// saved: the file has nothing of them.
    pub skipped: Vec<(LogSource, SourceFailure)>,
}

/// Saves the whole log of `sources` to `target`, one source after another,
/// without keeping it in memory. `written` follows the bytes written so far.
///
/// With several sources every line starts with the [`LogSource::label`] of
/// its source (`pod/container `), as the lines of a merged stream are
/// tagged; the bytes of a single source are written as the server sent them.
/// A source the server refuses is skipped and reported when another one was
/// saved; when none was, or when a log breaks off in the middle, the
/// download fails and nothing is left on the disk. Dropping the future
/// (cancel) removes the unfinished file as well.
///
/// Must run on the [`runtime`](crate::runtime).
pub async fn download_logs(
    client: &Client,
    sources: Vec<LogSource>,
    options: LogDownloadOptions,
    target: &Path,
    written: &AtomicU64,
) -> Result<LogDownloadReport, LogDownloadError> {
    download_with(&sources, target, written, |source| {
        let client = client.clone();
        async move {
            let api: Api<Pod> = Api::namespaced(client, &source.namespace);
            api.log_stream(&source.pod, &options.to_params(&source.container))
                .await
                .map_err(StreamError::from)
        }
    })
    .await
}

/// [`download_logs`] over anything that opens the log of a source.
async fn download_with<R, F, Fut>(
    sources: &[LogSource],
    target: &Path,
    written: &AtomicU64,
    open: F,
) -> Result<LogDownloadReport, LogDownloadError>
where
    R: AsyncRead + Unpin,
    F: Fn(LogSource) -> Fut,
    Fut: Future<Output = Result<R, StreamError>>,
{
    // Declared before the file: dropped after it, so the file is closed
    // when an unfinished download is removed.
    let partial = PartialFile::new(target);
    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(partial.path())
        .await
        .map_err(LogDownloadError::File)?;
    let mut out = tokio::io::BufWriter::with_capacity(WRITE_BUFFER, file);
    let tagged = sources.len() > 1;
    let mut report = LogDownloadReport::default();
    let mut buf = vec![0u8; READ_CHUNK];
    let mut prefixed = Vec::new();
    for source in sources {
        let mut reader = match open(source.clone()).await {
            Ok(reader) => reader,
            Err(error) => {
                report
                    .skipped
                    .push((source.clone(), SourceFailure::from(&error)));
                continue;
            }
        };
        let mut prefixer = tagged.then(|| LinePrefixer::new(format!("{} ", source.label())));
        loop {
            let n = reader
                .read(&mut buf)
                .await
                .map_err(|error| LogDownloadError::Source {
                    log: source.clone(),
                    reason: SourceFailure::Other(error.to_string()),
                })?;
            if n == 0 {
                break;
            }
            let chunk = match &mut prefixer {
                Some(prefixer) => {
                    prefixed.clear();
                    prefixer.push(&buf[..n], &mut prefixed);
                    &prefixed[..]
                }
                None => &buf[..n],
            };
            out.write_all(chunk).await.map_err(LogDownloadError::File)?;
            report.bytes += chunk.len() as u64;
            written.store(report.bytes, Ordering::Relaxed);
        }
        // The next source starts on its own line.
        if prefixer.is_some_and(|prefixer| !prefixer.at_line_start) {
            out.write_all(b"\n").await.map_err(LogDownloadError::File)?;
            report.bytes += 1;
            written.store(report.bytes, Ordering::Relaxed);
        }
    }
    if report.skipped.len() == sources.len()
        && let Some((log, reason)) = report.skipped.first().cloned()
    {
        return Err(LogDownloadError::Source { log, reason });
    }
    out.flush().await.map_err(LogDownloadError::File)?;
    let file = out.into_inner();
    file.sync_all().await.map_err(LogDownloadError::File)?;
    drop(file);
    partial.commit().map_err(LogDownloadError::File)?;
    Ok(report)
}

/// Puts a prefix before every line of a byte stream cut into chunks
/// anywhere, without decoding or buffering lines.
#[derive(Debug)]
struct LinePrefixer {
    prefix: Vec<u8>,
    at_line_start: bool,
}

impl LinePrefixer {
    fn new(prefix: String) -> Self {
        Self {
            prefix: prefix.into_bytes(),
            at_line_start: true,
        }
    }

    /// Appends `chunk` to `out` with the prefix before each line that
    /// starts in it.
    fn push(&mut self, chunk: &[u8], out: &mut Vec<u8>) {
        for piece in chunk.split_inclusive(|&byte| byte == b'\n') {
            if self.at_line_start {
                out.extend_from_slice(&self.prefix);
            }
            out.extend_from_slice(piece);
            self.at_line_start = piece.ends_with(b"\n");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::{Context, Poll};

    use k8s_openapi::jiff::Timestamp;
    use k8s_openapi::jiff::tz::TimeZone;
    use kube::core::Status;

    use super::*;

    /// A log that arrives in the given steps; after them it ends, or never
    /// answers again when `hang` is set.
    struct Script {
        steps: VecDeque<io::Result<Vec<u8>>>,
        hang: bool,
    }

    impl Script {
        fn new(steps: Vec<io::Result<&str>>) -> Self {
            Self {
                steps: steps
                    .into_iter()
                    .map(|step| step.map(|text| text.as_bytes().to_vec()))
                    .collect(),
                hang: false,
            }
        }

        fn text(text: &str) -> Self {
            Self::new(vec![Ok(text)])
        }
    }

    impl AsyncRead for Script {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<io::Result<usize>> {
            match self.steps.pop_front() {
                Some(Ok(bytes)) => {
                    buf[..bytes.len()].copy_from_slice(&bytes);
                    Poll::Ready(Ok(bytes.len()))
                }
                Some(Err(error)) => Poll::Ready(Err(error)),
                None if self.hang => Poll::Pending,
                None => Poll::Ready(Ok(0)),
            }
        }
    }

    fn source(container: &str) -> LogSource {
        LogSource::new("ns", "web-0", container)
    }

    fn api_error(code: u16, reason: &str, message: &str) -> StreamError {
        StreamError::from(kube::Error::Api(
            Status {
                code,
                reason: reason.to_owned(),
                message: message.to_owned(),
                ..Status::default()
            }
            .into(),
        ))
    }

    /// Names of the files in `dir`.
    fn files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn names_the_file_after_pod_container_and_time() {
        let at = "2026-11-28T10:11:12Z"
            .parse::<Timestamp>()
            .unwrap()
            .to_zoned(TimeZone::UTC);
        assert_eq!(
            log_file_name("web-0", Some("app"), &at),
            "web-0_app_20261128-101112.log"
        );
        assert_eq!(
            log_file_name("web-0", None, &at),
            "web-0_20261128-101112.log"
        );
        assert_eq!(
            log_file_name("Deployment/web", Some(""), &at),
            "Deployment-web_20261128-101112.log"
        );
        assert_eq!(
            log_file_name("", Some("a b"), &at),
            "logs_a-b_20261128-101112.log"
        );
    }

    #[test]
    fn asks_for_the_whole_log() {
        let options = LogDownloadOptions {
            previous: true,
            timestamps: true,
        };
        let params = options.to_params("app");
        assert_eq!(params.container.as_deref(), Some("app"));
        assert!(params.previous && params.timestamps && !params.follow);
        assert_eq!(params.tail_lines, None);
        assert_eq!(params.limit_bytes, None);
        assert_eq!((params.since_seconds, params.since_time), (None, None));
        assert!(!LogDownloadOptions::default().to_params("app").timestamps);
    }

    #[test]
    fn prefixes_lines_cut_anywhere() {
        let mut prefixer = LinePrefixer::new("p/c ".to_owned());
        let mut out = Vec::new();
        for chunk in ["one\ntw", "o\n", "\nthr", "ee"] {
            prefixer.push(chunk.as_bytes(), &mut out);
        }
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "p/c one\np/c two\np/c \np/c three"
        );
        assert!(!prefixer.at_line_start);
    }

    #[test]
    fn api_errors_are_read_by_code_and_reason() {
        let failure = |error: StreamError| SourceFailure::from(&error);
        assert_eq!(failure(api_error(403, "", "no")), SourceFailure::Forbidden);
        assert_eq!(
            failure(api_error(0, "Forbidden", "no")),
            SourceFailure::Forbidden
        );
        assert_eq!(failure(api_error(404, "", "gone")), SourceFailure::NotFound);
        assert_eq!(
            failure(api_error(400, "BadRequest", "container is waiting")),
            SourceFailure::Other("container is waiting".to_owned())
        );
        assert_eq!(
            failure(StreamError::Timeout("slow".to_owned())),
            SourceFailure::Other("timed out: slow".to_owned())
        );
    }

    #[test]
    fn text_is_saved_through_a_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.log");
        std::fs::write(&target, "old").unwrap();
        save_log_text(&target, "one\ntwo\n").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "one\ntwo\n");
        assert_eq!(files(dir.path()), ["out.log"]);
        // A folder that is not there: an error and nothing left behind.
        let missing = dir.path().join("missing").join("out.log");
        assert!(save_log_text(&missing, "text").is_err());
        assert_eq!(files(dir.path()), ["out.log"]);
    }

    #[test]
    fn an_abandoned_partial_file_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.log");
        let partial = PartialFile::new(&target);
        std::fs::write(partial.path(), "half").unwrap();
        assert_eq!(partial.path().parent(), target.parent());
        assert_ne!(partial.path(), target);
        drop(partial);
        assert!(files(dir.path()).is_empty());
    }

    #[tokio::test]
    async fn a_single_source_is_saved_as_sent() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.log");
        let written = AtomicU64::new(0);
        let report = download_with(&[source("app")], &target, &written, |_| async {
            Ok(Script::new(vec![
                Ok("one\r\ntw"),
                Ok("o\n\x1b[31mred\x1b[0m"),
            ]))
        })
        .await
        .unwrap();
        let text = "one\r\ntwo\n\x1b[31mred\x1b[0m";
        assert_eq!(std::fs::read_to_string(&target).unwrap(), text);
        assert_eq!(report.bytes, text.len() as u64);
        assert_eq!(written.load(Ordering::Relaxed), report.bytes);
        assert!(report.skipped.is_empty());
        assert_eq!(files(dir.path()), ["out.log"]);
    }

    #[tokio::test]
    async fn several_sources_are_tagged_line_by_line() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.log");
        let written = AtomicU64::new(0);
        let sources = [source("a"), source("b"), source("c")];
        let report = download_with(&sources, &target, &written, |source| async move {
            match source.container.as_str() {
                // No newline at the end: the next source still starts a line.
                "a" => Ok(Script::new(vec![Ok("a1\na"), Ok("2")])),
                "b" => Err(api_error(400, "BadRequest", "container is waiting")),
                _ => Ok(Script::text("c1\n")),
            }
        })
        .await
        .unwrap();
        let text = "web-0/a a1\nweb-0/a a2\nweb-0/c c1\n";
        assert_eq!(std::fs::read_to_string(&target).unwrap(), text);
        assert_eq!(report.bytes, text.len() as u64);
        assert_eq!(
            report.skipped,
            [(
                source("b"),
                SourceFailure::Other("container is waiting".to_owned())
            )]
        );
    }

    #[tokio::test]
    async fn nothing_is_saved_when_no_source_answers() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.log");
        std::fs::write(&target, "old").unwrap();
        let written = AtomicU64::new(0);
        let sources = [source("a"), source("b")];
        let error = download_with(&sources, &target, &written, |_| async {
            Err::<Script, _>(api_error(403, "Forbidden", "no"))
        })
        .await
        .unwrap_err();
        assert!(matches!(
            &error,
            LogDownloadError::Source { log, reason: SourceFailure::Forbidden } if *log == source("a")
        ));
        assert_eq!(error.to_string(), "web-0/a: forbidden");
        // What was under the name before is untouched.
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "old");
        assert_eq!(files(dir.path()), ["out.log"]);
    }

    #[tokio::test]
    async fn a_log_that_breaks_off_is_not_saved() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.log");
        let written = AtomicU64::new(0);
        let error = download_with(&[source("app")], &target, &written, |_| async {
            Ok(Script::new(vec![
                Ok("one\n"),
                Err(io::Error::new(io::ErrorKind::ConnectionReset, "reset")),
            ]))
        })
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            LogDownloadError::Source {
                reason: SourceFailure::Other(_),
                ..
            }
        ));
        assert!(files(dir.path()).is_empty());
    }

    #[tokio::test]
    async fn a_file_that_cannot_be_created_is_a_file_error() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("missing").join("out.log");
        let written = AtomicU64::new(0);
        let error = download_with(&[source("app")], &target, &written, |_| async {
            Ok(Script::text("one\n"))
        })
        .await
        .unwrap_err();
        assert!(matches!(error, LogDownloadError::File(_)));
    }

    #[tokio::test]
    async fn a_cancelled_download_leaves_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.log");
        let written = Arc::new(AtomicU64::new(0));
        let task = tokio::spawn({
            let (target, written) = (target.clone(), written.clone());
            async move {
                download_with(&[source("app")], &target, &written, |_| async {
                    let mut script = Script::text("one\n");
                    script.hang = true;
                    Ok(script)
                })
                .await
            }
        });
        // The first lines are on their way to the temporary file.
        while written.load(Ordering::Relaxed) == 0 {
            tokio::task::yield_now().await;
        }
        assert_eq!(files(dir.path()).len(), 1);
        assert!(!target.exists());
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(files(dir.path()).is_empty());
    }
}
