//! Node maintenance for the agent (#105): `cordon_node`, `uncordon_node`
//! and `drain_node`.
//!
//! A write on a node is beyond any session identity: a temporary
//! ServiceAccount has a Role, never a ClusterRole (ADR 0005). So these
//! calls run with the user's credentials, for this one call, like a Helm
//! chart with cluster-scoped objects (#18). What stands in front of them:
//! the policy (access to nodes and changes, asked for as any access; never
//! in a pinned session), the user's RBAC before anyone is asked, and a
//! window for every call that names the node and every pod, needs its own
//! tick and is never remembered.
//!
//! A drain evicts exactly the pods the user approved, by UID: a pod that
//! reached the node after the window is reported, not evicted. Pods go
//! through the Eviction API only, so PodDisruptionBudgets hold, and there
//! is no force: pods without a controller stay.
//!
//! The window lists everything; the agent's text names pods only in
//! namespaces the session may read and counts the rest.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use k8s_openapi::api::core::v1::{Node, Pod};
use k8s_openapi::api::policy::v1::PodDisruptionBudget;
use kubach_core::drain::{
    Block, DEFAULT_TIMEOUT, DrainOptions, DrainPlan, PodKey, Verdict, budget_refusal,
    eviction_plan, has_empty_dir, remove_pods, set_cordoned,
};
use kubach_core::rbac::ResourceAccess;
use kubach_core::sync;
use kube::api::{Api, ListParams, PatchParams};
use kube::{Client, ResourceExt as _};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::confirm::RequestKind;
use crate::grants::{Grant, GrantState};
use crate::policy::{Denial, DenialCode, Finding, Request};
use crate::tools::{
    Ctx, FIELD_MANAGER, ToolError, ToolOutput, authorize, confirm_as, ensure_rbac, finish, parse,
};

/// Who runs a call on a node, for the audit log.
pub const NODE_USER_IDENTITY: &str =
    "user: your kubeconfig credentials, allowed for this node call only";

/// How often a running drain looks whether the session ended.
const SESSION_POLL: Duration = Duration::from_millis(200);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NodeArgs {
    name: String,
}

/// No `force`: the unknown field is refused like any other (#105).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DrainArgs {
    name: String,
    #[serde(default)]
    ignore_daemonsets: bool,
    #[serde(default)]
    delete_empty_dir_data: bool,
    pod_selector: Option<String>,
    timeout_seconds: Option<u64>,
    #[serde(default)]
    preview: bool,
}

/// Refuses what the user's RBAC does not allow on the cluster level: more
/// access would not help, so nobody is asked.
async fn ensure_cluster_rbac(ctx: &Ctx<'_>, accesses: &[(&str, &str)]) -> Result<(), ToolError> {
    for (verb, resource) in accesses {
        if !ctx
            .session
            .user_can(&ResourceAccess::new(*verb, "", resource))
            .await
        {
            return Err(Denial::new(
                DenialCode::Rbac,
                format!(
                    "your Kubernetes account has no right to {verb} {resource} in the cluster; \
                     Kubach cannot grant RBAC rights. Ask the cluster administrator"
                ),
            )
            .into());
        }
    }
    Ok(())
}

/// Why the window needs its own tick.
fn as_user_finding(node: &str) -> Finding {
    Finding {
        document: None,
        path: format!("node/{node}"),
        message: "runs with your own kubeconfig credentials, not the session's identity, and \
                  reaches every namespace on the node"
            .into(),
    }
}

fn write_params() -> PatchParams {
    PatchParams {
        field_manager: Some(FIELD_MANAGER.to_owned()),
        ..PatchParams::default()
    }
}

fn is_cordoned(node: &Node) -> bool {
    node.spec.as_ref().and_then(|s| s.unschedulable) == Some(true)
}

/// `cordon_node` (`cordon`) and `uncordon_node`.
pub(crate) async fn cordon_node(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
    cordon: bool,
) -> Result<ToolOutput, ToolError> {
    let a: NodeArgs = parse(args)?;
    let allowed = authorize(
        ctx,
        &Request::NodeCordon {
            name: a.name.clone(),
            cordon,
        },
    )
    .await?;
    ensure_cluster_rbac(ctx, &[("get", "nodes"), ("patch", "nodes")]).await?;
    let client = ctx.session.user_client();
    let node = Api::<Node>::all(client.clone()).get(&a.name).await?;
    if is_cordoned(&node) == cordon {
        let state = if cordon { "cordoned" } else { "schedulable" };
        return Ok(ToolOutput::read(format!(
            "Node {} is already {state}: nothing changed, the user was not asked.",
            a.name
        )));
    }
    let (summary, details) = if cordon {
        (
            format!("Cordon node {} as your kubeconfig user", a.name),
            "No new pods are scheduled on the node. Running pods stay.",
        )
    } else {
        (
            format!("Uncordon node {} as your kubeconfig user", a.name),
            "The scheduler may place pods on the node again.",
        )
    };
    confirm_as(
        ctx,
        RequestKind::Call,
        &allowed,
        ctx.tool,
        summary.clone(),
        details.to_owned(),
        vec![as_user_finding(&a.name)],
    )
    .await?;
    *sync::lock(&ctx.identity) = Some(NODE_USER_IDENTITY.to_owned());
    set_cordoned(&client, &a.name, cordon, &write_params()).await?;
    Ok(ToolOutput {
        audit: Some(json!({"node": a.name, "cordoned": cordon})),
        ..ToolOutput::confirmed(format!("Done: {summary}"))
    })
}

/// The pods of `node` a drain considers, in every namespace.
async fn selected_pods(
    client: &Client,
    node: &str,
    selector: Option<&str>,
) -> Result<Vec<Pod>, kube::Error> {
    let mut params = ListParams::default().fields(&format!("spec.nodeName={node}"));
    if let Some(selector) = selector {
        params = params.labels(selector);
    }
    Ok(Api::<Pod>::all(client.clone()).list(&params).await?.items)
}

/// Which part of the report a pod belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Part {
    Evict,
    Budget,
    NoController,
    Waited,
    Stays,
    Stops,
}

impl Part {
    const ALL: [Self; 6] = [
        Self::Evict,
        Self::Budget,
        Self::NoController,
        Self::Waited,
        Self::Stays,
        Self::Stops,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Evict => "To evict",
            Self::Budget => {
                "Blocked by a PodDisruptionBudget now (retried until the timeout, never forced)"
            }
            Self::NoController => "No controller, nothing recreates them: left in place",
            Self::Waited => "Already being deleted: only waited for",
            Self::Stays => "Left on the node",
            Self::Stops => "Stop the drain, nothing changes while they are here",
        }
    }
}

/// One pod of a drain with what is known about it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PodLine {
    label: String,
    namespace: String,
    part: Part,
    /// The reason after the name, if any.
    note: Option<String>,
}

/// A drain as read from the cluster.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct DrainView {
    lines: Vec<PodLine>,
    /// Namespaces whose budgets could not be read: the eviction decides.
    unread_budgets: Vec<String>,
}

impl DrainView {
    fn count(&self, part: Part) -> usize {
        self.lines.iter().filter(|l| l.part == part).count()
    }

    /// What goes on the report, by part; pods of namespaces `shown` says
    /// no to are only counted.
    fn text(&self, shown: &dyn Fn(&str) -> bool) -> String {
        let mut out = String::new();
        let mut hidden: BTreeMap<Part, usize> = BTreeMap::new();
        for part in Part::ALL {
            let mine: Vec<&PodLine> = self.lines.iter().filter(|l| l.part == part).collect();
            let (named, others): (Vec<&PodLine>, Vec<&PodLine>) =
                mine.iter().partition(|l| shown(&l.namespace));
            if !others.is_empty() {
                hidden.insert(part, others.len());
            }
            if named.is_empty() {
                continue;
            }
            let _ = writeln!(out, "{} ({}):", part.title(), named.len());
            for line in named {
                match &line.note {
                    Some(note) => {
                        let _ = writeln!(out, "  {}: {note}", line.label);
                    }
                    None => {
                        let _ = writeln!(out, "  {}", line.label);
                    }
                }
            }
        }
        if !hidden.is_empty() {
            let counts: Vec<String> = hidden
                .iter()
                .map(|(part, n)| format!("{n} ({})", part.title()))
                .collect();
            let _ = writeln!(
                out,
                "In namespaces this session may not read: {}",
                counts.join(", ")
            );
        }
        let unread: Vec<&str> = self
            .unread_budgets
            .iter()
            .map(String::as_str)
            .filter(|ns| shown(ns))
            .collect();
        if !unread.is_empty() {
            let _ = writeln!(
                out,
                "PodDisruptionBudgets not readable in {}: the eviction decides",
                unread.join(", ")
            );
        }
        if out.is_empty() {
            out.push_str("No pods selected on this node.\n");
        }
        out
    }
}

/// The report's lines for `plan`; `refusals` are the budget refusals by
/// pod label.
fn lines(plan: &DrainPlan, pods: &[Pod], refusals: &BTreeMap<String, String>) -> Vec<PodLine> {
    plan.pods
        .iter()
        .map(|(key, verdict)| {
            let label = key.label();
            let pod = pods.iter().find(|pod| pod.uid() == key.uid);
            let (part, note) = match verdict {
                // No direct delete in an eviction plan.
                Verdict::Evict | Verdict::Delete => {
                    let data = pod
                        .is_some_and(has_empty_dir)
                        .then(|| "its emptyDir data is lost".to_owned());
                    match refusals.get(&label) {
                        Some(refusal) => (Part::Budget, Some(refusal.clone())),
                        None => (Part::Evict, data),
                    }
                }
                Verdict::Wait => (Part::Waited, None),
                Verdict::Skip(skip) => (Part::Stays, Some(skip.describe().english())),
                Verdict::Block(Block::NoController) => (Part::NoController, None),
                Verdict::Block(block) => (Part::Stops, Some(block.describe().english())),
            };
            PodLine {
                namespace: key.namespace.clone(),
                label,
                part,
                note,
            }
        })
        .collect()
}

/// Budgets that refuse a pod of `plan` now, by pod label, and the
/// namespaces whose budgets are not readable. Read namespace by namespace:
/// the user may have that right only in some.
async fn budget_refusals(
    client: &Client,
    plan: &DrainPlan,
    pods: &[Pod],
) -> (BTreeMap<String, String>, Vec<String>) {
    let targets: Vec<&PodKey> = plan
        .pods
        .iter()
        .filter(|(_, verdict)| *verdict == Verdict::Evict)
        .map(|(key, _)| key)
        .collect();
    let namespaces: BTreeSet<&str> = targets.iter().map(|key| key.namespace.as_str()).collect();
    let mut refusals = BTreeMap::new();
    let mut unread = Vec::new();
    for namespace in namespaces {
        let api: Api<PodDisruptionBudget> = Api::namespaced(client.clone(), namespace);
        let budgets = match api.list(&ListParams::default()).await {
            Ok(budgets) => budgets.items,
            Err(_) => {
                unread.push(namespace.to_owned());
                continue;
            }
        };
        for key in targets.iter().filter(|key| key.namespace == namespace) {
            let refusal = pods
                .iter()
                .find(|pod| pod.uid() == key.uid)
                .and_then(|pod| budgets.iter().find_map(|b| budget_refusal(b, pod)));
            if let Some(refusal) = refusal {
                refusals.insert(key.label(), refusal.to_string());
            }
        }
    }
    (refusals, unread)
}

/// Whether the agent gets to see names in `namespace`: the namespaces it
/// could read with one call, so pinned ones, or any the user did not take
/// back.
fn may_name(ctx: &Ctx<'_>, namespace: &str) -> bool {
    let scope = ctx.session.scope();
    if scope.pinned() {
        return scope.namespaces.iter().any(|n| n == namespace);
    }
    ctx.session
        .grants()
        .state(&Grant::read(namespace.to_owned()))
        != Some(GrantState::Denied)
}

/// One line per option of the drain, for the window.
fn options_text(options: &DrainOptions, selector: Option<&str>) -> String {
    let yes_no = |on: bool| if on { "yes" } else { "no" };
    format!(
        "Pods: {}\nIgnore DaemonSets: {}\nDelete emptyDir data: {}\nTimeout: {}s\nThe node stays \
         cordoned afterwards.\n",
        selector.map_or("all on the node".to_owned(), |s| format!("labels {s}")),
        yes_no(options.ignore_daemonsets),
        yes_no(options.delete_emptydir_data),
        options.timeout.as_secs(),
    )
}

/// Pods of `pods` a drain would go for that the user did not approve.
fn not_approved(pods: &[Pod], options: &DrainOptions, approved: &[PodKey]) -> Vec<PodKey> {
    let uids: BTreeSet<_> = approved.iter().map(|key| &key.uid).collect();
    eviction_plan(pods, options)
        .removed()
        .filter(|(key, _)| !uids.contains(&key.uid))
        .map(|(key, _)| key.clone())
        .collect()
}

/// `drain_node`: the preview, or after the user's yes: cordon, evict what
/// they saw, wait until it is gone.
pub(crate) async fn drain_node(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    let a: DrainArgs = parse(args)?;
    let allowed = authorize(
        ctx,
        &Request::NodeDrain {
            name: a.name.clone(),
            pod_selector: a.pod_selector.clone(),
            timeout_seconds: a.timeout_seconds,
            preview: a.preview,
        },
    )
    .await?;
    if !a.preview {
        ensure_cluster_rbac(
            ctx,
            &[("get", "nodes"), ("patch", "nodes"), ("list", "pods")],
        )
        .await?;
    }
    let client = ctx.session.user_client();
    let node = Api::<Node>::all(client.clone()).get(&a.name).await?;
    let options = DrainOptions {
        ignore_daemonsets: a.ignore_daemonsets,
        delete_emptydir_data: a.delete_empty_dir_data,
        force: false,
        timeout: a
            .timeout_seconds
            .map_or(DEFAULT_TIMEOUT, Duration::from_secs),
        namespace: None,
    };
    let selector = a.pod_selector.as_deref();
    let pods = selected_pods(&client, &a.name, selector).await?;
    let plan = eviction_plan(&pods, &options);
    let (refusals, unread_budgets) = budget_refusals(&client, &plan, &pods).await;
    let view = DrainView {
        lines: lines(&plan, &pods, &refusals),
        unread_budgets,
    };
    let for_agent = view.text(&|namespace| may_name(ctx, namespace));
    if a.preview {
        let state = if is_cordoned(&node) {
            "already cordoned"
        } else {
            "not cordoned yet"
        };
        return Ok(ToolOutput::read(finish(&format!(
            "Preview of draining node {} ({state}):\n{for_agent}Budgets are as of now: one that \
             allows fewer disruptions than it has pods here holds the rest back until the \
             timeout. Nothing was changed and the user was not asked. Call again without preview \
             to do it; the user then sees every pod and approves.",
            a.name
        ))));
    }
    if view.count(Part::Stops) > 0 {
        return Err(Denial::new(
            DenialCode::InvalidInput,
            format!(
                "nothing changed on node {}, the user was not asked: pass ignoreDaemonsets or \
                 deleteEmptyDirData, or narrow podSelector, for the pods that stop the drain.\n\
                 {for_agent}",
                a.name
            ),
        )
        .into());
    }
    let work: Vec<(PodKey, Verdict)> = plan.removed().cloned().collect();
    let namespaces: BTreeSet<&str> = work.iter().map(|(key, _)| key.namespace.as_str()).collect();
    for namespace in namespaces {
        ensure_rbac(ctx, namespace, &[("create", "", "pods/eviction")]).await?;
    }
    let evicting = view.count(Part::Evict) + view.count(Part::Budget);
    let mut dangerous = vec![as_user_finding(&a.name)];
    dangerous.extend(
        plan.pods
            .iter()
            .filter(|(_, verdict)| *verdict == Verdict::Evict)
            .filter(|(key, _)| {
                pods.iter()
                    .any(|pod| pod.uid() == key.uid && has_empty_dir(pod))
            })
            .map(|(key, _)| Finding {
                document: None,
                path: key.label(),
                message: "loses its emptyDir data".into(),
            }),
    );
    confirm_as(
        ctx,
        RequestKind::Call,
        &allowed,
        ctx.tool,
        format!(
            "Drain node {}: cordon it and evict {evicting} pods as your kubeconfig user",
            a.name
        ),
        format!(
            "{}\n{}",
            options_text(&options, selector),
            view.text(&|_| true)
        ),
        dangerous,
    )
    .await?;
    *sync::lock(&ctx.identity) = Some(NODE_USER_IDENTITY.to_owned());
    set_cordoned(&client, &a.name, true, &write_params()).await?;

    // Scheduled between the window and the cordon: not what the user saw.
    let approved: Vec<PodKey> = work.iter().map(|(key, _)| key.clone()).collect();
    let late = match selected_pods(&client, &a.name, selector).await {
        Ok(now) => not_approved(&now, &options, &approved),
        Err(_) => Vec::new(),
    };

    let total = work.len();
    let cancel = Arc::new(AtomicBool::new(false));
    let finished = AtomicBool::new(false);
    let mut done = 0;
    let removing = async {
        let summary = remove_pods(&client, work, options.timeout, cancel.clone(), || {
            done += 1;
            if let Some(progress) = &ctx.progress {
                progress(format!("{done} of {total} pods gone"));
            }
        })
        .await;
        finished.store(true, Ordering::Relaxed);
        summary
    };
    // Rights end with the session: no eviction after it.
    let watching = async {
        while !finished.load(Ordering::Relaxed) {
            if !ctx.session.is_active() {
                cancel.store(true, Ordering::Relaxed);
                return;
            }
            tokio::time::sleep(SESSION_POLL).await;
        }
    };
    let (summary, ()) = tokio::join!(removing, watching);

    let failed: BTreeSet<&str> = summary.failures.iter().map(|f| f.object.as_str()).collect();
    let removed: Vec<String> = approved
        .iter()
        .map(PodKey::label)
        .filter(|label| !failed.contains(label.as_str()))
        .collect();
    let left: Vec<&PodLine> = view
        .lines
        .iter()
        .filter(|line| line.part == Part::NoController)
        .collect();
    let shown = |namespace: &str| may_name(ctx, namespace);
    let complete = summary.failures.is_empty() && left.is_empty() && late.is_empty();
    let mut out = format!(
        "Node {} is cordoned. {} of {total} pods are gone.\n",
        a.name, summary.succeeded
    );
    for failure in &summary.failures {
        match failure.object.split_once('/') {
            Some((namespace, _)) if shown(namespace) => {
                let _ = writeln!(
                    out,
                    "Not evicted: {}: {}",
                    failure.object,
                    failure.reason.english()
                );
            }
            _ => {
                let _ = writeln!(out, "Not evicted: a pod of a namespace not read here");
            }
        }
    }
    if !left.is_empty() {
        let names: Vec<&str> = left
            .iter()
            .filter(|line| shown(&line.namespace))
            .map(|line| line.label.as_str())
            .collect();
        let _ = writeln!(
            out,
            "Left in place, no controller ({}): {}",
            left.len(),
            names.join(", ")
        );
    }
    if !late.is_empty() {
        let names: Vec<String> = late
            .iter()
            .filter(|key| shown(&key.namespace))
            .map(PodKey::label)
            .collect();
        let _ = writeln!(
            out,
            "Reached the node after the user approved, not evicted ({}): {}",
            late.len(),
            names.join(", ")
        );
    }
    out.push_str(if complete {
        "Drained. uncordon_node makes it schedulable again."
    } else {
        "The drain is not complete. The node stays cordoned; uncordon_node makes it schedulable \
         again."
    });
    Ok(ToolOutput {
        audit: Some(json!({
            "node": a.name,
            "cordoned": true,
            "selector": a.pod_selector,
            "evicted": removed,
            "failed": summary.failures.iter().map(|failure| json!({
                "pod": failure.object,
                "reason": failure.reason.english(),
            })).collect::<Vec<_>>(),
            "leftInPlace": left.iter().map(|line| &line.label).collect::<Vec<_>>(),
            "notApproved": late.iter().map(PodKey::label).collect::<Vec<_>>(),
        })),
        ..ToolOutput::confirmed(finish(&out))
    })
}

#[cfg(test)]
mod tests;
