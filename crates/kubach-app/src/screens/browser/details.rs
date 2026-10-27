//! The details panel of the selected object: header and actions, the
//! metrics block, one main block (name, namespace, created, UID, the kind's
//! own fields, owners, labels, annotations), a workload's pods (#393), the
//! kind's sections, related objects, conditions and events. A pod clicked
//! in the pods block is shown in place, with a way back ([`stack`]).
//!
//! A view of its own (#363): the browser embeds it cached, so a tick of the
//! table's ages does not lay the details out again, and a tick of the
//! details' ages (or their events, metrics, related objects) does not
//! redraw the table. The browser hands it the selected object
//! ([`DetailsTarget`]); its buttons ask the browser to act through
//! [`DetailsEvent`], since actions, confirmations and their toast belong to
//! the browser.
//!
//! Plugin output and related lists are computed when the object or the
//! watched objects change, not on every frame, so redraws (metrics, hover)
//! stay cheap.

use std::collections::HashSet;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use k8s_openapi::api::core::v1::{Node, Pod};
use kubach_core::cache::{Subscription as Watch, WatchFilter, WatchStatus};
use kubach_core::metrics::prometheus::Target;
use kubach_core::metrics::{
    Resources, node_allocatable, pod_is_active, pod_resources, sum_resources,
};
use kubach_core::plugin::builtin::action as action_id;
use kubach_core::plugin::builtin::terminating_used_by;
use kubach_core::plugin::{
    Action, ActionKind, Brief, ConditionInfo, ContainerInfo, ContainerPhase, ContainerRole,
    DetailSection, Field, FieldValue, Health, HealthStatus, ObjectRef, Placement, PluginRegistry,
    RelatedFilter, RelatedObjects, ResourcePlugin, SectionBody,
};
use kubach_core::secrets::mask::is_core_secret;
use kubach_core::streams::{ForwardInfo, ForwardPort, ForwardSource};
use kubach_core::{ResourceKind, scale};
use kubach_i18n::{Message, t};
use kubach_ui::{
    AgeTicker, ChipList, CompactTable, KeyValueList, Panel, PortChip, ReplicaBar, StatusBadge,
    StatusCubes, StatusDot, Tone, UsageMeter, format_since, unix_now,
};
use kube::api::DynamicObject;

use super::super::access::Reveal;
use super::super::activity::watch_changes;
use super::super::events::ObjectEvents;
use super::super::managed_by::ManagedBy;
use super::super::metrics::{MetricsSource, MetricsSubject, Reference, ResourceMetrics};
use super::super::plugin_text;
use super::super::prometheus_link::{PrometheusRegistry, PrometheusStatus};
use super::super::rows;
use super::super::secret::{SecretId, SecretValues};
use super::super::topology::Connections;
use super::namespace::Contents;
use super::workload_pods::{PodsChanged, WorkloadPods};

mod stack;
use crate::cluster::{CatalogState, Cluster};
use crate::dock::Forwards;
pub(in crate::screens) use stack::OpenObject;
pub(super) use stack::ShowObject;

/// Longest text value shown (annotations can hold whole manifests).
const VALUE_MAX: usize = 300;
pub(super) const DETAILS_WIDTH: Pixels = px(380.);

/// Objects of another kind listed in a section (a Deployment's Pods).
struct RelatedList {
    spec: RelatedObjects,
    /// `None` when the cluster does not serve the kind.
    watch: Option<Watch>,
    plugin: Arc<dyn ResourcePlugin>,
    /// Generation of the watch the rows were built from.
    generation: Option<u64>,
    objects: Vec<Arc<DynamicObject>>,
    rows: Vec<RelatedRow>,
    missing: Vec<String>,
    status: WatchStatus,
    _listener: Option<Task<()>>,
}

struct RelatedRow {
    name: String,
    namespace: Option<String>,
    brief: Brief,
}

/// What the panel computed for the selected object.
#[derive(Default)]
struct DetailsState {
    /// Identity and version of the object `sections` were made from.
    version: String,
    /// Identity alone: folds survive updates of the same object.
    id: String,
    sections: Vec<DetailSection>,
    health: Option<Health>,
    related: Vec<RelatedList>,
    /// Folds flipped from their default.
    toggled: HashSet<String>,
    /// What the metrics subject was last built from.
    metrics_key: Option<(String, Option<u64>, String)>,
    /// Keeps "Created 5m ago" current.
    ages: AgeTicker,
}

impl DetailsState {
    /// Active pods that mount `claim` (a PersistentVolumeClaim), from the
    /// related list of the details; `None` when the details do not list
    /// them (another object is selected) or the list is not loaded yet.
    fn claim_users(&self, claim: &DynamicObject) -> Option<Vec<String>> {
        let name = claim.metadata.name.as_deref()?;
        let list = self.related.iter().find(|list| {
            list.spec.filter == RelatedFilter::ClaimUsers(name.to_owned())
                && list.spec.namespace == claim.metadata.namespace
        })?;
        if list.generation.is_none() || list.status != WatchStatus::Ready {
            return None;
        }
        Some(
            list.objects
                .iter()
                .filter(|pod| pod_is_active(pod))
                .filter_map(|pod| pod.metadata.name.clone())
                .collect(),
        )
    }
}

/// The object the details show, as the browser lists it.
#[derive(Clone)]
pub(in crate::screens) struct DetailsTarget {
    pub kind: ResourceKind,
    pub plugin: Arc<dyn ResourcePlugin>,
    pub object: Arc<DynamicObject>,
    /// Listed from the cache without a connection or API catalog.
    pub provisional: bool,
    /// The list still comes from the disk cache, the watch has not loaded.
    pub from_disk: bool,
    /// A watch that lists the object is reconnecting.
    pub reconnecting: bool,
    /// An action of the browser runs: the action buttons are off.
    pub running: bool,
}

impl DetailsTarget {
    /// Whether `other` shows the same: the same version of the same object,
    /// in the same state. A watch resync hands over equal objects anew.
    fn same_as(&self, other: &Self) -> bool {
        let (a, b) = (&self.object.metadata, &other.object.metadata);
        self.kind == other.kind
            && std::ptr::addr_eq(Arc::as_ptr(&self.plugin), Arc::as_ptr(&other.plugin))
            && a.uid == b.uid
            && a.name == b.name
            && a.namespace == b.namespace
            && a.resource_version == b.resource_version
            && self.provisional == other.provisional
            && self.from_disk == other.from_disk
            && self.reconnecting == other.reconnecting
            && self.running == other.running
    }
}

/// What the buttons of the details ask the screen showing them to do.
pub(in crate::screens) enum DetailsEvent {
    /// The close button: the browser drops the selection.
    Close,
    /// "Back" on an object opened by [`ResourceDetails::open`]: the screen
    /// shows its own view again (the topology's node summary, #394).
    Back,
    /// A plugin action on the shown object, confirmed by the browser if needed.
    /// `kind` and `plugin` are the shown object's: not the listed kind's
    /// when the details show another object in place (#393).
    Action {
        action: Action,
        object: Arc<DynamicObject>,
        kind: ResourceKind,
        plugin: Arc<dyn ResourcePlugin>,
    },
    /// A step of the replica stepper; `confirm` for a step to zero.
    Scale {
        object: Arc<DynamicObject>,
        kind: ResourceKind,
        plugin: Arc<dyn ResourcePlugin>,
        delta: i32,
        confirm: bool,
    },
    /// Drain… of a Node.
    Drain(String),
    /// Shell on a Node.
    NodeShell(String),
}

/// The details column of the resource browser (and of the topology).
pub(in crate::screens) struct ResourceDetails {
    pub(super) cluster: Entity<Cluster>,
    registry: PluginRegistry,
    /// What the details show: the browser's selection, or the object shown
    /// in place over it (the top of `stack`).
    pub(super) target: Option<DetailsTarget>,
    /// The browser's selected object.
    base: Option<DetailsTarget>,
    /// Objects shown in place over the selection (#393), last on top.
    stack: Vec<stack::Frame>,
    /// Label of "back" to the view of the screen that opened the details
    /// over it ([`ResourceDetails::open`]); `None` in the browser.
    home: Option<String>,
    /// An object opened by reference, until its watch lists it.
    pending: Option<stack::Pending>,
    /// An action of the screen runs (when there is no selection to say so).
    screen_running: bool,
    /// Esc goes back while the details have focus (after a push).
    focus: FocusHandle,
    focus_pending: bool,
    state: DetailsState,
    /// Values of the shown Secret, shown on demand.
    secret: Option<Entity<SecretValues>>,
    /// Events of the shown object.
    object_events: Option<Entity<ObjectEvents>>,
    /// Neighbors of the shown object on the topology (#324).
    connections: Option<(Entity<Connections>, Subscription)>,
    /// Pods of the shown workload (#393).
    pods: Option<(Entity<WorkloadPods>, [Subscription; 2])>,
    /// GitOps object managing the shown object (#88).
    managed_by: Option<(Entity<ManagedBy>, [Subscription; 2])>,
    /// Metrics block of the shown object.
    metrics: Option<Entity<ResourceMetrics>>,
    /// What is inside the shown namespace (Namespaces list only).
    pub(super) contents: Option<Contents>,
    /// Active port-forwards, for the ports of a pod's containers (#372).
    forwards: Option<(Entity<Forwards>, Subscription)>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DetailsEvent> for ResourceDetails {}
/// Links of the details (owners, related objects, "Managed by").
impl EventEmitter<Reveal> for ResourceDetails {}

fn clip(text: &str) -> String {
    if text.chars().count() <= VALUE_MAX {
        return text.to_owned();
    }
    let mut clipped: String = text.chars().take(VALUE_MAX).collect();
    clipped.push('…');
    clipped
}

fn group_of(api_version: &str) -> &str {
    api_version.rsplit_once('/').map_or("", |(group, _)| group)
}

fn reveal_of(target: &ObjectRef) -> Reveal {
    Reveal {
        group: group_of(&target.api_version).to_owned(),
        kind: target.kind.clone(),
        namespace: target.namespace.clone(),
        name: target.name.clone(),
    }
}

fn ago(time: i64) -> String {
    t!("details-ago", age = format_since(time, unix_now()))
}

/// Tone of a condition, from the polarity its plugin set: a missing good
/// state is a warning, a present problem is a danger.
fn condition_tone(c: &ConditionInfo) -> Tone {
    match (c.is_good(), c.negative) {
        (Some(true), _) => Tone::Success,
        (Some(false), true) => Tone::Danger,
        (Some(false), false) => Tone::Warning,
        (None, _) => Tone::Neutral,
    }
}

fn cores(nanocores: Option<u64>) -> Option<f64> {
    nanocores.map(|n| n as f64 / 1e9)
}

fn bytes(value: Option<u64>) -> Option<f64> {
    value.map(|n| n as f64)
}

/// CPU and memory references of a set of pods, summed in core.
fn references(all: &[(Resources, Resources)]) -> (Reference, Reference) {
    let (requests, limits) = sum_resources(all);
    (
        Reference {
            request: cores(requests.cpu),
            limit: cores(limits.cpu),
        },
        Reference {
            request: bytes(requests.memory),
            limit: bytes(limits.memory),
        },
    )
}

fn decode<K: serde::de::DeserializeOwned>(object: &DynamicObject) -> Option<K> {
    serde_json::to_value(object)
        .ok()
        .and_then(|value| serde_json::from_value(value).ok())
}

impl ResourceDetails {
    pub(in crate::screens) fn new(cluster: Entity<Cluster>, cx: &mut Context<Self>) -> Self {
        // A new connection, catalog or Prometheus changes what the blocks
        // can show; nothing to redraw while no object is shown.
        let mut subscriptions = vec![cx.observe(&cluster, |this, _, cx| {
            if this.target.is_some() {
                cx.notify();
            }
        })];
        if let Some(registry) = PrometheusRegistry::try_global(cx) {
            subscriptions.push(cx.observe(&registry, |this, _, cx| {
                if this.target.is_some() {
                    cx.notify();
                }
            }));
        }
        Self {
            cluster,
            registry: PluginRegistry::with_builtins(),
            target: None,
            base: None,
            stack: Vec::new(),
            home: None,
            pending: None,
            screen_running: false,
            focus: cx.focus_handle(),
            focus_pending: false,
            state: DetailsState::default(),
            secret: None,
            object_events: None,
            connections: None,
            pods: None,
            managed_by: None,
            metrics: None,
            contents: None,
            forwards: None,
            _subscriptions: subscriptions,
        }
    }

    /// Shows `target` (the browser's selected object), or nothing. Redraws
    /// only when something shown changes. Without an object every block
    /// and watch is dropped at once: hidden details do not render.
    ///
    /// Objects shown in place stay over a newer version of the same
    /// selection; another selection drops them.
    pub(in crate::screens) fn set_target(
        &mut self,
        target: Option<DetailsTarget>,
        cx: &mut Context<Self>,
    ) {
        let same_object = match (&self.base, &target) {
            (Some(old), Some(new)) => stack::same_object(old, new),
            _ => false,
        };
        if same_object {
            let running = target.as_ref().is_some_and(|t| t.running);
            for frame in &mut self.stack {
                frame.target.running = running;
            }
        } else {
            self.stack.clear();
            self.home = None;
            self.pending = None;
        }
        self.base = target;
        self.show_top(cx);
        if self.target.is_none() {
            self.clear(cx);
        }
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        self.stack.clear();
        self.home = None;
        self.pending = None;
        self.drop_secret(cx);
        self.object_events = None;
        self.connections = None;
        self.pods = None;
        self.managed_by = None;
        self.metrics = None;
        self.contents = None;
        self.state = DetailsState::default();
    }

    /// Active pods that mount `claim`, when the details list them.
    pub(super) fn claim_users(&self, claim: &DynamicObject) -> Option<Vec<String>> {
        self.state.claim_users(claim)
    }

    /// Hides the shown Secret values (clearing the clipboard if needed)
    /// and forgets them.
    fn drop_secret(&mut self, cx: &mut Context<Self>) {
        if let Some(secret) = self.secret.take() {
            secret.update(cx, |secret, cx| secret.conceal(cx));
        }
    }

    /// Keeps [`SecretValues`] on the shown Secret: another object, kind,
    /// namespace or context hides what the previous one showed.
    fn sync_secret(&mut self, cx: &mut Context<Self>) {
        let wanted = self.target.as_ref().and_then(|target| {
            let api = &target.kind.api;
            let object = is_core_secret(&api.group, &api.kind).then(|| target.object.clone())?;
            let connection = self.cluster.read(cx).connection()?;
            let id = SecretId::of(&connection.context, &object)?;
            Some((id, object, connection.client.clone()))
        });
        let Some((id, object, client)) = wanted else {
            self.drop_secret(cx);
            return;
        };
        if let Some(secret) = &self.secret
            && secret.read(cx).id() == &id
        {
            secret.update(cx, |secret, _| secret.set_object(object));
            return;
        }
        self.drop_secret(cx);
        let cluster = self.cluster.clone();
        self.secret = Some(cx.new(|cx| SecretValues::new(id, client, cluster, object, cx)));
    }

    /// Keeps [`ManagedBy`] on the shown object; its links are re-emitted
    /// for the workspace to open.
    fn sync_managed_by(&mut self, cx: &mut Context<Self>) {
        let wanted = self.target.as_ref().map(|target| {
            let object = target.object.clone();
            (ObjectEvents::id_of(&target.kind, &object), object)
        });
        let Some((id, object)) = wanted else {
            self.managed_by = None;
            return;
        };
        if self
            .managed_by
            .as_ref()
            .is_some_and(|(panel, _)| panel.read(cx).id() == id)
        {
            return;
        }
        let cluster = self.cluster.read(cx);
        let (Some(connection), CatalogState::Ready(catalog)) =
            (cluster.connection(), cluster.catalog())
        else {
            self.managed_by = None;
            return;
        };
        let (client, catalog) = (connection.client.clone(), catalog.clone());
        let panel = cx.new(|cx| ManagedBy::new(id, client, catalog, &object, cx));
        let subscriptions = [
            cx.subscribe(&panel, |_, _, reveal: &Reveal, cx| cx.emit(reveal.clone())),
            // The panel shows up once its lookup finds something.
            cx.observe(&panel, |_, _, cx| cx.notify()),
        ];
        self.managed_by = Some((panel, subscriptions));
    }

    /// Keeps [`ObjectEvents`] on the shown object.
    fn sync_object_events(&mut self, cx: &mut Context<Self>) {
        let Some((kind, object)) = self
            .target
            .as_ref()
            .map(|target| (target.kind.clone(), target.object.clone()))
        else {
            self.object_events = None;
            return;
        };
        let id = ObjectEvents::id_of(&kind, &object);
        if self
            .object_events
            .as_ref()
            .is_some_and(|events| events.read(cx).id() == id)
        {
            return;
        }
        let cluster = self.cluster.read(cx);
        let (Some(cache), CatalogState::Ready(catalog)) = (cluster.cache(), cluster.catalog())
        else {
            self.object_events = None;
            return;
        };
        let (cache, catalog) = (cache.clone(), catalog.clone());
        self.object_events =
            Some(cx.new(|cx| ObjectEvents::new(&cache, &catalog, &kind, &object, cx)));
    }

    /// Recomputes what the details show when the object (or what it lists)
    /// changed; keeps the metrics block in line.
    fn sync_details(&mut self, cx: &mut Context<Self>) {
        let Some((kind, plugin, object)) = self.target.as_ref().map(|target| {
            (
                target.kind.clone(),
                target.plugin.clone(),
                target.object.clone(),
            )
        }) else {
            self.state = DetailsState::default();
            self.metrics = None;
            return;
        };
        let id = ObjectEvents::id_of(&kind, &object);
        let version = format!(
            "{id}@{}",
            object
                .metadata
                .resource_version
                .as_deref()
                .unwrap_or_default()
        );
        if self.state.id != id {
            self.state.toggled.clear();
            self.state.id = id;
        }
        if self.state.version != version {
            self.state.version = version;
            self.state.sections = plugin.details(&object);
            self.state.health = Some(plugin.health(&object));
            self.sync_related(cx);
        }
        self.refresh_related();
        if kind.api.group.is_empty()
            && kind.api.kind == "PersistentVolumeClaim"
            && object.metadata.deletion_timestamp.is_some()
        {
            // Kept by `kubernetes.io/pvc-protection`: name the pods.
            let mut health = plugin.health(&object);
            if let Some(users) = self.state.claim_users(&object).filter(|u| !u.is_empty()) {
                health = Health::phrase(health.status, terminating_used_by(&users));
            }
            self.state.health = Some(health);
        }
        let youngest = std::iter::once(object.metadata.creation_timestamp.as_ref().map(|t| t.0))
            .flatten()
            .chain(self.state.sections.iter().flat_map(|section| {
                match &section.body {
                    SectionBody::Fields(fields) => fields
                        .iter()
                        .filter_map(|field| match &field.value {
                            FieldValue::Time(time) => Some(*time),
                            _ => None,
                        })
                        .collect(),
                    _ => Vec::new(),
                }
            }))
            .map(|time| time.as_second())
            .max();
        self.state.ages.track(youngest, cx);
        self.sync_metrics(&kind, &object, cx);
    }

    /// Keeps [`Connections`] on the selected object; its links are
    /// re-emitted for the workspace to open.
    fn sync_connections(&mut self, cx: &mut Context<Self>) {
        let wanted = self
            .target
            .as_ref()
            .map(|target| (target.kind.clone(), target.object.clone()))
            .filter(|(kind, _)| Connections::applies(kind));
        let cluster = self.cluster.read(cx);
        let (Some((kind, object)), Some(cache), CatalogState::Ready(catalog)) =
            (wanted, cluster.cache().cloned(), cluster.catalog())
        else {
            self.connections = None;
            return;
        };
        if let Some((panel, _)) = &self.connections
            && panel.read(cx).watches(&cache, &object)
        {
            panel.update(cx, |panel, cx| panel.set_target(&kind, &object, cx));
            return;
        }
        let catalog = catalog.clone();
        let panel = cx.new(|cx| Connections::new(&cache, &catalog, &kind, &object, cx));
        // The panel redraws itself; only its links concern the browser.
        let subscription =
            cx.subscribe(&panel, |_, _, reveal: &Reveal, cx| cx.emit(reveal.clone()));
        self.connections = Some((panel, subscription));
    }

    /// Keeps [`WorkloadPods`] on the selected workload; a click on a pod
    /// shows it in place (with a way back), and a change of its pods
    /// redraws the details (the metrics block counts them).
    fn sync_pods(&mut self, cx: &mut Context<Self>) {
        let wanted = self.target.as_ref().and_then(|target| {
            let workload = WorkloadPods::workload(&target.kind)?;
            Some((workload, target.object.clone()))
        });
        let cluster = self.cluster.read(cx);
        let (Some((workload, object)), Some(cache), CatalogState::Ready(catalog)) =
            (wanted, cluster.cache().cloned(), cluster.catalog())
        else {
            self.pods = None;
            return;
        };
        if let Some((panel, _)) = &self.pods
            && panel.read(cx).follows(&cache, workload, &object)
        {
            panel.update(cx, |panel, cx| panel.set_object(object, cx));
            return;
        }
        let catalog = catalog.clone();
        let panel = cx.new(|cx| WorkloadPods::new(&cache, &catalog, workload, object, cx));
        let subscriptions = [
            cx.subscribe(&panel, |this, _, show: &ShowObject, cx| this.push(show, cx)),
            cx.subscribe(&panel, |_, _, _: &PodsChanged, cx| cx.notify()),
        ];
        self.pods = Some((panel, subscriptions));
    }

    /// Whether the "Pods" block lists what `spec` would: a workload's own
    /// related pods are not listed twice.
    fn superseded(&self, spec: &RelatedObjects) -> bool {
        spec.kind == "Pod"
            && spec.api_version == "v1"
            && self
                .target
                .as_ref()
                .is_some_and(|target| WorkloadPods::workload(&target.kind).is_some())
    }

    /// Starts watching what the sections list; keeps the watches that are
    /// still wanted.
    fn sync_related(&mut self, cx: &mut Context<Self>) {
        let specs: Vec<RelatedObjects> = self
            .state
            .sections
            .iter()
            .filter_map(|section| match &section.body {
                SectionBody::Related(spec) if !self.superseded(spec) => Some(spec.clone()),
                _ => None,
            })
            .collect();
        let current: Vec<&RelatedObjects> = self.state.related.iter().map(|l| &l.spec).collect();
        if current.len() == specs.len() && current.iter().zip(&specs).all(|(a, b)| *a == b) {
            return;
        }
        let cluster = self.cluster.read(cx);
        let (cache, catalog) = match (cluster.cache(), cluster.catalog()) {
            (Some(cache), CatalogState::Ready(catalog)) => (cache.clone(), catalog.clone()),
            _ => {
                self.state.related.clear();
                return;
            }
        };
        let mut lists = Vec::new();
        for spec in specs {
            let plugin = self.registry.plugin_for(spec.group(), &spec.kind);
            let kind: Option<ResourceKind> = catalog.find(spec.group(), &spec.kind).cloned();
            let watch = kind.map(|kind| {
                let filter = WatchFilter {
                    labels: spec.labels().map(str::to_owned),
                    ..WatchFilter::default()
                };
                cache.subscribe_filtered(&kind, spec.namespace.as_deref(), filter)
            });
            let listener = watch
                .as_ref()
                .map(|watch| watch_changes(watch.changes(), cx, |_, cx| cx.notify()));
            let status = match &watch {
                Some(_) => WatchStatus::Loading,
                None => WatchStatus::Error(kubach_core::cache::WatchError {
                    forbidden: false,
                    connection: false,
                    message: t!("details-not-served", kind = spec.kind.as_str()),
                }),
            };
            lists.push(RelatedList {
                spec,
                watch,
                plugin,
                generation: None,
                objects: Vec::new(),
                rows: Vec::new(),
                missing: Vec::new(),
                status,
                _listener: listener,
            });
        }
        self.state.related = lists;
    }

    /// Rebuilds the rows of lists whose watch changed.
    fn refresh_related(&mut self) {
        for list in &mut self.state.related {
            let Some(watch) = &list.watch else {
                continue;
            };
            let snapshot = watch.snapshot();
            if list.generation == Some(snapshot.generation) {
                continue;
            }
            list.generation = Some(snapshot.generation);
            list.status = snapshot.status.clone();
            let mut objects: Vec<Arc<DynamicObject>> = snapshot
                .objects
                .iter()
                .filter(|object| list.spec.matches(object))
                .cloned()
                .collect();
            list.spec.sort(&mut objects);
            list.missing = list
                .spec
                .missing(&objects)
                .into_iter()
                .map(str::to_owned)
                .collect();
            list.rows = objects
                .iter()
                .map(|object| RelatedRow {
                    name: object.metadata.name.clone().unwrap_or_default(),
                    namespace: object.metadata.namespace.clone(),
                    brief: list.plugin.brief(object),
                })
                .collect();
            list.objects = objects;
        }
    }

    /// The pods of the selected workload (from the "Pods" block, or the
    /// sections' related list), and a version that changes with them.
    fn related_pods(&self, cx: &App) -> Option<(Vec<Arc<DynamicObject>>, Option<u64>)> {
        if let Some((panel, _)) = &self.pods {
            let (pods, version) = panel.read(cx).pods();
            return Some((pods.to_vec(), Some(version)));
        }
        self.state
            .related
            .iter()
            .find(|list| list.spec.kind == "Pod" && list.spec.api_version == "v1")
            .map(|list| (list.objects.clone(), list.generation))
    }

    fn metrics_subject(
        &self,
        target: Target,
        object: &DynamicObject,
        cx: &App,
    ) -> Option<MetricsSubject> {
        let subject = match &target {
            Target::Pod { name, .. } => {
                let pod: Pod = decode(object)?;
                let spec = pod.spec.as_ref();
                let parts = spec.map_or(0, |s| {
                    s.containers.len()
                        + s.init_containers
                            .iter()
                            .flatten()
                            .filter(|c| c.restart_policy.as_deref() == Some("Always"))
                            .count()
                });
                let (cpu, memory) = references(&[pod_resources(&pod)]);
                MetricsSubject {
                    pods: vec![name.clone()],
                    parts,
                    cpu,
                    memory,
                    target,
                }
            }
            Target::Workload { .. } => {
                let pods: Vec<Arc<DynamicObject>> = self
                    .related_pods(cx)
                    .map(|(pods, _)| pods.into_iter().filter(|p| pod_is_active(p)).collect())
                    .unwrap_or_default();
                let resources: Vec<(Resources, Resources)> = pods
                    .iter()
                    .filter_map(|p| decode::<Pod>(p))
                    .map(|pod| pod_resources(&pod))
                    .collect();
                let (cpu, memory) = references(&resources);
                MetricsSubject {
                    pods: pods
                        .iter()
                        .filter_map(|p| p.metadata.name.clone())
                        .collect(),
                    parts: pods.len(),
                    cpu,
                    memory,
                    target,
                }
            }
            Target::Node { .. } => {
                let allocatable = decode::<Node>(object)
                    .map(|node| node_allocatable(&node))
                    .unwrap_or_default();
                MetricsSubject {
                    pods: Vec::new(),
                    parts: 0,
                    cpu: Reference {
                        request: None,
                        limit: cores(allocatable.cpu),
                    },
                    memory: Reference {
                        request: None,
                        limit: bytes(allocatable.memory),
                    },
                    target,
                }
            }
        };
        Some(subject)
    }

    /// Creates, updates or drops the metrics block for the selected object.
    fn sync_metrics(
        &mut self,
        kind: &ResourceKind,
        object: &DynamicObject,
        cx: &mut Context<Self>,
    ) {
        let target = object.metadata.name.as_deref().and_then(|name| {
            Target::of(
                &kind.api.group,
                &kind.api.kind,
                object.metadata.namespace.as_deref(),
                name,
            )
        });
        let Some(target) = target else {
            self.metrics = None;
            self.state.metrics_key = None;
            return;
        };
        let status = super::prometheus_status(&self.cluster, cx);
        let hint = status.as_ref().and_then(PrometheusStatus::problem);
        let source = match status.as_ref().and_then(PrometheusStatus::prometheus) {
            Some(prometheus) => MetricsSource::Prometheus(prometheus.clone()),
            None => {
                let searching = status.as_ref().is_some_and(PrometheusStatus::pending);
                let fallback = if searching {
                    MetricsSource::Searching
                } else {
                    MetricsSource::None
                };
                // The table's metrics-server polls, shared.
                self.cluster
                    .read(cx)
                    .metrics_server()
                    .map(|server| MetricsSource::Server(Box::new(server)))
                    .unwrap_or(fallback)
            }
        };
        let source_key = source.key();
        let key = (
            self.state.version.clone(),
            self.related_pods(cx).and_then(|(_, generation)| generation),
            source_key.clone(),
        );
        let same_block = self
            .metrics
            .as_ref()
            .is_some_and(|metrics| metrics.read(cx).shows(&target, &source_key));
        if same_block && self.state.metrics_key.as_ref() == Some(&key) {
            if let Some(metrics) = &self.metrics {
                metrics.update(cx, |metrics, cx| metrics.set_hint(hint, cx));
            }
            return;
        }
        let Some(subject) = self.metrics_subject(target.clone(), object, cx) else {
            self.metrics = None;
            return;
        };
        self.state.metrics_key = Some(key);
        match &self.metrics {
            Some(metrics) if same_block => {
                metrics.update(cx, |metrics, cx| {
                    metrics.set_subject(subject, cx);
                    metrics.set_hint(hint, cx);
                });
            }
            _ => {
                let context = self
                    .cluster
                    .read(cx)
                    .active()
                    .map(|context| context.name.clone())
                    .unwrap_or_default();
                self.metrics = Some(cx.new(|cx| {
                    let mut metrics = ResourceMetrics::new(context, subject, source, cx);
                    metrics.set_hint(hint, cx);
                    metrics
                }));
            }
        }
    }

    fn toggle(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.state.toggled.remove(&key) {
            self.state.toggled.insert(key);
        }
        cx.notify();
    }

    /// Whether the fold `key` is open, given its default.
    fn is_open(&self, key: &str, default: bool) -> bool {
        default != self.state.toggled.contains(key)
    }

    fn toggle_listener(
        &self,
        key: String,
        cx: &mut Context<Self>,
    ) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle(key.clone(), cx))
    }

    fn render_header(
        &self,
        target: &DetailsTarget,
        object: &Arc<DynamicObject>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let health = self
            .state
            .health
            .clone()
            .unwrap_or_else(|| target.plugin.health(object));
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child(object.metadata.name.clone().unwrap_or_default()),
            )
            .children(header_badge(target, object, &health))
            .child(crate::editor::edit_button(
                &self.cluster,
                &target.kind,
                object,
            ))
            .child(
                Button::new("close-details")
                    .ghost()
                    .small()
                    .icon(IconName::Close)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DetailsEvent::Close))),
            )
    }

    /// "← Deployment web" over an object shown in place: back one frame.
    fn render_back(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let label = stack::back_label(self.base.as_ref(), &self.stack, self.home.as_deref())?;
        Some(
            div()
                .min_w_0()
                .child(
                    kubach_ui::BackButton::new("details-back", label.clone())
                        .tooltip(t!("details-back-tooltip", place = label.as_str()))
                        .on_click(cx.listener(|this, _, _, cx| this.back(cx))),
                )
                .into_any_element(),
        )
    }

    /// Logs / Shell / Debug / Port forward, the kind's own actions
    /// (restart, trigger, evict, delete, …) and a replica stepper for
    /// scalable kinds. Dangerous ones are confirmed like bulk actions.
    fn render_actions(
        &self,
        target: &DetailsTarget,
        object: &Arc<DynamicObject>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let streams = crate::dock::stream_actions(object, target.plugin.as_ref());
        let running = target.running;
        let actions = target.plugin.actions_for(object);
        let mut buttons: Vec<AnyElement> = actions
            .iter()
            .filter(|a| {
                // Streams have buttons of their own (above).
                a.kind == ActionKind::Api && target.plugin.action_params(object, &a.id).is_empty()
            })
            .enumerate()
            .map(|(ix, action)| {
                let action = action.clone();
                let object = object.clone();
                let (kind, plugin) = (target.kind.clone(), target.plugin.clone());
                let icon: gpui_kit::assets::IconName = match action.id {
                    action_id::RESTART => IconName::RotateCw.into(),
                    action_id::DELETE => gpui_kit::assets::IconName::Trash,
                    action_id::EVICT => IconName::ArrowRight.into(),
                    _ => IconName::Play.into(),
                };
                let tooltip = match action.id {
                    action_id::EVICT => t!("details-evict-tooltip"),
                    _ => plugin_text::action_title(&action),
                };
                Button::new(("details-action", ix))
                    .outline()
                    .small()
                    .icon(icon)
                    .tooltip(tooltip)
                    .disabled(running)
                    .when(action.dangerous, |button| button.danger())
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(DetailsEvent::Action {
                            action: action.clone(),
                            object: object.clone(),
                            kind: kind.clone(),
                            plugin: plugin.clone(),
                        })
                    }))
                    .into_any_element()
            })
            .collect();
        buttons.extend(node_buttons(target, object, running, cx));
        let scalable = actions.iter().any(|a| a.id == action_id::SCALE)
            && !target
                .plugin
                .action_params(object, &action_id::SCALE)
                .is_empty();
        let scale = scalable.then(|| self.render_scale(target, object, running, cx));
        if streams.is_none() && buttons.is_empty() && scale.is_none() {
            return None;
        }
        // Streams, the kind's actions and the replica stepper share one row
        // of icon buttons; tooltips name them.
        Some(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .children(streams)
                .children(buttons)
                .children(scale),
        )
    }

    /// The replica stepper (#347): each click steps from the count the
    /// cluster has now, read from `/scale` when it runs; off while the
    /// shown data may be stale; a step to zero is confirmed; a note when an
    /// HPA owns the count. Buttons stay off while a step runs.
    fn render_scale(
        &self,
        target: &DetailsTarget,
        object: &Arc<DynamicObject>,
        running: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let desired = object
            .data
            .pointer("/spec/replicas")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(1);
        let blocked = scale_blocked(target.provisional, target.from_disk, target.reconnecting);
        let hpa = self.hpa_of(target, object, cx);
        let step = |id: &'static str, icon: IconName, delta: i32, cx: &mut Context<Self>| {
            let object = object.clone();
            let (kind, plugin) = (target.kind.clone(), target.plugin.clone());
            let tooltip = blocked.clone().unwrap_or_else(|| {
                if delta < 0 {
                    t!("details-scale-down")
                } else {
                    t!("details-scale-up")
                }
            });
            Button::new(id)
                .outline()
                .small()
                .icon(icon)
                .disabled(running || blocked.is_some() || desired + i64::from(delta) < 0)
                .tooltip(tooltip)
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(DetailsEvent::Scale {
                        object: object.clone(),
                        kind: kind.clone(),
                        plugin: plugin.clone(),
                        delta,
                        confirm: scale::needs_confirmation(desired, delta),
                    })
                }))
        };
        let down = step("scale-down", IconName::Minus, -1, cx);
        let up = step("scale-up", IconName::Plus, 1, cx);
        let theme = cx.theme();
        // Why it is off, or the HPA that owns the count: an icon with the
        // reason in its tooltip, like the other buttons of the row.
        let note = blocked
            .map(|reason| (IconName::Info, reason, theme.muted_foreground))
            .or_else(|| {
                hpa.map(|name| {
                    (
                        IconName::TriangleAlert,
                        t!("details-scale-hpa", name = name),
                        theme.warning,
                    )
                })
            })
            .map(|(icon, text, color)| {
                Button::new("scale-note")
                    .ghost()
                    .small()
                    .icon(Icon::new(icon).text_color(color))
                    .tooltip(text)
            });
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(down)
            .child(
                div()
                    .px_1()
                    .text_sm()
                    .text_color(theme.foreground)
                    .child(t!("details-replicas", count = desired)),
            )
            .child(up)
            .children(note)
            .into_any_element()
    }

    /// The HPA of the namespace that targets `object`, from HPAs already
    /// watched (no watch is started for this).
    fn hpa_of(&self, target: &DetailsTarget, object: &DynamicObject, cx: &App) -> Option<String> {
        let namespace = object.metadata.namespace.as_deref()?;
        let cluster = self.cluster.read(cx);
        let (Some(cache), CatalogState::Ready(catalog)) = (cluster.cache(), cluster.catalog())
        else {
            return None;
        };
        let kind = catalog.find("autoscaling", "HorizontalPodAutoscaler")?;
        let hpas = cache.loaded_objects_in(kind, namespace)?;
        let api = &target.kind.api;
        scale::hpa_for(
            &hpas,
            &api.group,
            &api.kind,
            object.metadata.name.as_deref()?,
        )
    }

    fn render_value(
        &self,
        id: SharedString,
        value: &FieldValue,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        match value {
            FieldValue::Text(text) => div()
                .whitespace_normal()
                .child(clip(text))
                .into_any_element(),
            FieldValue::Phrase(phrase) => div()
                .whitespace_normal()
                .child(clip(&phrase.to_string()))
                .into_any_element(),
            FieldValue::Link {
                label,
                target,
                phrase,
            } => {
                let reveal = reveal_of(target);
                Button::new(id)
                    .link()
                    .small()
                    .label(
                        phrase
                            .as_ref()
                            .map_or_else(|| label.clone(), Message::to_string),
                    )
                    .tooltip(if target.name.is_empty() {
                        // A link without a name opens the kind's list (CRD objects).
                        t!("details-show-all", kind = target.kind.as_str())
                    } else {
                        t!(
                            "details-open",
                            kind = target.kind.as_str(),
                            name = target.name.as_str()
                        )
                    })
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(reveal.clone())))
                    .into_any_element()
            }
            FieldValue::Labels(pairs) => ChipList::new(id, pairs.clone()).into_any_element(),
            FieldValue::Replicas(counts) => div()
                .flex()
                .flex_col()
                .gap_1()
                .child(ReplicaBar::new(
                    counts.desired.max(0) as u32,
                    counts.current.max(0) as u32,
                    counts.ready.max(0) as u32,
                ))
                .child(
                    div()
                        .whitespace_normal()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(plugin_text::replicas(counts)),
                )
                .into_any_element(),
            FieldValue::Time(time) => div()
                .flex()
                .flex_wrap()
                .gap_x_2()
                .child(ago(time.as_second()))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(time.to_string()),
                )
                .into_any_element(),
            FieldValue::Usage {
                used,
                hard,
                permille,
            } => {
                let share = permille
                    .map(|p| format!(" · {}%", p / 10))
                    .unwrap_or_default();
                UsageMeter::new("", format!("{used} / {hard}{share}"))
                    .fill(permille.map(|p| p as f32 / 1000.))
                    .into_any_element()
            }
        }
    }

    /// Labels and annotations: hidden until opened, they are rarely what
    /// the panel is opened for and can be long.
    fn render_chips(
        &self,
        key: &'static str,
        pairs: &[(String, String)],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        ChipList::new(key, pairs.iter().cloned())
            .folded(0)
            .expanded(self.is_open(key, false))
            .on_toggle(self.toggle_listener(key.to_owned(), cx))
            .into_any_element()
    }

    /// Name, namespace, created, UID, the kind's overview fields, owners,
    /// managers, labels and annotations.
    fn render_main(
        &self,
        target: &DetailsTarget,
        object: &Arc<DynamicObject>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let meta = &object.metadata;
        let mut list =
            KeyValueList::new().row(t!("details-name"), meta.name.clone().unwrap_or_default());
        if let Some(namespace) = &meta.namespace {
            list = list.row(t!("details-namespace"), namespace.clone());
        }
        if let Some(created) = &meta.creation_timestamp {
            list = list.row(
                t!("details-created"),
                self.render_value("created".into(), &FieldValue::Time(created.0), cx),
            );
        }
        if let Some(uid) = &meta.uid {
            list = list.row(
                t!("details-uid"),
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(div().min_w_0().truncate().text_xs().child(uid.clone()))
                    .child(
                        Clipboard::new("copy-uid")
                            .value(uid.clone())
                            .tooltip(t!("details-copy-uid")),
                    ),
            );
        }
        let overview: Vec<(String, FieldValue)> = self
            .state
            .sections
            .iter()
            .filter(|s| s.placement == Placement::Overview)
            .flat_map(|s| match &s.body {
                SectionBody::Fields(fields) => fields
                    .iter()
                    .map(|Field { key, value }| (key.clone(), value.clone()))
                    .collect::<Vec<_>>(),
                SectionBody::KeyValues(pairs) => pairs
                    .iter()
                    .map(|(k, v)| (k.clone(), FieldValue::Text(v.clone())))
                    .collect(),
                _ => Vec::new(),
            })
            .collect();
        let has_reason = overview.iter().any(|(key, _)| key == "Reason");
        for (ix, (key, value)) in overview.into_iter().enumerate() {
            let id = SharedString::from(format!("overview-{ix}"));
            let value = localized_value(&key, &value);
            list = list.row(plugin_text::label(&key), self.render_value(id, &value, cx));
        }
        let health = self
            .state
            .health
            .clone()
            .unwrap_or_else(|| target.plugin.health(object));
        if !has_reason
            && !health.message.is_empty()
            && health.message != rows::status_name(health.status)
        {
            list = list.row(
                t!("details-reason"),
                div()
                    .whitespace_normal()
                    .child(plugin_text::health_message(&health)),
            );
        }
        if let Some(badge) = super::super::security::pod_badge(&target.kind, object) {
            list = list.row(t!("details-security"), badge);
        }
        for (ix, owner) in meta.owner_references.iter().flatten().enumerate() {
            let reveal = Reveal {
                group: group_of(&owner.api_version).to_owned(),
                kind: owner.kind.clone(),
                namespace: meta.namespace.clone(),
                name: owner.name.clone(),
            };
            list = list.row(
                if ix == 0 {
                    t!("details-owner")
                } else {
                    String::new()
                },
                Button::new(("owner", ix))
                    .link()
                    .small()
                    .label(format!("{} {}", owner.kind, owner.name))
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(reveal.clone()))),
            );
        }
        let managers = self
            .managed_by
            .as_ref()
            .map(|(panel, _)| panel.read(cx).found())
            .unwrap_or_default();
        for (ix, (title, reveal)) in managers.into_iter().enumerate() {
            let label = match &reveal.namespace {
                Some(namespace) => format!("{title} {namespace}/{}", reveal.name),
                None => format!("{title} {}", reveal.name),
            };
            list = list.row(
                if ix == 0 {
                    t!("details-managed-by")
                } else {
                    String::new()
                },
                Button::new(("managed-by", ix))
                    .link()
                    .small()
                    .label(label)
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(reveal.clone()))),
            );
        }
        let labels: Vec<(String, String)> = meta
            .labels
            .iter()
            .flatten()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if !labels.is_empty() {
            list = list.row(
                t!("details-labels"),
                self.render_chips("labels", &labels, cx),
            );
        }
        let annotations: Vec<(String, String)> = meta
            .annotations
            .iter()
            .flatten()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if !annotations.is_empty() {
            list = list.row(
                t!("details-annotations"),
                self.render_chips("annotations", &annotations, cx),
            );
        }
        Panel::new().child(list)
    }

    /// Watches the dock's port-forwards once the dock is there (it is made
    /// after the browser): a forward started or stopped redraws the ports.
    fn sync_forwards(&mut self, cx: &mut Context<Self>) {
        if self.forwards.is_some() {
            return;
        }
        if let Some(forwards) = crate::dock::forwards(cx) {
            let subscription = cx.observe(&forwards, |this, _, cx| {
                if this.target.is_some() {
                    cx.notify();
                }
            });
            self.forwards = Some((forwards, subscription));
        }
    }

    /// The ports of a pod's container as chips (#372): a click forwards the
    /// port the way the Port forward button does; a forwarded port shows
    /// its local address with Open, Copy and Stop. Ports that cannot be
    /// forwarded (not TCP, or not in a pod) stay plain text.
    fn render_ports(&self, ix: usize, container: &ContainerInfo, cx: &App) -> AnyElement {
        let source = self
            .target
            .as_ref()
            .and_then(|target| ForwardSource::from_object(&target.object))
            .filter(|source| source.kind == "Pod");
        let ports: Vec<ForwardPort> = source
            .as_ref()
            .map(|source| container_forward_ports(source, &container.name))
            .unwrap_or_default();
        let (Some(source), false) = (source, ports.is_empty()) else {
            return div()
                .whitespace_normal()
                .child(container.ports.join(", "))
                .into_any_element();
        };
        let forwards = self.forwards.as_ref().map(|(forwards, _)| forwards.clone());
        let list: Vec<ForwardInfo> = forwards
            .as_ref()
            .map(|forwards| forwards.read(cx).list().to_vec())
            .unwrap_or_default();
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .children(ports.iter().enumerate().map(|(p, port)| {
                let key = format!("{ix}-{p}");
                let active = crate::dock::find_forward(&list, &source, port.port);
                render_port(&key, &source, port, active, forwards.clone())
            }))
            .into_any_element()
    }

    fn render_container(
        &self,
        ix: usize,
        container: &ContainerInfo,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let role = match container.role {
            ContainerRole::Init => Some("init"),
            ContainerRole::Sidecar => Some("sidecar"),
            ContainerRole::App => None,
        };
        let mut state = plugin_text::container_phase(&container.phase);
        if let ContainerPhase::Running { since: Some(since) } = &container.phase {
            state.push_str(&format!(" · {}", ago(since.as_second())));
        }
        let ready = match container.ready {
            Some(true) => Some(t!("details-container-ready")),
            Some(false) => Some(t!("details-container-not-ready")),
            None => None,
        };
        let restarts = (container.restarts > 0)
            .then(|| t!("details-container-restarts", count = container.restarts));
        let summary = ready
            .into_iter()
            .chain(restarts)
            .collect::<Vec<_>>()
            .join(" · ");
        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .child(StatusCubes::new(
                SharedString::from(format!("container-cube-{ix}")),
                [rows::container_cube(&container.brief())],
            ))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child(container.name.clone()),
            )
            .children(role.map(|role| {
                div()
                    .px_1p5()
                    .rounded_sm()
                    .bg(theme.muted)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(role)
            }))
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(summary),
            );
        let mut list = KeyValueList::new().row(
            t!("details-image"),
            div().whitespace_normal().child(container.image.clone()),
        );
        list = list.row(t!("details-state"), div().whitespace_normal().child(state));
        if let Some(last) = &container.last_state {
            list = list.row(
                t!("details-last-state"),
                div().whitespace_normal().child(last.clone()),
            );
        }
        if !container.ports.is_empty() {
            list = list.row(t!("details-ports"), self.render_ports(ix, container, cx));
        }
        for (name, probe) in &container.probes {
            list = list.row(name.clone(), div().whitespace_normal().child(probe.clone()));
        }
        list = list
            .row(t!("details-requests"), container.requests.clone())
            .row(t!("details-limits"), container.limits.clone());
        let fold =
            |title: String, key: String, pairs: &[(String, String)], cx: &mut Context<Self>| {
                let open = self.is_open(&key, false);
                let toggle = div()
                    .id(SharedString::from(key.clone()))
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .cursor_pointer()
                    .hover(|row| row.text_color(theme.foreground))
                    .on_click(self.toggle_listener(key, cx))
                    .child(
                        gpui_kit::component::Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .xsmall(),
                    )
                    .child(title);
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(toggle)
                    .when(open, |column| {
                        column.child(pairs.iter().fold(KeyValueList::new(), |list, (k, v)| {
                            list.row(k.clone(), div().whitespace_normal().child(clip(v)))
                        }))
                    })
            };
        let env = (!container.env.is_empty()).then(|| {
            fold(
                t!("details-environment", count = container.env.len()),
                format!("env:{}", container.name),
                &container.env,
                cx,
            )
        });
        let mounts = (!container.mounts.is_empty()).then(|| {
            fold(
                t!("details-mounts", count = container.mounts.len()),
                format!("mounts:{}", container.name),
                &container.mounts,
                cx,
            )
        });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .child(header)
            .child(list)
            .children(env)
            .children(mounts)
    }

    fn render_related(&self, list: &RelatedList, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let note = |text: String| {
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(text)
                .into_any_element()
        };
        if list.rows.is_empty() && list.missing.is_empty() {
            return match &list.status {
                WatchStatus::Loading => note(t!("details-loading")),
                WatchStatus::Error(error) => {
                    note(t!("details-unavailable", error = error.message.as_str()))
                }
                WatchStatus::Ready => note(t!("details-none")),
            };
        }
        let group = list.spec.group().to_owned();
        let kind = list.spec.kind.clone();
        let rows = list.rows.iter().enumerate().map(|(ix, row)| {
            let reveal = Reveal {
                group: group.clone(),
                kind: kind.clone(),
                namespace: row.namespace.clone(),
                name: row.name.clone(),
            };
            let status = if row.brief.containers.is_empty() {
                StatusDot::new(rows::tone(row.brief.health.status)).into_any_element()
            } else {
                StatusCubes::new(
                    SharedString::from(format!("related-{kind}-{ix}")),
                    row.brief.containers.iter().map(rows::container_cube),
                )
                .into_any_element()
            };
            let mut note = plugin_text::brief_note(&row.brief);
            if row.brief.health.status != HealthStatus::Healthy
                && !row.brief.health.message.is_empty()
            {
                let reason = plugin_text::health_message(&row.brief.health);
                note = if note.is_empty() {
                    reason
                } else {
                    format!("{reason} · {note}")
                };
            }
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(status)
                .child(
                    div().min_w_0().child(
                        Button::new(SharedString::from(format!("related-{kind}-link-{ix}")))
                            .link()
                            .small()
                            .label(row.name.clone())
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(reveal.clone()))),
                    ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_right()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(note),
                )
                .into_any_element()
        });
        let missing = list.missing.iter().map(|name| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(StatusDot::new(Tone::Neutral))
                .child(name.clone())
                .child(
                    div()
                        .flex_1()
                        .text_right()
                        .text_xs()
                        .child(t!("details-not-created-yet")),
                )
                .into_any_element()
        });
        div()
            .flex()
            .flex_col()
            .gap_1p5()
            .children(rows)
            .children(missing)
            .into_any_element()
    }

    fn render_conditions(conditions: &[ConditionInfo], cx: &App) -> AnyElement {
        let theme = cx.theme();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(conditions.iter().map(|c| {
                let detail = match (&c.reason, &c.message) {
                    (Some(reason), Some(message)) => format!("{reason}: {message}"),
                    (Some(text), None) | (None, Some(text)) => text.clone(),
                    (None, None) => String::new(),
                };
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(StatusBadge::new(c.type_.clone(), condition_tone(c)))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(c.status.clone()),
                            )
                            .child(div().flex_1())
                            .children(c.last_transition.map(|time| {
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(ago(time.as_second()))
                            })),
                    )
                    .when(!detail.is_empty(), |row| {
                        row.child(
                            div()
                                .whitespace_normal()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(clip(&detail)),
                        )
                    })
            }))
            .into_any_element()
    }

    fn render_section(
        &self,
        ix: usize,
        section: &DetailSection,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let body: AnyElement = match &section.body {
            // Keys of these sections are the user's (ConfigMap keys,
            // StorageClass parameters, selector labels), not our labels.
            SectionBody::KeyValues(pairs) if USER_KEYED.contains(&section.title.as_str()) => pairs
                .iter()
                .fold(KeyValueList::new(), |list, (k, v)| {
                    list.row(k.clone(), div().whitespace_normal().child(clip(v)))
                })
                .into_any_element(),
            SectionBody::KeyValues(pairs) => pairs
                .iter()
                .fold(KeyValueList::new(), |list, (k, v)| {
                    let v = plugin_text::field_value(k, v);
                    list.row(
                        plugin_text::label(k),
                        div().whitespace_normal().child(clip(&v)),
                    )
                })
                .into_any_element(),
            SectionBody::Notes(notes) => notes
                .iter()
                .fold(KeyValueList::new(), |list, note| {
                    let locale = kubach_i18n::locale();
                    list.row(
                        note.key_in(locale),
                        div()
                            .whitespace_normal()
                            .child(clip(&note.value_in(locale))),
                    )
                })
                .into_any_element(),
            SectionBody::Fields(fields) => {
                let mut list = KeyValueList::new();
                for (row, field) in fields.iter().enumerate() {
                    let id = SharedString::from(format!("section-{ix}-{row}"));
                    let value = localized_value(&field.key, &field.value);
                    list = list.row(
                        plugin_text::label(&field.key),
                        self.render_value(id, &value, cx),
                    );
                }
                list.into_any_element()
            }
            SectionBody::Labels(pairs) => {
                ChipList::new(SharedString::from(format!("section-{ix}")), pairs.clone())
                    .into_any_element()
            }
            SectionBody::Conditions(conditions) => Self::render_conditions(conditions, cx),
            SectionBody::Containers(containers) => div()
                .flex()
                .flex_col()
                .gap_2()
                .children(
                    containers
                        .iter()
                        .enumerate()
                        .map(|(c, container)| self.render_container(ix * 100 + c, container, cx)),
                )
                .into_any_element(),
            SectionBody::Related(spec) => {
                match self.state.related.iter().find(|l| &l.spec == spec) {
                    Some(list) => self.render_related(list, cx),
                    None => div().into_any_element(),
                }
            }
            SectionBody::Table(table) => table
                .rows
                .iter()
                .fold(
                    CompactTable::new(table.columns.iter().map(|c| plugin_text::label(c))),
                    |t, row| t.row(row.clone()),
                )
                .into_any_element(),
        };
        let note = match &section.body {
            SectionBody::Related(spec) => self
                .state
                .related
                .iter()
                .find(|l| &l.spec == spec)
                .filter(|l| !l.rows.is_empty())
                .map(|l| l.rows.len().to_string()),
            SectionBody::Containers(containers) if containers.len() > 1 => {
                Some(containers.len().to_string())
            }
            _ => None,
        };
        let key = format!("section:{}", section.title);
        let open = self.is_open(&key, !section.collapsed);
        Panel::new()
            .title(plugin_text::section_title(&section.title))
            .when_some(note, |panel, note| panel.note(note))
            .collapsible(
                SharedString::from(format!("section-toggle-{ix}")),
                open,
                self.toggle_listener(key, cx),
            )
            .child(body)
    }

    /// The whole details column; the browser puts it in its side panel.
    fn render_details(
        &self,
        target: &DetailsTarget,
        object: &Arc<DynamicObject>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let back = self.render_back(cx);
        let header = self.render_header(target, object, cx);
        let actions = self.render_actions(target, object, cx);
        let main = self.render_main(target, object, cx);
        let shown = |s: &&DetailSection| {
            s.placement == Placement::Section
                && match &s.body {
                    SectionBody::KeyValues(p) | SectionBody::Labels(p) => !p.is_empty(),
                    SectionBody::Notes(n) => !n.is_empty(),
                    SectionBody::Fields(f) => !f.is_empty(),
                    SectionBody::Conditions(c) => !c.is_empty(),
                    SectionBody::Containers(c) => !c.is_empty(),
                    SectionBody::Related(spec) => !self.superseded(spec),
                    SectionBody::Table(t) => !t.rows.is_empty(),
                }
        };
        let is_conditions = |s: &&DetailSection| matches!(s.body, SectionBody::Conditions(_));
        let sections: Vec<(usize, &DetailSection)> = self
            .state
            .sections
            .iter()
            .enumerate()
            .filter(|(_, s)| shown(s) && !is_conditions(s))
            .chain(
                self.state
                    .sections
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| shown(s) && is_conditions(s)),
            )
            .collect();
        let rendered: Vec<AnyElement> = sections
            .into_iter()
            .map(|(ix, section)| self.render_section(ix, section, cx).into_any_element())
            .collect();
        // The header and actions stay put; everything from the metrics
        // block down scrolls under them.
        let body = div()
            .id("resource-details")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_3()
            .children(self.metrics.clone())
            .child(main)
            .children(self.pods.as_ref().map(|(panel, _)| panel.clone()))
            .children(self.render_namespace_contents(cx))
            .children(self.secret.clone())
            .children(rendered)
            .children(self.object_events.clone())
            // Last: a canvas that only pans after a click (#460).
            .children(self.connections.as_ref().map(|(panel, _)| panel.clone()));
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && this.can_go_back() {
                    cx.stop_propagation();
                    this.back(cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Back),
                cx.listener(|this, _, _, cx| {
                    if this.can_go_back() {
                        cx.stop_propagation();
                        this.back(cx);
                    }
                }),
            )
            // `⌘[` / `Alt+←` go back in the panel's own stack first; with
            // nothing left there, the workspace goes back (#395).
            .on_action(cx.listener(|this, _: &super::super::GoBack, _, cx| {
                if this.can_go_back() {
                    this.back(cx);
                } else {
                    cx.propagate();
                }
            }))
            .children(back.map(|back| div().flex_none().child(back)))
            .child(div().flex_none().child(header))
            .children(actions.map(|actions| div().flex_none().child(actions)))
            .child(body)
    }
}

impl Render for ResourceDetails {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_secret(cx);
        self.sync_object_events(cx);
        self.sync_managed_by(cx);
        self.sync_connections(cx);
        self.sync_pods(cx);
        self.sync_details(cx);
        self.sync_namespace_contents(cx);
        self.sync_forwards(cx);
        if std::mem::take(&mut self.focus_pending) {
            window.focus(&self.focus, cx);
        }
        let Some(target) = self.target.clone() else {
            return self.render_pending(cx);
        };
        self.render_details(&target, &target.object, cx)
            .into_any_element()
    }
}

impl ResourceDetails {
    /// An object opened by reference while its watch has not listed it:
    /// the way back and "Loading…" (or "Not found"); nothing otherwise.
    fn render_pending(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(pending) = &self.pending else {
            return div().into_any_element();
        };
        let text = match pending.status() {
            WatchStatus::Loading => t!("details-loading"),
            WatchStatus::Ready => t!("details-not-found"),
            WatchStatus::Error(error) if error.forbidden => t!("details-no-access"),
            WatchStatus::Error(error) => t!("details-cant-load", error = error.message.as_str()),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .children(self.render_back(cx))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(text),
            )
            .into_any_element()
    }
}

/// Drain… and Shell for a Node (#318); nothing for other kinds. The
/// browser asks and runs them.
fn node_buttons(
    target: &DetailsTarget,
    object: &DynamicObject,
    running: bool,
    cx: &mut Context<ResourceDetails>,
) -> Vec<AnyElement> {
    let api = &target.kind.api;
    if !api.group.is_empty() || api.kind != "Node" {
        return Vec::new();
    }
    let node = object.metadata.name.clone().unwrap_or_default();
    let drain_node = node.clone();
    vec![
        Button::new("node-drain")
            .outline()
            .small()
            .danger()
            .icon(IconName::ArrowRight)
            .tooltip(t!("details-drain-tooltip"))
            .disabled(running)
            .on_click(
                cx.listener(move |_, _, _, cx| cx.emit(DetailsEvent::Drain(drain_node.clone()))),
            )
            .into_any_element(),
        Button::new("node-shell")
            .outline()
            .small()
            .icon(IconName::SquareTerminal)
            .tooltip(t!("details-node-shell-tooltip"))
            .on_click(
                cx.listener(move |_, _, _, cx| cx.emit(DetailsEvent::NodeShell(node.clone()))),
            )
            .into_any_element(),
    ]
}

/// Key/value sections whose keys are the user's data rather than labels.
const USER_KEYED: [&str; 6] = [
    "Data",
    "Binary data",
    "Parameters",
    "Selector",
    "Labels",
    "Annotations",
];

/// `value` of a field labelled `key`, with a text of ours (`Yes`) in the
/// language of the interface.
fn localized_value(key: &str, value: &FieldValue) -> FieldValue {
    match value {
        FieldValue::Text(text) => FieldValue::Text(plugin_text::field_value(key, text)),
        other => other.clone(),
    }
}

/// The state badge of the details header. Objects without a status
/// (ConfigMap, Secret, …) show their plugin's note, such as `3 keys`, or
/// nothing, rather than "Unknown" (#275).
fn header_badge(
    target: &DetailsTarget,
    object: &DynamicObject,
    health: &Health,
) -> Option<StatusBadge> {
    if health.status != HealthStatus::Unknown {
        return Some(StatusBadge::new(
            plugin_text::status_title(health.status),
            rows::tone(health.status),
        ));
    }
    let note = plugin_text::brief_note(&target.plugin.brief(object));
    (!note.is_empty()).then(|| StatusBadge::new(note, Tone::Neutral))
}

/// Why the replica stepper is off: the replicas shown may be stale (from
/// the disk cache, offline, or a watch reconnecting).
pub(super) fn scale_blocked(
    provisional: bool,
    from_disk: bool,
    reconnecting: bool,
) -> Option<String> {
    if provisional {
        Some(t!("details-scale-offline"))
    } else if from_disk {
        Some(t!("details-scale-loading"))
    } else if reconnecting {
        Some(t!("details-scale-reconnecting"))
    } else {
        None
    }
}

/// The ports `source` (a pod) declares in `container`, as forwardable ports.
fn container_forward_ports(source: &ForwardSource, container: &str) -> Vec<ForwardPort> {
    source
        .ports
        .iter()
        .filter(|port| port.container.as_deref() == Some(container))
        .cloned()
        .collect()
}

/// `https 10250/TCP`.
fn port_label(port: &ForwardPort) -> String {
    match &port.name {
        Some(name) => format!("{name} {}/{}", port.port, port.protocol),
        None => format!("{}/{}", port.port, port.protocol),
    }
}

/// What a click on a port does, for its tooltip.
fn port_tooltip(port: &ForwardPort, active: Option<&ForwardInfo>, url: Option<&str>) -> String {
    let number = port.port.to_string();
    match (active, url) {
        _ if !port.is_tcp() => t!("details-port-tcp-only", protocol = port.protocol.as_str()),
        (Some(info), Some(url)) => t!(
            "details-port-forwarded-open",
            address = info.local_addr.to_string(),
            url = url
        ),
        (Some(info), None) => t!(
            "details-port-forwarded-copy",
            address = info.local_addr.to_string()
        ),
        (None, _) => t!("details-port-forward", port = number),
    }
}

/// One port chip, plus Open / Copy / Stop when it is forwarded.
fn render_port(
    key: &str,
    source: &ForwardSource,
    port: &ForwardPort,
    active: Option<&ForwardInfo>,
    forwards: Option<Entity<Forwards>>,
) -> AnyElement {
    let url = active.and_then(|info| {
        crate::dock::http_scheme(port.port, port.name.as_deref())
            .map(|scheme| format!("{scheme}://{}", info.local_addr))
    });
    let tooltip = port_tooltip(port, active, url.as_deref());
    let chip = PortChip::new(
        SharedString::from(format!("container-port-{key}")),
        port_label(port),
    )
    .tooltip(tooltip)
    .disabled(!port.is_tcp())
    .forwarded(active.map(|info| info.local_addr.to_string()));
    let Some(info) = active else {
        let (source, number) = (source.clone(), port.port);
        return chip
            .on_click(move |_, window, cx| {
                crate::dock::start_forward(source.clone(), number, window, cx)
            })
            .into_any_element();
    };
    let address = info.local_addr.to_string();
    let id = info.id;
    let chip = {
        let (url, address) = (url.clone(), address.clone());
        chip.on_click(move |_, _, cx| match &url {
            Some(url) => cx.open_url(url),
            None => cx.write_to_clipboard(ClipboardItem::new_string(address.clone())),
        })
    };
    div()
        .flex()
        .items_center()
        .gap_0p5()
        .child(chip)
        .children(url.map(|url| {
            Button::new(SharedString::from(format!("container-port-open-{key}")))
                .ghost()
                .xsmall()
                .icon(IconName::ExternalLink)
                .tooltip(t!("details-port-open", url = url.as_str()))
                .on_click(move |_, _, cx| cx.open_url(&url))
        }))
        .child(
            Button::new(SharedString::from(format!("container-port-copy-{key}")))
                .ghost()
                .xsmall()
                .icon(IconName::Copy)
                .tooltip(t!("details-copy-address"))
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(address.clone()))
                }),
        )
        .child(
            Button::new(SharedString::from(format!("container-port-stop-{key}")))
                .ghost()
                .xsmall()
                .icon(IconName::Close)
                .tooltip(t!("details-stop-forward"))
                .on_click(move |_, _, cx| {
                    if let Some(forwards) = &forwards {
                        forwards.update(cx, |forwards, cx| forwards.stop(id, cx));
                    }
                }),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use kubach_core::metrics::Resources;
    use kubach_core::plugin::ConditionInfo;
    use kubach_core::scale::needs_confirmation;
    use kubach_ui::Tone;

    use kubach_core::streams::ForwardSource;
    use kube::api::DynamicObject;

    use super::{
        DetailsTarget, condition_tone, container_forward_ports, group_of, port_label, port_tooltip,
        references, scale_blocked,
    };

    /// The operator pod of kube-prometheus-stack as the API lists it: no
    /// `kind` (list items have none), Running, owned by a ReplicaSet (#372).
    fn listed_operator_pod() -> DynamicObject {
        serde_json::from_value(serde_json::json!({
            "metadata": {
                "name": "kube-prometheus-stack-operator-7d9c5b8f4-x2x7q",
                "namespace": "monitoring",
                "ownerReferences": [{
                    "apiVersion": "apps/v1", "kind": "ReplicaSet",
                    "name": "kube-prometheus-stack-operator-7d9c5b8f4",
                    "uid": "1", "controller": true
                }]
            },
            "spec": {"containers": [{
                "name": "kube-prometheus-stack",
                "image": "quay.io/prometheus-operator/prometheus-operator:v0.86.0",
                "ports": [{"name": "https", "containerPort": 10250, "protocol": "TCP"}]
            }]},
            "status": {
                "phase": "Running",
                "containerStatuses": [{
                    "name": "kube-prometheus-stack", "ready": true,
                    "state": {"running": {}}
                }]
            }
        }))
        .unwrap()
    }

    #[test]
    fn container_ports_of_a_listed_pod_are_forwardable() {
        let api = kube::api::ApiResource::erase::<k8s_openapi::api::core::v1::Pod>(&());
        let pod = kubach_core::discovery::typed(std::sync::Arc::new(listed_operator_pod()), &api);
        let source = ForwardSource::from_object(&pod).expect("a pod is a forward source");
        let ports = container_forward_ports(&source, "kube-prometheus-stack");
        assert_eq!(ports.len(), 1);
        assert_eq!(port_label(&ports[0]), "https 10250/TCP");
        assert_eq!(
            port_tooltip(&ports[0], None, None),
            "Port forward 10250: pick the local port"
        );
        assert!(container_forward_ports(&source, "other").is_empty());

        let mut udp = ports[0].clone();
        udp.protocol = "UDP".into();
        assert!(port_tooltip(&udp, None, None).contains("TCP only"));
    }

    #[test]
    fn stepper_is_off_on_stale_data() {
        assert_eq!(scale_blocked(false, false, false), None);
        assert!(scale_blocked(true, false, false).is_some());
        assert!(scale_blocked(false, true, false).is_some());
        assert!(scale_blocked(false, false, true).is_some());
        assert!(
            scale_blocked(true, true, true)
                .unwrap()
                .contains("Not connected")
        );
    }

    #[test]
    fn only_a_step_to_zero_is_confirmed() {
        // What the stepper shows decides; the server refuses an unconfirmed
        // zero when the cluster has fewer replicas than shown.
        assert!(needs_confirmation(1, -1));
        assert!(!needs_confirmation(3, -1));
        assert!(!needs_confirmation(1, 1));
    }

    fn condition(status: &str, negative: bool) -> ConditionInfo {
        ConditionInfo {
            type_: "X".into(),
            status: status.into(),
            reason: None,
            message: None,
            last_transition: None,
            negative,
            warning: false,
        }
    }

    #[test]
    fn condition_tones_follow_polarity() {
        assert_eq!(condition_tone(&condition("True", false)), Tone::Success);
        assert_eq!(condition_tone(&condition("False", false)), Tone::Warning);
        assert_eq!(condition_tone(&condition("True", true)), Tone::Danger);
        assert_eq!(condition_tone(&condition("False", true)), Tone::Success);
        assert_eq!(condition_tone(&condition("Unknown", false)), Tone::Neutral);
        assert_eq!(condition_tone(&condition("Unknown", true)), Tone::Neutral);
        // Warning types (`OrphanedResourceWarning`, `DisruptionTarget`).
        let warning = |status: &str| ConditionInfo {
            warning: true,
            ..condition(status, false)
        };
        assert_eq!(condition_tone(&warning("True")), Tone::Warning);
        assert_eq!(condition_tone(&warning("False")), Tone::Success);
    }

    #[test]
    fn references_are_in_cores_and_bytes() {
        let pod = |cpu_lim: Option<u64>| {
            (
                Resources {
                    cpu: Some(100_000_000),
                    memory: Some(1024),
                },
                Resources {
                    cpu: cpu_lim,
                    memory: Some(2048),
                },
            )
        };
        let (cpu, memory) = references(&[pod(Some(500_000_000)), pod(None)]);
        assert_eq!(cpu.request, Some(0.2));
        assert_eq!(cpu.limit, None);
        assert_eq!(memory.request, Some(2048.0));
        assert_eq!(memory.limit, Some(4096.0));
        let (cpu, _) = references(&[pod(Some(500_000_000))]);
        assert_eq!(cpu.limit, Some(0.5));
    }

    fn target(version: &str) -> DetailsTarget {
        use std::sync::Arc;

        use kubach_core::ResourceKind;
        use kubach_core::plugin::PluginRegistry;
        use kube::api::{ApiResource, DynamicObject, GroupVersionKind};

        let api = ApiResource::from_gvk(&GroupVersionKind::gvk("", "v1", "Pod"));
        let mut object = DynamicObject::new("web-0", &api).within("web");
        object.metadata.uid = Some("u1".into());
        object.metadata.resource_version = Some(version.into());
        DetailsTarget {
            plugin: PluginRegistry::with_builtins().plugin_for_resource(&api),
            kind: ResourceKind {
                api,
                namespaced: true,
                verbs: Vec::new(),
            },
            object: Arc::new(object),
            provisional: false,
            from_disk: false,
            reconnecting: false,
            running: false,
        }
    }

    #[test]
    fn details_redraw_only_for_a_change_they_show() {
        let shown = target("1");
        // A resync hands over an equal object anew: nothing to redraw.
        let mut same = target("1");
        same.plugin = shown.plugin.clone();
        assert!(shown.same_as(&same));
        assert!(!shown.same_as(&DetailsTarget {
            plugin: shown.plugin.clone(),
            ..target("2")
        }));
        // The buttons follow a running action and stale data.
        assert!(!shown.same_as(&DetailsTarget {
            running: true,
            ..same.clone()
        }));
        assert!(!shown.same_as(&DetailsTarget {
            reconnecting: true,
            ..same.clone()
        }));
        assert!(!shown.same_as(&DetailsTarget {
            from_disk: true,
            ..same
        }));
    }

    #[test]
    fn groups_of_api_versions() {
        assert_eq!(group_of("apps/v1"), "apps");
        assert_eq!(group_of("v1"), "");
    }
}
