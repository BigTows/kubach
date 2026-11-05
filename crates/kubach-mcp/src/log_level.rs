//! The level of a log line, for `minLevel` of `get_logs` (#117).
//!
//! Recognizes logfmt and slog text (`level=error`, `lvl=eror`, `level=ERROR+2`),
//! JSON objects (`level`, `severity`, `lvl` and the like, by name or as a
//! pino/bunyan number) and klog (`E1103 12:34:56.789012 ...`). Anything else
//! has no level: a line is never guessed from words in its message, so
//! `connection error` does not count as an error. Continuation lines of a
//! stack trace have no level of their own either.
//!
//! Lines come here masked, and masking can break JSON (`"token_count":5`
//! becomes `"token_count":***`), so a line that starts as an object but does
//! not parse is scanned for its level field instead.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

/// Field names that hold the level, in any case.
const LEVEL_KEYS: [&str; 6] = [
    "level",
    "lvl",
    "severity",
    "loglevel",
    "log_level",
    "log.level",
];

/// `"key":"value"` and `"key":30` in a line that isn't valid JSON.
static JSON_FIELD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""([A-Za-z_.]+)"\s*:\s*(?:"([^"]*)"|(\d+))"#).unwrap());

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

impl Level {
    pub(crate) const NAMES: &str = "trace, debug, info, warn, error, fatal";

    /// A level name as loggers write it, in any case. slog's offsets
    /// (`ERROR+2`, `INFO-1`) count as the base level.
    pub(crate) fn parse(name: &str) -> Option<Self> {
        let name = name
            .trim_end_matches([',', ';'])
            .trim_end_matches(|c: char| c.is_ascii_digit())
            .trim_end_matches(['+', '-'])
            .to_ascii_lowercase();
        Some(match name.as_str() {
            "trace" | "trc" | "verbose" => Self::Trace,
            "debug" | "dbg" | "dbug" => Self::Debug,
            "info" | "inf" | "information" | "informational" | "notice" => Self::Info,
            "warn" | "wrn" | "warning" => Self::Warn,
            "error" | "err" | "eror" => Self::Error,
            "fatal" | "ftl" | "critical" | "crit" | "panic" | "dpanic" | "alert" | "emerg"
            | "emergency" => Self::Fatal,
            _ => return None,
        })
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Fatal => "fatal",
        }
    }

    /// pino and bunyan: 10 trace, 20 debug, ... 60 fatal; custom levels sit
    /// between them. Small numbers (syslog's, reversed) are not these.
    fn from_number(number: u64) -> Option<Self> {
        Some(match number / 10 {
            1 => Self::Trace,
            2 => Self::Debug,
            3 => Self::Info,
            4 => Self::Warn,
            5 => Self::Error,
            6 => Self::Fatal,
            _ => return None,
        })
    }
}

/// The level of `line`, `None` when no known format gives one.
pub(crate) fn line_level(line: &str) -> Option<Level> {
    let line = line.trim_start();
    if line.starts_with('{') {
        match serde_json::from_str::<Map<String, Value>>(line) {
            // A parsed object has the last word: `level=error` inside its
            // message is not its level.
            Ok(fields) => return json_level(&fields),
            Err(_) => {
                if let Some(level) = broken_json_level(line) {
                    return Some(level);
                }
            }
        }
    }
    klog_level(line).or_else(|| logfmt_level(line))
}

fn is_level_key(key: &str) -> bool {
    LEVEL_KEYS.iter().any(|k| k.eq_ignore_ascii_case(key))
}

fn json_level(fields: &Map<String, Value>) -> Option<Level> {
    fields
        .iter()
        .filter(|(key, _)| is_level_key(key))
        .find_map(|(_, value)| match value {
            Value::String(name) => Level::parse(name),
            Value::Number(number) => number.as_u64().and_then(Level::from_number),
            _ => None,
        })
}

fn broken_json_level(line: &str) -> Option<Level> {
    JSON_FIELD
        .captures_iter(line)
        .filter(|field| is_level_key(&field[1]))
        .find_map(|field| match (field.get(2), field.get(3)) {
            (Some(name), _) => Level::parse(name.as_str()),
            (_, Some(number)) => number.as_str().parse().ok().and_then(Level::from_number),
            _ => None,
        })
}

/// `Lmmdd hh:mm:ss.uuuuuu threadid file:line] msg`, with L one of I, W, E, F.
fn klog_level(line: &str) -> Option<Level> {
    const SHAPE: &[u8] = b"0000 00:00:00";
    let bytes = line.as_bytes();
    let level = match bytes.first()? {
        b'I' => Level::Info,
        b'W' => Level::Warn,
        b'E' => Level::Error,
        b'F' => Level::Fatal,
        _ => return None,
    };
    let header = bytes.get(1..=SHAPE.len())?;
    header
        .iter()
        .zip(SHAPE)
        .all(|(byte, shape)| match shape {
            b'0' => byte.is_ascii_digit(),
            _ => byte == shape,
        })
        .then_some(level)
}

/// The first `level=...` pair of `line`. Quoted values are skipped whole, so
/// `msg="level=error"` is no level. Words without `=` are passed over: a
/// timestamp or a prefix may come first.
fn logfmt_level(line: &str) -> Option<Level> {
    let mut rest = line;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            return None;
        }
        let end = rest.find([' ', '\t', '=']).unwrap_or(rest.len());
        let (key, tail) = rest.split_at(end);
        let Some(tail) = tail.strip_prefix('=') else {
            rest = tail;
            continue;
        };
        let (value, tail) = logfmt_value(tail);
        if is_level_key(key)
            && let Some(level) = Level::parse(value)
        {
            return Some(level);
        }
        rest = tail;
    }
}

/// Splits a value off the start of `text`: the value and what follows it.
fn logfmt_value(text: &str) -> (&str, &str) {
    let Some(quoted) = text.strip_prefix('"') else {
        return text.split_at(text.find([' ', '\t']).unwrap_or(text.len()));
    };
    let mut escaped = false;
    for (index, c) in quoted.char_indices() {
        match c {
            '"' if !escaped => return (&quoted[..index], &quoted[index + 1..]),
            '\\' if !escaped => escaped = true,
            _ => escaped = false,
        }
    }
    (quoted, "")
}

/// What `minLevel` saw across the streams of one call.
#[derive(Default)]
pub(crate) struct LevelCounts {
    read: usize,
    recognized: usize,
}

impl LevelCounts {
    /// Counts `line` and says whether it is at `min` or above. A line
    /// without a level is not.
    pub(crate) fn admit(&mut self, line: &str, min: Level) -> bool {
        self.read += 1;
        let level = line_level(line);
        self.recognized += usize::from(level.is_some());
        level.is_some_and(|level| level >= min)
    }

    /// The note that gives an empty result its meaning. `matched` is what
    /// came through, the text filter included.
    pub(crate) fn note(&self, min: Level, matched: usize, text_filter: bool) -> String {
        let Self { read, recognized } = *self;
        let unrecognized = read - recognized;
        let also = if text_filter { " and the filter" } else { "" };
        let mut note = format!(
            "(minLevel {}: {read} read, {recognized} with a recognized level, {matched} matched{also}",
            min.name()
        );
        if read > 0 && recognized == 0 {
            note.push_str(
                "; no line is in logfmt/slog, JSON or klog format, so this says nothing about \
                 their levels: read them without minLevel, or with filter",
            );
        } else if unrecognized > 0 {
            note.push_str(&format!(
                "; {unrecognized} without a recognized level were excluded"
            ));
        }
        note.push(')');
        note
    }
}

#[cfg(test)]
mod tests {
    use super::Level::{Debug, Error, Fatal, Info, Trace, Warn};
    use super::*;
    use crate::masking::mask_text;

    #[test]
    fn logfmt_and_slog() {
        for (line, level) in [
            ("level=error msg=boom", Error),
            ("level=ERROR msg=boom", Error),
            (
                r#"time="2026-11-05T10:00:00Z" level=warning msg="slow query""#,
                Warn,
            ),
            (
                "time=2026-11-05T10:00:00Z level=INFO msg=started port=80",
                Info,
            ),
            (
                "ts=2026-11-05T10:00:00Z caller=main.go:1 level=debug msg=x",
                Debug,
            ),
            ("t=2026-11-05T10:00:00Z lvl=eror msg=x", Error),
            ("t=2026-11-05T10:00:00Z lvl=crit msg=x", Fatal),
            (r#"level="error" msg=x"#, Error),
            // slog's offsets from a base level.
            ("level=ERROR+2 msg=x", Error),
            ("level=INFO-1 msg=x", Info),
            // After a plain prefix, and with a comma after the value.
            ("2026-11-05T10:00:00Z level=warn msg=x", Warn),
            ("time=now, level=error, msg=x", Error),
            ("severity=fatal msg=x", Fatal),
            ("log.level=trace msg=x", Trace),
        ] {
            assert_eq!(line_level(line), Some(level), "{line}");
        }
    }

    #[test]
    fn logfmt_values_are_not_keys() {
        // The real level comes after a quoted one, or there is none.
        assert_eq!(
            line_level(r#"msg="retry level=error failed" level=info"#),
            Some(Info)
        );
        assert_eq!(line_level(r#"msg="say \"level=error\"" port=1"#), None);
        assert_eq!(line_level("msg=level=error"), None);
        // A value that is no level doesn't stop the scan.
        assert_eq!(line_level("severity=high level=warn"), Some(Warn));
        assert_eq!(line_level("mylevel=error"), None);
        assert_eq!(line_level(r#"level="error"#), Some(Error));
    }

    #[test]
    fn json() {
        for (line, level) in [
            (r#"{"level":"error","msg":"boom"}"#, Error),
            (r#"{"msg":"boom","level":"ERROR"}"#, Error),
            (r#"{"severity":"WARNING","message":"x"}"#, Warn),
            (r#"{"severity":"CRITICAL","message":"x"}"#, Fatal),
            (r#"{"lvl":"info"}"#, Info),
            (r#"{"Level":"Warning"}"#, Warn),
            (r#"{"log.level":"debug"}"#, Debug),
            (r#"  {"level":"dpanic"}"#, Fatal),
            // slog's JSON handler.
            (r#"{"time":"t","level":"ERROR+2","msg":"x"}"#, Error),
            // pino and bunyan.
            (r#"{"level":10,"msg":"x"}"#, Trace),
            (r#"{"level":20,"msg":"x"}"#, Debug),
            (r#"{"level":30,"msg":"x"}"#, Info),
            (r#"{"level":40,"msg":"x"}"#, Warn),
            (r#"{"level":50,"msg":"x"}"#, Error),
            (r#"{"level":60,"msg":"x"}"#, Fatal),
            (r#"{"level":35,"msg":"custom"}"#, Info),
        ] {
            assert_eq!(line_level(line), Some(level), "{line}");
        }
    }

    #[test]
    fn json_without_a_level_has_none() {
        for line in [
            r#"{"msg":"boom"}"#,
            r#"{"msg":"level=error"}"#,
            r#"{"msg":"\"level\":\"error\""}"#,
            r#"{"nested":{"level":"error"}}"#,
            r#"{"level":"loud"}"#,
            r#"{"level":3}"#,
            r#"{"level":70}"#,
            r#"{"level":true}"#,
            r#"{"level":-40}"#,
            r#"{"level":40.5}"#,
        ] {
            assert_eq!(line_level(line), None, "{line}");
        }
    }

    #[test]
    fn json_broken_by_masking_keeps_its_level() {
        for (line, level) in [
            (r#"{"level":"error","token_count":5,"msg":"x"}"#, Error),
            (r#"{"token_count":5,"level":50}"#, Error),
            (r#"{"credentials":{"a":1},"severity":"WARNING"}"#, Warn),
        ] {
            let masked = mask_text(line);
            assert!(
                serde_json::from_str::<Value>(&masked).is_err(),
                "still JSON: {masked}"
            );
            assert_eq!(line_level(&masked), Some(level), "{masked}");
        }
        // Cut off by the size limit.
        assert_eq!(line_level(r#"{"level":"warn","msg":"cut o"#), Some(Warn));
        // A brace that opens no JSON at all.
        assert_eq!(line_level("{app} level=error msg=x"), Some(Error));
        assert_eq!(line_level(r#"{"msg":"\"level\":\"error\"", cut"#), None);
    }

    #[test]
    fn klog() {
        for (line, level) in [
            ("I1103 12:34:56.789012       1 main.go:42] started", Info),
            ("W1103 12:34:56.789012       1 main.go:42] slow", Warn),
            ("E1103 12:34:56.789012       1 main.go:42] failed", Error),
            ("F1103 12:34:56.789012       1 main.go:42] died", Fatal),
            // The klog header decides, whatever structured values follow.
            (
                r#"I1103 12:34:56.789012 1 a.go:1] "Done" level=error"#,
                Info,
            ),
        ] {
            assert_eq!(line_level(line), Some(level), "{line}");
        }
        for line in [
            "E1103",
            "E1103 failed",
            "D1103 12:34:56.789012 1 a.go:1] x",
            "Error 12:34:56.789012",
            "E110 12:34:56.789012",
            "Ё1103 12:34:56",
        ] {
            assert_eq!(line_level(line), None, "{line}");
        }
    }

    #[test]
    fn free_text_is_not_guessed_from() {
        for line in [
            "",
            "   ",
            "connection error: refused",
            "ERROR db failed",
            "2026/11/05 10:00:00 [error] 1#1: upstream timed out",
            "panic: runtime error",
        ] {
            assert_eq!(line_level(line), None, "{line}");
        }
    }

    /// `get_logs` goes line by line and has no notion of a multi-line
    /// record: what follows the error line has no level.
    #[test]
    fn stack_trace_lines_have_no_level() {
        let mut counts = LevelCounts::default();
        let log = "level=error msg=boom\n\
                   goroutine 1 [running]:\n\
                   main.main()\n\
                   \t/app/main.go:42 +0x1d\n\
                   \tat com.example.Main.run(Main.java:42)\n\
                   Caused by: java.io.IOException: closed\n";
        let admitted: Vec<&str> = log
            .lines()
            .filter(|line| counts.admit(line, Level::Warn))
            .collect();
        assert_eq!(admitted, ["level=error msg=boom"]);
        assert_eq!((counts.read, counts.recognized), (6, 1));
    }

    #[test]
    fn levels_are_ordered_and_named() {
        assert!(Trace < Debug && Debug < Info && Info < Warn && Warn < Error && Error < Fatal);
        for level in [Trace, Debug, Info, Warn, Error, Fatal] {
            assert_eq!(Level::parse(level.name()), Some(level));
            assert!(Level::NAMES.contains(level.name()));
        }
        assert_eq!(Level::parse("WARNING"), Some(Warn));
        assert_eq!(Level::parse("loud"), None);
        assert_eq!(Level::parse(""), None);
        assert_eq!(Level::parse("5"), None);
    }

    #[test]
    fn counts_say_what_an_empty_result_means() {
        let count = |lines: &[&str]| {
            let mut counts = LevelCounts::default();
            let matched = lines.iter().filter(|l| counts.admit(l, Warn)).count();
            (counts, matched)
        };

        // Recognized, and nothing that high: really no warnings.
        let (counts, matched) = count(&["level=info msg=a", "level=debug msg=b"]);
        assert_eq!(
            counts.note(Warn, matched, false),
            "(minLevel warn: 2 read, 2 with a recognized level, 0 matched)"
        );

        // Partly recognized.
        let (counts, matched) = count(&["level=error msg=a", "level=info msg=b", "plain"]);
        assert_eq!(
            counts.note(Warn, matched, false),
            "(minLevel warn: 3 read, 2 with a recognized level, 1 matched; \
             1 without a recognized level were excluded)"
        );

        // Nothing recognized: the empty result says nothing.
        let (counts, matched) = count(&["plain", "ERROR db failed"]);
        let note = counts.note(Warn, matched, false);
        assert!(note.starts_with("(minLevel warn: 2 read, 0 with"), "{note}");
        assert!(note.contains("says nothing about their levels"), "{note}");

        // Nothing to read: no format to blame.
        let (counts, matched) = count(&[]);
        assert_eq!(
            counts.note(Warn, matched, false),
            "(minLevel warn: 0 read, 0 with a recognized level, 0 matched)"
        );

        // With a text filter, matched covers both.
        let (counts, _) = count(&["level=error msg=a"]);
        assert_eq!(
            counts.note(Warn, 0, true),
            "(minLevel warn: 1 read, 1 with a recognized level, 0 matched and the filter)"
        );
    }
}
