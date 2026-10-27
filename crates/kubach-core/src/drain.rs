//! Node drain, like `kubectl drain`: cordon the node, then evict its pods
//! through the Eviction API ([`evict_pod`]) and wait until they are gone.
//!
//! Which pods go and which stay is decided by [`plan`], a pure function of
//! the pods and the [`DrainOptions`], the same rules as kubectl:
//!
//! - mirror (static) pods are skipped: the kubelet owns them;
//! - DaemonSet pods are skipped with `ignore_daemonsets`, otherwise they
//!   block the drain (the DaemonSet would put them right back);
//! - finished pods (`Succeeded` / `Failed`) are deleted, no budget to honour;
//! - pods without a controller block the drain unless `force` (nothing
//!   recreates them);
//! - pods with `emptyDir` volumes block the drain unless
//!   `delete_emptydir_data` (their data is lost);
//! - pods already being deleted are only waited for.
//!
//! A blocked pod stops the drain before anything is evicted, as in kubectl;
//! the node stays cordoned. A PodDisruptionBudget refusal is retried with
//! backoff until the timeout.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use k8s_openapi::api::core::v1::{Node, Pod};
use k8s_openapi::api::policy::v1::PodDisruptionBudget;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use kubach_i18n::{Message, msg};
use kube::api::{Api, DeleteParams, ListParams, Patch, PatchParams};
use kube::{Client, ResourceExt as _};
use tokio::time::Instant;

use crate::deletion::{self, DeleteError};
use crate::eviction::{EvictError, Evicted, evict_pod};
use crate::plugin::bulk::{BulkFailure, BulkSummary, error_text, run_bounded};

/// Default of [`DrainOptions::timeout`], kubectl's `--timeout=5m` as
/// Kubach offers it.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Annotation the kubelet puts on mirror pods of static pods.
pub const MIRROR_ANNOTATION: &str = "kubernetes.io/config.mirror";

/// Pods evicted at once.
const CONCURRENCY: usize = 32;
/// First pause after a PodDisruptionBudget refusal; doubles up to
/// [`BACKOFF_MAX`].
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(10);
/// How often an evicted pod is checked for being gone.
const GONE_POLL: Duration = Duration::from_secs(1);

/// The flags of `kubectl drain` Kubach supports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrainOptions {
    /// `--ignore-daemonsets`: skip DaemonSet pods instead of refusing.
    pub ignore_daemonsets: bool,
    /// `--delete-emptydir-data`: evict pods with `emptyDir` volumes.
    pub delete_emptydir_data: bool,
    /// `--force`: evict pods no controller recreates.
    pub force: bool,
    /// `--timeout`: for the whole drain, PDB retries and waiting included.
    pub timeout: Duration,
    /// Only pods of this namespace (`--pod-selector` has no namespace
    /// filter; this is for tests that must not touch system pods).
    pub namespace: Option<String>,
}

impl Default for DrainOptions {
    fn default() -> Self {
        Self {
            ignore_daemonsets: true,
            delete_emptydir_data: false,
            force: false,
            timeout: DEFAULT_TIMEOUT,
            namespace: None,
        }
    }
}

/// `namespace/name` of a pod with its UID, so a pod recreated under the
/// same name does not count as the evicted one still running.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PodKey {
    pub namespace: String,
    pub name: String,
    pub uid: Option<String>,
}

impl PodKey {
    fn of(pod: &Pod) -> Self {
        Self {
            namespace: pod.namespace().unwrap_or_default(),
            name: pod.name_any(),
            uid: pod.uid(),
        }
    }

    /// `namespace/name`.
    pub fn label(&self) -> String {
        format!("{}/{}", self.namespace, self.name)
    }
}

/// Why a pod stays on the node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    /// A mirror of a static pod: the kubelet runs it from a file.
    Mirror,
    /// Managed by a DaemonSet, skipped with `ignore_daemonsets`.
    DaemonSet,
}

impl Skip {
    /// The UI translates it, like every text of the drain.
    pub fn describe(self) -> Message {
        match self {
            Skip::Mirror => msg!("drain-skip-mirror"),
            Skip::DaemonSet => msg!("drain-skip-daemonset"),
        }
    }

    /// `count` pods left for this reason: `2 DaemonSet pods`.
    fn count(self, count: usize) -> Message {
        match self {
            Skip::Mirror => msg!("drain-skipped-mirror", count = count),
            Skip::DaemonSet => msg!("drain-skipped-daemonset", count = count),
        }
    }
}

/// Why a pod stops the drain with the given options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Block {
    /// A DaemonSet pod without `ignore_daemonsets`.
    DaemonSet,
    /// No controller recreates it; needs `force`.
    NoController,
    /// Has `emptyDir` volumes; needs `delete_emptydir_data`.
    EmptyDir,
}

impl Block {
    pub fn describe(self) -> Message {
        match self {
            Block::DaemonSet => msg!("drain-block-daemonset"),
            Block::NoController => msg!("drain-block-no-controller"),
            Block::EmptyDir => msg!("drain-block-emptydir"),
        }
    }
}

/// What the drain does with one pod.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Evicted through the Eviction API.
    Evict,
    /// Finished: deleted directly.
    Delete,
    /// Already being deleted: only waited for.
    Wait,
    Skip(Skip),
    Block(Block),
}

/// What a drain does with every pod of the node, in pod order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DrainPlan {
    pub pods: Vec<(PodKey, Verdict)>,
}

impl DrainPlan {
    /// Pods the drain removes (evicts, deletes or waits for).
    pub fn removed(&self) -> impl Iterator<Item = &(PodKey, Verdict)> {
        self.pods
            .iter()
            .filter(|(_, v)| matches!(v, Verdict::Evict | Verdict::Delete | Verdict::Wait))
    }

    pub fn skipped(&self) -> impl Iterator<Item = (&PodKey, Skip)> {
        self.pods.iter().filter_map(|(k, v)| match v {
            Verdict::Skip(skip) => Some((k, *skip)),
            _ => None,
        })
    }

    pub fn blocked(&self) -> impl Iterator<Item = (&PodKey, Block)> {
        self.pods.iter().filter_map(|(k, v)| match v {
            Verdict::Block(block) => Some((k, *block)),
            _ => None,
        })
    }

    pub fn is_blocked(&self) -> bool {
        self.blocked().next().is_some()
    }

    /// One line per pod for confirmation dialogs: `ns/name — evict`.
    pub fn lines(&self) -> Vec<Message> {
        self.pods
            .iter()
            .map(|(key, verdict)| {
                let what = match verdict {
                    Verdict::Evict => msg!("drain-verdict-evict"),
                    Verdict::Delete => msg!("drain-verdict-delete"),
                    Verdict::Wait => msg!("drain-verdict-wait"),
                    Verdict::Skip(skip) => msg!("drain-verdict-skip", reason = skip.describe()),
                    Verdict::Block(block) => {
                        msg!("drain-verdict-blocked", reason = block.describe())
                    }
                };
                msg!("drain-plan-line", pod = key.label(), what = what)
            })
            .collect()
    }
}

fn is_mirror(pod: &Pod) -> bool {
    pod.annotations().contains_key(MIRROR_ANNOTATION)
}

fn controller_kind(pod: &Pod) -> Option<&str> {
    pod.owner_references()
        .iter()
        .find(|owner| owner.controller == Some(true))
        .map(|owner| owner.kind.as_str())
}

fn is_finished(pod: &Pod) -> bool {
    matches!(
        pod.status.as_ref().and_then(|s| s.phase.as_deref()),
        Some("Succeeded" | "Failed")
    )
}

fn has_empty_dir(pod: &Pod) -> bool {
    pod.spec
        .as_ref()
        .and_then(|spec| spec.volumes.as_ref())
        .is_some_and(|volumes| volumes.iter().any(|v| v.empty_dir.is_some()))
}

/// What a drain with `options` does with `pod`, the kubectl rules in the
/// order kubectl applies them.
pub fn verdict(pod: &Pod, options: &DrainOptions) -> Verdict {
    if pod.metadata.deletion_timestamp.is_some() {
        return Verdict::Wait;
    }
    if is_mirror(pod) {
        return Verdict::Skip(Skip::Mirror);
    }
    let controller = controller_kind(pod);
    if controller == Some("DaemonSet") {
        return if options.ignore_daemonsets {
            Verdict::Skip(Skip::DaemonSet)
        } else {
            Verdict::Block(Block::DaemonSet)
        };
    }
    if is_finished(pod) {
        return Verdict::Delete;
    }
    if controller.is_none() && !options.force {
        return Verdict::Block(Block::NoController);
    }
    if has_empty_dir(pod) && !options.delete_emptydir_data {
        return Verdict::Block(Block::EmptyDir);
    }
    Verdict::Evict
}

/// The drain of `pods` (the node's) with `options`, sorted by pod.
pub fn plan(pods: &[Pod], options: &DrainOptions) -> DrainPlan {
    let mut pods: Vec<(PodKey, Verdict)> = pods
        .iter()
        .filter(|pod| {
            options
                .namespace
                .as_deref()
                .is_none_or(|ns| pod.namespace().as_deref() == Some(ns))
        })
        .map(|pod| (PodKey::of(pod), verdict(pod, options)))
        .collect();
    pods.sort_by(|a, b| a.0.cmp(&b.0));
    DrainPlan { pods }
}

/// `kubectl drain <node> --ignore-daemonsets … --timeout=5m`.
pub fn drain_command(node: &str, options: &DrainOptions) -> String {
    let mut command = format!("kubectl drain {node}");
    if options.ignore_daemonsets {
        command.push_str(" --ignore-daemonsets");
    }
    if options.delete_emptydir_data {
        command.push_str(" --delete-emptydir-data");
    }
    if options.force {
        command.push_str(" --force");
    }
    command.push_str(&format!(" --timeout={}", format_timeout(options.timeout)));
    command
}

/// `5m`, `90s`, `1h`: kubectl's duration syntax.
fn format_timeout(timeout: Duration) -> String {
    let secs = timeout.as_secs();
    if secs > 0 && secs.is_multiple_of(3600) {
        format!("{}h", secs / 3600)
    } else if secs > 0 && secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

/// Every pod scheduled to `node` (field selector `spec.nodeName`), in all
/// namespaces or only `namespace`.
pub async fn node_pods(
    client: &Client,
    node: &str,
    namespace: Option<&str>,
) -> Result<Vec<Pod>, kube::Error> {
    let api: Api<Pod> = match namespace {
        Some(namespace) => Api::namespaced(client.clone(), namespace),
        None => Api::all(client.clone()),
    };
    let params = ListParams::default().fields(&format!("spec.nodeName={node}"));
    Ok(api.list(&params).await?.items)
}

/// Marks `node` unschedulable (`kubectl cordon`).
pub async fn cordon(client: &Client, node: &str) -> Result<(), kube::Error> {
    let api: Api<Node> = Api::all(client.clone());
    let patch = crate::plugin::builtin::cordon_patch(true);
    api.patch(node, &PatchParams::default(), &Patch::Strategic(patch))
        .await?;
    Ok(())
}

/// Progress of a running drain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DrainEvent {
    /// Cordoned and planned: `total` pods are to go.
    Planned { total: usize },
    /// One of them is gone, or failed.
    PodDone,
}

/// How a drain ended.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DrainReport {
    /// Pods removed and those that failed (blocked ones included), by pod.
    pub summary: BulkSummary,
    /// Pods left on the node on purpose.
    pub skipped: Vec<(String, Skip)>,
}

fn failure(object: String, reason: Message) -> BulkFailure {
    BulkFailure {
        object,
        reason,
        forbidden: false,
    }
}

/// A failure of the whole drain (cordon or listing), before any pod:
/// `what` makes the reason from the API server's error.
fn whole_failure(node: &str, what: fn(Message) -> Message, error: &kube::Error) -> DrainReport {
    let forbidden = matches!(error, kube::Error::Api(status) if status.is_forbidden());
    let reason = what(match error {
        kube::Error::Api(status) if status.is_forbidden() => {
            msg!("action-error-forbidden", error = &status.message)
        }
        kube::Error::Api(status) => error_text(&status.message),
        other => error_text(other.to_string()),
    });
    DrainReport {
        summary: BulkSummary {
            succeeded: 0,
            failures: vec![BulkFailure {
                object: node.to_owned(),
                reason,
                forbidden,
            }],
        },
        skipped: Vec::new(),
    }
}

/// Drains `node`: cordon, plan, evict (or delete) and wait. `cancel` stops
/// retries and waits at the next check; pods not gone by then fail as
/// cancelled. Run it on the I/O runtime.
pub async fn drain(
    client: Client,
    node: String,
    options: DrainOptions,
    cancel: Arc<AtomicBool>,
    mut on_event: impl FnMut(DrainEvent),
) -> DrainReport {
    let deadline = Instant::now() + options.timeout;
    if let Err(error) = cordon(&client, &node).await {
        return whole_failure(
            &node,
            |error| msg!("drain-cordon-failed", error = error),
            &error,
        );
    }
    let pods = match node_pods(&client, &node, options.namespace.as_deref()).await {
        Ok(pods) => pods,
        Err(error) => {
            return whole_failure(
                &node,
                |error| msg!("drain-list-pods-failed", error = error),
                &error,
            );
        }
    };
    let plan = plan(&pods, &options);
    let skipped = plan
        .skipped()
        .map(|(key, skip)| (key.label(), skip))
        .collect();
    if plan.is_blocked() {
        let failures = plan
            .blocked()
            .map(|(key, block)| failure(key.label(), block.describe()))
            .collect();
        on_event(DrainEvent::Planned { total: 0 });
        return DrainReport {
            summary: BulkSummary {
                succeeded: 0,
                failures,
            },
            skipped,
        };
    }
    let work: Vec<(PodKey, Verdict)> = plan.removed().cloned().collect();
    on_event(DrainEvent::Planned { total: work.len() });
    let results = run_bounded(
        work,
        CONCURRENCY,
        |(key, verdict)| {
            let client = client.clone();
            let cancel = cancel.clone();
            async move { remove_pod(&client, &key, verdict, deadline, &cancel).await }
        },
        |_, _| on_event(DrainEvent::PodDone),
    )
    .await;
    let mut summary = BulkSummary::default();
    for ((key, _), result) in results {
        match result {
            Ok(()) => summary.succeeded += 1,
            Err(reason) => summary.failures.push(failure(key.label(), reason)),
        }
    }
    summary.failures.sort_by(|a, b| a.object.cmp(&b.object));
    DrainReport { summary, skipped }
}

/// Evicts (retrying PDB refusals with backoff) or deletes one pod, then
/// waits until it is gone. The error is the readable reason.
async fn remove_pod(
    client: &Client,
    key: &PodKey,
    verdict: Verdict,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<(), Message> {
    let api: Api<Pod> = Api::namespaced(client.clone(), &key.namespace);
    match verdict {
        Verdict::Delete => {
            let params = deletion::with_uid(DeleteParams::default(), key.uid.as_deref());
            match deletion::finish(api.delete(&key.name, &params).await) {
                Ok(()) => {}
                // Another pod of that name: the one planned is gone.
                Err(DeleteError::Recreated) => return Ok(()),
                Err(DeleteError::Kube(error)) => return Err(kube_reason(&error)),
            }
        }
        Verdict::Evict => {
            let mut backoff = BACKOFF_START;
            loop {
                let pod = match api.get_opt(&key.name).await {
                    Ok(Some(pod)) if pod.uid() == key.uid => pod,
                    Ok(_) => return Ok(()),
                    Err(error) => return Err(kube_reason(&error)),
                };
                // A budget that allows no disruption now refuses with HTTP
                // 429 (not retried by the client, #345). Asking the budget
                // first saves the refused POSTs and names the budget; the
                // eviction is bounded by the timeout and Cancel all the same.
                let refusal: Result<(), EvictError> = match blocking_budget(client, &pod).await {
                    Some(refusal) => Err(refusal),
                    None => {
                        match until_deadline(
                            evict_pod(client, &key.namespace, &key.name),
                            deadline,
                            cancel,
                        )
                        .await
                        {
                            Some(Ok(Evicted::Evicted)) => break,
                            Some(Ok(Evicted::AlreadyGone)) => return Ok(()),
                            Some(Err(error @ EvictError::DisruptionBudget { .. })) => Err(error),
                            Some(Err(EvictError::Kube(error))) => {
                                return Err(kube_reason(&error));
                            }
                            None if cancel.load(Ordering::Relaxed) => {
                                return Err(msg!("drain-cancelled-evicting"));
                            }
                            None => return Err(msg!("drain-eviction-timeout")),
                        }
                    }
                };
                if let Err(error) = refusal {
                    let now = Instant::now();
                    if now >= deadline {
                        return Err(msg!("drain-gave-up", error = error.to_string()));
                    }
                    if !pause(backoff.min(deadline - now), cancel).await {
                        return Err(msg!("drain-cancelled", error = error.to_string()));
                    }
                    backoff = (backoff * 2).min(BACKOFF_MAX);
                }
            }
        }
        Verdict::Wait => {}
        Verdict::Skip(_) | Verdict::Block(_) => return Ok(()),
    }
    // Gone means: no pod by that name, or one with another UID.
    loop {
        match api.get_opt(&key.name).await {
            Ok(None) => return Ok(()),
            Ok(Some(pod)) if pod.uid() != key.uid => return Ok(()),
            Ok(Some(_)) => {}
            Err(error) => return Err(kube_reason(&error)),
        }
        if Instant::now() + GONE_POLL >= deadline {
            return Err(msg!("drain-terminating-timeout"));
        }
        if !pause(GONE_POLL, cancel).await {
            return Err(msg!("drain-cancelled-waiting"));
        }
    }
}

/// Runs `future` until `deadline` or Cancel; `None` then.
async fn until_deadline<T>(
    future: impl std::future::Future<Output = T>,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Option<T> {
    let watch = async {
        while Instant::now() < deadline && !cancel.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    };
    tokio::select! {
        value = future => Some(value),
        () = watch => None,
    }
}

/// Whether `selector` selects `labels` (matchLabels and matchExpressions,
/// as the API server reads them). An empty selector selects everything,
/// as for PodDisruptionBudgets in `policy/v1`.
pub fn selector_matches(selector: &LabelSelector, labels: &BTreeMap<String, String>) -> bool {
    let by_labels = selector
        .match_labels
        .iter()
        .flatten()
        .all(|(k, v)| labels.get(k) == Some(v));
    let by_expressions = selector.match_expressions.iter().flatten().all(|e| {
        let values = e.values.as_deref().unwrap_or_default();
        let value = labels.get(&e.key);
        match e.operator.as_str() {
            "In" => value.is_some_and(|v| values.contains(v)),
            "NotIn" => value.is_none_or(|v| !values.contains(v)),
            "Exists" => value.is_some(),
            "DoesNotExist" => value.is_none(),
            _ => false,
        }
    });
    by_labels && by_expressions
}

/// The refusal a budget covering `pod` gives now (it allows no
/// disruption), from its status: `needs 2 healthy pods and has 2`.
pub fn budget_refusal(budget: &PodDisruptionBudget, pod: &Pod) -> Option<EvictError> {
    let spec = budget.spec.as_ref()?;
    let selector = spec.selector.as_ref()?;
    if !selector_matches(selector, pod.labels()) {
        return None;
    }
    let status = budget.status.as_ref()?;
    if status.disruptions_allowed.unwrap_or(0) > 0 {
        return None;
    }
    Some(EvictError::DisruptionBudget {
        budget: Some(budget.name_any()),
        detail: Some(format!(
            "needs {} healthy pods and has {} currently",
            status.desired_healthy.unwrap_or(0),
            status.current_healthy.unwrap_or(0)
        )),
    })
}

/// A budget in the pod's namespace that would refuse its eviction now.
/// Reading budgets may be forbidden: then the eviction decides.
async fn blocking_budget(client: &Client, pod: &Pod) -> Option<EvictError> {
    let namespace = pod.namespace()?;
    let api: Api<PodDisruptionBudget> = Api::namespaced(client.clone(), &namespace);
    let budgets = api.list(&ListParams::default()).await.ok()?;
    budgets
        .items
        .iter()
        .find_map(|budget| budget_refusal(budget, pod))
}

/// Sleeps `duration` in short steps; `false` once cancelled.
async fn pause(duration: Duration, cancel: &AtomicBool) -> bool {
    let until = Instant::now() + duration;
    while Instant::now() < until {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let step = (until - Instant::now()).min(Duration::from_millis(200));
        tokio::time::sleep(step).await;
    }
    !cancel.load(Ordering::Relaxed)
}

fn kube_reason(error: &kube::Error) -> Message {
    match error {
        kube::Error::Api(status) if status.is_forbidden() => {
            msg!("action-error-forbidden", error = &status.message)
        }
        kube::Error::Api(status) if !status.message.is_empty() => error_text(&status.message),
        other => error_text(other.to_string()),
    }
}

/// Skipped pods grouped by reason for a summary line:
/// `Skipped 3 DaemonSet pods, 1 static (mirror) pod`.
pub fn skipped_note(skipped: &[(String, Skip)]) -> Option<Message> {
    if skipped.is_empty() {
        return None;
    }
    let parts: Vec<Message> = [Skip::DaemonSet, Skip::Mirror]
        .into_iter()
        .filter_map(|reason| {
            let n = skipped.iter().filter(|(_, skip)| *skip == reason).count();
            (n > 0).then(|| reason.count(n))
        })
        .collect();
    Some(msg!("drain-left-on-node", pods = parts))
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn pod(name: &str, extra: Value) -> Pod {
        let mut value = json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": name, "namespace": "ns", "uid": format!("uid-{name}")},
            "spec": {"nodeName": "n1", "containers": [{"name": "c"}]},
            "status": {"phase": "Running"}
        });
        merge(&mut value, extra);
        serde_json::from_value(value).unwrap()
    }

    fn merge(into: &mut Value, from: Value) {
        match (into, from) {
            (Value::Object(into), Value::Object(from)) => {
                for (k, v) in from {
                    merge(into.entry(k).or_insert(Value::Null), v);
                }
            }
            (into, from) => *into = from,
        }
    }

    fn owned_by(kind: &str) -> Value {
        json!({"metadata": {"ownerReferences": [{
            "apiVersion": "apps/v1", "kind": kind, "name": "owner", "uid": "o",
            "controller": true
        }]}})
    }

    fn empty_dir() -> Value {
        json!({"spec": {"volumes": [{"name": "scratch", "emptyDir": {}}]}})
    }

    #[test]
    fn verdicts_follow_kubectl() {
        let opts = DrainOptions::default();
        let rs = pod("web", owned_by("ReplicaSet"));
        assert_eq!(verdict(&rs, &opts), Verdict::Evict);

        let ds = pod("agent", owned_by("DaemonSet"));
        assert_eq!(verdict(&ds, &opts), Verdict::Skip(Skip::DaemonSet));
        let strict = DrainOptions {
            ignore_daemonsets: false,
            ..DrainOptions::default()
        };
        assert_eq!(verdict(&ds, &strict), Verdict::Block(Block::DaemonSet));

        let mirror = pod(
            "etcd-n1",
            json!({"metadata": {"annotations": {MIRROR_ANNOTATION: "abc"}}}),
        );
        assert_eq!(verdict(&mirror, &opts), Verdict::Skip(Skip::Mirror));
        // Mirror wins over everything else, as it has no controller.
        assert_eq!(
            verdict(
                &mirror,
                &DrainOptions {
                    force: true,
                    ..opts.clone()
                }
            ),
            Verdict::Skip(Skip::Mirror)
        );

        let bare = pod("bare", json!({}));
        assert_eq!(verdict(&bare, &opts), Verdict::Block(Block::NoController));
        let force = DrainOptions {
            force: true,
            ..DrainOptions::default()
        };
        assert_eq!(verdict(&bare, &force), Verdict::Evict);
        // A non-controller owner does not count.
        let weak = pod(
            "weak",
            json!({"metadata": {"ownerReferences": [{
                "apiVersion": "v1", "kind": "ConfigMap", "name": "x", "uid": "x"
            }]}}),
        );
        assert_eq!(verdict(&weak, &opts), Verdict::Block(Block::NoController));

        let mut scratch = owned_by("ReplicaSet");
        merge(&mut scratch, empty_dir());
        let scratch = pod("cache", scratch);
        assert_eq!(verdict(&scratch, &opts), Verdict::Block(Block::EmptyDir));
        let wipe = DrainOptions {
            delete_emptydir_data: true,
            ..DrainOptions::default()
        };
        assert_eq!(verdict(&scratch, &wipe), Verdict::Evict);
        // Without a controller and with emptyDir both flags are needed.
        let bare_scratch = pod("bs", empty_dir());
        assert_eq!(
            verdict(&bare_scratch, &force),
            Verdict::Block(Block::EmptyDir)
        );
        assert_eq!(
            verdict(
                &bare_scratch,
                &DrainOptions {
                    force: true,
                    ..wipe.clone()
                }
            ),
            Verdict::Evict
        );
    }

    #[test]
    fn finished_and_terminating_pods() {
        let opts = DrainOptions::default();
        // Finished pods go without force or the emptyDir flag.
        let mut done = empty_dir();
        merge(&mut done, json!({"status": {"phase": "Succeeded"}}));
        assert_eq!(verdict(&pod("job-1", done), &opts), Verdict::Delete);
        let failed = pod("job-2", json!({"status": {"phase": "Failed"}}));
        assert_eq!(verdict(&failed, &opts), Verdict::Delete);
        // A finished DaemonSet pod is still skipped.
        let mut ds_done = owned_by("DaemonSet");
        merge(&mut ds_done, json!({"status": {"phase": "Failed"}}));
        assert_eq!(
            verdict(&pod("ds", ds_done), &opts),
            Verdict::Skip(Skip::DaemonSet)
        );
        let going = pod(
            "going",
            json!({"metadata": {"deletionTimestamp": "2026-01-01T00:00:00Z"}}),
        );
        assert_eq!(verdict(&going, &opts), Verdict::Wait);
    }

    #[test]
    fn plan_sorts_filters_and_reports() {
        let pods = vec![
            pod("web", owned_by("ReplicaSet")),
            pod("agent", owned_by("DaemonSet")),
            pod("bare", json!({})),
            pod("other", json!({"metadata": {"namespace": "kube-system"}})),
        ];
        let plan = plan(&pods, &DrainOptions::default());
        assert!(plan.is_blocked());
        assert_eq!(plan.removed().count(), 1);
        assert_eq!(plan.skipped().count(), 1);
        assert_eq!(plan.blocked().count(), 2);
        assert_eq!(plan.pods[0].0.label(), "kube-system/other");
        assert_eq!(plan.lines()[1].english(), "ns/agent — skip: DaemonSet pod");

        let only = DrainOptions {
            namespace: Some("ns".into()),
            force: true,
            ..DrainOptions::default()
        };
        let plan = super::plan(&pods, &only);
        assert!(!plan.is_blocked());
        assert_eq!(plan.pods.len(), 3);
        assert_eq!(plan.removed().count(), 2);
    }

    #[test]
    fn commands_and_notes() {
        let mut opts = DrainOptions::default();
        assert_eq!(
            drain_command("n1", &opts),
            "kubectl drain n1 --ignore-daemonsets --timeout=5m"
        );
        opts.delete_emptydir_data = true;
        opts.force = true;
        opts.timeout = Duration::from_secs(90);
        assert_eq!(
            drain_command("n1", &opts),
            "kubectl drain n1 --ignore-daemonsets --delete-emptydir-data --force --timeout=90s"
        );
        opts.ignore_daemonsets = false;
        opts.timeout = Duration::from_secs(3600);
        assert_eq!(
            drain_command("n1", &opts),
            "kubectl drain n1 --delete-emptydir-data --force --timeout=1h"
        );

        assert_eq!(skipped_note(&[]), None);
        let skipped = vec![
            ("a".to_owned(), Skip::DaemonSet),
            ("b".to_owned(), Skip::DaemonSet),
            ("c".to_owned(), Skip::Mirror),
        ];
        assert_eq!(
            skipped_note(&skipped).unwrap().english(),
            "Left on the node: 2 DaemonSet pods, 1 static (mirror) pod."
        );
    }

    #[test]
    fn budgets_refuse_matching_pods_without_allowed_disruptions() {
        let budget = |allowed: i32, selector: Value| -> PodDisruptionBudget {
            serde_json::from_value(json!({
                "metadata": {"name": "web", "namespace": "ns"},
                "spec": {"minAvailable": 2, "selector": selector},
                "status": {"disruptionsAllowed": allowed, "currentHealthy": 2,
                           "desiredHealthy": 2, "expectedPods": 2, "observedGeneration": 1}
            }))
            .unwrap()
        };
        let web = pod(
            "web-1",
            json!({"metadata": {"labels": {"app": "web", "tier": "fe"}}}),
        );
        let refusal =
            budget_refusal(&budget(0, json!({"matchLabels": {"app": "web"}})), &web).unwrap();
        assert!(refusal.is_disruption_budget());
        assert_eq!(
            refusal.to_string(),
            "PodDisruptionBudget web does not allow evicting the pod now \
             (needs 2 healthy pods and has 2 currently)"
        );
        assert!(budget_refusal(&budget(1, json!({"matchLabels": {"app": "web"}})), &web).is_none());
        assert!(budget_refusal(&budget(0, json!({"matchLabels": {"app": "db"}})), &web).is_none());
        // An empty selector covers every pod of the namespace.
        assert!(budget_refusal(&budget(0, json!({})), &web).is_some());

        let exprs = |op: &str, values: Value| -> LabelSelector {
            serde_json::from_value(json!({"matchExpressions": [
                {"key": "tier", "operator": op, "values": values}
            ]}))
            .unwrap()
        };
        let labels = web.labels();
        assert!(selector_matches(&exprs("In", json!(["fe", "be"])), labels));
        assert!(!selector_matches(&exprs("NotIn", json!(["fe"])), labels));
        assert!(selector_matches(&exprs("Exists", json!(null)), labels));
        assert!(!selector_matches(
            &exprs("DoesNotExist", json!(null)),
            labels
        ));
    }

    #[tokio::test]
    async fn deadline_and_cancel_stop_waiting() {
        let go = AtomicBool::new(false);
        let soon = Instant::now() + Duration::from_millis(50);
        assert_eq!(until_deadline(async { 7 }, soon, &go).await, Some(7));
        let never = std::future::pending::<()>();
        assert_eq!(until_deadline(never, soon, &go).await, None);
        let stop = AtomicBool::new(true);
        let later = Instant::now() + Duration::from_secs(60);
        assert_eq!(
            until_deadline(std::future::pending::<()>(), later, &stop).await,
            None
        );
    }

    #[tokio::test]
    async fn pause_stops_on_cancel() {
        let cancel = AtomicBool::new(true);
        assert!(!pause(Duration::from_secs(5), &cancel).await);
        let go = AtomicBool::new(false);
        assert!(pause(Duration::from_millis(10), &go).await);
    }
}
