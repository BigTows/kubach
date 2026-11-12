//! Stand for the data side of `LogView` under a fast stream
//! (`docs/perf.md`): what `LogView::push` does to its buffer and its
//! search, without a window, and what a keystroke in the search field costs
//! on a full buffer. Laying out and painting the visible lines,
//! the list state and the selection are not measured.
//!
//! An ignored test that prints its figures; run it optimized:
//!
//! ```sh
//! cargo test -p kubach-ui --release --test perf_stand -- --ignored --nocapture
//! ```

use std::time::{Duration, Instant};

use kubach_ui::{DEFAULT_LOG_CAPACITY, LogBuffer, LogEntry, LogSearch};

/// Lines per second of the stream.
const RATE: usize = 5000;
/// Lines per `LogView::push`, as the log panel batches them.
const BATCH: usize = 256;
/// Seconds of stream: the buffer fills up and evicts for the second half.
const SECONDS: usize = 200;

fn line(ix: usize) -> String {
    let level = match ix % 10 {
        0 => "ERROR",
        1 | 2 => "WARN",
        _ => "INFO",
    };
    format!(
        "{level} request_id={:016x} method=GET path=/api/v1/items/{} status={} duration_ms={} \
         msg=\"handled request for tenant-{}\"",
        ix as u64 * 0x9e37_79b9,
        ix % 9973,
        if ix.is_multiple_of(10) { 500 } else { 200 },
        ix % 250,
        ix % 40,
    )
}

fn entry(ix: usize) -> LogEntry {
    LogEntry::new(line(ix))
        .timestamp(format!(
            "2026-10-01T08:{:02}:{:02}.{:03}Z",
            ix / RATE / 60 % 60,
            ix / RATE % 60,
            ix % RATE * 1000 / RATE
        ))
        .tag(format!("web-{}", ix % 3), ix % 3)
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.
}

/// Feeds `SECONDS` of stream; returns the time spent per second of stream
/// while the buffer fills and once it evicts, and the slowest batch.
fn feed(query: &str) -> (f64, f64, Duration, usize) {
    let mut buffer = LogBuffer::new(DEFAULT_LOG_CAPACITY);
    let mut search = LogSearch::default();
    search.set_query(query, &buffer);
    let total = RATE * SECONDS;
    let (mut filling, mut evicting) = (Duration::ZERO, Duration::ZERO);
    let mut slowest = Duration::ZERO;
    let mut next = 0;
    while next < total {
        let batch: Vec<LogEntry> = (next..(next + BATCH).min(total)).map(entry).collect();
        next += batch.len();
        let started = Instant::now();
        // The steps of `LogView::push`.
        let from = buffer.end_id();
        let pushed = buffer.push(batch);
        if pushed.evicted > 0 {
            search.evict(&buffer);
        }
        search.extend(&buffer, from);
        let taken = started.elapsed();
        slowest = slowest.max(taken);
        if pushed.evicted > 0 {
            evicting += taken;
        } else {
            filling += taken;
        }
    }
    let fill_seconds = (DEFAULT_LOG_CAPACITY / RATE).min(SECONDS) as f64;
    let evict_seconds = (SECONDS as f64 - fill_seconds).max(1.);
    (
        ms(filling) / fill_seconds,
        ms(evicting) / evict_seconds,
        slowest,
        search.len(),
    )
}

#[test]
#[ignore = "performance stand: prints timings, see docs/perf.md"]
fn perf_stand_log_stream() {
    println!(
        "\n== log buffer: {RATE} lines/s in batches of {BATCH}, capacity {DEFAULT_LOG_CAPACITY}, {SECONDS} s =="
    );
    let started = Instant::now();
    let built: usize = (0..RATE).map(|ix| entry(ix).text.len()).sum();
    println!(
        "building {RATE} entries: {:.2} ms ({} bytes of text)",
        ms(started.elapsed()),
        built
    );
    let mut full = LogBuffer::new(DEFAULT_LOG_CAPACITY);
    full.push((0..DEFAULT_LOG_CAPACITY).map(entry));
    for query in ["e", "error", "tenant-17", "no such text"] {
        let mut search = LogSearch::default();
        let started = Instant::now();
        search.set_query(query, &full);
        println!(
            "typing {query:14?} into the search of a full buffer: {:7.1} ms, {} matches",
            ms(started.elapsed()),
            search.len()
        );
    }
    for (title, query) in [
        ("no search", ""),
        ("search \"error\" (1 line in 10)", "error"),
        ("search \"status=\" (every line)", "status="),
    ] {
        let (filling, evicting, slowest, matches) = feed(query);
        println!(
            "{title:32} filling {filling:6.2} ms per second of stream, evicting {evicting:6.2} ms, \
             slowest batch {:.3} ms, {matches} matches kept",
            ms(slowest)
        );
    }
}
