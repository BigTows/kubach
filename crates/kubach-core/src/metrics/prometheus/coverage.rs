//! Which chart ranges have data (#376): one instant query finds the oldest
//! sample Prometheus still keeps for the object, and a range is offered
//! when it shows more than the next shorter one. The panel asks with its
//! chart refresh (once a minute) only while some range is still disabled.
//!
//! The query asks about the object itself, not the server: it answers both
//! "how far back does retention go" and "how old is this pod" at once.
//! `prometheus_tsdb_lowest_timestamp` needs Prometheus to scrape itself and
//! describes the whole TSDB, and the configured retention says nothing about
//! a pod created ten minutes ago: either would offer a week of mostly empty
//! chart.

use kubach_i18n::{Message, msg};

use super::{Metric, Prometheus, PrometheusError, Range, Target, queries, usage};
use crate::plugin::format_age;

/// How far back the oldest sample is looked for: the longest range.
const LOOKBACK: &str = "7d";
/// Resolution of the lookup: a subquery step of 5 minutes keeps it cheap
/// (2016 evaluations of one small selector over a week) and precise enough
/// for ranges of 15 minutes and more.
const RESOLUTION: &str = "5m";

/// PromQL of the Unix time of the oldest memory sample of `target` within
/// [`LOOKBACK`]. Memory is a gauge present for every running container and
/// node, so it marks when data starts without `rate()` windows.
pub fn oldest_query(target: &Target) -> Option<String> {
    let memory = queries(target, Metric::Memory, Range::Hour)
        .into_iter()
        .next()?
        .promql;
    Some(format!(
        "min_over_time(timestamp({memory})[{LOOKBACK}:{RESOLUTION}])"
    ))
}

/// Parses the answer of [`oldest_query`]: `None` when Prometheus has no
/// sample of the object at all.
pub fn parse_oldest(body: &str) -> Result<Option<f64>, PrometheusError> {
    Ok(usage::parse_vector(body)?
        .into_iter()
        .map(|(_, value)| value)
        .reduce(f64::min))
}

/// Why each of [`Range::ALL`] is not offered, `None` for offered ones,
/// given the oldest sample (`oldest`, see [`parse_oldest`]) at `now`. The
/// messages are about "this object" (`subject = "object"`); a screen about
/// a set of pods sets `subject` to `"pods"`.
///
/// The shortest range is always offered. A longer one is offered when data
/// goes back past the next shorter range: otherwise it would draw the same
/// points squeezed to the right edge.
pub fn unavailable(oldest: Option<f64>, now: f64) -> [Option<Message>; Range::ALL.len()] {
    let mut out: [Option<Message>; Range::ALL.len()] = Default::default();
    for (ix, slot) in out.iter_mut().enumerate().skip(1) {
        let shorter = Range::ALL[ix - 1];
        *slot = match oldest {
            // No sample at all, or one younger than the last subquery step.
            None => Some(msg!(
                "metrics-coverage-less",
                range = Range::ALL[0].label(),
                subject = "object"
            )),
            Some(oldest) if oldest > now - shorter.seconds() as f64 => Some(msg!(
                "metrics-coverage-same",
                age = format_age((now - oldest).max(0.0) as i64),
                range = shorter.label(),
                subject = "object"
            )),
            Some(_) => None,
        };
    }
    out
}

/// `range` if it is offered, else the longest offered range shorter than
/// it (the shortest range is always offered).
pub fn fit<T>(range: Range, unavailable: &[Option<T>; Range::ALL.len()]) -> Range {
    let ix = Range::ALL.iter().position(|r| *r == range).unwrap_or(0);
    (0..=ix)
        .rev()
        .find(|&ix| unavailable[ix].is_none())
        .map_or(Range::ALL[0], |ix| Range::ALL[ix])
}

impl Prometheus {
    /// Unix time of the oldest sample of `target` (see [`oldest_query`]).
    pub async fn oldest(&self, target: &Target) -> Result<Option<f64>, PrometheusError> {
        let Some(promql) = oldest_query(target) else {
            return Ok(None);
        };
        let body = self.get(&usage::query_path(&promql)).await?;
        parse_oldest(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::{Range, Target, fit, oldest_query, parse_oldest, unavailable};

    #[test]
    fn asks_for_the_oldest_memory_sample() {
        let pod = Target::Pod {
            namespace: "ns".into(),
            name: "web-0".into(),
        };
        let promql = oldest_query(&pod).unwrap();
        assert!(promql.starts_with("min_over_time(timestamp(sum(container_memory_working_set_bytes{namespace=\"ns\",pod=\"web-0\""));
        assert!(promql.ends_with("[7d:5m])"));
    }

    #[test]
    fn parses_the_oldest_sample() {
        let body = r#"{"status":"success","data":{"resultType":"vector","result":[
            {"metric":{},"value":[1700000000,"1699990000"]}]}}"#;
        assert_eq!(parse_oldest(body).unwrap(), Some(1_699_990_000.0));
        let empty = r#"{"status":"success","data":{"resultType":"vector","result":[]}}"#;
        assert_eq!(parse_oldest(empty).unwrap(), None);
        assert!(parse_oldest(r#"{"status":"error","error":"bad"}"#).is_err());
    }

    #[test]
    fn offers_ranges_that_show_more_than_the_shorter_one() {
        let now = 1_000_000.0;
        let offered = |oldest: Option<f64>| -> Vec<&str> {
            Range::ALL
                .iter()
                .zip(unavailable(oldest, now))
                .filter(|(_, why)| why.is_none())
                .map(|(r, _)| r.label())
                .collect()
        };
        // A pod started ten minutes ago: only 15m.
        assert_eq!(offered(Some(now - 600.0)), ["15m"]);
        // Forty minutes: 1h shows more than 15m, 6h would not.
        assert_eq!(offered(Some(now - 2400.0)), ["15m", "1h"]);
        // Two days of retention: everything up to 7d.
        assert_eq!(
            offered(Some(now - 2.0 * 86400.0)),
            ["15m", "1h", "6h", "24h", "7d"]
        );
        assert_eq!(offered(None), ["15m"]);
        let why = unavailable(Some(now - 2400.0), now);
        assert_eq!(
            why[2].as_ref().map(|why| why.english()).as_deref(),
            Some("Prometheus has only 40m of data for this object: same as 1h")
        );
    }

    #[test]
    fn falls_back_to_the_longest_offered_range() {
        let now = 1_000_000.0;
        let young = unavailable(Some(now - 600.0), now);
        assert_eq!(fit(Range::Hour, &young), Range::FifteenMinutes);
        let old = unavailable(Some(now - 7200.0), now);
        assert_eq!(fit(Range::Hour, &old), Range::Hour);
        assert_eq!(fit(Range::Week, &old), Range::SixHours);
    }
}
