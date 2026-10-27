use super::*;
use futures::StreamExt as _;
use kube::api::{ApiResource, GroupVersionKind};
use kube::discovery::{ApiCapabilities, Scope, verbs};
use serde_json::{Value, json};

const GI: u64 = 1 << 30;
const MI: u64 = 1 << 20;

#[test]
fn parses_quantities() {
    let cases: &[(&str, f64)] = &[
        ("0", 0.0),
        ("1", 1.0),
        ("250m", 0.25),
        ("1500u", 0.0015),
        ("12345n", 0.000_012_345),
        ("1.5", 1.5),
        (".5", 0.5),
        ("5.", 5.0),
        ("+2", 2.0),
        ("-1", -1.0),
        ("1k", 1e3),
        ("2M", 2e6),
        ("3G", 3e9),
        ("1T", 1e12),
        ("1P", 1e15),
        ("1E", 1e18),
        ("1Ki", 1024.0),
        ("1.5Mi", 1.5 * 1024.0 * 1024.0),
        ("2Gi", 2.0 * GI as f64),
        ("1Ti", 1024f64.powi(4)),
        ("1Ei", 1024f64.powi(6)),
        ("1e3", 1e3),
        ("1E3", 1e3),
        ("5e-3", 0.005),
        ("1e+2", 100.0),
        (" 100Mi ", 100.0 * MI as f64),
    ];
    for &(s, want) in cases {
        let got = parse_quantity(s).unwrap_or_else(|e| panic!("{s}: {e}"));
        assert!(
            (got - want).abs() <= want.abs() * 1e-12,
            "{s}: {got} != {want}"
        );
    }
}

#[test]
fn rejects_invalid_quantities() {
    for s in [
        "", " ", "m", "Mi", "-", ".", "1.2.3", "1x", "1mi", "1KI", "1e", "1e+", "1e1.5", "1 Gi",
        "abc", "1ki", "1e400",
    ] {
        assert!(parse_quantity(s).is_err(), "{s:?} should be invalid");
    }
}

#[test]
fn converts_to_units() {
    assert_eq!(cpu_nanocores("250m").unwrap(), 250_000_000);
    assert_eq!(cpu_nanocores("2").unwrap(), 2_000_000_000);
    assert_eq!(cpu_nanocores("123456789n").unwrap(), 123_456_789);
    assert_eq!(cpu_nanocores("0.1").unwrap(), 100_000_000);
    assert_eq!(cpu_nanocores("1500u").unwrap(), 1_500_000);
    assert!(cpu_nanocores("-1").is_err());
    assert_eq!(memory_bytes("128Mi").unwrap(), 128 * MI);
    assert_eq!(memory_bytes("1G").unwrap(), 1_000_000_000);
    assert_eq!(memory_bytes("1.5Gi").unwrap(), 3 * GI / 2);
    assert_eq!(memory_bytes("12345Ki").unwrap(), 12345 * 1024);
    assert_eq!(memory_bytes("1k").unwrap(), 1000);
    assert_eq!(memory_bytes("1e3").unwrap(), 1000);
}

#[test]
fn formats_units() {
    assert_eq!(format_cpu(0), "0.000");
    assert_eq!(format_cpu(250_000_000), "0.250");
    assert_eq!(format_cpu(1_000_000_000), "1.000");
    assert_eq!(format_cpu(1_500_000_000), "1.500");
    assert_eq!(format_memory(900), "0.0 MiB");
    assert_eq!(format_memory(512 * MI), "512.0 MiB");
    assert_eq!(format_memory(3 * GI / 2), "1536.0 MiB");
}

fn pod_metrics() -> PodMetrics {
    PodMetrics::from_value(json!({
        "kind": "PodMetrics",
        "apiVersion": "metrics.k8s.io/v1beta1",
        "metadata": { "name": "api-1", "namespace": "web", "creationTimestamp": "2026-10-22T12:00:00Z" },
        "timestamp": "2026-10-22T11:59:50Z",
        "window": "10.233s",
        "containers": [
            { "name": "api", "usage": { "cpu": "125000000n", "memory": "64Mi" } },
            { "name": "proxy", "usage": { "cpu": "5m", "memory": "16384Ki" } }
        ]
    }))
    .unwrap()
}

#[test]
fn parses_pod_metrics() {
    let m = pod_metrics();
    assert_eq!(m.name, "api-1");
    assert_eq!(m.namespace, "web");
    assert_eq!(m.window.as_deref(), Some("10.233s"));
    assert_eq!(m.timestamp, Some("2026-10-22T11:59:50Z".parse().unwrap()));
    assert_eq!(
        m.total(),
        Usage {
            cpu: 130_000_000,
            memory: 80 * MI
        }
    );
}

#[test]
fn parses_node_metrics() {
    let m = NodeMetrics::from_value(json!({
        "metadata": { "name": "node-1" },
        "timestamp": "2026-10-22T11:59:50Z",
        "window": "20s",
        "usage": { "cpu": "1234567890n", "memory": "2Gi" }
    }))
    .unwrap();
    assert_eq!(m.name, "node-1");
    assert_eq!(
        m.usage,
        Usage {
            cpu: 1_234_567_890,
            memory: 2 * GI
        }
    );
}

#[test]
fn rejects_bad_metric_quantities() {
    let err = NodeMetrics::from_value(json!({
        "metadata": { "name": "node-1" },
        "usage": { "cpu": "lots", "memory": "2Gi" }
    }))
    .unwrap_err();
    assert!(matches!(err, MetricsError::Invalid(_)), "{err}");
    assert!(!err.is_unavailable());
}

fn pod(spec: Value) -> Pod {
    serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": { "name": "api-1", "namespace": "web" },
        "spec": spec
    }))
    .unwrap()
}

fn container(name: &str, resources: Value) -> Value {
    json!({ "name": name, "image": "x", "resources": resources })
}

#[test]
fn computes_pod_usage_against_requests_and_limits() {
    let pod = pod(json!({
        "containers": [
            container("api", json!({
                "requests": { "cpu": "250m", "memory": "128Mi" },
                "limits": { "cpu": "500m", "memory": "256Mi" }
            })),
            container("proxy", json!({ "limits": { "cpu": "50m", "memory": "32Mi" } })),
        ]
    }));
    let usage = pod_usage(&pod, &pod_metrics());

    // Requests default to limits for `proxy`.
    assert_eq!(usage.cpu.request, Some(300_000_000));
    assert_eq!(usage.cpu.limit, Some(550_000_000));
    assert_eq!(usage.memory.request, Some(160 * MI));
    assert_eq!(usage.memory.limit, Some(288 * MI));
    let cpu = usage.cpu.request_percent().unwrap();
    assert!((cpu - 130.0 / 300.0 * 100.0).abs() < 1e-9);
    assert!((usage.memory.request_percent().unwrap() - 50.0).abs() < 1e-9);

    let api = &usage.containers[0];
    assert_eq!(api.name, "api");
    assert!((api.cpu.request_percent().unwrap() - 50.0).abs() < 1e-9);
    assert!((api.cpu.limit_percent().unwrap() - 25.0).abs() < 1e-9);
    assert!((api.memory.limit_percent().unwrap() - 25.0).abs() < 1e-9);
    let proxy = &usage.containers[1];
    assert!((proxy.cpu.limit_percent().unwrap() - 10.0).abs() < 1e-9);
    assert!((proxy.memory.request_percent().unwrap() - 50.0).abs() < 1e-9);
}

#[test]
fn missing_requests_and_unbounded_limits_give_no_percentages() {
    let pod = pod(json!({
        "containers": [
            container("api", json!({ "requests": { "cpu": "100m" } })),
            container("proxy", json!({ "limits": { "cpu": "100m" } })),
        ]
    }));
    let usage = pod_usage(&pod, &pod_metrics());
    assert_eq!(usage.cpu.request, Some(200_000_000));
    // `api` has no CPU limit, so the pod is unbounded.
    assert_eq!(usage.cpu.limit, None);
    assert_eq!(usage.cpu.limit_percent(), None);
    assert_eq!(usage.memory.request, None);
    assert_eq!(usage.memory.request_percent(), None);
}

#[test]
fn zero_request_has_no_percentage() {
    let stat = UsageStat {
        used: 5,
        request: Some(0),
        limit: None,
    };
    assert_eq!(stat.request_percent(), None);
}

#[test]
fn effective_requests_include_init_containers_sidecars_and_overhead() {
    let pod = pod(json!({
        "initContainers": [
            container("migrate", json!({ "requests": { "cpu": "1", "memory": "64Mi" } })),
            {
                "name": "mesh", "image": "x", "restartPolicy": "Always",
                "resources": { "requests": { "cpu": "100m", "memory": "32Mi" } }
            }
        ],
        "containers": [
            container("api", json!({ "requests": { "cpu": "200m", "memory": "128Mi" } })),
        ],
        "overhead": { "cpu": "10m", "memory": "1Mi" }
    }));
    let (requests, limits) = pod_resources(&pod);
    // CPU: max(200m + 100m, 1) + 10m; memory: max(128 + 32, 64) + 1 Mi.
    assert_eq!(requests.cpu, Some(1_010_000_000));
    assert_eq!(requests.memory, Some(161 * MI));
    assert_eq!(limits, Resources::default());
}

fn dynamic_pod(status: Value) -> DynamicObject {
    serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": { "name": "api-1", "namespace": "web" },
        "status": status
    }))
    .unwrap()
}

#[test]
fn only_running_pods_are_active() {
    for (status, active) in [
        (json!({ "phase": "Running" }), true),
        (json!({ "phase": "Pending" }), true),
        (json!({ "phase": "Unknown" }), true),
        (json!({}), true),
        (json!({ "phase": "Succeeded" }), false),
        (json!({ "phase": "Failed" }), false),
    ] {
        assert_eq!(
            pod_is_active(&dynamic_pod(status.clone())),
            active,
            "{status}"
        );
    }
}

#[test]
fn sums_nothing_for_no_pods() {
    assert_eq!(sum_resources(&[]), Default::default());
}

#[test]
fn sums_requests_of_pods_that_set_them_and_unbounded_limits() {
    let pod = |cpu_req: Option<u64>, cpu_lim: Option<u64>| {
        (
            Resources {
                cpu: cpu_req,
                memory: Some(MI),
            },
            Resources {
                cpu: cpu_lim,
                memory: Some(2 * MI),
            },
        )
    };
    let (requests, limits) = sum_resources(&[
        pod(Some(100_000_000), Some(500_000_000)),
        pod(None, None),
        pod(Some(100_000_000), Some(500_000_000)),
    ]);
    assert_eq!(requests.cpu, Some(200_000_000));
    // The second pod has no CPU limit: the workload is unbounded.
    assert_eq!(limits.cpu, None);
    assert_eq!(requests.memory, Some(3 * MI));
    assert_eq!(limits.memory, Some(6 * MI));
    // Nobody sets a request: none.
    let (requests, _) = sum_resources(&[pod(None, None)]);
    assert_eq!(requests.cpu, None);
}

#[test]
fn sums_pods_with_init_containers_sidecars_and_mixed_units() {
    // One-shot init container bigger than the app; limits only, so
    // requests default to them. Units: cores, millicores, G and Mi.
    let migrating = pod(json!({
        "initContainers": [
            container("migrate", json!({ "limits": { "cpu": "1", "memory": "1G" } })),
        ],
        "containers": [
            container("api", json!({ "limits": { "cpu": "250m", "memory": "256Mi" } })),
        ]
    }));
    // A sidecar (init with `restartPolicy: Always`) runs alongside the app
    // and adds up; it has no limit, so this pod is unbounded. Units:
    // nanocores, plain bytes and Ki.
    let meshed = pod(json!({
        "initContainers": [
            {
                "name": "mesh", "image": "x", "restartPolicy": "Always",
                "resources": { "requests": { "cpu": "50000000n", "memory": "1048576" } }
            }
        ],
        "containers": [
            container("api", json!({
                "requests": { "cpu": "0.1", "memory": "1024Ki" },
                "limits": { "cpu": "200m", "memory": "2Mi" }
            })),
        ]
    }));
    // Nothing set at all.
    let bare = pod(json!({ "containers": [container("api", json!({}))] }));

    let (requests, limits) = sum_resources(&[
        pod_resources(&migrating),
        pod_resources(&meshed),
        pod_resources(&bare),
    ]);
    // CPU: max(250m, 1) + (100m + 50m); memory: max(256Mi, 1G) + 2 Mi.
    assert_eq!(requests.cpu, Some(1_150_000_000));
    assert_eq!(requests.memory, Some(1_000_000_000 + 2 * MI));
    assert_eq!(limits, Resources::default());

    let (requests, limits) = sum_resources(&[pod_resources(&migrating)]);
    assert_eq!(requests.cpu, Some(1_000_000_000));
    assert_eq!(limits.cpu, Some(1_000_000_000));
    assert_eq!(limits.memory, Some(1_000_000_000));
}

#[test]
fn computes_node_usage_against_allocatable() {
    let node: Node = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Node",
        "metadata": { "name": "node-1" },
        "status": {
            "capacity": { "cpu": "4", "memory": "8Gi" },
            "allocatable": { "cpu": "3800m", "memory": "7Gi" }
        }
    }))
    .unwrap();
    let metrics = NodeMetrics {
        name: "node-1".into(),
        timestamp: None,
        window: None,
        usage: Usage {
            cpu: 950_000_000,
            memory: 7 * GI / 2,
        },
    };
    let usage = node_usage(&node, &metrics);
    assert_eq!(usage.cpu.capacity, Some(4_000_000_000));
    assert!((usage.cpu.percent().unwrap() - 25.0).abs() < 1e-9);
    assert!((usage.memory.percent().unwrap() - 50.0).abs() < 1e-9);
}

fn kind(group: &str, kind: &str, plural: &str) -> (ApiResource, ApiCapabilities) {
    let gvk = GroupVersionKind::gvk(group, "v1beta1", kind);
    (
        ApiResource::from_gvk_with_plural(&gvk, plural),
        ApiCapabilities {
            scope: Scope::Namespaced,
            subresources: Vec::new(),
            operations: vec![verbs::LIST.into(), verbs::GET.into()],
        },
    )
}

#[test]
fn detects_metrics_server() {
    let with = ApiCatalog::from_resources([
        kind(METRICS_GROUP, "PodMetrics", "pods"),
        kind(METRICS_GROUP, "NodeMetrics", "nodes"),
    ]);
    assert!(MetricsApi::is_available(&with));
    let without = ApiCatalog::from_resources([kind("", "Pod", "pods")]);
    assert!(!MetricsApi::is_available(&without));
}

#[test]
fn unavailable_errors_are_flagged() {
    assert!(MetricsError::NotInstalled.is_unavailable());
    assert!(MetricsError::Unavailable("503".into()).is_unavailable());
    let status = kube::core::Status::failure("service unavailable", "ServiceUnavailable")
        .with_code(503)
        .boxed();
    assert!(MetricsError::from_kube(kube::Error::Api(status)).is_unavailable());
    let forbidden = kube::core::Status::failure("forbidden", "Forbidden")
        .with_code(403)
        .boxed();
    assert!(!MetricsError::from_kube(kube::Error::Api(forbidden)).is_unavailable());
}

#[tokio::test]
async fn polls_until_dropped() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let calls = Arc::new(AtomicU32::new(0));
    let mut stream = poll(Duration::from_millis(10), {
        let calls = Arc::clone(&calls);
        move || {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                if n == 1 {
                    Err(MetricsError::Unavailable("not yet".into()))
                } else {
                    Ok(n)
                }
            }
        }
    });
    assert_eq!(stream.next().await.unwrap().unwrap(), 0);
    assert!(stream.next().await.unwrap().unwrap_err().is_unavailable());
    assert_eq!(stream.next().await.unwrap().unwrap(), 2);
    drop(stream);
    let after = calls.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(50)).await;
    // At most one fetch was in flight when the stream was dropped.
    assert!(calls.load(Ordering::SeqCst) <= after + 1);
}

#[test]
fn index_joins_listed_pods_and_nodes() {
    let pods = MetricsIndex::from_pods([pod_metrics()]);
    let pod: DynamicObject = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": { "name": "api-1", "namespace": "web" },
        "spec": { "containers": [
            container("api", json!({ "requests": { "cpu": "250m", "memory": "128Mi" } })),
            container("proxy", json!({}))
        ] }
    }))
    .unwrap();
    assert_eq!(
        pods.usage_of(&pod),
        Some(Usage {
            cpu: 130_000_000,
            memory: 80 * MI
        })
    );
    let usage = pods.pod_usage_of(&pod).unwrap();
    assert_eq!(usage.cpu.request, Some(250_000_000));
    assert_eq!(usage.containers.len(), 2);

    let mut other = pod.clone();
    other.metadata.namespace = Some("db".into());
    assert_eq!(pods.usage_of(&other), None);
    assert_eq!(pods.pod_usage_of(&other), None);

    let nodes = MetricsIndex::from_nodes([NodeMetrics {
        name: "node-1".into(),
        timestamp: None,
        window: None,
        usage: Usage {
            cpu: 950_000_000,
            memory: GI,
        },
    }]);
    let node: DynamicObject = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Node",
        "metadata": { "name": "node-1" },
        "status": { "allocatable": { "cpu": "3800m", "memory": "4Gi" } }
    }))
    .unwrap();
    assert_eq!(nodes.usage_of(&node).map(|u| u.cpu), Some(950_000_000));
    let usage = nodes.node_usage_of(&node).unwrap();
    assert_eq!(usage.memory.fill(), Some(0.25));
    assert_eq!(nodes.usage_of(&pod), None);
}

#[test]
fn describes_usage_against_requests_and_limits() {
    let stat = UsageStat {
        used: 120_000_000,
        request: Some(250_000_000),
        limit: None,
    };
    assert_eq!(
        stat.describe(format_cpu),
        "0.120 · 48% of 0.250 request · no limit"
    );
    assert_eq!(stat.fill(), Some(0.48));

    let over = UsageStat {
        used: 600 * MI,
        request: Some(256 * MI),
        limit: Some(512 * MI),
    };
    assert_eq!(over.fill(), Some(1.0));
    assert!(
        over.describe(format_memory)
            .contains("117% of 512.0 MiB limit")
    );

    let bare = UsageStat {
        used: 5_000_000,
        ..UsageStat::default()
    };
    assert_eq!(bare.describe(format_cpu), "0.005 · no request · no limit");
    assert_eq!(bare.fill(), None);

    let node = NodeStat {
        used: 950_000_000,
        allocatable: Some(3_800_000_000),
        capacity: None,
    };
    assert_eq!(
        node.describe(format_cpu),
        "0.950 · 25% of 3.800 allocatable"
    );
    assert_eq!(NodeStat::default().describe(format_cpu), "0.000");
}
