//! Stand for the row pipeline of the resource browser (`docs/perf.md`): a
//! pod list from the cache becomes table rows and replaces the rows of a
//! table, as `ResourceBrowser::update_rows` does on every cache update,
//! metrics poll and search keystroke.
//!
//! The rows are the browser's own [`RowCache`]. No window: the table is a
//! [`RowsProbe`], the data side of `ResourceTable::set_shared_rows`. The
//! selection in the table state and the redraw are not measured.
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
use kubach_core::metrics::{ContainerMetrics, MetricsIndex, PodMetrics, Usage};
use kubach_core::perf::{self, Churn, PodShape};
use kubach_core::plugin::{PluginRegistry, ResourcePlugin};
use kubach_ui::{ResourceRow, RowsProbe, SortDirection};
use kube::api::DynamicObject;

use super::rows::{self, RowBuilder, RowCache, RowInputs};
use super::usage::{MetricsState, UsageView};

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

/// The rows of a pod list as the browser keeps them; `usage` adds the CPU
/// and Memory cells.
fn row_cache(plugin: &Arc<dyn ResourcePlugin>, usage: bool) -> RowCache {
    RowCache::new(RowBuilder::new(plugin.clone(), None), usage)
}

/// What `KindView::rows` does: brings the rows to `objects`, then takes
/// those matching the search `query`.
fn rows_of(
    cache: &mut RowCache,
    objects: &[Arc<DynamicObject>],
    usage: Option<UsageView>,
    query: &str,
) -> Vec<Arc<ResourceRow>> {
    cache.update(objects, &RowInputs::current(usage));
    cache.rows(query)
}

/// Every pod uses `step` more than in the last sample.
fn usage_of_all(pods: &[Arc<DynamicObject>], step: u64) -> MetricsState {
    let samples = pods.iter().zip(0u64..).map(|(pod, ix)| PodMetrics {
        name: pod.metadata.name.clone().unwrap_or_default(),
        namespace: pod.metadata.namespace.clone().unwrap_or_default(),
        timestamp: None,
        window: None,
        containers: vec![ContainerMetrics {
            name: "app".into(),
            usage: Usage {
                cpu: (ix % 900 + step * 37) * 1_000_000,
                memory: (ix % 700 + step * 11) * 1_048_576,
            },
        }],
    });
    MetricsState::Ready(MetricsIndex::from_pods(samples))
}

/// One pass of the pipeline over a list, step by step: the first build of
/// the rows, then the updates the browser runs while the list is shown.
#[test]
#[ignore = "performance stand: prints timings, see docs/perf.md"]
fn perf_stand_rows_of_a_pod_list() {
    let count = pod_count();
    let plugin = pod_plugin();
    println!("\n== rows of {count} pods (median of {RUNS}) ==");
    for (name, shape) in SHAPES {
        let pods = perf::shared(perf::pods(count, NAMESPACES, shape));
        let bytes: usize = pods.iter().take(200).map(|pod| perf::json_len(pod)).sum();
        let build = median(
            || row_cache(&plugin, false),
            |mut cache| rows_of(&mut cache, &pods, None, ""),
        );
        println!(
            "{name:8} {:5} B/pod  first build {:8.1} ms  ({:.2} us/row)",
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

    // The updates of a list that is shown: rows and table together, as
    // `ResourceBrowser::update_rows` runs them.
    println!("-- update of {count} rows: rows + table.set_rows --");
    let mut cache = row_cache(&plugin, true);
    let mut probe = RowsProbe::new(rows::columns(plugin.as_ref()));
    probe.set_rows(rows_of(&mut cache, &pods, None, ""));
    let mut update = |cache: &mut RowCache,
                      objects: &[Arc<DynamicObject>],
                      usage: Option<UsageView>,
                      query: &str| {
        let rows = rows_of(cache, objects, usage, query);
        probe.set_rows(rows)
    };
    let unchanged = median(|| (), |()| update(&mut cache, &pods, None, ""));
    println!("nothing changed:                {:8.2} ms", ms(unchanged));

    // Two lists that differ in `CHANGED` pods, shown in turn.
    let mut other = pods.clone();
    let step = (count / CHANGED).max(1);
    for (revision, pod) in other.iter_mut().step_by(step).take(CHANGED).enumerate() {
        perf::touch(Arc::make_mut(pod), 20_000_000 + revision as u64);
    }
    let mut turn = 0;
    let changed = median(
        || {
            turn += 1;
            if turn % 2 == 1 { &other } else { &pods }
        },
        |objects| update(&mut cache, objects, None, ""),
    );
    println!(
        "{CHANGED} pods changed:                {:8.2} ms",
        ms(changed)
    );

    let mut removed = pods.clone();
    removed.truncate(count - count / 10);
    let mut turn = 0;
    let relisted = median(
        || {
            turn += 1;
            if turn % 2 == 1 { &removed } else { &pods }
        },
        |objects| update(&mut cache, objects, None, ""),
    );
    println!("a tenth removed / added back:   {:8.2} ms", ms(relisted));

    let samples = [usage_of_all(&pods, 1), usage_of_all(&pods, 2)];
    let mut revision = 0;
    let polled = median(
        || {
            revision += 1;
            UsageView {
                state: &samples[revision as usize % 2],
                revision,
            }
        },
        |usage| update(&mut cache, &pods, Some(usage), ""),
    );
    println!("metrics poll, all usage changed:{:8.2} ms", ms(polled));
    let usage = UsageView {
        state: &samples[revision as usize % 2],
        revision,
    };
    let same_poll = median(
        || {
            revision += 1;
            UsageView { revision, ..usage }
        },
        |usage| update(&mut cache, &pods, Some(usage), ""),
    );
    println!("metrics poll, same usage:       {:8.2} ms", ms(same_poll));

    // A search narrows the rows and widens them again, keystroke by
    // keystroke: every update replaces the rows of the table.
    let queries = ["app-0", "app-00", "app-001", "app-00", "app-0", ""];
    let mut key = 0;
    let typed = median(
        || {
            key += 1;
            queries[key % queries.len()]
        },
        |query| update(&mut cache, &pods, Some(usage), query),
    );
    println!("search keystroke:               {:8.2} ms", ms(typed));
    let tick = median(|| (), |()| cache.due(kubach_ui::unix_now()));
    println!("clock tick (nothing turns):     {:8.4} ms", ms(tick));

    let rows = rows_of(&mut row_cache(&plugin, false), &pods, None, "");
    let mut last_changed = rows.clone();
    if let Some(row) = last_changed.last_mut() {
        Arc::make_mut(row).key = "changed".into();
    }
    // Rows built again for the same objects: equal, in other allocations.
    let rebuilt = rows_of(&mut row_cache(&plugin, false), &pods, None, "");
    let mut probe = RowsProbe::new(rows::columns(plugin.as_ref()));
    probe.replace_rows(rows.clone());
    println!("-- table.set_rows on {count} rows --");
    let same = median(|| (), |()| probe.same_rows(&rows));
    let equal = median(|| (), |()| probe.same_rows(&rebuilt));
    let differs = median(|| (), |()| probe.same_rows(&last_changed));
    println!("compare, the same rows:         {:8.1} ms", ms(same));
    println!("compare, rows built again:      {:8.1} ms", ms(equal));
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
    let mut cache = row_cache(&plugin, false);
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
        let rows = rows_of(&mut cache, &snapshot.objects, None, "");
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
