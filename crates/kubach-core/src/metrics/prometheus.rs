//! Usage history from Prometheus (ADR 0009): finding the server of a
//! context, PromQL for CPU, memory, network and filesystem of Pods,
//! workloads (Deployments, StatefulSets, DaemonSets, ReplicaSets, Jobs) and
//! Nodes, and parsing `query_range` answers.
//!
//! In-cluster servers are reached through the API server's service proxy
//! with the context's credentials, so nothing is exposed and no port is
//! forwarded. A URL set by hand is requested directly from this machine.
//!
//! Pods and workloads use the cAdvisor series scraped from kubelets
//! (`container_cpu_usage_seconds_total`, `container_memory_working_set_bytes`,
//! `container_network_*_bytes_total`, `container_fs_*_bytes_total`), which
//! kube-prometheus-stack and the community `prometheus` chart both collect.
//! Nodes use node-exporter, else the cAdvisor root cgroup.
//!
//! [`usage`] has the instant queries behind the CPU / Memory columns of
//! tables when metrics-server is missing.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::{Stream, StreamExt as _};
use http_body_util::BodyExt as _;
use k8s_openapi::api::core::v1::Service;
use kube::api::{Api, ListParams};
use kube::{Client, Config};
use serde::Deserialize;

use super::{format_cpu, format_memory};

pub mod coverage;
pub mod overview;
pub mod source;
pub mod usage;

/// How many discovered services are probed (at once) at most.
pub const PROBE_CANDIDATES: usize = 8;

/// A server that does not answer a probe within this time is skipped.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// Namespaces where Prometheus is usually installed. Candidates found here
/// rank above all others whatever their score, so a look-alike Service in
/// some other namespace cannot take over the search (#200); a server found
/// only elsewhere needs the user's confirmation (see [`source::Plan`]).
/// Searched alone when services cannot be listed cluster-wide.
pub const WELL_KNOWN_NAMESPACES: [&str; 8] = [
    "monitoring",
    "prometheus",
    "kube-prometheus",
    "kube-prometheus-stack",
    "observability",
    "kube-system",
    "openshift-monitoring",
    "cattle-monitoring-system",
];

/// Whether `namespace` is one of [`WELL_KNOWN_NAMESPACES`].
pub fn well_known_namespace(namespace: &str) -> bool {
    WELL_KNOWN_NAMESPACES.contains(&namespace)
}

/// Largest response body read from Prometheus (#195): a bigger answer (or a
/// fake server streaming without end) fails instead of filling the memory.
/// Charts and table usage are a few hundred KiB even on large clusters.
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// History is refreshed this often while shown.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum PrometheusError {
    /// No Prometheus service found in the cluster and no URL configured.
    #[error("Prometheus not found in the cluster")]
    NotFound,
    #[error(
        "invalid Prometheus address {0:?}: expected http(s)://host[:port][/path] or namespace/service[:port]"
    )]
    InvalidAddress(String),
    #[error("Prometheus request failed: {0}")]
    Request(String),
    /// Prometheus answered with `"status": "error"`.
    #[error("Prometheus query failed ({kind}): {message}")]
    Query { kind: String, message: String },
    #[error("invalid Prometheus response: {0}")]
    Invalid(String),
}

/// Where a Prometheus server is reached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    /// A Service in the cluster, through the API server's proxy.
    Service {
        namespace: String,
        name: String,
        /// Port name or number.
        port: String,
        https: bool,
    },
    /// A URL requested directly from this machine.
    Url(String),
}

impl Endpoint {
    /// Parses an address set by hand: `http(s)://host[:port][/path]`, or
    /// `namespace/service[:port]` for a Service reached through the proxy
    /// (port 9090 when omitted).
    pub fn parse(address: &str) -> Result<Self, PrometheusError> {
        let address = address.trim();
        let invalid = || PrometheusError::InvalidAddress(address.to_owned());
        if address.starts_with("http://") || address.starts_with("https://") {
            let uri: http::Uri = address.parse().map_err(|_| invalid())?;
            if uri.host().is_none_or(str::is_empty) {
                return Err(invalid());
            }
            return Ok(Self::Url(address.trim_end_matches('/').to_owned()));
        }
        let (namespace, rest) = address.split_once('/').ok_or_else(invalid)?;
        let (name, port) = rest.split_once(':').unwrap_or((rest, "9090"));
        let label = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        };
        if !label(namespace) || !label(name) || !label(port) {
            return Err(invalid());
        }
        Ok(Self::Service {
            namespace: namespace.to_owned(),
            name: name.to_owned(),
            port: port.to_owned(),
            // As found by discovery: a port named `https` is TLS.
            https: port.contains("https"),
        })
    }

    /// Path prefix of the Prometheus HTTP API: the service proxy path, or
    /// empty for a URL (whose own path the client keeps).
    fn prefix(&self) -> String {
        match self {
            Self::Service {
                namespace,
                name,
                port,
                https,
            } => {
                let scheme = if *https { "https:" } else { "" };
                format!("/api/v1/namespaces/{namespace}/services/{scheme}{name}:{port}/proxy")
            }
            Self::Url(_) => String::new(),
        }
    }

    /// The namespace of a Service outside [`WELL_KNOWN_NAMESPACES`]; `None`
    /// for well-known namespaces and URLs.
    pub fn unusual_namespace(&self) -> Option<&str> {
        match self {
            Self::Service { namespace, .. } if !well_known_namespace(namespace) => Some(namespace),
            _ => None,
        }
    }

    /// `monitoring/prometheus-operated:web` or the URL, for display.
    pub fn describe(&self) -> String {
        match self {
            Self::Service {
                namespace,
                name,
                port,
                ..
            } => format!("{namespace}/{name}:{port}"),
            Self::Url(url) => url.clone(),
        }
    }
}

/// How likely a Service is the Prometheus server, `None` for not at all.
fn score(service: &Service) -> Option<u32> {
    let name = service.metadata.name.as_deref()?;
    let labels = service.metadata.labels.as_ref();
    let label = |key: &str| labels.and_then(|l| l.get(key)).map(String::as_str);
    const NOT_SERVER: [&str; 7] = [
        "alertmanager",
        "operator",
        "exporter",
        "pushgateway",
        "adapter",
        "blackbox",
        "thanos",
    ];
    if NOT_SERVER.iter().any(|part| name.contains(part)) {
        return None;
    }
    if label("app") == Some("kube-prometheus-stack-prometheus") {
        return Some(100);
    }
    match name {
        "prometheus-operated" => return Some(90),
        "prometheus-k8s" => return Some(80),
        "prometheus-server" => return Some(70),
        _ => {}
    }
    if label("app.kubernetes.io/name") == Some("prometheus")
        && label("app.kubernetes.io/component").is_none_or(|c| c == "server")
    {
        return Some(60);
    }
    (name == "prometheus").then_some(50)
}

/// The web port of a Prometheus Service: named `web`/`http-web`/`http`,
/// else 9090, else the first.
fn web_port(service: &Service) -> Option<(String, bool)> {
    let ports = service.spec.as_ref()?.ports.as_ref()?;
    let named = |wanted: &str| {
        ports
            .iter()
            .find(|p| p.name.as_deref() == Some(wanted))
            .map(|p| p.name.clone().unwrap_or_default())
    };
    let port = named("web")
        .or_else(|| named("http-web"))
        .or_else(|| named("http"))
        .or_else(|| named("https"))
        .or_else(|| {
            ports
                .iter()
                .find(|p| p.port == 9090)
                .map(|p| p.port.to_string())
        })
        .or_else(|| ports.first().map(|p| p.port.to_string()))?;
    let https = port.contains("https");
    Some((port, https))
}

/// Prometheus servers among `services`, most likely first: those in
/// [`WELL_KNOWN_NAMESPACES`] before all others whatever their score, then
/// by score.
pub fn candidates(services: &[Service]) -> Vec<Endpoint> {
    let mut found: Vec<(u32, Endpoint)> = services
        .iter()
        .filter_map(|service| {
            let score = score(service)?;
            let (port, https) = web_port(service)?;
            Some((
                score,
                Endpoint::Service {
                    namespace: service.metadata.namespace.clone()?,
                    name: service.metadata.name.clone()?,
                    port,
                    https,
                },
            ))
        })
        .collect();
    // Stable: equal keys keep the API's (namespace, name) order.
    found.sort_by_key(|(score, endpoint)| {
        (
            endpoint.unusual_namespace().is_some(),
            std::cmp::Reverse(*score),
        )
    });
    found.into_iter().map(|(_, endpoint)| endpoint).collect()
}

/// Controllers whose pods are charted together, matched by pod name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Workload {
    /// `<deployment>-<template hash>-<suffix>`.
    Deployment,
    /// `<statefulset>-<ordinal>`.
    StatefulSet,
    /// `<daemonset>-<suffix>`.
    DaemonSet,
    /// `<replicaset>-<suffix>`.
    ReplicaSet,
    /// `<job>-<suffix>`, or `<job>-<index>-<suffix>` for indexed jobs.
    Job,
}

impl Workload {
    /// RE2 pattern for the names of the pods of workload `name`.
    pub fn pod_pattern(self, name: &str) -> String {
        let name = regex_literal(name);
        match self {
            Self::Deployment => format!("{name}-[a-z0-9]+-[a-z0-9]+"),
            Self::StatefulSet => format!("{name}-[0-9]+"),
            Self::DaemonSet | Self::ReplicaSet => format!("{name}-[a-z0-9]{{5}}"),
            Self::Job => format!("{name}-([0-9]+-)?[a-z0-9]{{5}}"),
        }
    }
}

/// What a chart is about.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    Pod {
        namespace: String,
        name: String,
    },
    /// The pods of a workload, matched by name (see [`Workload`]): pods
    /// replaced long ago still count for their time.
    Workload {
        kind: Workload,
        namespace: String,
        name: String,
    },
    Node {
        name: String,
    },
}

impl Target {
    /// The target for an object of `kind` (core Pods and Nodes, `apps`
    /// workloads, Jobs), else `None`.
    pub fn of(group: &str, kind: &str, namespace: Option<&str>, name: &str) -> Option<Self> {
        let name = name.to_owned();
        let workload = |kind: Workload| {
            namespace.map(|namespace| Self::Workload {
                kind,
                namespace: namespace.to_owned(),
                name: name.clone(),
            })
        };
        match (group, kind) {
            ("", "Pod") => namespace.map(|namespace| Self::Pod {
                namespace: namespace.to_owned(),
                name: name.clone(),
            }),
            ("apps", "Deployment") => workload(Workload::Deployment),
            ("apps", "StatefulSet") => workload(Workload::StatefulSet),
            ("apps", "DaemonSet") => workload(Workload::DaemonSet),
            ("apps", "ReplicaSet") => workload(Workload::ReplicaSet),
            ("batch", "Job") => workload(Workload::Job),
            ("", "Node") => Some(Self::Node { name: name.clone() }),
            _ => None,
        }
    }

    /// Label the per-part chart splits by: containers of a pod, pods of a
    /// workload; nodes are not split.
    pub fn split_label(&self) -> Option<&'static str> {
        match self {
            Self::Pod { .. } => Some("container"),
            Self::Workload { .. } => Some("pod"),
            Self::Node { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Range {
    FifteenMinutes,
    #[default]
    Hour,
    SixHours,
    Day,
    Week,
}

impl Range {
    /// Shortest first: [`coverage`] relies on the order.
    pub const ALL: [Range; 5] = [
        Range::FifteenMinutes,
        Range::Hour,
        Range::SixHours,
        Range::Day,
        Range::Week,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::FifteenMinutes => "15m",
            Self::Hour => "1h",
            Self::SixHours => "6h",
            Self::Day => "24h",
            Self::Week => "7d",
        }
    }

    /// Length of the range in seconds.
    pub fn seconds(self) -> u64 {
        match self {
            Self::FifteenMinutes => 15 * 60,
            Self::Hour => 3600,
            Self::SixHours => 6 * 3600,
            Self::Day => 24 * 3600,
            Self::Week => 7 * 24 * 3600,
        }
    }

    /// Resolution: 120 points per range.
    pub fn step(self) -> u64 {
        self.seconds() / 120
    }

    /// `rate()` window: at least two steps and two minutes, so each point
    /// covers several scrapes and no samples fall between points.
    fn rate_window(self) -> String {
        format!("{}s", (self.step() * 2).max(120))
    }
}

/// Start, end and step of a `query_range`, in Unix seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub start: u64,
    pub end: u64,
    pub step: u64,
}

impl Window {
    /// `range` ending at `now`, aligned to the step so that refreshes reuse
    /// the same points.
    pub fn ending_at(range: Range, now: u64) -> Self {
        let step = range.step();
        let end = now - now % step;
        Self {
            start: end.saturating_sub(range.seconds()),
            end,
            step,
        }
    }

    pub fn now(range: Range) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        Self::ending_at(range, now)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Metric {
    /// Cores.
    Cpu,
    /// Bytes.
    Memory,
    /// Bytes per second, received and transmitted.
    Network,
    /// Bytes per second, read from and written to disk.
    Filesystem,
}

impl Metric {
    pub const ALL: [Metric; 4] = [
        Metric::Cpu,
        Metric::Memory,
        Metric::Network,
        Metric::Filesystem,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Memory => "Memory",
            Self::Network => "Network",
            Self::Filesystem => "Filesystem",
        }
    }

    /// A value of this metric for display: `250m`, `512Mi`, `1.2Ki/s`.
    pub fn format(self, value: f64) -> String {
        let value = value.max(0.0);
        match self {
            Self::Cpu => format_cpu((value * 1e9).round() as u64),
            Self::Memory => format_memory(value.round() as u64),
            Self::Network | Self::Filesystem => format!("{}/s", format_bytes(value)),
        }
    }

    /// Names of the lines of this metric's chart.
    pub fn lines(self) -> &'static [&'static str] {
        match self {
            Self::Cpu => &["CPU"],
            Self::Memory => &["Working set"],
            Self::Network => &["Receive", "Transmit"],
            Self::Filesystem => &["Read", "Write"],
        }
    }
}

/// Bytes with a binary unit and up to one decimal: `512B`, `1.5Ki`, `20Mi`.
pub fn format_bytes(bytes: f64) -> String {
    const UNITS: [&str; 6] = ["B", "Ki", "Mi", "Gi", "Ti", "Pi"];
    let mut value = bytes.max(0.0);
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 || value >= 100.0 {
        format!("{value:.0}{}", UNITS[unit])
    } else {
        let text = format!("{value:.1}");
        format!("{}{}", text.trim_end_matches(".0"), UNITS[unit])
    }
}

/// One PromQL expression of a chart and the name of its line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeriesQuery {
    pub name: &'static str,
    pub promql: String,
}

/// `value` as a PromQL string literal body.
fn quote(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// `value` matched literally inside a PromQL regex (RE2).
fn regex_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if r"\.+*?()|[]{}^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Host interfaces left out of node network: loopback and the virtual
/// interfaces of pods and bridges, whose traffic the physical ones carry.
const VIRTUAL_DEVICES: &str = "lo|veth.*|cali.*|cni.*|flannel.*|cilium.*|lxc.*|docker.*|br-.*|virbr.*|vxlan.*|tunl.*|kube-ipvs.*";

fn sum(series: &str, selector: &str, window: Option<&str>) -> String {
    sum_by(series, selector, window, None)
}

/// `sum [by (label)] ([rate(] series{selector} [[window])])`.
fn sum_by(series: &str, selector: &str, window: Option<&str>, by: Option<&str>) -> String {
    let by = by.map(|label| format!(" by ({label})")).unwrap_or_default();
    match window {
        Some(window) => format!("sum{by}(rate({series}{{{selector}}}[{window}]))"),
        None => format!("sum{by}({series}{{{selector}}})"),
    }
}

/// CPU and memory of pods: container cgroups only. The pod cgroup and the
/// sandbox (`container=""`, or `POD` with dockershim) would count everything
/// again, and the kubelet's `/metrics/resource` endpoint repeats the same
/// series names when it is scraped too.
fn container_selector(pods: &str) -> String {
    format!(r#"{pods},container!="",container!="POD",metrics_path!="/metrics/resource""#)
}

/// One node: node-exporter first (kube-prometheus-stack drops the kubelet's
/// root cgroup), matched to the node by `node_uname_info{nodename}`; then the
/// cAdvisor root cgroup (`id="/"`), labelled `node` by kube-prometheus-stack
/// and `instance` by the community chart.
fn node_query(metric: Metric, line: usize, node: &str, window: &str) -> String {
    let on_node = format!(r#"and on (instance) node_uname_info{{nodename="{node}"}}"#);
    let direction = if line == 0 { "receive" } else { "transmit" };
    let (disk, fs) = if line == 0 {
        ("read", "reads")
    } else {
        ("written", "writes")
    };
    let (exporter, cadvisor, rate) = match metric {
        Metric::Filesystem => (
            format!("sum(rate(node_disk_{disk}_bytes_total[{window}]) {on_node})"),
            format!("container_fs_{fs}_bytes_total"),
            true,
        ),
        Metric::Cpu => (
            format!(
                r#"sum(rate(node_cpu_seconds_total{{mode!~"idle|iowait|steal"}}[{window}]) {on_node})"#
            ),
            "container_cpu_usage_seconds_total".to_owned(),
            true,
        ),
        Metric::Memory => (
            format!("sum((node_memory_MemTotal_bytes - node_memory_MemAvailable_bytes) {on_node})"),
            "container_memory_working_set_bytes".to_owned(),
            false,
        ),
        Metric::Network => (
            format!(
                r#"sum(rate(node_network_{direction}_bytes_total{{device!~"{VIRTUAL_DEVICES}"}}[{window}]) {on_node})"#
            ),
            format!("container_network_{direction}_bytes_total"),
            true,
        ),
    };
    let window = rate.then_some(window);
    let root = |label: &str| sum(&cadvisor, &format!(r#"id="/",{label}="{node}""#), window);
    format!("{exporter} or {} or {}", root("node"), root("instance"))
}

/// Label matchers for the pods of `target`; `None` for nodes.
fn pod_selector(target: &Target) -> Option<String> {
    match target {
        Target::Pod { namespace, name } => Some(format!(
            r#"namespace="{}",pod="{}""#,
            quote(namespace),
            quote(name)
        )),
        Target::Workload {
            kind,
            namespace,
            name,
        } => Some(format!(
            r#"namespace="{}",pod=~"{}""#,
            quote(namespace),
            quote(&kind.pod_pattern(name))
        )),
        Target::Node { .. } => None,
    }
}

/// PromQL of line `line` of `metric` over the pods in `pods`, summed by
/// `by` when set.
fn pod_query(metric: Metric, line: usize, pods: &str, window: &str, by: Option<&str>) -> String {
    match metric {
        Metric::Cpu => sum_by(
            "container_cpu_usage_seconds_total",
            &container_selector(pods),
            Some(window),
            by,
        ),
        Metric::Memory => sum_by(
            "container_memory_working_set_bytes",
            &container_selector(pods),
            None,
            by,
        ),
        // Network is accounted on the pod sandbox (one series per
        // interface), so no container filter.
        Metric::Network => {
            let direction = if line == 0 { "receive" } else { "transmit" };
            sum_by(
                &format!("container_network_{direction}_bytes_total"),
                pods,
                Some(window),
                by,
            )
        }
        Metric::Filesystem => {
            let direction = if line == 0 { "reads" } else { "writes" };
            sum_by(
                &format!("container_fs_{direction}_bytes_total"),
                &container_selector(pods),
                Some(window),
                by,
            )
        }
    }
}

/// The PromQL lines of `metric` for `target`.
pub fn queries(target: &Target, metric: Metric, range: Range) -> Vec<SeriesQuery> {
    let window = range.rate_window();
    let names = metric.lines();
    let Some(pods) = pod_selector(target) else {
        let Target::Node { name } = target else {
            return Vec::new();
        };
        return names
            .iter()
            .enumerate()
            .map(|(line, &series)| SeriesQuery {
                name: series,
                promql: node_query(metric, line, &quote(name), &window),
            })
            .collect();
    };
    names
        .iter()
        .enumerate()
        .map(|(line, &name)| SeriesQuery {
            name,
            promql: pod_query(metric, line, &pods, &window, None),
        })
        .collect()
}

/// PromQL for `metric` per part of `target` (containers of a pod, pods of
/// a workload), with the label naming the part. Only CPU and memory are
/// split: network belongs to the pod, not its containers.
pub fn split_query(
    target: &Target,
    metric: Metric,
    range: Range,
) -> Option<(&'static str, String)> {
    if !matches!(metric, Metric::Cpu | Metric::Memory) {
        return None;
    }
    let label = target.split_label()?;
    let pods = pod_selector(target)?;
    let promql = pod_query(metric, 0, &pods, &range.rate_window(), Some(label));
    Some((label, promql))
}

/// A sample: Unix seconds and value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub time: f64,
    pub value: f64,
}

/// One line of a chart.
#[derive(Clone, Debug, PartialEq)]
pub struct Series {
    pub name: String,
    pub points: Vec<Point>,
}

impl Series {
    pub fn latest(&self) -> Option<f64> {
        self.points.last().map(|p| p.value)
    }
}

/// The lines of one metric over a window.
#[derive(Clone, Debug, PartialEq)]
pub struct Chart {
    pub metric: Metric,
    pub window: Window,
    /// Lines with at least one point.
    pub series: Vec<Series>,
}

impl Chart {
    pub fn is_empty(&self) -> bool {
        self.series.is_empty()
    }
}

#[derive(Deserialize)]
struct RawResponse {
    status: String,
    #[serde(default)]
    data: Option<RawData>,
    #[serde(default, rename = "errorType")]
    error_type: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
struct RawData {
    #[serde(rename = "resultType")]
    result_type: String,
    result: serde_json::Value,
}

#[derive(Deserialize)]
struct RawMatrix {
    #[serde(default)]
    metric: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    values: Vec<(f64, String)>,
}

/// Parses a `query_range` answer into its series' points (labels are not
/// needed: every query sums to one series). `NaN` and infinite values are
/// dropped, leaving gaps.
pub fn parse_query_range(body: &str) -> Result<Vec<Vec<Point>>, PrometheusError> {
    Ok(parse_matrix(body)?
        .into_iter()
        .map(|(_, points)| points)
        .collect())
}

/// Like [`parse_query_range`], with each series named by its `label`
/// (empty when missing), sorted by name.
pub fn parse_query_range_by(
    body: &str,
    label: &str,
) -> Result<Vec<(String, Vec<Point>)>, PrometheusError> {
    let mut series: Vec<_> = parse_matrix(body)?
        .into_iter()
        .map(|(mut labels, points)| (labels.remove(label).unwrap_or_default(), points))
        .filter(|(_, points)| !points.is_empty())
        .collect();
    series.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(series)
}

type Labels = std::collections::BTreeMap<String, String>;

fn parse_matrix(body: &str) -> Result<Vec<(Labels, Vec<Point>)>, PrometheusError> {
    let invalid = |e: &dyn std::fmt::Display| PrometheusError::Invalid(e.to_string());
    let raw: RawResponse = serde_json::from_str(body).map_err(|e| invalid(&e))?;
    if raw.status != "success" {
        return Err(PrometheusError::Query {
            kind: raw.error_type.unwrap_or_else(|| "unknown".into()),
            message: raw.error.unwrap_or_default(),
        });
    }
    let data = raw.data.ok_or_else(|| invalid(&"no data"))?;
    if data.result_type != "matrix" {
        return Err(invalid(&format!(
            "expected a matrix, got {}",
            data.result_type
        )));
    }
    let matrix: Vec<RawMatrix> = serde_json::from_value(data.result).map_err(|e| invalid(&e))?;
    matrix
        .into_iter()
        .map(|series| {
            let points = series
                .values
                .into_iter()
                .filter_map(|(time, value)| match value.parse::<f64>() {
                    Ok(value) if value.is_finite() => Some(Ok(Point { time, value })),
                    Ok(_) => None,
                    Err(_) => Some(Err(invalid(&format!("bad sample value {value:?}")))),
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok((series.metric, points))
        })
        .collect()
}

/// Percent-encodes a query string value.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len() * 3);
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Path and query of a `query_range` request, after the endpoint prefix.
pub fn query_range_path(promql: &str, window: Window) -> String {
    format!(
        "/api/v1/query_range?query={}&start={}&end={}&step={}",
        encode(promql),
        window.start,
        window.end,
        window.step
    )
}

/// A Prometheus server of one context. Cheap to clone.
#[derive(Clone)]
pub struct Prometheus {
    client: Client,
    endpoint: Endpoint,
}

impl std::fmt::Debug for Prometheus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prometheus")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl Prometheus {
    /// A client for `endpoint`; `cluster` is used for in-cluster services.
    /// Must run on the I/O runtime (a URL gets a client of its own).
    pub fn new(cluster: &Client, endpoint: Endpoint) -> Result<Self, PrometheusError> {
        let client = match &endpoint {
            Endpoint::Service { .. } => cluster.clone(),
            Endpoint::Url(url) => {
                let uri: http::Uri = url
                    .parse()
                    .map_err(|_| PrometheusError::InvalidAddress(url.clone()))?;
                Client::try_from(Config::new(uri))
                    .map_err(|e| PrometheusError::Request(e.to_string()))?
            }
        };
        Ok(Self { client, endpoint })
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// A client for `endpoint` that answered a probe within
    /// [`PROBE_TIMEOUT`]. Must run on the I/O runtime.
    pub async fn connect(cluster: &Client, endpoint: Endpoint) -> Result<Self, PrometheusError> {
        let prometheus = Self::new(cluster, endpoint)?;
        prometheus.probe_in_time().await?;
        Ok(prometheus)
    }

    /// Looks for Prometheus Services in all namespaces, or in
    /// [`WELL_KNOWN_NAMESPACES`] when listing all is forbidden, probes the
    /// first [`PROBE_CANDIDATES`] at once and returns every one that
    /// answered within [`PROBE_TIMEOUT`], most likely first.
    /// [`PrometheusError::NotFound`] when none did; other errors when the
    /// cluster could not be searched at all. Must run on the I/O runtime.
    pub async fn discover(cluster: &Client) -> Result<Vec<Self>, PrometheusError> {
        let params = ListParams::default();
        let services = match Api::<Service>::all(cluster.clone()).list(&params).await {
            Ok(list) => list.items,
            Err(error) => {
                let mut services = Vec::new();
                let mut listed = false;
                for namespace in WELL_KNOWN_NAMESPACES {
                    let api = Api::<Service>::namespaced(cluster.clone(), namespace);
                    if let Ok(list) = api.list(&params).await {
                        listed = true;
                        services.extend(list.items);
                    }
                }
                if !listed {
                    return Err(PrometheusError::Request(error.to_string()));
                }
                services
            }
        };
        let probes = candidates(&services)
            .into_iter()
            .take(PROBE_CANDIDATES)
            .map(|endpoint| async move {
                let prometheus = Self::new(cluster, endpoint).ok()?;
                prometheus.probe_in_time().await.ok()?;
                Some(prometheus)
            });
        let found: Vec<Self> = futures::future::join_all(probes)
            .await
            .into_iter()
            .flatten()
            .collect();
        if found.is_empty() {
            Err(PrometheusError::NotFound)
        } else {
            Ok(found)
        }
    }

    /// [`probe`](Self::probe) within [`PROBE_TIMEOUT`].
    async fn probe_in_time(&self) -> Result<(), PrometheusError> {
        tokio::time::timeout(PROBE_TIMEOUT, self.probe())
            .await
            .map_err(|_| PrometheusError::Request("no answer in time".to_owned()))?
    }

    async fn get(&self, path_and_query: &str) -> Result<String, PrometheusError> {
        self.get_limited(path_and_query, MAX_RESPONSE_BYTES).await
    }

    /// Like `get`, failing on a body larger than `limit` bytes.
    async fn get_limited(
        &self,
        path_and_query: &str,
        limit: usize,
    ) -> Result<String, PrometheusError> {
        let uri = format!("{}{path_and_query}", self.endpoint.prefix());
        let request = http::Request::get(uri)
            .body(kube::client::Body::from(Vec::new()))
            .map_err(|e| PrometheusError::Request(e.to_string()))?;
        // Not `request_text`: it reads the whole body (also of an error
        // answer) into memory, whatever its size.
        let response = self
            .client
            .send(request)
            .await
            .map_err(|error| PrometheusError::Request(error.to_string()))?;
        let status = response.status();
        let length = response
            .headers()
            .get(http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok()?.parse::<u64>().ok());
        if length.is_some_and(|length| length > limit as u64) {
            return Err(too_large(limit));
        }
        let body = read_limited(response.into_body().into_data_stream(), limit).await?;
        let text = String::from_utf8(body)
            .map_err(|_| PrometheusError::Invalid("response is not UTF-8".to_owned()))?;
        if status.is_client_error() || status.is_server_error() {
            return Err(error_answer(status.as_u16(), &text));
        }
        Ok(text)
    }

    /// Checks that the endpoint is a Prometheus API.
    pub async fn probe(&self) -> Result<(), PrometheusError> {
        let body = self.get("/api/v1/query?query=1").await?;
        #[derive(Deserialize)]
        struct Status {
            status: String,
        }
        match serde_json::from_str::<Status>(&body) {
            Ok(status) if status.status == "success" => Ok(()),
            _ => Err(PrometheusError::Invalid(
                "not a Prometheus HTTP API".to_owned(),
            )),
        }
    }

    pub async fn query_range(
        &self,
        promql: &str,
        window: Window,
    ) -> Result<Vec<Vec<Point>>, PrometheusError> {
        let body = self.get(&query_range_path(promql, window)).await?;
        parse_query_range(&body)
    }

    /// One chart of `target` over `window`.
    pub async fn chart(
        &self,
        target: &Target,
        metric: Metric,
        range: Range,
        window: Window,
    ) -> Result<Chart, PrometheusError> {
        let mut series = Vec::new();
        for query in queries(target, metric, range) {
            let points = self
                .query_range(&query.promql, window)
                .await?
                .into_iter()
                .next()
                .unwrap_or_default();
            if !points.is_empty() {
                series.push(Series {
                    name: query.name.to_owned(),
                    points,
                });
            }
        }
        Ok(Chart {
            metric,
            window,
            series,
        })
    }

    /// CPU, memory, network and filesystem charts of `target` over the
    /// last `range`.
    pub async fn charts(
        &self,
        target: &Target,
        range: Range,
    ) -> Result<Vec<Chart>, PrometheusError> {
        let window = Window::now(range);
        let charts = Metric::ALL.map(|metric| self.chart(target, metric, range, window));
        futures::future::try_join_all(charts).await
    }

    /// `metric` of `target` per container or pod (see [`split_query`]);
    /// `None` when the metric is not split for this target.
    pub async fn split_chart(
        &self,
        target: &Target,
        metric: Metric,
        range: Range,
        window: Window,
    ) -> Result<Option<Chart>, PrometheusError> {
        let Some((label, promql)) = split_query(target, metric, range) else {
            return Ok(None);
        };
        let body = self.get(&query_range_path(&promql, window)).await?;
        let series = parse_query_range_by(&body, label)?
            .into_iter()
            .map(|(name, points)| Series { name, points })
            .collect();
        Ok(Some(Chart {
            metric,
            window,
            series,
        }))
    }

    /// Everything the details panel charts: one chart per metric (in
    /// [`Metric::ALL`] order) and the split charts there are. A failing
    /// split query leaves its chart out rather than failing the rest.
    pub async fn panel(
        &self,
        target: &Target,
        range: Range,
    ) -> Result<PanelCharts, PrometheusError> {
        let window = Window::now(range);
        let totals = futures::future::try_join_all(
            Metric::ALL.map(|metric| self.chart(target, metric, range, window)),
        );
        let splits = futures::future::join_all(
            Metric::ALL.map(|metric| self.split_chart(target, metric, range, window)),
        );
        let (totals, splits) = futures::future::join(totals, splits).await;
        Ok(PanelCharts {
            totals: totals?,
            splits: splits
                .into_iter()
                .filter_map(|split| split.ok().flatten())
                .collect(),
        })
    }
}

fn too_large(limit: usize) -> PrometheusError {
    if limit >= 1024 * 1024 {
        PrometheusError::Request(format!(
            "response larger than {} MiB",
            limit / (1024 * 1024)
        ))
    } else {
        PrometheusError::Request(format!("response larger than {} KiB", limit / 1024))
    }
}

/// Collects `body` up to `limit` bytes; a longer body fails as soon as it
/// passes the limit, without reading the rest.
async fn read_limited<S, B, E>(body: S, limit: usize) -> Result<Vec<u8>, PrometheusError>
where
    S: Stream<Item = Result<B, E>>,
    B: AsRef<[u8]>,
    E: std::fmt::Display,
{
    let mut body = std::pin::pin!(body);
    let mut bytes = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|e| PrometheusError::Request(e.to_string()))?;
        let chunk = chunk.as_ref();
        if bytes.len() + chunk.len() > limit {
            return Err(too_large(limit));
        }
        bytes.extend_from_slice(chunk);
    }
    Ok(bytes)
}

/// The error for a non-2xx answer with `body`: Prometheus' own JSON error, a
/// Kubernetes `Status` (from the API server's proxy), or the body as is.
fn error_answer(code: u16, body: &str) -> PrometheusError {
    #[derive(Deserialize)]
    struct Status {
        kind: Option<String>,
        message: Option<String>,
    }
    if let Ok(Status {
        kind: Some(kind),
        message,
    }) = serde_json::from_str::<Status>(body)
        && kind == "Status"
    {
        let message = message.unwrap_or_default();
        return PrometheusError::Request(format!("{code} {message}"));
    }
    if let Err(query @ PrometheusError::Query { .. }) = parse_query_range(body) {
        return query;
    }
    PrometheusError::Request(format!("{code} {}", body.trim()))
}

/// Charts of the details panel, see [`Prometheus::panel`].
#[derive(Clone, Debug, PartialEq)]
pub struct PanelCharts {
    pub totals: Vec<Chart>,
    pub splits: Vec<Chart>,
}

#[cfg(test)]
mod tests;
