//! Benchmarks of the hot paths `docs/perf.md` tracks: decoding a pod and
//! its table cells, the object store of a watch, merging namespace
//! snapshots, building the topology and writing the list cache.
//!
//! `cargo bench -p kubach-core --bench core` (a filter narrows it:
//! `-- merge`). Objects come from `kubach_core::perf`.

use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use kube::api::DynamicObject;
use kube::runtime::watcher::Event;

use kubach_core::ansi;
use kubach_core::cache::{ObjectStore, Snapshot, WatchStatus, merge};
use kubach_core::list_cache::{CachedKind, Limits, ListCache, ListKey};
use kubach_core::perf::{self, ClusterSize, PodShape};
use kubach_core::plugin::PluginRegistry;
use kubach_core::topology::netpol::{self, PolicySet};
use kubach_core::topology::{BuildOptions, Detail, SourceKind, TopologyBuilder, TopologyInput};

const SHAPES: [(&str, PodShape); 3] = [
    ("full", PodShape::FULL),
    ("managed", PodShape::MANAGED),
    ("slim", PodShape::SLIM),
];

/// Per-pod costs of a table row: the typed decode and the plugin's cells.
fn pod(c: &mut Criterion) {
    const PODS: usize = 1000;
    let plugin = PluginRegistry::with_builtins().plugin_for("", "Pod");
    let mut group = c.benchmark_group("pod");
    group.throughput(Throughput::Elements(PODS as u64));
    for (name, shape) in SHAPES {
        let pods = perf::pods(PODS, 10, shape);
        group.bench_with_input(BenchmarkId::new("decode", name), &pods, |b, pods| {
            b.iter(|| {
                for pod in pods {
                    black_box(perf::decode_pod(black_box(pod)));
                }
            });
        });
        group.bench_with_input(BenchmarkId::new("cells", name), &pods, |b, pods| {
            b.iter(|| {
                for pod in pods {
                    black_box(plugin.cells(black_box(pod)));
                }
            });
        });
        group.bench_with_input(BenchmarkId::new("clone", name), &pods, |b, pods| {
            b.iter(|| {
                for pod in pods {
                    black_box(DynamicObject::clone(black_box(pod)));
                }
            });
        });
    }
    group.bench_function("columns", |b| {
        b.iter(|| {
            for _ in 0..PODS {
                black_box(plugin.columns());
            }
        });
    });
    group.finish();
}

/// The store of one watch: the initial list and a change with the
/// snapshot a subscriber reads after it.
fn store(c: &mut Criterion) {
    let mut group = c.benchmark_group("store");
    group.sample_size(20);
    for count in [10_000usize, 50_000] {
        let pods = perf::pods(count, 50, PodShape::SLIM);
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(BenchmarkId::new("initial_list", count), &pods, |b, pods| {
            b.iter_batched(
                || pods.clone(),
                |pods| {
                    let mut store = ObjectStore::new(false);
                    store.apply(Ok(Event::Init));
                    for pod in pods {
                        store.apply(Ok(Event::InitApply(pod)));
                    }
                    store.apply(Ok(Event::InitDone));
                    black_box(store.snapshot());
                    store
                },
                BatchSize::LargeInput,
            );
        });

        let mut loaded = ObjectStore::new(false);
        loaded.apply(Ok(Event::Init));
        for pod in pods.iter().cloned() {
            loaded.apply(Ok(Event::InitApply(pod)));
        }
        loaded.apply(Ok(Event::InitDone));
        let mut changed = pods[count / 2].clone();
        let mut revision = 0;
        group.throughput(Throughput::Elements(1));
        group.bench_function(BenchmarkId::new("apply_and_snapshot", count), |b| {
            b.iter(|| {
                revision += 1;
                perf::touch(&mut changed, revision);
                loaded.apply(Ok(Event::Apply(changed.clone())));
                black_box(loaded.snapshot());
            });
        });
    }
    group.finish();
}

/// Snapshots of `namespaces` watches with `per_namespace` pods each.
fn parts(namespaces: usize, per_namespace: usize) -> Vec<(Option<String>, Snapshot)> {
    let pods = perf::shared(perf::pods(
        namespaces * per_namespace,
        namespaces,
        PodShape::SLIM,
    ));
    pods.chunks(per_namespace)
        .map(|chunk| {
            (
                chunk[0].metadata.namespace.clone(),
                Snapshot {
                    objects: chunk.to_vec().into(),
                    status: WatchStatus::Ready,
                    generation: 1,
                },
            )
        })
        .collect()
}

/// A kind shown in several namespaces: every update merges their snapshots.
fn merge_namespaces(c: &mut Criterion) {
    let mut group = c.benchmark_group("merge");
    for (namespaces, per_namespace) in [(1usize, 20_000usize), (20, 1000), (50, 1000)] {
        let parts = parts(namespaces, per_namespace);
        group.throughput(Throughput::Elements((namespaces * per_namespace) as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{namespaces}x{per_namespace}")),
            &parts,
            |b, parts| {
                b.iter_batched(
                    || parts.clone(),
                    |parts| black_box(merge(parts)),
                    BatchSize::LargeInput,
                );
            },
        );
    }
    group.finish();
}

/// `input` after a watch delivered a change of one pod: every list but the
/// pods' is the same allocation, and every pod but one the same object.
fn one_pod_changed(input: &TopologyInput) -> TopologyInput {
    let mut pods = input.objects(SourceKind::Pod).to_vec();
    let mut pod = DynamicObject::clone(&pods[0]);
    perf::touch(&mut pod, 99_000_000);
    pods[0] = Arc::new(pod);
    let mut changed = input.clone();
    changed.set(SourceKind::Pod, pods);
    changed
}

/// The topology of a cluster: the first build, a rebuild with nothing
/// changed and one after a pod changed.
fn topology(c: &mut Criterion) {
    let mut group = c.benchmark_group("topology");
    group.sample_size(20);
    let sizes = [
        ClusterSize {
            namespaces: 20,
            apps: 25,
            replicas: 4,
        },
        ClusterSize {
            namespaces: 50,
            apps: 40,
            replicas: 5,
        },
    ];
    for size in sizes {
        let objects = perf::cluster(size, PodShape::MANAGED);
        let input = TopologyInput::from_objects(objects);
        let changed = one_pod_changed(&input);
        let label = format!("{}pods", size.pods());
        for (detail_name, detail) in [("workloads", Detail::Workloads), ("pods", Detail::Pods)] {
            let options = BuildOptions {
                detail,
                ..BuildOptions::default()
            };
            let id = |step: &str| BenchmarkId::new(format!("{step}/{detail_name}"), &label);
            group.bench_function(id("cold"), |b| {
                b.iter(|| black_box(TopologyBuilder::new().build(&input, &options)));
            });
            let mut warm = TopologyBuilder::new();
            warm.build(&input, &options);
            group.bench_function(id("unchanged"), |b| {
                b.iter(|| black_box(warm.build(&input, &options)));
            });
            let mut flip = false;
            group.bench_function(id("one_pod_changed"), |b| {
                b.iter(|| {
                    flip = !flip;
                    black_box(warm.build(if flip { &changed } else { &input }, &options))
                });
            });
        }
    }
    group.finish();
}

/// Prints where a rebuild after one changed pod spends its time: the
/// median of each phase over 15 builds.
fn phases(objects: &[DynamicObject], size: ClusterSize) {
    let input = TopologyInput::from_objects(objects.to_vec());
    let changed = one_pod_changed(&input);
    for (detail_name, detail) in [("workloads", Detail::Workloads), ("pods", Detail::Pods)] {
        let options = BuildOptions {
            detail,
            ..BuildOptions::default()
        };
        let mut builder = TopologyBuilder::new();
        builder.build(&input, &options);
        let mut laps: Vec<(&str, Vec<Duration>)> = Vec::new();
        for run in 0..15 {
            builder.build(if run % 2 == 0 { &changed } else { &input }, &options);
            for (ix, (name, time)) in builder.phases().iter().enumerate() {
                if laps.len() <= ix {
                    laps.push((name, Vec::new()));
                }
                laps[ix].1.push(*time);
            }
        }
        let text: Vec<String> = laps
            .iter_mut()
            .map(|(name, times)| {
                times.sort();
                format!(
                    "{name} {:.1} ms",
                    times[times.len() / 2].as_secs_f64() * 1e3
                )
            })
            .collect();
        eprintln!(
            "phases, one pod changed, {} pods, {detail_name}: {}",
            size.pods(),
            text.join(", ")
        );
    }
}

/// What the base cluster of [`topology`] lacks: the NetworkPolicy layer
/// (200 policies over 10 000 pods in 50 namespaces) and StatefulSets with
/// the PVCs of their volume claim templates (250 sets, 1 500 PVCs).
fn topology_layers(c: &mut Criterion) {
    let mut group = c.benchmark_group("topology_layers");
    group.sample_size(10);
    let size = ClusterSize {
        namespaces: 50,
        apps: 40,
        replicas: 5,
    };
    let base = perf::cluster(size, PodShape::MANAGED);
    phases(&base, size);

    let mut objects = base.clone();
    objects.extend(perf::namespaces(size.namespaces));
    objects.extend(perf::network_policies(size, 200));
    let input = TopologyInput::from_objects(objects);
    let changed = one_pod_changed(&input);
    group.bench_function("policy_set/200policies", |b| {
        b.iter(|| black_box(PolicySet::from_input(&input)));
    });
    for (detail_name, detail) in [("workloads", Detail::Workloads), ("pods", Detail::Pods)] {
        let plain = BuildOptions {
            detail,
            ..BuildOptions::default()
        };
        let layered = BuildOptions {
            network_policies: true,
            ..plain.clone()
        };
        let graph = TopologyBuilder::new().build(&input, &plain);
        let policies = PolicySet::from_input(&input);
        let id = |step: &str| BenchmarkId::new(format!("{step}/{detail_name}"), "200policies");
        // The layer alone, over a built graph.
        group.bench_function(id("netpol_apply"), |b| {
            b.iter_batched(
                || graph.clone(),
                |graph| black_box(netpol::apply(graph, &policies)),
                BatchSize::LargeInput,
            );
        });
        // A rebuild with the layer on, after a pod changed.
        let mut warm = TopologyBuilder::new();
        warm.build(&input, &layered);
        let mut flip = false;
        group.bench_function(id("one_pod_changed"), |b| {
            b.iter(|| {
                flip = !flip;
                black_box(warm.build(if flip { &changed } else { &input }, &layered))
            });
        });
    }

    let mut objects = base;
    objects.extend(perf::stateful_sets(
        size.namespaces,
        5,
        3,
        PodShape::MANAGED,
    ));
    let input = TopologyInput::from_objects(objects);
    let changed = one_pod_changed(&input);
    let options = BuildOptions::default();
    group.bench_function("stateful_sets/cold", |b| {
        b.iter(|| black_box(TopologyBuilder::new().build(&input, &options)));
    });
    let mut warm = TopologyBuilder::new();
    warm.build(&input, &options);
    let mut flip = false;
    group.bench_function("stateful_sets/one_pod_changed", |b| {
        b.iter(|| {
            flip = !flip;
            black_box(warm.build(if flip { &changed } else { &input }, &options))
        });
    });
    group.finish();
}

/// Writing a pod list to the disk cache: slimming, JSON, gzip, the file.
fn list_cache(c: &mut Criterion) {
    const PODS: usize = 10_000;
    let dir = tempfile::tempdir().expect("a temp dir");
    // The entry size limit is lifted so every variant is written in full.
    let limits = Limits {
        max_entry_bytes: u64::MAX,
        max_total_bytes: u64::MAX,
        ..Limits::default()
    };
    let cache = ListCache::new(dir.path(), limits);
    let kind = CachedKind::of(&perf::pod_kind());
    let key = ListKey::new("bench", "", "Pod", None);
    let mut group = c.benchmark_group("list_cache");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));
    group.throughput(Throughput::Elements(PODS as u64));
    for (name, shape) in SHAPES {
        let pods: Vec<Arc<DynamicObject>> = perf::shared(perf::pods(PODS, 50, shape));
        assert!(cache.store(&key, &kind, &pods), "the list is stored");
        group.bench_with_input(BenchmarkId::new("store", name), &pods, |b, pods| {
            b.iter(|| black_box(cache.store(&key, &kind, pods)));
        });
        group.bench_with_input(BenchmarkId::new("load", name), &pods, |b, _| {
            b.iter(|| black_box(cache.load(&key)));
        });
    }
    group.finish();
}

/// What the log panel does to each line before it is shown: plain lines
/// are only scanned for escape sequences, colored ones are parsed.
fn log_lines(c: &mut Criterion) {
    const LINES: usize = 5000;
    let plain: Vec<String> = (0..LINES)
        .map(|ix| {
            format!(
                "INFO request_id={:016x} method=GET path=/api/v1/items/{} status=200 duration_ms={}",
                ix as u64 * 0x9e37_79b9,
                ix % 9973,
                ix % 250
            )
        })
        .collect();
    let colored: Vec<String> = plain
        .iter()
        .map(|line| {
            let (level, rest) = line.split_at(4);
            format!("\x1b[32m{level}\x1b[0m\x1b[2m{rest}\x1b[0m \x1b[1;31mdone\x1b[0m")
        })
        .collect();
    let mut group = c.benchmark_group("log_lines");
    group.throughput(Throughput::Elements(LINES as u64));
    for (name, lines) in [("plain", &plain), ("colored", &colored)] {
        group.bench_with_input(BenchmarkId::new("ansi", name), lines, |b, lines| {
            b.iter(|| {
                for line in lines {
                    if ansi::has_controls(line) {
                        black_box(ansi::parse(line));
                    }
                }
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    pod,
    store,
    merge_namespaces,
    topology,
    topology_layers,
    list_cache,
    log_lines
);
criterion_main!(benches);
