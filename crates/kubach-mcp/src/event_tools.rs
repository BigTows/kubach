//! What `list_events` reads and how it says so (#111).
//!
//! `type` and `reason` are field selectors, so the API server applies them
//! before any limit. Time is not selectable and a list comes in key order,
//! not by time: a window can only be answered by reading on. So every read
//! pages through `continue` up to a bound and keeps the newest; a read that
//! stopped earlier says so, because the events it did not reach may be the
//! newest ones.

use std::fmt::Write as _;

use k8s_openapi::api::core::v1::Event;
use k8s_openapi::jiff::Timestamp;
use kube::api::{Api, ListParams};

use crate::policy::{Allowed, Denial, DenialCode};
use crate::tools::{Ctx, MAX_LIST_ITEMS, ToolError, event_line, event_time, finish, has_more};

/// Most `list` calls of one read, and so most events one namespace gives.
const MAX_EVENT_PAGES: u32 = 10;
pub(crate) const MAX_EVENTS_READ: u32 = MAX_EVENT_PAGES * MAX_LIST_ITEMS;
/// A week: API servers keep events for an hour by default, some for days.
pub(crate) const MAX_SINCE_SECONDS: u32 = 7 * 24 * 3600;
/// What the events API allows a `reason`.
const MAX_REASON_LEN: usize = 128;

/// Which of the events kept are wanted. Only [`EventFilter::new`] makes one
/// with values: the selector relies on what it checks.
#[derive(Debug, Default)]
pub(crate) struct EventFilter {
    event_type: Option<String>,
    reason: Option<String>,
    since_seconds: Option<u32>,
}

impl EventFilter {
    pub(crate) fn new(
        event_type: Option<String>,
        reason: Option<String>,
        since_seconds: Option<u32>,
    ) -> Result<Self, ToolError> {
        let invalid =
            |message: String| ToolError::from(Denial::new(DenialCode::InvalidInput, message));
        if let Some(event_type) = &event_type
            && !matches!(event_type.as_str(), "Normal" | "Warning")
        {
            return Err(invalid(format!(
                "type must be Normal or Warning, not {event_type:?}"
            )));
        }
        if let Some(reason) = &reason
            && !valid_reason(reason)
        {
            return Err(invalid(format!(
                "reason must be one event reason, such as BackOff: 1 to {MAX_REASON_LEN} letters, \
                 digits or . _ : / - and nothing else, not {reason:?}"
            )));
        }
        if since_seconds.is_some_and(|since| since == 0 || since > MAX_SINCE_SECONDS) {
            return Err(invalid(format!(
                "sinceSeconds must be 1 to {MAX_SINCE_SECONDS}"
            )));
        }
        Ok(Self {
            event_type,
            reason,
            since_seconds,
        })
    }

    pub(crate) fn since_seconds(&self) -> Option<u32> {
        self.since_seconds
    }

    /// The field selector of a read, with `object` if it is about one.
    fn selector(&self, object: Option<(&str, &str)>) -> Option<String> {
        let mut terms = Vec::new();
        if let Some((kind, name)) = object {
            terms.push(format!("involvedObject.kind={kind}"));
            terms.push(format!("involvedObject.name={name}"));
        }
        if let Some(event_type) = &self.event_type {
            terms.push(format!("type={event_type}"));
        }
        if let Some(reason) = &self.reason {
            terms.push(format!("reason={reason}"));
        }
        (!terms.is_empty()).then(|| terms.join(","))
    }

    /// An event with no time at all is kept: nothing says it is too old.
    fn in_window(&self, event: &Event, now: Timestamp) -> bool {
        let Some(since) = self.since_seconds else {
            return true;
        };
        event_time(event).is_none_or(|time| time.as_second() >= now.as_second() - i64::from(since))
    }
}

/// Allowed characters, not forbidden ones: a selector also knows `!` and
/// the escape `\`, and a reason is a word, not a place for either.
fn valid_reason(reason: &str) -> bool {
    !reason.is_empty()
        && reason.len() <= MAX_REASON_LEN
        && reason
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:/-".contains(&byte))
}

/// One namespace's matching events.
#[derive(Debug, Default)]
pub(crate) struct EventsRead {
    /// The newest ones, as many as asked for, oldest first.
    pub(crate) events: Vec<Event>,
    /// All the matching ones met, kept or not.
    pub(crate) found: usize,
    /// The read stopped with more to read: anything may be behind that.
    pub(crate) more: bool,
}

impl EventsRead {
    /// Takes one page in; only `keep` events stay in memory.
    fn add(&mut self, page: impl Iterator<Item = Event>, keep: usize) {
        let before = self.events.len();
        self.events.extend(page);
        self.found += self.events.len() - before;
        self.events.sort_by_key(event_time);
        let extra = self.events.len().saturating_sub(keep);
        self.events.drain(..extra);
    }
}

/// Reads the events of one namespace that `filter` wants, keeping the newest
/// `keep`.
pub(crate) async fn read_events(
    ctx: &Ctx<'_>,
    allowed: &Allowed,
    filter: &EventFilter,
    keep: usize,
) -> Result<EventsRead, ToolError> {
    let api: Api<Event> = Api::namespaced(
        ctx.session.client_in(&allowed.namespace)?,
        &allowed.namespace,
    );
    let object = allowed
        .kind
        .as_ref()
        .zip(allowed.name.as_ref())
        .map(|(kind, name)| (kind.api.kind.as_str(), name.as_str()));
    let mut params = ListParams::default().limit(MAX_LIST_ITEMS);
    if let Some(selector) = filter.selector(object) {
        params = params.fields(&selector);
    }
    let now = Timestamp::now();
    let mut read = EventsRead::default();
    for page in 1..=MAX_EVENT_PAGES {
        let list = match api.list(&params).await.map_err(ToolError::from) {
            Ok(list) => list,
            // The continue token expired: what was read stands, the rest
            // is unknown.
            Err(ToolError::Api {
                code: Some(410), ..
            }) if page > 1 => {
                read.more = true;
                break;
            }
            Err(error) => return Err(error),
        };
        read.more = has_more(&list.metadata);
        read.add(
            list.items.into_iter().filter(|event| {
                event.metadata.namespace.as_deref() == Some(allowed.namespace.as_str())
                    && filter.in_window(event, now)
            }),
            keep,
        );
        if !read.more {
            break;
        }
        params.continue_token = list.metadata.continue_;
    }
    Ok(read)
}

/// The first line of an answer with `sinceSeconds`. `missing` says why
/// events in the window may not have been reached, if they may not.
pub(crate) fn window_note(since: u32, shown: usize, found: usize, missing: Option<&str>) -> String {
    let window = format!("Window: the last {since} s");
    match missing {
        Some(why) => format!(
            "{window}, NOT fully covered: events in it may be missing, {why}. {shown} of the \
             {found} found in it are shown.\n"
        ),
        None if shown < found => format!(
            "{window}, read to the end but CUT: the newest {shown} of {found} events in it are \
             shown. Raise `limit`, at most 200, or narrow the query.\n"
        ),
        None => format!(
            "{window}, fully covered: all {found} events in it that the API server still keeps \
             are shown.\n"
        ),
    }
}

/// Events of one namespace as text, the newest `limit`.
pub(crate) async fn list_events(
    ctx: &Ctx<'_>,
    allowed: &Allowed,
    filter: &EventFilter,
    limit: usize,
) -> Result<String, ToolError> {
    let read = read_events(ctx, allowed, filter, limit).await?;
    let now = Timestamp::now();
    let shown = read.events.len();
    let unfinished = format!(
        "the read stopped before the end (at most {MAX_EVENTS_READ} events are read, in no time \
         order); narrow with type, reason or kind + name"
    );
    let mut out = String::new();
    if let Some(since) = filter.since_seconds() {
        let missing = read.more.then_some(unfinished.as_str());
        out.push_str(&window_note(since, shown, read.found, missing));
    }
    for event in &read.events {
        let _ = writeln!(out, "{}", event_line(event, now));
    }
    if read.events.is_empty() {
        out.push_str("(no events)\n");
    }
    if filter.since_seconds().is_none() {
        if shown < read.found {
            let _ = writeln!(
                out,
                "(the newest {shown} of {} events read; list_events takes a `limit` of at most \
                 200)",
                read.found
            );
        }
        if read.more {
            let _ = writeln!(
                out,
                "(more events exist and newer ones may be missing: {unfinished})"
            );
        }
    }
    Ok(finish(&out))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn filter(
        event_type: Option<&str>,
        reason: Option<&str>,
        since: Option<u32>,
    ) -> Result<EventFilter, ToolError> {
        EventFilter::new(
            event_type.map(str::to_owned),
            reason.map(str::to_owned),
            since,
        )
    }

    fn event_at(time: &str) -> Event {
        serde_json::from_value(json!({
            "metadata": {"name": time},
            "involvedObject": {},
            "lastTimestamp": time,
        }))
        .unwrap()
    }

    #[test]
    fn only_the_two_types_pass() {
        assert!(filter(Some("Normal"), None, None).is_ok());
        assert!(filter(Some("Warning"), None, None).is_ok());
        for wrong in ["warning", "Error", "", "Warning,reason=x"] {
            assert!(filter(Some(wrong), None, None).is_err(), "{wrong}");
        }
    }

    #[test]
    fn a_reason_cannot_add_selector_terms() {
        for fine in [
            "BackOff",
            "FailedCreatePodSandBox",
            "cert-manager.io/Issued",
            "a_b:c",
        ] {
            assert!(filter(None, Some(fine), None).is_ok(), "{fine}");
        }
        let too_long = "x".repeat(MAX_REASON_LEN + 1);
        for wrong in [
            "",
            "a,b",
            "a=b",
            "a!=b",
            "BackOff,type=Normal",
            "a\\,b",
            "a b",
            "a\n",
            "причина",
            too_long.as_str(),
        ] {
            assert!(filter(None, Some(wrong), None).is_err(), "{wrong:?}");
        }
    }

    #[test]
    fn the_window_is_positive_and_bounded() {
        assert!(filter(None, None, Some(1)).is_ok());
        assert!(filter(None, None, Some(MAX_SINCE_SECONDS)).is_ok());
        assert!(filter(None, None, Some(0)).is_err());
        assert!(filter(None, None, Some(MAX_SINCE_SECONDS + 1)).is_err());
    }

    #[test]
    fn the_selector_joins_what_is_set() {
        let none = EventFilter::default();
        assert_eq!(none.selector(None), None);
        let both = filter(Some("Warning"), Some("BackOff"), Some(60)).unwrap();
        assert_eq!(
            both.selector(None).as_deref(),
            Some("type=Warning,reason=BackOff")
        );
        assert_eq!(
            both.selector(Some(("Pod", "web"))).as_deref(),
            Some("involvedObject.kind=Pod,involvedObject.name=web,type=Warning,reason=BackOff")
        );
    }

    #[test]
    fn the_window_goes_by_the_last_occurrence() {
        let now: Timestamp = "2026-01-01T01:00:00Z".parse().unwrap();
        let hour = filter(None, None, Some(3600)).unwrap();
        assert!(hour.in_window(&event_at("2026-01-01T00:00:00Z"), now));
        assert!(!hour.in_window(&event_at("2025-12-31T23:59:59Z"), now));
        assert!(EventFilter::default().in_window(&event_at("2020-01-01T00:00:00Z"), now));

        // A series started long ago and still going is in the window.
        let series: Event = serde_json::from_value(json!({
            "metadata": {"name": "e"},
            "involvedObject": {},
            "eventTime": "2025-01-01T00:00:00.000000Z",
            "series": {"count": 9, "lastObservedTime": "2026-01-01T00:30:00.000000Z"},
        }))
        .unwrap();
        assert!(hour.in_window(&series, now));
    }

    #[test]
    fn pages_keep_the_newest_and_count_all() {
        let mut read = EventsRead::default();
        let page = |times: &[&str]| times.iter().map(|t| event_at(t)).collect::<Vec<_>>();
        read.add(
            page(&["2026-01-03T00:00:00Z", "2026-01-01T00:00:00Z"]).into_iter(),
            2,
        );
        read.add(
            page(&["2026-01-02T00:00:00Z", "2026-01-04T00:00:00Z"]).into_iter(),
            2,
        );
        assert_eq!(read.found, 4);
        let kept: Vec<_> = read
            .events
            .iter()
            .map(|event| event.metadata.name.as_deref().unwrap())
            .collect();
        assert_eq!(kept, ["2026-01-03T00:00:00Z", "2026-01-04T00:00:00Z"]);
    }

    #[test]
    fn the_note_never_calls_a_cut_window_covered() {
        assert!(window_note(60, 3, 3, None).contains("fully covered: all 3"));
        let hidden = window_note(60, 2, 3, None);
        assert!(hidden.contains("CUT: the newest 2 of 3"), "{hidden}");
        assert!(!hidden.contains("fully covered"), "{hidden}");
        let unread = window_note(60, 3, 3, Some("the read stopped"));
        assert!(unread.contains("NOT fully covered"), "{unread}");
        assert!(unread.contains("the read stopped"), "{unread}");
    }
}
