//! Built-in resource plugins (see ADR 0003).
//!
//! A plugin describes how one or more GVKs are shown and managed. Plugins return
//! plain **data**; rendering is the job of the UI layer.

pub mod builtin;
pub mod bulk;
mod details;
mod generic;
mod registry;

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use k8s_openapi::jiff::Timestamp;
use kubach_i18n::Message;
use kube::Client;
use kube::api::{DynamicObject, GroupVersionKind};

pub use details::{
    Brief, ConditionInfo, ContainerBrief, ContainerHealth, ContainerInfo, ContainerPhase,
    ContainerRole, DetailSection, Field, FieldValue, Note, Placement, REVISION_ANNOTATION,
    RelatedFilter, RelatedObjects, RelatedOrder, ReplicaCounts, SectionBody, Table,
    not_ready_count, revision,
};
pub use generic::GenericPlugin;
pub use registry::PluginRegistry;

/// Boxed future, keeps [`ResourcePlugin`] object-safe.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("unknown action `{0}`")]
    UnknownAction(String),
    /// An [`ActionKind::Stream`] action: the UI or the MCP agent opens it,
    /// `run_action` does not.
    #[error("action `{0}` is a stream, opened by the caller")]
    Stream(String),
    #[error(transparent)]
    Kube(#[from] kube::Error),
    /// A required action parameter is missing or invalid.
    #[error("invalid parameter `{name}`: {reason}")]
    InvalidParam { name: String, reason: String },
    /// The object cannot be decoded into the expected Kubernetes type.
    #[error("invalid object: {0}")]
    InvalidObject(String),
    /// The API server refused the action for a reason the message explains
    /// in plain words (a PodDisruptionBudget blocking an eviction).
    #[error("{0}")]
    Refused(String),
}

impl From<crate::deletion::DeleteError> for PluginError {
    fn from(error: crate::deletion::DeleteError) -> Self {
        match error {
            crate::deletion::DeleteError::Kube(error) => Self::Kube(error),
            refused => Self::Refused(refused.to_string()),
        }
    }
}

impl From<crate::eviction::EvictError> for PluginError {
    fn from(error: crate::eviction::EvictError) -> Self {
        match error {
            crate::eviction::EvictError::Kube(error) => Self::Kube(error),
            refused => Self::Refused(refused.to_string()),
        }
    }
}

/// Aggregated resource state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    Healthy,
    Progressing,
    Degraded,
    Failed,
    Unknown,
}

/// Status with a human-readable reason (may be empty).
///
/// `message` is English (the MCP agent reads it). When the reason is a text
/// of ours rather than data from the cluster (a condition message, a pod
/// phase), `phrase` keeps it as a [`Message`] the UI translates (ADR 0018).
#[derive(Debug, Clone)]
pub struct Health {
    pub status: HealthStatus,
    pub message: String,
    pub phrase: Option<Message>,
}

impl Health {
    /// A reason from the cluster or made of its data, shown as it is.
    pub fn new(status: HealthStatus, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            phrase: None,
        }
    }

    /// A reason of ours: `message` is its English text.
    pub fn phrase(status: HealthStatus, phrase: Message) -> Self {
        Self {
            status,
            message: phrase.english(),
            phrase: Some(phrase),
        }
    }

    pub fn unknown() -> Self {
        Self::new(HealthStatus::Unknown, "")
    }
}

/// The phrase only translates `message`: equal status and message are the
/// same health.
impl PartialEq for Health {
    fn eq(&self, other: &Self) -> bool {
        self.status == other.status && self.message == other.message
    }
}

impl Eq for Health {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    Text,
    Number,
    Age,
    Status,
    /// [`Cell::Containers`]: a status cube per container.
    Containers,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub id: &'static str,
    pub title: &'static str,
    pub kind: ColumnKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Text(String),
    Number(i64),
    /// A moment in the past (creation, last schedule, start of a running
    /// job). Views show the time elapsed since it and keep it current, so
    /// plugins never bake "now" into a cell.
    Age(Option<Timestamp>),
    /// A count with the moment of the latest one, like restarts: views show
    /// `12 (3h ago)`, kept current like [`Cell::Age`].
    CountAge(i64, Timestamp),
    Status(Health),
    /// One status per container of a pod, init containers first, including
    /// those that have not started yet.
    Containers(Vec<ContainerBrief>),
}

/// Identifier of a plugin action (`delete`, `scale`, `logs`, …). Shared ids
/// live in [`builtin::action`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ActionId(&'static str);

impl ActionId {
    pub const fn new(id: &'static str) -> Self {
        Self(id)
    }

    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for ActionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl PartialEq<str> for ActionId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for ActionId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

/// Interactive streams the UI (dock) or the MCP agent opens itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamKind {
    /// Container logs.
    Logs,
    /// A shell in a container.
    Exec,
    /// A local port forwarded to a pod of the object.
    PortForward,
    /// An ephemeral debug container, attached.
    Debug,
}

/// How an action runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionKind {
    /// Requests to the API server, sent by [`ResourcePlugin::run_action`].
    Api,
    /// A stream the plugin only declares: the caller opens it by its
    /// [`StreamKind`]; `run_action` refuses it.
    Stream(StreamKind),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub id: ActionId,
    pub title: &'static str,
    /// Destructive actions require confirmation in the UI.
    pub dangerous: bool,
    /// Confirmed even on one object: it creates something or cannot be
    /// undone with one click (Trigger now creates a Job), though it is not
    /// destructive. Runs on several objects are always confirmed.
    pub confirm_single: bool,
    pub kind: ActionKind,
}

impl Action {
    /// An action run through the API server.
    pub const fn api(id: ActionId, title: &'static str, dangerous: bool) -> Self {
        Self {
            id,
            title,
            dangerous,
            confirm_single: false,
            kind: ActionKind::Api,
        }
    }

    /// The action, confirmed even on one object ([`Action::confirm_single`]).
    pub const fn confirmed(mut self) -> Self {
        self.confirm_single = true;
        self
    }

    /// A stream opened by the caller, never by [`ResourcePlugin::run_action`].
    pub const fn stream(id: ActionId, title: &'static str, kind: StreamKind) -> Self {
        Self {
            id,
            title,
            dangerous: false,
            confirm_single: false,
            kind: ActionKind::Stream(kind),
        }
    }

    /// The stream this action opens; `None` for API actions.
    pub fn stream_kind(&self) -> Option<StreamKind> {
        match self.kind {
            ActionKind::Stream(kind) => Some(kind),
            ActionKind::Api => None,
        }
    }
}

/// Values for action parameters, keyed by [`ActionParam::id`].
pub type ActionParams = BTreeMap<String, serde_json::Value>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionParamKind {
    /// An integer (passed as a JSON number) within the given bounds.
    Integer { min: i64, max: Option<i64> },
}

/// Input an action needs before it can run (e.g. the replica count for scale).
#[derive(Debug, Clone, PartialEq)]
pub struct ActionParam {
    pub id: &'static str,
    pub title: &'static str,
    pub kind: ActionParamKind,
    /// Suggested initial value, e.g. the current replica count.
    pub default: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationKind {
    /// The target owns the current object (ownerReferences).
    OwnedBy,
    /// The current object selects the target (label selectors).
    Selects,
    /// The current object references the target (PVC → PV, …).
    References,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectRef {
    pub api_version: String,
    pub kind: String,
    pub name: String,
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relation {
    pub kind: RelationKind,
    pub target: ObjectRef,
}

/// Objects matched by a label selector rather than by name (Service → Pods).
/// Describes the relation only; listing the targets is up to the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectorRelation {
    pub kind: RelationKind,
    pub api_version: String,
    pub target_kind: String,
    /// Namespace to search in; `None` for cluster-scoped targets.
    pub namespace: Option<String>,
    /// Selector in kubectl syntax (`app=web,tier in (a,b)`), usable with
    /// `ListParams::labels`.
    pub label_selector: String,
}

/// Support for specific GVKs. Implementations are built in and registered in
/// [`PluginRegistry`].
pub trait ResourcePlugin: Send + Sync {
    fn name(&self) -> &'static str;

    /// GVKs handled by this plugin. Matching ignores the version.
    fn supported(&self) -> Vec<GroupVersionKind>;

    fn columns(&self) -> Vec<Column>;

    /// Cells in the same order as [`columns`](Self::columns).
    fn cells(&self, obj: &DynamicObject) -> Vec<Cell>;

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection>;

    fn actions(&self) -> Vec<Action> {
        Vec::new()
    }

    /// Actions applicable to this object (e.g. "Resume" only when suspended).
    /// Defaults to [`actions`](Self::actions).
    fn actions_for(&self, _obj: &DynamicObject) -> Vec<Action> {
        self.actions()
    }

    /// Parameters the UI must collect before running `action_id`.
    fn action_params(&self, _obj: &DynamicObject, _action_id: &ActionId) -> Vec<ActionParam> {
        Vec::new()
    }

    /// Runs an [`ActionKind::Api`] action with its parameters (see
    /// [`action_params`](Self::action_params); empty for actions without
    /// any). The UI, bulk runs and the MCP agent all go through it.
    /// [`ActionKind::Stream`] actions are refused with [`PluginError::Stream`]
    /// before anything is sent.
    fn run_action<'a>(
        &'a self,
        _client: &'a Client,
        _obj: &'a DynamicObject,
        action_id: &'a ActionId,
        _params: &'a ActionParams,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        let error = builtin::unknown(action_id);
        Box::pin(async move { Err(error) })
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation>;

    /// Relations expressed by label selectors (Deployment → Pods, Service → Pods).
    fn selector_relations(&self, _obj: &DynamicObject) -> Vec<SelectorRelation> {
        Vec::new()
    }

    fn health(&self, obj: &DynamicObject) -> Health;

    /// One-line state for lists of related objects. Defaults to the health
    /// and, when the object has one, its `status.phase` (`Bound`), which
    /// also stands in for an unknown health.
    fn brief(&self, obj: &DynamicObject) -> Brief {
        let phase = obj
            .data
            .pointer("/status/phase")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let mut health = self.health(obj);
        if health.status == HealthStatus::Unknown {
            let status = match phase.as_str() {
                "Bound" | "Active" | "Available" | "Running" | "Succeeded" => HealthStatus::Healthy,
                "Pending" | "Released" => HealthStatus::Progressing,
                "Lost" | "Failed" => HealthStatus::Failed,
                _ => HealthStatus::Unknown,
            };
            health = Health::new(status, phase.clone());
        }
        Brief {
            health,
            note: phase,
            phrase: None,
            containers: Vec::new(),
        }
    }
}

/// Formats an age or duration in seconds like kubectl's `HumanDuration`
/// (`45s`, `3m12s`, `25m`, `2h5m`, `3d4h`, `3y10d`); negative is `0s`.
///
/// For text output (MCP, plugin cells). The GUI formats ages at render time
/// with `kubach_ui::format_age`, the same rules: `kubach-ui` and
/// `kubach-core` do not depend on each other. The rules must stay the same;
/// both crates check them against the same `AGE_CASES` table.
pub fn format_age(secs: i64) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const YEAR: i64 = 365 * DAY;
    let s = secs.max(0);
    let pair = |major: i64, unit: &str, minor: i64, minor_unit: &str| {
        if minor == 0 {
            format!("{major}{unit}")
        } else {
            format!("{major}{unit}{minor}{minor_unit}")
        }
    };
    match s {
        s if s < 2 * MINUTE => format!("{s}s"),
        s if s < 10 * MINUTE => pair(s / MINUTE, "m", s % MINUTE, "s"),
        s if s < 3 * HOUR => format!("{}m", s / MINUTE),
        s if s < 8 * HOUR => pair(s / HOUR, "h", s % HOUR / MINUTE, "m"),
        s if s < 2 * DAY => format!("{}h", s / HOUR),
        s if s < 8 * DAY => pair(s / DAY, "d", s % DAY / HOUR, "h"),
        s if s < 2 * YEAR => format!("{}d", s / DAY),
        s if s < 8 * YEAR => pair(s / YEAR, "y", s % YEAR / DAY, "d"),
        s => format!("{}y", s / YEAR),
    }
}

#[cfg(test)]
mod format_age_tests {
    use super::format_age;

    /// Shared with the other copy of `format_age` (`kubach-core/src/plugin/mod.rs`
    /// and `kubach-ui/src/age.rs`): keep both tables equal.
    const AGE_CASES: [(i64, &str); 16] = [
        (0, "0s"),
        (45, "45s"),
        (119, "119s"),
        (125, "2m5s"),
        (180, "3m"),
        (192, "3m12s"),
        (25 * 60, "25m"),
        (3 * 3600 + 5 * 60, "3h5m"),
        (5 * 3600, "5h"),
        (20 * 3600 + 59 * 60, "20h"),
        (3 * 86400 + 4 * 3600, "3d4h"),
        (5 * 86400, "5d"),
        (12 * 86400, "12d"),
        (3 * 365 * 86400 + 10 * 86400, "3y10d"),
        (9 * 365 * 86400, "9y"),
        (2 * 365 * 86400, "2y"),
    ];

    #[test]
    fn matches_the_shared_age_table() {
        for (seconds, expected) in AGE_CASES {
            assert_eq!(format_age(seconds), expected, "{seconds}s");
        }
        assert_eq!(format_age(-1), "0s");
    }
}
