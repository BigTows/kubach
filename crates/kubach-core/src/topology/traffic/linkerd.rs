//! Linkerd: the proxies' `response_total`, `response_latency_ms` and
//! `tcp_{read,write}_bytes_total`, outbound side (the caller's proxy knows
//! both ends; `dst_service` names the Service called).

use super::{
    FlowKey, NamespaceScope, Peer, TrafficMetric, TrafficPlugin, TrafficQuery, TrafficWindow,
};
use crate::metrics::prometheus::usage::Labels;

pub struct Linkerd;

/// Workload labels of the source (the proxy's own pod) and the
/// destination: whichever controller the pod belongs to is set.
const BY: &str = "namespace, deployment, statefulset, daemonset, dst_namespace, dst_deployment, dst_statefulset, dst_daemonset, dst_service";

/// The first of `keys` that is set.
fn first<'a>(labels: &'a Labels, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .filter_map(|key| labels.get(*key))
        .map(String::as_str)
        .find(|value| !value.is_empty())
}

impl TrafficPlugin for Linkerd {
    fn id(&self) -> &'static str {
        "linkerd"
    }

    fn name(&self) -> &'static str {
        "Linkerd"
    }

    fn api_groups(&self) -> &'static [&'static str] {
        &["linkerd.io", "policy.linkerd.io"]
    }

    fn probe(&self) -> &'static str {
        r#"count(response_total{direction="outbound",dst_namespace!=""})"#
    }

    fn queries(&self, scope: &NamespaceScope, window: TrafficWindow) -> Vec<TrafficQuery> {
        let range = window.range();
        let sum = |series: &str, filter: &str| {
            scope.either("dst_namespace", "namespace", |ns| {
                format!(
                    r#"sum by ({BY}) (rate({series}{{direction="outbound"{filter}{ns}}}[{range}]))"#
                )
            })
        };
        let buckets = scope.either("dst_namespace", "namespace", |ns| {
            format!(
                r#"sum by (le, {BY}) (rate(response_latency_ms_bucket{{direction="outbound"{ns}}}[{range}]))"#
            )
        });
        vec![
            TrafficQuery {
                metric: TrafficMetric::Requests,
                promql: sum("response_total", ""),
            },
            TrafficQuery {
                metric: TrafficMetric::Errors,
                promql: sum("response_total", r#",classification="failure""#),
            },
            TrafficQuery {
                metric: TrafficMetric::LatencyP95,
                promql: format!("histogram_quantile(0.95, {buckets})"),
            },
            // One query per direction (see Istio); they add up in `parse`.
            TrafficQuery {
                metric: TrafficMetric::Bytes,
                promql: sum("tcp_read_bytes_total", r#",peer="dst""#),
            },
            TrafficQuery {
                metric: TrafficMetric::Bytes,
                promql: sum("tcp_write_bytes_total", r#",peer="dst""#),
            },
        ]
    }

    fn flow(&self, labels: &Labels) -> Option<FlowKey> {
        let destination_namespace = first(labels, &["dst_namespace"])?;
        let destination = first(
            labels,
            &["dst_deployment", "dst_statefulset", "dst_daemonset"],
        )?;
        let source = match (
            first(labels, &["namespace"]),
            first(labels, &["deployment", "statefulset", "daemonset"]),
        ) {
            (Some(namespace), Some(name)) => Some(Peer::new(namespace, name)),
            _ => None,
        };
        // `dst_service` is the Service's name, or its FQDN in older proxies.
        let service = first(labels, &["dst_service"]).map(|service| {
            let name = service.split('.').next().unwrap_or(service);
            Peer::new(destination_namespace, name)
        });
        Some(FlowKey {
            source,
            destination: Peer::new(destination_namespace, destination),
            service,
        })
    }
}
