//! A running MCP session: scope, the cluster's kinds, the capabilities
//! `SelfSubjectAccessReview` confirmed, the rights the user granted on
//! request (#227) and, per namespace, the identity the agent's calls there
//! run under (#401).
//!
//! A session starts without namespaces. When the agent first works in one,
//! the session first sets up the strongest identity the user's rights allow
//! in that namespace (impersonation, then a temporary ServiceAccount, then
//! client only, ADR 0005; chosen automatically since #15) and only then
//! records the right. A namespace where only Kubach holds the boundary is marked so.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use k8s_openapi::jiff::Timestamp;
use kubach_core::discovery::{ApiCatalog, DiscoveryError};
use kubach_core::rbac::{ResourceAccess, can_i, can_i_all};
use kube::{Client, Config};
use serde::{Deserialize, Serialize};

use crate::access::{Access, Raises};
use crate::forwards::{SessionForwards, StopReason};
use crate::grants::{Grant, GrantState, Grants};
use crate::identity::{self, Identity, IdentityError, TemporaryServiceAccount};
use crate::policy::{NamespaceModes, Policy};
use crate::rules::{Capability, temporary_sa_requirements};
use crate::scope::{ScopeError, SessionMode, SessionScope, SessionSpec};
use crate::settings::SecurityPolicy;
use kubach_core::sync;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Scope(#[from] ScopeError),
    #[error(transparent)]
    Identity(#[from] Box<IdentityError>),
    #[error(transparent)]
    Discovery(#[from] Box<DiscoveryError>),
    #[error("the session has ended")]
    Ended,
    #[error("namespace `{0}` is not part of this session")]
    NotInSession(String),
}

impl From<IdentityError> for SessionError {
    fn from(error: IdentityError) -> Self {
        Self::Identity(Box::new(error))
    }
}

/// Who holds the boundary in one namespace: the identity the agent's calls
/// there run under, why a stronger one was not used, and the warning when a
/// configured server-side identity failed (#197).
struct Boundary {
    identity: Identity,
    notes: Vec<String>,
    degraded: Option<String>,
}

/// One namespace's boundary, for the UI, the bridge and `session_info`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundaryInfo {
    pub namespace: String,
    pub mode: SessionMode,
    /// `user`, `impersonated:<name>` or `serviceaccount:<ns>/<name>`.
    pub identity: String,
    /// Why a stronger identity was not used.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// `server-side limits are off: …` when a configured identity failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

impl BoundaryInfo {
    /// Whether the API server holds the boundary in this namespace too.
    pub fn server_enforced(&self) -> bool {
        self.mode.server_enforced()
    }

    /// One line for logs and lists: `web: boundary enforced by the API
    /// server (serviceaccount:web/kubach-mcp-1)`, then why and warnings.
    pub fn summary(&self) -> String {
        let mut line = format!(
            "{}: {} ({})",
            self.namespace,
            if self.server_enforced() {
                "boundary enforced by the API server"
            } else {
                "boundary only in Kubach"
            },
            self.identity
        );
        for note in &self.notes {
            line.push_str("; ");
            line.push_str(note);
        }
        if let Some(warning) = &self.warning {
            line.push_str("; WARNING: ");
            line.push_str(warning);
        }
        line
    }
}

/// Where the identities of namespaces come from.
enum Identities {
    /// Chosen per namespace from the user's kubeconfig for the context.
    Cluster { base: Box<Config> },
    /// One identity for every namespace, set up by tests.
    #[cfg(test)]
    Fixed(Identity),
}

/// A started session. Shared by the MCP server and the app (for closing).
pub struct Session {
    scope: SessionScope,
    identities: Identities,
    /// The user's own credentials with the session's User-Agent: discovery,
    /// `list_namespaces` and setting up identities.
    user_client: Client,
    /// Per namespace, set up when the user first allows reading it.
    boundaries: Mutex<BTreeMap<String, Arc<Boundary>>>,
    catalog: ApiCatalog,
    /// What the user's RBAC allows, as tests set it up; real sessions ask
    /// the API server per namespace when a call needs it ([`Session::user_can`]).
    test_rbac: Option<HashSet<Capability>>,
    /// `SelfSubjectAccessReview` answers of the user per namespace, cached
    /// for the session.
    rbac: Mutex<HashMap<ResourceAccess, bool>>,
    /// The user's answers about access (#15).
    raises: Mutex<Raises>,
    /// Chart sources (`oci://…` repositories) the user allowed or refused
    /// for this session (#418).
    chart_sources: Mutex<HashMap<String, bool>>,
    closed: AtomicBool,
    activity: Mutex<Activity>,
    /// The user's answers so far; empty at the start.
    grants: Mutex<Grants>,
    /// One question to the user at a time per session, so two calls do not
    /// ask for the same right twice.
    ask: tokio::sync::Mutex<()>,
    /// Orders grant changes with the identities and Role patches that
    /// mirror them.
    role: tokio::sync::Mutex<()>,
    /// Objects the agent created or changed with `apply_manifest` in this
    /// session, as `(namespace, kind, name)`: reading the logs of their pods
    /// is confirmed (#190).
    changed: Mutex<HashSet<ChangedObject>>,
    /// The agent's port-forwards (#17); they end with the session.
    forwards: SessionForwards,
    /// This session, for tasks that must not keep it alive.
    me: Weak<Session>,
}

/// Under which identity a `helm` process acts in a namespace (#418): the
/// namespace's own, never the user's where the session has another.
pub enum HelmAccess {
    /// The temporary ServiceAccount: a kubeconfig with the cluster only and
    /// a fresh token for `HELM_KUBETOKEN`.
    Token { kubeconfig: String, token: String },
    /// The administrator-prepared identity, over the user's kubeconfig.
    Impersonate { user: String, groups: Vec<String> },
    /// Client only: the user's own kubeconfig, as the app's Helm screens.
    User,
}

/// `(namespace, kind, name)` of an object the agent applied.
pub type ChangedObject = (String, String, String);

/// Who used a session and how much, for the sessions screen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Activity {
    /// MCP clients by their self-reported names, in order of first call.
    pub clients: Vec<String>,
    pub calls: u64,
    pub last_call: Option<Timestamp>,
}

impl Session {
    /// Validates `spec` against `policy`, runs discovery and probes which
    /// tools can work. No namespace is touched yet: identities are set up
    /// per namespace when the user allows reading it.
    ///
    /// `base` is the user's kubeconfig for the context. Must run on a tokio
    /// runtime; the session closes itself when its TTL ends.
    pub async fn start(
        base: &Config,
        spec: &SessionSpec,
        policy: &SecurityPolicy,
    ) -> Result<Arc<Self>, SessionError> {
        let scope = SessionScope::new(spec, policy)?;
        let user_config = identity::session_config(base, &scope.id)?;
        let user_client = kubach_core::redact::client(user_config).map_err(IdentityError::from)?;
        // Kinds are the same for every identity; what each may do with them
        // the API server decides per namespace.
        let catalog = ApiCatalog::discover(&user_client).await.map_err(Box::new)?;
        // The user's RBAC is checked per namespace when a call needs it,
        // not probed here: with namespaces on request (#401) the home
        // namespace says nothing about the one the agent will name.
        let forwards = SessionForwards::new(&scope.id);
        let session = Arc::new_cyclic(|me| Self {
            scope,
            identities: Identities::Cluster {
                base: Box::new(base.clone()),
            },
            user_client,
            boundaries: Mutex::default(),
            catalog,
            test_rbac: None,
            rbac: Mutex::default(),
            raises: Mutex::default(),
            chart_sources: Mutex::default(),
            closed: AtomicBool::new(false),
            activity: Mutex::default(),
            grants: Mutex::default(),
            ask: tokio::sync::Mutex::new(()),
            role: tokio::sync::Mutex::new(()),
            changed: Mutex::default(),
            forwards,
            me: me.clone(),
        });
        spawn_ttl_guard(Arc::downgrade(&session), session.scope.deadline());
        Ok(session)
    }

    /// A session from prepared parts, skipping discovery and RBAC probes;
    /// `identity` serves every namespace.
    #[cfg(test)]
    pub(crate) fn from_parts(
        scope: SessionScope,
        identity: Identity,
        catalog: ApiCatalog,
        capabilities: HashSet<Capability>,
    ) -> Arc<Self> {
        let forwards = SessionForwards::new(&scope.id);
        Arc::new_cyclic(|me| Self {
            scope,
            user_client: identity.client(),
            identities: Identities::Fixed(identity),
            boundaries: Mutex::default(),
            catalog,
            test_rbac: Some(capabilities),
            rbac: Mutex::default(),
            raises: Mutex::default(),
            chart_sources: Mutex::default(),
            closed: AtomicBool::new(false),
            activity: Mutex::default(),
            grants: Mutex::default(),
            ask: tokio::sync::Mutex::new(()),
            role: tokio::sync::Mutex::new(()),
            changed: Mutex::default(),
            forwards,
            me: me.clone(),
        })
    }

    pub fn id(&self) -> &str {
        &self.scope.id
    }

    pub fn scope(&self) -> &SessionScope {
        &self.scope
    }

    fn boundary(&self, namespace: &str) -> Option<Arc<Boundary>> {
        sync::lock(&self.boundaries).get(namespace).cloned()
    }

    /// The mode in `namespace`: client only until an identity is set up
    /// there (nothing runs there before that).
    pub fn mode_in(&self, namespace: &str) -> SessionMode {
        if let Some(boundary) = self.boundary(namespace) {
            return boundary.identity.mode();
        }
        match &self.identities {
            Identities::Cluster { .. } => SessionMode::ClientOnly,
            #[cfg(test)]
            Identities::Fixed(identity) => identity.mode(),
        }
    }

    /// The weakest mode among the session's namespaces, for one-line
    /// summaries; client only while none is set up. [`Self::boundaries`]
    /// has the details.
    pub fn mode(&self) -> SessionMode {
        let boundaries = sync::lock(&self.boundaries);
        if boundaries.is_empty() {
            drop(boundaries);
            return match &self.identities {
                Identities::Cluster { .. } => SessionMode::ClientOnly,
                #[cfg(test)]
                Identities::Fixed(identity) => identity.mode(),
            };
        }
        if boundaries
            .values()
            .any(|b| b.identity.mode() == SessionMode::ClientOnly)
        {
            SessionMode::ClientOnly
        } else if boundaries
            .values()
            .any(|b| b.identity.mode() == SessionMode::Impersonation)
        {
            SessionMode::Impersonation
        } else {
            SessionMode::TemporaryServiceAccount
        }
    }

    /// Modes per namespace, for the policy.
    pub fn modes(&self) -> NamespaceModes {
        let default = match &self.identities {
            Identities::Cluster { .. } => SessionMode::ClientOnly,
            #[cfg(test)]
            Identities::Fixed(identity) => identity.mode(),
        };
        NamespaceModes {
            default,
            by_namespace: sync::lock(&self.boundaries)
                .iter()
                .map(|(namespace, boundary)| (namespace.clone(), boundary.identity.mode()))
                .collect(),
        }
    }

    /// Client of the identity of `namespace` for the next call. Refused
    /// for a namespace without one: the agent's calls never fall back to
    /// the user's credentials by accident.
    pub fn client_in(&self, namespace: &str) -> Result<Client, SessionError> {
        if let Some(boundary) = self.boundary(namespace) {
            return Ok(boundary.identity.client());
        }
        match &self.identities {
            Identities::Cluster { .. } => Err(SessionError::NotInSession(namespace.to_owned())),
            #[cfg(test)]
            Identities::Fixed(identity) => Ok(identity.client()),
        }
    }

    /// The user's credentials with the session's User-Agent. Only for what
    /// is not in a namespace: listing namespace names (#401).
    pub fn user_client(&self) -> Client {
        self.user_client.clone()
    }

    /// For the audit log: the identity of `namespace`, or `user` where none
    /// is set up (only namespace names are read that way).
    pub fn describe_identity(&self, namespace: Option<&str>) -> String {
        if let Some(boundary) = namespace.and_then(|ns| self.boundary(ns)) {
            return boundary.identity.describe();
        }
        match &self.identities {
            Identities::Cluster { .. } => "user".into(),
            #[cfg(test)]
            Identities::Fixed(identity) => identity.describe(),
        }
    }

    /// Boundaries of the namespaces set up so far, by name.
    pub fn boundaries(&self) -> Vec<BoundaryInfo> {
        sync::lock(&self.boundaries)
            .iter()
            .map(|(namespace, boundary)| BoundaryInfo {
                namespace: namespace.clone(),
                mode: boundary.identity.mode(),
                identity: boundary.identity.describe(),
                notes: boundary.notes.clone(),
                warning: boundary.degraded.clone(),
            })
            .collect()
    }

    /// Namespaces of the session: the pinned ones, else those the user
    /// allowed reading.
    pub fn namespaces(&self) -> Vec<String> {
        if self.scope.pinned() {
            return self.scope.namespaces.clone();
        }
        self.grants()
            .granted()
            .filter(|grant| matches!(grant, Grant::Read { .. }))
            .map(|grant| grant.namespace().to_owned())
            .collect()
    }

    pub fn catalog(&self) -> &ApiCatalog {
        &self.catalog
    }

    /// `server-side limits are off: …` when the context asked for a
    /// server-enforced identity and `namespace` fell back to client-only
    /// limits. The UI must show it where it cannot be missed (#197).
    pub fn server_limits_warning(&self, namespace: &str) -> Option<String> {
        self.boundary(namespace)
            .and_then(|boundary| boundary.degraded.clone())
    }

    /// Every such warning of the session, as `namespace: warning`.
    pub fn server_limits_warnings(&self) -> Vec<String> {
        sync::lock(&self.boundaries)
            .iter()
            .filter_map(|(namespace, boundary)| {
                boundary
                    .degraded
                    .as_ref()
                    .map(|warning| format!("{namespace}: {warning}"))
            })
            .collect()
    }

    /// Whether the user's own RBAC allows `access` (its namespace set):
    /// `SelfSubjectAccessReview` under the user's credentials, cached for
    /// the session. A review that fails counts as allowed: the API server
    /// still enforces the real rights. Raising the agent's ceiling never
    /// adds RBAC rights, so a no here is not a question for the user.
    pub async fn user_can(&self, access: &ResourceAccess) -> bool {
        if let Some(allowed) = &self.test_rbac {
            return Capability::ALL
                .iter()
                .find(|capability| {
                    let known = capability.access("");
                    known.verb == access.verb
                        && known.group == access.group
                        && known.full_resource() == access.full_resource()
                })
                .is_none_or(|capability| allowed.contains(capability));
        }
        if let Some(known) = sync::lock(&self.rbac).get(access) {
            return *known;
        }
        match can_i(&self.user_client, access).await {
            Ok(decision) => {
                sync::lock(&self.rbac).insert(access.clone(), decision.allowed);
                decision.allowed
            }
            Err(_) => true,
        }
    }

    /// The user's answers about access so far (#15).
    pub fn raises(&self) -> Raises {
        sync::lock(&self.raises).clone()
    }

    /// What the user allowed beyond the minimum in this session, if
    /// anything.
    pub fn raised(&self) -> Option<Access> {
        let granted = self.raises().granted;
        (granted != Access::NONE).then_some(granted)
    }

    /// What the session has without asking at its start: the context's
    /// minimum, capped by the launch's profile.
    pub fn minimum(&self) -> Access {
        self.scope.policy.minimum.capped(self.scope.profile)
    }

    /// The access now: the minimum, raised by what the user allowed,
    /// capped by the launch's profile. The Role of a temporary
    /// ServiceAccount mirrors it.
    pub fn access(&self) -> Access {
        self.scope
            .policy
            .minimum
            .join(self.raises().granted)
            .capped(self.scope.profile)
    }

    /// Records a tool call of `client`.
    pub fn note_call(&self, client: &str) {
        let mut activity = sync::lock(&self.activity);
        if !activity.clients.iter().any(|known| known == client) {
            activity.clients.push(client.to_owned());
        }
        activity.calls += 1;
        activity.last_call = Some(Timestamp::now());
    }

    pub fn activity(&self) -> Activity {
        sync::lock(&self.activity).clone()
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub fn is_active(&self) -> bool {
        !self.is_closed() && !self.scope.expired()
    }

    /// The policy for the next call.
    pub fn policy(&self) -> Policy<'_> {
        Policy::new(
            &self.scope,
            &self.catalog,
            self.is_closed(),
            self.grants(),
            self.modes(),
        )
        .with_raises(self.raises())
    }

    /// Records that the agent created or changed `kind` `name` in
    /// `namespace` (#190).
    pub fn note_change(&self, namespace: &str, kind: &str, name: &str) {
        sync::lock(&self.changed).insert((namespace.to_owned(), kind.to_owned(), name.to_owned()));
    }

    /// Whether the agent changed anything in `namespace` in this session.
    pub fn changed_in(&self, namespace: &str) -> bool {
        sync::lock(&self.changed)
            .iter()
            .any(|(ns, _, _)| ns == namespace)
    }

    /// Whether the agent created or changed `kind` `name` in `namespace`.
    pub fn was_changed(&self, namespace: &str, kind: &str, name: &str) -> bool {
        sync::lock(&self.changed).contains(&(
            namespace.to_owned(),
            kind.to_owned(),
            name.to_owned(),
        ))
    }

    /// The user's answers so far.
    pub fn grants(&self) -> Grants {
        sync::lock(&self.grants).clone()
    }

    /// Held while the user is asked for a right; see [`Session::decide`].
    pub async fn asking(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.ask.lock().await
    }

    /// Records the answer for `grant`: given within the access (#15) or by
    /// the user (revealing a Secret). Granting `read(ns)` first sets up the
    /// namespace's identity (#401); a grant patches the Role of a temporary
    /// ServiceAccount. If either fails the right is not granted and the
    /// error is returned (#227, variant A). A no is kept so the user is not
    /// asked again.
    pub async fn decide(&self, grant: Grant, granted: bool) -> Result<(), SessionError> {
        self.decide_with(grant, granted, false).await
    }

    /// Sets up a pinned namespace from the start, because the launch
    /// arguments say so (`kubach mcp --grant-read`). Only `read(ns)`, which
    /// the minimum gives anyway since #15, and only for the session's
    /// pinned namespaces.
    pub async fn grant_read_at_launch(&self, namespace: &str) -> Result<(), SessionError> {
        if !self.scope.namespaces.iter().any(|n| n == namespace) {
            return Err(SessionError::NotInSession(namespace.to_owned()));
        }
        self.decide_with(Grant::read(namespace), true, true).await
    }

    async fn decide_with(
        &self,
        grant: Grant,
        granted: bool,
        at_launch: bool,
    ) -> Result<(), SessionError> {
        let _role = self.role.lock().await;
        if !self.is_active() {
            return Err(SessionError::Ended);
        }
        if granted && let Grant::Read { namespace } = &grant {
            self.admit(namespace)?;
            self.prepare(namespace).await?;
        }
        let state = if granted {
            GrantState::Granted
        } else {
            GrantState::Denied
        };
        let mut next = self.grants();
        next.set(grant.clone(), state);
        if at_launch {
            next.mark_at_launch(grant.clone());
        }
        if granted {
            self.sync_namespace(&next, grant.namespace()).await?;
        }
        *sync::lock(&self.grants) = next;
        Ok(())
    }

    /// Whether reading `namespace` may be granted at all: a pinned session
    /// keeps to its namespaces. The policy checks the same first.
    fn admit(&self, namespace: &str) -> Result<(), SessionError> {
        if self.scope.pinned() && !self.scope.namespaces.iter().any(|n| n == namespace) {
            return Err(SessionError::NotInSession(namespace.to_owned()));
        }
        Ok(())
    }

    /// Sets up the identity of `namespace` unless it has one: ADR 0005's
    /// choice, done per namespace.
    async fn prepare(&self, namespace: &str) -> Result<(), SessionError> {
        if self.boundary(namespace).is_some() {
            return Ok(());
        }
        let base = match &self.identities {
            Identities::Cluster { base } => base,
            #[cfg(test)]
            Identities::Fixed(_) => return Ok(()),
        };
        if self.scope.policy.temporary_objects {
            // Leftovers of crashed sessions (ADR 0005); best effort.
            let _ = identity::sweep_expired(&self.user_client, namespace).await;
        }
        let (identity, notes) = choose_identity(
            base,
            &self.user_client,
            &self.scope,
            self.access().profile,
            namespace,
        )
        .await;
        let degraded = server_limits_warning(identity.mode(), &notes, &self.scope.policy);
        sync::lock(&self.boundaries).insert(
            namespace.to_owned(),
            Arc::new(Boundary {
                identity,
                notes,
                degraded,
            }),
        );
        Ok(())
    }

    /// Mirrors `grants` on the identity of `namespace`, within the access
    /// as raised so far.
    async fn sync_namespace(&self, grants: &Grants, namespace: &str) -> Result<(), SessionError> {
        if let Some(boundary) = self.boundary(namespace) {
            boundary.identity.sync_grants(grants, self.access()).await?;
        }
        Ok(())
    }

    /// The user's answer about chart source `source` in this session, if
    /// asked (#418).
    pub fn chart_source(&self, source: &str) -> Option<bool> {
        sync::lock(&self.chart_sources).get(source).copied()
    }

    /// Records the user's answer about chart source `source`.
    pub fn set_chart_source(&self, source: &str, allowed: bool) {
        sync::lock(&self.chart_sources).insert(source.to_owned(), allowed);
    }

    /// How a `helm` process acts in `namespace` (#418). Refused for a
    /// namespace without an identity, as every other call.
    pub async fn helm_access(&self, namespace: &str) -> Result<HelmAccess, SessionError> {
        let (boundary, base) = match &self.identities {
            Identities::Cluster { base } => (
                self.boundary(namespace)
                    .ok_or_else(|| SessionError::NotInSession(namespace.to_owned()))?,
                base,
            ),
            #[cfg(test)]
            Identities::Fixed(_) => return Ok(HelmAccess::User),
        };
        Ok(match &boundary.identity {
            Identity::ClientOnly { .. } => HelmAccess::User,
            Identity::Impersonation { user, groups, .. } => HelmAccess::Impersonate {
                user: user.clone(),
                groups: groups.clone(),
            },
            Identity::TemporaryServiceAccount(sa) => HelmAccess::Token {
                kubeconfig: identity::cluster_kubeconfig(base),
                token: sa.helm_token().await?,
            },
        })
    }

    /// Widens the Role of the namespace's temporary ServiceAccount with
    /// `extra` while a `helm` call the user approved runs (#418); a no-op
    /// for other identities. [`Session::restore_role`] puts the granted
    /// rules back.
    pub async fn widen_role(
        &self,
        namespace: &str,
        extra: Vec<k8s_openapi::api::rbac::v1::PolicyRule>,
    ) -> Result<(), SessionError> {
        let _role = self.role.lock().await;
        if !self.is_active() {
            return Err(SessionError::Ended);
        }
        if let Some(boundary) = self.boundary(namespace)
            && let Identity::TemporaryServiceAccount(sa) = &boundary.identity
        {
            let mut rules = crate::rules::grant_rules(&self.grants(), namespace, self.access());
            rules.extend(extra);
            sa.set_rules(rules).await?;
        }
        Ok(())
    }

    /// The Role of `namespace` back to exactly the granted rights.
    pub async fn restore_role(&self, namespace: &str) -> Result<(), SessionError> {
        let _role = self.role.lock().await;
        let grants = self.grants();
        self.sync_namespace(&grants, namespace).await
    }

    /// Records the user's answer about access `raise` (#15). A yes widens
    /// the Role of every temporary ServiceAccount the session has, so the
    /// API server allows the rights already set up in those namespaces up
    /// to the new access (`modify(ns)` still follows on the first change
    /// there). If a patch fails, the access is not raised and the error is
    /// returned; Roles already patched are put back. A no is kept so the
    /// same request is not asked again. `saved`: the user raised the
    /// context's minimum in the settings too.
    pub async fn decide_access(
        &self,
        raise: Access,
        granted: bool,
        saved: bool,
    ) -> Result<(), SessionError> {
        let _role = self.role.lock().await;
        if !self.is_active() {
            return Err(SessionError::Ended);
        }
        if !granted {
            sync::lock(&self.raises).refused.push(raise);
            return Ok(());
        }
        let before = self.raises();
        let mut next = before.clone();
        next.granted = next.granted.join(raise);
        next.saved |= saved;
        *sync::lock(&self.raises) = next;
        let grants = self.grants();
        let namespaces: Vec<String> = sync::lock(&self.boundaries).keys().cloned().collect();
        for namespace in &namespaces {
            if let Err(error) = self.sync_namespace(&grants, namespace).await {
                *sync::lock(&self.raises) = before;
                for namespace in &namespaces {
                    let _ = self.sync_namespace(&grants, namespace).await;
                }
                return Err(error);
            }
        }
        Ok(())
    }

    /// Records that the user approved writing or deleting Secret
    /// `namespace/name` (#230). A temporary ServiceAccount's Role gets
    /// `patch`/`delete` on that name first; if the patch fails, nothing is
    /// recorded and the error is returned.
    pub async fn allow_secret_write(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<(), SessionError> {
        let _role = self.role.lock().await;
        if !self.is_active() {
            return Err(SessionError::Ended);
        }
        let mut next = self.grants();
        if !next.add_secret_write(namespace, name) {
            return Ok(());
        }
        self.sync_namespace(&next, namespace).await?;
        *sync::lock(&self.grants) = next;
        Ok(())
    }

    /// Takes `grant` back (and the rights standing on it). Kubach stops
    /// allowing it at once; the Role patch follows, and its error, if any,
    /// is returned for the UI.
    pub async fn revoke(&self, grant: &Grant) -> Result<(), SessionError> {
        let _role = self.role.lock().await;
        let next = {
            let mut grants = sync::lock(&self.grants);
            grants.revoke(grant);
            grants.clone()
        };
        // Forwards stand on reading their namespace (#17).
        if let Grant::Read { namespace } = grant {
            self.forwards.stop_in(namespace, StopReason::Revoked);
        }
        if self.is_active() {
            self.sync_namespace(&next, grant.namespace()).await?;
        }
        Ok(())
    }

    /// The agent's port-forwards (#17).
    pub fn forwards(&self) -> &SessionForwards {
        &self.forwards
    }

    /// Before a port-forward in `namespace` starts (#17): a temporary
    /// ServiceAccount's Role gets `pods/portforward` there. If the patch
    /// fails, nothing is recorded and the error is returned.
    pub(crate) async fn begin_forward(&self, namespace: &str) -> Result<(), SessionError> {
        let _role = self.role.lock().await;
        if !self.is_active() {
            return Err(SessionError::Ended);
        }
        let mut next = self.grants();
        if !next.set_forwarding(namespace, true) {
            return Ok(());
        }
        self.sync_namespace(&next, namespace).await?;
        *sync::lock(&self.grants) = next;
        Ok(())
    }

    /// After the last port-forward in `namespace` stopped: the Role loses
    /// `pods/portforward` again. Best effort; the Role goes with the
    /// session anyway.
    pub(crate) async fn end_forward(&self, namespace: &str) {
        let _role = self.role.lock().await;
        if self.forwards.any_in(namespace) {
            return;
        }
        let next = {
            let mut grants = sync::lock(&self.grants);
            if !grants.set_forwarding(namespace, false) {
                return;
            }
            grants.clone()
        };
        if self.is_active() {
            let _ = self.sync_namespace(&next, namespace).await;
        }
    }

    /// Notices forwards the user stopped in the app, once a forward runs:
    /// they are audited, and the Role follows (#17).
    pub(crate) fn watch_forwards(&self) {
        let me = self.me.clone();
        self.forwards.watch_with(|manager| {
            let mut changes = manager.subscribe();
            kubach_core::runtime::spawn(async move {
                use futures::StreamExt as _;
                while changes.next().await.is_some() {
                    while changes.try_recv().is_ok() {}
                    let Some(session) = me.upgrade() else {
                        break;
                    };
                    for namespace in session.forwards.reconcile() {
                        session.end_forward(&namespace).await;
                    }
                }
            })
        });
    }

    /// Sets answers directly, skipping identities and the Role. For tests
    /// of other layers.
    #[cfg(test)]
    pub(crate) fn set_grants(&self, grants: Grants) {
        *sync::lock(&self.grants) = grants;
    }

    /// Ends the session: later calls are denied and server-side rights are
    /// revoked in every namespace. Idempotent.
    pub async fn close(&self) -> Result<(), SessionError> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.forwards.stop_all(StopReason::SessionEnded);
        let boundaries: Vec<Arc<Boundary>> =
            sync::lock(&self.boundaries).values().cloned().collect();
        let mut first_error = None;
        for boundary in boundaries {
            if let Err(error) = boundary.identity.end().await {
                first_error.get_or_insert(error);
            }
        }
        match first_error {
            Some(error) => Err(error.into()),
            None => Ok(()),
        }
    }
}

fn spawn_ttl_guard(session: Weak<Session>, deadline: std::time::Instant) {
    tokio::spawn(async move {
        tokio::time::sleep_until(deadline.into()).await;
        if let Some(session) = session.upgrade() {
            let _ = session.close().await;
        }
    });
}

async fn allowed(client: &Client, accesses: &[ResourceAccess]) -> Result<(), String> {
    let mut denied = Vec::new();
    for (access, result) in accesses.iter().zip(can_i_all(client, accesses).await) {
        match result {
            Ok(decision) if decision.allowed => {}
            Ok(_) => denied.push(format!("{} {}", access.verb, access.full_resource())),
            Err(error) => return Err(error.to_string()),
        }
    }
    if denied.is_empty() {
        Ok(())
    } else {
        Err(format!("missing rights: {}", denied.join(", ")))
    }
}

/// ADR 0005 for `namespace`: impersonation if configured and permitted,
/// else a temporary SA if the user's RBAC allows it, else client only.
/// `profile` names the impersonated identity.
async fn choose_identity(
    base: &Config,
    user_client: &Client,
    scope: &SessionScope,
    profile: crate::settings::Profile,
    namespace: &str,
) -> (Identity, Vec<String>) {
    let mut notes = Vec::new();
    let policy = &scope.policy;

    if let Some(target) = &policy.impersonation {
        let (user, groups) = target.render(namespace, profile);
        let mut accesses = vec![ResourceAccess::new("impersonate", "", "users").named(&user)];
        accesses.extend(
            groups
                .iter()
                .map(|g| ResourceAccess::new("impersonate", "", "groups").named(g)),
        );
        match allowed(user_client, &accesses).await {
            Ok(_) => {
                let extra = ResourceAccess::new(
                    "impersonate",
                    "authentication.k8s.io",
                    "userextras/kubach.io/session",
                )
                .named(&scope.id);
                let with_extra = matches!(can_i(user_client, &extra).await, Ok(d) if d.allowed);
                if !with_extra {
                    notes.push("impersonation without the kubach.io/session extra (no right to impersonate userextras)".into());
                }
                match identity::impersonation_config(base, &scope.id, &user, &groups, with_extra)
                    .and_then(|config| {
                        kubach_core::redact::client(config).map_err(IdentityError::from)
                    }) {
                    Ok(client) => {
                        return (
                            Identity::Impersonation {
                                client,
                                user,
                                groups,
                            },
                            notes,
                        );
                    }
                    Err(error) => notes.push(format!("impersonation failed: {error}")),
                }
            }
            Err(reason) => notes.push(format!("impersonation of {user} not possible: {reason}")),
        }
    }

    if policy.temporary_objects {
        match allowed(user_client, &temporary_sa_requirements(namespace)).await {
            Ok(_) => {
                match TemporaryServiceAccount::create(user_client, base, scope, namespace).await {
                    Ok(sa) => return (Identity::TemporaryServiceAccount(sa), notes),
                    Err(error) => notes.push(format!("temporary ServiceAccount failed: {error}")),
                }
            }
            Err(reason) => notes.push(format!("temporary ServiceAccount not possible: {reason}")),
        }
    } else {
        notes.push("temporary objects are off for this session".into());
    }

    (
        Identity::ClientOnly {
            client: user_client.clone(),
        },
        notes,
    )
}

/// The mark of a client-only namespace (#197, #15): `server-side limits
/// are off: <why>` when a server-enforced identity was tried and did not
/// work (no RBAC for a temporary ServiceAccount, broken impersonation).
/// Nothing when none was tried (tests only; the UI marks client-only
/// namespaces anyway).
fn server_limits_warning(
    mode: SessionMode,
    notes: &[String],
    policy: &SecurityPolicy,
) -> Option<String> {
    if mode.server_enforced() {
        return None;
    }
    let reason = notes.join("; ");
    let tried = policy.impersonation.is_some() || policy.temporary_objects;
    tried.then(|| format!("server-side limits are off: {reason}"))
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;

    use super::*;
    use crate::settings::{ImpersonationTarget, Profile};
    use crate::testutil::{NS, scope_with};

    /// An API server that answers every `SelfSubjectAccessReview` with
    /// `allowed`. Returns its URL.
    async fn fake_ssar(allowed: bool) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let (read, mut write) = stream.into_split();
                    let mut reader = BufReader::new(read);
                    let mut length = 0usize;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                            return;
                        }
                        if line == "\r\n" {
                            break;
                        }
                        if let Some((name, value)) = line.split_once(':')
                            && name.eq_ignore_ascii_case("content-length")
                        {
                            length = value.trim().parse().unwrap_or(0);
                        }
                    }
                    let mut body = vec![0; length];
                    let _ = reader.read_exact(&mut body).await;
                    let answer = serde_json::json!({
                        "apiVersion": "authorization.k8s.io/v1",
                        "kind": "SelfSubjectAccessReview",
                        "metadata": {},
                        "spec": {},
                        "status": {"allowed": allowed},
                    })
                    .to_string();
                    let response = format!(
                        "HTTP/1.1 201 Created\r\ncontent-type: application/json\r\n\
                         content-length: {}\r\nconnection: close\r\n\r\n{answer}",
                        answer.len()
                    );
                    let _ = write.write_all(response.as_bytes()).await;
                });
            }
        });
        url
    }

    /// A URL nothing listens on: every SSAR fails.
    fn unreachable() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        url
    }

    fn impersonating() -> SecurityPolicy {
        SecurityPolicy {
            mcp_enabled: true,
            impersonation: Some(ImpersonationTarget {
                user: "kubach:mcp:{namespace}:{profile}".into(),
                groups: Vec::new(),
            }),
            temporary_objects: false,
            ..SecurityPolicy::default()
        }
    }

    /// Runs the identity choice against `url`, as [`Session::start`] does.
    async fn start_identity(url: &str, policy: &SecurityPolicy) -> (SessionMode, Option<String>) {
        let base = Config::new(url.parse().unwrap());
        let user = Client::try_from(base.clone()).unwrap();
        let scope = scope_with(Profile::Deploy, policy, &[]);
        let (identity, notes) = choose_identity(&base, &user, &scope, Profile::ReadOnly, NS).await;
        let warning = server_limits_warning(identity.mode(), &notes, policy);
        (identity.mode(), warning)
    }

    #[tokio::test]
    async fn broken_impersonation_starts_with_a_warning() {
        let url = fake_ssar(false).await;
        let (mode, warning) = start_identity(&url, &impersonating()).await;
        assert_eq!(mode, SessionMode::ClientOnly);
        let warning = warning.expect("the fallback is not silent");
        assert!(
            warning.starts_with("server-side limits are off: "),
            "{warning}"
        );
        assert!(
            warning.contains("impersonation of kubach:mcp:kubach-test-mcp:read-only not possible"),
            "{warning}"
        );
    }

    #[tokio::test]
    async fn without_rbac_for_a_service_account_the_namespace_is_marked() {
        // The automatic choice (#15): no rights for a temporary
        // ServiceAccount, so limits only in Kubach, and that is not silent.
        let (mode, warning) = start_identity(&unreachable(), &SecurityPolicy::default()).await;
        assert_eq!(mode, SessionMode::ClientOnly);
        let warning = warning.expect("marked");
        assert!(
            warning.starts_with("server-side limits are off: "),
            "{warning}"
        );
        assert!(
            warning.contains("temporary ServiceAccount not possible"),
            "{warning}"
        );
    }

    #[tokio::test]
    async fn working_impersonation_needs_no_mark() {
        let url = fake_ssar(true).await;
        let (mode, warning) = start_identity(&url, &impersonating()).await;
        assert_eq!(mode, SessionMode::Impersonation);
        assert_eq!(warning, None);
    }

    /// A session on the API server at `url` started without namespaces,
    /// as [`Session::start`] builds it, minus discovery.
    fn open_session(url: &str, policy: &SecurityPolicy) -> Arc<Session> {
        let base = Config::new(url.parse().unwrap());
        let user_client = Client::try_from(base.clone()).unwrap();
        let scope = scope_with(Profile::Deploy, policy, &[]);
        let forwards = SessionForwards::new(&scope.id);
        Arc::new_cyclic(|me| Session {
            scope,
            identities: Identities::Cluster {
                base: Box::new(base),
            },
            user_client,
            boundaries: Mutex::default(),
            catalog: crate::testutil::catalog(),
            test_rbac: None,
            rbac: Mutex::default(),
            raises: Mutex::default(),
            chart_sources: Mutex::default(),
            closed: AtomicBool::new(false),
            activity: Mutex::default(),
            grants: Mutex::default(),
            ask: tokio::sync::Mutex::new(()),
            role: tokio::sync::Mutex::new(()),
            changed: Mutex::default(),
            forwards,
            me: me.clone(),
        })
    }

    #[tokio::test]
    async fn each_new_namespace_gets_its_own_identity() {
        let url = fake_ssar(true).await;
        let session = open_session(&url, &impersonating());
        // Nothing is set up before the agent works in a namespace.
        assert!(session.boundaries().is_empty());
        assert!(session.client_in("web").is_err());
        assert_eq!(session.describe_identity(Some("web")), "user");

        session.decide(Grant::read("web"), true).await.unwrap();
        session.decide(Grant::read("api"), true).await.unwrap();
        let boundaries = session.boundaries();
        assert_eq!(boundaries.len(), 2);
        // The administrator's identity of each namespace (ADR 0005).
        assert_eq!(boundaries[0].namespace, "api");
        assert_eq!(
            boundaries[0].identity,
            "impersonated:kubach:mcp:api:read-only"
        );
        assert_eq!(
            boundaries[1].identity,
            "impersonated:kubach:mcp:web:read-only"
        );
        assert!(boundaries.iter().all(BoundaryInfo::server_enforced));
        assert_eq!(session.mode_in("web"), SessionMode::Impersonation);
        assert_eq!(session.mode(), SessionMode::Impersonation);
        assert!(session.client_in("web").is_ok());
        assert_eq!(session.namespaces(), ["api", "web"]);
        // A refusal sets up nothing.
        session.decide(Grant::read("db"), false).await.unwrap();
        assert!(session.client_in("db").is_err());
    }

    #[tokio::test]
    async fn a_client_only_namespace_is_marked() {
        let url = fake_ssar(false).await;
        let session = open_session(&url, &impersonating());
        session.decide(Grant::read("web"), true).await.unwrap();
        let boundaries = session.boundaries();
        assert_eq!(boundaries[0].mode, SessionMode::ClientOnly);
        let warning = boundaries[0].warning.clone().expect("not silent");
        assert!(
            warning.starts_with("server-side limits are off: "),
            "{warning}"
        );
        assert_eq!(session.server_limits_warning("web"), Some(warning.clone()));
        assert_eq!(
            session.server_limits_warnings(),
            [format!("web: {warning}")]
        );
        assert_eq!(session.describe_identity(Some("web")), "user");
    }

    #[tokio::test]
    async fn any_number_of_namespaces() {
        let url = fake_ssar(true).await;
        let session = open_session(&url, &impersonating());
        for namespace in ["web", "api", "db"] {
            session.decide(Grant::read(namespace), true).await.unwrap();
        }
        assert_eq!(session.namespaces(), ["api", "db", "web"]);
    }

    #[test]
    fn server_enforced_modes_need_no_warning() {
        for mode in [
            SessionMode::Impersonation,
            SessionMode::TemporaryServiceAccount,
        ] {
            assert_eq!(
                server_limits_warning(mode, &[], &SecurityPolicy::default()),
                None
            );
        }
    }
}
