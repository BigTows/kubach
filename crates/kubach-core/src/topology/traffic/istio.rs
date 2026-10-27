//! Istio: the standard metrics of the sidecars (or ztunnel/waypoints),
//! `istio_requests_total`, `istio_request_duration_milliseconds` and
//! `istio_tcp_{sent,received}_bytes_total`, as reported by the destination
//! (it sees callers from outside the mesh too, as `unknown`).

use super::{
    FlowKey, NamespaceScope, Peer, TrafficMetric, TrafficPlugin, TrafficQuery, TrafficWindow,
};
use crate::metrics::prometheus::usage::Labels;

pub struct Istio;

/// Labels every query sums by: one series per pair of workloads (and the
/// Service called).
const BY: &str = "source_workload_namespace, source_workload, destination_workload_namespace, destination_workload, destination_service_namespace, destination_service_name";

impl TrafficPlugin for Istio {
    fn id(&self) -> &'static str {
        "istio"
    }

    fn name(&self) -> &'static str {
        "Istio"
    }

    fn api_groups(&self) -> &'static [&'static str] {
        &[
            "networking.istio.io",
            "security.istio.io",
            "telemetry.istio.io",
        ]
    }

    fn probe(&self) -> &'static str {
        r#"count(istio_requests_total{reporter="destination"}) or count(istio_tcp_sent_bytes_total{reporter="destination"})"#
    }

    fn queries(&self, scope: &NamespaceScope, window: TrafficWindow) -> Vec<TrafficQuery> {
        let range = window.range();
        let sum = |series: &str, filter: &str| {
            scope.either(
                "destination_workload_namespace",
                "source_workload_namespace",
                |ns| {
                    format!(
                        r#"sum by ({BY}) (rate({series}{{reporter="destination"{filter}{ns}}}[{range}]))"#
                    )
                },
            )
        };
        let buckets = scope.either(
            "destination_workload_namespace",
            "source_workload_namespace",
            |ns| {
                format!(
                    r#"sum by (le, {BY}) (rate(istio_request_duration_milliseconds_bucket{{reporter="destination"{ns}}}[{range}]))"#
                )
            },
        );
        vec![
            TrafficQuery {
                metric: TrafficMetric::Requests,
                promql: sum("istio_requests_total", ""),
            },
            TrafficQuery {
                metric: TrafficMetric::Errors,
                promql: sum("istio_requests_total", r#",response_code=~"5..""#),
            },
            TrafficQuery {
                metric: TrafficMetric::LatencyP95,
                promql: format!("histogram_quantile(0.95, {buckets})"),
            },
            // One query per direction: `rate()` of both names at once would
            // drop the name and collide; the two answers add up in `parse`.
            TrafficQuery {
                metric: TrafficMetric::Bytes,
                promql: sum("istio_tcp_sent_bytes_total", ""),
            },
            TrafficQuery {
                metric: TrafficMetric::Bytes,
                promql: sum("istio_tcp_received_bytes_total", ""),
            },
        ]
    }

    fn flow(&self, labels: &Labels) -> Option<FlowKey> {
        Some(FlowKey {
            source: Peer::from_labels(labels, "source_workload_namespace", "source_workload"),
            destination: Peer::from_labels(
                labels,
                "destination_workload_namespace",
                "destination_workload",
            )?,
            service: Peer::from_labels(
                labels,
                "destination_service_namespace",
                "destination_service_name",
            ),
        })
    }
}
