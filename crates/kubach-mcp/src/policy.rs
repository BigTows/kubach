//! Client-side policy: checked before every API call, in every session mode
//! (ADR 0005, "Клиентская политика — всегда и первой").
//!
//! The policy is pure: it sees the request, the session scope and the
//! cluster's kind catalog, never the network. A request that passes comes out
//! as an [`Allowed`] plan with the namespace, kind and manifests to use; the
//! tool layer only ever acts on that plan.
//!
//! Minimal access (#15): the context's settings give the agent an
//! [`Access`] it has without asking, the user may raise it from the session
//! ([`DenialCode::NeedsAccess`]), and `kubach mcp --profile` may cap it.
//! Within the access, the session's [`Grants`] are the per-namespace
//! mechanism: `read(ns)` and `modify(ns)` are granted by the tool layer
//! without a question (they set up the namespace's identity), revealing one
//! Secret is asked for that Secret ([`DenialCode::NeedsGrant`]).
//!
//! Namespaces (#401): a session pinned at launch keeps to its namespaces;
//! otherwise the agent names any valid namespace, as many as it needs.
//! Cluster-scoped resources and all-namespace calls stay refused; only
//! [`Request::ListNamespaces`] reads namespace names.

use std::collections::BTreeMap;
use std::fmt;

use kubach_core::apply::{self, ResolvedManifest};
use kubach_core::discovery::{ApiCatalog, ResourceKind};
use kubach_core::secrets::mask::is_core_secret;
use kubach_core::workload_pods::Workload;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::access::{Access, Raises};
use crate::grants::{Grant, GrantState, Grants};
use crate::names;
use crate::rules::{is_readable, is_writable};
use crate::scope::{SessionMode, SessionScope};
use crate::secret_write::{self, CREATE_TYPES, MAX_KEYS, SecretOp};
use crate::settings::Profile;

/// Largest manifest bundle accepted by `apply`.
pub const MAX_APPLY_BYTES: usize = 512 * 1024;
/// Most documents in one `apply`.
pub const MAX_APPLY_DOCUMENTS: usize = 20;
/// Most namespaces in one topology request: each may need its own read
/// right, and a call asks for at most three.
pub const MAX_TOPOLOGY_NAMESPACES: usize = 3;
/// Most arguments of an exec command.
pub const MAX_EXEC_ARGS: usize = 64;
/// Longest exec argument, in bytes.
pub const MAX_EXEC_ARG_BYTES: usize = 2048;
/// Longest exec command, all arguments together, in bytes. Keeps the
/// confirmation dialog readable in full (#194).
pub const MAX_EXEC_COMMAND_BYTES: usize = 4096;
/// Longest image reference of a debug container (#64).
pub const MAX_IMAGE_REF_BYTES: usize = 512;
/// Lowest local port an agent's port-forward may take (#17): well-known
/// ports stay with the system and the user.
pub const MIN_FORWARD_LOCAL_PORT: u16 = 1024;

/// The remote port of a port-forward as the agent names it: a number, or
/// a port name (`http`) looked up on the target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ForwardPortRef {
    Number(u16),
    Name(String),
}

impl fmt::Display for ForwardPortRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(port) => write!(f, "{port}"),
            Self::Name(name) => f.write_str(name),
        }
    }
}

/// A kind as the agent names it: `Deployment`, `deployments`, optionally
/// with `apiVersion` to disambiguate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KindRef {
    pub kind: String,
    pub api_version: Option<String>,
}

impl KindRef {
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            api_version: None,
        }
    }
}

/// A tool call as the policy sees it.
#[derive(Clone, Debug)]
pub enum Request {
    /// Session information; always allowed while the session is alive.
    Info,
    ListKinds,
    /// Names of the namespaces the agent may ask for (#401); no right
    /// needed, nothing but names.
    ListNamespaces,
    List {
        kind: KindRef,
        namespace: Option<String>,
        all_namespaces: bool,
        label_selector: Option<String>,
    },
    /// `get` and `describe`.
    Get {
        kind: KindRef,
        name: String,
        namespace: Option<String>,
    },
    Events {
        namespace: Option<String>,
        object: Option<(KindRef, String)>,
    },
    /// `get_logs`: exactly one of a pod, a workload (kind + name) or a
    /// label selector picks the pods.
    Logs {
        pod: Option<String>,
        workload: Option<(KindRef, String)>,
        label_selector: Option<String>,
        container: Option<String>,
        namespace: Option<String>,
    },
    Top {
        namespace: Option<String>,
    },
    /// The topology graph of namespaces (`get_topology`) and NetworkPolicy
    /// verdicts between workloads in them (`can_reach`): read-only, built
    /// from lists of the readable kinds. Empty: the session's namespace.
    Topology {
        namespaces: Vec<String>,
    },
    Apply {
        yaml: String,
        namespace: Option<String>,
        dry_run: bool,
    },
    Scale {
        kind: KindRef,
        name: String,
        namespace: Option<String>,
        replicas: i64,
    },
    Restart {
        kind: KindRef,
        name: String,
        namespace: Option<String>,
    },
    Delete {
        kind: KindRef,
        name: String,
        namespace: Option<String>,
    },
    Exec {
        pod: String,
        container: Option<String>,
        namespace: Option<String>,
        command: Vec<String>,
    },
    /// `debug_container` (#64): an ephemeral container added to `pod`, in
    /// the process namespace of `target` when given, then `command` run in
    /// it like exec (none: it is left running for exec). Confirmed every
    /// time: the pod keeps the container until it is deleted.
    Debug {
        pod: String,
        target: Option<String>,
        name: Option<String>,
        image: Option<String>,
        namespace: Option<String>,
        command: Vec<String>,
    },
    /// `port_forward_start` (#17): a local port on `127.0.0.1` to a pod,
    /// or to a ready pod of a Service or Deployment. `target` is
    /// `pod/<name>`, `service/<name>` or `deployment/<name>`.
    PortForward {
        namespace: Option<String>,
        target: String,
        port: ForwardPortRef,
        local_port: Option<u16>,
    },
    /// Secret names, types, keys, sizes and who uses them; no values.
    SecretList {
        namespace: Option<String>,
        name: Option<String>,
    },
    /// Checks of a Secret's values inside Kubach; only conclusions leave.
    SecretCheck {
        namespace: Option<String>,
        name: String,
        key: Option<String>,
    },
    /// Whether two values in the cluster are equal; only yes or no leaves.
    SecretCompare {
        namespace: Option<String>,
        name: String,
        key: String,
        other_namespace: Option<String>,
        other_name: String,
        other_key: String,
    },
    /// The values of one Secret (one key, or all), after the user confirmed
    /// that very Secret.
    SecretReveal {
        namespace: Option<String>,
        name: String,
        key: Option<String>,
    },
    /// Create or update a Secret with values the user enters in Kubach;
    /// the agent names keys, never values (#230).
    SecretWrite {
        namespace: Option<String>,
        name: String,
        mode: SecretOp,
        secret_type: Option<String>,
        keys: Vec<String>,
        remove: Vec<String>,
        explanation: String,
    },
    /// Copy a Secret inside Kubach (#230).
    SecretCopy {
        namespace: Option<String>,
        name: String,
        to_namespace: Option<String>,
        to_name: Option<String>,
        explanation: String,
    },
    /// Delete a Secret (#230).
    SecretDelete {
        namespace: Option<String>,
        name: String,
        explanation: String,
    },
    /// Helm charts of the repositories added in Kubach or of an `oci://`
    /// reference (#418): search and default values. No namespace.
    HelmCharts,
    /// Helm releases of a namespace: list, history and values. Needs
    /// access to Secrets, where Helm stores them (#53).
    HelmRead {
        namespace: Option<String>,
    },
    /// `helm install`, `upgrade`, `rollback` or `uninstall` of `release`.
    HelmChange {
        namespace: Option<String>,
        release: String,
    },
}

impl Request {
    /// Changes the cluster or runs code in it.
    pub fn is_mutating(&self) -> bool {
        match self {
            Self::Apply { dry_run, .. } => !dry_run,
            Self::Scale { .. }
            | Self::Restart { .. }
            | Self::Delete { .. }
            | Self::Exec { .. }
            | Self::Debug { .. }
            | Self::SecretWrite { .. }
            | Self::SecretCopy { .. }
            | Self::SecretDelete { .. }
            | Self::HelmChange { .. } => true,
            _ => false,
        }
    }
}

/// Why a request was denied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DenialCode {
    SessionEnded,
    Namespace,
    ClusterScoped,
    Secret,
    ForbiddenKind,
    UnknownKind,
    Profile,
    Settings,
    InvalidName,
    InvalidInput,
    Manifest,
    /// A right in a namespace nobody answered for yet: [`Denial::grant`]
    /// names it. `read(ns)` and `modify(ns)` within the access are granted
    /// without a question; revealing a Secret is asked. The tool layer acts
    /// and checks again.
    NeedsGrant,
    /// The user refused (or revoked) a right this request needs.
    GrantDenied,
    /// Secret values are hidden on this context; the user is told.
    RevealDisabled,
    /// Beyond the session's access (#15), but the user may allow it:
    /// [`Denial::access`] names what is missing. The tool layer asks and
    /// checks again.
    NeedsAccess,
    /// The user refused this access in this session.
    AccessRefused,
    /// The user's own RBAC does not allow it in this namespace; more access
    /// would not help, so nobody is asked.
    Rbac,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Denial {
    pub code: DenialCode,
    pub message: String,
    /// The right to ask the user for ([`DenialCode::NeedsGrant`]) or the one
    /// refused ([`DenialCode::GrantDenied`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant: Option<Grant>,
    /// What the access lacks ([`DenialCode::NeedsAccess`],
    /// [`DenialCode::AccessRefused`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<Access>,
}

impl Denial {
    pub fn new(code: DenialCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            grant: None,
            access: None,
        }
    }

    fn needs(grant: Grant) -> Self {
        Self {
            code: DenialCode::NeedsGrant,
            message: format!("this session needs the right to {grant}"),
            grant: Some(grant),
            access: None,
        }
    }

    fn refused(grant: Grant) -> Self {
        Self {
            code: DenialCode::GrantDenied,
            message: format!(
                "the user did not allow this session to {grant}; Kubach does not ask again in \
                 this session"
            ),
            grant: Some(grant),
            access: None,
        }
    }
}

impl fmt::Display for Denial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "denied by policy ({:?}): {}", self.code, self.message)
    }
}

impl std::error::Error for Denial {}

/// A field that lets a workload escape the namespace boundary in practice
/// (host access, another ServiceAccount's rights, a Secret's values, a
/// GitOps controller's rights). Requires an explicit confirmation of its
/// own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Which document of an apply, if any.
    pub document: Option<usize>,
    /// `.spec.template.spec.volumes[0].hostPath` style.
    pub path: String,
    pub message: String,
}

/// Whether the user has to approve the call in the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Confirmation {
    NotNeeded,
    Required,
}

/// A request that passed the policy, with everything resolved.
#[derive(Clone, Debug)]
pub struct Allowed {
    /// The namespace to act in; always one of the session's.
    pub namespace: String,
    pub kind: Option<ResourceKind>,
    /// Validated object name, if the request has one.
    pub name: Option<String>,
    /// For `apply`: documents bound to kinds, `metadata.namespace` forced.
    pub manifests: Vec<ResolvedManifest>,
    pub findings: Vec<Finding>,
    pub confirmation: Confirmation,
    /// The Secret key a request names, validated.
    pub key: Option<String>,
    /// The second value of a [`Request::SecretCompare`].
    pub other: Option<SecretKeyRef>,
    /// The target of a [`Request::SecretCopy`].
    pub to: Option<ObjectRef>,
    /// Every namespace of a [`Request::Topology`], `namespace` first.
    pub namespaces: Vec<String>,
}

/// A named object in a session namespace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectRef {
    pub namespace: String,
    pub name: String,
}

/// A key of a Secret in a session namespace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretKeyRef {
    pub namespace: String,
    pub name: String,
    pub key: String,
}

/// The mode of each namespace of a session (#401): who holds the boundary
/// there. Namespaces without an identity yet count as `default`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamespaceModes {
    pub default: SessionMode,
    pub by_namespace: BTreeMap<String, SessionMode>,
}

impl NamespaceModes {
    pub fn of(&self, namespace: &str) -> SessionMode {
        self.by_namespace
            .get(namespace)
            .copied()
            .unwrap_or(self.default)
    }
}

/// The same mode everywhere.
impl From<SessionMode> for NamespaceModes {
    fn from(default: SessionMode) -> Self {
        Self {
            default,
            by_namespace: BTreeMap::new(),
        }
    }
}

/// The policy of one session.
pub struct Policy<'a> {
    scope: &'a SessionScope,
    catalog: &'a ApiCatalog,
    closed: bool,
    grants: Grants,
    modes: NamespaceModes,
    /// The user's answers about access (#15).
    raises: Raises,
}

impl<'a> Policy<'a> {
    /// `grants` are the session's rights so far; the mode of a namespace
    /// decides whether a change needs `modify(ns)` (only where the API
    /// server holds the boundary).
    pub fn new(
        scope: &'a SessionScope,
        catalog: &'a ApiCatalog,
        closed: bool,
        grants: Grants,
        modes: impl Into<NamespaceModes>,
    ) -> Self {
        Self {
            scope,
            catalog,
            closed,
            grants,
            modes: modes.into(),
            raises: Raises::default(),
        }
    }

    /// With the session's answers about access (#15).
    pub fn with_raises(mut self, raises: Raises) -> Self {
        self.raises = raises;
        self
    }

    /// What the session has without asking: the context's minimum, capped
    /// by the launch's profile.
    pub fn minimum(&self) -> Access {
        self.scope.policy.minimum.capped(self.scope.profile)
    }

    /// The access now: the minimum, raised by what the user allowed, capped
    /// by the launch's profile.
    pub fn access(&self) -> Access {
        self.scope
            .policy
            .minimum
            .join(self.raises.granted)
            .capped(self.scope.profile)
    }

    /// What `request` needs of the access, where the user may raise it.
    /// Custom resources are found by kind, in [`Policy::read_kind`].
    pub fn needed_access(request: &Request) -> Option<Access> {
        let need = |profile, secrets| {
            Some(Access {
                profile,
                secrets,
                crds: false,
            })
        };
        match request {
            Request::Logs { .. }
            | Request::Exec { .. }
            | Request::Debug { .. }
            | Request::PortForward { .. } => need(Profile::Debug, false),
            Request::Apply { .. }
            | Request::Scale { .. }
            | Request::Restart { .. }
            | Request::Delete { .. }
            | Request::HelmChange { .. } => need(Profile::Deploy, false),
            Request::SecretWrite { .. }
            | Request::SecretCopy { .. }
            | Request::SecretDelete { .. } => need(Profile::Deploy, true),
            // Helm keeps releases in Secrets of their namespace (#53).
            Request::SecretList { .. }
            | Request::SecretCheck { .. }
            | Request::SecretCompare { .. }
            | Request::SecretReveal { .. }
            | Request::HelmRead { .. } => need(Profile::ReadOnly, true),
            _ => None,
        }
    }

    /// Whether `needed` lies within the access, or else why not: beyond
    /// the launch's cap (never asked), refused already, or to be asked
    /// ([`DenialCode::NeedsAccess`]).
    pub fn within_access(&self, needed: Access, what: &str) -> Result<(), Denial> {
        let have = self.access();
        let Some(missing) = have.missing(needed) else {
            return Ok(());
        };
        let context = &self.scope.context;
        if needed.profile > self.scope.profile {
            return Err(Denial::new(
                DenialCode::Profile,
                format!(
                    "{what} needs the {} profile, and this session was started with --profile {}; \
                     it never gets more. Start kubach mcp with a broader --profile or without it",
                    needed.profile, self.scope.profile
                ),
            ));
        }
        let (code, message) = if self.raises.is_refused(missing) {
            (
                DenialCode::AccessRefused,
                format!(
                    "{what} needs {missing}; the user did not allow it on {context} in this \
                     session and is not asked again"
                ),
            )
        } else {
            (
                DenialCode::NeedsAccess,
                format!(
                    "{what} needs {missing}, beyond this session's access ({have}); the user has \
                     to allow it in Kubach"
                ),
            )
        };
        Err(Denial {
            code,
            message,
            grant: None,
            access: Some(missing),
        })
    }

    fn server_enforced(&self, namespace: &str) -> bool {
        self.modes.of(namespace).server_enforced()
    }

    /// Checks `request`. Order: session alive, profile and settings, then
    /// namespace, kind and names, then the rights granted on request.
    pub fn check(&self, request: &Request) -> Result<Allowed, Denial> {
        self.alive()?;
        self.profile_allows(request)?;
        let allowed = self.check_request(request)?;
        self.granted(request, &allowed)?;
        Ok(allowed)
    }

    /// The rights `request` needs, in ladder order: reading its namespace
    /// first, then revealing the Secret or changing the namespace.
    pub fn required_grants(&self, request: &Request, allowed: &Allowed) -> Vec<Grant> {
        let read = Grant::read(allowed.namespace.clone());
        let mut needed = match request {
            Request::Info | Request::ListKinds | Request::ListNamespaces | Request::HelmCharts => {
                Vec::new()
            }
            Request::HelmRead { .. } => vec![read],
            Request::List { .. }
            | Request::Get { .. }
            | Request::Events { .. }
            | Request::Logs { .. }
            | Request::Top { .. }
            | Request::SecretList { .. }
            | Request::SecretCheck { .. } => vec![read],
            // Reading the namespace; the Role gets `pods/portforward` only
            // while a forward runs, and every forward is confirmed (#17).
            Request::PortForward { .. } => vec![read],
            Request::Topology { .. } => allowed
                .namespaces
                .iter()
                .map(|namespace| Grant::read(namespace.clone()))
                .collect(),
            Request::SecretCompare { .. } => {
                let mut needed = vec![read];
                if let Some(other) = &allowed.other {
                    needed.push(Grant::read(other.namespace.clone()));
                }
                needed
            }
            Request::SecretReveal { .. } => vec![
                read,
                Grant::secret(
                    allowed.namespace.clone(),
                    allowed.name.clone().unwrap_or_default(),
                ),
            ],
            Request::SecretCopy { .. } => {
                let target = allowed
                    .to
                    .as_ref()
                    .map_or(allowed.namespace.clone(), |to| to.namespace.clone());
                // Reading A, then changing B (and reading it first, as
                // the ladder goes).
                vec![read, Grant::read(target.clone()), self.change_right(target)]
            }
            Request::Apply { .. }
            | Request::Scale { .. }
            | Request::Restart { .. }
            | Request::Delete { .. }
            | Request::Exec { .. }
            | Request::Debug { .. }
            | Request::SecretWrite { .. }
            | Request::SecretDelete { .. }
            | Request::HelmChange { .. } => {
                if self.server_enforced(&allowed.namespace) {
                    vec![read, Grant::modify(allowed.namespace.clone())]
                } else {
                    // Client only: nothing to remember, every change is
                    // confirmed on its own (ADR 0005).
                    vec![read]
                }
            }
        };
        needed.dedup();
        needed
    }

    /// The right a change in `namespace` needs: `modify(ns)` when the API
    /// server enforces the boundary, else reading it (client only: every
    /// change is confirmed on its own).
    fn change_right(&self, namespace: String) -> Grant {
        if self.server_enforced(&namespace) {
            Grant::modify(namespace)
        } else {
            Grant::read(namespace)
        }
    }

    fn granted(&self, request: &Request, allowed: &Allowed) -> Result<(), Denial> {
        for grant in self.required_grants(request, allowed) {
            match self.grants.state(&grant) {
                Some(GrantState::Granted) => {}
                Some(GrantState::Denied) => return Err(Denial::refused(grant)),
                None => return Err(Denial::needs(grant)),
            }
        }
        // Every change stays confirmed one by one, scaling and restarts
        // included: no per-namespace "ok" spares them since #15.
        Ok(())
    }

    fn check_request(&self, request: &Request) -> Result<Allowed, Denial> {
        match request {
            // Not about one namespace: the session's own if it has one.
            Request::Info | Request::ListKinds | Request::ListNamespaces | Request::HelmCharts => {
                Ok(self.allowed(self.namespace(None).unwrap_or_default()))
            }
            Request::HelmRead { namespace } => {
                Ok(self.allowed(self.namespace(namespace.as_deref())?))
            }
            Request::HelmChange { namespace, release } => {
                let mut allowed = self.allowed(self.namespace(namespace.as_deref())?);
                kubach_core::helm::cli::check_release_name(release)
                    .map_err(|error| Denial::new(DenialCode::InvalidName, error.to_string()))?;
                allowed.name = Some(release.clone());
                allowed.confirmation = Confirmation::Required;
                Ok(allowed)
            }
            Request::List {
                kind,
                namespace,
                all_namespaces,
                label_selector,
            } => {
                if *all_namespaces {
                    return Err(Denial::new(
                        DenialCode::Namespace,
                        "listing across all namespaces is not allowed in this session",
                    ));
                }
                if let Some(selector) = label_selector {
                    check_selector(selector)?;
                }
                let mut allowed = self.allowed(self.namespace(namespace.as_deref())?);
                allowed.kind = Some(self.read_kind(kind)?);
                Ok(allowed)
            }
            Request::Get {
                kind,
                name,
                namespace,
            } => {
                let mut allowed = self.allowed(self.namespace(namespace.as_deref())?);
                allowed.kind = Some(self.read_kind(kind)?);
                allowed.name = Some(object_name(name)?);
                Ok(allowed)
            }
            Request::Events { namespace, object } => {
                let mut allowed = self.allowed(self.namespace(namespace.as_deref())?);
                if let Some((kind, name)) = object {
                    allowed.kind = Some(self.read_kind(kind)?);
                    allowed.name = Some(object_name(name)?);
                }
                Ok(allowed)
            }
            Request::Logs {
                pod,
                workload,
                label_selector,
                container,
                namespace,
            } => {
                let mut allowed = self.allowed(self.namespace(namespace.as_deref())?);
                match (pod, workload, label_selector) {
                    (Some(pod), None, None) => allowed.name = Some(object_name(pod)?),
                    (None, Some((kind, name)), None) => {
                        let kind = self.read_kind(kind)?;
                        if Workload::from_api(&kind.api.group, &kind.api.kind).is_none() {
                            return Err(Denial::new(
                                DenialCode::InvalidInput,
                                format!(
                                    "logs by kind take a Deployment, StatefulSet, DaemonSet, \
                                     ReplicaSet, Job or CronJob, not {}",
                                    kind.api.kind
                                ),
                            ));
                        }
                        allowed.kind = Some(kind);
                        allowed.name = Some(object_name(name)?);
                    }
                    (None, None, Some(selector)) => {
                        if selector.trim().is_empty() {
                            return Err(Denial::new(
                                DenialCode::InvalidInput,
                                "labelSelector must not be empty",
                            ));
                        }
                        check_selector(selector)?;
                    }
                    _ => {
                        return Err(Denial::new(
                            DenialCode::InvalidInput,
                            "give exactly one of pod, kind + name, or labelSelector",
                        ));
                    }
                }
                if let Some(container) = container {
                    container_name(container)?;
                }
                Ok(allowed)
            }
            Request::Top { namespace } => Ok(self.allowed(self.namespace(namespace.as_deref())?)),
            Request::Topology { namespaces } => {
                if namespaces.len() > MAX_TOPOLOGY_NAMESPACES {
                    return Err(Denial::new(
                        DenialCode::InvalidInput,
                        format!("at most {MAX_TOPOLOGY_NAMESPACES} namespaces per call"),
                    ));
                }
                let mut resolved: Vec<String> = Vec::new();
                if namespaces.is_empty() {
                    resolved.push(self.namespace(None)?);
                }
                for namespace in namespaces {
                    let namespace = self.namespace(Some(namespace))?;
                    if !resolved.contains(&namespace) {
                        resolved.push(namespace);
                    }
                }
                let mut allowed = self.allowed(resolved[0].clone());
                allowed.namespaces = resolved;
                Ok(allowed)
            }
            Request::Apply {
                yaml,
                namespace,
                dry_run,
            } => self.check_apply(yaml, namespace.as_deref(), *dry_run),
            Request::Scale {
                kind,
                name,
                namespace,
                replicas,
            } => {
                if !(0..=1000).contains(replicas) {
                    return Err(Denial::new(
                        DenialCode::InvalidInput,
                        "replicas must be between 0 and 1000",
                    ));
                }
                self.check_action(kind, name, namespace.as_deref())
            }
            Request::Restart {
                kind,
                name,
                namespace,
            }
            | Request::Delete {
                kind,
                name,
                namespace,
            } => self.check_action(kind, name, namespace.as_deref()),
            Request::Exec {
                pod,
                container,
                namespace,
                command,
            } => {
                let mut allowed = self.allowed(self.namespace(namespace.as_deref())?);
                allowed.name = Some(object_name(pod)?);
                if let Some(container) = container {
                    container_name(container)?;
                }
                check_command(command)?;
                allowed.confirmation = Confirmation::Required;
                Ok(allowed)
            }
            Request::Debug {
                pod,
                target,
                name,
                image,
                namespace,
                command,
            } => {
                let mut allowed = self.allowed(self.namespace(namespace.as_deref())?);
                allowed.name = Some(object_name(pod)?);
                for container in [target, name].into_iter().flatten() {
                    container_name(container)?;
                }
                if let Some(image) = image {
                    check_image(image)?;
                }
                if !command.is_empty() {
                    check_command(command)?;
                }
                allowed.confirmation = Confirmation::Required;
                Ok(allowed)
            }
            Request::PortForward {
                namespace,
                target,
                port,
                local_port,
            } => {
                let mut allowed = self.allowed(self.namespace(namespace.as_deref())?);
                let (kind, name) = forward_target(target)?;
                allowed.kind = Some(self.read_kind(&KindRef::new(kind))?);
                allowed.name = Some(object_name(name)?);
                check_forward_port(port)?;
                if let Some(local) = local_port
                    && *local < MIN_FORWARD_LOCAL_PORT
                {
                    return Err(Denial::new(
                        DenialCode::InvalidInput,
                        format!(
                            "localPort must be {MIN_FORWARD_LOCAL_PORT} to 65535; leave it out \
                             for any free port"
                        ),
                    ));
                }
                allowed.confirmation = Confirmation::Required;
                Ok(allowed)
            }
            Request::SecretList { namespace, name } => {
                let mut allowed = self.allowed(self.namespace(namespace.as_deref())?);
                if let Some(name) = name {
                    allowed.name = Some(object_name(name)?);
                }
                Ok(allowed)
            }
            Request::SecretCheck {
                namespace,
                name,
                key,
            }
            | Request::SecretReveal {
                namespace,
                name,
                key,
            } => {
                let mut allowed = self.allowed(self.namespace(namespace.as_deref())?);
                allowed.name = Some(object_name(name)?);
                if let Some(key) = key {
                    allowed.key = Some(secret_key(key)?);
                }
                Ok(allowed)
            }
            Request::SecretCompare {
                namespace,
                name,
                key,
                other_namespace,
                other_name,
                other_key,
            } => {
                let mut allowed = self.allowed(self.namespace(namespace.as_deref())?);
                allowed.name = Some(object_name(name)?);
                allowed.key = Some(secret_key(key)?);
                let other_namespace = match other_namespace {
                    Some(other) => self.namespace(Some(other))?,
                    None => allowed.namespace.clone(),
                };
                allowed.other = Some(SecretKeyRef {
                    namespace: other_namespace,
                    name: object_name(other_name)?,
                    key: secret_key(other_key)?,
                });
                Ok(allowed)
            }
            Request::SecretWrite {
                namespace,
                name,
                mode,
                secret_type,
                keys,
                remove,
                explanation,
            } => {
                let mut allowed = self.allowed(self.namespace(namespace.as_deref())?);
                allowed.name = Some(object_name(name)?);
                match mode {
                    SecretOp::Create => {
                        if let Some(secret_type) = secret_type
                            && !CREATE_TYPES.contains(&secret_type.as_str())
                        {
                            return Err(Denial::new(
                                DenialCode::InvalidInput,
                                format!(
                                    "type must be one of {}; Kubach has forms for these only",
                                    CREATE_TYPES.join(", ")
                                ),
                            ));
                        }
                        if !remove.is_empty() {
                            return Err(Denial::new(
                                DenialCode::InvalidInput,
                                "removeKeys is for mode \"update\"",
                            ));
                        }
                    }
                    SecretOp::Update => {
                        if secret_type.is_some() {
                            return Err(Denial::new(
                                DenialCode::InvalidInput,
                                "type is taken from the existing Secret in mode \"update\"",
                            ));
                        }
                    }
                    SecretOp::Copy | SecretOp::Delete => {
                        return Err(Denial::new(
                            DenialCode::InvalidInput,
                            "mode must be \"create\" or \"update\"; use copy_secret or \
                             delete_secret",
                        ));
                    }
                }
                secret_keys(keys)?;
                secret_keys(remove)?;
                secret_write::check_explanation(explanation)?;
                allowed.confirmation = Confirmation::Required;
                Ok(allowed)
            }
            Request::SecretCopy {
                namespace,
                name,
                to_namespace,
                to_name,
                explanation,
            } => {
                let mut allowed = self.allowed(self.namespace(namespace.as_deref())?);
                let name = object_name(name)?;
                let to_namespace = match to_namespace {
                    Some(to) => self.namespace(Some(to))?,
                    None => allowed.namespace.clone(),
                };
                let to_name = match to_name {
                    Some(to) => object_name(to)?,
                    None => name.clone(),
                };
                if to_namespace == allowed.namespace && to_name == name {
                    return Err(Denial::new(
                        DenialCode::InvalidInput,
                        "a copy needs another namespace or name",
                    ));
                }
                secret_write::check_explanation(explanation)?;
                allowed.name = Some(name);
                allowed.to = Some(ObjectRef {
                    namespace: to_namespace,
                    name: to_name,
                });
                allowed.confirmation = Confirmation::Required;
                Ok(allowed)
            }
            Request::SecretDelete {
                namespace,
                name,
                explanation,
            } => {
                let mut allowed = self.allowed(self.namespace(namespace.as_deref())?);
                allowed.name = Some(object_name(name)?);
                secret_write::check_explanation(explanation)?;
                allowed.confirmation = Confirmation::Required;
                Ok(allowed)
            }
        }
    }

    fn allowed(&self, namespace: String) -> Allowed {
        Allowed {
            namespace,
            kind: None,
            name: None,
            manifests: Vec::new(),
            findings: Vec::new(),
            confirmation: Confirmation::NotNeeded,
            key: None,
            other: None,
            to: None,
            namespaces: Vec::new(),
        }
    }

    fn alive(&self) -> Result<(), Denial> {
        if self.closed {
            return Err(Denial::new(
                DenialCode::SessionEnded,
                "the session was closed",
            ));
        }
        if self.scope.expired() {
            return Err(Denial::new(
                DenialCode::SessionEnded,
                "the session has expired",
            ));
        }
        Ok(())
    }

    fn profile_allows(&self, request: &Request) -> Result<(), Denial> {
        // A setting the agent cannot ask to change: what the user may not
        // see, the agent does not get either (#96).
        if matches!(request, Request::SecretReveal { .. }) && !self.scope.policy.reveal_allowed() {
            return Err(Denial::new(
                DenialCode::RevealDisabled,
                "Secret values are hidden on this context, from the user and the agent alike. \
                 Only the user can show them, in Kubach: Settings → Security → Hide Secret \
                 values. Use list_secrets and check_secret, which need no values",
            ));
        }
        // The access, which the user may raise on request (#15).
        let Some(needed) = Self::needed_access(request) else {
            return Ok(());
        };
        let what = match request {
            Request::Logs { .. } => "reading logs",
            Request::Exec { .. } => "exec",
            Request::Debug { .. } => "a debug container",
            Request::PortForward { .. } => "port-forward",
            Request::Apply { dry_run: true, .. } => "a dry-run apply",
            Request::Apply { .. } => "apply",
            Request::SecretWrite { .. }
            | Request::SecretCopy { .. }
            | Request::SecretDelete { .. } => "writing Secrets",
            Request::SecretList { .. }
            | Request::SecretCheck { .. }
            | Request::SecretCompare { .. } => "Secret metadata",
            Request::SecretReveal { .. } => "revealing a Secret",
            Request::HelmRead { .. } => "reading Helm releases (Helm keeps them in Secrets)",
            Request::HelmChange { .. } => "a Helm change",
            _ => "changing resources",
        };
        self.within_access(needed, what)
    }

    /// Namespaces the session works in: the pinned ones, else those the
    /// user allowed reading.
    fn namespaces_in_use(&self) -> Vec<String> {
        if self.scope.pinned() {
            return self.scope.namespaces.clone();
        }
        self.grants
            .granted()
            .filter(|grant| matches!(grant, Grant::Read { .. }))
            .map(|grant| grant.namespace().to_owned())
            .collect()
    }

    /// The namespace to act in. Without one named: the session's only
    /// namespace. A named one must be a valid name and one of the pinned
    /// namespaces if the launch pinned them (#401).
    pub fn namespace(&self, requested: Option<&str>) -> Result<String, Denial> {
        let in_use = self.namespaces_in_use();
        let Some(namespace) = requested else {
            return match in_use.as_slice() {
                [only] => Ok(only.clone()),
                [] => Err(Denial::new(
                    DenialCode::Namespace,
                    "pass `namespace`: this session has none yet. list_namespaces shows the \
                     namespaces Kubach knows of",
                )),
                many => Err(Denial::new(
                    DenialCode::Namespace,
                    format!(
                        "namespace is required; this session has {}",
                        many.join(", ")
                    ),
                )),
            };
        };
        if self.scope.pinned() {
            return if in_use.iter().any(|n| n == namespace) {
                Ok(namespace.to_owned())
            } else {
                Err(Denial::new(
                    DenialCode::Namespace,
                    format!(
                        "namespace {namespace:?} is outside this session (allowed: {})",
                        in_use.join(", ")
                    ),
                ))
            };
        }
        if !names::is_dns_label(namespace) {
            return Err(Denial::new(
                DenialCode::Namespace,
                format!("{namespace:?} is not a namespace name"),
            ));
        }
        Ok(namespace.to_owned())
    }

    /// A kind the agent may read: served, namespaced, not a Secret, and in
    /// the built-in allowlist unless the session has custom resources
    /// (#192, asked for on request since #15).
    pub fn read_kind(&self, kind: &KindRef) -> Result<ResourceKind, Denial> {
        let resolved = self.find_kind(kind)?;
        if !resolved.namespaced {
            return Err(Denial::new(
                DenialCode::ClusterScoped,
                format!(
                    "{} is cluster-scoped; only namespaced resources are available",
                    resolved.api.kind
                ),
            ));
        }
        if is_core_secret(&resolved.api.group, &resolved.api.kind) {
            return Err(Denial::new(
                DenialCode::Secret,
                "Secrets are not available to the agent",
            ));
        }
        if !is_readable(&resolved.api.group, &resolved.api.plural) {
            self.within_access(
                Access {
                    crds: true,
                    ..Access::NONE
                },
                &format!("{} ({})", resolved.api.kind, resolved.api.api_version),
            )?;
        }
        Ok(resolved)
    }

    /// A kind the agent may change: readable, not an identity object, and
    /// in the built-in allowlist unless the session has custom resources.
    fn write_kind(&self, kind: &KindRef) -> Result<ResourceKind, Denial> {
        let resolved = self.read_kind(kind)?;
        ensure_writable(&resolved.api.group, &resolved.api.kind)?;
        if !is_writable(&resolved.api.group, &resolved.api.plural) {
            self.within_access(
                Access {
                    crds: true,
                    ..Access::NONE
                },
                &format!("changing {} objects", resolved.api.kind),
            )?;
        }
        Ok(resolved)
    }

    fn find_kind(&self, kind: &KindRef) -> Result<ResourceKind, Denial> {
        if !names::is_kind_name(&kind.kind) {
            return Err(Denial::new(
                DenialCode::InvalidName,
                format!("{:?} is not a kind name", kind.kind),
            ));
        }
        let group = match &kind.api_version {
            Some(api_version) if names::is_api_version(api_version) => {
                Some(api_version.split_once('/').map_or("", |(group, _)| group))
            }
            Some(api_version) => {
                return Err(Denial::new(
                    DenialCode::InvalidName,
                    format!("{api_version:?} is not an apiVersion"),
                ));
            }
            None => None,
        };
        let lower = kind.kind.to_ascii_lowercase();
        let candidates: Vec<&ResourceKind> = self
            .catalog
            .kinds()
            .iter()
            .filter(|k| k.api.kind.eq_ignore_ascii_case(&kind.kind) || k.api.plural == lower)
            .filter(|k| group.is_none_or(|g| k.api.group == g))
            .collect();
        let chosen = match candidates.as_slice() {
            [] => {
                return Err(Denial::new(
                    DenialCode::UnknownKind,
                    format!("the cluster does not serve kind {:?}", kind.kind),
                ));
            }
            [only] => *only,
            many => {
                // Prefer the core group, as kubectl does for `events`.
                match many.iter().find(|k| k.api.group.is_empty()) {
                    Some(core) => *core,
                    None => {
                        let groups: Vec<&str> = many.iter().map(|k| k.api.group.as_str()).collect();
                        return Err(Denial::new(
                            DenialCode::UnknownKind,
                            format!(
                                "kind {:?} is ambiguous (groups: {}); pass apiVersion",
                                kind.kind,
                                groups.join(", ")
                            ),
                        ));
                    }
                }
            }
        };
        let mut resolved = chosen.clone();
        if let Some(api_version) = &kind.api_version {
            if let Some((_, version)) = api_version.split_once('/') {
                resolved.api.version = version.to_owned();
            } else {
                resolved.api.version = api_version.clone();
            }
            resolved.api.api_version = api_version.clone();
        }
        Ok(resolved)
    }

    fn check_action(
        &self,
        kind: &KindRef,
        name: &str,
        namespace: Option<&str>,
    ) -> Result<Allowed, Denial> {
        let mut allowed = self.allowed(self.namespace(namespace)?);
        allowed.kind = Some(self.write_kind(kind)?);
        allowed.name = Some(object_name(name)?);
        allowed.confirmation = Confirmation::Required;
        Ok(allowed)
    }

    fn check_apply(
        &self,
        yaml: &str,
        namespace: Option<&str>,
        dry_run: bool,
    ) -> Result<Allowed, Denial> {
        let namespace = self.namespace(namespace)?;
        if yaml.len() > MAX_APPLY_BYTES {
            return Err(Denial::new(
                DenialCode::InvalidInput,
                format!("manifests exceed {MAX_APPLY_BYTES} bytes"),
            ));
        }
        let manifests = apply::parse(yaml)
            .map_err(|e| Denial::new(DenialCode::Manifest, format!("invalid YAML: {e:?}")))?;
        if manifests.is_empty() || manifests.len() > MAX_APPLY_DOCUMENTS {
            return Err(Denial::new(
                DenialCode::Manifest,
                format!("expected 1 to {MAX_APPLY_DOCUMENTS} documents"),
            ));
        }
        let mut resolved = Vec::with_capacity(manifests.len());
        let mut findings = Vec::new();
        for manifest in &manifests {
            let doc = manifest.index + 1;
            let fail =
                |code, message: String| Denial::new(code, format!("document {doc}: {message}"));
            let value = &manifest.value;
            let api_version = value
                .get("apiVersion")
                .and_then(Value::as_str)
                .unwrap_or("");
            let kind_name = value.get("kind").and_then(Value::as_str).unwrap_or("");
            if kind_name.ends_with("List") {
                return Err(fail(
                    DenialCode::Manifest,
                    "List documents are not supported; pass objects as separate documents".into(),
                ));
            }
            if !names::is_api_version(api_version) {
                return Err(fail(
                    DenialCode::Manifest,
                    "apiVersion is missing or invalid".into(),
                ));
            }
            let kind = self
                .write_kind(&KindRef {
                    kind: kind_name.to_owned(),
                    api_version: Some(api_version.to_owned()),
                })
                .map_err(|d| fail(d.code, d.message))?;
            // `find_kind` also matches plurals; manifests must use the kind.
            if kind.api.kind != kind_name {
                return Err(fail(
                    DenialCode::Manifest,
                    format!("unknown kind {kind_name:?}"),
                ));
            }
            let metadata = value.get("metadata").and_then(Value::as_object);
            match metadata.and_then(|m| m.get("namespace")) {
                None | Some(Value::Null) => {}
                Some(Value::String(ns)) if *ns == namespace => {}
                Some(other) => {
                    return Err(fail(
                        DenialCode::Namespace,
                        format!(
                            "metadata.namespace {other} differs from the session namespace {namespace:?}"
                        ),
                    ));
                }
            }
            if let Some(name) = metadata.and_then(|m| m.get("name")).and_then(Value::as_str) {
                object_name(name).map_err(|d| fail(d.code, d.message))?;
            }
            if metadata
                .and_then(|m| m.get("ownerReferences"))
                .is_some_and(|refs| !refs.is_null())
            {
                return Err(fail(
                    DenialCode::Manifest,
                    "metadata.ownerReferences may not be set by the agent".into(),
                ));
            }
            if let Some(path) = nested_foreign_namespace(value, &namespace) {
                return Err(fail(
                    DenialCode::Namespace,
                    format!("{path} points outside the session namespace"),
                ));
            }
            let gitops = gitops_findings(&kind, value, &namespace)
                .map_err(|message| fail(DenialCode::Namespace, message))?;
            let bound = apply::resolve(manifest, self.catalog, &namespace)
                .map_err(|e| fail(DenialCode::Manifest, e.kind.to_string()))?;
            // Defense in depth: never trust the resolver alone.
            if bound.namespace.as_deref() != Some(namespace.as_str()) {
                return Err(fail(
                    DenialCode::Namespace,
                    "resolved namespace differs from the session".into(),
                ));
            }
            let mut bound = bound;
            bound.kind = kind;
            findings.extend(dangerous_fields(value).into_iter().map(|mut f| {
                f.document = Some(manifest.index);
                f
            }));
            findings.extend(
                foreign_namespace_refs(value, &namespace)
                    .into_iter()
                    .chain(gitops)
                    .map(|mut f| {
                        f.document = Some(manifest.index);
                        f
                    }),
            );
            resolved.push(bound);
        }
        let mut allowed = self.allowed(namespace);
        allowed.manifests = resolved;
        allowed.findings = findings;
        allowed.confirmation = if dry_run {
            Confirmation::NotNeeded
        } else {
            Confirmation::Required
        };
        Ok(allowed)
    }
}

/// Kinds the agent may never write: Secrets and anything that changes who
/// can do what (ServiceAccounts, RBAC).
/// ServiceAccounts, RBAC and the other kinds that decide who may do what:
/// the agent never changes them, and `list_kinds` does not offer them.
pub(crate) fn is_identity(group: &str, kind: &str) -> bool {
    (group.is_empty() && kind == "ServiceAccount")
        || group == "rbac.authorization.k8s.io"
        || group == "authorization.k8s.io"
        || group == "authentication.k8s.io"
        || group == "certificates.k8s.io"
        || group == "admissionregistration.k8s.io"
}

fn ensure_writable(group: &str, kind: &str) -> Result<(), Denial> {
    let identity = is_identity(group, kind);
    if is_core_secret(group, kind) {
        Err(Denial::new(
            DenialCode::Secret,
            "Secrets are not available to the agent",
        ))
    } else if identity {
        Err(Denial::new(
            DenialCode::ForbiddenKind,
            format!("the agent may not change {kind} objects"),
        ))
    } else {
        Ok(())
    }
}

fn object_name(name: &str) -> Result<String, Denial> {
    if names::is_dns_subdomain(name) {
        Ok(name.to_owned())
    } else {
        Err(Denial::new(
            DenialCode::InvalidName,
            format!("{name:?} is not a valid object name"),
        ))
    }
}

fn secret_key(key: &str) -> Result<String, Denial> {
    kubach_core::secrets::validate_key(key)
        .map(|()| key.to_owned())
        .map_err(|_| {
            Denial::new(
                DenialCode::InvalidName,
                format!("{key:?} is not a valid Secret key"),
            )
        })
}

/// Keys the agent names: valid, unique, at most [`MAX_KEYS`].
fn secret_keys(keys: &[String]) -> Result<(), Denial> {
    if keys.len() > MAX_KEYS {
        return Err(Denial::new(
            DenialCode::InvalidInput,
            format!("at most {MAX_KEYS} keys"),
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for key in keys {
        secret_key(key)?;
        if !seen.insert(key) {
            return Err(Denial::new(
                DenialCode::InvalidInput,
                format!("key {key:?} is named twice"),
            ));
        }
    }
    Ok(())
}

/// The kind and name of a port-forward target: `pod/<name>`,
/// `service/<name>` or `deployment/<name>` (also `pods`, `po`, `svc`,
/// `deploy`), as in the forward dialog of the app (#17).
pub fn forward_target(target: &str) -> Result<(&'static str, &str), Denial> {
    let invalid = || {
        Denial::new(
            DenialCode::InvalidInput,
            "target must be pod/<name>, service/<name> or deployment/<name>",
        )
    };
    let (kind, name) = target.split_once('/').ok_or_else(invalid)?;
    let kind = match kind.to_ascii_lowercase().as_str() {
        "pod" | "pods" | "po" => "Pod",
        "service" | "services" | "svc" => "Service",
        "deployment" | "deployments" | "deploy" => "Deployment",
        _ => return Err(invalid()),
    };
    Ok((kind, name))
}

/// A remote port: a number from 1, or a port name (`IANA_SVC_NAME`: up to
/// 15 lowercase letters, digits and inner dashes, with a letter).
fn check_forward_port(port: &ForwardPortRef) -> Result<(), Denial> {
    let valid = match port {
        ForwardPortRef::Number(number) => *number > 0,
        ForwardPortRef::Name(name) => {
            (1..=15).contains(&name.len())
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && name.bytes().any(|b| b.is_ascii_lowercase())
                && !name.starts_with('-')
                && !name.ends_with('-')
                && !name.contains("--")
        }
    };
    if valid {
        Ok(())
    } else {
        Err(Denial::new(
            DenialCode::InvalidInput,
            "port must be a port number (1-65535) or a port name such as http",
        ))
    }
}

fn container_name(name: &str) -> Result<(), Denial> {
    if names::is_dns_label(name) {
        Ok(())
    } else {
        Err(Denial::new(
            DenialCode::InvalidName,
            format!("{name:?} is not a valid container name"),
        ))
    }
}

/// An image reference as the user sees it in the confirmation: registry,
/// path, tag and digest characters only, nothing hidden (#64).
fn check_image(image: &str) -> Result<(), Denial> {
    let valid = !image.is_empty()
        && image.len() <= MAX_IMAGE_REF_BYTES
        && !image.starts_with(['-', '.', '/', ':', '@'])
        && image
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-/:@".contains(&b));
    if valid {
        Ok(())
    } else {
        Err(Denial::new(
            DenialCode::InvalidInput,
            format!(
                "{image:?} is not an image reference: use letters, digits and . _ - / : @                  only, at most {MAX_IMAGE_REF_BYTES} bytes, e.g. docker.io/library/busybox:1.37"
            ),
        ))
    }
}

fn check_selector(selector: &str) -> Result<(), Denial> {
    if selector.len() > 1024 || selector.chars().any(char::is_control) {
        return Err(Denial::new(
            DenialCode::InvalidInput,
            "invalid label selector",
        ));
    }
    Ok(())
}

fn check_command(command: &[String]) -> Result<(), Denial> {
    if command.is_empty() || command.len() > MAX_EXEC_ARGS {
        return Err(Denial::new(
            DenialCode::InvalidInput,
            format!("command must have 1 to {MAX_EXEC_ARGS} arguments"),
        ));
    }
    for (ix, arg) in command.iter().enumerate() {
        if arg.len() > MAX_EXEC_ARG_BYTES {
            return Err(Denial::new(
                DenialCode::InvalidInput,
                format!("command argument {ix} is longer than {MAX_EXEC_ARG_BYTES} bytes"),
            ));
        }
        if let Some(c) = arg.chars().find(|&c| hidden_in_command(c)) {
            return Err(Denial::new(
                DenialCode::InvalidInput,
                format!(
                    "command argument {ix} contains U+{:04X}: control, line-breaking, \
                     invisible and bidi characters are not allowed in exec; pass scripts \
                     with `;` instead of newlines",
                    u32::from(c)
                ),
            ));
        }
    }
    let total: usize = command.iter().map(String::len).sum();
    if total > MAX_EXEC_COMMAND_BYTES {
        return Err(Denial::new(
            DenialCode::InvalidInput,
            format!("command is longer than {MAX_EXEC_COMMAND_BYTES} bytes"),
        ));
    }
    Ok(())
}

/// Characters that could hide part of an exec command from the user who
/// confirms it (#194): C0 controls except tab (shown as `\t`), DEL, C1,
/// line and paragraph separators, zero-width and other invisible format
/// characters and bidi overrides, embeddings, isolates and marks. The
/// confirmation dialog spells such characters out as well.
pub fn hidden_in_command(c: char) -> bool {
    (c.is_control() && c != '\t')
        || matches!(
            c,
            '\u{061C}'
                | '\u{115F}'
                | '\u{1160}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{3164}'
                | '\u{FE00}'..='\u{FE0F}'
                | '\u{FEFF}'
                | '\u{FFA0}'
                | '\u{FFF9}'..='\u{FFFB}'
        )
}

/// A nested `metadata.namespace` (pod templates, job templates) that differs
/// from the session namespace. The top-level one is checked separately.
fn nested_foreign_namespace(value: &Value, namespace: &str) -> Option<String> {
    let mut found = None;
    walk_fields(value, &mut String::new(), &mut |path, key, child| {
        if found.is_none()
            && key == "namespace"
            && path.ends_with(".metadata.namespace")
            && path != ".metadata.namespace"
            && !child.is_null()
            && child.as_str() != Some(namespace)
        {
            found = Some(path.to_owned());
        }
    });
    found
}

/// Other `namespace`/`namespaces` fields naming foreign namespaces
/// (affinity terms, cross-namespace references of CRDs). Allowed, but the
/// user has to see them.
fn foreign_namespace_refs(value: &Value, namespace: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    walk_fields(value, &mut String::new(), &mut |path, key, child| {
        let foreign = match (key, child) {
            ("namespace", Value::String(ns)) => ns != namespace,
            ("namespaces", Value::Array(list)) => {
                list.iter().any(|ns| ns.as_str() != Some(namespace))
            }
            _ => false,
        };
        if foreign && path != ".metadata.namespace" {
            findings.push(Finding {
                document: None,
                path: path.to_owned(),
                message: format!("references another namespace: {child}"),
            });
        }
    });
    findings
}

const FLUX_KUSTOMIZE: &str = "kustomize.toolkit.fluxcd.io";
const FLUX_HELM: &str = "helm.toolkit.fluxcd.io";
const ARGO: &str = "argoproj.io";
/// The cluster an Argo CD application runs in, by its default names.
const ARGO_IN_CLUSTER: &[&str] = &["https://kubernetes.default.svc", "in-cluster"];

/// GitOps objects make a controller apply other objects with its own rights
/// (#192). Where they deploy to must be the session namespace (an `Err`
/// with the path, like `metadata.namespace`); who applies and what they read
/// the user has to see.
fn gitops_findings(
    kind: &ResourceKind,
    value: &Value,
    namespace: &str,
) -> Result<Vec<Finding>, String> {
    let mut findings = Vec::new();
    let finding = |path: &str, message: &str| Finding {
        document: None,
        path: path.to_owned(),
        message: message.to_owned(),
    };
    let spec = value.get("spec");
    let field = |key: &str| {
        spec.and_then(|s| s.get(key))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    let group = kind.api.group.as_str();
    let flux = (group == FLUX_KUSTOMIZE && kind.api.kind == "Kustomization")
        || (group == FLUX_HELM && kind.api.kind == "HelmRelease");
    if flux {
        for key in ["targetNamespace", "storageNamespace"] {
            if let Some(target) = field(key)
                && target != namespace
            {
                return Err(format!(
                    ".spec.{key} {target:?} points outside the session namespace"
                ));
            }
        }
        if field("targetNamespace").is_none() {
            findings.push(finding(
                ".spec.targetNamespace",
                "not set: the objects keep the namespaces written in their source, which \
                 Kubach does not see",
            ));
        }
        // A set name is reported by `dangerous_fields` ("runs as").
        if field("serviceAccountName").is_none() {
            findings.push(finding(
                ".spec.serviceAccountName",
                "not set: the Flux controller applies with its own rights, usually \
                 cluster-admin",
            ));
        }
        if spec.and_then(|s| s.get("kubeConfig")).is_some() {
            findings.push(finding(".spec.kubeConfig", "deploys to another cluster"));
        }
    }
    if group == ARGO {
        match kind.api.kind.as_str() {
            "Application" => argo_application(spec, ".spec", namespace, &mut findings)?,
            "ApplicationSet" => {
                let template = spec
                    .and_then(|s| s.get("template"))
                    .and_then(|t| t.get("spec"));
                argo_application(template, ".spec.template.spec", namespace, &mut findings)?;
            }
            "AppProject" => findings.push(finding(
                ".spec",
                "defines where Argo CD applications may deploy and which kinds they may \
                 create",
            )),
            _ => {}
        }
    }
    Ok(findings)
}

/// The destination and project of an Argo CD application (spec at `path`).
fn argo_application(
    spec: Option<&Value>,
    path: &str,
    namespace: &str,
    findings: &mut Vec<Finding>,
) -> Result<(), String> {
    let destination = spec.and_then(|s| s.get("destination"));
    let text = |value: Option<&Value>, key: &str| {
        value
            .and_then(|v| v.get(key))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let mut push = |field: &str, message: String| {
        findings.push(Finding {
            document: None,
            path: format!("{path}{field}"),
            message,
        });
    };
    match text(destination, "namespace") {
        Some(target) if target != namespace => {
            return Err(format!(
                "{path}.destination.namespace {target:?} points outside the session namespace"
            ));
        }
        Some(_) => {}
        None => push(
            ".destination.namespace",
            "not set: the objects keep the namespaces written in their source, which Kubach \
             does not see"
                .into(),
        ),
    }
    for key in ["server", "name"] {
        if let Some(cluster) = text(destination, key)
            && !ARGO_IN_CLUSTER.contains(&cluster.as_str())
        {
            push(
                &format!(".destination.{key}"),
                format!("deploys to another cluster: {cluster:?}"),
            );
        }
    }
    let project = text(spec, "project").unwrap_or_else(|| "default".to_owned());
    let default_note = if project == "default" {
        ", which by default allows every namespace and cluster-scoped kinds"
    } else {
        ""
    };
    push(
        ".project",
        format!(
            "Argo CD applies with its controller's rights, limited only by project \
             {project:?}{default_note}"
        ),
    );
    Ok(())
}

/// The Secret an object names: `secretName` of a volume, `name` elsewhere.
fn secret_name(object: &serde_json::Map<String, Value>) -> String {
    object
        .get("secretName")
        .or_else(|| object.get("name"))
        .and_then(Value::as_str)
        .map_or_else(|| "(unnamed)".to_owned(), |name| format!("{name:?}"))
}

/// What a workload reads or runs as that its logs may give away (#190):
/// Secrets in volumes, `envFrom` and `env`, ServiceAccount tokens, and
/// Secrets Flux reads into values.
fn secret_access(key: &str, child: &Value) -> Option<String> {
    match (key, child) {
        // Volumes and projected sources.
        ("secret", Value::Object(object)) => Some(format!(
            "manifest reads Secret {} (volume)",
            secret_name(object)
        )),
        // `envFrom`, and Secrets of other volume types.
        ("secretRef", Value::Object(object)) => Some(format!(
            "manifest reads Secret {} (secretRef)",
            secret_name(object)
        )),
        ("secretKeyRef", Value::Object(object)) => Some(format!(
            "manifest reads key {} of Secret {} (env)",
            object
                .get("key")
                .and_then(Value::as_str)
                .map_or_else(|| "(unnamed)".to_owned(), |key| format!("{key:?}")),
            secret_name(object)
        )),
        ("serviceAccountToken", Value::Object(_)) => {
            Some("mounts a ServiceAccount token (projected volume)".to_owned())
        }
        ("serviceAccountName" | "serviceAccount", Value::String(sa))
            if !sa.is_empty() && sa != "default" =>
        {
            Some(format!("runs as ServiceAccount {sa:?}"))
        }
        ("automountServiceAccountToken", Value::Bool(true)) => {
            Some("mounts the ServiceAccount token into the pod".to_owned())
        }
        // Flux reads these Secrets into values and substitutions.
        ("valuesFrom" | "substituteFrom", Value::Array(items)) => {
            let names: Vec<String> = items
                .iter()
                .filter_map(Value::as_object)
                .filter(|item| item.get("kind").and_then(Value::as_str) == Some("Secret"))
                .map(secret_name)
                .collect();
            (!names.is_empty())
                .then(|| format!("manifest reads Secret {} ({key})", names.join(", ")))
        }
        _ => None,
    }
}

/// Fields that give a workload host access, another identity or a Secret's
/// values.
pub fn dangerous_fields(value: &Value) -> Vec<Finding> {
    let mut findings = Vec::new();
    walk_fields(value, &mut String::new(), &mut |path, key, child| {
        if let Some(message) = secret_access(key, child) {
            findings.push(Finding {
                document: None,
                path: path.to_owned(),
                message,
            });
            return;
        }
        let message = match (key, child) {
            ("hostPath", Value::Object(_)) => Some("mounts a hostPath volume"),
            ("privileged", Value::Bool(true)) => Some("runs a privileged container"),
            ("allowPrivilegeEscalation", Value::Bool(true)) => Some("allows privilege escalation"),
            ("hostNetwork", Value::Bool(true)) => Some("uses the host network"),
            ("hostPID", Value::Bool(true)) => Some("uses the host PID namespace"),
            ("hostIPC", Value::Bool(true)) => Some("uses the host IPC namespace"),
            ("hostPort", Value::Number(_)) => Some("binds a host port"),
            ("procMount", Value::String(m)) if m == "Unmasked" => Some("unmasks /proc"),
            ("nodeName", Value::String(n)) if !n.is_empty() => {
                Some("pins the pod to a node, bypassing the scheduler")
            }
            ("ephemeralContainers", Value::Array(list)) if !list.is_empty() => {
                Some("adds ephemeral containers")
            }
            ("capabilities", Value::Object(caps))
                if caps
                    .get("add")
                    .and_then(Value::as_array)
                    .is_some_and(|add| !add.is_empty()) =>
            {
                Some("adds Linux capabilities")
            }
            _ => None,
        };
        if let Some(message) = message {
            findings.push(Finding {
                document: None,
                path: path.to_owned(),
                message: message.to_owned(),
            });
        }
    });
    findings
}

/// ServiceAccounts a pod (template) would run as that differ from what is
/// live. A new object running as `default` is not a change.
pub fn service_account_changes(before: Option<&Value>, after: &Value) -> Vec<Finding> {
    let collect = |value: &Value| {
        let mut found = Vec::new();
        walk_fields(value, &mut String::new(), &mut |path, key, child| {
            if (key == "serviceAccountName" || key == "serviceAccount")
                && let Value::String(sa) = child
            {
                found.push((path.to_owned(), sa.clone()));
            }
        });
        found
    };
    let before = before.map(collect).unwrap_or_default();
    collect(after)
        .into_iter()
        .filter(|(path, sa)| {
            let old = before
                .iter()
                .find(|(p, _)| p == path)
                .map(|(_, v)| v.as_str());
            match old {
                Some(old) => old != sa,
                None => sa != "default",
            }
        })
        .map(|(path, sa)| Finding {
            document: None,
            path,
            message: format!("runs as ServiceAccount {sa:?} (changed)"),
        })
        .collect()
}

/// Calls `f(path, key, value)` for every object field, depth first.
pub(crate) fn walk_fields(value: &Value, path: &mut String, f: &mut dyn FnMut(&str, &str, &Value)) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let len = path.len();
                path.push('.');
                path.push_str(key);
                f(path, key, child);
                walk_fields(child, path, f);
                path.truncate(len);
            }
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                let len = path.len();
                path.push_str(&format!("[{i}]"));
                walk_fields(item, path, f);
                path.truncate(len);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests;
