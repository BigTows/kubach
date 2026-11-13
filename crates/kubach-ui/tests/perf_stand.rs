//! Stand for the data side of `LogView` under a fast stream
//! (`docs/perf.md`): what `LogView::push` does to its buffer and its
//! search, without a window, what a keystroke in the search field costs
//! on a full buffer (the search runs in pieces, as the view runs it, while
//! lines keep arriving) and what a buffered line takes in memory. Laying
//! out and painting the visible lines, the list state and the selection
//! are not measured.
//!
//! An ignored test that prints its figures; run it optimized:
//!
//! ```sh
//! cargo test -p kubach-ui --release --test perf_stand -- --ignored --nocapture
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::SharedString;
use kubach_ui::{DEFAULT_LOG_BYTES, DEFAULT_LOG_CAPACITY, LogBuffer, LogEntry, LogSearch};

/// Counts the bytes the test holds on the heap (as requested from the
/// allocator, without its rounding), to tell what a buffered line costs.
struct Counting;

static HELD: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call goes to the system allocator unchanged; only sizes are
// counted.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        HELD.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        HELD.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        HELD.fetch_add(new_size, Ordering::Relaxed);
        HELD.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn held() -> usize {
    HELD.load(Ordering::Relaxed)
}

/// Lines per second of the stream.
const RATE: usize = 5000;
/// Lines per `LogView::push`, as the log panel batches them.
const BATCH: usize = 256;
/// Seconds of stream: the buffer fills up and evicts for the second half.
const SECONDS: usize = 200;
/// What `LogView` searches between two looks at the clock, and for how long
/// it searches before it lets the UI thread go (`SEARCH_STEP_BYTES` and
/// `SEARCH_SLICE` there).
const SEARCH_STEP_BYTES: usize = 64 * 1024;
const SEARCH_SLICE: Duration = Duration::from_millis(3);

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

/// The tags of the three containers of the stream: made once, as the log
/// panel makes them, and shared by their lines.
fn tags() -> Vec<SharedString> {
    (0..3).map(|ix| format!("web-{ix}").into()).collect()
}

fn entry(ix: usize, tags: &[SharedString]) -> LogEntry {
    // From 2026-10-01T08:00:00Z on.
    let at = 1_790_841_600_000 + (ix * 1000 / RATE) as i64;
    LogEntry::new(line(ix))
        .timestamp(jiff::Timestamp::from_millisecond(at).unwrap_or_default())
        .tag(tags[ix % 3].clone(), ix % 3)
}

fn buffer() -> LogBuffer {
    LogBuffer::new(DEFAULT_LOG_CAPACITY).with_max_bytes(DEFAULT_LOG_BYTES)
}

/// The steps of `LogView::push`.
fn push(buffer: &mut LogBuffer, search: &mut LogSearch, batch: Vec<LogEntry>) -> usize {
    let searched = search.is_complete(buffer);
    let pushed = buffer.push(batch);
    if pushed.evicted > 0 {
        search.evict(buffer);
    }
    if searched {
        search.finish(buffer);
    }
    pushed.evicted
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.
}

/// Feeds `SECONDS` of stream; returns the time spent per second of stream
/// while the buffer fills and once it evicts, and the slowest batch.
fn feed(query: &str) -> (f64, f64, Duration, usize) {
    let tags = tags();
    let mut buffer = buffer();
    let mut search = LogSearch::default();
    search.set_query(query, &buffer);
    let total = RATE * SECONDS;
    let (mut filling, mut evicting) = (Duration::ZERO, Duration::ZERO);
    let mut slowest = Duration::ZERO;
    let mut next = 0;
    while next < total {
        let batch: Vec<LogEntry> = (next..(next + BATCH).min(total))
            .map(|ix| entry(ix, &tags))
            .collect();
        next += batch.len();
        let started = Instant::now();
        let evicted = push(&mut buffer, &mut search, batch);
        let taken = started.elapsed();
        slowest = slowest.max(taken);
        if evicted > 0 {
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

/// What typing a query costs.
struct Typing {
    /// The keystroke itself: `LogSearch::set_query`.
    keystroke: Duration,
    /// Slices of `SEARCH_SLICE` the search took, their total and the
    /// longest of them: the longest the UI thread is held at once.
    slices: usize,
    searching: Duration,
    longest_slice: Duration,
    /// The longest push of a batch that arrived between two slices.
    longest_push: Duration,
    matches: usize,
    /// Heap bytes the matches hold.
    held: usize,
}

/// Types `query` into the search of the full `buffer` and searches it the
/// way `LogView` does: a slice, then the UI thread is free; here a batch of
/// the stream arrives in every such gap.
fn type_query(
    buffer: &mut LogBuffer,
    query: &str,
    tags: &[SharedString],
    next: &mut usize,
) -> Typing {
    let before = held();
    let mut search = LogSearch::default();
    let started = Instant::now();
    search.set_query(query, buffer);
    let mut typing = Typing {
        keystroke: started.elapsed(),
        slices: 0,
        searching: Duration::ZERO,
        longest_slice: Duration::ZERO,
        longest_push: Duration::ZERO,
        matches: 0,
        held: 0,
    };
    loop {
        let started = Instant::now();
        let complete = loop {
            let step = search.step(buffer, SEARCH_STEP_BYTES);
            if step.complete || started.elapsed() >= SEARCH_SLICE {
                break step.complete;
            }
        };
        let taken = started.elapsed();
        typing.slices += 1;
        typing.searching += taken;
        typing.longest_slice = typing.longest_slice.max(taken);
        if complete {
            break;
        }
        let batch: Vec<LogEntry> = (*next..*next + BATCH).map(|ix| entry(ix, tags)).collect();
        *next += BATCH;
        let started = Instant::now();
        push(buffer, &mut search, batch);
        typing.longest_push = typing.longest_push.max(started.elapsed());
    }
    typing.matches = search.len();
    typing.held = held().saturating_sub(before);
    typing
}

#[test]
#[ignore = "performance stand: prints timings, see docs/perf.md"]
fn perf_stand_log_stream() {
    println!(
        "\n== log buffer: {RATE} lines/s in batches of {BATCH}, capacity {DEFAULT_LOG_CAPACITY}, {SECONDS} s =="
    );
    let tags = tags();
    let started = Instant::now();
    let built: usize = (0..RATE).map(|ix| entry(ix, &tags).text.len()).sum();
    println!(
        "building {RATE} entries: {:.2} ms ({} bytes of text)",
        ms(started.elapsed()),
        built
    );
    let before = held();
    let mut full = buffer();
    for start in (0..DEFAULT_LOG_CAPACITY).step_by(BATCH) {
        full.push((start..(start + BATCH).min(DEFAULT_LOG_CAPACITY)).map(|ix| entry(ix, &tags)));
    }
    let text: usize = full.iter().map(|entry| entry.text.len()).sum();
    let lines = full.len();
    println!(
        "memory of a full buffer: {:.1} MiB for {lines} lines, {:.0} bytes per line, of them {:.0} text; \
         LogEntry is {} bytes; the buffer counts {:.1} MiB",
        (held() - before) as f64 / 1048576.,
        (held() - before) as f64 / lines as f64,
        text as f64 / lines as f64,
        size_of::<LogEntry>(),
        full.bytes() as f64 / 1048576.,
    );
    let mut next = DEFAULT_LOG_CAPACITY;
    for query in ["e", "error", "tenant-17", "no such text"] {
        let typing = type_query(&mut full, query, &tags, &mut next);
        println!(
            "typing {query:14?} into the search of a full buffer: keystroke {:.3} ms, then {} slices, \
             {:5.1} ms in all, longest slice {:.2} ms, longest push between {:.2} ms; \
             {} matches, {:.1} MiB",
            ms(typing.keystroke),
            typing.slices,
            ms(typing.searching),
            ms(typing.longest_slice),
            ms(typing.longest_push),
            typing.matches,
            typing.held as f64 / 1048576.
        );
    }
    // A stream of long lines: the limit of bytes is the one that holds.
    let before = held();
    let mut long = buffer();
    let long_line = "x".repeat(16 * 1024);
    let started = Instant::now();
    for _ in 0..40_000 / BATCH {
        long.push((0..BATCH).map(|_| LogEntry::new(long_line.clone())));
    }
    println!(
        "{} lines of 16 KiB ({:.0} MiB of text) pushed in {:.0} ms: {} lines kept, {:.1} MiB held, \
         the buffer counts {:.1} MiB of {} allowed",
        40_000 / BATCH * BATCH,
        (40_000 / BATCH * BATCH * long_line.len()) as f64 / 1048576.,
        ms(started.elapsed()),
        long.len(),
        (held() - before) as f64 / 1048576.,
        long.bytes() as f64 / 1048576.,
        DEFAULT_LOG_BYTES / 1048576,
    );
    drop(long);
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
