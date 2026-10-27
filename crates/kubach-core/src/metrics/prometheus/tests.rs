use k8s_openapi::api::core::v1::Service;
use serde_json::json;

use super::{
    Endpoint, MAX_RESPONSE_BYTES, Metric, Point, PrometheusError, Range, Target, Window, Workload,
    candidates, error_answer, format_bytes, parse_query_range, parse_query_range_by, queries,
    query_range_path, read_limited, split_query,
};

fn service(
    namespace: &str,
    name: &str,
    labels: serde_json::Value,
    ports: serde_json::Value,
) -> Service {
    serde_json::from_value(json!({
        "metadata": { "namespace": namespace, "name": name, "labels": labels },
        "spec": { "ports": ports },
    }))
    .unwrap()
}

fn proxied(namespace: &str, name: &str, port: &str) -> Endpoint {
    Endpoint::Service {
        namespace: namespace.into(),
        name: name.into(),
        port: port.into(),
        https: false,
    }
}

#[test]
fn ranks_prometheus_services() {
    let services = [
        service(
            "monitoring",
            "stack-kube-prometheus-alertmanager",
            json!({ "app": "kube-prometheus-stack-alertmanager" }),
            json!([{ "name": "http-web", "port": 9093 }]),
        ),
        service(
            "monitoring",
            "prometheus-operated",
            json!({ "operated-prometheus": "true" }),
            json!([{ "name": "web", "port": 9090 }]),
        ),
        service(
            "monitoring",
            "stack-kube-prometheus-prometheus",
            json!({ "app": "kube-prometheus-stack-prometheus" }),
            json!([{ "name": "http-web", "port": 9090 }, { "name": "reloader-web", "port": 8080 }]),
        ),
        service(
            "monitoring",
            "stack-prometheus-node-exporter",
            json!({ "app.kubernetes.io/name": "prometheus-node-exporter" }),
            json!([{ "name": "http-metrics", "port": 9100 }]),
        ),
        service(
            "prom",
            "prometheus-server",
            json!({ "app.kubernetes.io/name": "prometheus", "app.kubernetes.io/component": "server" }),
            json!([{ "name": "http", "port": 80 }]),
        ),
        service("web", "api", json!({}), json!([{ "port": 8080 }])),
        service("kubach", "prometheus", json!({}), json!([{ "port": 9090 }])),
    ];
    assert_eq!(
        candidates(&services),
        vec![
            proxied("monitoring", "stack-kube-prometheus-prometheus", "http-web"),
            proxied("monitoring", "prometheus-operated", "web"),
            proxied("prom", "prometheus-server", "http"),
            proxied("kubach", "prometheus", "9090"),
        ]
    );
}

/// A look-alike in a namespace that sorts first does not win a tie (#200).
#[test]
fn well_known_namespaces_rank_first() {
    let stack = || json!({ "app": "kube-prometheus-stack-prometheus" });
    let web = || json!([{ "name": "web", "port": 9090 }]);
    let services = [
        service("aaa-dev", "prometheus-operated", stack(), web()),
        service("monitoring", "prometheus-operated", stack(), web()),
    ];
    let found = candidates(&services);
    assert_eq!(
        found,
        vec![
            proxied("monitoring", "prometheus-operated", "web"),
            proxied("aaa-dev", "prometheus-operated", "web"),
        ]
    );
    assert_eq!(found[0].unusual_namespace(), None);
    assert_eq!(found[1].unusual_namespace(), Some("aaa-dev"));

    // Even a weaker match in a well-known namespace comes first.
    let services = [
        service("aaa-dev", "prometheus-operated", stack(), web()),
        service("kube-system", "prometheus", json!({}), web()),
    ];
    assert_eq!(
        candidates(&services),
        vec![
            proxied("kube-system", "prometheus", "web"),
            proxied("aaa-dev", "prometheus-operated", "web"),
        ]
    );
    assert_eq!(
        Endpoint::Url("http://p:9090".into()).unusual_namespace(),
        None
    );
}

#[test]
fn parses_addresses() {
    assert_eq!(
        Endpoint::parse(" http://prometheus.example:9090/ ").unwrap(),
        Endpoint::Url("http://prometheus.example:9090".into())
    );
    assert_eq!(
        Endpoint::parse("https://example.org/prometheus").unwrap(),
        Endpoint::Url("https://example.org/prometheus".into())
    );
    assert_eq!(
        Endpoint::parse("monitoring/prometheus-operated").unwrap(),
        proxied("monitoring", "prometheus-operated", "9090")
    );
    assert_eq!(
        Endpoint::parse("monitoring/prometheus-k8s:web").unwrap(),
        proxied("monitoring", "prometheus-k8s", "web")
    );
    for bad in [
        "",
        "prometheus",
        "http://",
        "Monitoring/x",
        "a/b:c:d",
        "ns/",
    ] {
        assert!(
            matches!(
                Endpoint::parse(bad),
                Err(PrometheusError::InvalidAddress(_))
            ),
            "{bad:?}"
        );
    }
}

#[test]
fn proxies_services_through_the_api_server() {
    assert_eq!(
        proxied("monitoring", "prometheus-operated", "web").prefix(),
        "/api/v1/namespaces/monitoring/services/prometheus-operated:web/proxy"
    );
    let https = Endpoint::Service {
        namespace: "m".into(),
        name: "p".into(),
        port: "https".into(),
        https: true,
    };
    assert_eq!(
        https.prefix(),
        "/api/v1/namespaces/m/services/https:p:https/proxy"
    );
    assert_eq!(Endpoint::Url("http://x".into()).prefix(), "");
}

#[test]
fn builds_pod_queries() {
    let pod = Target::Pod {
        namespace: "web".into(),
        name: "api-0".into(),
    };
    let cpu = queries(&pod, Metric::Cpu, Range::Hour);
    assert_eq!(cpu.len(), 1);
    assert_eq!(
        cpu[0].promql,
        r#"sum(rate(container_cpu_usage_seconds_total{namespace="web",pod="api-0",container!="",container!="POD",metrics_path!="/metrics/resource"}[120s]))"#
    );
    let memory = queries(&pod, Metric::Memory, Range::Day);
    assert_eq!(
        memory[0].promql,
        r#"sum(container_memory_working_set_bytes{namespace="web",pod="api-0",container!="",container!="POD",metrics_path!="/metrics/resource"})"#
    );
    let network = queries(&pod, Metric::Network, Range::Day);
    assert_eq!(
        network.iter().map(|q| q.name).collect::<Vec<_>>(),
        ["Receive", "Transmit"]
    );
    assert_eq!(
        network[1].promql,
        r#"sum(rate(container_network_transmit_bytes_total{namespace="web",pod="api-0"}[1440s]))"#
    );
}

#[test]
fn deployment_queries_match_its_pods_by_name() {
    let deployment = Target::Workload {
        kind: Workload::Deployment,
        namespace: "web".into(),
        name: "api.v2".into(),
    };
    let cpu = &queries(&deployment, Metric::Cpu, Range::SixHours)[0].promql;
    assert_eq!(
        cpu,
        r#"sum(rate(container_cpu_usage_seconds_total{namespace="web",pod=~"api\\.v2-[a-z0-9]+-[a-z0-9]+",container!="",container!="POD",metrics_path!="/metrics/resource"}[360s]))"#
    );
}

#[test]
fn workload_pod_patterns() {
    assert_eq!(Workload::StatefulSet.pod_pattern("db"), "db-[0-9]+");
    assert_eq!(
        Workload::DaemonSet.pod_pattern("agent"),
        "agent-[a-z0-9]{5}"
    );
    assert_eq!(Workload::Job.pod_pattern("x"), "x-([0-9]+-)?[a-z0-9]{5}");
}

#[test]
fn filesystem_and_split_queries() {
    let pod = Target::Pod {
        namespace: "web".into(),
        name: "api-0".into(),
    };
    let fs = queries(&pod, Metric::Filesystem, Range::Hour);
    assert_eq!(
        fs.iter().map(|q| q.name).collect::<Vec<_>>(),
        ["Read", "Write"]
    );
    assert_eq!(
        fs[1].promql,
        r#"sum(rate(container_fs_writes_bytes_total{namespace="web",pod="api-0",container!="",container!="POD",metrics_path!="/metrics/resource"}[120s]))"#
    );
    assert_eq!(
        split_query(&pod, Metric::Memory, Range::Hour),
        Some((
            "container",
            r#"sum by (container)(container_memory_working_set_bytes{namespace="web",pod="api-0",container!="",container!="POD",metrics_path!="/metrics/resource"})"#.to_owned()
        ))
    );
    assert_eq!(split_query(&pod, Metric::Network, Range::Hour), None);
    let sts = Target::of("apps", "StatefulSet", Some("ns"), "db").unwrap();
    let (label, promql) = split_query(&sts, Metric::Cpu, Range::Hour).unwrap();
    assert_eq!(label, "pod");
    assert!(promql.starts_with("sum by (pod)(rate("), "{promql}");
    let node = Target::Node { name: "n".into() };
    assert_eq!(split_query(&node, Metric::Cpu, Range::Hour), None);
    assert!(
        queries(&node, Metric::Filesystem, Range::Hour)[0]
            .promql
            .starts_with("sum(rate(node_disk_read_bytes_total[120s]) and on (instance)")
    );
}

#[test]
fn parses_series_by_label() {
    let body = r#"{"status":"success","data":{"resultType":"matrix","result":[
        {"metric":{"container":"b"},"values":[[1,"2"]]},
        {"metric":{"container":"a"},"values":[[1,"1"],[2,"NaN"]]},
        {"metric":{"container":"c"},"values":[]}
    ]}}"#;
    let series = parse_query_range_by(body, "container").unwrap();
    assert_eq!(
        series,
        [
            (
                "a".to_owned(),
                vec![Point {
                    time: 1.0,
                    value: 1.0
                }]
            ),
            (
                "b".to_owned(),
                vec![Point {
                    time: 1.0,
                    value: 2.0
                }]
            ),
        ]
    );
}

#[test]
fn node_queries_prefer_node_exporter() {
    let node = Target::Node {
        name: "minikube".into(),
    };
    assert_eq!(
        queries(&node, Metric::Cpu, Range::Hour)[0].promql,
        r#"sum(rate(node_cpu_seconds_total{mode!~"idle|iowait|steal"}[120s]) and on (instance) node_uname_info{nodename="minikube"}) or sum(rate(container_cpu_usage_seconds_total{id="/",node="minikube"}[120s])) or sum(rate(container_cpu_usage_seconds_total{id="/",instance="minikube"}[120s]))"#
    );
    assert_eq!(
        queries(&node, Metric::Memory, Range::Hour)[0].promql,
        r#"sum((node_memory_MemTotal_bytes - node_memory_MemAvailable_bytes) and on (instance) node_uname_info{nodename="minikube"}) or sum(container_memory_working_set_bytes{id="/",node="minikube"}) or sum(container_memory_working_set_bytes{id="/",instance="minikube"})"#
    );
    let network = queries(&node, Metric::Network, Range::Hour);
    assert_eq!(
        network.iter().map(|q| q.name).collect::<Vec<_>>(),
        ["Receive", "Transmit"]
    );
    let transmit = &network[1].promql;
    assert!(
        transmit.starts_with(r#"sum(rate(node_network_transmit_bytes_total{device!~"lo|veth.*|"#),
        "{transmit}"
    );
    assert!(
        transmit.ends_with(r#"[120s]) and on (instance) node_uname_info{nodename="minikube"}) or sum(rate(container_network_transmit_bytes_total{id="/",node="minikube"}[120s])) or sum(rate(container_network_transmit_bytes_total{id="/",instance="minikube"}[120s]))"#),
        "{transmit}"
    );
}

/// Services of a real kube-prometheus-stack install (chart 92.11, Grafana
/// and Alertmanager off), labels and ports as served.
#[test]
fn discovers_kube_prometheus_stack() {
    let list: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/kube-prometheus-stack-services.json")).unwrap();
    let services: Vec<Service> = serde_json::from_value(list["items"].clone()).unwrap();
    assert_eq!(services.len(), 5);
    assert_eq!(
        candidates(&services),
        vec![
            proxied("monitoring", "kube-prometheus-stack-prometheus", "http-web"),
            proxied("monitoring", "prometheus-operated", "http-web"),
        ]
    );
}

/// `query_range` answers of a real kube-prometheus-stack.
#[test]
fn parses_real_answers() {
    let network = parse_query_range(include_str!("fixtures/pod-network-receive.json")).unwrap();
    assert_eq!(network.len(), 1);
    assert!(network[0].len() >= 2);
    assert!(network[0].windows(2).all(|w| w[0].time < w[1].time));
    assert!(network[0].iter().all(|p| p.value > 0.0));

    let cpu = parse_query_range(include_str!("fixtures/node-cpu.json")).unwrap();
    assert_eq!(cpu.len(), 1);
    assert!(cpu[0].iter().all(|p| p.value > 0.0 && p.value < 64.0));

    // Without the container filter a pod has three memory series: its
    // container, the pod cgroup (about the same again) and the sandbox.
    let memory = parse_query_range(include_str!("fixtures/pod-memory-unfiltered.json")).unwrap();
    assert_eq!(memory.len(), 3);
}

#[test]
fn quotes_label_values() {
    let pod = Target::Pod {
        namespace: r#"a"b"#.into(),
        name: r"c\d".into(),
    };
    let promql = &queries(&pod, Metric::Memory, Range::Hour)[0].promql;
    assert!(
        promql.contains(r#"namespace="a\"b",pod="c\\d""#),
        "{promql}"
    );
}

#[test]
fn targets_of_supported_kinds() {
    assert_eq!(
        Target::of("", "Pod", Some("ns"), "p"),
        Some(Target::Pod {
            namespace: "ns".into(),
            name: "p".into()
        })
    );
    assert!(matches!(
        Target::of("apps", "Deployment", Some("ns"), "d"),
        Some(Target::Workload {
            kind: Workload::Deployment,
            ..
        })
    ));
    assert!(matches!(
        Target::of("apps", "StatefulSet", Some("ns"), "s"),
        Some(Target::Workload {
            kind: Workload::StatefulSet,
            ..
        })
    ));
    assert_eq!(
        Target::of("", "Node", None, "n"),
        Some(Target::Node { name: "n".into() })
    );
    assert_eq!(Target::of("batch", "CronJob", Some("ns"), "c"), None);
    assert_eq!(Target::of("", "Pod", None, "p"), None);
}

#[test]
fn windows_align_to_the_step() {
    assert_eq!(Range::Hour.step(), 30);
    assert_eq!(Range::SixHours.step(), 180);
    assert_eq!(Range::Day.step(), 720);
    let window = Window::ending_at(Range::Hour, 1_700_000_017);
    assert_eq!(
        window,
        Window {
            start: 1_700_000_010 - 3600,
            end: 1_700_000_010,
            step: 30
        }
    );
}

#[test]
fn encodes_query_range_requests() {
    let window = Window {
        start: 100,
        end: 200,
        step: 30,
    };
    assert_eq!(
        query_range_path(r#"sum(up{job="a b"})"#, window),
        "/api/v1/query_range?query=sum%28up%7Bjob%3D%22a%20b%22%7D%29&start=100&end=200&step=30"
    );
}

#[test]
fn parses_matrix_responses() {
    let body = json!({
        "status": "success",
        "data": {
            "resultType": "matrix",
            "result": [
                {
                    "metric": {},
                    "values": [[1700000000, "0.25"], [1700000030.5, "NaN"], [1700000060, "1e3"]]
                },
                { "metric": { "pod": "x" }, "values": [] }
            ]
        }
    })
    .to_string();
    assert_eq!(
        parse_query_range(&body).unwrap(),
        vec![
            vec![
                Point {
                    time: 1_700_000_000.0,
                    value: 0.25
                },
                Point {
                    time: 1_700_000_060.0,
                    value: 1000.0
                },
            ],
            vec![],
        ]
    );
    let empty = r#"{"status":"success","data":{"resultType":"matrix","result":[]}}"#;
    assert_eq!(parse_query_range(empty).unwrap(), Vec::<Vec<Point>>::new());
}

#[test]
fn reports_query_errors() {
    let body = r#"{"status":"error","errorType":"bad_data","error":"parse error at char 4"}"#;
    match parse_query_range(body) {
        Err(PrometheusError::Query { kind, message }) => {
            assert_eq!(kind, "bad_data");
            assert_eq!(message, "parse error at char 4");
        }
        other => panic!("{other:?}"),
    }
    let vector = r#"{"status":"success","data":{"resultType":"vector","result":[]}}"#;
    assert!(matches!(
        parse_query_range(vector),
        Err(PrometheusError::Invalid(_))
    ));
    let bad_value =
        r#"{"status":"success","data":{"resultType":"matrix","result":[{"values":[[1,"x"]]}]}}"#;
    assert!(matches!(
        parse_query_range(bad_value),
        Err(PrometheusError::Invalid(_))
    ));
    assert!(matches!(
        parse_query_range("<html>"),
        Err(PrometheusError::Invalid(_))
    ));
}

#[test]
fn formats_metric_values() {
    assert_eq!(Metric::Cpu.format(0.25), "0.250");
    assert_eq!(Metric::Memory.format(512.0 * 1024.0 * 1024.0), "512.0 MiB");
    assert_eq!(Metric::Network.format(1536.0), "1.5Ki/s");
    assert_eq!(Metric::Network.format(-1.0), "0B/s");
    assert_eq!(format_bytes(2.0 * 1024.0 * 1024.0), "2Mi");
    assert_eq!(format_bytes(150.0 * 1024.0), "150Ki");
}

#[tokio::test]
async fn bodies_are_read_up_to_the_limit() {
    let chunks = |sizes: Vec<usize>| {
        futures::stream::iter(
            sizes
                .into_iter()
                .map(|size| Ok::<_, std::io::Error>(vec![b'x'; size])),
        )
    };
    let body = read_limited(chunks(vec![3, 4]), 7).await.unwrap();
    assert_eq!(body.len(), 7);
    assert!(matches!(
        read_limited(chunks(vec![3, 4, 1]), 7).await,
        Err(PrometheusError::Request(message)) if message.contains("larger than")
    ));
    // An endless body stops at the limit instead of growing without end.
    let endless = futures::stream::repeat_with(|| Ok::<_, std::io::Error>(vec![0u8; 64 * 1024]));
    assert!(read_limited(endless, MAX_RESPONSE_BYTES).await.is_err());
}

#[test]
fn error_answers_keep_their_message() {
    let query = r#"{"status":"error","errorType":"bad_data","error":"parse error"}"#;
    assert!(matches!(
        error_answer(400, query),
        PrometheusError::Query { kind, .. } if kind == "bad_data"
    ));
    let status =
        r#"{"kind":"Status","status":"Failure","message":"services \"x\" not found","code":404}"#;
    assert!(matches!(
        error_answer(404, status),
        PrometheusError::Request(message) if message == r#"404 services "x" not found"#
    ));
    assert!(matches!(
        error_answer(502, "bad gateway\n"),
        PrometheusError::Request(message) if message == "502 bad gateway"
    ));
}
