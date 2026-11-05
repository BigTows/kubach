//! `wait_for_condition` (#109): waits until an object, or every object a
//! label selector matches, has a status condition, appears or goes away,
//! like `kubectl wait`, so an agent does not poll for it.
//!
//! The wait reads with plain `get` and `list`, never `watch`: that is all a
//! temporary ServiceAccount's Role names for a custom resource (#103). It
//! is one of the policy [`Request`]s of `get_resource` and `list_resources`,
//! so what they need and ask for is what it needs and asks for. Each round
//! asks the policy again, without asking the user: a right taken back ends
//! the wait at once on every identity, not only where the API server
//! enforces it. Nothing is locked between rounds.

use std::fmt::{self, Write as _};
use std::time::Duration;

use k8s_openapi::jiff::Timestamp;
use kubach_core::discovery;
use kube::ResourceExt as _;
use kube::api::{ApiResource, DynamicObject, ListParams};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::masking::mask_text;
use crate::policy::{Allowed, Denial, DenialCode, Request};
use crate::rollout_tools::{first_line, pod_line, wait_timeout};
use crate::session::Session;
use crate::tools::{
    Ctx, MAX_LIST_ITEMS, ToolError, ToolOutput, authorize, ensure_namespace, explain_forbidden,
    fetch, finish, has_more, kind_of, kind_ref, parse,
};

/// How often the objects are read again.
const POLL: Duration = Duration::from_secs(2);
/// How soon an ended session is noticed, also during a read that hangs.
const END_CHECK: Duration = Duration::from_millis(250);
/// Objects described when the condition is not met.
const MAX_OBJECTS: usize = 10;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WaitArgs {
    kind: String,
    api_version: Option<String>,
    namespace: Option<String>,
    name: Option<String>,
    label_selector: Option<String>,
    condition: String,
    timeout_seconds: Option<u64>,
}

/// What the agent waits for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Wanted {
    Exists,
    Deleted,
    /// `Ready=True`: `status.conditions` has this type with this status.
    Status {
        condition: String,
        status: &'static str,
    },
}

impl Wanted {
    fn parse(text: &str) -> Result<Self, Denial> {
        let text = text.trim();
        if text.eq_ignore_ascii_case("exists") {
            return Ok(Self::Exists);
        }
        if text.eq_ignore_ascii_case("deleted") {
            return Ok(Self::Deleted);
        }
        let invalid = || {
            Denial::new(
                DenialCode::InvalidInput,
                "condition takes exists, deleted, or a status condition as Type=Status, where \
                 Status is True, False or Unknown (Ready=True)",
            )
        };
        let (condition, status) = text.split_once('=').ok_or_else(invalid)?;
        let status = ["True", "False", "Unknown"]
            .into_iter()
            .find(|known| known.eq_ignore_ascii_case(status.trim()))
            .ok_or_else(invalid)?;
        let condition = condition.trim();
        let plain = |c: char| c.is_ascii_alphanumeric() || "._/-".contains(c);
        if condition.is_empty() || !condition.chars().all(plain) {
            return Err(invalid());
        }
        Ok(Self::Status {
            condition: condition.to_owned(),
            status,
        })
    }

    /// What "met" means with a label selector.
    fn meaning(&self) -> String {
        match self {
            Self::Exists => "at least one object matches".to_owned(),
            Self::Deleted => "no object matches".to_owned(),
            Self::Status { .. } => {
                format!("every matching object has {self}, and at least one matches")
            }
        }
    }
}

impl fmt::Display for Wanted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exists => f.write_str("exists"),
            Self::Deleted => f.write_str("deleted"),
            Self::Status { condition, status } => write!(f, "{condition}={status}"),
        }
    }
}

/// Why `obj` does not have the status condition yet; `None` when it does.
/// As in `kubectl wait`, a condition about an older generation of the object
/// does not count: right after a change it still tells of the old spec.
fn unmet_status(obj: &DynamicObject, condition: &str, status: &str) -> Option<String> {
    let conditions = obj
        .data
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let text = |entry: &Value, key: &str| {
        entry
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let Some(found) = conditions
        .iter()
        .find(|entry| text(entry, "type").eq_ignore_ascii_case(condition))
    else {
        let reported = conditions
            .iter()
            .map(|entry| format!("{}={}", text(entry, "type"), text(entry, "status")))
            .collect::<Vec<_>>()
            .join(", ");
        return Some(if reported.is_empty() {
            format!("no {condition} condition yet, and no conditions at all")
        } else {
            format!("no {condition} condition yet; it has {reported}")
        });
    };
    let actual = text(found, "status");
    if !actual.eq_ignore_ascii_case(status) {
        let reason = text(found, "reason");
        let message = text(found, "message");
        let details: Vec<&str> = [reason.as_str(), first_line(&message)]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect();
        return Some(if details.is_empty() {
            format!("{condition}={actual}")
        } else {
            format!("{condition}={actual} ({})", details.join(": "))
        });
    }
    let observed = found
        .get("observedGeneration")
        .or_else(|| obj.data.pointer("/status/observedGeneration"))
        .and_then(Value::as_i64);
    match (observed, obj.metadata.generation) {
        (Some(observed), Some(generation)) if observed < generation => Some(format!(
            "{condition}={actual} is about generation {observed}, the object is at {generation}"
        )),
        _ => None,
    }
}

/// What a round read.
#[derive(Default)]
struct Snapshot {
    objects: Vec<DynamicObject>,
    /// More objects match than one read returns.
    cut: bool,
}

/// What the wait knows so far, kept for the answer however the wait ends.
#[derive(Default)]
struct Seen {
    /// Objects of the last read that do not meet the condition, and why.
    unmet: Vec<(DynamicObject, Option<String>)>,
    /// How many objects the last read returned.
    objects: usize,
    cut: bool,
    /// The last state, in one line.
    state: Option<String>,
    /// Why the last read failed, when the wait goes on regardless.
    error: Option<String>,
    /// The object named for `deleted`, as first read.
    uid: Option<String>,
    /// Whether any object was read at all.
    any: bool,
    /// A new object took the name of the one waited for.
    replaced: bool,
}

/// What the wait is on: one object or what a selector matches.
struct Target<'a> {
    api: &'a ApiResource,
    namespace: &'a str,
    wanted: &'a Wanted,
    name: Option<&'a str>,
    selector: Option<&'a str>,
}

impl Target<'_> {
    /// How the answer names what was waited for.
    fn subject(&self) -> String {
        match self.name {
            Some(name) => format!("{} {}/{name}", self.api.kind, self.namespace),
            None => format!(
                "{} matching {} in {}",
                self.api.plural,
                self.selector.unwrap_or_default(),
                self.namespace
            ),
        }
    }

    /// Takes in a round's objects; whether the condition is met. Waiting
    /// cannot settle "every object" when not all of them can be read.
    fn judge(&self, read: Snapshot, seen: &mut Seen) -> Result<bool, ToolError> {
        let Snapshot { mut objects, cut } = read;
        if cut && matches!(self.wanted, Wanted::Status { .. }) {
            return Err(ToolError::Failed(format!(
                "{}: more than {MAX_LIST_ITEMS} objects match, too many to tell whether all of \
                 them have {}. Narrow labelSelector.",
                self.subject(),
                self.wanted
            )));
        }
        if self.name.is_some() && *self.wanted == Wanted::Deleted {
            // A StatefulSet's pod comes back under its name at once: the
            // object waited for is gone all the same.
            match (&seen.uid, objects.first().and_then(|obj| obj.uid())) {
                (None, Some(uid)) => seen.uid = Some(uid),
                (Some(first), Some(uid)) if *first != uid => {
                    seen.replaced = true;
                    objects.clear();
                }
                _ => {}
            }
        }
        seen.any |= !objects.is_empty() || seen.replaced;
        seen.objects = objects.len();
        seen.cut = cut;
        seen.error = None;
        let met = match self.wanted {
            Wanted::Exists => {
                seen.unmet.clear();
                !objects.is_empty()
            }
            Wanted::Deleted => {
                let met = objects.is_empty();
                seen.unmet = objects.into_iter().map(|obj| (obj, None)).collect();
                met
            }
            Wanted::Status { condition, status } => {
                let any = !objects.is_empty();
                seen.unmet = objects
                    .into_iter()
                    .filter_map(|obj| {
                        let why = unmet_status(&obj, condition, status)?;
                        Some((obj, Some(why)))
                    })
                    .collect();
                any && seen.unmet.is_empty()
            }
        };
        seen.state = Some(self.state(seen));
        Ok(met)
    }

    /// Where the objects stand, in one line.
    fn state(&self, seen: &Seen) -> String {
        let count = format!("{}{}", seen.objects, if seen.cut { "+" } else { "" });
        let first_why = seen.unmet.first().and_then(|(_, why)| why.as_deref());
        match (self.name, self.wanted) {
            (Some(_), _) if seen.objects == 0 => "not found".to_owned(),
            (None, _) if seen.objects == 0 => "no object matches".to_owned(),
            (Some(_), Wanted::Deleted) => "still there".to_owned(),
            (Some(_), _) => first_why.unwrap_or("found").to_owned(),
            (None, Wanted::Deleted) => format!("{count} still match"),
            (None, Wanted::Exists) => format!("{count} match"),
            (None, Wanted::Status { .. }) => format!(
                "{} of {count} have {}{}",
                seen.objects - seen.unmet.len(),
                self.wanted,
                first_why.map(|why| format!("; {why}")).unwrap_or_default()
            ),
        }
    }

    /// The answer when the condition is met.
    fn met(&self, seen: &Seen, waited: u64) -> String {
        let subject = self.subject();
        let wanted = self.wanted;
        let mut out = match (self.name, wanted) {
            (Some(_), Wanted::Exists) => format!("{subject} exists ({waited}s)."),
            (Some(_), Wanted::Deleted) if seen.replaced => format!(
                "{subject} is gone ({waited}s): a new object with the same name took its place."
            ),
            (Some(_), Wanted::Deleted) if !seen.any => {
                format!("{subject} is gone: it was not there when the wait began.")
            }
            (Some(_), Wanted::Deleted) => format!("{subject} is gone ({waited}s)."),
            (Some(_), Wanted::Status { .. }) => format!("{subject} has {wanted} ({waited}s)."),
            (None, Wanted::Exists) => format!("{} {subject} exist ({waited}s).", seen.objects),
            (None, Wanted::Deleted) if !seen.any => {
                format!("No {subject}: none matched when the wait began.")
            }
            (None, Wanted::Deleted) => format!("No {subject} any more ({waited}s)."),
            (None, Wanted::Status { .. }) => {
                format!("All {} {subject} have {wanted} ({waited}s).", seen.objects)
            }
        };
        out.push('\n');
        if self.selector.is_some() {
            let _ = writeln!(out, "Met means: {}.", wanted.meaning());
        }
        out
    }

    /// The final state of a wait that did not end with the condition met.
    fn final_state(&self, ctx: &Ctx<'_>, seen: &Seen) -> String {
        let mut out = String::new();
        if self.selector.is_some() {
            let _ = writeln!(out, "Met means: {}.", self.wanted.meaning());
        }
        match &seen.state {
            Some(state) => {
                let _ = writeln!(out, "Final state: {state}");
            }
            None => out.push_str("Final state: unknown, no read has finished.\n"),
        }
        if let Some(error) = &seen.error {
            let _ = writeln!(out, "The last read failed: {error}");
        }
        let now = Timestamp::now();
        for (obj, why) in seen.unmet.iter().take(MAX_OBJECTS) {
            let _ = writeln!(
                out,
                "  {}",
                object_line(ctx, self.api, obj, why.as_deref(), now)
            );
        }
        if seen.unmet.len() > MAX_OBJECTS {
            let _ = writeln!(out, "  (and {} more)", seen.unmet.len() - MAX_OBJECTS);
        }
        out
    }
}

/// One object that does not meet the condition: what the condition found
/// and what holds it up. A pod's line tells why it is not ready, any other
/// object's health is its plugin's.
fn object_line(
    ctx: &Ctx<'_>,
    api: &ApiResource,
    obj: &DynamicObject,
    why: Option<&str>,
    now: Timestamp,
) -> String {
    let pod = api.group.is_empty() && api.kind == "Pod";
    let mut line = if pod {
        pod_line(obj, now)
    } else {
        obj.name_any()
    };
    let mut notes: Vec<String> = why.iter().map(ToString::to_string).collect();
    if obj.metadata.deletion_timestamp.is_some() {
        let finalizers = obj.finalizers();
        notes.push(if finalizers.is_empty() {
            "terminating".to_owned()
        } else {
            format!("terminating, held by finalizers {}", finalizers.join(", "))
        });
    }
    if !pod {
        let health = ctx.registry.plugin_for_resource(api).health(obj);
        if !health.message.is_empty() {
            notes.push(first_line(&health.message).to_owned());
        }
    }
    if !notes.is_empty() {
        let _ = write!(line, ": {}", notes.join("; "));
    }
    line
}

/// One round's read with the kind's `get` or `list`. No object is an
/// answer here, not an error.
async fn read(
    ctx: &Ctx<'_>,
    allowed: &Allowed,
    selector: Option<&str>,
) -> Result<Snapshot, ToolError> {
    let client = ctx.session.client_in(&allowed.namespace)?;
    let result = match selector {
        None => fetch(&client, allowed).await.map(|obj| Snapshot {
            objects: vec![obj],
            cut: false,
        }),
        Some(selector) => {
            let api = discovery::api(&client, kind_of(allowed), Some(&allowed.namespace));
            let params = ListParams::default().labels(selector).limit(MAX_LIST_ITEMS);
            match api.list(&params).await {
                Ok(list) => {
                    for obj in &list.items {
                        ensure_namespace(obj, &allowed.namespace)?;
                    }
                    Ok(Snapshot {
                        cut: has_more(&list.metadata),
                        objects: list.items,
                    })
                }
                Err(error) => Err(error.into()),
            }
        }
    };
    match result {
        Err(ToolError::Api {
            code: Some(404), ..
        }) => Ok(Snapshot::default()),
        Err(error) => {
            let verb = if selector.is_some() { "list" } else { "get" };
            Err(explain_forbidden(ctx, allowed, verb, error))
        }
        ok => ok,
    }
}

/// A failed read the wait goes on after: the API server restarting or
/// overloaded says nothing about the object. A refusal is final.
fn passes(error: &ToolError) -> bool {
    matches!(
        error,
        ToolError::Api { code: None, .. }
            | ToolError::Api {
                code: Some(408 | 429 | 500..),
                ..
            }
    )
}

/// Reads until the condition is met. The caller drops it to stop the wait,
/// so what it learns goes to `seen` at once.
async fn poll(
    ctx: &Ctx<'_>,
    request: &Request,
    target: &Target<'_>,
    seen: &mut Seen,
) -> Result<(), ToolError> {
    let mut told = String::new();
    loop {
        let allowed = ctx.session.policy().check(request)?;
        match read(ctx, &allowed, target.selector).await {
            Ok(snapshot) => {
                if target.judge(snapshot, seen)? {
                    return Ok(());
                }
            }
            Err(error) if passes(&error) => seen.error = Some(error.message()),
            Err(error) => return Err(error),
        }
        let state = seen.error.clone().or_else(|| seen.state.clone());
        if let (Some(progress), Some(state)) = (&ctx.progress, state)
            && state != told
        {
            progress(mask_text(&format!("{}: {state}", target.subject())).into_owned());
            told = state;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Why a wait stopped before its timeout.
enum Stop {
    Cancelled,
    SessionEnded,
}

/// Ends when the wait has to stop: the agent cancelled the call or went
/// away, or the session is over.
async fn stopped(ctx: &Ctx<'_>) -> Stop {
    tokio::select! {
        () = ctx.cancel.cancelled() => Stop::Cancelled,
        () = session_over(ctx.session) => Stop::SessionEnded,
    }
}

async fn session_over(session: &Session) {
    while session.is_active() {
        tokio::time::sleep(END_CHECK).await;
    }
}

pub(crate) async fn wait_for_condition(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    let a: WaitArgs = parse(args)?;
    let timeout = wait_timeout(a.timeout_seconds)?;
    let wanted = Wanted::parse(&a.condition)?;
    let kind = kind_ref(a.kind, a.api_version);
    let request = match (a.name, &a.label_selector) {
        (Some(name), None) => Request::Get {
            kind,
            name,
            namespace: a.namespace,
        },
        (None, Some(selector)) if !selector.trim().is_empty() => Request::List {
            kind,
            namespace: a.namespace,
            all_namespaces: false,
            label_selector: a.label_selector.clone(),
        },
        _ => {
            return Err(Denial::new(
                DenialCode::InvalidInput,
                "pass exactly one of name and labelSelector",
            )
            .into());
        }
    };
    let allowed = authorize(ctx, &request).await?;
    let target = Target {
        api: &kind_of(&allowed).api,
        namespace: &allowed.namespace,
        wanted: &wanted,
        name: allowed.name.as_deref(),
        selector: a.label_selector.as_deref(),
    };
    let started = tokio::time::Instant::now();
    let mut seen = Seen::default();
    let stop = tokio::select! {
        biased;
        stop = stopped(ctx) => Some(stop),
        () = tokio::time::sleep_until(started + timeout) => None,
        result = poll(ctx, &request, &target, &mut seen) => {
            result?;
            let waited = started.elapsed().as_secs();
            return Ok(ToolOutput::read(finish(&target.met(&seen, waited))));
        }
    };
    let waited = started.elapsed().as_secs();
    let subject = target.subject();
    let last = target.final_state(ctx, &seen);
    Err(match stop {
        None => ToolError::Failed(finish(&format!(
            "{subject}: {wanted} not met after {waited}s.\n{last}Call again to wait longer, or \
             look at describe_resource and list_events.\n"
        ))),
        Some(Stop::Cancelled) => ToolError::Failed(finish(&format!(
            "The wait for {wanted} on {subject} was cancelled after {waited}s.\n{last}"
        ))),
        Some(Stop::SessionEnded) => Denial::new(
            DenialCode::SessionEnded,
            finish(&format!(
                "the session ended after {waited}s of waiting for {wanted} on {subject}.\n{last}"
            )),
        )
        .into(),
    })
}

#[cfg(test)]
mod tests {
    use kube::api::{ApiResource, DynamicObject};
    use serde_json::{Value, json};

    use super::{Seen, Snapshot, Target, Wanted, unmet_status};

    fn object(name: &str, uid: &str, generation: i64, status: Value) -> DynamicObject {
        serde_json::from_value(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": name, "namespace": "ns", "uid": uid, "generation": generation},
            "status": status,
        }))
        .unwrap()
    }

    fn ready(name: &str, value: &str) -> DynamicObject {
        object(
            name,
            name,
            1,
            json!({"conditions": [{"type": "Ready", "status": value,
                                   "reason": "Why", "message": "because"}]}),
        )
    }

    fn snapshot(objects: Vec<DynamicObject>) -> Snapshot {
        Snapshot {
            objects,
            cut: false,
        }
    }

    fn pods() -> ApiResource {
        ApiResource {
            group: String::new(),
            version: "v1".into(),
            api_version: "v1".into(),
            kind: "Pod".into(),
            plural: "pods".into(),
        }
    }

    fn target<'a>(api: &'a ApiResource, wanted: &'a Wanted, name: Option<&'a str>) -> Target<'a> {
        Target {
            api,
            namespace: "ns",
            wanted,
            name,
            selector: name.is_none().then_some("app=web"),
        }
    }

    fn wanted(text: &str) -> Wanted {
        Wanted::parse(text).unwrap()
    }

    #[test]
    fn conditions_parse_or_are_refused() {
        assert_eq!(wanted("exists"), Wanted::Exists);
        assert_eq!(wanted(" Deleted "), Wanted::Deleted);
        assert_eq!(
            wanted("Ready=true"),
            Wanted::Status {
                condition: "Ready".into(),
                status: "True"
            }
        );
        assert_eq!(
            wanted("foo.io/Synced=unknown").to_string(),
            "foo.io/Synced=Unknown"
        );
        for bad in [
            "",
            "Ready",
            "=True",
            "Ready=",
            "Ready=Maybe",
            "Re dy=True",
            "a=b=True",
        ] {
            assert!(Wanted::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_status_condition_tells_why_it_is_not_met() {
        assert_eq!(unmet_status(&ready("a", "True"), "ready", "True"), None);
        assert_eq!(
            unmet_status(&ready("a", "False"), "Ready", "True").unwrap(),
            "Ready=False (Why: because)"
        );
        assert_eq!(
            unmet_status(&ready("a", "True"), "Synced", "True").unwrap(),
            "no Synced condition yet; it has Ready=True"
        );
        assert_eq!(
            unmet_status(&object("a", "a", 1, json!({})), "Ready", "True").unwrap(),
            "no Ready condition yet, and no conditions at all"
        );
    }

    #[test]
    fn a_condition_of_an_older_generation_does_not_count() {
        let stale = object(
            "a",
            "a",
            3,
            json!({"observedGeneration": 2,
                   "conditions": [{"type": "Available", "status": "True"}]}),
        );
        assert!(
            unmet_status(&stale, "Available", "True")
                .unwrap()
                .contains("generation 2")
        );
        // The condition's own generation goes first.
        let fresh = object(
            "a",
            "a",
            3,
            json!({"observedGeneration": 2, "conditions": [
                {"type": "Available", "status": "True", "observedGeneration": 3}]}),
        );
        assert_eq!(unmet_status(&fresh, "Available", "True"), None);
    }

    #[test]
    fn a_selector_needs_all_objects_and_at_least_one() {
        let (api, ready_true) = (pods(), wanted("Ready=True"));
        let target = target(&api, &ready_true, None);
        let mut seen = Seen::default();
        assert!(!target.judge(snapshot(vec![]), &mut seen).unwrap(), "none");
        assert_eq!(seen.state.as_deref(), Some("no object matches"));
        let some = vec![ready("a", "True"), ready("b", "False")];
        assert!(!target.judge(snapshot(some), &mut seen).unwrap(), "1 of 2");
        assert_eq!(
            seen.state.as_deref(),
            Some("1 of 2 have Ready=True; Ready=False (Why: because)")
        );
        let all = vec![ready("a", "True"), ready("b", "True")];
        assert!(target.judge(snapshot(all), &mut seen).unwrap());
        let text = target.met(&seen, 3);
        assert!(
            text.starts_with("All 2 pods matching app=web in ns have Ready=True (3s)."),
            "{text}"
        );
        assert!(
            text.contains("every matching object has Ready=True, and at least one matches"),
            "{text}"
        );
    }

    #[test]
    fn too_many_objects_settle_no_status_condition() {
        let cut = || Snapshot {
            objects: vec![ready("a", "True")],
            cut: true,
        };
        let api = pods();
        let ready_true = wanted("Ready=True");
        let failed = target(&api, &ready_true, None).judge(cut(), &mut Seen::default());
        assert!(
            failed
                .unwrap_err()
                .message()
                .contains("Narrow labelSelector")
        );
        let exists = target(&api, &Wanted::Exists, None).judge(cut(), &mut Seen::default());
        assert!(exists.unwrap());
        let mut seen = Seen::default();
        let deleted = target(&api, &Wanted::Deleted, None).judge(cut(), &mut seen);
        assert!(!deleted.unwrap());
        assert_eq!(seen.state.as_deref(), Some("1+ still match"));
    }

    #[test]
    fn exists_and_deleted_on_a_selector() {
        let api = pods();
        let exists = target(&api, &Wanted::Exists, None);
        let mut seen = Seen::default();
        assert!(!exists.judge(snapshot(vec![]), &mut seen).unwrap());
        let one = snapshot(vec![ready("a", "False")]);
        assert!(exists.judge(one, &mut seen).unwrap());

        let deleted = target(&api, &Wanted::Deleted, None);
        let mut seen = Seen::default();
        let one = snapshot(vec![ready("a", "True")]);
        assert!(!deleted.judge(one, &mut seen).unwrap());
        assert!(deleted.judge(snapshot(vec![]), &mut seen).unwrap());
        assert!(deleted.met(&seen, 2).contains("any more"));
        // Nothing ever matched: a typo looks just like this, so it is said.
        let mut never = Seen::default();
        assert!(deleted.judge(snapshot(vec![]), &mut never).unwrap());
        let text = deleted.met(&never, 0);
        assert!(text.contains("none matched when the wait began"), "{text}");
    }

    #[test]
    fn deleted_follows_the_object_not_the_name() {
        let api = pods();
        let target = target(&api, &Wanted::Deleted, Some("web-0"));
        let mut seen = Seen::default();
        let first = object("web-0", "uid-1", 1, json!({}));
        let again = snapshot(vec![first.clone()]);
        assert!(!target.judge(snapshot(vec![first]), &mut seen).unwrap());
        assert!(!target.judge(again, &mut seen).unwrap());
        let next = snapshot(vec![object("web-0", "uid-2", 1, json!({}))]);
        assert!(target.judge(next, &mut seen).unwrap());
        let text = target.met(&seen, 4);
        assert!(text.contains("a new object with the same name"), "{text}");
        // Never there: met, and said.
        let mut never = Seen::default();
        assert!(target.judge(snapshot(vec![]), &mut never).unwrap());
        let text = target.met(&never, 0);
        assert!(text.contains("not there when the wait began"), "{text}");
    }
}
