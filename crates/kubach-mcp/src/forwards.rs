//! Port-forwards the agent opens (#17): `port_forward_start`,
//! `port_forward_list` and `port_forward_stop`.
//!
//! The forwards run in the same [`PortForwardManager`] as the user's: in
//! the app it is the manager of the "Port forwards" screen (the app hands
//! it over with [`SessionForwards::set_manager`]), so the user sees every
//! agent's forward there, marked with the client and session, and can
//! stop it; `kubach mcp` without the app keeps its own. Every forward
//! listens on `127.0.0.1` only and tunnels TCP only, as the manager does.
//!
//! A forward needs the debug level and `read(ns)`, passes the user's RBAC
//! (`pods/portforward`) and is confirmed in the window, one by one like
//! exec. A temporary ServiceAccount's Role gets `pods/portforward` only
//! while a forward runs in its namespace. Forwards end with the session
//! (TTL, closing, the app quitting), with `read(ns)` revoked, or when the
//! user stops them; each start and stop is in the audit log.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use k8s_openapi::jiff::Timestamp;
use kubach_core::runtime::BackgroundTask;
use kubach_core::streams::{
    ForwardAgent, ForwardId, ForwardInfo, ForwardSource, ForwardState, LocalPort,
    PortForwardManager, StreamError, fetch_source,
};
use kubach_core::sync;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::audit::{AuditEntry, AuditLog, DecidedBy};
use crate::confirm::RequestKind;
use crate::policy::{Denial, DenialCode, ForwardPortRef, Request};
use crate::tools::{Ctx, ToolError, ToolOutput, authorize, confirm_as, ensure_rbac, parse};

/// Most forwards one session keeps open at a time.
pub const MAX_FORWARDS_PER_SESSION: usize = 8;

/// Why an agent's forward stopped, for the audit log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// The agent called `port_forward_stop`; the call is audited itself.
    Agent,
    /// The user stopped it on the "Port forwards" screen.
    User,
    /// The session ended: TTL, closed, revoked, the app quit.
    SessionEnded,
    /// The user revoked reading its namespace.
    Revoked,
}

impl StopReason {
    fn label(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::User => "user",
            Self::SessionEnded => "session-ended",
            Self::Revoked => "revoked",
        }
    }
}

/// What the session remembers of one of its forwards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentForward {
    pub namespace: String,
    /// `svc/web`, as [`ForwardInfo::source`].
    pub target: String,
    /// The remote port as the agent named it.
    pub port: String,
    /// The MCP client that opened it.
    pub client: String,
    /// The identity it runs under, for the audit log.
    pub identity: String,
}

/// The forwards of one session.
pub struct SessionForwards {
    session: String,
    manager: Mutex<PortForwardManager>,
    tracked: Mutex<BTreeMap<ForwardId, AgentForward>>,
    audit: Mutex<Option<Arc<AuditLog>>>,
    /// Notices forwards the user stopped; started with the first forward.
    watch: Mutex<Option<BackgroundTask>>,
    /// One start at a time, so the limit holds.
    starting: tokio::sync::Mutex<()>,
}

impl SessionForwards {
    pub(crate) fn new(session: &str) -> Self {
        Self {
            session: session.to_owned(),
            manager: Mutex::new(PortForwardManager::new()),
            tracked: Mutex::default(),
            audit: Mutex::default(),
            watch: Mutex::default(),
            starting: tokio::sync::Mutex::new(()),
        }
    }

    /// Runs the session's forwards in `manager` (the app's, so they show on
    /// the "Port forwards" screen). Only before the first forward; false
    /// afterwards.
    pub fn set_manager(&self, manager: PortForwardManager) -> bool {
        if !sync::lock(&self.tracked).is_empty() || sync::lock(&self.watch).is_some() {
            return false;
        }
        *sync::lock(&self.manager) = manager;
        true
    }

    pub fn manager(&self) -> PortForwardManager {
        sync::lock(&self.manager).clone()
    }

    /// Where stops outside a tool call are recorded.
    pub(crate) fn set_audit(&self, audit: Option<Arc<AuditLog>>) {
        *sync::lock(&self.audit) = audit;
    }

    /// The session's forwards still running, ordered by id.
    pub fn list(&self) -> Vec<(ForwardInfo, AgentForward)> {
        let manager = self.manager();
        sync::lock(&self.tracked)
            .iter()
            .filter_map(|(id, forward)| Some((manager.get(*id)?, forward.clone())))
            .collect()
    }

    pub fn count(&self) -> usize {
        sync::lock(&self.tracked).len()
    }

    /// Whether the session has a forward in `namespace`.
    pub fn any_in(&self, namespace: &str) -> bool {
        sync::lock(&self.tracked)
            .values()
            .any(|forward| forward.namespace == namespace)
    }

    fn track(&self, id: ForwardId, forward: AgentForward) {
        sync::lock(&self.tracked).insert(id, forward);
    }

    /// Stops forward `id` if it is this session's.
    pub fn stop(&self, id: ForwardId, reason: StopReason) -> Option<AgentForward> {
        let forward = sync::lock(&self.tracked).remove(&id)?;
        self.manager().stop(id);
        self.record_stop(id, &forward, reason);
        Some(forward)
    }

    /// Stops the session's forwards in `namespace`.
    pub fn stop_in(&self, namespace: &str, reason: StopReason) {
        let ids: Vec<ForwardId> = sync::lock(&self.tracked)
            .iter()
            .filter(|(_, forward)| forward.namespace == namespace)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.stop(id, reason);
        }
    }

    /// Stops every forward of the session.
    pub fn stop_all(&self, reason: StopReason) {
        let ids: Vec<ForwardId> = sync::lock(&self.tracked).keys().copied().collect();
        for id in ids {
            self.stop(id, reason);
        }
        // Nothing left to watch.
        sync::lock(&self.watch).take();
    }

    /// Forgets forwards that are no longer in the manager (the user
    /// stopped them) and records it. Returns their namespaces.
    pub(crate) fn reconcile(&self) -> Vec<String> {
        let manager = self.manager();
        let gone: Vec<(ForwardId, AgentForward)> = {
            let mut tracked = sync::lock(&self.tracked);
            let ids: Vec<ForwardId> = tracked
                .keys()
                .copied()
                .filter(|id| manager.get(*id).is_none())
                .collect();
            ids.into_iter()
                .filter_map(|id| Some((id, tracked.remove(&id)?)))
                .collect()
        };
        gone.into_iter()
            .map(|(id, forward)| {
                self.record_stop(id, &forward, StopReason::User);
                forward.namespace
            })
            .collect()
    }

    /// Starts watching the manager for forwards the user stops, once.
    pub(crate) fn watch_with(&self, start: impl FnOnce(&PortForwardManager) -> BackgroundTask) {
        let mut watch = sync::lock(&self.watch);
        if watch.is_none() {
            *watch = Some(start(&self.manager()));
        }
    }

    fn record_stop(&self, id: ForwardId, forward: &AgentForward, reason: StopReason) {
        if reason == StopReason::Agent {
            return;
        }
        let Some(audit) = sync::lock(&self.audit).clone() else {
            return;
        };
        let (decided_by, why) = match reason {
            StopReason::User => (DecidedBy::User, "stopped by the user in Kubach"),
            StopReason::Revoked => (
                DecidedBy::User,
                "the user revoked reading the namespace of this forward",
            ),
            StopReason::SessionEnded | StopReason::Agent => (DecidedBy::None, "the session ended"),
        };
        let _ = audit.record(&AuditEntry {
            time: Timestamp::now().to_string(),
            session: self.session.clone(),
            client: forward.client.clone(),
            tool: "port_forward_stop".into(),
            args: json!({
                "id": id,
                "namespace": forward.namespace,
                "target": forward.target,
                "port": forward.port,
                "by": reason.label(),
            }),
            change: None,
            decided_by,
            remembered: None,
            allowed: true,
            reason: Some(why.into()),
            identity: forward.identity.clone(),
            result: "stopped".into(),
            duration_ms: 0,
        });
    }
}

impl Drop for SessionForwards {
    /// A session dropped without closing still takes its forwards along:
    /// the app's manager outlives it.
    fn drop(&mut self) {
        let manager = self.manager();
        for id in sync::lock(&self.tracked).keys() {
            manager.stop(*id);
        }
    }
}

// ------------------------------------------------------------- tools

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StartArgs {
    namespace: Option<String>,
    target: String,
    port: ForwardPortRef,
    local_port: Option<u16>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StopArgs {
    id: ForwardId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NoArgs {}

/// The pod port number `port` names on `source`: a number as it is, a
/// name looked up among the source's TCP ports.
fn port_number(source: &ForwardSource, port: &ForwardPortRef) -> Result<u16, ToolError> {
    match port {
        ForwardPortRef::Number(number) => Ok(*number),
        ForwardPortRef::Name(name) => source
            .ports
            .iter()
            .find(|p| p.name.as_deref() == Some(name.as_str()) && p.is_tcp())
            .map(|p| p.port)
            .ok_or_else(|| {
                Denial::new(
                    DenialCode::InvalidInput,
                    format!("{} has no TCP port named {name}", source.label()),
                )
                .into()
            }),
    }
}

fn stream_error(error: StreamError) -> ToolError {
    match error {
        StreamError::Api(error) => ToolError::from(*error),
        other => ToolError::Failed(other.to_string()),
    }
}

/// `port_forward_start`.
pub(crate) async fn start(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    let a: StartArgs = parse(args)?;
    let request = Request::PortForward {
        namespace: a.namespace.clone(),
        target: a.target.clone(),
        port: a.port.clone(),
        local_port: a.local_port,
    };
    let allowed = authorize(ctx, &request).await?;
    let namespace = allowed.namespace.clone();
    ensure_rbac(ctx, &namespace, &[("create", "", "pods/portforward")]).await?;
    let forwards = ctx.session.forwards();
    let _starting = forwards.starting.lock().await;
    if forwards.count() >= MAX_FORWARDS_PER_SESSION {
        return Err(Denial::new(
            DenialCode::InvalidInput,
            format!(
                "this session has {MAX_FORWARDS_PER_SESSION} port-forwards open, the most it may; \
                 stop one with port_forward_stop"
            ),
        )
        .into());
    }
    let kind = allowed
        .kind
        .as_ref()
        .expect("policy resolved the kind")
        .api
        .kind
        .clone();
    let name = allowed.name.clone().expect("policy validated the name");
    let target = format!("{}/{name}", kind.to_ascii_lowercase());
    let local = a
        .local_port
        .map_or_else(|| "any free port".to_owned(), |port| port.to_string());
    let scope = ctx.session.scope();
    confirm_as(
        ctx,
        RequestKind::PortForward {
            target: target.clone(),
            port: a.port.to_string(),
            local_port: a.local_port,
        },
        &allowed,
        "port_forward_start",
        format!(
            "Forward 127.0.0.1 to {target} port {} in {namespace}",
            a.port
        ),
        format!(
            "Namespace: {namespace}\nTarget: {target}\nRemote port: {}\nLocal: 127.0.0.1, {local}\n\
             Session: {} ({}, context {})\nOnly this machine can connect. The forward ends with \
             the session; you can stop it any time under Port forwards.",
            a.port,
            ctx.session.id(),
            ctx.client_name,
            scope.context,
        ),
        Vec::new(),
    )
    .await?;
    ctx.session.begin_forward(&namespace).await?;
    let started = open(ctx, &kind, &name, &namespace, &a).await;
    let info = match started {
        Ok(info) => info,
        Err(error) => {
            ctx.session.end_forward(&namespace).await;
            return Err(error);
        }
    };
    // Defense in depth: the manager binds 127.0.0.1 only.
    if !info.local_addr.ip().is_loopback() {
        forwards.manager().stop(info.id);
        ctx.session.end_forward(&namespace).await;
        return Err(ToolError::Failed(
            "the forward did not listen on 127.0.0.1; stopped".into(),
        ));
    }
    forwards.track(
        info.id,
        AgentForward {
            namespace: namespace.clone(),
            target: info.source.clone(),
            port: a.port.to_string(),
            client: ctx.client_name.to_owned(),
            identity: ctx.session.describe_identity(Some(&namespace)),
        },
    );
    ctx.session.watch_forwards();
    // The session may have ended or lost the namespace meanwhile.
    if !ctx.session.is_active() || ctx.session.policy().check(&request).is_err() {
        forwards.stop(info.id, StopReason::SessionEnded);
        ctx.session.end_forward(&namespace).await;
        return Err(Denial::new(
            DenialCode::SessionEnded,
            "the session ended or lost this namespace while the forward started",
        )
        .into());
    }
    let address = info.local_addr.to_string();
    let result = json!({
        "id": info.id,
        "address": address,
        "namespace": info.namespace,
        "target": info.source,
        "port": info.source_port,
        "pod": info.pod,
        "podPort": info.remote_port,
    });
    Ok(ToolOutput {
        text: format!(
            "Forwarding {address} to {}/{}:{} (pod {}, port {}). Connect to {address} from this \
             machine; stop it with port_forward_stop id {}.\n{result}",
            info.namespace, info.source, info.source_port, info.pod, info.remote_port, info.id
        ),
        confirmed: true,
        audit: Some(json!({"forward": info.id, "address": address, "target": info.source})),
    })
}

/// Reads the target under the session's identity and starts the forward.
async fn open(
    ctx: &Ctx<'_>,
    kind: &str,
    name: &str,
    namespace: &str,
    a: &StartArgs,
) -> Result<ForwardInfo, ToolError> {
    let client = ctx.session.client_in(namespace)?;
    let source = fetch_source(&client, kind, namespace, name)
        .await
        .map_err(stream_error)?;
    let port = port_number(&source, &a.port)?;
    let local = LocalPort::Exact(a.local_port.unwrap_or(0));
    let agent = ForwardAgent {
        context: ctx.session.scope().context.clone(),
        client: ctx.client_name.to_owned(),
        session: ctx.session.id().to_owned(),
    };
    ctx.session
        .forwards()
        .manager()
        .start_for_agent(&client, &source, port, local, agent)
        .await
        .map_err(|error| match error {
            StreamError::PortInUse(port) => ToolError::Failed(format!(
                "local port {port} is taken; leave localPort out for any free port"
            )),
            other => stream_error(other),
        })
}

fn state(info: &ForwardInfo) -> &'static str {
    match info.state {
        _ if info.paused => "paused",
        ForwardState::Ready => "ready",
        ForwardState::Failing => "failing",
        ForwardState::Reconnecting => "reconnecting",
    }
}

/// `port_forward_list`: this session's forwards, never anyone else's.
pub(crate) async fn list(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    parse::<NoArgs>(args)?;
    authorize(ctx, &Request::Info).await?;
    let forwards: Vec<Value> = ctx
        .session
        .forwards()
        .list()
        .into_iter()
        .map(|(info, _)| {
            json!({
                "id": info.id,
                "address": info.local_addr.to_string(),
                "namespace": info.namespace,
                "target": info.source,
                "port": info.source_port,
                "pod": info.pod,
                "podPort": info.remote_port,
                "state": state(&info),
                "activeConnections": info.active_connections,
                "totalConnections": info.total_connections,
                "lastError": info.last_error,
            })
        })
        .collect();
    let text = if forwards.is_empty() {
        "No port-forwards in this session.".to_owned()
    } else {
        serde_json::to_string_pretty(&forwards).unwrap_or_default()
    };
    Ok(ToolOutput::read(text))
}

/// `port_forward_stop`: one of this session's forwards.
pub(crate) async fn stop(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    let a: StopArgs = parse(args)?;
    authorize(ctx, &Request::Info).await?;
    let Some(forward) = ctx.session.forwards().stop(a.id, StopReason::Agent) else {
        return Err(Denial::new(
            DenialCode::InvalidInput,
            format!(
                "this session has no port-forward {}; port_forward_list shows its forwards",
                a.id
            ),
        )
        .into());
    };
    ctx.session.end_forward(&forward.namespace).await;
    *sync::lock(&ctx.namespace) = Some(forward.namespace.clone());
    Ok(ToolOutput {
        text: format!(
            "Stopped port-forward {} to {}/{}.",
            a.id, forward.namespace, forward.target
        ),
        confirmed: false,
        audit: Some(json!({"forward": a.id, "target": forward.target})),
    })
}

#[cfg(test)]
mod tests;
