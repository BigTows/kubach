//! Cilium Hubble: `hubble_http_requests_total`,
//! `hubble_http_request_duration_seconds` (L7, where an HTTP policy or
//! visibility makes Cilium parse HTTP) and `hubble_flows_processed_total`
//! (every L3/L4 flow, forwarded or dropped). Workload names need the
//! metrics' `labelsContext` with
//! `source_namespace,source_workload,destination_namespace,destination_workload`;
//! the probe checks for them.

use super::{
    FlowKey, NamespaceScope, Peer, TrafficMetric, TrafficPlugin, TrafficQuery, TrafficWindow,
};
use crate::metrics::prometheus::usage::Labels;

pub struct Hubble;

const BY: &str = "source_namespace, source_workload, destination_namespace, destination_workload";

impl TrafficPlugin for Hubble {
    fn id(&self) -> &'static str {
        "hubble"
    }

    fn name(&self) -> &'static str {
        "Cilium Hubble"
    }

    fn api_groups(&self) -> &'static [&'static str] {
        &["cilium.io"]
    }

    fn probe(&self) -> &'static str {
        r#"count(hubble_flows_processed_total{destination_workload!=""}) or count(hubble_http_requests_total{destination_workload!=""})"#
    }

    fn queries(&self, scope: &NamespaceScope, window: TrafficWindow) -> Vec<TrafficQuery> {
        let range = window.range();
        let sum = |series: &str, filter: &str| {
            scope.either("destination_namespace", "source_namespace", |ns| {
                format!(
                    r#"sum by ({BY}) (rate({series}{{destination_workload!=""{filter}{ns}}}[{range}]))"#
                )
            })
        };
        let buckets = scope.either("destination_namespace", "source_namespace", |ns| {
            format!(
                r#"sum by (le, {BY}) (rate(hubble_http_request_duration_seconds_bucket{{destination_workload!=""{ns}}}[{range}]))"#
            )
        });
        vec![
            TrafficQuery {
                metric: TrafficMetric::Requests,
                promql: sum("hubble_http_requests_total", ""),
            },
            TrafficQuery {
                metric: TrafficMetric::Errors,
                promql: sum("hubble_http_requests_total", r#",status=~"5..""#),
            },
            TrafficQuery {
                metric: TrafficMetric::LatencyP95,
                promql: format!("histogram_quantile(0.95, {buckets}) * 1000"),
            },
            TrafficQuery {
                metric: TrafficMetric::Flows,
                promql: sum("hubble_flows_processed_total", r#",verdict="FORWARDED""#),
            },
            TrafficQuery {
                metric: TrafficMetric::Dropped,
                promql: sum("hubble_flows_processed_total", r#",verdict="DROPPED""#),
            },
        ]
    }

    fn flow(&self, labels: &Labels) -> Option<FlowKey> {
        Some(FlowKey {
            // No workload: the host, the world or a pod without controller.
            source: Peer::from_labels(labels, "source_namespace", "source_workload"),
            destination: Peer::from_labels(
                labels,
                "destination_namespace",
                "destination_workload",
            )?,
            service: None,
        })
    }
}
