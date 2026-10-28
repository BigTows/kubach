//! The MCP server handler (`rmcp`): tool catalog filtered by profile,
//! settings and RBAC, and the audited call path.

use std::sync::Arc;
use std::time::Instant;

use k8s_openapi::jiff::Timestamp;
use kubach_core::plugin::PluginRegistry;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::{Map, Value, json};

use crate::access::Access;
use crate::audit::{AuditEntry, AuditLog, DecidedBy};
use crate::confirm::Confirmer;
use crate::grants::Grant;
use crate::helm_tools::HelmEnv;
use crate::masking::mask_audit_args;
use crate::rules::writes_secrets;
use crate::session::Session;
use crate::settings::Profile;
use crate::tools::{self, AccessDecision, Ctx, GrantDecision, ToolError};
use kubach_core::sync;

/// Kubach's MCP server for one session. Cheap to clone.
#[derive(Clone)]
pub struct McpServer {
    inner: Arc<Inner>,
}

struct Inner {
    session: Arc<Session>,
    registry: PluginRegistry,
    confirmer: Arc<dyn Confirmer>,
    audit: Option<Arc<AuditLog>>,
    /// The user's `helm` (#418); without it the Helm tools are not offered.
    helm: std::sync::RwLock<Option<Arc<HelmEnv>>>,
}

/// Outcome of one call, as the agent sees it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CallOutcome {
    pub text: String,
    pub is_error: bool,
}

impl McpServer {
    /// `confirmer` decides mutating calls ([`DenyAll`](crate::DenyAll) until
    /// the app has a UI); `audit` receives one line per call.
    pub fn new(
        session: Arc<Session>,
        confirmer: Arc<dyn Confirmer>,
        audit: Option<Arc<AuditLog>>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                session: {
                    // Stops outside a tool call (the user, the session's
                    // end) are audited too (#17).
                    session.forwards().set_audit(audit.clone());
                    session
                },
                registry: PluginRegistry::with_builtins(),
                confirmer,
                audit,
                helm: std::sync::RwLock::new(None),
            }),
        }
    }

    /// Offers the Helm tools with the user's `helm` (#418).
    pub fn set_helm(&self, env: HelmEnv) {
        *sync::write(&self.inner.helm) = Some(Arc::new(env));
    }

    /// Runs the session's port-forwards in `manager`: the app's, so they
    /// show on its "Port forwards" screen (#17). Before the first forward.
    pub fn set_forward_manager(&self, manager: kubach_core::streams::PortForwardManager) {
        self.inner.session.forwards().set_manager(manager);
    }

    fn helm(&self) -> Option<Arc<HelmEnv>> {
        sync::read(&self.inner.helm).clone()
    }

    pub fn session(&self) -> &Arc<Session> {
        &self.inner.session
    }

    /// Tools offered to the agent. Beyond the session's access (#15) a
    /// tool is still offered, marked as needing the user's approval in
    /// Kubach, so the agent can ask. Hidden: what the launch's `--profile`
    /// excludes and revealing values on a context that hides them, since
    /// nobody can be asked for those. The user's RBAC does not hide
    /// anything: it is checked in the namespace a call names (#401). Hidden
    /// tools are still refused by the policy if called.
    pub fn tools(&self) -> Vec<Tool> {
        let session = &self.inner.session;
        let scope = session.scope();
        let have = session.access();
        catalog()
            .into_iter()
            .filter_map(|spec| {
                let shown = match spec.name {
                    name if name.starts_with("helm_") => self.helm().is_some(),
                    "top_pods" => session
                        .catalog()
                        .find("metrics.k8s.io", "PodMetrics")
                        .is_some(),
                    "get_secret_value" => scope.policy.reveal_allowed(),
                    _ => true,
                };
                let needed = tool_access(spec.name);
                if !shown || needed.is_some_and(|needed| needed.profile > scope.profile) {
                    return None;
                }
                let note = needed
                    .and_then(|needed| have.missing(needed))
                    .map(|missing| {
                        format!(
                            " Requires your approval in Kubach: this needs {missing}, beyond this \
                         session's access ({have}); the first call asks the user to allow it \
                         (for this session or by default on this cluster)."
                        )
                    });
                Some(spec.into_tool(note))
            })
            .collect()
    }

    /// Runs a tool call and audits it. `client` is the MCP client's name.
    pub async fn call(&self, client: &str, tool: &str, args: Map<String, Value>) -> CallOutcome {
        self.call_with_progress(client, tool, args, None).await
    }

    /// [`McpServer::call`], passing live phases of long calls (helm) to
    /// `progress`.
    pub async fn call_with_progress(
        &self,
        client: &str,
        tool: &str,
        args: Map<String, Value>,
        progress: Option<tools::Progress>,
    ) -> CallOutcome {
        let started = Instant::now();
        self.inner.session.note_call(client);
        let ctx = Ctx {
            session: &self.inner.session,
            registry: &self.inner.registry,
            confirmer: self.inner.confirmer.as_ref(),
            client_name: client,
            tool,
            decisions: std::sync::Mutex::new(Vec::new()),
            accesses: std::sync::Mutex::new(Vec::new()),
            helm: self.helm(),
            progress,
            namespace: std::sync::Mutex::new(None),
            identity: std::sync::Mutex::new(None),
        };
        let result = tools::run(&ctx, tool, &args).await;
        let decisions = sync::into_inner(ctx.decisions);
        let accesses = sync::into_inner(ctx.accesses);
        let namespace = sync::into_inner(ctx.namespace);
        let ran_as = sync::into_inner(ctx.identity);
        let change = result.as_ref().ok().and_then(|output| output.audit.clone());
        let (decided_by, allowed, reason, code) = match &result {
            Ok(output) if output.confirmed => (DecidedBy::User, true, None, "ok".to_owned()),
            Ok(_) => (DecidedBy::None, true, None, "ok".to_owned()),
            Err(error @ ToolError::Denied(_)) => (
                DecidedBy::Policy,
                false,
                Some(error.message()),
                "denied".into(),
            ),
            Err(error @ ToolError::Rejected(_)) => (
                DecidedBy::User,
                false,
                Some(error.message()),
                "rejected".into(),
            ),
            Err(error @ ToolError::Api { code, .. }) => (
                DecidedBy::Api,
                false,
                Some(error.message()),
                code.map_or_else(|| "error".to_owned(), |c| c.to_string()),
            ),
            Err(error @ ToolError::Failed(_)) => {
                (DecidedBy::None, true, Some(error.message()), "error".into())
            }
        };
        if let Some(audit) = &self.inner.audit {
            // Answers for rights first, each on its own line: what was asked
            // (namespace, Secret name), never a value.
            for decision in &decisions {
                let _ = audit.record(&self.grant_entry(client, tool, decision));
            }
            for decision in &accesses {
                let _ = audit.record(&self.access_entry(client, tool, decision));
            }
            let mut masked_args = Value::Object(args);
            mask_audit_args(&mut masked_args);
            let entry = AuditEntry {
                time: Timestamp::now().to_string(),
                session: self.inner.session.id().to_owned(),
                client: client.to_owned(),
                tool: tool.to_owned(),
                args: masked_args,
                change,
                decided_by,
                allowed,
                reason: reason.clone(),
                identity: ran_as
                    .unwrap_or_else(|| self.inner.session.describe_identity(namespace.as_deref())),
                result: code,
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            };
            // A failing audit log must not hide the call from the agent's
            // result; the app surfaces I/O errors separately.
            let _ = audit.record(&entry);
        }
        match result {
            Ok(output) => CallOutcome {
                text: output.text,
                is_error: false,
            },
            Err(error) => CallOutcome {
                text: error.message(),
                is_error: true,
            },
        }
    }

    /// Grants reading the session's pinned namespaces from the start, as
    /// `kubach mcp --grant-read` asks, and records it in the audit log as
    /// decided by the launch arguments.
    pub async fn grant_read_at_launch(&self) -> Result<(), crate::session::SessionError> {
        let session = &self.inner.session;
        for namespace in session.scope().namespaces.clone() {
            let result = session.grant_read_at_launch(&namespace).await;
            if let Some(audit) = &self.inner.audit {
                let _ = audit.record(&AuditEntry {
                    time: Timestamp::now().to_string(),
                    session: session.id().to_owned(),
                    client: String::new(),
                    tool: "grant:--grant-read".into(),
                    args: serde_json::to_value(Grant::read(namespace.clone()))
                        .unwrap_or(Value::Null),
                    change: None,
                    decided_by: DecidedBy::LaunchArgs,
                    allowed: result.is_ok(),
                    reason: result.as_ref().err().map(|e| e.to_string()),
                    identity: session.describe_identity(Some(&namespace)),
                    result: if result.is_ok() { "granted" } else { "error" }.into(),
                    duration_ms: 0,
                });
            }
            result?;
        }
        Ok(())
    }

    /// The audit line of one answer for a right.
    fn grant_entry(&self, client: &str, tool: &str, decision: &GrantDecision) -> AuditEntry {
        AuditEntry {
            time: Timestamp::now().to_string(),
            session: self.inner.session.id().to_owned(),
            client: client.to_owned(),
            tool: format!("grant:{tool}"),
            args: serde_json::to_value(&decision.grant).unwrap_or(Value::Null),
            change: None,
            decided_by: if decision.outcome == "access" {
                DecidedBy::Access
            } else {
                DecidedBy::User
            },
            allowed: matches!(decision.outcome, "granted" | "access"),
            reason: decision.reason.clone(),
            identity: self
                .inner
                .session
                .describe_identity(Some(decision.grant.namespace())),
            result: decision.outcome.to_owned(),
            duration_ms: 0,
        }
    }

    /// The audit line of one answer about access (#15): what was asked, on
    /// which context, and whether the context's minimum was raised too
    /// (`always`).
    fn access_entry(&self, client: &str, tool: &str, decision: &AccessDecision) -> AuditEntry {
        let scope = self.inner.session.scope();
        AuditEntry {
            time: Timestamp::now().to_string(),
            session: self.inner.session.id().to_owned(),
            client: client.to_owned(),
            tool: format!("access:{tool}"),
            args: json!({
                "raise": decision.raise,
                "context": scope.context,
                "server": scope.server_url,
                "namespace": decision.namespace,
                "savedToSettings": decision.outcome == "always",
            }),
            change: None,
            decided_by: DecidedBy::User,
            allowed: matches!(decision.outcome, "session" | "always"),
            reason: decision.reason.clone(),
            identity: self
                .inner
                .session
                .describe_identity(decision.namespace.as_deref()),
            result: decision.outcome.to_owned(),
            duration_ms: 0,
        }
    }

    /// What the agent is told about the session when it connects.
    pub fn instructions(&self) -> String {
        let session = &self.inner.session;
        let scope = session.scope();
        let access = session.access();
        let reveal = if scope.policy.reveal_allowed() {
            " get_secret_value reveals one Secret after the user confirms it, so prefer the \
             checks."
        } else {
            " Secret values are hidden on this context."
        };
        let secrets = if access.secrets {
            format!(
                "Secret metadata and checks (no values) are available through list_secrets, \
                 check_secret and compare_secret_values.{reveal}"
            )
        } else {
            format!(
                "Secrets (list_secrets, check_secret, compare_secret_values) need the user's \
                 approval first.{reveal}"
            )
        };
        let writes = if writes_secrets(Access::ALL.capped(scope.profile)) {
            " To create or change a Secret call request_secret_input: the user enters or \
             generates the values in Kubach and you get keys and sizes, never values; never ask \
             the user to paste a value into the chat. copy_secret and delete_secret are \
             confirmed by the user too."
        } else {
            ""
        };
        let namespaces = if scope.pinned() {
            format!(
                "This session is limited to namespace(s) {}.",
                scope.namespaces.join(", ")
            )
        } else {
            "Pass `namespace` to the tools; list_namespaces shows the namespaces Kubach knows \
             of. Any namespace works, as many as you need."
                .to_owned()
        };
        let more = " Tools beyond your access are marked \"Requires your approval in Kubach\": \
             calling one asks the user to allow it (for this session or by default on this \
             cluster); a refusal is final for the session.";
        let helm = if self.helm().is_some() {
            " Helm: helm_search_charts and helm_show_values for charts of the repositories added \
             in Kubach (or an oci:// chart the user allows), helm_list_releases and \
             helm_release_history; helm_install, helm_upgrade, helm_rollback and helm_uninstall \
             run a server-side dry run that the user approves in Kubach. A chart with \
             cluster-scoped objects (CRDs, ClusterRoles, webhooks) or RBAC: Kubach asks the user \
             to run that one call with their own credentials."
        } else {
            ""
        };
        format!(
            "Kubach gives you access to Kubernetes namespaces of context {}. Without asking you \
             have {access}.{more}{helm} {namespaces} Cluster-scoped resources and calls across \
             all namespaces are not available. {secrets}{writes} Changes, deletions, \
             commands in containers and port-forwards (port_forward_start, on 127.0.0.1 of the \
             user's machine) are previewed and need the user's approval one by one. Call \
             `session_info` for details.",
            scope.context
        )
    }
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("kubach", kubach_core::VERSION))
            .with_instructions(self.instructions())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let client = client_name(&context);
        // Live phases of a long call as MCP progress notifications, when
        // the client asked for them with a progress token.
        let progress: Option<tools::Progress> = request
            .meta
            .as_ref()
            .and_then(|meta| meta.get_progress_token())
            .map(|token| {
                let peer = context.peer.clone();
                let step = Arc::new(std::sync::atomic::AtomicU32::new(0));
                Arc::new(move |phase: String| {
                    let peer = peer.clone();
                    let token = token.clone();
                    let n = step.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    tokio::spawn(async move {
                        let _ = peer
                            .notify_progress(
                                rmcp::model::ProgressNotificationParam::new(token, f64::from(n))
                                    .with_message(phase),
                            )
                            .await;
                    });
                }) as tools::Progress
            });
        let outcome = self
            .call_with_progress(
                &client,
                &request.name,
                request.arguments.unwrap_or_default(),
                progress,
            )
            .await;
        let content = vec![ContentBlock::text(outcome.text)];
        Ok(if outcome.is_error {
            CallToolResult::error(content)
        } else {
            CallToolResult::success(content)
        }
        .into())
    }
}

/// The MCP client's self-reported name (for the audit log only; it is not
/// authenticated).
pub(crate) fn client_name(context: &RequestContext<RoleServer>) -> String {
    if let Some(info) = context.peer.peer_info() {
        return info.client_info.name.clone();
    }
    serde_json::to_value(&context.meta)
        .ok()
        .and_then(|meta| {
            meta.get("io.modelcontextprotocol/clientInfo")?
                .get("name")?
                .as_str()
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".into())
}

// ------------------------------------------------------------- tool catalog

struct ToolSpec {
    name: &'static str,
    description: &'static str,
    schema: Value,
    read_only: bool,
    destructive: bool,
}

/// What a tool needs of the access, where the user may raise it (#15).
/// Custom resources are found by kind, at the call.
fn tool_access(name: &str) -> Option<Access> {
    let need = |profile, secrets| {
        Some(Access {
            profile,
            secrets,
            crds: false,
        })
    };
    match name {
        "get_logs" | "exec" | "port_forward_start" => need(Profile::Debug, false),
        "apply_manifest" | "scale" | "rollout_restart" | "delete_resource" | "helm_install"
        | "helm_upgrade" | "helm_rollback" | "helm_uninstall" => need(Profile::Deploy, false),
        "request_secret_input" | "copy_secret" | "delete_secret" => need(Profile::Deploy, true),
        "list_secrets" | "check_secret" | "compare_secret_values" | "get_secret_value" => {
            need(Profile::ReadOnly, true)
        }
        _ => None,
    }
}

impl ToolSpec {
    fn into_tool(self, note: Option<String>) -> Tool {
        let schema = match self.schema {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        let description = match note {
            Some(note) => format!("{}{note}", self.description),
            None => self.description.to_owned(),
        };
        Tool::new(self.name, description, Arc::new(schema)).annotate(
            ToolAnnotations::new()
                .read_only(self.read_only)
                .destructive(self.destructive),
        )
    }
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn kind_props(extra: Value) -> Value {
    let mut props = json!({
        "kind": {"type": "string", "description": "Kind or plural resource, e.g. Deployment or deployments"},
        "apiVersion": {"type": "string", "description": "Only to disambiguate kinds served by several groups"},
        "namespace": {"type": "string", "description": NAMESPACE_HELP},
    });
    if let (Some(props), Value::Object(extra)) = (props.as_object_mut(), extra) {
        props.extend(extra);
    }
    props
}

/// How tools describe their `namespace` argument (#401).
const NAMESPACE_HELP: &str = "Namespace to act in; may be left out once the session has exactly \
     one. Any namespace works; list_namespaces shows the ones Kubach knows of";

fn catalog() -> Vec<ToolSpec> {
    let ns = json!({"namespace": {"type": "string", "description": NAMESPACE_HELP}});
    vec![
        ToolSpec {
            name: "session_info",
            description: "Namespaces, access (minimal and now), rights so far, expiry and, per \
                          namespace, whether the API server enforces the boundary too.",
            schema: object_schema(json!({}), &[]),
            read_only: true,
            destructive: false,
        },
        ToolSpec {
            name: "list_namespaces",
            description: "Names of the namespaces you can ask for, each with where Kubach knows it \
                          from (the cluster, or kubeconfig and recently opened in Kubach when \
                          the user may not list namespaces) and whether it is allowed (in use), \
                          available or refused. Names only.",
            schema: object_schema(json!({}), &[]),
            read_only: true,
            destructive: false,
        },
        ToolSpec {
            name: "list_kinds",
            description: "Namespaced resource kinds available in this session.",
            schema: object_schema(json!({}), &[]),
            read_only: true,
            destructive: false,
        },
        ToolSpec {
            name: "list_resources",
            description: "List resources of a kind in the session namespace, as a table.",
            schema: object_schema(
                kind_props(json!({
                    "labelSelector": {"type": "string"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 500},
                })),
                &["kind"],
            ),
            read_only: true,
            destructive: false,
        },
        ToolSpec {
            name: "get_resource",
            description: "One resource as YAML (Secret values and last-applied-configuration masked).",
            schema: object_schema(
                kind_props(json!({"name": {"type": "string"}})),
                &["kind", "name"],
            ),
            read_only: true,
            destructive: false,
        },
        ToolSpec {
            name: "describe_resource",
            description: "Status, details, relations and recent events of a resource.",
            schema: object_schema(
                kind_props(json!({"name": {"type": "string"}})),
                &["kind", "name"],
            ),
            read_only: true,
            destructive: false,
        },
        ToolSpec {
            name: "list_events",
            description: "Recent events in the namespace, optionally for one object (kind + name).",
            schema: object_schema(
                kind_props(json!({
                    "name": {"type": "string"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200},
                })),
                &[],
            ),
            read_only: true,
            destructive: false,
        },
        ToolSpec {
            name: "get_logs",
            description: "Recent container logs (bounded, credentials masked best-effort).",
            schema: object_schema(
                {
                    let mut p = json!({
                        "pod": {"type": "string"},
                        "container": {"type": "string"},
                        "tailLines": {"type": "integer", "minimum": 1, "maximum": 2000},
                        "sinceSeconds": {"type": "integer", "minimum": 1},
                        "previous": {"type": "boolean"},
                    });
                    p.as_object_mut()
                        .unwrap()
                        .extend(ns.as_object().unwrap().clone());
                    p
                },
                &["pod"],
            ),
            read_only: true,
            destructive: false,
        },
        ToolSpec {
            name: "top_pods",
            description: "CPU and memory usage of pods (needs metrics-server).",
            schema: object_schema(ns.clone(), &[]),
            read_only: true,
            destructive: false,
        },
        ToolSpec {
            name: "get_topology",
            description: "How the namespace's components connect, as compact JSON: Ingress/Gateway \
                          → Service → workloads/pods → ConfigMaps, Secrets (names only), PVCs, \
                          ServiceAccounts and external addresses. Nodes (id, kind, namespace, \
                          name, presence, pods, ports, hosts, NetworkPolicy isolation), typed edges \
                          (routes, endpoints, references, policy access) and diagnostics (broken \
                          references, Services without ready pods). Narrow to one resource's \
                          upstream and downstream with `resource` and `depth`.",
            schema: object_schema(
                json!({
                    "namespace": {"type": "string", "description": "Optional; any namespace"},
                    "namespaces": {"type": "array", "items": {"type": "string"}, "maxItems": 3, "description": "Several namespaces in one graph"},
                    "resource": {
                        "type": "object",
                        "description": "Only this resource with what routes to it and what it uses",
                        "properties": {
                            "kind": {"type": "string", "description": "Ingress, Service, Deployment, Pod, ConfigMap…"},
                            "name": {"type": "string"},
                            "namespace": {"type": "string"},
                        },
                        "required": ["kind", "name"],
                        "additionalProperties": false,
                    },
                    "depth": {"type": "integer", "minimum": 1, "maximum": 10, "description": "Steps up and down from `resource`; unbounded by default"},
                    "detail": {"type": "string", "enum": ["workloads", "pods"], "default": "workloads", "description": "workloads: pods collapsed into their workload"},
                    "networkPolicies": {"type": "boolean", "default": true, "description": "Add the NetworkPolicy layer: isolation, blocked Service paths, allowed workload pairs"},
                }),
                &[],
            ),
            read_only: true,
            destructive: false,
        },
        ToolSpec {
            name: "can_reach",
            description: "Whether NetworkPolicies let `from` open a connection to `to` on a port, \
                          with the policies that decide each side (egress of the source, ingress \
                          of the target) and why. Ends are workloads or pods ({kind, name, \
                          namespace?}); `to` may be a Service (its port maps to the pods' target \
                          port) or {address} of an IP outside the cluster. An analysis of the \
                          objects: whether the CNI enforces them is unknown.",
            schema: object_schema(
                {
                    let end = json!({
                        "type": "object",
                        "properties": {
                            "kind": {"type": "string"},
                            "name": {"type": "string"},
                            "namespace": {"type": "string", "description": "Optional; any namespace"},
                            "address": {"type": "string", "description": "An IP outside the cluster, instead of kind and name"},
                        },
                        "additionalProperties": false,
                    });
                    json!({
                        "from": end.clone(),
                        "to": end,
                        "port": {"type": ["integer", "string"], "description": "Port number, or a named container port of the target"},
                        "protocol": {"type": "string", "enum": ["TCP", "UDP", "SCTP"], "default": "TCP"},
                    })
                },
                &["from", "to", "port"],
            ),
            read_only: true,
            destructive: false,
        },
        ToolSpec {
            name: "apply_manifest",
            description: "Server-side apply of YAML manifests in the session namespace. dryRun \
                          (default true) returns the diff; a real apply needs the user's approval.",
            schema: object_schema(
                {
                    let mut p = json!({
                        "yaml": {"type": "string", "description": "One or more YAML documents"},
                        "dryRun": {"type": "boolean", "default": true},
                    });
                    p.as_object_mut()
                        .unwrap()
                        .extend(ns.as_object().unwrap().clone());
                    p
                },
                &["yaml"],
            ),
            read_only: false,
            destructive: true,
        },
        ToolSpec {
            name: "scale",
            description: "Scale a Deployment, StatefulSet or ReplicaSet (needs the user's approval).",
            schema: object_schema(
                kind_props(json!({
                    "name": {"type": "string"},
                    "replicas": {"type": "integer", "minimum": 0, "maximum": 1000},
                })),
                &["kind", "name", "replicas"],
            ),
            read_only: false,
            destructive: false,
        },
        ToolSpec {
            name: "rollout_restart",
            description: "Rollout restart of a Deployment, StatefulSet or DaemonSet (needs the user's approval).",
            schema: object_schema(
                kind_props(json!({"name": {"type": "string"}})),
                &["kind", "name"],
            ),
            read_only: false,
            destructive: false,
        },
        ToolSpec {
            name: "delete_resource",
            description: "Delete a resource (needs the user's approval).",
            schema: object_schema(
                kind_props(json!({"name": {"type": "string"}})),
                &["kind", "name"],
            ),
            read_only: false,
            destructive: true,
        },
        ToolSpec {
            name: "list_secrets",
            description: "Secrets of the namespace without values: type, keys, sizes, age, which \
                          pods and workloads use them (env, envFrom, volumes, imagePullSecrets) \
                          and which keys they miss.",
            schema: object_schema(
                {
                    let mut p = json!({
                        "name": {"type": "string", "description": "Only this Secret"},
                    });
                    p.as_object_mut()
                        .unwrap()
                        .extend(ns.as_object().unwrap().clone());
                    p
                },
                &[],
            ),
            read_only: true,
            destructive: false,
        },
        ToolSpec {
            name: "check_secret",
            description: "Checks a Secret's values inside Kubach and returns only conclusions: \
                          empty, whitespace or newline at the edges, valid JSON/YAML/base64, URL \
                          scheme/host/port, certificate dates/CN/SAN/issuer, whether tls.key \
                          matches tls.crt, registries of a docker config. Never values or hashes.",
            schema: object_schema(
                {
                    let mut p = json!({
                        "name": {"type": "string"},
                        "key": {"type": "string", "description": "Only this key"},
                    });
                    p.as_object_mut()
                        .unwrap()
                        .extend(ns.as_object().unwrap().clone());
                    p
                },
                &["name"],
            ),
            read_only: true,
            destructive: false,
        },
        ToolSpec {
            name: "compare_secret_values",
            description: "Whether two keys (of one or two Secrets) hold the same value. Compared \
                          inside Kubach; only yes or no is returned.",
            schema: object_schema(
                {
                    let mut p = json!({
                        "name": {"type": "string"},
                        "key": {"type": "string"},
                        "otherName": {"type": "string"},
                        "otherKey": {"type": "string"},
                        "otherNamespace": {"type": "string", "description": "Optional; any namespace"},
                    });
                    p.as_object_mut()
                        .unwrap()
                        .extend(ns.as_object().unwrap().clone());
                    p
                },
                &["name", "key", "otherName", "otherKey"],
            ),
            read_only: true,
            destructive: false,
        },
        ToolSpec {
            name: "get_secret_value",
            description: "The values of one Secret (or one key). The user confirms each Secret \
                          and is warned that the values go to the model's provider; prefer \
                          check_secret, which needs no values.",
            schema: object_schema(
                {
                    let mut p = json!({
                        "name": {"type": "string"},
                        "key": {"type": "string", "description": "Only this key"},
                    });
                    p.as_object_mut()
                        .unwrap()
                        .extend(ns.as_object().unwrap().clone());
                    p
                },
                &["name"],
            ),
            read_only: true,
            destructive: false,
        },
        ToolSpec {
            name: "request_secret_input",
            description: "Create a Secret or change one (replace, add or remove keys) with values \
                          the user enters or generates in the Kubach window. You name the keys \
                          and explain why; you never see the values: the answer lists keys, \
                          sizes and the workloads that use the Secret. Types for mode create: \
                          Opaque (default), kubernetes.io/tls (the user picks .crt/.key files), \
                          kubernetes.io/dockerconfigjson (registry, user, password).",
            schema: object_schema(
                {
                    let mut p = json!({
                        "name": {"type": "string"},
                        "mode": {"type": "string", "enum": ["create", "update"]},
                        "type": {"type": "string", "enum": ["Opaque", "kubernetes.io/tls", "kubernetes.io/dockerconfigjson"], "description": "Mode create only"},
                        "keys": {"type": "array", "items": {"type": "string"}, "description": "Keys to ask values for; the user may change them"},
                        "removeKeys": {"type": "array", "items": {"type": "string"}, "description": "Mode update: keys you suggest removing; the user decides"},
                        "explanation": {"type": "string", "description": "Why, for the user (shown as unverified text, max 1000 characters)"},
                    });
                    p.as_object_mut()
                        .unwrap()
                        .extend(ns.as_object().unwrap().clone());
                    p
                },
                &["name", "mode"],
            ),
            read_only: false,
            destructive: true,
        },
        ToolSpec {
            name: "copy_secret",
            description: "Copy a Secret (data and type) to another namespace of this session or \
                          another name, inside Kubach; ownerReferences, labels and annotations \
                          are dropped. Needs the user's approval; an existing target is not \
                          overwritten. You get keys and sizes, never values.",
            schema: object_schema(
                {
                    let mut p = json!({
                        "name": {"type": "string"},
                        "toNamespace": {"type": "string", "description": "Optional; any namespace"},
                        "toName": {"type": "string", "description": "Optional; the same name by default"},
                        "explanation": {"type": "string", "description": "Why, for the user"},
                    });
                    p.as_object_mut()
                        .unwrap()
                        .extend(ns.as_object().unwrap().clone());
                    p
                },
                &["name"],
            ),
            read_only: false,
            destructive: false,
        },
        ToolSpec {
            name: "delete_secret",
            description: "Delete a Secret. The user confirms each deletion and sees which pods \
                          and workloads reference it.",
            schema: object_schema(
                {
                    let mut p = json!({
                        "name": {"type": "string"},
                        "explanation": {"type": "string", "description": "Why, for the user"},
                    });
                    p.as_object_mut()
                        .unwrap()
                        .extend(ns.as_object().unwrap().clone());
                    p
                },
                &["name"],
            ),
            read_only: false,
            destructive: true,
        },
        ToolSpec {
            name: "helm_search_charts",
            description: "Search Helm charts of the repositories added in Kubach (like helm search \
                          repo), or list the versions of an oci:// chart (`oci`); the user is \
                          asked once to allow a registry not added in Kubach.",
            schema: object_schema(
                json!({
                    "query": {"type": "string", "description": "Part of repo/chart, a description or a keyword; empty lists all"},
                    "oci": {"type": "string", "description": "oci://registry/path of one chart, instead of query"},
                    "versions": {"type": "boolean", "default": false, "description": "Every version, not only the latest"},
                    "devel": {"type": "boolean", "default": false, "description": "Include pre-releases"},
                }),
                &[],
            ),
            read_only: true,
            destructive: false,
        },
        ToolSpec {
            name: "helm_show_values",
            description: "The default values.yaml of a chart (repo/chart of a repository added in \
                          Kubach, or oci://…), to choose values for helm_install.",
            schema: object_schema(
                json!({
                    "chart": {"type": "string", "description": "repo/chart or oci://registry/path"},
                    "version": {"type": "string"},
                }),
                &["chart"],
            ),
            read_only: true,
            destructive: false,
        },
        ToolSpec {
            name: "helm_list_releases",
            description: "Helm releases of the namespace: name, revision, status, chart, app version.",
            schema: object_schema(ns.clone(), &[]),
            read_only: true,
            destructive: false,
        },
        ToolSpec {
            name: "helm_release_history",
            description: "Revisions of one Helm release (helm history), for helm_rollback.",
            schema: object_schema(
                {
                    let mut p = json!({"release": {"type": "string"}});
                    p.as_object_mut()
                        .unwrap()
                        .extend(ns.as_object().unwrap().clone());
                    p
                },
                &["release"],
            ),
            read_only: true,
            destructive: false,
        },
        ToolSpec {
            name: "helm_install",
            description: "Install a chart as a release in the namespace with Kubach's helm. Kubach \
                          runs a server-side dry run, shows the user the chart, version, values \
                          (secrets masked) and objects, and installs after the user approves. \
                          Cluster-scoped objects (CRDs, ClusterRoles, webhooks) and RBAC: \
                          Kubach asks the user to run this call with their own credentials \
                          (found with helm template before the dry run); if the user says no, \
                          you get the list of objects. Charts only from repositories added in \
                          Kubach or oci://; values only as YAML text.",
            schema: object_schema(
                {
                    let mut p = json!({
                        "release": {"type": "string", "description": "Release name (DNS label, max 53)"},
                        "chart": {"type": "string", "description": "repo/chart or oci://registry/path"},
                        "version": {"type": "string", "description": "Chart version; the latest stable by default"},
                        "values": {"type": "string", "description": "Values as one YAML mapping (no file paths)"},
                        "createNamespace": {"type": "boolean", "default": false},
                    });
                    p.as_object_mut()
                        .unwrap()
                        .extend(ns.as_object().unwrap().clone());
                    p
                },
                &["release", "chart"],
            ),
            read_only: false,
            destructive: false,
        },
        ToolSpec {
            name: "helm_upgrade",
            description: "Upgrade a release: a new chart or version and/or new values (YAML; they \
                          replace the release's values; without values it keeps its own). The \
                          user sees the dry-run diff and approves. Cluster-scoped objects or \
                          RBAC in the new chart or the current revision: Kubach asks the user to \
                          run this call with their own credentials first.",
            schema: object_schema(
                {
                    let mut p = json!({
                        "release": {"type": "string"},
                        "chart": {"type": "string", "description": "repo/chart or oci://…; the chart kept in the release by default"},
                        "version": {"type": "string"},
                        "values": {"type": "string", "description": "Values as one YAML mapping"},
                    });
                    p.as_object_mut()
                        .unwrap()
                        .extend(ns.as_object().unwrap().clone());
                    p
                },
                &["release"],
            ),
            read_only: false,
            destructive: true,
        },
        ToolSpec {
            name: "helm_rollback",
            description: "Roll a release back to an earlier revision (see helm_release_history); \
                          the user sees the diff and approves. Cluster-scoped objects or RBAC in \
                          either revision: Kubach asks the user to run it with their own \
                          credentials.",
            schema: object_schema(
                {
                    let mut p = json!({
                        "release": {"type": "string"},
                        "revision": {"type": "integer", "minimum": 1},
                    });
                    p.as_object_mut()
                        .unwrap()
                        .extend(ns.as_object().unwrap().clone());
                    p
                },
                &["release", "revision"],
            ),
            read_only: false,
            destructive: true,
        },
        ToolSpec {
            name: "helm_uninstall",
            description: "Uninstall a release; the user sees the objects that go with it and \
                          approves. Cluster-scoped objects or RBAC in the release: Kubach asks \
                          the user to run it with their own credentials.",
            schema: object_schema(
                {
                    let mut p = json!({
                        "release": {"type": "string"},
                        "keepHistory": {"type": "boolean", "default": false},
                    });
                    p.as_object_mut()
                        .unwrap()
                        .extend(ns.as_object().unwrap().clone());
                    p
                },
                &["release"],
            ),
            read_only: false,
            destructive: true,
        },
        ToolSpec {
            name: "port_forward_start",
            description: "Forward a local TCP port on 127.0.0.1 of the user's machine to a pod, or \
                          to a ready pod of a Service or Deployment, to check a service from \
                          inside (health endpoint, metrics, a database while debugging). Needs \
                          the user's approval. Returns the forward's id and 127.0.0.1:<port>; \
                          the forward ends with the session.",
            schema: object_schema(
                {
                    let mut p = json!({
                        "target": {"type": "string", "description": "pod/<name>, service/<name> or deployment/<name>"},
                        "port": {"description": "Remote port: a number (a Service port for services) or a port name such as http",
                                 "oneOf": [{"type": "integer", "minimum": 1, "maximum": 65535}, {"type": "string"}]},
                        "localPort": {"type": "integer", "minimum": 1024, "maximum": 65535,
                                      "description": "Local port on 127.0.0.1; any free port when left out"},
                    });
                    p.as_object_mut()
                        .unwrap()
                        .extend(ns.as_object().unwrap().clone());
                    p
                },
                &["target", "port"],
            ),
            read_only: false,
            destructive: false,
        },
        ToolSpec {
            name: "port_forward_list",
            description: "Port-forwards of this session: target, ports, local address, state and \
                          last error.",
            schema: object_schema(json!({}), &[]),
            read_only: true,
            destructive: false,
        },
        ToolSpec {
            name: "port_forward_stop",
            description: "Stop one of this session's port-forwards by id.",
            schema: object_schema(json!({"id": {"type": "integer", "minimum": 1}}), &["id"]),
            read_only: false,
            destructive: false,
        },
        ToolSpec {
            name: "exec",
            description: "Run one command in a container, without a TTY (needs the user's approval). \
                          Output is bounded and masked best-effort.",
            schema: object_schema(
                {
                    let mut p = json!({
                        "pod": {"type": "string"},
                        "container": {"type": "string"},
                        "command": {"type": "array", "items": {"type": "string"}, "minItems": 1},
                        "timeoutSeconds": {"type": "integer", "minimum": 1, "maximum": 120},
                    });
                    p.as_object_mut()
                        .unwrap()
                        .extend(ns.as_object().unwrap().clone());
                    p
                },
                &["pod", "command"],
            ),
            read_only: false,
            destructive: true,
        },
    ]
}

#[cfg(test)]
mod tests;
