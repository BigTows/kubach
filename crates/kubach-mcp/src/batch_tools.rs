//! `scale_batch` and `rollout_restart_batch` (#108): one window instead of
//! dozens of calls.
//!
//! A batch is the single calls in a row with one question: every object
//! passes the same policy and RBAC checks as `scale` or `rollout_restart`,
//! under the identity of its namespace, and has its own audit line.
//! Namespaces are opened and gaps named as for `allNamespaces`
//! ([`crate::all_namespaces`]).
//!
//! The window is the promise: objects are found first, a `labelSelector`
//! included, and afterwards only those objects are touched, by UID. A patch
//! carries the UID too, so the API server turns down an object of the same
//! name created in between. What matches the selector only after the answer
//! is reported and left alone.
//!
//! Nothing is remembered from a batch window. The one thing that skips it
//! is the single call's own rule: a batch in one namespace where the user
//! said not to ask again about that very change.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;

use futures::{StreamExt as _, stream};
use kubach_core::discovery::{self, ResourceKind};
use kubach_core::plugin::builtin::{action, restart_object, scale_object};
use kubach_core::plugin::{ActionId, ActionKind};
use kubach_core::sync;
use kube::ResourceExt as _;
use kube::api::{DynamicObject, ListParams};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::all_namespaces::{Coverage, fan_out, forget_namespace, named_targets, settled, targets};
use crate::audit::DecidedBy;
use crate::confirm::{BatchAction, BatchChange, BatchObject, ConfirmationRequest, RequestKind};
use crate::policy::{Allowed, Denial, DenialCode, KindRef, Policy, Request};
use crate::remember::{RememberNote, rememberable};
use crate::tools::{
    Ctx, ObjectAudit, ToolError, ToolOutput, approve_change, ask_access, authorize, ensure_rbac,
    fetch, finish, kind_of, kind_ref, parse,
};

/// Most objects in one batch: all of them have to fit in the window, and a
/// batch is never cut to fit.
pub(crate) const MAX_OBJECTS: usize = 200;
/// Most restarts sent at the same time.
pub(crate) const MAX_CONCURRENCY: usize = 16;
/// Restarts sent at the same time unless the agent says otherwise.
const DEFAULT_CONCURRENCY: usize = 4;
/// Objects read, and scales sent, at the same time.
const CONCURRENT_CALLS: usize = 8;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ObjectArgs {
    kind: Option<String>,
    api_version: Option<String>,
    namespace: Option<String>,
    name: String,
    replicas: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BatchArgs {
    kind: Option<String>,
    api_version: Option<String>,
    namespace: Option<String>,
    objects: Option<Vec<ObjectArgs>>,
    label_selector: Option<String>,
    namespaces: Option<Vec<String>>,
    #[serde(default)]
    all_namespaces: bool,
    replicas: Option<i64>,
    concurrency: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Scale,
    Restart,
}

impl Op {
    fn tool(self) -> &'static str {
        match self {
            Self::Scale => "scale_batch",
            Self::Restart => "rollout_restart_batch",
        }
    }

    /// The tool of one such change.
    fn single(self) -> &'static str {
        match self {
            Self::Scale => "scale",
            Self::Restart => "rollout_restart",
        }
    }

    fn action(self) -> ActionId {
        match self {
            Self::Scale => action::SCALE,
            Self::Restart => action::RESTART,
        }
    }

    /// The request of the single call on one object.
    fn request(
        self,
        kind: KindRef,
        name: String,
        namespace: Option<String>,
        replicas: Option<i64>,
    ) -> Request {
        match self {
            Self::Scale => Request::Scale {
                kind,
                name,
                namespace,
                replicas: replicas.unwrap_or_default(),
            },
            Self::Restart => Request::Restart {
                kind,
                name,
                namespace,
            },
        }
    }

    /// What the single call patches, for the user's RBAC.
    fn resource(self, kind: &ResourceKind) -> String {
        match self {
            Self::Scale => format!("{}/scale", kind.api.plural),
            Self::Restart => kind.api.plural.clone(),
        }
    }
}

/// An object the agent named.
#[derive(Debug)]
struct Wanted {
    kind: KindRef,
    namespace: String,
    name: String,
    replicas: Option<i64>,
}

impl Wanted {
    fn label(&self) -> String {
        format!("{} {}/{}", self.kind.kind, self.namespace, self.name)
    }
}

/// Where a selector looks.
#[derive(Debug, PartialEq, Eq)]
enum Place {
    Named(Vec<String>),
    All,
}

/// What the call asks to change.
#[derive(Debug)]
enum Selection {
    Objects(Vec<Wanted>),
    Selector {
        kind: KindRef,
        selector: String,
        place: Place,
        replicas: Option<i64>,
    },
}

fn invalid(message: impl Into<String>) -> ToolError {
    Denial::new(DenialCode::InvalidInput, message).into()
}

/// How many changes go out at once.
fn concurrency(op: Op, asked: Option<usize>) -> Result<usize, ToolError> {
    match (op, asked) {
        (Op::Scale, None) => Ok(CONCURRENT_CALLS),
        (Op::Scale, Some(_)) => Err(invalid("concurrency is for rollout_restart_batch")),
        (Op::Restart, None) => Ok(DEFAULT_CONCURRENCY),
        (Op::Restart, Some(n)) if (1..=MAX_CONCURRENCY).contains(&n) => Ok(n),
        (Op::Restart, Some(_)) => Err(invalid(format!(
            "concurrency must be 1 to {MAX_CONCURRENCY}"
        ))),
    }
}

/// The arguments as a [`Selection`]. `session_namespace` answers for what
/// names no namespace, as in the single call.
fn selection(
    op: Op,
    a: BatchArgs,
    session_namespace: impl Fn() -> Result<String, Denial>,
) -> Result<Selection, ToolError> {
    if op == Op::Restart && a.replicas.is_some() {
        return Err(invalid("replicas is for scale_batch"));
    }
    let top_replicas = a.replicas;
    let replicas = |own: Option<i64>| match (op, own.or(top_replicas)) {
        (Op::Scale, None) => Err(invalid("replicas is required, at the top or per object")),
        (_, replicas) => Ok(replicas),
    };
    match (a.objects, a.label_selector) {
        (Some(objects), None) => {
            if a.namespaces.is_some() || a.all_namespaces {
                return Err(invalid(
                    "namespaces and allNamespaces go with labelSelector; objects name their own",
                ));
            }
            if objects.is_empty() || objects.len() > MAX_OBJECTS {
                return Err(invalid(format!(
                    "objects must hold 1 to {MAX_OBJECTS} items"
                )));
            }
            let mut wanted = Vec::with_capacity(objects.len());
            for object in objects {
                if op == Op::Restart && object.replicas.is_some() {
                    return Err(invalid("replicas is for scale_batch"));
                }
                let (kind, api_version) = match object.kind {
                    Some(kind) => (kind, object.api_version),
                    None => (
                        a.kind
                            .clone()
                            .ok_or_else(|| invalid("kind is required, at the top or per object"))?,
                        object.api_version.or_else(|| a.api_version.clone()),
                    ),
                };
                let namespace = match object.namespace.or_else(|| a.namespace.clone()) {
                    Some(namespace) => namespace,
                    None => session_namespace()?,
                };
                wanted.push(Wanted {
                    kind: kind_ref(kind, api_version),
                    namespace,
                    name: object.name,
                    replicas: replicas(object.replicas)?,
                });
            }
            Ok(Selection::Objects(wanted))
        }
        (None, Some(selector)) => {
            if selector.trim().is_empty() {
                return Err(invalid("labelSelector must not be empty"));
            }
            let kind = a
                .kind
                .ok_or_else(|| invalid("kind goes with labelSelector"))?;
            let place = match (a.namespace, a.namespaces, a.all_namespaces) {
                (None, None, true) => Place::All,
                (None, Some(mut names), false) if !names.is_empty() => {
                    names.sort();
                    names.dedup();
                    Place::Named(names)
                }
                (Some(name), None, false) => Place::Named(vec![name]),
                (None, None, false) => Place::Named(vec![session_namespace()?]),
                _ => {
                    return Err(invalid(
                        "give one of namespace, a non-empty namespaces or allNamespaces",
                    ));
                }
            };
            Ok(Selection::Selector {
                kind: kind_ref(kind, a.api_version),
                selector,
                place,
                replicas: replicas(None)?,
            })
        }
        _ => Err(invalid("give exactly one of objects or labelSelector")),
    }
}

/// An object the batch is about to change, or has changed.
#[derive(Clone, Debug)]
struct Target {
    kind: ResourceKind,
    namespace: String,
    name: String,
    uid: String,
    /// Replicas when the object was found, for a scale.
    from: Option<i64>,
    to: Option<i64>,
    request: Request,
}

impl Target {
    fn label(&self) -> String {
        format!("{} {}/{}", self.kind.api.kind, self.namespace, self.name)
    }

    fn args(&self) -> Value {
        json!({
            "kind": self.kind.api.kind,
            "namespace": self.namespace,
            "name": self.name,
            "uid": self.uid,
            "originalReplicas": self.from,
            "replicas": self.to,
        })
    }

    fn batch_object(&self) -> BatchObject {
        BatchObject {
            kind: self.kind.api.kind.clone(),
            namespace: self.namespace.clone(),
            name: self.name.clone(),
            replicas: self.from.zip(self.to),
        }
    }
}

/// `spec.replicas`, which the API server defaults to 1.
fn replicas_of(obj: &DynamicObject) -> i64 {
    obj.data
        .pointer("/spec/replicas")
        .and_then(Value::as_i64)
        .unwrap_or(1)
}

/// What the call found before anything is asked.
#[derive(Default)]
struct Plan {
    /// The objects that will change: what the window shows.
    targets: Vec<Target>,
    /// Scales already at the count asked for.
    unchanged: Vec<Target>,
    /// Objects the batch does not include, by why.
    left_out: BTreeMap<String, Vec<String>>,
    /// Namespaces a selector could not cover.
    coverage: Coverage,
    /// Namespaces a selector looked in, and every UID it found there.
    looked_in: Vec<String>,
    seen: HashSet<String>,
}

impl Plan {
    /// Takes in one object's verdict. The session's end stops the call.
    fn add(
        &mut self,
        ctx: &Ctx<'_>,
        label: String,
        namespace: &str,
        verdict: Result<Target, ToolError>,
    ) -> Result<(), ToolError> {
        match verdict {
            Ok(target) if target.from.is_some() && target.from == target.to => {
                let (args, by) = (target.args(), DecidedBy::None);
                audit(ctx, args, namespace, by, true, None, "unchanged");
                self.unchanged.push(target);
            }
            Ok(target) => self.targets.push(target),
            Err(ToolError::Denied(denial)) if denial.code == DenialCode::SessionEnded => {
                return Err(denial.into());
            }
            Err(error) => {
                let reason = error.message();
                let (args, by) = (json!({"object": label}), decided_by(&error));
                audit(
                    ctx,
                    args,
                    namespace,
                    by,
                    false,
                    Some(reason.clone()),
                    "left-out",
                );
                self.left_out.entry(reason).or_default().push(label);
            }
        }
        Ok(())
    }

    /// Why namespaces and objects stay out, one line per reason.
    fn left_out_lines(&self) -> Vec<String> {
        let namespaces = self
            .coverage
            .gaps
            .iter()
            .map(|(why, names)| format!("namespaces {}: {why}", names.join(", ")));
        let objects = self
            .left_out
            .iter()
            .map(|(why, names)| format!("{}: {why}", names.join(", ")));
        namespaces.chain(objects).collect()
    }

    /// The namespaces of the objects that will change.
    fn namespaces(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.targets.iter().map(|t| t.namespace.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        names
    }
}

fn decided_by(error: &ToolError) -> DecidedBy {
    match error {
        ToolError::Denied(_) => DecidedBy::Policy,
        ToolError::Rejected(_) => DecidedBy::User,
        ToolError::Api { .. } => DecidedBy::Api,
        ToolError::Failed(_) => DecidedBy::None,
    }
}

fn audit(
    ctx: &Ctx<'_>,
    args: Value,
    namespace: &str,
    decided_by: DecidedBy,
    allowed: bool,
    reason: Option<String>,
    result: &str,
) {
    (ctx.audit_object)(ObjectAudit {
        args,
        namespace: namespace.to_owned(),
        decided_by,
        allowed,
        reason,
        result: result.to_owned(),
    });
}

/// Puts one object through what the single call goes through. `found` is
/// the object if a selector already read it.
async fn admit(
    ctx: &Ctx<'_>,
    op: Op,
    kind: KindRef,
    namespace: String,
    name: String,
    replicas: Option<i64>,
    found: Option<DynamicObject>,
) -> Result<Target, ToolError> {
    let request = op.request(kind, name, Some(namespace), replicas);
    let allowed = authorize(ctx, &request).await?;
    let kind = kind_of(&allowed).clone();
    ensure_rbac(
        ctx,
        &allowed.namespace,
        &[("patch", &kind.api.group, &op.resource(&kind))],
    )
    .await?;
    let obj = match found {
        Some(obj) => obj,
        None => {
            let client = ctx.session.client_in(&allowed.namespace)?;
            settled(ctx, &allowed, || fetch(&client, &allowed)).await?
        }
    };
    let supported = ctx
        .registry
        .plugin_for_resource(&kind.api)
        .actions_for(&obj)
        .into_iter()
        .any(|a| a.id == op.action() && a.kind == ActionKind::Api);
    if !supported {
        return Err(ToolError::Failed(format!(
            "{} does not support `{}`",
            kind.api.kind,
            op.action()
        )));
    }
    let Some(uid) = obj.metadata.uid.clone() else {
        return Err(ToolError::Failed("the object has no UID".into()));
    };
    Ok(Target {
        from: (op == Op::Scale).then(|| replicas_of(&obj)),
        to: replicas.filter(|_| op == Op::Scale),
        namespace: allowed.namespace.clone(),
        name: obj.name_any(),
        uid,
        kind,
        request,
    })
}

/// What a selector matches in `allowed`'s namespace, up to one more than a
/// batch holds, so that too many are noticed.
async fn matching(
    ctx: &Ctx<'_>,
    allowed: &Allowed,
    selector: &str,
) -> Result<Vec<DynamicObject>, ToolError> {
    let client = ctx.session.client_in(&allowed.namespace)?;
    let api = discovery::api(&client, kind_of(allowed), Some(&allowed.namespace));
    let limit = u32::try_from(MAX_OBJECTS + 1).unwrap_or(u32::MAX);
    let params = ListParams::default().labels(selector).limit(limit);
    Ok(api.list(&params).await?.items)
}

fn list_request(kind: &KindRef, selector: &str, namespace: &str) -> Request {
    Request::List {
        kind: kind.clone(),
        namespace: Some(namespace.to_owned()),
        all_namespaces: false,
        label_selector: Some(selector.to_owned()),
        field_selector: None,
    }
}

/// The objects the agent listed.
async fn resolve_objects(
    ctx: &Ctx<'_>,
    op: Op,
    wanted: Vec<Wanted>,
    plan: &mut Plan,
) -> Result<(), ToolError> {
    let mut names: Vec<String> = wanted.iter().map(|w| w.namespace.clone()).collect();
    names.sort();
    names.dedup();
    let (open, mut coverage) = named_targets(ctx, &names).await?;
    // The agent named these objects: they are the gaps here, not the
    // namespaces they are in.
    let gaps = std::mem::take(&mut coverage.gaps);
    plan.coverage = coverage;
    let (inside, outside): (Vec<Wanted>, Vec<Wanted>) = wanted
        .into_iter()
        .partition(|w| open.contains(&w.namespace));
    for object in outside {
        let why = gaps
            .iter()
            .find(|(_, names)| names.contains(&object.namespace))
            .map_or("its namespace is not available", |(why, _)| why.as_str());
        let denied = ToolError::Denied(Denial::new(DenialCode::Namespace, why));
        plan.add(ctx, object.label(), &object.namespace, Err(denied))?;
    }
    let verdicts: Vec<_> = stream::iter(inside)
        .map(|object| async move {
            let label = object.label();
            let namespace = object.namespace.clone();
            let verdict = admit(
                ctx,
                op,
                object.kind,
                object.namespace,
                object.name,
                object.replicas,
                None,
            )
            .await;
            (label, namespace, verdict)
        })
        .buffer_unordered(CONCURRENT_CALLS)
        .collect()
        .await;
    for (label, namespace, verdict) in verdicts {
        plan.add(ctx, label, &namespace, verdict)?;
    }
    Ok(())
}

/// What a selector matches right now: the only time it is resolved for the
/// change.
async fn resolve_selector(
    ctx: &Ctx<'_>,
    op: Op,
    kind: &KindRef,
    selector: &str,
    place: Place,
    replicas: Option<i64>,
    plan: &mut Plan,
) -> Result<(), ToolError> {
    let (namespaces, coverage) = match place {
        Place::All => targets(ctx).await?,
        Place::Named(names) => named_targets(ctx, &names).await?,
    };
    plan.coverage = coverage;
    let results = fan_out(namespaces, |namespace| async move {
        let request = list_request(kind, selector, &namespace);
        let allowed = authorize(ctx, &request).await?;
        settled(ctx, &allowed, || matching(ctx, &allowed, selector)).await
    })
    .await;
    let mut found = Vec::new();
    for (namespace, result) in results {
        match result {
            Ok(objects) => {
                plan.looked_in.push(namespace.clone());
                found.extend(objects.into_iter().map(|obj| (namespace.clone(), obj)));
            }
            Err(error) => plan.coverage.failed(namespace, error, None)?,
        }
    }
    if found.len() > MAX_OBJECTS {
        return Err(invalid(format!(
            "labelSelector {selector} matches more than {MAX_OBJECTS} objects, and a batch is \
             never cut to fit: narrow it or name fewer namespaces"
        )));
    }
    plan.seen
        .extend(found.iter().filter_map(|(_, obj)| obj.metadata.uid.clone()));
    let verdicts: Vec<_> = stream::iter(found)
        .map(|(namespace, obj)| async move {
            let name = obj.name_any();
            let label = format!("{} {namespace}/{name}", kind.kind);
            let verdict = admit(
                ctx,
                op,
                kind.clone(),
                namespace.clone(),
                name,
                replicas,
                Some(obj),
            )
            .await;
            (label, namespace, verdict)
        })
        .buffer_unordered(CONCURRENT_CALLS)
        .collect()
        .await;
    for (label, namespace, verdict) in verdicts {
        plan.add(ctx, label, &namespace, verdict)?;
    }
    for (why, names) in &plan.coverage.gaps {
        for namespace in names {
            let args = json!({"namespace": namespace, "labelSelector": selector});
            let reason = Some(why.clone());
            audit(
                ctx,
                args,
                namespace,
                DecidedBy::Policy,
                false,
                reason,
                "left-out",
            );
        }
    }
    Ok(())
}

/// Finds the objects of `selection`, each through the single call's checks.
async fn resolve(ctx: &Ctx<'_>, op: Op, selection: Selection) -> Result<Plan, ToolError> {
    let mut plan = Plan::default();
    match selection {
        Selection::Objects(wanted) => resolve_objects(ctx, op, wanted, &mut plan).await?,
        Selection::Selector {
            kind,
            selector,
            place,
            replicas,
        } => resolve_selector(ctx, op, &kind, &selector, place, replicas, &mut plan).await?,
    }
    plan.targets
        .sort_by(|a, b| (&a.namespace, &a.name).cmp(&(&b.namespace, &b.name)));
    let mut uids = HashSet::new();
    for target in plan.targets.iter().chain(&plan.unchanged) {
        if !uids.insert(target.uid.as_str()) {
            return Err(invalid(format!("{} is listed twice", target.label())));
        }
    }
    Ok(plan)
}

/// The window for `plan`.
fn question(ctx: &Ctx<'_>, op: Op, plan: &Plan) -> ConfirmationRequest {
    let session = ctx.session;
    let namespaces = plan.namespaces();
    let (action, verb) = match op {
        Op::Scale => (BatchAction::Scale, "scale"),
        Op::Restart => (BatchAction::Restart, "restart"),
    };
    let batch = BatchChange {
        action,
        objects: plan.targets.iter().map(Target::batch_object).collect(),
        left_out: plan.left_out_lines(),
    };
    let mut details = String::new();
    for object in &batch.objects {
        let change = object.change().unwrap_or_default();
        let _ = writeln!(details, "{} {change}", object.label());
    }
    // Where any namespace rests on Kubach alone, the window says so.
    let mode = namespaces
        .iter()
        .map(|namespace| session.mode_in(namespace))
        .find(|mode| !mode.server_enforced())
        .unwrap_or_else(|| session.mode_in(namespaces.first().copied().unwrap_or_default()));
    ConfirmationRequest {
        session_id: session.id().to_owned(),
        context: session.scope().context.clone(),
        client: ctx.client_name.to_owned(),
        tool: op.tool().to_owned(),
        namespace: match namespaces.as_slice() {
            [only] => (*only).to_owned(),
            _ => String::new(),
        },
        summary: format!(
            "{} wants to {verb} {} objects in {} namespaces",
            ctx.client_name,
            batch.objects.len(),
            namespaces.len()
        ),
        details,
        dangerous: Vec::new(),
        mode,
        allow_remember: false,
        kind: RequestKind::Batch {
            batch: Box::new(batch),
        },
        diff_cut: None,
    }
}

/// The user's answer about `plan`, unless the single call's "don't ask
/// again" already covers exactly this: the same change in one namespace.
async fn approve(ctx: &Ctx<'_>, op: Op, plan: &Plan) -> Result<DecidedBy, ToolError> {
    let request = question(ctx, op, plan);
    let single = ConfirmationRequest {
        kind: RequestKind::Call,
        tool: op.single().to_owned(),
        ..request.clone()
    };
    if let Some(remembered) = rememberable(&single)
        && ctx.session.is_remembered(&remembered)
    {
        *sync::lock(&ctx.remembered) = Some(RememberNote::Used(remembered));
        return Ok(DecidedBy::Remembered);
    }
    approve_change(
        ctx,
        request,
        "the session ended while waiting for confirmation",
    )
    .await?;
    Ok(DecidedBy::User)
}

/// What became of one approved object.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// With the replicas it had right before the change: an agent rolls
    /// back to these, not to the window's.
    Changed {
        original: Option<i64>,
    },
    Failed {
        reason: String,
        code: String,
    },
    /// Not the object the user approved, or no longer allowed.
    NotTouched {
        reason: String,
    },
}

fn failed(error: &ToolError) -> Outcome {
    let code = match error {
        ToolError::Api {
            code: Some(code), ..
        } => code.to_string(),
        _ => "error".to_owned(),
    };
    Outcome::Failed {
        reason: error.message(),
        code,
    }
}

async fn change(ctx: &Ctx<'_>, op: Op, target: &Target) -> Outcome {
    // The answer may have taken minutes: rights may have gone meanwhile.
    let allowed = match ctx.session.policy().check(&target.request) {
        Ok(allowed) => allowed,
        Err(denial) => {
            return Outcome::NotTouched {
                reason: denial.to_string(),
            };
        }
    };
    let client = match ctx.session.client_in(&target.namespace) {
        Ok(client) => client,
        Err(error) => return failed(&error.into()),
    };
    let api = discovery::api(&client, &target.kind, Some(&target.namespace));
    let read = || async { api.get(&target.name).await.map_err(ToolError::from) };
    let now = match settled(ctx, &allowed, read).await {
        Ok(now) => now,
        Err(error) => return failed(&error),
    };
    if now.metadata.uid.as_deref() != Some(target.uid.as_str()) {
        return Outcome::NotTouched {
            reason: "another object of this name replaced the approved one".to_owned(),
        };
    }
    let original = target.from.map(|_| replicas_of(&now));
    let send = || async {
        let (name, uid) = (&target.name, &target.uid);
        match (op, target.to.map(i32::try_from)) {
            (Op::Scale, Some(Ok(to))) => scale_object(&api, name, uid, to).await,
            (Op::Restart, _) => restart_object(&api, name, uid).await,
            (Op::Scale, _) => return Err(ToolError::Failed("no replica count".into())),
        }
        .map_err(ToolError::from)
    };
    match settled(ctx, &allowed, send).await {
        Ok(()) => Outcome::Changed { original },
        Err(error) => failed(&error),
    }
}

/// Changes the approved objects, `limit` at a time. Each is audited as it
/// ends: a client that drops the call does not lose what was done.
async fn run_all(
    ctx: &Ctx<'_>,
    op: Op,
    targets: Vec<Target>,
    limit: usize,
    by: DecidedBy,
) -> Vec<(Target, Outcome)> {
    let mut done: Vec<(Target, Outcome)> = stream::iter(targets)
        .map(|target| async move {
            let outcome = change(ctx, op, &target).await;
            let mut args = target.args();
            let (allowed, reason, result) = match &outcome {
                Outcome::Changed { original } => {
                    args["originalReplicas"] = json!(original);
                    (true, None, "ok".to_owned())
                }
                Outcome::Failed { reason, code } => (true, Some(reason.clone()), code.clone()),
                Outcome::NotTouched { reason } => {
                    (false, Some(reason.clone()), "not-touched".to_owned())
                }
            };
            audit(ctx, args, &target.namespace, by, allowed, reason, &result);
            (target, outcome)
        })
        .buffer_unordered(limit)
        .collect()
        .await;
    done.sort_by(|(a, _), (b, _)| (&a.namespace, &a.name).cmp(&(&b.namespace, &b.name)));
    done
}

/// What matches the selector now and was not found before the question:
/// reported, never touched. Namespaces that cannot be read again are named.
async fn latecomers(
    ctx: &Ctx<'_>,
    kind: &KindRef,
    selector: &str,
    plan: &Plan,
) -> (Vec<String>, Vec<String>) {
    let mut unchecked = Vec::new();
    let mut late = Vec::new();
    for namespace in &plan.looked_in {
        let request = list_request(kind, selector, namespace);
        let Ok(allowed) = ctx.session.policy().check(&request) else {
            unchecked.push(namespace.clone());
            continue;
        };
        let Ok(found) = matching(ctx, &allowed, selector).await else {
            unchecked.push(namespace.clone());
            continue;
        };
        for obj in found {
            let uid = obj.metadata.uid.clone().unwrap_or_default();
            if plan.seen.contains(&uid) {
                continue;
            }
            let kind = &kind_of(&allowed).api.kind;
            let label = format!("{kind} {namespace}/{}", obj.name_any());
            let args = json!({"object": label, "uid": uid});
            let reason = Some("appeared after the user's answer".to_owned());
            audit(
                ctx,
                args,
                namespace,
                DecidedBy::None,
                false,
                reason,
                "not-touched",
            );
            late.push(label);
        }
    }
    (late, unchecked)
}

/// What the agent gets back: the verdict first, named gaps next, then every
/// object. Nothing here reads as complete unless it is.
#[derive(Default)]
struct Report {
    tool: &'static str,
    done: Vec<(Target, Outcome)>,
    unchanged: Vec<Target>,
    left_out: Vec<String>,
    /// Why other namespaces may exist.
    unknown: Option<String>,
    late: Vec<String>,
    unchecked: Vec<String>,
    /// The answer about opening namespaces, if this call asked.
    question: Option<&'static str>,
}

impl Report {
    fn count(&self, what: impl Fn(&Outcome) -> bool) -> usize {
        self.done.iter().filter(|(_, o)| what(o)).count()
    }

    fn changed(&self) -> usize {
        self.count(|o| matches!(o, Outcome::Changed { .. }))
    }

    fn failed(&self) -> usize {
        self.count(|o| matches!(o, Outcome::Failed { .. }))
    }

    fn is_complete(&self) -> bool {
        self.changed() == self.done.len()
            && self.left_out.is_empty()
            && self.unknown.is_none()
            && self.late.is_empty()
            && self.unchecked.is_empty()
    }

    /// The arguments that put back the replicas of what was scaled.
    fn rollback(&self) -> Option<String> {
        let objects: Vec<Value> = self
            .done
            .iter()
            .filter_map(|(target, outcome)| match outcome {
                Outcome::Changed {
                    original: Some(original),
                } => Some(json!({
                    "kind": target.kind.api.kind,
                    "apiVersion": target.kind.api.api_version,
                    "namespace": target.namespace,
                    "name": target.name,
                    "replicas": original,
                })),
                _ => None,
            })
            .collect();
        (!objects.is_empty()).then(|| json!({"objects": objects}).to_string())
    }

    fn heading(&self) -> String {
        let (changed, approved, failed) = (self.changed(), self.done.len(), self.failed());
        let already = self.unchanged.len();
        if !self.is_complete() {
            let untouched = approved - changed - failed;
            format!(
                "{}: PARTIAL, not everything asked for was done. {changed} of {approved} approved \
                 objects changed, {failed} failed, {untouched} not touched; {already} already as \
                 asked.",
                self.tool
            )
        } else if approved + already == 0 {
            format!("{}: nothing to change, no object matched.", self.tool)
        } else {
            format!(
                "{}: complete. {changed} objects changed, {already} already as asked.",
                self.tool
            )
        }
    }

    fn text(&self) -> String {
        let mut out = format!("{}\n", self.heading());
        if let Some(rollback) = self.rollback() {
            let _ = writeln!(out, "To roll back, call scale_batch with: {rollback}");
        }
        if !self.left_out.is_empty() {
            let _ = writeln!(out, "Left out, not changed:");
            for line in &self.left_out {
                let _ = writeln!(out, "- {line}");
            }
        }
        if !self.late.is_empty() {
            let _ = writeln!(
                out,
                "Appeared after the user's answer, not touched: {}",
                self.late.join(", ")
            );
        }
        if !self.unchecked.is_empty() {
            let _ = writeln!(
                out,
                "Could not look again for objects that appeared later in: {}",
                self.unchecked.join(", ")
            );
        }
        if let Some(unknown) = &self.unknown {
            let _ = writeln!(out, "Other namespaces may exist: {unknown}");
        }
        if !self.done.is_empty() || !self.unchanged.is_empty() {
            let _ = writeln!(out, "\nRESULT\tOBJECT\tORIGINAL\tNOW\tNOTE");
        }
        let number = |n: Option<i64>| n.map_or_else(|| "-".to_owned(), |n| n.to_string());
        for (target, outcome) in &self.done {
            let (result, original, now, note) = match outcome {
                Outcome::Changed { original } => ("changed", *original, target.to, ""),
                Outcome::Failed { reason, .. } => ("failed", target.from, None, reason.as_str()),
                Outcome::NotTouched { reason } => {
                    ("not-touched", target.from, None, reason.as_str())
                }
            };
            let _ = writeln!(
                out,
                "{result}\t{}\t{}\t{}\t{note}",
                target.label(),
                number(original),
                number(now)
            );
        }
        for target in &self.unchanged {
            let _ = writeln!(
                out,
                "unchanged\t{}\t{}\t{}\t",
                target.label(),
                number(target.from),
                number(target.to)
            );
        }
        out
    }

    fn audit(&self) -> Value {
        let labels = |what: &dyn Fn(&Outcome) -> bool| -> Vec<String> {
            self.done
                .iter()
                .filter(|(_, o)| what(o))
                .map(|(t, _)| t.label())
                .collect()
        };
        json!({
            "changed": labels(&|o| matches!(o, Outcome::Changed { .. })),
            "failed": labels(&|o| matches!(o, Outcome::Failed { .. })),
            "notTouched": labels(&|o| matches!(o, Outcome::NotTouched { .. })),
            "unchanged": self.unchanged.iter().map(Target::label).collect::<Vec<_>>(),
            "leftOut": self.left_out,
            "appeared": self.late,
            "complete": self.is_complete(),
            "question": self.question,
        })
    }
}

/// Asks for the access a change needs once, before any namespace: refused,
/// the whole call ends here and nothing was read.
async fn ensure_access(ctx: &Ctx<'_>, op: Op) -> Result<(), ToolError> {
    let probe = op.request(KindRef::new("Deployment"), String::new(), None, None);
    let Some(needed) = Policy::needed_access(&probe) else {
        return Ok(());
    };
    let verdict = ctx
        .session
        .policy()
        .within_access(needed, "changing resources");
    match verdict {
        Ok(()) => Ok(()),
        Err(denial) if denial.code == DenialCode::NeedsAccess => {
            let Some(raise) = denial.access else {
                return Err(denial.into());
            };
            ask_access(ctx, &probe, raise).await
        }
        Err(denial) => Err(denial.into()),
    }
}

async fn run(ctx: &Ctx<'_>, op: Op, args: &Map<String, Value>) -> Result<ToolOutput, ToolError> {
    let a: BatchArgs = parse(args)?;
    let limit = concurrency(op, a.concurrency)?;
    ensure_access(ctx, op).await?;
    let selection = {
        let policy = ctx.session.policy();
        selection(op, a, || policy.namespace(None))?
    };
    let selector = match &selection {
        Selection::Selector { kind, selector, .. } => Some((kind.clone(), selector.clone())),
        Selection::Objects(_) => None,
    };
    let plan = resolve(ctx, op, selection).await?;
    let asked = !plan.targets.is_empty();
    let by = if asked {
        match approve(ctx, op, &plan).await {
            Ok(by) => by,
            Err(error) => {
                for target in &plan.targets {
                    let (args, by) = (target.args(), decided_by(&error));
                    let reason = Some(error.message());
                    audit(ctx, args, &target.namespace, by, false, reason, "rejected");
                }
                return Err(error);
            }
        }
    } else {
        DecidedBy::None
    };
    let done = run_all(ctx, op, plan.targets.clone(), limit, by).await;
    let (late, unchecked) = match &selector {
        Some((kind, selector)) if asked => latecomers(ctx, kind, selector, &plan).await,
        _ => (Vec::new(), Vec::new()),
    };
    if plan.namespaces().len() != 1 {
        forget_namespace(ctx);
    }
    let report = Report {
        tool: op.tool(),
        done,
        left_out: plan.left_out_lines(),
        unknown: plan.coverage.unknown.clone(),
        question: plan.coverage.answer,
        unchanged: plan.unchanged,
        late,
        unchecked,
    };
    let text = finish(&report.text());
    if report.changed() == 0 && report.failed() > 0 {
        return Err(ToolError::Failed(text));
    }
    Ok(ToolOutput {
        confirmed: asked,
        audit: Some(report.audit()),
        ..ToolOutput::read(text)
    })
}

pub(crate) async fn scale(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    run(ctx, Op::Scale, args).await
}

pub(crate) async fn restart(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    run(ctx, Op::Restart, args).await
}
