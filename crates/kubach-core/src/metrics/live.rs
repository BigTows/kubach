//! Usage history kept on the client while the details panel is open, for
//! clusters without Prometheus: every metrics-server poll adds a sample,
//! so the CPU and memory charts grow from the moment the panel opened.

use std::collections::VecDeque;

use kubach_i18n::{Message, msg};

use super::prometheus::{Chart, Metric, Point, Series, Window};
use super::{NodeMetrics, PodMetrics, Usage};

/// Samples kept: an hour at the default 15 s poll interval.
pub const LIVE_CAPACITY: usize = 240;

/// Span of a chart from the session's history: a fixed window, or all of
/// it (up to [`LIVE_CAPACITY`] samples, an hour at the default interval).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LiveRange {
    FiveMinutes,
    FifteenMinutes,
    #[default]
    All,
}

impl LiveRange {
    pub const ALL: [LiveRange; 3] = [Self::FiveMinutes, Self::FifteenMinutes, Self::All];

    pub fn label(self) -> &'static str {
        match self {
            Self::FiveMinutes => "5m",
            Self::FifteenMinutes => "15m",
            Self::All => "All",
        }
    }

    /// Length in seconds; `None` for all of the history.
    pub fn seconds(self) -> Option<u64> {
        match self {
            Self::FiveMinutes => Some(5 * 60),
            Self::FifteenMinutes => Some(15 * 60),
            Self::All => None,
        }
    }
}

/// One poll: the total and its parts (containers of a pod, pods of a
/// workload), at Unix time `time`.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveSample {
    pub time: f64,
    pub total: Usage,
    pub parts: Vec<(String, Usage)>,
}

impl LiveSample {
    /// A sample of the pods named `pods` among `metrics`: split by
    /// container when it is one pod, else by pod. `None` when none of them
    /// has metrics yet.
    pub fn of_pods<'a>(
        metrics: impl IntoIterator<Item = &'a PodMetrics>,
        pods: &[String],
        time: f64,
    ) -> Option<Self> {
        let found: Vec<&PodMetrics> = metrics
            .into_iter()
            .filter(|m| pods.contains(&m.name))
            .collect();
        if found.is_empty() {
            return None;
        }
        let parts = match found.as_slice() {
            [pod] if pods.len() == 1 => pod
                .containers
                .iter()
                .map(|c| (c.name.clone(), c.usage))
                .collect(),
            _ => found.iter().map(|m| (m.name.clone(), m.total())).collect(),
        };
        Some(Self {
            time,
            total: found.iter().map(|m| m.total()).sum(),
            parts,
        })
    }

    pub fn of_node(metrics: &NodeMetrics, time: f64) -> Self {
        Self {
            time,
            total: metrics.usage,
            parts: Vec::new(),
        }
    }
}

/// Bounded list of [`LiveSample`]s, oldest first.
#[derive(Clone, Debug, Default)]
pub struct LiveHistory {
    samples: VecDeque<LiveSample>,
}

/// CPU in cores or memory in bytes, as charts plot them.
fn value(metric: Metric, usage: Usage) -> Option<f64> {
    match metric {
        Metric::Cpu => Some(usage.cpu as f64 / 1e9),
        Metric::Memory => Some(usage.memory as f64),
        Metric::Network | Metric::Filesystem => None,
    }
}

impl LiveHistory {
    pub fn push(&mut self, sample: LiveSample) {
        if self
            .samples
            .back()
            .is_some_and(|last| last.time >= sample.time)
        {
            return;
        }
        if self.samples.len() == LIVE_CAPACITY {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    pub fn latest(&self) -> Option<&LiveSample> {
        self.samples.back()
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Time span covered so far, from the first sample to `now`.
    pub fn window(&self, now: f64, step: u64) -> Window {
        let start = self.samples.front().map_or(now, |s| s.time);
        Window {
            start: start as u64,
            end: now.max(start) as u64,
            step,
        }
    }

    /// The window of `range` ending at `now`: the last minutes, or
    /// everything collected.
    pub fn range_window(&self, range: LiveRange, now: f64, step: u64) -> Window {
        let all = self.window(now, step);
        match range.seconds() {
            Some(seconds) => Window {
                start: all.end.saturating_sub(seconds),
                ..all
            },
            None => all,
        }
    }

    /// Why each of [`LiveRange::ALL`] is not offered at `now` (`None` for
    /// offered ones): a fixed window needs that much history, collected
    /// since the panel opened; until then it equals "All". The messages
    /// say "panel" (`place = "panel"`); a screen sets `place` to
    /// `"screen"`.
    pub fn unavailable(&self, now: f64) -> [Option<Message>; LiveRange::ALL.len()] {
        let collected = self
            .samples
            .front()
            .map_or(0.0, |first| (now - first.time).max(0.0));
        LiveRange::ALL.map(|range| match range.seconds() {
            Some(seconds) if collected < seconds as f64 => Some(msg!(
                "metrics-live-collected",
                age = crate::plugin::format_age(collected as i64),
                place = "panel"
            )),
            _ => None,
        })
    }

    /// The chart of `metric` (CPU and memory only): the total, or one line
    /// per part when `split`.
    pub fn chart(&self, metric: Metric, split: bool, window: Window) -> Option<Chart> {
        value(metric, Usage::default())?;
        let series = if split {
            let mut names: Vec<&str> = Vec::new();
            for sample in self
                .samples
                .iter()
                .filter(|s| s.time >= window.start as f64)
            {
                for (name, _) in &sample.parts {
                    if !names.contains(&name.as_str()) {
                        names.push(name);
                    }
                }
            }
            names.sort_unstable();
            names
                .into_iter()
                .map(|name| Series {
                    name: name.to_owned(),
                    points: self
                        .samples
                        .iter()
                        .filter(|s| s.time >= window.start as f64)
                        .filter_map(|s| {
                            let (_, usage) = s.parts.iter().find(|(n, _)| n == name)?;
                            Some(Point {
                                time: s.time,
                                value: value(metric, *usage)?,
                            })
                        })
                        .collect(),
                })
                .collect()
        } else {
            vec![Series {
                name: metric.lines()[0].to_owned(),
                points: self
                    .samples
                    .iter()
                    .filter(|s| s.time >= window.start as f64)
                    .filter_map(|s| {
                        Some(Point {
                            time: s.time,
                            value: value(metric, s.total)?,
                        })
                    })
                    .collect(),
            }]
        };
        Some(Chart {
            metric,
            window,
            series: series
                .into_iter()
                .filter(|s| !s.points.is_empty())
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::ContainerMetrics;

    fn pod(name: &str, containers: &[(&str, u64)]) -> PodMetrics {
        PodMetrics {
            name: name.into(),
            namespace: "ns".into(),
            timestamp: None,
            window: None,
            containers: containers
                .iter()
                .map(|(n, cpu)| ContainerMetrics {
                    name: (*n).into(),
                    usage: Usage {
                        cpu: *cpu,
                        memory: 1 << 20,
                    },
                })
                .collect(),
        }
    }

    #[test]
    fn samples_split_by_container_or_pod() {
        let metrics = [
            pod("a", &[("app", 100_000_000), ("sidecar", 50_000_000)]),
            pod("b", &[("app", 200_000_000)]),
        ];
        let one = LiveSample::of_pods(&metrics, &["a".into()], 1.0).unwrap();
        assert_eq!(one.total.cpu, 150_000_000);
        assert_eq!(one.parts.len(), 2);
        assert_eq!(one.parts[1].0, "sidecar");
        let both = LiveSample::of_pods(&metrics, &["a".into(), "b".into()], 1.0).unwrap();
        assert_eq!(both.total.cpu, 350_000_000);
        assert_eq!(
            both.parts.iter().map(|p| p.0.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert!(LiveSample::of_pods(&metrics, &["c".into()], 1.0).is_none());
    }

    #[test]
    fn history_is_bounded_and_charts_cpu_and_memory() {
        let metrics = [pod("a", &[("app", 250_000_000)])];
        let mut history = LiveHistory::default();
        for t in 0..(LIVE_CAPACITY + 5) {
            history.push(LiveSample::of_pods(&metrics, &["a".into()], t as f64).unwrap());
        }
        // Out-of-order and repeated samples are ignored.
        history.push(LiveSample::of_pods(&metrics, &["a".into()], 3.0).unwrap());
        assert_eq!(history.len(), LIVE_CAPACITY);
        let window = history.window(1000.0, 15);
        assert_eq!(window.start, 5);
        let cpu = history.chart(Metric::Cpu, false, window).unwrap();
        assert_eq!(cpu.series.len(), 1);
        assert_eq!(cpu.series[0].points[0].value, 0.25);
        let split = history.chart(Metric::Memory, true, window).unwrap();
        assert_eq!(split.series[0].name, "app");
        assert!(history.chart(Metric::Network, false, window).is_none());
    }

    #[test]
    fn ranges_open_as_history_grows() {
        let metrics = [pod("a", &[("app", 250_000_000)])];
        let mut history = LiveHistory::default();
        for t in 0..41 {
            history.push(LiveSample::of_pods(&metrics, &["a".into()], t as f64 * 15.0).unwrap());
        }
        // Ten minutes collected: 5m and All, not 15m.
        let now = 600.0;
        let why = history.unavailable(now);
        assert_eq!(why[0], None);
        assert_eq!(
            why[1].as_ref().map(|why| why.english()).as_deref(),
            Some("Collected since the panel opened: 10m so far")
        );
        assert_eq!(why[2], None);
        assert!(LiveHistory::default().unavailable(0.0)[0].is_some());
        // The 5m window keeps only its samples.
        let window = history.range_window(LiveRange::FiveMinutes, now, 15);
        assert_eq!((window.start, window.end), (300, 600));
        let cpu = history.chart(Metric::Cpu, false, window).unwrap();
        assert_eq!(cpu.series[0].points.len(), 21);
        assert_eq!(history.range_window(LiveRange::All, now, 15).start, 0);
    }
}
