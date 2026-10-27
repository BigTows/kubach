//! Current CPU and memory of all listed Pods or Nodes from Prometheus, for
//! the usage columns of resource tables when metrics-server is missing
//! (#179). Two instant queries per poll cover every visible namespace: one
//! for CPU, one for memory, summed by `namespace, pod, container` (Pods) or
//! by `node` (Nodes), in the units of metrics-server (nanocores, bytes).

use std::collections::BTreeMap;

use serde::Deserialize;

use super::{Prometheus, PrometheusError, RawResponse, container_selector, regex_literal};
use crate::metrics::{ContainerMetrics, MetricsIndex, NodeMetrics, PodMetrics, Usage};

/// `rate()` window of the CPU query: several scrapes at the usual 30 s
/// interval, so a missed scrape does not blank the column.
pub const USAGE_RATE_WINDOW: &str = "2m";

/// Label matcher for series in `namespaces` (any namespace when `None`).
pub fn namespace_matcher(namespaces: Option<&[String]>) -> String {
    match namespaces {
        Some(namespaces) => {
            let names: Vec<String> = namespaces.iter().map(|n| regex_literal(n)).collect();
            format!(r#"namespace=~"{}""#, super::quote(&names.join("|")))
        }
        None => r#"namespace!="""#.to_owned(),
    }
}

/// PromQL for CPU (cores) and memory (bytes) per container of the pods in
/// `namespaces` (all when `None`). Same container filter as the charts:
/// the pod cgroup and the sandbox would count everything twice.
pub fn pod_usage_queries(namespaces: Option<&[String]>) -> [String; 2] {
    let selector = container_selector(&namespace_matcher(namespaces));
    [
        format!(
            "sum by (namespace, pod, container) (rate(container_cpu_usage_seconds_total{{{selector}}}[{USAGE_RATE_WINDOW}]))"
        ),
        format!(
            "sum by (namespace, pod, container) (container_memory_working_set_bytes{{{selector}}})"
        ),
    ]
}

/// PromQL for CPU (cores) and memory (bytes) per node, labelled `node`.
/// node-exporter first, tied to the node by `node_uname_info{nodename}`
/// (kube-prometheus-stack drops the kubelet's root cgroup); then the
/// cAdvisor root cgroup (`id="/"`), labelled `node` by kube-prometheus-stack
/// and `instance` by the community chart.
pub fn node_usage_queries() -> [String; 2] {
    let per_node = |expr: &str| {
        format!(
            r#"sum by (node) (label_replace({expr} * on (instance) group_left (nodename) node_uname_info, "node", "$1", "nodename", "(.+)"))"#
        )
    };
    let root = |series: &str, rate: bool| {
        let by_node = format!(r#"{series}{{id="/",node!=""}}"#);
        let by_instance = format!(r#"{series}{{id="/",instance!=""}}"#);
        let (by_node, by_instance) = if rate {
            (
                format!("rate({by_node}[{USAGE_RATE_WINDOW}])"),
                format!("rate({by_instance}[{USAGE_RATE_WINDOW}])"),
            )
        } else {
            (by_node, by_instance)
        };
        format!(
            r#"sum by (node) ({by_node}) or label_replace(sum by (instance) ({by_instance}), "node", "$1", "instance", "(.+)")"#
        )
    };
    [
        format!(
            "{} or {}",
            per_node(&format!(
                r#"rate(node_cpu_seconds_total{{mode!~"idle|iowait|steal"}}[{USAGE_RATE_WINDOW}])"#
            )),
            root("container_cpu_usage_seconds_total", true)
        ),
        format!(
            "{} or {}",
            per_node("(node_memory_MemTotal_bytes - node_memory_MemAvailable_bytes)"),
            root("container_memory_working_set_bytes", false)
        ),
    ]
}

/// Path and query of an instant `query` request, after the endpoint prefix.
pub fn query_path(promql: &str) -> String {
    format!("/api/v1/query?query={}", super::encode(promql))
}

/// Labels of a series.
pub type Labels = BTreeMap<String, String>;

#[derive(Deserialize)]
struct RawSample {
    #[serde(default)]
    metric: Labels,
    value: (f64, String),
}

/// Parses an instant `query` answer (a vector) into labels and values.
/// `NaN` and infinite values are dropped.
pub fn parse_vector(body: &str) -> Result<Vec<(Labels, f64)>, PrometheusError> {
    let invalid = |e: &dyn std::fmt::Display| PrometheusError::Invalid(e.to_string());
    let raw: RawResponse = serde_json::from_str(body).map_err(|e| invalid(&e))?;
    if raw.status != "success" {
        return Err(PrometheusError::Query {
            kind: raw.error_type.unwrap_or_else(|| "unknown".into()),
            message: raw.error.unwrap_or_default(),
        });
    }
    let data = raw.data.ok_or_else(|| invalid(&"no data"))?;
    if data.result_type != "vector" {
        return Err(invalid(&format!(
            "expected a vector, got {}",
            data.result_type
        )));
    }
    let samples: Vec<RawSample> = serde_json::from_value(data.result).map_err(|e| invalid(&e))?;
    samples
        .into_iter()
        .filter_map(|sample| match sample.value.1.parse::<f64>() {
            Ok(value) if value.is_finite() => Some(Ok((sample.metric, value))),
            Ok(_) => None,
            Err(_) => Some(Err(invalid(&format!(
                "bad sample value {:?}",
                sample.value.1
            )))),
        })
        .collect()
}

/// Cores to nanocores, as metrics-server reports CPU.
fn nanocores(cores: f64) -> u64 {
    (cores.max(0.0) * 1e9).round() as u64
}

fn bytes(value: f64) -> u64 {
    value.max(0.0).round() as u64
}

/// Joins the CPU and memory vectors of [`pod_usage_queries`] into pod
/// metrics. A container missing from one of them counts zero there.
pub fn pod_index(cpu: Vec<(Labels, f64)>, memory: Vec<(Labels, f64)>) -> MetricsIndex {
    let mut pods: BTreeMap<(String, String), BTreeMap<String, Usage>> = BTreeMap::new();
    let mut add = |samples: Vec<(Labels, f64)>, set: fn(&mut Usage, f64)| {
        for (mut labels, value) in samples {
            let (Some(namespace), Some(pod)) = (labels.remove("namespace"), labels.remove("pod"))
            else {
                continue;
            };
            let container = labels.remove("container").unwrap_or_default();
            set(
                pods.entry((namespace, pod))
                    .or_default()
                    .entry(container)
                    .or_default(),
                value,
            );
        }
    };
    add(cpu, |usage, value| usage.cpu = nanocores(value));
    add(memory, |usage, value| usage.memory = bytes(value));
    MetricsIndex::from_pods(pods.into_iter().map(|((namespace, name), containers)| {
        PodMetrics {
            name,
            namespace,
            timestamp: None,
            window: None,
            containers: containers
                .into_iter()
                .map(|(name, usage)| ContainerMetrics { name, usage })
                .collect(),
        }
    }))
}

/// Joins the CPU and memory vectors of [`node_usage_queries`] into node
/// metrics.
pub fn node_index(cpu: Vec<(Labels, f64)>, memory: Vec<(Labels, f64)>) -> MetricsIndex {
    let mut nodes: BTreeMap<String, Usage> = BTreeMap::new();
    for (labels, value) in cpu {
        if let Some(node) = labels.get("node") {
            nodes.entry(node.clone()).or_default().cpu = nanocores(value);
        }
    }
    for (labels, value) in memory {
        if let Some(node) = labels.get("node") {
            nodes.entry(node.clone()).or_default().memory = bytes(value);
        }
    }
    MetricsIndex::from_nodes(nodes.into_iter().map(|(name, usage)| NodeMetrics {
        name,
        timestamp: None,
        window: None,
        usage,
    }))
}

impl Prometheus {
    /// An instant query: labels and value of each series.
    pub async fn query(&self, promql: &str) -> Result<Vec<(Labels, f64)>, PrometheusError> {
        let body = self.get(&query_path(promql)).await?;
        parse_vector(&body)
    }

    /// [`Self::query`] with a smaller answer limit than
    /// [`MAX_RESPONSE_BYTES`](super::MAX_RESPONSE_BYTES): a bigger answer
    /// fails without being read in full.
    pub async fn query_limited(
        &self,
        promql: &str,
        limit: usize,
    ) -> Result<Vec<(Labels, f64)>, PrometheusError> {
        let body = self.get_limited(&query_path(promql), limit).await?;
        parse_vector(&body)
    }

    /// Current CPU and memory of the pods in `namespaces` (all when
    /// `None`), per container: two queries whatever the number of pods.
    pub async fn pod_usage(
        &self,
        namespaces: Option<&[String]>,
    ) -> Result<MetricsIndex, PrometheusError> {
        let [cpu, memory] = pod_usage_queries(namespaces);
        let (cpu, memory) =
            futures::future::try_join(self.query(&cpu), self.query(&memory)).await?;
        Ok(pod_index(cpu, memory))
    }

    /// Current CPU and memory of every node: two queries.
    pub async fn node_usage(&self) -> Result<MetricsIndex, PrometheusError> {
        let [cpu, memory] = node_usage_queries();
        let (cpu, memory) =
            futures::future::try_join(self.query(&cpu), self.query(&memory)).await?;
        Ok(node_index(cpu, memory))
    }
}

#[cfg(test)]
mod tests {
    use kube::api::DynamicObject;
    use serde_json::json;

    use super::{node_index, node_usage_queries, parse_vector, pod_index, pod_usage_queries};
    use crate::metrics::Usage;
    use crate::metrics::prometheus::PrometheusError;

    fn vector(samples: serde_json::Value) -> String {
        json!({
            "status": "success",
            "data": { "resultType": "vector", "result": samples },
        })
        .to_string()
    }

    fn object(namespace: Option<&str>, name: &str) -> DynamicObject {
        serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": if namespace.is_some() { "Pod" } else { "Node" },
            "metadata": { "name": name, "namespace": namespace },
        }))
        .unwrap()
    }

    #[test]
    fn pod_queries_cover_the_visible_namespaces_at_once() {
        let [cpu, memory] = pod_usage_queries(Some(&["web".into(), "a.b".into()]));
        assert_eq!(
            cpu,
            r#"sum by (namespace, pod, container) (rate(container_cpu_usage_seconds_total{namespace=~"web|a\\.b",container!="",container!="POD",metrics_path!="/metrics/resource"}[2m]))"#
        );
        assert_eq!(
            memory,
            r#"sum by (namespace, pod, container) (container_memory_working_set_bytes{namespace=~"web|a\\.b",container!="",container!="POD",metrics_path!="/metrics/resource"})"#
        );
        let [all, _] = pod_usage_queries(None);
        assert!(all.contains(r#"{namespace!="",container!="""#));
    }

    #[test]
    fn node_queries_prefer_node_exporter_and_fall_back_to_the_root_cgroup() {
        let [cpu, memory] = node_usage_queries();
        assert!(cpu.starts_with(
            r#"sum by (node) (label_replace(rate(node_cpu_seconds_total{mode!~"idle|iowait|steal"}[2m]) * on (instance) group_left (nodename) node_uname_info, "node", "$1", "nodename", "(.+)"))"#
        ));
        assert!(cpu.contains(
            r#"or sum by (node) (rate(container_cpu_usage_seconds_total{id="/",node!=""}[2m]))"#
        ));
        assert!(memory.contains("node_memory_MemTotal_bytes - node_memory_MemAvailable_bytes"));
        assert!(memory.contains(r#"container_memory_working_set_bytes{id="/",instance!=""}"#));
    }

    #[test]
    fn pods_join_cpu_and_memory_in_metrics_server_units() {
        let cpu = parse_vector(&vector(json!([
            { "metric": { "namespace": "web", "pod": "api-1", "container": "app" }, "value": [1.0, "0.25"] },
            { "metric": { "namespace": "web", "pod": "api-1", "container": "sidecar" }, "value": [1.0, "0.0005"] },
            { "metric": { "namespace": "web", "pod": "api-2", "container": "app" }, "value": [1.0, "NaN"] },
            { "metric": { "pod": "no-namespace" }, "value": [1.0, "1"] },
        ])))
        .unwrap();
        let memory = parse_vector(&vector(json!([
            { "metric": { "namespace": "web", "pod": "api-1", "container": "app" }, "value": [1.0, "1048576"] },
            { "metric": { "namespace": "web", "pod": "api-2", "container": "app" }, "value": [1.0, "2048"] },
        ])))
        .unwrap();
        let index = pod_index(cpu, memory);
        assert_eq!(
            index.usage_of(&object(Some("web"), "api-1")),
            Some(Usage {
                cpu: 250_500_000,
                memory: 1 << 20
            })
        );
        assert_eq!(index.pod("web", "api-1").unwrap().containers.len(), 2);
        // NaN CPU leaves zero, memory still known.
        assert_eq!(
            index.usage_of(&object(Some("web"), "api-2")),
            Some(Usage {
                cpu: 0,
                memory: 2048
            })
        );
        assert_eq!(index.usage_of(&object(Some("web"), "gone")), None);
    }

    #[test]
    fn nodes_are_keyed_by_node_label() {
        let cpu = parse_vector(&vector(json!([
            { "metric": { "node": "n1" }, "value": [1.0, "1.5"] },
        ])))
        .unwrap();
        let memory = parse_vector(&vector(json!([
            { "metric": { "node": "n1" }, "value": [1.0, "2369867776"] },
            { "metric": {}, "value": [1.0, "1"] },
        ])))
        .unwrap();
        let index = node_index(cpu, memory);
        assert_eq!(
            index.usage_of(&object(None, "n1")),
            Some(Usage {
                cpu: 1_500_000_000,
                memory: 2_369_867_776
            })
        );
    }

    #[test]
    fn vector_errors_and_wrong_types_are_reported() {
        let error = json!({ "status": "error", "errorType": "bad_data", "error": "parse error" });
        assert!(matches!(
            parse_vector(&error.to_string()),
            Err(PrometheusError::Query { kind, .. }) if kind == "bad_data"
        ));
        let matrix =
            json!({ "status": "success", "data": { "resultType": "matrix", "result": [] } });
        assert!(matches!(
            parse_vector(&matrix.to_string()),
            Err(PrometheusError::Invalid(_))
        ));
    }
}
