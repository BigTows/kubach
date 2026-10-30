//! MCP tools: argument parsing, the policy check and execution under the
//! session identity. Every call goes `parse → policy → (ask for a right) →
//! (confirm) → API → mask`; nothing reaches the API without an [`Allowed`]
//! plan.

use std::fmt::Write as _;
use std::time::Duration;

use k8s_openapi::api::apps::v1::ReplicaSet;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::{Event, Pod};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ListMeta;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use k8s_openapi::jiff::Timestamp;
use kubach_core::apply::{self as core_apply, ApplyOptions, yaml_diff};
use kubach_core::discovery::{self, ResourceKind};
use kubach_core::plugin::builtin::action;
use kubach_core::plugin::{ActionId, ActionKind, ActionParams, Cell, PluginRegistry, format_age};
use kubach_core::rbac::ResourceAccess;
use kubach_core::streams::exec_capture;
use kube::api::{Api, DynamicObject, ListParams, LogParams};
use kube::{Client, ResourceExt};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::access::Access;
use crate::confirm::{ConfirmationDecision, ConfirmationRequest, Confirmer, RequestKind, ask};
use crate::grants::Grant;
use crate::helm_tools;
use crate::masking::{mask_object, mask_text, truncate};
use crate::policy::{
    Allowed, Confirmation, Denial, DenialCode, Finding, KindRef, Request, service_account_changes,
    walk_fields,
};
use crate::remember::{RememberNote, rememberable};
use crate::scope::NamespaceSource;
use crate::secret_tools;
use crate::secret_write;
use crate::session::{Session, SessionError};
use crate::topology_tools;
use kubach_core::sync;

/// Field manager for changes made by the agent, distinct from the UI's.
pub const FIELD_MANAGER: &str = "kubach-mcp";
pub(crate) const MAX_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_LIST_ITEMS: u32 = 500;
const DEFAULT_LOG_LINES: i64 = 200;
const MAX_LOG_LINES: i64 = 2000;
const MAX_LOG_BYTES: i64 = 256 * 1024;
const MAX_EXEC_OUTPUT: usize = 64 * 1024;
const DEFAULT_EXEC_TIMEOUT: u64 = 30;
const MAX_EXEC_TIMEOUT: u64 = 120;

/// Why a call failed, and who decided.
#[derive(Debug)]
pub(crate) enum ToolError {
    /// Rejected by the policy (including malformed arguments); no API call.
    Denied(Denial),
    /// The user said no.
    Rejected(Option<String>),
    /// The API server refused or failed.
    Api { code: Option<u16>, message: String },
    /// Anything else after the policy passed (timeouts, decoding).
    Failed(String),
}

impl ToolError {
    pub(crate) fn message(&self) -> String {
        match self {
            Self::Denied(denial) => denial.to_string(),
            Self::Rejected(reason) => format!(
                "the user rejected this call{}",
                reason
                    .as_deref()
                    .map(|r| format!(": {r}"))
                    .unwrap_or_default()
            ),
            Self::Api {
                code: Some(code),
                message,
            } => format!("API error {code}: {message}"),
            Self::Api {
                code: None,
                message,
            } => format!("API error: {message}"),
            Self::Failed(message) => message.clone(),
        }
    }
}

impl From<Denial> for ToolError {
    fn from(denial: Denial) -> Self {
        Self::Denied(denial)
    }
}

impl From<SessionError> for ToolError {
    fn from(error: SessionError) -> Self {
        Self::Failed(error.to_string())
    }
}

impl From<kube::Error> for ToolError {
    fn from(error: kube::Error) -> Self {
        match error {
            kube::Error::Api(status) => Self::Api {
                code: Some(status.code),
                message: status.message.clone(),
            },
            other => Self::Api {
                code: None,
                message: other.to_string(),
            },
        }
    }
}

/// Successful output, plus whether the user approved it.
pub(crate) struct ToolOutput {
    pub text: String,
    pub confirmed: bool,
    /// What the audit log records about the change beyond the arguments
    /// (a Secret's keys and operation, #230); never a value.
    pub audit: Option<Value>,
}

impl ToolOutput {
    pub(crate) fn read(text: String) -> Self {
        Self {
            text,
            confirmed: false,
            audit: None,
        }
    }

    fn confirmed(text: String) -> Self {
        Self {
            text,
            confirmed: true,
            audit: None,
        }
    }
}

pub(crate) struct Ctx<'a> {
    pub session: &'a Session,
    pub registry: &'a PluginRegistry,
    pub confirmer: &'a dyn Confirmer,
    pub client_name: &'a str,
    /// The tool being called, for the questions the user sees.
    pub tool: &'a str,
    /// Rights the user answered for during this call, for the audit log.
    pub decisions: std::sync::Mutex<Vec<GrantDecision>>,
    /// Answers about access during this call (#15).
    pub accesses: std::sync::Mutex<Vec<AccessDecision>>,
    /// The user's `helm`, for the Helm tools (#418).
    pub helm: Option<std::sync::Arc<crate::helm_tools::HelmEnv>>,
    /// Live phases of a long call (helm), for MCP progress notifications.
    pub progress: Option<Progress>,
    /// The namespace the call acts in once the policy allowed it, for the
    /// identity in the audit log.
    pub namespace: std::sync::Mutex<Option<String>>,
    /// Who the call ran as when not the namespace's identity: a helm call
    /// the user allowed under their own credentials (#18).
    pub identity: std::sync::Mutex<Option<String>>,
    /// Whether the change ran on an earlier "don't ask again" or the user
    /// ticked it now (#30), for the audit log and the agent.
    pub remembered: std::sync::Mutex<Option<RememberNote>>,
}

/// One answer for a right, as the audit log records it: never a value.
#[derive(Clone, Debug)]
pub(crate) struct GrantDecision {
    pub grant: Grant,
    /// `access` (within the session's access, no question), `granted`,
    /// `refused` (remembered) or `no-answer` (asked again later).
    pub outcome: &'static str,
    pub reason: Option<String>,
}

/// Receives the phases of a long call as they happen.
pub(crate) type Progress = std::sync::Arc<dyn Fn(String) + Send + Sync>;

/// One answer about access, for the audit log (#15).
#[derive(Clone, Debug)]
pub(crate) struct AccessDecision {
    pub raise: Access,
    pub namespace: Option<String>,
    /// `session`, `always` (the settings were raised too), `refused`,
    /// `no-answer` or `error`.
    pub outcome: &'static str,
    pub reason: Option<String>,
}

// ------------------------------------------------------------- arguments

pub(crate) fn parse<T: DeserializeOwned>(args: &Map<String, Value>) -> Result<T, ToolError> {
    serde_json::from_value(Value::Object(args.clone())).map_err(|e| {
        ToolError::Denied(Denial::new(
            DenialCode::InvalidInput,
            format!("invalid arguments: {e}"),
        ))
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NoArgs {}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListArgs {
    kind: String,
    api_version: Option<String>,
    namespace: Option<String>,
    label_selector: Option<String>,
    limit: Option<u32>,
    /// Accepted only to be refused explicitly.
    #[serde(default)]
    all_namespaces: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ObjectArgs {
    kind: String,
    api_version: Option<String>,
    name: String,
    namespace: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScaleArgs {
    kind: String,
    api_version: Option<String>,
    name: String,
    namespace: Option<String>,
    replicas: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EventsArgs {
    namespace: Option<String>,
    kind: Option<String>,
    api_version: Option<String>,
    name: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LogsArgs {
    pod: String,
    container: Option<String>,
    namespace: Option<String>,
    tail_lines: Option<i64>,
    since_seconds: Option<i64>,
    #[serde(default)]
    previous: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NamespaceArgs {
    namespace: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ApplyArgs {
    yaml: String,
    namespace: Option<String>,
    dry_run: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExecArgs {
    pod: String,
    container: Option<String>,
    namespace: Option<String>,
    command: Vec<String>,
    timeout_seconds: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SecretListArgs {
    namespace: Option<String>,
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SecretArgs {
    namespace: Option<String>,
    name: String,
    key: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CompareArgs {
    namespace: Option<String>,
    name: String,
    key: String,
    other_namespace: Option<String>,
    other_name: String,
    other_key: String,
}

pub(crate) fn kind_ref(kind: String, api_version: Option<String>) -> KindRef {
    KindRef { kind, api_version }
}

// ------------------------------------------------------------- rights

/// Calls in one request may need at most this many rights and access
/// questions (a level and custom resources, reading two namespaces and
/// changing one).
const MAX_GRANTS_PER_CALL: usize = 6;

/// The policy's verdict on `request`, setting up the rights it needs
/// within the access and asking the user for the rest on the way (#227,
/// #15).
pub(crate) async fn authorize(ctx: &Ctx<'_>, request: &Request) -> Result<Allowed, ToolError> {
    for _ in 0..=MAX_GRANTS_PER_CALL {
        match ctx.session.policy().check(request) {
            Ok(allowed) => {
                if !allowed.namespace.is_empty() {
                    *sync::lock(&ctx.namespace) = Some(allowed.namespace.clone());
                }
                return Ok(allowed);
            }
            Err(denial) if denial.code == DenialCode::NeedsGrant => {
                let Some(grant) = denial.grant.clone() else {
                    return Err(denial.into());
                };
                ask_grant(ctx, request, grant).await?;
            }
            Err(denial) if denial.code == DenialCode::NeedsAccess => {
                let Some(raise) = denial.access else {
                    return Err(denial.into());
                };
                ask_access(ctx, request, raise).await?;
            }
            Err(denial) if denial.code == DenialCode::RevealDisabled => {
                tell_reveal_blocked(ctx, request).await;
                return Err(denial.into());
            }
            Err(denial) => return Err(denial.into()),
        }
    }
    Err(ToolError::Failed(
        "this call needs too many rights at once".into(),
    ))
}

/// What the user is asked, in one line.
fn grant_summary(client: &str, grant: &Grant) -> String {
    match grant {
        Grant::Read { namespace } => format!("{client} wants to read namespace {namespace}"),
        Grant::Secret { namespace, name } => {
            format!("{client} wants the values of Secret {namespace}/{name}")
        }
        Grant::Modify { namespace } => format!("{client} wants to change namespace {namespace}"),
    }
}

/// What a yes means, for the dialog.
fn grant_details(grant: &Grant) -> String {
    match grant {
        Grant::Secret { .. } => "The values will be sent into the model's context and, as a \
             rule, to the LLM provider. This cannot be taken back. Rotate this Secret once the \
             work is done. Only this Secret, only in this session."
            .into(),
        // Not asked since #15; kept for the bridge's older questions.
        Grant::Read { namespace } | Grant::Modify { namespace } => {
            format!("Rights of the session in namespace {namespace}.")
        }
    }
}

/// Sets up `grant` for `request`: reading and changing a namespace come
/// with the session's access and are granted without a question (#15,
/// they set up the namespace's identity); revealing a Secret is asked for
/// that Secret, and the answer recorded in the session. Only one question
/// per session is open at a time. While waiting for it the session may
/// have changed, so the policy is asked again first and the question goes
/// out only if `request` still needs this very right.
async fn ask_grant(ctx: &Ctx<'_>, request: &Request, grant: Grant) -> Result<(), ToolError> {
    let _asking = ctx.session.asking().await;
    match ctx.session.policy().check(request) {
        Err(denial)
            if denial.code == DenialCode::NeedsGrant && denial.grant == Some(grant.clone()) => {}
        // Answered meanwhile or refused now; the caller checks again.
        _ => return Ok(()),
    }
    if !matches!(grant, Grant::Secret { .. }) {
        let result = ctx.session.decide(grant.clone(), true).await;
        let (outcome, reason) = match &result {
            Ok(()) => ("access", None),
            Err(error) => ("error", Some(error.to_string())),
        };
        sync::lock(&ctx.decisions).push(GrantDecision {
            grant: grant.clone(),
            outcome,
            reason,
        });
        return result.map_err(|error| {
            ToolError::Failed(format!(
                "Kubach could not set up the right to {grant}: {error}"
            ))
        });
    }
    let request = ConfirmationRequest {
        kind: RequestKind::Grant {
            grant: grant.clone(),
        },
        session_id: ctx.session.id().to_owned(),
        context: ctx.session.scope().context.clone(),
        client: ctx.client_name.to_owned(),
        tool: ctx.tool.to_owned(),
        namespace: grant.namespace().to_owned(),
        summary: grant_summary(ctx.client_name, &grant),
        details: grant_details(&grant),
        dangerous: Vec::new(),
        mode: ctx.session.mode_in(grant.namespace()),
        allow_remember: false,
    };
    let decision = ask(ctx.confirmer, &request).await;
    if !ctx.session.is_active() {
        return Err(Denial::new(
            DenialCode::SessionEnded,
            "the session ended while waiting for the user",
        )
        .into());
    }
    let record = |outcome: &'static str, reason: Option<String>| {
        sync::lock(&ctx.decisions).push(GrantDecision {
            grant: grant.clone(),
            outcome,
            reason,
        });
    };
    match decision {
        ConfirmationDecision::Approved
        | ConfirmationDecision::ApprovedAlways
        | ConfirmationDecision::ApprovedForSession => {
            if let Err(error) = ctx.session.decide(grant.clone(), true).await {
                let message = format!(
                    "the user allowed this session to {grant}, but Kubach could not grant it: \
                     {error}"
                );
                record("error", Some(message.clone()));
                return Err(ToolError::Failed(message));
            }
            record("granted", None);
            Ok(())
        }
        ConfirmationDecision::Rejected { reason } => {
            let _ = ctx.session.decide(grant.clone(), false).await;
            record("refused", reason.clone());
            Err(ToolError::Rejected(Some(format!(
                "the user did not allow this session to {grant}{}",
                reason.map(|r| format!(" ({r})")).unwrap_or_default()
            ))))
        }
        ConfirmationDecision::Denied { reason } => {
            record("no-answer", reason.clone());
            Err(ToolError::Rejected(reason))
        }
        ConfirmationDecision::Provided { .. } => {
            let reason = Some("unexpected answer to a question about a right".to_owned());
            record("no-answer", reason.clone());
            Err(ToolError::Rejected(reason))
        }
    }
}

/// The namespace a request names, if any; shown in the access question
/// for context only.
fn named_namespace(request: &Request) -> Option<&str> {
    match request {
        Request::List { namespace, .. }
        | Request::Get { namespace, .. }
        | Request::Events { namespace, .. }
        | Request::Logs { namespace, .. }
        | Request::Top { namespace }
        | Request::Apply { namespace, .. }
        | Request::Scale { namespace, .. }
        | Request::Restart { namespace, .. }
        | Request::Delete { namespace, .. }
        | Request::Exec { namespace, .. }
        | Request::PortForward { namespace, .. }
        | Request::SecretList { namespace, .. }
        | Request::SecretCheck { namespace, .. }
        | Request::SecretCompare { namespace, .. }
        | Request::SecretReveal { namespace, .. }
        | Request::SecretWrite { namespace, .. }
        | Request::SecretCopy { namespace, .. }
        | Request::SecretDelete { namespace, .. }
        | Request::HelmRead { namespace }
        | Request::HelmChange { namespace, .. } => namespace.as_deref(),
        _ => None,
    }
}

/// Asks the user to allow `raise` beyond the session's access (#15) and
/// records the answer. One question per session at a time; the policy is
/// asked again first, as for rights. "Allow by default" is the app's to
/// apply to the settings; the session only learns that it did.
pub(crate) async fn ask_access(
    ctx: &Ctx<'_>,
    request: &Request,
    raise: Access,
) -> Result<(), ToolError> {
    let _asking = ctx.session.asking().await;
    match ctx.session.policy().check(request) {
        Err(denial) if denial.code == DenialCode::NeedsAccess && denial.access == Some(raise) => {}
        _ => return Ok(()),
    }
    ask_access_now(ctx, named_namespace(request), raise).await
}

/// [`ask_access`] without the policy check, for callers that checked and
/// hold [`Session::asking`].
pub(crate) async fn ask_access_now(
    ctx: &Ctx<'_>,
    namespace: Option<&str>,
    raise: Access,
) -> Result<(), ToolError> {
    let scope = ctx.session.scope();
    let current = ctx.session.access();
    let namespace = namespace
        .map(str::to_owned)
        .or_else(|| ctx.session.policy().namespace(None).ok())
        .unwrap_or_default();
    let place = if namespace.is_empty() {
        String::new()
    } else {
        format!(" (namespace {namespace})")
    };
    let question = ConfirmationRequest {
        kind: RequestKind::Access {
            raise,
            current,
            server_url: scope.server_url.clone(),
        },
        session_id: ctx.session.id().to_owned(),
        context: scope.context.clone(),
        client: ctx.client_name.to_owned(),
        tool: ctx.tool.to_owned(),
        namespace: namespace.clone(),
        summary: format!(
            "{} asks for {raise} on {}{place}",
            ctx.client_name, scope.context
        ),
        details: format!(
            "Without asking, the agent has {current} on {context}. Allowing adds this until the \
             session ends; with \"Allow by default\" the context's minimal access is raised for \
             good. Either way every change, deletion and command is still confirmed one by one, \
             and nothing exceeds your own RBAC.",
            context = scope.context,
        ),
        dangerous: Vec::new(),
        mode: if namespace.is_empty() {
            ctx.session.mode()
        } else {
            ctx.session.mode_in(&namespace)
        },
        allow_remember: false,
    };
    let decision = ask(ctx.confirmer, &question).await;
    if !ctx.session.is_active() {
        return Err(Denial::new(
            DenialCode::SessionEnded,
            "the session ended while waiting for the user",
        )
        .into());
    }
    let record = |outcome: &'static str, reason: Option<String>| {
        sync::lock(&ctx.accesses).push(AccessDecision {
            raise,
            namespace: (!namespace.is_empty()).then(|| namespace.clone()),
            outcome,
            reason,
        });
    };
    match decision {
        ConfirmationDecision::Approved
        | ConfirmationDecision::ApprovedAlways
        | ConfirmationDecision::ApprovedForSession => {
            let always = decision == ConfirmationDecision::ApprovedAlways;
            if let Err(error) = ctx.session.decide_access(raise, true, always).await {
                let message = format!(
                    "the user allowed {raise}, but Kubach could not raise the session's access: \
                     {error}"
                );
                record("error", Some(message.clone()));
                return Err(ToolError::Failed(message));
            }
            record(if always { "always" } else { "session" }, None);
            Ok(())
        }
        ConfirmationDecision::Rejected { reason } => {
            let _ = ctx.session.decide_access(raise, false, false).await;
            record("refused", reason.clone());
            Err(ToolError::Rejected(Some(format!(
                "the user did not allow {raise} on {} in this session{}; Kubach does not ask \
                 again in this session",
                scope.context,
                reason.map(|r| format!(" ({r})")).unwrap_or_default()
            ))))
        }
        ConfirmationDecision::Denied { reason } => {
            record("no-answer", reason.clone());
            Err(ToolError::Rejected(Some(format!(
                "no answer from the user about {raise}{}",
                reason.map(|r| format!(" ({r})")).unwrap_or_default()
            ))))
        }
        ConfirmationDecision::Provided { .. } => {
            let reason = Some("unexpected answer to a question about access".to_owned());
            record("no-answer", reason.clone());
            Err(ToolError::Rejected(reason))
        }
    }
}

/// Refuses a call the user's own RBAC does not allow in its namespace:
/// `SelfSubjectAccessReview` under the user's credentials, cached for the
/// session. More access for the agent never adds RBAC rights, so this is
/// a plain refusal, not a question.
pub(crate) async fn ensure_rbac(
    ctx: &Ctx<'_>,
    namespace: &str,
    accesses: &[(&str, &str, &str)],
) -> Result<(), ToolError> {
    for (verb, group, resource) in accesses {
        let access = ResourceAccess::new(*verb, *group, resource).in_namespace(namespace);
        if !ctx.session.user_can(&access).await {
            let full = if group.is_empty() {
                (*resource).to_owned()
            } else {
                format!("{resource}.{group}")
            };
            return Err(Denial::new(
                DenialCode::Rbac,
                format!(
                    "your Kubernetes account has no right to {verb} {full} in namespace \
                     {namespace}; Kubach cannot grant RBAC rights. Ask the cluster administrator, \
                     or work in a namespace where the account has them"
                ),
            )
            .into());
        }
    }
    Ok(())
}

/// Tells the user that the agent wanted a Secret's values on a context
/// where revealing is off. Not a question: the UI answers at once.
async fn tell_reveal_blocked(ctx: &Ctx<'_>, request: &Request) {
    let Request::SecretReveal {
        namespace, name, ..
    } = request
    else {
        return;
    };
    let Ok(namespace) = ctx.session.policy().namespace(namespace.as_deref()) else {
        return;
    };
    let context = ctx.session.scope().context.clone();
    let notice = ConfirmationRequest {
        kind: RequestKind::RevealBlocked { name: name.clone() },
        session_id: ctx.session.id().to_owned(),
        context: context.clone(),
        client: ctx.client_name.to_owned(),
        tool: ctx.tool.to_owned(),
        summary: format!(
            "{} wanted to reveal Secret {namespace}/{name}; revealing is off on context {context}",
            ctx.client_name
        ),
        mode: ctx.session.mode_in(&namespace),
        namespace,
        details: String::new(),
        dangerous: Vec::new(),
        allow_remember: false,
    };
    let _ = tokio::time::timeout(Duration::from_secs(10), ctx.confirmer.confirm(&notice)).await;
}

// ------------------------------------------------------------- dispatch

/// Runs one tool call.
pub(crate) async fn run(
    ctx: &Ctx<'_>,
    tool: &str,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    match tool {
        "session_info" => {
            parse::<NoArgs>(args)?;
            authorize(ctx, &Request::Info).await?;
            Ok(ToolOutput::read(session_info(ctx.session)))
        }
        "list_kinds" => {
            parse::<NoArgs>(args)?;
            authorize(ctx, &Request::ListKinds).await?;
            Ok(ToolOutput::read(list_kinds(ctx.session)))
        }
        "list_namespaces" => {
            parse::<NoArgs>(args)?;
            authorize(ctx, &Request::ListNamespaces).await?;
            Ok(ToolOutput::read(list_namespaces(ctx.session).await))
        }
        "list_resources" => {
            let a: ListArgs = parse(args)?;
            let request = Request::List {
                kind: kind_ref(a.kind, a.api_version),
                namespace: a.namespace,
                all_namespaces: a.all_namespaces,
                label_selector: a.label_selector.clone(),
            };
            let allowed = authorize(ctx, &request).await?;
            list_resources(ctx, &allowed, a.label_selector.as_deref(), a.limit).await
        }
        "get_resource" | "describe_resource" => {
            let a: ObjectArgs = parse(args)?;
            let request = Request::Get {
                kind: kind_ref(a.kind, a.api_version),
                name: a.name,
                namespace: a.namespace,
            };
            let allowed = authorize(ctx, &request).await?;
            if tool == "get_resource" {
                get_resource(ctx, &allowed).await
            } else {
                describe_resource(ctx, &allowed).await
            }
        }
        "list_events" => {
            let a: EventsArgs = parse(args)?;
            let object = match (a.kind, a.name) {
                (Some(kind), Some(name)) => Some((kind_ref(kind, a.api_version), name)),
                (None, None) => None,
                _ => {
                    return Err(Denial::new(
                        DenialCode::InvalidInput,
                        "pass both kind and name, or neither",
                    )
                    .into());
                }
            };
            let request = Request::Events {
                namespace: a.namespace,
                object,
            };
            let allowed = authorize(ctx, &request).await?;
            ensure_rbac(ctx, &allowed.namespace, &[("list", "", "events")]).await?;
            let events = list_events(ctx, &allowed, a.limit.unwrap_or(50).min(200)).await?;
            Ok(ToolOutput::read(events))
        }
        "get_logs" => {
            let a: LogsArgs = parse(args)?;
            let request = Request::Logs {
                pod: a.pod.clone(),
                container: a.container.clone(),
                namespace: a.namespace.clone(),
            };
            let allowed = authorize(ctx, &request).await?;
            ensure_rbac(ctx, &allowed.namespace, &[("get", "", "pods/log")]).await?;
            get_logs(ctx, &allowed, &a).await
        }
        "top_pods" => {
            let a: NamespaceArgs = parse(args)?;
            let request = Request::Top {
                namespace: a.namespace,
            };
            let allowed = authorize(ctx, &request).await?;
            ensure_rbac(
                ctx,
                &allowed.namespace,
                &[("list", "metrics.k8s.io", "pods")],
            )
            .await?;
            top_pods(ctx, &allowed).await
        }
        "get_topology" => topology_tools::get_topology(ctx, args).await,
        "wait_for_rollout" => crate::rollout_tools::wait_for_rollout(ctx, args).await,
        "helm_search_charts" => helm_tools::search(ctx, args).await,
        "helm_show_values" => helm_tools::show_values(ctx, args).await,
        "helm_list_releases" => helm_tools::list_releases(ctx, args).await,
        "helm_release_history" => helm_tools::history(ctx, args).await,
        "helm_get_values" => helm_tools::get_values(ctx, args).await,
        "helm_install" => helm_tools::install(ctx, args).await,
        "helm_upgrade" => helm_tools::upgrade(ctx, args).await,
        "helm_rollback" => helm_tools::rollback(ctx, args).await,
        "helm_uninstall" => helm_tools::uninstall(ctx, args).await,
        "can_reach" => topology_tools::can_reach(ctx, args).await,
        "apply_manifest" => {
            let a: ApplyArgs = parse(args)?;
            let dry_run = a.dry_run.unwrap_or(true);
            let request = Request::Apply {
                yaml: a.yaml,
                namespace: a.namespace,
                dry_run,
            };
            let allowed = authorize(ctx, &request).await?;
            apply_manifest(ctx, &allowed, dry_run).await
        }
        "scale" => {
            let a: ScaleArgs = parse(args)?;
            let request = Request::Scale {
                kind: kind_ref(a.kind, a.api_version),
                name: a.name,
                namespace: a.namespace,
                replicas: a.replicas,
            };
            let allowed = authorize(ctx, &request).await?;
            let kind = kind_of(&allowed);
            let scale = format!("{}/scale", kind.api.plural);
            ensure_rbac(
                ctx,
                &allowed.namespace,
                &[("patch", &kind.api.group, &scale)],
            )
            .await?;
            let mut params = ActionParams::new();
            params.insert(action::REPLICAS.into(), json!(a.replicas));
            let summary = format!("to {} replicas", a.replicas);
            run_action(ctx, &allowed, "scale", action::SCALE, &params, &summary).await
        }
        "rollout_restart" | "delete_resource" => {
            let a: ObjectArgs = parse(args)?;
            let kind = kind_ref(a.kind, a.api_version);
            let request = if tool == "rollout_restart" {
                Request::Restart {
                    kind,
                    name: a.name,
                    namespace: a.namespace,
                }
            } else {
                Request::Delete {
                    kind,
                    name: a.name,
                    namespace: a.namespace,
                }
            };
            let allowed = authorize(ctx, &request).await?;
            let kind = kind_of(&allowed);
            let verb = if tool == "rollout_restart" {
                "patch"
            } else {
                "delete"
            };
            ensure_rbac(
                ctx,
                &allowed.namespace,
                &[(verb, &kind.api.group, &kind.api.plural)],
            )
            .await?;
            let id = if tool == "rollout_restart" {
                action::RESTART
            } else {
                action::DELETE
            };
            run_action(ctx, &allowed, tool, id, &ActionParams::new(), "").await
        }
        "exec" => {
            let a: ExecArgs = parse(args)?;
            let request = Request::Exec {
                pod: a.pod,
                container: a.container.clone(),
                namespace: a.namespace,
                command: a.command.clone(),
            };
            let allowed = authorize(ctx, &request).await?;
            ensure_rbac(ctx, &allowed.namespace, &[("create", "", "pods/exec")]).await?;
            let timeout = a
                .timeout_seconds
                .unwrap_or(DEFAULT_EXEC_TIMEOUT)
                .clamp(1, MAX_EXEC_TIMEOUT);
            exec(ctx, &allowed, a.container.as_deref(), &a.command, timeout).await
        }
        "port_forward_start" => crate::forwards::start(ctx, args).await,
        "port_forward_list" => crate::forwards::list(ctx, args).await,
        "port_forward_stop" => crate::forwards::stop(ctx, args).await,
        "list_secrets" => {
            let a: SecretListArgs = parse(args)?;
            let request = Request::SecretList {
                namespace: a.namespace,
                name: a.name,
            };
            let allowed = authorize(ctx, &request).await?;
            secret_tools::list(ctx, &allowed)
                .await
                .map(ToolOutput::read)
        }
        "check_secret" => {
            let a: SecretArgs = parse(args)?;
            let request = Request::SecretCheck {
                namespace: a.namespace,
                name: a.name,
                key: a.key,
            };
            let allowed = authorize(ctx, &request).await?;
            secret_tools::check(ctx, &allowed)
                .await
                .map(ToolOutput::read)
        }
        "compare_secret_values" => {
            let a: CompareArgs = parse(args)?;
            let request = Request::SecretCompare {
                namespace: a.namespace,
                name: a.name,
                key: a.key,
                other_namespace: a.other_namespace,
                other_name: a.other_name,
                other_key: a.other_key,
            };
            let allowed = authorize(ctx, &request).await?;
            secret_tools::compare(ctx, &allowed)
                .await
                .map(ToolOutput::read)
        }
        "get_secret_value" => {
            let a: SecretArgs = parse(args)?;
            let request = Request::SecretReveal {
                namespace: a.namespace,
                name: a.name,
                key: a.key,
            };
            let allowed = authorize(ctx, &request).await?;
            secret_tools::reveal(ctx, &allowed)
                .await
                .map(ToolOutput::confirmed)
        }
        "request_secret_input" => {
            let a: secret_write::InputArgs = parse(args)?;
            let request = Request::SecretWrite {
                namespace: a.namespace,
                name: a.name,
                mode: a.mode,
                secret_type: a.secret_type,
                keys: a.keys,
                remove: a.remove_keys,
                explanation: a.explanation,
            };
            let allowed = authorize(ctx, &request).await?;
            let verb = match a.mode {
                secret_write::SecretOp::Create => "create",
                _ => "patch",
            };
            ensure_rbac(ctx, &allowed.namespace, &[(verb, "", "secrets")]).await?;
            secret_write::input(ctx, &allowed, args).await
        }
        "copy_secret" => {
            let a: secret_write::CopyArgs = parse(args)?;
            let request = Request::SecretCopy {
                namespace: a.namespace,
                name: a.name,
                to_namespace: a.to_namespace,
                to_name: a.to_name,
                explanation: a.explanation,
            };
            let allowed = authorize(ctx, &request).await?;
            let target = allowed
                .to
                .as_ref()
                .map_or(allowed.namespace.clone(), |to| to.namespace.clone());
            ensure_rbac(ctx, &target, &[("create", "", "secrets")]).await?;
            secret_write::copy(ctx, &allowed, args).await
        }
        "delete_secret" => {
            let a: secret_write::DeleteArgs = parse(args)?;
            let request = Request::SecretDelete {
                namespace: a.namespace,
                name: a.name,
                explanation: a.explanation,
            };
            let allowed = authorize(ctx, &request).await?;
            ensure_rbac(ctx, &allowed.namespace, &[("delete", "", "secrets")]).await?;
            secret_write::delete(ctx, &allowed, args).await
        }
        other => {
            Err(Denial::new(DenialCode::InvalidInput, format!("unknown tool `{other}`")).into())
        }
    }
}

// ------------------------------------------------------------- helpers

pub(crate) fn kind_of(allowed: &Allowed) -> &ResourceKind {
    allowed.kind.as_ref().expect("policy resolved the kind")
}

fn name_of(allowed: &Allowed) -> &str {
    allowed.name.as_deref().expect("policy validated the name")
}

/// Defense in depth: objects returned by a namespaced endpoint must be in
/// that namespace.
fn ensure_namespace(obj: &DynamicObject, namespace: &str) -> Result<(), ToolError> {
    if obj.metadata.namespace.as_deref() == Some(namespace) {
        Ok(())
    } else {
        Err(ToolError::Failed(format!(
            "the API returned an object outside {namespace}; refusing to use it"
        )))
    }
}

pub(crate) async fn fetch(client: &Client, allowed: &Allowed) -> Result<DynamicObject, ToolError> {
    let api = discovery::api(client, kind_of(allowed), Some(&allowed.namespace));
    let obj = api.get(name_of(allowed)).await?;
    ensure_namespace(&obj, &allowed.namespace)?;
    Ok(obj)
}

fn masked_value(obj: &DynamicObject) -> Value {
    let mut value = serde_json::to_value(obj).unwrap_or(Value::Null);
    mask_object(&mut value);
    value
}

/// Final masking and size bound of any text shown to the agent.
pub(crate) fn finish(text: &str) -> String {
    truncate(&mask_text(text), MAX_OUTPUT_BYTES).into_owned()
}

fn render_cell(cell: &Cell, now: Timestamp) -> String {
    let text = match cell {
        Cell::Text(s) => s.clone(),
        Cell::Number(n) => n.to_string(),
        Cell::Age(Some(t)) => format_age(now.as_second() - t.as_second()),
        Cell::Age(None) => String::new(),
        Cell::CountAge(count, t) => {
            format!(
                "{count} ({} ago)",
                format_age(now.as_second() - t.as_second())
            )
        }
        Cell::Status(health) if health.message.is_empty() => format!("{:?}", health.status),
        Cell::Status(health) => format!("{:?} ({})", health.status, health.message),
        // `migrate=Terminated: Completed (exit code 0), app=Running`.
        Cell::Containers(containers) => containers
            .iter()
            .map(|c| format!("{}={}", c.name, c.state))
            .collect::<Vec<_>>()
            .join(", "),
    };
    text.replace(['\t', '\n'], " ")
}

// ------------------------------------------------------------- read tools

fn session_info(session: &Session) -> String {
    let scope = session.scope();
    // Per namespace: who holds the boundary there (#401).
    let boundaries: Vec<Value> = session
        .boundaries()
        .into_iter()
        .map(|b| {
            json!({
                "namespace": b.namespace,
                "mode": b.mode,
                "serverEnforced": b.mode.server_enforced(),
                "boundary": if b.mode.server_enforced() {
                    "Kubach and the API server"
                } else {
                    "only in Kubach"
                },
                "identity": b.identity,
                "notes": b.notes,
                "warning": b.warning,
            })
        })
        .collect();
    let info = json!({
        "session": scope.id,
        "context": scope.context,
        "namespaces": session.namespaces(),
        "pinnedNamespaces": scope.pinned(),
        "access": {
            "minimum": session.minimum(),
            "now": session.access(),
            "raisedForThisSession": session.raises().granted,
            "savedToSettings": session.raises().saved,
            "launchProfileCap": scope.profile,
            "howToRaise": "Tools beyond the access are marked \"Requires your approval in \
                Kubach\": call them and the user is asked to allow it, for this session or by \
                default on this cluster. Changes are still confirmed one by one.",
        },
        "secretValues": if scope.policy.reveal_allowed() {
            "revealed one Secret at a time after the user confirms it"
        } else {
            "hidden on this context"
        },
        "boundaries": boundaries,
        "expiresAt": scope.expires_at.to_string(),
        "grants": session.grants().entries(),
        "notAskedAgain": session.remembered(),
        "notAskedAgainRules": "The user may tick \"don't ask again\" in Kubach for one kind of \
            change in one namespace (scale, rollout_restart, apply_manifest, exec, \
            port_forward_start, helm_install, helm_upgrade, helm_rollback); such calls then run \
            without a window until the session ends, and the result says so. Deleting, Secrets, \
            Helm with the user's credentials or beyond its namespace and changes with dangerous \
            fields are always asked.",
        "namespaceRules": "Name any namespace in a call (list_namespaces shows the ones Kubach \
            knows of); reading built-in resources there needs no question. Revealing one Secret \
            is asked for that Secret; a refusal holds until the session ends.",

    });
    serde_json::to_string_pretty(&info).unwrap_or_default()
}

/// `list_namespaces`: names only (#401). Listed with the user's
/// credentials when they may; else the namespaces Kubach knows of for the
/// context, each with its source. Either way with what the session may do
/// in each.
async fn list_namespaces(session: &Session) -> String {
    use k8s_openapi::api::core::v1::Namespace;
    let scope = session.scope();
    let mut found: Vec<(String, NamespaceSource)> = Vec::new();
    let mut add = |name: &str, source: NamespaceSource| {
        if !found.iter().any(|(known, _)| known == name) {
            found.push((name.to_owned(), source));
        }
    };
    let (complete, note) = if scope.pinned() {
        for namespace in &scope.namespaces {
            add(namespace, NamespaceSource::LaunchArguments);
        }
        (
            true,
            "This session was started for these namespaces only.".to_owned(),
        )
    } else {
        let api: Api<Namespace> = Api::all(session.user_client());
        let listed = api
            .list_metadata(&ListParams::default().limit(MAX_LIST_ITEMS))
            .await;
        match listed {
            Ok(list) => {
                for namespace in list.items.iter().filter_map(|n| n.metadata.name.as_deref()) {
                    add(namespace, NamespaceSource::Cluster);
                }
                let more = list
                    .metadata
                    .continue_
                    .as_deref()
                    .is_some_and(|c| !c.is_empty());
                (
                    !more,
                    if more {
                        format!("Only the first {MAX_LIST_ITEMS} namespaces.")
                    } else {
                        "All namespaces of the cluster.".to_owned()
                    },
                )
            }
            Err(error) => {
                let reason = ToolError::from(error).message();
                for hint in &scope.hints {
                    add(&hint.name, hint.source);
                }
                for namespace in session.namespaces() {
                    add(&namespace, NamespaceSource::Session);
                }
                (
                    false,
                    format!(
                        "The cluster does not let you list namespaces ({reason}); these are the \
                         ones Kubach knows of. Another namespace can still be named in a call."
                    ),
                )
            }
        }
    };
    let grants = session.grants();
    let policy = session.policy();
    let namespaces: Vec<Value> = found
        .into_iter()
        .map(|(name, source)| {
            let access = match grants.state(&Grant::read(name.clone())) {
                Some(crate::grants::GrantState::Granted) => "allowed",
                Some(crate::grants::GrantState::Denied) => "refused",
                None if policy.namespace(Some(&name)).is_ok() => "available",
                None => "not-in-this-session",
            };
            json!({"name": name, "source": source, "access": access})
        })
        .collect();
    let out = json!({
        "namespaces": namespaces,
        "complete": complete,
        "note": note,
        "howToUse": "Pass `namespace` to a tool: reading built-in resources needs no question, \
            in as many namespaces as you need.",
    });
    serde_json::to_string_pretty(&out).unwrap_or_default()
}

fn list_kinds(session: &Session) -> String {
    let policy = session.policy();
    let mut out = String::from("KIND\tAPIVERSION\tRESOURCE\n");
    for kind in session.catalog().kinds() {
        let reference = KindRef {
            kind: kind.api.kind.clone(),
            api_version: Some(kind.api.api_version.clone()),
        };
        if policy.read_kind(&reference).is_ok() {
            let _ = writeln!(
                out,
                "{}\t{}\t{}",
                kind.api.kind, kind.api.api_version, kind.api.plural
            );
        }
    }
    out
}

async fn list_resources(
    ctx: &Ctx<'_>,
    allowed: &Allowed,
    label_selector: Option<&str>,
    limit: Option<u32>,
) -> Result<ToolOutput, ToolError> {
    let kind = kind_of(allowed);
    let client = ctx.session.client_in(&allowed.namespace)?;
    let api = discovery::api(&client, kind, Some(&allowed.namespace));
    let mut params = ListParams::default().limit(limit.unwrap_or(100).clamp(1, MAX_LIST_ITEMS));
    if let Some(selector) = label_selector {
        params = params.labels(selector);
    }
    let list = api.list(&params).await?;
    let plugin = ctx.registry.plugin_for_resource(&kind.api);
    let columns = plugin.columns();
    let now = Timestamp::now();
    let mut out = columns
        .iter()
        .map(|c| c.title.to_uppercase())
        .collect::<Vec<_>>()
        .join("\t");
    out.push('\n');
    let mut shown = 0;
    for obj in &list.items {
        if ensure_namespace(obj, &allowed.namespace).is_err() {
            continue;
        }
        let cells: Vec<String> = plugin
            .cells(obj)
            .iter()
            .map(|c| render_cell(c, now))
            .collect();
        out.push_str(&cells.join("\t"));
        out.push('\n');
        shown += 1;
    }
    let _ = write!(
        out,
        "({shown} {} in {})",
        kind.api.plural, allowed.namespace
    );
    if has_more(&list.metadata) {
        out.push_str(" — more items exist; raise `limit` or narrow `labelSelector`");
    }
    Ok(ToolOutput::read(finish(&out)))
}

/// A list answer cut by its `limit`: the server has more items.
fn has_more(metadata: &ListMeta) -> bool {
    metadata.continue_.as_deref().is_some_and(|c| !c.is_empty())
}

async fn get_resource(ctx: &Ctx<'_>, allowed: &Allowed) -> Result<ToolOutput, ToolError> {
    let obj = fetch(&ctx.session.client_in(&allowed.namespace)?, allowed).await?;
    Ok(ToolOutput::read(finish(&core_apply::to_yaml(
        &masked_value(&obj),
    ))))
}

async fn describe_resource(ctx: &Ctx<'_>, allowed: &Allowed) -> Result<ToolOutput, ToolError> {
    let client = ctx.session.client_in(&allowed.namespace)?;
    let obj = fetch(&client, allowed).await?;
    let kind = kind_of(allowed);
    let plugin = ctx.registry.plugin_for_resource(&kind.api);
    let health = plugin.health(&obj);
    let mut out = format!(
        "{} {}/{}\nHealth: {:?}{}\n",
        kind.api.kind,
        allowed.namespace,
        obj.name_any(),
        health.status,
        if health.message.is_empty() {
            String::new()
        } else {
            format!(" ({})", health.message)
        }
    );
    for section in plugin.details(&obj) {
        let _ = writeln!(out, "\n{}:", section.title);
        for line in section.text_lines() {
            let _ = writeln!(out, "  {line}");
        }
    }
    let relations = plugin.relations(&obj);
    if !relations.is_empty() {
        out.push_str("\nRelations:\n");
        for relation in relations {
            let _ = writeln!(
                out,
                "  {:?} {} {}",
                relation.kind, relation.target.kind, relation.target.name
            );
        }
    }
    for relation in plugin.selector_relations(&obj) {
        let _ = writeln!(
            out,
            "  {:?} {} with {}",
            relation.kind, relation.target_kind, relation.label_selector
        );
    }
    let events = list_events(ctx, allowed, 20)
        .await
        .unwrap_or_else(|e| e.message());
    let _ = write!(out, "\nEvents:\n{events}");
    Ok(ToolOutput::read(finish(&out)))
}

async fn list_events(ctx: &Ctx<'_>, allowed: &Allowed, limit: usize) -> Result<String, ToolError> {
    let api: Api<Event> = Api::namespaced(
        ctx.session.client_in(&allowed.namespace)?,
        &allowed.namespace,
    );
    let mut params = ListParams::default().limit(MAX_LIST_ITEMS);
    if let (Some(kind), Some(name)) = (&allowed.kind, &allowed.name) {
        params = params.fields(&format!(
            "involvedObject.kind={},involvedObject.name={name}",
            kind.api.kind
        ));
    }
    let list = api.list(&params).await?;
    let more = has_more(&list.metadata);
    let mut events = list.items;
    let when = |e: &Event| {
        e.last_timestamp
            .as_ref()
            .map(|t| t.0)
            .or(e.event_time.as_ref().map(|t| t.0))
            .or(e.metadata.creation_timestamp.as_ref().map(|t| t.0))
    };
    events.sort_by_key(when);
    let now = Timestamp::now();
    let skip = events.len().saturating_sub(limit);
    let mut out = String::new();
    for event in events.iter().skip(skip) {
        if event.metadata.namespace.as_deref() != Some(allowed.namespace.as_str()) {
            continue;
        }
        let age = when(event)
            .map(|t| format_age(now.as_second() - t.as_second()))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "{age}\t{}\t{}\t{}/{}\t{}{}",
            event.type_.as_deref().unwrap_or(""),
            event.reason.as_deref().unwrap_or(""),
            event.involved_object.kind.as_deref().unwrap_or(""),
            event.involved_object.name.as_deref().unwrap_or(""),
            event.message.as_deref().unwrap_or("").trim(),
            event
                .count
                .filter(|c| *c > 1)
                .map(|c| format!(" (x{c})"))
                .unwrap_or_default(),
        );
    }
    if out.is_empty() {
        out.push_str("(no events)\n");
    }
    if more {
        let _ = writeln!(
            out,
            "(only {MAX_LIST_ITEMS} events were read; more exist and newer ones may be missing)"
        );
    }
    Ok(finish(&out))
}

async fn get_logs(
    ctx: &Ctx<'_>,
    allowed: &Allowed,
    args: &LogsArgs,
) -> Result<ToolOutput, ToolError> {
    let api: Api<Pod> = Api::namespaced(
        ctx.session.client_in(&allowed.namespace)?,
        &allowed.namespace,
    );
    let params = LogParams {
        container: args.container.clone(),
        follow: false,
        limit_bytes: Some(MAX_LOG_BYTES),
        pretty: false,
        previous: args.previous,
        since_seconds: args.since_seconds.map(|s| s.clamp(1, 7 * 24 * 3600)),
        since_time: None,
        tail_lines: Some(
            args.tail_lines
                .unwrap_or(DEFAULT_LOG_LINES)
                .clamp(1, MAX_LOG_LINES),
        ),
        timestamps: false,
    };
    let pod = name_of(allowed);
    // Logs of a pod the agent itself shaped are the shortest way out for
    // what its manifest read (#190, ADR 0005): confirmed one by one.
    let confirmed = match agent_shaped(ctx, &api, &allowed.namespace, pod).await {
        Some(reason) => {
            let mut confirm_plan = allowed.clone();
            confirm_plan.confirmation = Confirmation::Required;
            confirm(
                ctx,
                &confirm_plan,
                "get_logs",
                format!("Read the logs of pod {}/{pod}", allowed.namespace),
                format!(
                    "{reason}\n\nThe logs carry whatever the agent's manifest prints: values \
                     of the Secrets it reads and ServiceAccount tokens it mounts. Kubach masks \
                     well-known secret formats in logs, best effort only."
                ),
                Vec::new(),
            )
            .await?;
            true
        }
        None => false,
    };
    let logs = api.logs(pod, &params).await?;
    let text = finish(&logs);
    Ok(if confirmed {
        ToolOutput::confirmed(text)
    } else {
        ToolOutput::read(text)
    })
}

/// Why the logs of `pod` need the user's yes: the agent created or changed
/// the pod, its ReplicaSet, Deployment, StatefulSet, DaemonSet, Job or
/// CronJob, or a ConfigMap it uses, in this session. `None` without such a
/// change, and at once when the agent changed nothing in `namespace`.
async fn agent_shaped(
    ctx: &Ctx<'_>,
    pods: &Api<Pod>,
    namespace: &str,
    pod: &str,
) -> Option<String> {
    let session = ctx.session;
    if !session.changed_in(namespace) {
        return None;
    }
    let changed = |kind: &str, name: &str| {
        session
            .was_changed(namespace, kind, name)
            .then(|| format!("This agent created or changed {kind} {name} in this session."))
    };
    if let Some(reason) = changed("Pod", pod) {
        return Some(reason);
    }
    let object = match pods.get(pod).await {
        Ok(object) => object,
        Err(error) => {
            return Some(format!(
                "This agent changed objects in {namespace} in this session, and Kubach could \
                 not read pod {pod} to tell whether it is one of them: {}",
                ToolError::from(error).message()
            ));
        }
    };
    let client = match session.client_in(namespace) {
        Ok(client) => client,
        Err(error) => return Some(format!("Kubach cannot check pod {pod}: {error}")),
    };
    for owner in object.metadata.owner_references.iter().flatten() {
        if let Some(reason) = changed(&owner.kind, &owner.name) {
            return Some(reason);
        }
        // One level up: Deployment → ReplicaSet, CronJob → Job.
        let parents = match owner.kind.as_str() {
            "ReplicaSet" => owners_of::<ReplicaSet>(&client, namespace, &owner.name).await,
            "Job" => owners_of::<Job>(&client, namespace, &owner.name).await,
            _ => Ok(Vec::new()),
        };
        match parents {
            Ok(parents) => {
                for parent in parents {
                    if let Some(reason) = changed(&parent.kind, &parent.name) {
                        return Some(reason);
                    }
                }
            }
            Err(message) => {
                return Some(format!(
                    "This agent changed objects in {namespace} in this session, and Kubach \
                     could not read {} {} to tell whether pod {pod} belongs to one of them: \
                     {message}",
                    owner.kind, owner.name
                ));
            }
        }
    }
    let spec = serde_json::to_value(&object.spec).unwrap_or(Value::Null);
    let mut config_maps = Vec::new();
    walk_fields(&spec, &mut String::new(), &mut |_, key, child| {
        if matches!(key, "configMap" | "configMapRef" | "configMapKeyRef")
            && let Some(name) = child.get("name").and_then(Value::as_str)
        {
            config_maps.push(name.to_owned());
        }
    });
    config_maps
        .iter()
        .find_map(|name| changed("ConfigMap", name))
        .map(|reason| format!("{reason} Pod {pod} uses it."))
}

/// The owners of `name`; a vanished object has none.
async fn owners_of<K>(
    client: &Client,
    namespace: &str,
    name: &str,
) -> Result<Vec<OwnerReference>, String>
where
    K: kube::Resource<Scope = k8s_openapi::NamespaceResourceScope>
        + Clone
        + DeserializeOwned
        + std::fmt::Debug,
    K::DynamicType: Default,
{
    let api: Api<K> = Api::namespaced(client.clone(), namespace);
    match api.get_opt(name).await {
        Ok(Some(object)) => Ok(object.meta().owner_references.clone().unwrap_or_default()),
        Ok(None) => Ok(Vec::new()),
        Err(error) => Err(ToolError::from(error).message()),
    }
}

async fn top_pods(ctx: &Ctx<'_>, allowed: &Allowed) -> Result<ToolOutput, ToolError> {
    let Some(kind) = ctx.session.catalog().find("metrics.k8s.io", "PodMetrics") else {
        return Err(ToolError::Failed(
            "metrics-server is not installed in this cluster".into(),
        ));
    };
    let api = discovery::api(
        &ctx.session.client_in(&allowed.namespace)?,
        kind,
        Some(&allowed.namespace),
    );
    let list = api
        .list(&ListParams::default().limit(MAX_LIST_ITEMS))
        .await?;
    let mut out = String::from("POD\tCONTAINER\tCPU\tMEMORY\n");
    for pod in &list.items {
        if pod.metadata.namespace.as_deref() != Some(allowed.namespace.as_str()) {
            continue;
        }
        for container in pod.data["containers"].as_array().into_iter().flatten() {
            let _ = writeln!(
                out,
                "{}\t{}\t{}\t{}",
                pod.name_any(),
                container["name"].as_str().unwrap_or(""),
                container["usage"]["cpu"].as_str().unwrap_or(""),
                container["usage"]["memory"].as_str().unwrap_or(""),
            );
        }
    }
    if has_more(&list.metadata) {
        let _ = writeln!(
            out,
            "(only the first {MAX_LIST_ITEMS} pods are shown; more exist)"
        );
    }
    Ok(ToolOutput::read(finish(&out)))
}

// ------------------------------------------------------------- confirmation

async fn confirm(
    ctx: &Ctx<'_>,
    allowed: &Allowed,
    tool: &str,
    summary: String,
    details: String,
    dangerous: Vec<Finding>,
) -> Result<(), ToolError> {
    confirm_as(
        ctx,
        RequestKind::Call,
        allowed,
        tool,
        summary,
        details,
        dangerous,
    )
    .await
}

pub(crate) async fn confirm_as(
    ctx: &Ctx<'_>,
    kind: RequestKind,
    allowed: &Allowed,
    tool: &str,
    summary: String,
    details: String,
    dangerous: Vec<Finding>,
) -> Result<(), ToolError> {
    debug_assert_eq!(allowed.confirmation, Confirmation::Required);
    let request = ConfirmationRequest {
        kind,
        session_id: ctx.session.id().to_owned(),
        context: ctx.session.scope().context.clone(),
        client: ctx.client_name.to_owned(),
        tool: tool.to_owned(),
        namespace: allowed.namespace.clone(),
        summary,
        details,
        dangerous,
        mode: ctx.session.mode_in(&allowed.namespace),
        allow_remember: false,
    };
    approve_change(
        ctx,
        request,
        "the session ended while waiting for confirmation",
    )
    .await
}

/// Asks the user to approve a change, unless they said not to ask again
/// about this kind of change in this namespace in this session (#30).
/// [`rememberable`] decides whether the request offers that at all; a
/// "don't ask again" ticked now is kept unless something was forgotten
/// while the user decided.
pub(crate) async fn approve_change(
    ctx: &Ctx<'_>,
    mut request: ConfirmationRequest,
    ended: &str,
) -> Result<(), ToolError> {
    let remember = rememberable(&request);
    request.allow_remember = remember.is_some();
    if let Some(remembered) = &remember
        && ctx.session.is_remembered(remembered)
    {
        *sync::lock(&ctx.remembered) = Some(RememberNote::Used(remembered.clone()));
        return Ok(());
    }
    let epoch = ctx.session.remember_epoch();
    let decision = ask(ctx.confirmer, &request).await;
    // The session may have ended while the user was deciding.
    if !ctx.session.is_active() {
        return Err(Denial::new(DenialCode::SessionEnded, ended).into());
    }
    match decision {
        ConfirmationDecision::ApprovedForSession => {
            if let Some(remembered) = remember
                && ctx.session.remember(remembered.clone(), epoch)
            {
                *sync::lock(&ctx.remembered) = Some(RememberNote::Saved(remembered));
            }
            Ok(())
        }
        ConfirmationDecision::Approved | ConfirmationDecision::ApprovedAlways => Ok(()),
        ConfirmationDecision::Denied { reason } | ConfirmationDecision::Rejected { reason } => {
            Err(ToolError::Rejected(reason))
        }
        // Values answer only Secret forms (#230).
        ConfirmationDecision::Provided { .. } => Err(ToolError::Rejected(Some(
            "unexpected answer to a confirmation".into(),
        ))),
    }
}

fn findings_text(findings: &[Finding]) -> String {
    let mut out = String::new();
    for f in findings {
        let doc = f
            .document
            .map(|d| format!("document {}: ", d + 1))
            .unwrap_or_default();
        let _ = writeln!(out, "  ! {doc}{} {}", f.path, f.message);
    }
    out
}

// ------------------------------------------------------------- mutating tools

async fn apply_manifest(
    ctx: &Ctx<'_>,
    allowed: &Allowed,
    dry_run: bool,
) -> Result<ToolOutput, ToolError> {
    let client = ctx.session.client_in(&allowed.namespace)?;
    let options = ApplyOptions {
        field_manager: FIELD_MANAGER.into(),
        force: false,
        dry_run: true,
    };
    // Always dry-run first, under the session identity: the diff and RBAC
    // errors the user sees are the real ones.
    let mut diffs = String::new();
    let mut findings = allowed.findings.clone();
    for manifest in &allowed.manifests {
        let outcome = core_apply::apply(&client, manifest, &options)
            .await
            .map_err(apply_error)?;
        let before = outcome.before.as_ref().map(masked_value);
        let after = masked_value(&outcome.after);
        let raw_before = outcome
            .before
            .as_ref()
            .and_then(|b| serde_json::to_value(b).ok());
        let raw_after = serde_json::to_value(&outcome.after).unwrap_or(Value::Null);
        // The policy already reports every ServiceAccount the manifest
        // names; this adds what only the live object shows.
        let changes: Vec<Finding> = service_account_changes(raw_before.as_ref(), &raw_after)
            .into_iter()
            .map(|mut f| {
                f.document = Some(manifest.index);
                f
            })
            .filter(|f| {
                !findings
                    .iter()
                    .any(|known| known.document == f.document && known.path == f.path)
            })
            .collect();
        findings.extend(changes);
        let verb = if outcome.created() {
            "create"
        } else {
            "update"
        };
        let _ = writeln!(diffs, "# {} ({verb})", manifest.label());
        let diff = yaml_diff(before.as_ref(), &after);
        diffs.push_str(if diff.is_empty() {
            "(no changes)\n"
        } else {
            &diff
        });
    }
    let diffs = finish(&diffs);
    if dry_run {
        let mut out = format!("Dry run (nothing was changed):\n{diffs}");
        if !findings.is_empty() {
            let _ = write!(
                out,
                "\nA real apply would need explicit confirmation of:\n{}",
                findings_text(&findings)
            );
        }
        return Ok(ToolOutput::read(out));
    }

    let summary = format!(
        "Apply {} object(s) in {}: {}",
        allowed.manifests.len(),
        allowed.namespace,
        allowed
            .manifests
            .iter()
            .map(|m| m.label())
            .collect::<Vec<_>>()
            .join(", ")
    );
    confirm(
        ctx,
        allowed,
        "apply_manifest",
        summary,
        diffs.clone(),
        findings,
    )
    .await?;
    let options = ApplyOptions {
        dry_run: false,
        ..options
    };
    let mut out = String::from("Applied:\n");
    for manifest in &allowed.manifests {
        let kind = manifest.kind.api.kind.as_str();
        // Noted before the call: a failed answer may still have changed it.
        if !manifest.name.is_empty() {
            ctx.session
                .note_change(&allowed.namespace, kind, &manifest.name);
        }
        let outcome = core_apply::apply(&client, manifest, &options)
            .await
            .map_err(apply_error)?;
        ctx.session
            .note_change(&allowed.namespace, kind, &outcome.after.name_any());
        let verb = if outcome.created() {
            "created"
        } else {
            "configured"
        };
        let _ = writeln!(out, "  {} {verb}", manifest.label());
    }
    Ok(ToolOutput::confirmed(out))
}

fn apply_error(error: core_apply::ApplyError) -> ToolError {
    use core_apply::ApplyError;
    let message = mask_text(&error.to_string()).into_owned();
    match error {
        ApplyError::Get { source, .. } | ApplyError::Api { source, .. } => match *source {
            kube::Error::Api(status) => ToolError::Api {
                code: Some(status.code),
                message,
            },
            _ => ToolError::Api {
                code: None,
                message,
            },
        },
        ApplyError::Conflicts { .. } | ApplyError::Stale { .. } => ToolError::Api {
            code: Some(409),
            message,
        },
        ApplyError::Invalid { .. } => ToolError::Api {
            code: Some(422),
            message,
        },
    }
}

async fn run_action(
    ctx: &Ctx<'_>,
    allowed: &Allowed,
    tool: &str,
    action_id: ActionId,
    params: &ActionParams,
    detail: &str,
) -> Result<ToolOutput, ToolError> {
    let client = ctx.session.client_in(&allowed.namespace)?;
    let obj = fetch(&client, allowed).await?;
    let kind = kind_of(allowed);
    let plugin = ctx.registry.plugin_for_resource(&kind.api);
    // Kinds without a specialized plugin (CRDs, …) are deleted by the
    // generic one. Streams (logs, exec, …) have tools of their own.
    let Some(title) = plugin
        .actions_for(&obj)
        .into_iter()
        .find(|a| a.id == action_id && a.kind == ActionKind::Api)
        .map(|a| a.title)
    else {
        return Err(ToolError::Failed(format!(
            "{} does not support `{action_id}`",
            kind.api.kind
        )));
    };
    let target = format!("{} {}/{}", kind.api.kind, allowed.namespace, obj.name_any());
    let summary = format!("{title} {target} {detail}").trim_end().to_owned();
    // Scaling and restarts within a granted `modify(ns)` are not asked
    // again (#227); everything else is.
    if allowed.confirmation == Confirmation::Required {
        confirm(
            ctx,
            allowed,
            tool,
            summary.clone(),
            String::new(),
            Vec::new(),
        )
        .await?;
    }
    plugin
        .run_action(&client, &obj, &action_id, params)
        .await
        .map_err(|error| match error {
            kubach_core::plugin::PluginError::Kube(error) => ToolError::from(error),
            other => ToolError::Failed(other.to_string()),
        })?;
    Ok(ToolOutput::confirmed(format!("Done: {summary}")))
}

/// `sh -c 'echo hi'` style rendering for the confirmation prompt.
fn shell_words(command: &[String]) -> String {
    command
        .iter()
        .map(|arg| {
            if !arg.is_empty()
                && arg
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_./=:,@".contains(&b))
            {
                arg.clone()
            } else {
                format!("'{}'", arg.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

async fn exec(
    ctx: &Ctx<'_>,
    allowed: &Allowed,
    container: Option<&str>,
    command: &[String],
    timeout: u64,
) -> Result<ToolOutput, ToolError> {
    let pod = name_of(allowed);
    let target = match container {
        Some(container) => format!("pod {}/{pod}, container {container}", allowed.namespace),
        None => format!("pod {}/{pod}", allowed.namespace),
    };
    let rendered = shell_words(command);
    confirm_as(
        ctx,
        RequestKind::Exec {
            command: command.to_vec(),
        },
        allowed,
        "exec",
        format!("Run a command in {target}"),
        rendered,
        Vec::new(),
    )
    .await?;
    let client = ctx.session.client_in(&allowed.namespace)?;
    let run = exec_capture(
        &client,
        &allowed.namespace,
        pod,
        container,
        command.to_vec(),
        MAX_EXEC_OUTPUT,
    );
    let capture = tokio::time::timeout(Duration::from_secs(timeout), run)
        .await
        .map_err(|_| ToolError::Failed(format!("command timed out after {timeout}s")))?
        .map_err(|error| match error {
            kubach_core::streams::StreamError::Api(error) => ToolError::from(*error),
            other => ToolError::Failed(other.to_string()),
        })?;
    let stdout = String::from_utf8_lossy(&capture.stdout);
    let stderr = String::from_utf8_lossy(&capture.stderr);
    let mut out = format!("exit: {:?}\n", capture.status);
    if !stdout.is_empty() {
        let _ = write!(
            out,
            "--- stdout\n{}\n",
            truncate(&mask_text(&stdout), MAX_EXEC_OUTPUT)
        );
    }
    if !stderr.is_empty() {
        let _ = write!(
            out,
            "--- stderr\n{}\n",
            truncate(&mask_text(&stderr), MAX_EXEC_OUTPUT)
        );
    }
    if capture.truncated {
        let _ = writeln!(
            out,
            "--- output truncated: more than {} KiB, the connection was closed",
            MAX_EXEC_OUTPUT / 1024
        );
    }
    Ok(ToolOutput::confirmed(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cut_lists_are_noticed() {
        assert!(!has_more(&ListMeta::default()));
        let empty = ListMeta {
            continue_: Some(String::new()),
            ..Default::default()
        };
        assert!(!has_more(&empty));
        let cut = ListMeta {
            continue_: Some("token".into()),
            ..Default::default()
        };
        assert!(has_more(&cut));
    }

    #[test]
    fn shell_words_quote_when_needed() {
        let command = vec!["sh".to_string(), "-c".into(), "echo 'hi' > /tmp/x".into()];
        assert_eq!(shell_words(&command), r#"sh -c 'echo '\''hi'\'' > /tmp/x'"#);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let mut args = Map::new();
        args.insert("kind".into(), json!("Pod"));
        args.insert("allNamespace".into(), json!(true));
        assert!(matches!(
            parse::<ListArgs>(&args),
            Err(ToolError::Denied(_))
        ));
        args.remove("allNamespace");
        args.insert(
            "fieldSelector".into(),
            json!("metadata.namespace=kube-system"),
        );
        assert!(matches!(
            parse::<ListArgs>(&args),
            Err(ToolError::Denied(_))
        ));
    }
}
