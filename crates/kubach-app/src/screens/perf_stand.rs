//! Stand for the row pipeline of the resource browser (`docs/perf.md`): a
//! pod list from the cache becomes table rows and replaces the rows of a
//! table, as `ResourceBrowser::update_rows` does on every cache update.
//!
//! No window: the table is a [`RowsProbe`], the data side of
//! `ResourceTable::set_rows`. Search filtering, usage cells, the selection
//! in the table state and the redraw are not measured.
//!
//! Ignored tests that print their figures; run them optimized:
//!
//! ```sh
//! cargo test -p kubach-app --release perf_stand -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `KUBACH_PERF_PODS` sets the number of pods (50 000 by default),
//! `KUBACH_PERF_SECONDS` how long the live part runs (10 by default).

use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kubach_core::cache::{DEBOUNCE, ResourceCache, WatchFilter, WatchStatus};
use kubach_core::perf::{self, Churn, PodShape};
use kubach_core::plugin::{PluginRegistry, ResourcePlugin};
use kubach_ui::{ResourceRow, RowsProbe, SortDirection};
use kube::api::DynamicObject;

use super::rows;

const NAMESPACES: usize = 50;
/// Pods changed by every burst of the live part (a rollout in progress).
const CHANGED: usize = 50;
const RUNS: usize = 5;

const SHAPES: [(&str, PodShape); 3] = [
    ("full", PodShape::FULL),
    ("managed", PodShape::MANAGED),
    ("slim", PodShape::SLIM),
];

fn env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn pod_count() -> usize {
    env("KUBACH_PERF_PODS", 50_000)
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.
}

/// Median of `RUNS` runs of `run`; `setup` is not timed.
fn median<S, T>(mut setup: impl FnMut() -> S, mut run: impl FnMut(S) -> T) -> Duration {
    let mut times: Vec<Duration> = (0..RUNS)
        .map(|_| {
            let input = setup();
            let started = Instant::now();
            let output = run(input);
            let taken = started.elapsed();
            drop(black_box(output));
            taken
        })
        .collect();
    times.sort();
    times[times.len() / 2]
}

struct Spread {
    count: usize,
    median: Duration,
    p95: Duration,
    max: Duration,
    total: Duration,
}

fn spread(mut times: Vec<Duration>) -> Spread {
    times.sort();
    let at = |share: f64| {
        times
            .get(((times.len() as f64 * share) as usize).min(times.len().saturating_sub(1)))
            .copied()
            .unwrap_or_default()
    };
    Spread {
        count: times.len(),
        median: at(0.5),
        p95: at(0.95),
        max: times.last().copied().unwrap_or_default(),
        total: times.iter().sum(),
    }
}

fn pod_plugin() -> Arc<dyn ResourcePlugin> {
    PluginRegistry::with_builtins().plugin_for("", "Pod")
}

fn build_rows(
    plugin: &dyn ResourcePlugin,
    objects: &[Arc<DynamicObject>],
) -> Vec<Arc<ResourceRow>> {
    objects
        .iter()
        .map(|object| Arc::new(rows::row(plugin, object)))
        .collect()
}

/// One pass of the pipeline over a list that does not change between
/// passes, step by step.
#[test]
#[ignore = "performance stand: prints timings, see docs/perf.md"]
fn perf_stand_rows_of_a_pod_list() {
    let count = pod_count();
    let plugin = pod_plugin();
    println!("\n== rows of {count} pods (median of {RUNS}) ==");
    for (name, shape) in SHAPES {
        let pods = perf::shared(perf::pods(count, NAMESPACES, shape));
        let bytes: usize = pods.iter().take(200).map(|pod| perf::json_len(pod)).sum();
        let build = median(|| (), |()| build_rows(plugin.as_ref(), &pods));
        println!(
            "{name:8} {:5} B/pod  build rows {:8.1} ms  ({:.2} us/row)",
            bytes / pods.len().min(200),
            ms(build),
            ms(build) * 1000. / count as f64,
        );
    }

    let pods = perf::shared(perf::pods(count, NAMESPACES, PodShape::SLIM));
    let columns = median(
        || (),
        |()| {
            for _ in 0..count {
                black_box(plugin.columns());
            }
        },
    );
    let keys = median(
        || (),
        |()| {
            pods.iter()
                .map(|pod| rows::object_key(pod))
                .collect::<Vec<_>>()
        },
    );
    println!("plugin.columns() x {count}: {:.1} ms", ms(columns));
    println!("object_key x {count}:       {:.1} ms", ms(keys));

    let rows = build_rows(plugin.as_ref(), &pods);
    let mut last_changed = rows.clone();
    if let Some(row) = last_changed.last_mut() {
        Arc::make_mut(row).key = "changed".into();
    }
    let mut probe = RowsProbe::new(rows::columns(plugin.as_ref()));
    probe.replace_rows(rows.clone());
    println!("-- table.set_rows on {count} rows --");
    let same = median(|| (), |()| probe.same_rows(&rows));
    let differs = median(|| (), |()| probe.same_rows(&last_changed));
    println!("compare, rows equal:            {:8.1} ms", ms(same));
    println!("compare, last row differs:      {:8.1} ms", ms(differs));
    println!("clone of the rows:              {:8.1} ms", {
        ms(median(|| (), |()| rows.clone()))
    });

    let replace =
        |probe: &mut RowsProbe| ms(median(|| rows.clone(), |rows| probe.replace_rows(rows)));
    println!(
        "replace, caller order:          {:8.1} ms",
        replace(&mut probe)
    );
    for (column, direction) in [
        ("name", SortDirection::Descending),
        ("status", SortDirection::Ascending),
        ("age", SortDirection::Ascending),
        ("restarts", SortDirection::Descending),
    ] {
        probe.sort_by(column, direction);
        println!(
            "replace, sorted by {column:9}:   {:8.1} ms",
            replace(&mut probe)
        );
    }
    probe.select(rows[rows.len() / 2].key.clone());
    println!(
        "  + a selected row:             {:8.1} ms",
        replace(&mut probe)
    );
    probe.check_all();
    println!(
        "  + every row checked:          {:8.1} ms",
        replace(&mut probe)
    );
}

/// Figures of one live run.
#[derive(Default)]
struct Live {
    snapshot: Vec<Duration>,
    rows: Vec<Duration>,
    set_rows: Vec<Duration>,
    whole: Vec<Duration>,
}

/// The list changes every [`DEBOUNCE`]; every notification of the cache
/// runs the pipeline, like the browser showing all namespaces
/// (`namespaces` is `None`) or a selection of them.
fn live(shape: PodShape, namespaces: Option<&[String]>, sorted: bool) -> (Duration, Live) {
    let count = pod_count();
    let seconds = env("KUBACH_PERF_SECONDS", 10) as u64;
    let churn = Churn {
        period: DEBOUNCE,
        changed: CHANGED,
    };
    // A watch per namespace gets its share of the changes.
    let per_watch = Churn {
        changed: namespaces.map_or(CHANGED, |all| (CHANGED / all.len()).max(1)),
        ..churn
    };
    let cache = ResourceCache::with_source(perf::source(
        perf::pods(count, NAMESPACES, shape),
        Some(per_watch),
    ));
    let plugin = pod_plugin();
    let watches = cache.subscribe_many(&perf::pod_kind(), namespaces, WatchFilter::default());
    let mut changes = watches.changes();
    let mut probe = RowsProbe::new(rows::columns(plugin.as_ref()));
    if sorted {
        probe.sort_by("age", SortDirection::Ascending);
    }

    let loading = Instant::now();
    let mut loaded = None;
    let mut figures = Live::default();
    let mut deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        assert!(futures::executor::block_on(changes.next()));
        let started = Instant::now();
        let snapshot = watches.snapshot();
        let read = started.elapsed();
        if snapshot.status != WatchStatus::Ready || snapshot.objects.len() < count {
            continue;
        }
        let rows = build_rows(plugin.as_ref(), &snapshot.objects);
        let built = started.elapsed();
        probe.set_rows(rows);
        let done = started.elapsed();
        if loaded.is_none() {
            // The first full list: the live part starts here.
            loaded = Some(loading.elapsed());
            deadline = Instant::now() + Duration::from_secs(seconds);
            continue;
        }
        figures.snapshot.push(read);
        figures.rows.push(built - read);
        figures.set_rows.push(done - built);
        figures.whole.push(done);
    }
    (loaded.expect("the list loaded"), figures)
}

fn report(title: &str, seconds: f64, loaded: Duration, figures: Live) {
    println!("-- {title} --");
    println!("initial list shown after {:.0} ms", ms(loaded));
    for (step, times) in [
        ("snapshot", figures.snapshot),
        ("build rows", figures.rows),
        ("set_rows", figures.set_rows),
        ("whole update", figures.whole),
    ] {
        let spread = spread(times);
        println!(
            "{step:12} median {:7.1} ms  p95 {:7.1} ms  max {:7.1} ms  ({} updates, {:.0} % of the time)",
            ms(spread.median),
            ms(spread.p95),
            ms(spread.max),
            spread.count,
            spread.total.as_secs_f64() / seconds * 100.,
        );
    }
}

#[test]
#[ignore = "performance stand: prints timings, see docs/perf.md"]
fn perf_stand_live_pod_list() {
    let count = pod_count();
    let seconds = env("KUBACH_PERF_SECONDS", 10) as f64;
    println!(
        "\n== live: {count} pods, {CHANGED} change every {} ms, {seconds} s ==",
        DEBOUNCE.as_millis()
    );
    for (name, shape) in [("managed", PodShape::MANAGED), ("slim", PodShape::SLIM)] {
        let (loaded, figures) = live(shape, None, false);
        report(
            &format!("{name}, all namespaces, caller order"),
            seconds,
            loaded,
            figures,
        );
    }
    let (loaded, figures) = live(PodShape::SLIM, None, true);
    report(
        "slim, all namespaces, sorted by age",
        seconds,
        loaded,
        figures,
    );
    let namespaces: Vec<String> = (0..NAMESPACES).map(perf::namespace).collect();
    let (loaded, figures) = live(PodShape::SLIM, Some(&namespaces), false);
    report(
        &format!("slim, {NAMESPACES} namespaces merged, caller order"),
        seconds,
        loaded,
        figures,
    );
}
