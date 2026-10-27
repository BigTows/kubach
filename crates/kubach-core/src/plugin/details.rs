//! What the details panel of an object shows, as plain data: sections of
//! fields, chips, containers and related objects. Views decide how to draw
//! them; text output (MCP) uses [`DetailSection::text_lines`].

use k8s_openapi::jiff::Timestamp;
use kube::api::DynamicObject;

use kubach_i18n::{Locale, Message, msg};

use super::{Health, HealthStatus, ObjectRef};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConditionInfo {
    pub type_: String,
    pub status: String,
    pub reason: Option<String>,
    pub message: Option<String>,
    /// When the status last changed.
    pub last_transition: Option<Timestamp>,
    /// Whether `True` reports a problem (`MemoryPressure`, `ReplicaFailure`)
    /// rather than a good state (`Ready`, `Available`). Filled by the plugin;
    /// see [`ConditionInfo::negative_by_default`] for kinds it does not know.
    pub negative: bool,
    /// Whether `True` reports something to look at rather than a failure
    /// (Argo CD `OrphanedResourceWarning`, Pod `DisruptionTarget`, Flux
    /// `Reconciling`). A warning type is not [`negative`](Self::negative):
    /// `True` is a missing good state, `False` the good one.
    pub warning: bool,
}

impl ConditionInfo {
    /// Words in a condition type that report a problem when `True`.
    const NEGATIVE_WORDS: [&str; 6] = [
        "Pressure",
        "Failure",
        "Failed",
        "Degraded",
        "Unavailable",
        "Stalled",
    ];

    /// Polarity of a condition type no plugin has a list for (CRDs):
    /// negative when the type names a problem (`DiskPressure`,
    /// `ReplicaFailure`, `Degraded`, `NetworkUnavailable`, `Stalled`).
    pub fn negative_by_default(type_: &str) -> bool {
        Self::NEGATIVE_WORDS.iter().any(|word| type_.contains(word))
    }

    /// Whether the condition is in its good state: `Some(true)` for a
    /// positive type that is `True` or a negative or warning one that is
    /// `False`, `None` when the status is neither (`Unknown`).
    pub fn is_good(&self) -> Option<bool> {
        let bad_when_true = self.negative || self.warning;
        match self.status.as_str() {
            "True" => Some(!bad_when_true),
            "False" => Some(bad_when_true),
            _ => None,
        }
    }
}

/// Replica counts of a workload, drawn as a mini bar: `ready` of the
/// `current` pods are ready, out of `desired`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReplicaCounts {
    pub desired: i32,
    /// Pods that exist, ready or not.
    pub current: i32,
    pub ready: i32,
    pub updated: Option<i32>,
    pub available: Option<i32>,
}

impl ReplicaCounts {
    /// `3 desired · 3 current · 2 ready · 3 updated · 2 available`.
    pub fn describe(&self) -> String {
        let mut parts = vec![
            format!("{} desired", self.desired),
            format!("{} current", self.current),
            format!("{} ready", self.ready),
        ];
        if let Some(updated) = self.updated {
            parts.push(format!("{updated} updated"));
        }
        if let Some(available) = self.available {
            parts.push(format!("{available} available"));
        }
        parts.join(" · ")
    }
}

/// Value of a [`Field`].
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    Text(String),
    /// A text of ours, possibly with cluster data in its arguments
    /// (`TLS Terminate`): the UI translates it, text output shows English.
    Phrase(Message),
    /// Opens `target`; shown as `label`.
    Link {
        label: String,
        target: ObjectRef,
        /// `label` as a text of ours the UI translates (`Show widgets`);
        /// `None` when the label is a name.
        phrase: Option<Message>,
    },
    /// `key=value` chips (selectors, node selectors).
    Labels(Vec<(String, String)>),
    Replicas(ReplicaCounts),
    /// A moment in the past: a live age and the time itself.
    Time(Timestamp),
    /// Use of a limited resource (a quota): `used` of `hard`, drawn as a
    /// bar when both parse as quantities.
    Usage {
        used: String,
        hard: String,
        /// `used / hard` in thousandths, capped at 1000; `None` when a
        /// side does not parse or `hard` is zero.
        permille: Option<u32>,
    },
}

impl FieldValue {
    /// [`FieldValue::Usage`] of `used` out of `hard` (Kubernetes quantities).
    pub fn usage(used: impl Into<String>, hard: impl Into<String>) -> Self {
        let (used, hard) = (used.into(), hard.into());
        let parse = |s: &str| crate::metrics::parse_quantity(s).ok();
        let permille = parse(&used)
            .zip(parse(&hard))
            .filter(|(_, hard)| *hard > 0.0)
            .map(|(used, hard)| ((used / hard).clamp(0.0, 1.0) * 1000.0).round() as u32);
        Self::Usage {
            used,
            hard,
            permille,
        }
    }

    /// Plain text, for text output (MCP).
    pub fn text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Phrase(phrase) => phrase.english(),
            Self::Link { label, .. } => label.clone(),
            Self::Labels(pairs) => pairs
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(","),
            Self::Replicas(counts) => counts.describe(),
            Self::Time(time) => time.to_string(),
            Self::Usage { used, hard, .. } => format!("{used}/{hard}"),
        }
    }
}

/// Arguments of phrases are names, counts and cluster data, never NaN.
impl Eq for FieldValue {}

/// One row of a [`SectionBody::Fields`] section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub key: String,
    pub value: FieldValue,
}

impl Field {
    pub fn new(key: impl Into<String>, value: FieldValue) -> Self {
        Self {
            key: key.into(),
            value,
        }
    }

    pub fn text(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self::new(key, FieldValue::Text(value.into()))
    }

    /// A link to any object, shown as `label`.
    pub fn object_link(
        key: impl Into<String>,
        label: impl Into<String>,
        target: ObjectRef,
    ) -> Self {
        Self::new(
            key,
            FieldValue::Link {
                label: label.into(),
                target,
                phrase: None,
            },
        )
    }

    /// A text of ours as the value (`TLS Terminate`).
    pub fn phrase(key: impl Into<String>, phrase: Message) -> Self {
        Self::new(key, FieldValue::Phrase(phrase))
    }

    /// A link to any object, its label a text of ours (`Show widgets`).
    pub fn phrase_link(key: impl Into<String>, phrase: Message, target: ObjectRef) -> Self {
        Self::new(
            key,
            FieldValue::Link {
                label: phrase.english(),
                target,
                phrase: Some(phrase),
            },
        )
    }

    /// A link to an object in the core API group.
    pub fn link(key: impl Into<String>, kind: &str, name: &str, namespace: Option<&str>) -> Self {
        Self::new(
            key,
            FieldValue::Link {
                label: name.to_owned(),
                target: ObjectRef {
                    api_version: "v1".into(),
                    kind: kind.into(),
                    name: name.into(),
                    namespace: namespace.map(str::to_owned),
                },
                phrase: None,
            },
        )
    }
}

/// Where a container belongs in the pod.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerRole {
    Init,
    /// An init container with `restartPolicy: Always`.
    Sidecar,
    App,
}

/// State of one container, as the kubelet reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainerPhase {
    Running {
        since: Option<Timestamp>,
    },
    Waiting {
        reason: String,
        message: Option<String>,
    },
    Terminated {
        reason: String,
        exit_code: i32,
    },
    /// No status reported yet.
    Unknown,
}

/// A container of a pod with its status, for the containers block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerInfo {
    pub name: String,
    pub role: ContainerRole,
    pub image: String,
    pub phase: ContainerPhase,
    /// The previous run, e.g. `Terminated: Error (exit code 1)`.
    pub last_state: Option<String>,
    pub ready: Option<bool>,
    pub restarts: i32,
    pub ports: Vec<String>,
    /// `(Liveness, http-get :80/ every 10s)`.
    pub probes: Vec<(String, String)>,
    pub requests: String,
    pub limits: String,
    pub env: Vec<(String, String)>,
    /// `(volume, mount path)`.
    pub mounts: Vec<(String, String)>,
}

/// Whether a waiting reason means the container is failing rather than
/// on its way up.
fn failing(reason: &str) -> bool {
    reason.contains("BackOff")
        || reason.starts_with("Err")
        || reason.starts_with("Invalid")
        || reason.contains("Error")
}

/// What a container's status cube shows, one color per state. Computed in
/// one place for the pod table, the details panel and related lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerHealth {
    /// Running and ready (or without a readiness verdict).
    Ready,
    /// Running, but not ready.
    NotReady,
    /// Waiting on its way up: `ContainerCreating`, `PodInitializing`.
    Starting,
    /// Waiting because it keeps failing: `CrashLoopBackOff`, `ErrImagePull`.
    Failing,
    /// Exited with code 0.
    Completed,
    /// Exited with a non-zero code.
    Failed,
    /// No status reported yet.
    Unknown,
}

impl ContainerHealth {
    /// State of a container in `phase` with the kubelet's readiness.
    pub fn of(phase: &ContainerPhase, ready: Option<bool>) -> Self {
        match phase {
            ContainerPhase::Running { .. } if ready != Some(false) => ContainerHealth::Ready,
            ContainerPhase::Running { .. } => ContainerHealth::NotReady,
            ContainerPhase::Waiting { reason, .. } if failing(reason) => ContainerHealth::Failing,
            ContainerPhase::Waiting { .. } => ContainerHealth::Starting,
            ContainerPhase::Terminated { exit_code: 0, .. } => ContainerHealth::Completed,
            ContainerPhase::Terminated { .. } => ContainerHealth::Failed,
            ContainerPhase::Unknown => ContainerHealth::Unknown,
        }
    }

    /// Running and ready, or done without an error.
    pub fn is_ok(self) -> bool {
        matches!(self, ContainerHealth::Ready | ContainerHealth::Completed)
    }

    pub fn status(self) -> HealthStatus {
        match self {
            ContainerHealth::Ready | ContainerHealth::Completed => HealthStatus::Healthy,
            ContainerHealth::NotReady | ContainerHealth::Starting => HealthStatus::Progressing,
            ContainerHealth::Failing | ContainerHealth::Failed => HealthStatus::Failed,
            ContainerHealth::Unknown => HealthStatus::Unknown,
        }
    }
}

/// A container as a status cube shows it: state and tooltip text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerBrief {
    pub name: String,
    pub role: ContainerRole,
    pub health: ContainerHealth,
    /// `Running`, `Running, not ready`, `Waiting: CrashLoopBackOff (…)`.
    pub state: String,
    /// What [`state`](Self::state) describes, for a view that words it in
    /// the language of the interface.
    pub phase: ContainerPhase,
    pub restarts: i32,
}

impl ContainerBrief {
    pub fn new(
        name: impl Into<String>,
        role: ContainerRole,
        phase: &ContainerPhase,
        ready: Option<bool>,
        restarts: i32,
    ) -> Self {
        let health = ContainerHealth::of(phase, ready);
        let mut state = phase.text();
        if health == ContainerHealth::NotReady {
            state.push_str(", not ready");
        }
        Self {
            name: name.into(),
            role,
            health,
            state,
            phase: phase.clone(),
            restarts,
        }
    }
}

/// Containers neither running and ready nor completed: what a containers
/// column sorts by.
pub fn not_ready_count(containers: &[ContainerBrief]) -> usize {
    containers.iter().filter(|c| !c.health.is_ok()).count()
}

impl ContainerInfo {
    /// The state its status cube shows.
    pub fn state(&self) -> ContainerHealth {
        ContainerHealth::of(&self.phase, self.ready)
    }

    /// [`state`](Self::state) as a health with a short reason: running and
    /// ready is healthy, a crash loop or a failed exit is failed, anything
    /// in between progressing.
    pub fn health(&self) -> Health {
        let status = self.state().status();
        match &self.phase {
            ContainerPhase::Running { .. } if self.ready != Some(false) => {
                Health::phrase(status, msg!("state-container-running"))
            }
            ContainerPhase::Running { .. } => {
                Health::phrase(status, msg!("state-container-not-ready"))
            }
            ContainerPhase::Waiting { reason, .. } | ContainerPhase::Terminated { reason, .. } => {
                Health::new(status, reason.clone())
            }
            ContainerPhase::Unknown => Health::phrase(status, msg!("state-container-unknown")),
        }
    }

    /// Name, state and restarts for a status cube.
    pub fn brief(&self) -> ContainerBrief {
        ContainerBrief::new(
            self.name.clone(),
            self.role,
            &self.phase,
            self.ready,
            self.restarts,
        )
    }

    /// `Running`, `Waiting: CrashLoopBackOff`, `Terminated: Completed (exit code 0)`.
    pub fn state_text(&self) -> String {
        self.phase.text()
    }
}

impl ContainerPhase {
    /// `Running`, `Waiting: CrashLoopBackOff`, `Terminated: Completed (exit code 0)`.
    pub fn text(&self) -> String {
        match self {
            ContainerPhase::Running { .. } => "Running".into(),
            ContainerPhase::Waiting { reason, message } => match message {
                Some(message) => format!("Waiting: {reason} ({message})"),
                None => format!("Waiting: {reason}"),
            },
            ContainerPhase::Terminated { reason, exit_code } => {
                format!("Terminated: {reason} (exit code {exit_code})")
            }
            ContainerPhase::Unknown => "Unknown".into(),
        }
    }
}

/// Which related objects a [`SectionBody::Related`] section lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelatedFilter {
    /// Objects matching a label selector (kubectl syntax; every object when
    /// `None`) and, when `owner_uid` is set, owned by that object.
    Selector {
        labels: Option<String>,
        owner_uid: Option<String>,
    },
    /// Exactly these names, in this order; missing ones are listed as not
    /// created yet (StatefulSet claims by ordinal).
    Names(Vec<String>),
    /// Pods that mount the PersistentVolumeClaim with this name
    /// (`spec.volumes[].persistentVolumeClaim.claimName`).
    ClaimUsers(String),
}

/// How related objects are ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelatedOrder {
    Name,
    /// By the trailing `-<n>` of the name (StatefulSet pods).
    Ordinal,
    /// Highest `deployment.kubernetes.io/revision` first.
    Revision,
    /// Newest first.
    Newest,
}

/// Objects of another kind that belong to this one (a Deployment's
/// ReplicaSets and Pods). The plugin only describes them; listing them
/// (from the cache) is up to the view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelatedObjects {
    pub api_version: String,
    pub kind: String,
    /// Namespace to look in; `None` for cluster-scoped kinds.
    pub namespace: Option<String>,
    pub filter: RelatedFilter,
    pub order: RelatedOrder,
}

/// Annotation with a Deployment revision, on Deployments and ReplicaSets.
pub const REVISION_ANNOTATION: &str = "deployment.kubernetes.io/revision";

/// Revision of a Deployment or ReplicaSet, from its annotation.
pub fn revision(obj: &DynamicObject) -> Option<i64> {
    obj.metadata
        .annotations
        .as_ref()?
        .get(REVISION_ANNOTATION)?
        .parse()
        .ok()
}

/// Trailing `-<n>` of a name.
fn ordinal(name: &str) -> Option<u64> {
    name.rsplit_once('-')?.1.parse().ok()
}

impl RelatedObjects {
    /// API group of [`kind`](Self::kind) (`""` for the core group).
    pub fn group(&self) -> &str {
        self.api_version
            .rsplit_once('/')
            .map_or("", |(group, _)| group)
    }

    /// Label selector the watch should use.
    pub fn labels(&self) -> Option<&str> {
        match &self.filter {
            RelatedFilter::Selector { labels, .. } => labels.as_deref(),
            RelatedFilter::Names(_) | RelatedFilter::ClaimUsers(_) => None,
        }
    }

    /// Whether `obj` (of this kind, from the watched namespace and selector)
    /// belongs to the list: owned by the owner, or one of the names.
    pub fn matches(&self, obj: &DynamicObject) -> bool {
        match &self.filter {
            RelatedFilter::Names(names) => {
                let name = obj.metadata.name.as_deref().unwrap_or_default();
                names.iter().any(|n| n == name)
            }
            RelatedFilter::ClaimUsers(claim) => super::builtin::pod_uses_claim(obj, claim),
            RelatedFilter::Selector { owner_uid, .. } => owner_uid.as_deref().is_none_or(|uid| {
                obj.metadata
                    .owner_references
                    .iter()
                    .flatten()
                    .any(|owner| owner.uid == uid)
            }),
        }
    }

    /// Sorts matching objects in this list's order.
    pub fn sort<O: AsRef<DynamicObject>>(&self, objects: &mut [O]) {
        let name = |o: &O| o.as_ref().metadata.name.clone().unwrap_or_default();
        let created = |o: &O| o.as_ref().metadata.creation_timestamp.as_ref().map(|t| t.0);
        match (&self.filter, self.order) {
            (RelatedFilter::Names(names), _) => objects.sort_by_key(|o| {
                let name = name(o);
                names.iter().position(|n| *n == name).unwrap_or(usize::MAX)
            }),
            (_, RelatedOrder::Name) => objects.sort_by_key(name),
            (_, RelatedOrder::Ordinal) => {
                objects.sort_by_key(|o| (ordinal(&name(o)).unwrap_or(u64::MAX), name(o)))
            }
            (_, RelatedOrder::Revision) => objects.sort_by_key(|o| {
                std::cmp::Reverse((revision(o.as_ref()).unwrap_or(-1), created(o)))
            }),
            (_, RelatedOrder::Newest) => objects.sort_by_key(|o| std::cmp::Reverse(created(o))),
        }
    }

    /// Names listed by a [`RelatedFilter::Names`] filter that `found` lacks.
    pub fn missing<'a, O: AsRef<DynamicObject>>(&'a self, found: &[O]) -> Vec<&'a str> {
        let RelatedFilter::Names(names) = &self.filter else {
            return Vec::new();
        };
        names
            .iter()
            .filter(|n| {
                !found
                    .iter()
                    .any(|o| o.as_ref().metadata.name.as_deref() == Some(n.as_str()))
            })
            .map(String::as_str)
            .collect()
    }
}

/// Short state of an object in a related list: its health, a one-line
/// note (`2/2 ready · 3 restarts`) and, for pods, one status per container.
#[derive(Debug, Clone)]
pub struct Brief {
    pub health: Health,
    /// English, for text output (MCP).
    pub note: String,
    /// [`note`](Self::note) as a message the UI translates, when the note
    /// is a text of ours rather than cluster data (a phase).
    pub phrase: Option<Message>,
    pub containers: Vec<ContainerBrief>,
}

impl Brief {
    /// A brief whose note is a text of ours: `note` is its English text.
    pub fn phrased(health: Health, phrase: Message, containers: Vec<ContainerBrief>) -> Self {
        Self {
            health,
            note: phrase.english(),
            phrase: Some(phrase),
            containers,
        }
    }
}

/// The phrase only translates `note`.
impl PartialEq for Brief {
    fn eq(&self, other: &Self) -> bool {
        self.health == other.health
            && self.note == other.note
            && self.containers == other.containers
    }
}

impl Eq for Brief {}

/// One problem a check found, a row of a [`SectionBody::Notes`] section
/// (pod diagnostics, security findings). Texts of ours are [`Message`]s
/// the UI translates; the cluster's message stays as it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    /// `Error`, `High`.
    pub severity: Message,
    pub title: Message,
    /// What happened, in our words.
    pub summary: Option<Message>,
    /// The message of the kubelet, the scheduler or an event; may be empty.
    pub message: String,
    pub hint: Option<Message>,
}

impl Note {
    /// `Error: Pod cannot be scheduled` in `locale`.
    pub fn key_in(&self, locale: Locale) -> String {
        msg!(
            "details-note-title",
            severity = self.severity.clone(),
            title = self.title.clone()
        )
        .translate(locale)
    }

    /// The summary, the cluster's message and the hint, one per line, in
    /// `locale`.
    pub fn value_in(&self, locale: Locale) -> String {
        let summary = self.summary.as_ref().map(|s| s.translate(locale));
        let message = Some(self.message.clone()).filter(|m| !m.is_empty());
        let hint = self
            .hint
            .as_ref()
            .map(|hint| msg!("details-note-hint", hint = hint.clone()).translate(locale));
        [summary, message, hint]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Arguments of notes are names, counts and cluster data, never NaN.
impl Eq for Note {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionBody {
    KeyValues(Vec<(String, String)>),
    /// Problems found by checks (diagnostics, security).
    Notes(Vec<Note>),
    Conditions(Vec<ConditionInfo>),
    /// Rows with links, chips, replica bars and times.
    Fields(Vec<Field>),
    /// `key=value` chips (labels, annotations).
    Labels(Vec<(String, String)>),
    Containers(Vec<ContainerInfo>),
    Related(RelatedObjects),
    /// Rows under column headers (RBAC rules, webhook rules, VPA
    /// recommendations).
    Table(Table),
}

/// A small table of text: column headers and rows of the same width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl Table {
    pub fn new(columns: &[&str], rows: Vec<Vec<String>>) -> Self {
        Self {
            columns: columns.iter().map(|c| (*c).to_owned()).collect(),
            rows,
        }
    }
}

/// Where a view puts a section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Placement {
    /// Joins the main block (name, namespace, created, …) of the details.
    Overview,
    /// Labels, annotations and owners: views with a main block show them
    /// from the object's metadata instead.
    Metadata,
    #[default]
    Section,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailSection {
    pub title: String,
    pub body: SectionBody,
    pub placement: Placement,
    /// Folded until opened (tolerations, affinity).
    pub collapsed: bool,
}

impl DetailSection {
    pub fn new(title: impl Into<String>, body: SectionBody) -> Self {
        Self {
            title: title.into(),
            body,
            placement: Placement::Section,
            collapsed: false,
        }
    }

    pub fn placement(mut self, placement: Placement) -> Self {
        self.placement = placement;
        self
    }

    pub fn collapsed(mut self) -> Self {
        self.collapsed = true;
        self
    }

    /// The section as lines of text, for text output (MCP).
    pub fn text_lines(&self) -> Vec<String> {
        match &self.body {
            SectionBody::KeyValues(pairs) | SectionBody::Labels(pairs) => {
                pairs.iter().map(|(k, v)| format!("{k}: {v}")).collect()
            }
            SectionBody::Notes(notes) => notes
                .iter()
                .map(|n| format!("{}: {}", n.key_in(Locale::En), n.value_in(Locale::En)))
                .collect(),
            SectionBody::Fields(fields) => fields
                .iter()
                .map(|f| format!("{}: {}", f.key, f.value.text()))
                .collect(),
            SectionBody::Conditions(conditions) => conditions
                .iter()
                .map(|c| {
                    format!(
                        "{} = {} {} {}",
                        c.type_,
                        c.status,
                        c.reason.as_deref().unwrap_or_default(),
                        c.message.as_deref().unwrap_or_default()
                    )
                })
                .collect(),
            SectionBody::Containers(containers) => containers
                .iter()
                .map(|c| {
                    format!(
                        "{} ({:?}): {} · {} · {} restarts",
                        c.name,
                        c.role,
                        c.image,
                        c.state_text(),
                        c.restarts
                    )
                })
                .collect(),
            SectionBody::Related(related) => vec![match &related.filter {
                RelatedFilter::Names(names) => format!("{} {}", related.kind, names.join(", ")),
                RelatedFilter::ClaimUsers(claim) => format!("{} using claim {claim}", related.kind),
                RelatedFilter::Selector { labels, .. } => format!(
                    "{} with {}",
                    related.kind,
                    labels.as_deref().unwrap_or("owner reference")
                ),
            }],
            SectionBody::Table(table) => table
                .rows
                .iter()
                .map(|row| {
                    table
                        .columns
                        .iter()
                        .zip(row)
                        .map(|(c, v)| format!("{c}: {v}"))
                        .collect::<Vec<_>>()
                        .join(" · ")
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn named(name: &str, owner: &str, revision: Option<&str>) -> DynamicObject {
        let mut annotations = serde_json::Map::new();
        if let Some(r) = revision {
            annotations.insert(REVISION_ANNOTATION.into(), json!(r));
        }
        serde_json::from_value(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {
                "name": name, "namespace": "ns", "annotations": annotations,
                "ownerReferences": [{"apiVersion": "apps/v1", "kind": "StatefulSet", "name": "db", "uid": owner}]
            }
        }))
        .unwrap()
    }

    fn related(filter: RelatedFilter, order: RelatedOrder) -> RelatedObjects {
        RelatedObjects {
            api_version: "apps/v1".into(),
            kind: "ReplicaSet".into(),
            namespace: Some("ns".into()),
            filter,
            order,
        }
    }

    #[test]
    fn filters_by_owner_and_sorts_by_ordinal() {
        let list = related(
            RelatedFilter::Selector {
                labels: Some("app=db".into()),
                owner_uid: Some("u1".into()),
            },
            RelatedOrder::Ordinal,
        );
        assert_eq!(list.group(), "apps");
        assert_eq!(list.labels(), Some("app=db"));
        let mut objects: Vec<_> = [
            named("db-10", "u1", None),
            named("db-2", "u1", None),
            named("other-0", "u2", None),
        ]
        .into_iter()
        .filter(|o| list.matches(o))
        .map(std::sync::Arc::new)
        .collect();
        list.sort(&mut objects);
        let names: Vec<_> = objects
            .iter()
            .map(|o| o.metadata.name.clone().unwrap())
            .collect();
        assert_eq!(names, ["db-2", "db-10"]);
    }

    #[test]
    fn revisions_sort_newest_first_and_names_report_missing() {
        let list = related(
            RelatedFilter::Selector {
                labels: None,
                owner_uid: None,
            },
            RelatedOrder::Revision,
        );
        let mut objects: Vec<_> = [
            named("a", "u", Some("2")),
            named("b", "u", Some("10")),
            named("c", "u", None),
        ]
        .map(std::sync::Arc::new)
        .to_vec();
        list.sort(&mut objects);
        assert_eq!(objects[0].metadata.name.as_deref(), Some("b"));
        assert_eq!(objects[2].metadata.name.as_deref(), Some("c"));

        let claims = related(
            RelatedFilter::Names(vec!["data-db-0".into(), "data-db-1".into()]),
            RelatedOrder::Name,
        );
        let found = vec![std::sync::Arc::new(named("data-db-1", "u", None))];
        assert!(claims.matches(&found[0]));
        assert!(!claims.matches(&named("data-x-0", "u", None)));
        assert_eq!(claims.missing(&found), ["data-db-0"]);
        assert!(claims.labels().is_none());
    }

    #[test]
    fn container_health_follows_state() {
        let mut c = ContainerInfo {
            name: "app".into(),
            role: ContainerRole::App,
            image: "nginx".into(),
            phase: ContainerPhase::Running { since: None },
            last_state: None,
            ready: Some(true),
            restarts: 0,
            ports: Vec::new(),
            probes: Vec::new(),
            requests: String::new(),
            limits: String::new(),
            env: Vec::new(),
            mounts: Vec::new(),
        };
        assert_eq!(c.health().status, HealthStatus::Healthy);
        assert_eq!(c.state(), ContainerHealth::Ready);
        c.ready = Some(false);
        c.restarts = 2;
        assert_eq!(c.health().status, HealthStatus::Progressing);
        assert_eq!(c.state(), ContainerHealth::NotReady);
        let brief = c.brief();
        assert_eq!(brief.state, "Running, not ready");
        assert_eq!(brief.restarts, 2);
        c.phase = ContainerPhase::Waiting {
            reason: "CrashLoopBackOff".into(),
            message: None,
        };
        assert_eq!(c.health().status, HealthStatus::Failed);
        assert_eq!(c.state(), ContainerHealth::Failing);
        assert_eq!(c.state_text(), "Waiting: CrashLoopBackOff");
        c.phase = ContainerPhase::Waiting {
            reason: "ContainerCreating".into(),
            message: None,
        };
        assert_eq!(c.health().status, HealthStatus::Progressing);
        assert_eq!(c.state(), ContainerHealth::Starting);
        c.phase = ContainerPhase::Terminated {
            reason: "Completed".into(),
            exit_code: 0,
        };
        assert_eq!(c.health().status, HealthStatus::Healthy);
        assert_eq!(c.state(), ContainerHealth::Completed);
        c.phase = ContainerPhase::Terminated {
            reason: "Error".into(),
            exit_code: 1,
        };
        assert_eq!(c.health(), Health::new(HealthStatus::Failed, "Error"));
        assert_eq!(c.state(), ContainerHealth::Failed);
        c.phase = ContainerPhase::Unknown;
        assert_eq!(c.state(), ContainerHealth::Unknown);
    }

    #[test]
    fn not_ready_count_skips_ready_and_completed() {
        let brief = |health| ContainerBrief {
            name: "c".into(),
            role: ContainerRole::App,
            health,
            state: String::new(),
            phase: ContainerPhase::Unknown,
            restarts: 0,
        };
        let containers = [
            brief(ContainerHealth::Ready),
            brief(ContainerHealth::Completed),
            brief(ContainerHealth::NotReady),
            brief(ContainerHealth::Failing),
            brief(ContainerHealth::Unknown),
        ];
        assert_eq!(not_ready_count(&containers), 3);
        assert_eq!(not_ready_count(&[]), 0);
    }

    #[test]
    fn text_lines_flatten_fields() {
        let section = DetailSection::new(
            "Overview",
            SectionBody::Fields(vec![
                Field::text("Strategy", "RollingUpdate"),
                Field::link("Node", "Node", "n1", None),
                Field::new(
                    "Replicas",
                    FieldValue::Replicas(ReplicaCounts {
                        desired: 3,
                        current: 3,
                        ready: 2,
                        updated: Some(3),
                        available: None,
                    }),
                ),
            ]),
        )
        .placement(Placement::Overview);
        assert_eq!(
            section.text_lines(),
            [
                "Strategy: RollingUpdate",
                "Node: n1",
                "Replicas: 3 desired · 3 current · 2 ready · 3 updated"
            ]
        );
        assert!(section.collapsed().collapsed);
    }

    #[test]
    fn default_polarity_follows_problem_words() {
        for negative in [
            "MemoryPressure",
            "ReplicaFailure",
            "SyncFailed",
            "Degraded",
            "NetworkUnavailable",
            "Stalled",
        ] {
            assert!(ConditionInfo::negative_by_default(negative), "{negative}");
        }
        for positive in ["Ready", "Available", "Progressing", "Complete", "Synced"] {
            assert!(!ConditionInfo::negative_by_default(positive), "{positive}");
        }
    }

    #[test]
    fn good_state_depends_on_polarity() {
        let condition = |status: &str, negative: bool| ConditionInfo {
            type_: "X".into(),
            status: status.into(),
            reason: None,
            message: None,
            last_transition: None,
            negative,
            warning: false,
        };
        assert_eq!(condition("True", false).is_good(), Some(true));
        assert_eq!(condition("False", false).is_good(), Some(false));
        assert_eq!(condition("True", true).is_good(), Some(false));
        assert_eq!(condition("False", true).is_good(), Some(true));
        assert_eq!(condition("Unknown", true).is_good(), None);
        assert_eq!(condition("Unknown", false).is_good(), None);
        let warning = |status: &str| ConditionInfo {
            warning: true,
            ..condition(status, false)
        };
        assert_eq!(warning("True").is_good(), Some(false));
        assert_eq!(warning("False").is_good(), Some(true));
        assert_eq!(warning("Unknown").is_good(), None);
    }
}
