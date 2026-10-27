//! PromQL of the Overview screen (#414): CPU and memory of the pods in the
//! selected namespaces over a range (the card's own, #481), pods by phase from kube-state-metrics
//! (`kube_pod_status_phase`), restarts within the range
//! (`kube_pod_container_status_restarts_total`) and the current usage when
//! metrics-server is missing.
//!
//! CPU and memory use the same cAdvisor series and container filter as the
//! details' charts and the tables (ADR 0009). The ranges are those of the
//! details, offered by the oldest memory sample of the scope
//! ([`oldest_query`], see [`coverage`](super::coverage)).

use super::usage::{self, USAGE_RATE_WINDOW, namespace_matcher};
use super::{
    Chart, Metric, Prometheus, PrometheusError, Range, Series, Window, container_selector,
    parse_query_range_by, query_range_path,
};
use crate::metrics::Usage;
use crate::metrics::overview::{PhaseChart, PodPhase};

/// CPU (cores) of the pods in `namespaces` (all when `None`), `rate` over
/// `window`.
pub fn cpu_query(namespaces: Option<&[String]>, window: &str) -> String {
    let selector = container_selector(&namespace_matcher(namespaces));
    format!("sum(rate(container_cpu_usage_seconds_total{{{selector}}}[{window}]))")
}

/// Working set (bytes) of the pods in `namespaces`.
pub fn memory_query(namespaces: Option<&[String]>) -> String {
    let selector = container_selector(&namespace_matcher(namespaces));
    format!("sum(container_memory_working_set_bytes{{{selector}}})")
}

/// Pods per phase in `namespaces`, labelled `phase` (kube-state-metrics:
/// one series per pod and phase, 1 for the pod's phase).
pub fn phase_query(namespaces: Option<&[String]>) -> String {
    format!(
        "sum by (phase) (kube_pod_status_phase{{{}}})",
        namespace_matcher(namespaces)
    )
}

/// Container restarts in `namespaces` within the last `range`.
pub fn restarts_query(namespaces: Option<&[String]>, range: Range) -> String {
    format!(
        "sum(increase(kube_pod_container_status_restarts_total{{{}}}[{}s]))",
        namespace_matcher(namespaces),
        range.seconds()
    )
}

/// Unix time of the oldest memory sample of the scope within a week, to
/// offer only the ranges with data (as [`coverage::oldest_query`](super::coverage::oldest_query)).
pub fn oldest_query(namespaces: Option<&[String]>) -> String {
    format!(
        "min_over_time(timestamp({})[7d:5m])",
        memory_query(namespaces)
    )
}

/// CPU and memory of the scope over a range: the "CPU and memory" card,
/// which asks for them on its own so a new range reloads only it (#481).
#[derive(Clone, Debug, PartialEq)]
pub struct UsageCharts {
    pub cpu: Chart,
    pub memory: Chart,
}

/// The rest of the screen from Prometheus: the cards' numbers and pods by
/// phase, over a range of their own.
#[derive(Clone, Debug, PartialEq)]
pub struct OverviewStatus {
    /// `None` without kube-state-metrics (no `kube_pod_status_phase`), or
    /// when its query failed.
    pub phases: Option<PhaseChart>,
    /// Restarts within the range; `None` without kube-state-metrics.
    pub restarts: Option<f64>,
    /// Current CPU and memory (`rate` over [`USAGE_RATE_WINDOW`]), for the
    /// cards when metrics-server is missing.
    pub usage: Option<Usage>,
}

/// Series of a phase query as a [`PhaseChart`], `None` when there are none
/// (no kube-state-metrics).
pub fn phase_chart(series: Vec<(String, Vec<super::Point>)>, window: Window) -> Option<PhaseChart> {
    if series.is_empty() {
        return None;
    }
    let series = PodPhase::ALL
        .into_iter()
        .filter_map(|phase| {
            let (_, points) = series.iter().find(|(name, _)| name == phase.label())?;
            // Unknown only when some pod was in it.
            if phase == PodPhase::Unknown && points.iter().all(|p| p.value == 0.0) {
                return None;
            }
            Some((phase, points.clone()))
        })
        .collect();
    Some(PhaseChart { window, series })
}

impl Prometheus {
    /// The single series of `promql` over `window`, named `name`.
    async fn line(
        &self,
        name: &str,
        promql: &str,
        window: Window,
    ) -> Result<Vec<Series>, PrometheusError> {
        let points = self
            .query_range(promql, window)
            .await?
            .into_iter()
            .next()
            .unwrap_or_default();
        Ok(if points.is_empty() {
            Vec::new()
        } else {
            vec![Series {
                name: name.to_owned(),
                points,
            }]
        })
    }

    /// A single value of an instant query; `None` when it has no series.
    async fn scalar(&self, promql: &str) -> Result<Option<f64>, PrometheusError> {
        let body = self.get(&usage::query_path(promql)).await?;
        Ok(usage::parse_vector(&body)?.first().map(|(_, value)| *value))
    }

    /// CPU and memory of `namespaces` over the last `range`.
    pub async fn overview_usage(
        &self,
        namespaces: Option<&[String]>,
        range: Range,
    ) -> Result<UsageCharts, PrometheusError> {
        let window = Window::now(range);
        let cpu_promql = cpu_query(namespaces, &range.rate_window());
        let memory_promql = memory_query(namespaces);
        let cpu = self.line(Metric::Cpu.lines()[0], &cpu_promql, window);
        let memory = self.line(Metric::Memory.lines()[0], &memory_promql, window);
        let (cpu, memory) = futures::join!(cpu, memory);
        Ok(UsageCharts {
            cpu: Chart {
                metric: Metric::Cpu,
                window,
                series: cpu?,
            },
            memory: Chart {
                metric: Metric::Memory,
                window,
                series: memory?,
            },
        })
    }

    /// Pods by phase and restarts of `namespaces` over the last `range`,
    /// and their current usage. The current memory must answer (it tells
    /// whether Prometheus does); phases, restarts and current CPU are left
    /// out when their queries fail.
    pub async fn overview_status(
        &self,
        namespaces: Option<&[String]>,
        range: Range,
    ) -> Result<OverviewStatus, PrometheusError> {
        let window = Window::now(range);
        let restarts_promql = restarts_query(namespaces, range);
        let current_cpu_promql = cpu_query(namespaces, USAGE_RATE_WINDOW);
        let memory_promql = memory_query(namespaces);
        let phases = async {
            let body = self
                .get(&query_range_path(&phase_query(namespaces), window))
                .await?;
            Ok::<_, PrometheusError>(phase_chart(parse_query_range_by(&body, "phase")?, window))
        };
        let restarts = self.scalar(&restarts_promql);
        let current_cpu = self.scalar(&current_cpu_promql);
        let current_memory = self.scalar(&memory_promql);
        let (phases, restarts, current_cpu, current_memory) =
            futures::join!(phases, restarts, current_cpu, current_memory);
        let usage = match (current_cpu, current_memory?) {
            (Ok(Some(cpu)), Some(memory)) => Some(Usage {
                cpu: (cpu.max(0.0) * 1e9).round() as u64,
                memory: memory.max(0.0).round() as u64,
            }),
            _ => None,
        };
        let phases = phases.ok().flatten();
        Ok(OverviewStatus {
            // Restarts come from kube-state-metrics too: none without it.
            restarts: phases.as_ref().and(restarts.ok().flatten()),
            phases,
            usage,
        })
    }

    /// Unix time of the oldest sample of the scope (see [`oldest_query`]).
    pub async fn overview_oldest(
        &self,
        namespaces: Option<&[String]>,
    ) -> Result<Option<f64>, PrometheusError> {
        let body = self
            .get(&usage::query_path(&oldest_query(namespaces)))
            .await?;
        super::coverage::parse_oldest(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Range, Window, cpu_query, memory_query, oldest_query, phase_chart, phase_query,
        restarts_query,
    };
    use crate::metrics::overview::PodPhase;
    use crate::metrics::prometheus::{Point, parse_query_range_by};

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn queries_follow_the_namespaces() {
        let two = names(&["web", "kube-system"]);
        assert_eq!(
            cpu_query(Some(&two), "2m"),
            r#"sum(rate(container_cpu_usage_seconds_total{namespace=~"web|kube-system",container!="",container!="POD",metrics_path!="/metrics/resource"}[2m]))"#
        );
        assert_eq!(
            memory_query(None),
            r#"sum(container_memory_working_set_bytes{namespace!="",container!="",container!="POD",metrics_path!="/metrics/resource"})"#
        );
        assert_eq!(
            phase_query(Some(&names(&["a.b"]))),
            r#"sum by (phase) (kube_pod_status_phase{namespace=~"a\\.b"})"#
        );
        assert_eq!(
            restarts_query(None, Range::Hour),
            r#"sum(increase(kube_pod_container_status_restarts_total{namespace!=""}[3600s]))"#
        );
        let oldest = oldest_query(None);
        assert!(
            oldest.starts_with("min_over_time(timestamp(sum(container_memory_working_set_bytes{")
        );
        assert!(oldest.ends_with(")[7d:5m])"));
    }

    #[test]
    fn phases_chart_in_screen_order() {
        let body = r#"{"status":"success","data":{"resultType":"matrix","result":[
            {"metric":{"phase":"Succeeded"},"values":[[100,"2"],[130,"2"]]},
            {"metric":{"phase":"Running"},"values":[[100,"17"],[130,"18"]]},
            {"metric":{"phase":"Unknown"},"values":[[100,"0"],[130,"0"]]},
            {"metric":{"phase":"Pending"},"values":[[100,"1"],[130,"0"]]}]}}"#;
        let window = Window {
            start: 100,
            end: 130,
            step: 30,
        };
        let chart = phase_chart(parse_query_range_by(body, "phase").unwrap(), window).unwrap();
        let phases: Vec<_> = chart.series.iter().map(|(p, _)| *p).collect();
        assert_eq!(
            phases,
            [PodPhase::Running, PodPhase::Pending, PodPhase::Succeeded]
        );
        assert_eq!(
            chart.series[0].1[1],
            Point {
                time: 130.0,
                value: 18.0
            }
        );
        // No kube-state-metrics: no chart, the session's counts instead.
        assert!(phase_chart(Vec::new(), window).is_none());
    }
}
