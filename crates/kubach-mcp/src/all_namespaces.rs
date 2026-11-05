//! `allNamespaces` on `list_resources` and `list_events` (#107).
//!
//! There is no cluster-wide call: a temporary ServiceAccount only has Roles
//! per namespace, and this must stay so. One call reads namespace by
//! namespace, a few at a time, each through the same policy check and under
//! the same identity as a call naming that namespace, so Secrets, revoked
//! namespaces and custom resources need no rules of their own here.
//!
//! The answer starts with what it covers. A partial result that reads like
//! a full one is the bug this exists for, so everything that was left out
//! is named: at the top, where cutting the text cannot reach it.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::future::Future;
use std::time::Duration;

use futures::{StreamExt as _, stream};
use k8s_openapi::jiff::Timestamp;
use serde_json::json;

use crate::confirm::{ConfirmationDecision, ConfirmationRequest, RequestKind, ask};
use crate::policy::{Allowed, Denial, DenialCode, Request};
use crate::scope::SessionMode;
use crate::tools::{
    Ctx, MAX_LIST_ITEMS, MAX_OUTPUT_BYTES, Selectors, ToolError, ToolOutput, authorize,
    ensure_rbac, event_line, event_time, explain_selector, finish, kind_of, known_namespaces,
    read_events, read_rows, table_heading,
};
use kubach_core::sync;

/// Namespaces read at the same time.
const CONCURRENT_READS: usize = 8;
/// Most namespaces one call opens: each gets its identity set up, one after
/// another. The next call goes on, without a new question.
const MAX_OPENED_PER_CALL: usize = 50;
/// Names the question shows: the window has no scrolling for this text.
const MAX_NAMES_ASKED: usize = 40;
/// One namespace may take this long; a slow one becomes a gap, not a hang.
const READ_TIMEOUT: Duration = Duration::from_secs(20);
/// After a Role patch the API server's authorizer needs a moment; a
/// namespace opened by this very call meets that.
const FORBIDDEN_RETRIES: [Duration; 3] = [
    Duration::from_millis(200),
    Duration::from_millis(500),
    Duration::from_secs(1),
];
/// What the rows may take of the answer: the rest is the coverage note,
/// which has to survive in full.
const MAX_ROWS_BYTES: usize = MAX_OUTPUT_BYTES - 64 * 1024;

const REFUSED: &str = "the user refused or revoked reading it in this session; it is never read";
const DECLINED: &str = "not opened in this session: the user declined opening more namespaces \
     for allNamespaces, and is not asked again. Naming one in `namespace` still works";
const OVER_LIMIT: &str = "not opened yet, a call opens a limited number of namespaces; repeat \
     the call for these";
const NO_ROOM: &str = "left out, the answer reached its size limit; lower `limit`, narrow the \
     selectors or name the namespace";

/// What an `allNamespaces` answer covers.
#[derive(Debug, Default)]
pub(crate) struct Coverage {
    /// The session is pinned: its namespaces are all it can see.
    pinned: bool,
    read: Vec<String>,
    /// Namespaces left out, by why.
    pub(crate) gaps: BTreeMap<String, Vec<String>>,
    /// Read, but with more items than the answer holds.
    cut: Vec<String>,
    /// Why other namespaces may exist that nobody here knows of.
    pub(crate) unknown: Option<String>,
    /// The answer to the question of this call, if it asked one.
    pub(crate) answer: Option<&'static str>,
}

impl Coverage {
    fn gap(&mut self, reason: impl Into<String>, namespaces: Vec<String>) {
        if !namespaces.is_empty() {
            self.gaps
                .entry(reason.into())
                .or_default()
                .extend(namespaces);
        }
    }

    fn is_complete(&self) -> bool {
        self.gaps.is_empty() && self.cut.is_empty() && self.unknown.is_none()
    }

    /// The note the answer starts with.
    fn note(&self) -> String {
        let left_out: usize = self.gaps.values().map(Vec::len).sum();
        let read = self.read.len();
        let names = self.read.join(", ");
        let mut out = if self.is_complete() {
            format!("Coverage: complete. All {read} namespaces were read: {names}\n")
        } else {
            format!(
                "Coverage: PARTIAL, not the whole picture. {read} of {} known namespaces were \
                 read: {names}\n",
                read + left_out
            )
        };
        if self.pinned {
            out.push_str("This session is pinned to these namespaces; it sees no others.\n");
        }
        if left_out > 0 {
            let _ = writeln!(out, "Not covered ({left_out}):");
            for (reason, namespaces) in &self.gaps {
                let _ = writeln!(out, "- {}: {reason}", namespaces.join(", "));
            }
        }
        if !self.cut.is_empty() {
            let _ = writeln!(
                out,
                "Cut by the limit, more items exist in: {}. Raise `limit` or narrow the query",
                self.cut.join(", ")
            );
        }
        if let Some(unknown) = &self.unknown {
            let _ = writeln!(out, "Other namespaces may exist: {unknown}");
        }
        out
    }

    /// The answer as a call's result; the audit log gets the same coverage.
    fn output(&self, body: &str) -> ToolOutput {
        let text = finish(&format!("{}\n{body}", self.note()));
        ToolOutput {
            confirmed: self.answer == Some("allowed"),
            audit: Some(json!({
                "namespaces": self.read,
                "notCovered": self.gaps,
                "cut": self.cut,
                "complete": self.is_complete(),
                "question": self.answer,
            })),
            ..ToolOutput::read(text)
        }
    }

    /// Takes a namespace's failure in. A selector the server turns down is
    /// wrong everywhere, and the session's end stops everything: those fail
    /// the call.
    pub(crate) fn failed(
        &mut self,
        namespace: String,
        error: ToolError,
        field_selector: Option<&str>,
    ) -> Result<(), ToolError> {
        let reason = match &error {
            ToolError::Api {
                code: Some(400), ..
            } => return Err(explain_selector(error, field_selector)),
            ToolError::Denied(denial) if denial.code == DenialCode::SessionEnded => {
                return Err(error);
            }
            ToolError::Denied(denial) if denial.code == DenialCode::GrantDenied => {
                REFUSED.to_owned()
            }
            ToolError::Denied(denial) if denial.code == DenialCode::Rbac => {
                "your Kubernetes account has no right to list this there; Kubach cannot grant \
                 RBAC rights"
                    .to_owned()
            }
            ToolError::Api {
                code: Some(403), ..
            } => "the API server refused this session there (403)".to_owned(),
            other => format!("not read: {}", other.message()),
        };
        self.gap(reason, vec![namespace]);
        Ok(())
    }
}

/// The namespaces to read: granted or pinned ones, and the others once the
/// user allowed opening them. Never a revoked one.
pub(crate) async fn targets(ctx: &Ctx<'_>) -> Result<(Vec<String>, Coverage), ToolError> {
    let session = ctx.session;
    let mut coverage = Coverage {
        pinned: session.scope().pinned(),
        ..Coverage::default()
    };
    let known = if coverage.pinned {
        Vec::new()
    } else {
        let known = known_namespaces(session).await;
        if !known.complete {
            coverage.unknown = Some(known.note);
        }
        known.found.into_iter().map(|(name, _)| name).collect()
    };
    let plan = session.policy().all_namespaces_plan(&known);
    coverage.gap(REFUSED, plan.refused);
    let summary = format!(
        "{} wants to read all namespaces on {}: {} more for this session",
        ctx.client_name,
        session.scope().context,
        plan.unopened.len()
    );
    opened(ctx, plan.readable, plan.unopened, coverage, summary).await
}

/// The namespaces out of `names` a batch may work in (#108), with a gap for
/// each of the others. The agent named them, so one unopened namespace opens
/// as in a call naming it. Several would set up identities across a cluster
/// on one call, and that is asked once, as for `allNamespaces`.
pub(crate) async fn named_targets(
    ctx: &Ctx<'_>,
    names: &[String],
) -> Result<(Vec<String>, Coverage), ToolError> {
    let session = ctx.session;
    let mut coverage = Coverage {
        pinned: session.scope().pinned(),
        ..Coverage::default()
    };
    let (usable, plan) = {
        let policy = session.policy();
        let mut usable = Vec::new();
        for name in names {
            match policy.namespace(Some(name)) {
                Ok(name) => usable.push(name),
                Err(denial) => coverage.gap(denial.message, vec![name.clone()]),
            }
        }
        let plan = policy.all_namespaces_plan(&usable);
        (usable, plan)
    };
    let named = |mut found: Vec<String>| {
        found.retain(|name| usable.contains(name));
        found
    };
    coverage.gap(REFUSED, named(plan.refused));
    let mut readable = named(plan.readable);
    let mut unopened = named(plan.unopened);
    if unopened.len() < 2 {
        readable.append(&mut unopened);
    }
    let summary = format!(
        "{} wants to change objects on {} in {} namespaces it has not opened yet",
        ctx.client_name,
        session.scope().context,
        unopened.len()
    );
    opened(ctx, readable, unopened, coverage, summary).await
}

/// `targets`, and `unopened` once the user allowed opening them.
async fn opened(
    ctx: &Ctx<'_>,
    mut targets: Vec<String>,
    mut unopened: Vec<String>,
    mut coverage: Coverage,
    summary: String,
) -> Result<(Vec<String>, Coverage), ToolError> {
    if unopened.is_empty() {
        return Ok((targets, coverage));
    }
    match may_open(ctx, &unopened, summary, &mut coverage).await? {
        Ok(()) => {
            let rest = unopened.split_off(unopened.len().min(MAX_OPENED_PER_CALL));
            targets.extend(unopened);
            coverage.gap(OVER_LIMIT, rest);
        }
        Err(reason) => coverage.gap(reason, unopened),
    }
    Ok((targets, coverage))
}

/// Whether `names` may be opened, or why not. One question for all of them
/// until the session ends: reading is within the access, but opening sets
/// up an identity in each namespace (a ServiceAccount, where the user's
/// rights allow), and that should not reach across a cluster on the
/// strength of one agent call. No answer is not a no: the next call asks.
async fn may_open(
    ctx: &Ctx<'_>,
    names: &[String],
    summary: String,
    coverage: &mut Coverage,
) -> Result<Result<(), String>, ToolError> {
    let session = ctx.session;
    let _asking = session.asking().await;
    match session.all_namespaces_answer() {
        Some(true) => return Ok(Ok(())),
        Some(false) => return Ok(Err(DECLINED.to_owned())),
        None => {}
    }
    let scope = session.scope();
    let more = match names.len().saturating_sub(MAX_NAMES_ASKED) {
        0 => String::new(),
        more => format!(" and {more} more"),
    };
    let question = ConfirmationRequest {
        kind: RequestKind::Namespaces {
            names: names.to_vec(),
        },
        session_id: session.id().to_owned(),
        context: scope.context.clone(),
        client: ctx.client_name.to_owned(),
        tool: ctx.tool.to_owned(),
        namespace: String::new(),
        summary,
        details: format!(
            "Namespaces: {}{more}\n\nKubach reads them one by one and sets up access in each, \
             as when the agent names a namespace: a temporary ServiceAccount where your rights \
             allow it, removed when the session ends. This holds until the session ends, for \
             other namespaces too. If you decline, the agent gets what it can already read and \
             is told what was left out.",
            names[..names.len().min(MAX_NAMES_ASKED)].join(", ")
        ),
        dangerous: Vec::new(),
        mode: session.mode(),
        allow_remember: false,
    };
    let decision = ask(ctx.confirmer, &question).await;
    if !session.is_active() {
        return Err(Denial::new(
            DenialCode::SessionEnded,
            "the session ended while waiting for the user",
        )
        .into());
    }
    Ok(match decision {
        ConfirmationDecision::Rejected { .. } => {
            session.set_all_namespaces_answer(false);
            coverage.answer = Some("refused");
            Err(DECLINED.to_owned())
        }
        decision if decision.is_approved() => {
            session.set_all_namespaces_answer(true);
            coverage.answer = Some("allowed");
            Ok(())
        }
        decision => {
            coverage.answer = Some("no-answer");
            let reason = decision
                .reason()
                .map(|reason| format!(" ({reason})"))
                .unwrap_or_default();
            Err(format!(
                "not opened in this session: no answer from the user{reason}. Repeat the call, \
                 or name one in `namespace`"
            ))
        }
    })
}

/// Runs `read` on `namespaces`, a few at a time; the results come sorted.
pub(crate) async fn fan_out<T, F, Fut>(
    namespaces: Vec<String>,
    read: F,
) -> Vec<(String, Result<T, ToolError>)>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<T, ToolError>>,
{
    let mut results: Vec<(String, Result<T, ToolError>)> = stream::iter(namespaces)
        .map(|namespace| {
            let reading = read(namespace.clone());
            async move { (namespace, reading.await) }
        })
        .buffer_unordered(CONCURRENT_READS)
        .collect()
        .await;
    results.sort_by(|a, b| a.0.cmp(&b.0));
    results
}

/// Reads with a time limit, and past the moment a fresh Role takes effect.
pub(crate) async fn settled<T, F, Fut>(
    ctx: &Ctx<'_>,
    allowed: &Allowed,
    read: F,
) -> Result<T, ToolError>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T, ToolError>>,
{
    let once = async || {
        tokio::time::timeout(READ_TIMEOUT, read())
            .await
            .unwrap_or_else(|_| {
                Err(ToolError::Failed(format!(
                    "no answer in {} s",
                    READ_TIMEOUT.as_secs()
                )))
            })
    };
    let temporary = ctx.session.mode_in(&allowed.namespace) == SessionMode::TemporaryServiceAccount;
    for pause in FORBIDDEN_RETRIES {
        match once().await {
            Err(ToolError::Api {
                code: Some(403), ..
            }) if temporary => tokio::time::sleep(pause).await,
            result => return result,
        }
    }
    once().await
}

/// The namespaces of this call were each their own; none speaks for it in
/// the audit log, which gets them from the call's coverage.
pub(crate) fn forget_namespace(ctx: &Ctx<'_>) {
    *sync::lock(&ctx.namespace) = None;
}

/// `list_resources` with `allNamespaces`. `request` passed the policy as a
/// whole; `limit` holds per namespace.
pub(crate) async fn list_resources(
    ctx: &Ctx<'_>,
    allowed: &Allowed,
    request: &Request,
    limit: Option<u32>,
) -> Result<ToolOutput, ToolError> {
    let Request::List {
        kind,
        label_selector,
        field_selector,
        ..
    } = request
    else {
        return Err(ToolError::Failed("not a list request".into()));
    };
    let selectors = Selectors {
        label: label_selector.as_deref(),
        field: field_selector.as_deref(),
    };
    let (namespaces, mut coverage) = targets(ctx).await?;
    let results = fan_out(namespaces, |namespace| async move {
        let one = Request::List {
            kind: kind.clone(),
            namespace: Some(namespace),
            all_namespaces: false,
            label_selector: label_selector.clone(),
            field_selector: field_selector.clone(),
        };
        let allowed = authorize(ctx, &one).await?;
        settled(ctx, &allowed, || read_rows(ctx, &allowed, selectors, limit)).await
    })
    .await;
    forget_namespace(ctx);

    let plural = &kind_of(allowed).api.plural;
    let mut rows = String::new();
    let mut shown = 0;
    for (namespace, result) in results {
        let found = match result {
            Ok(found) => found,
            Err(error) => {
                coverage.failed(namespace, error, selectors.field)?;
                continue;
            }
        };
        let mut section = String::new();
        for line in &found.lines {
            let _ = writeln!(section, "{namespace}\t{line}");
        }
        if rows.len() + section.len() > MAX_ROWS_BYTES {
            coverage.gap(NO_ROOM, vec![namespace]);
            continue;
        }
        rows.push_str(&section);
        shown += found.lines.len();
        if found.more {
            coverage.cut.push(namespace.clone());
        }
        coverage.read.push(namespace);
    }
    let heading = table_heading(ctx, kind_of(allowed));
    let read = coverage.read.len();
    Ok(coverage.output(&format!(
        "NAMESPACE\t{heading}\n{rows}({shown} {plural} in {read} namespaces)"
    )))
}

/// `list_events` with `allNamespaces`: the newest `limit` events across the
/// namespaces read.
pub(crate) async fn list_events(
    ctx: &Ctx<'_>,
    request: &Request,
    limit: usize,
) -> Result<ToolOutput, ToolError> {
    let Request::Events { object, .. } = request else {
        return Err(ToolError::Failed("not an events request".into()));
    };
    let (namespaces, mut coverage) = targets(ctx).await?;
    let results = fan_out(namespaces, |namespace| async move {
        let one = Request::Events {
            namespace: Some(namespace),
            object: object.clone(),
            all_namespaces: false,
        };
        let allowed = authorize(ctx, &one).await?;
        ensure_rbac(ctx, &allowed.namespace, &[("list", "", "events")]).await?;
        settled(ctx, &allowed, || read_events(ctx, &allowed)).await
    })
    .await;
    forget_namespace(ctx);

    let mut events = Vec::new();
    for (namespace, result) in results {
        match result {
            Ok((found, more)) => {
                events.extend(found.into_iter().map(|event| (namespace.clone(), event)));
                if more {
                    coverage.cut.push(namespace.clone());
                }
                coverage.read.push(namespace);
            }
            Err(error) => coverage.failed(namespace, error, None)?,
        }
    }
    events.sort_by_key(|(_, event)| event_time(event));
    let now = Timestamp::now();
    let skip = events.len().saturating_sub(limit);
    let mut body = String::new();
    for (namespace, event) in events.iter().skip(skip) {
        let _ = writeln!(body, "{namespace}\t{}", event_line(event, now));
    }
    if body.is_empty() {
        body.push_str("(no events)\n");
    }
    if skip > 0 {
        let _ = writeln!(
            body,
            "(the newest {limit} of {} events read; raise `limit`, at most 200)",
            events.len()
        );
    }
    if !coverage.cut.is_empty() {
        let _ = writeln!(
            body,
            "(at most {MAX_LIST_ITEMS} events are read per namespace; newer ones may be missing \
             where the coverage note says so)"
        );
    }
    Ok(coverage.output(&body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn only_a_gapless_answer_says_complete() {
        let mut coverage = Coverage {
            read: names(&["a", "b"]),
            ..Coverage::default()
        };
        assert!(coverage.note().starts_with("Coverage: complete."));

        coverage.gap(REFUSED, names(&["c"]));
        coverage.gap(DECLINED, names(&["d", "e"]));
        coverage.gap(OVER_LIMIT, Vec::new());
        let note = coverage.note();
        assert!(note.starts_with("Coverage: PARTIAL"), "{note}");
        assert!(note.contains("2 of 5 known namespaces"), "{note}");
        assert!(note.contains("Not covered (3):"), "{note}");
        assert!(note.contains(&format!("- c: {REFUSED}")), "{note}");
        assert!(note.contains(&format!("- d, e: {DECLINED}")), "{note}");
    }

    #[test]
    fn a_cut_namespace_or_an_unknown_list_is_not_complete() {
        let cut = Coverage {
            read: names(&["a"]),
            cut: names(&["a"]),
            ..Coverage::default()
        };
        assert!(cut.note().starts_with("Coverage: PARTIAL"));
        assert!(cut.note().contains("more items exist in: a"));

        let unknown = Coverage {
            read: names(&["a"]),
            unknown: Some("the cluster does not let you list namespaces".into()),
            ..Coverage::default()
        };
        assert!(unknown.note().starts_with("Coverage: PARTIAL"));
        assert!(unknown.note().contains("Other namespaces may exist"));
    }

    #[test]
    fn the_note_comes_before_rows_and_survives_cutting() {
        let mut coverage = Coverage {
            read: names(&["a"]),
            ..Coverage::default()
        };
        coverage.gap(REFUSED, names(&["secret-place"]));
        let output = coverage.output(&"x\t".repeat(MAX_OUTPUT_BYTES));
        assert!(output.text.len() <= MAX_OUTPUT_BYTES + 256);
        assert!(output.text.contains("secret-place"));
        let audit = output.audit.unwrap();
        assert_eq!(audit["complete"], false);
        assert_eq!(audit["notCovered"][REFUSED][0], "secret-place");
    }

    #[test]
    fn a_namespace_fails_alone_unless_the_query_itself_is_wrong() {
        let mut coverage = Coverage::default();
        let api = |code| ToolError::Api {
            code: Some(code),
            message: "boom".into(),
        };
        coverage.failed("a".into(), api(403), None).unwrap();
        coverage.failed("b".into(), api(500), None).unwrap();
        assert_eq!(coverage.gaps.len(), 2);
        let error = coverage
            .failed("c".into(), api(400), Some("spec.nope=1"))
            .unwrap_err();
        assert!(
            error.message().contains("spec.nope=1"),
            "{}",
            error.message()
        );
    }
}
