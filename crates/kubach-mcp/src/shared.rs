//! An MCP handler whose session the app can swap: agents keep one config
//! (URL and token, or `kubach mcp`) while sessions start and end.
//!
//! Since #421 the agent picks the context itself: without a session the
//! handler offers `session_info`, `list_contexts` and `use_context`; the
//! first `use_context` on a context asks the user in Kubach
//! ([`ContextChooser`]), and only a yes starts a session — without
//! namespaces and rights, as any session (#227, #401). With a session
//! running both tools stay, so the agent can see or switch the context
//! (switching asks again and ends the current session).

use std::sync::{Arc, RwLock};

use kubach_core::plugin::BoxFuture;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::server::{McpServer, client_name};
use kubach_core::sync;

/// Told to the agent while no session is active and nobody can choose one.
pub const NO_SESSION: &str = "No active Kubach session. Call list_contexts, then use_context: \
     the user approves the context in the Kubach window.";

/// A context the agent may ask for, as `list_contexts` shows it. Names and
/// where Kubach knows them from; never credentials.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEntry {
    pub name: String,
    /// API server URL.
    pub server: String,
    /// `kubeconfig` or `vault`.
    pub source: String,
    /// "AI agent access" is on for it.
    pub agent_access: bool,
    /// Why it cannot be used now (vault locked…), if so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
    /// The context of the running session.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub current: bool,
}

/// Lists contexts and starts a session on the one the agent names, after
/// the user agreed in Kubach. Implemented by the app and by `kubach mcp`.
pub trait ContextChooser: Send + Sync {
    fn list(&self) -> BoxFuture<'_, Vec<ContextEntry>>;

    /// Asks the user and starts a session on `context`; the error is told
    /// to the agent as is.
    fn choose<'a>(
        &'a self,
        context: &'a str,
        client: &'a str,
    ) -> BoxFuture<'a, Result<McpServer, String>>;
}

/// The current session's server, or none, and who picks the context.
/// Cheap to clone; clones share it.
#[derive(Clone, Default)]
pub struct SharedServer {
    current: Arc<RwLock<Option<McpServer>>>,
    chooser: Arc<RwLock<Option<Arc<dyn ContextChooser>>>>,
    /// Why the last session ended, told to the agent until a new one
    /// starts: its tools vanish with it (#56).
    ended: Arc<RwLock<Option<String>>>,
}

impl SharedServer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Serves `server` from the next request on; `None` serves nothing.
    pub fn set(&self, server: Option<McpServer>) {
        if server.is_some() {
            *sync::write(&self.ended) = None;
        }
        *sync::write(&self.current) = server;
    }

    /// Stops serving the session, and tells the agent on its next call
    /// that it ended and `why` (the user revoked it, the vault locked…).
    pub fn end(&self, why: &str) {
        let previous = sync::write(&self.current).take();
        if let Some(server) = previous {
            self.record_end(&server, why);
        }
    }

    fn record_end(&self, server: &McpServer, why: &str) {
        let context = &server.session().scope().context;
        *sync::write(&self.ended) = Some(format!(
            "The Kubach session on context {context} ended: {why}. Its tools are gone; call \
             use_context to start a new session (the user approves it in Kubach again)."
        ));
    }

    /// The active session's server, if its session is still active.
    pub fn get(&self) -> Option<McpServer> {
        let server = sync::read(&self.current).clone()?;
        if server.session().is_active() {
            return Some(server);
        }
        // It closed itself (TTL) or was closed under us: say why, once.
        let why = if server.session().scope().expired() {
            "its time limit ran out"
        } else {
            "it was closed"
        };
        let mut current = sync::write(&self.current);
        if current
            .as_ref()
            .is_some_and(|c| c.session().id() == server.session().id())
        {
            *current = None;
            drop(current);
            self.record_end(&server, why);
        }
        None
    }

    /// Why the last session ended, while no new one runs.
    pub fn ended(&self) -> Option<String> {
        sync::read(&self.ended).clone()
    }

    /// Lets the agent pick the context with `list_contexts` and
    /// `use_context` (#421).
    pub fn set_chooser(&self, chooser: Arc<dyn ContextChooser>) {
        *sync::write(&self.chooser) = Some(chooser);
    }

    fn chooser(&self) -> Option<Arc<dyn ContextChooser>> {
        sync::read(&self.chooser).clone()
    }

    /// Tools offered now: the session's, plus choosing a context.
    pub fn tools(&self) -> Vec<Tool> {
        let mut tools = Vec::new();
        let session = self.get();
        if self.chooser().is_some() {
            tools.extend(context_tools());
            if session.is_none() {
                tools.push(info_tool());
            }
        }
        if let Some(server) = session {
            tools.extend(server.tools());
        }
        tools
    }

    /// Runs `list_contexts` or `use_context`, or `None` for any other tool.
    pub async fn call_context_tool(
        &self,
        client: &str,
        tool: &str,
        args: &Map<String, Value>,
    ) -> Option<(String, bool, bool)> {
        let chooser = self.chooser()?;
        match tool {
            "list_contexts" => {
                let current = self
                    .get()
                    .map(|server| server.session().scope().context.clone());
                let mut contexts = chooser.list().await;
                for entry in &mut contexts {
                    entry.current = current.as_deref() == Some(entry.name.as_str());
                }
                let out = json!({
                    "contexts": contexts,
                    "current": current,
                    "howToUse": "Call use_context with a name: the user approves it in the Kubach \
                        window. A context with agentAccess false is refused; only the user \
                        changes that, in Kubach: Settings → Security.",
                });
                Some((
                    serde_json::to_string_pretty(&out).unwrap_or_default(),
                    false,
                    false,
                ))
            }
            "use_context" => {
                let Some(name) = args.get("context").and_then(Value::as_str) else {
                    return Some((
                        "pass `context`: a name from list_contexts".into(),
                        true,
                        false,
                    ));
                };
                if let Some(server) = self.get()
                    && server.session().scope().context == name
                {
                    return Some((
                        format!(
                            "Already working in context {name}. {}",
                            server.instructions()
                        ),
                        false,
                        false,
                    ));
                }
                match chooser.choose(name, client).await {
                    Ok(server) => {
                        let text =
                            format!("The user allowed context {name}. {}", server.instructions());
                        *sync::write(&self.ended) = None;
                        let previous = sync::write(&self.current).replace(server);
                        if let Some(previous) = previous {
                            let session = previous.session().clone();
                            tokio::spawn(async move {
                                let _ = session.close().await;
                            });
                        }
                        Some((text, false, true))
                    }
                    Err(error) => Some((error, true, false)),
                }
            }
            "session_info" if self.get().is_none() => Some((
                match self.ended() {
                    Some(ended) => ended,
                    None => "No context chosen yet: call list_contexts, then use_context. The \
                         user approves the context in the Kubach window; anything beyond the \
                         context's minimal access is asked for later, when a call needs it."
                        .into(),
                },
                false,
                false,
            )),
            _ => None,
        }
    }
}

fn info_tool() -> Tool {
    Tool::new(
        "session_info",
        "Whether a context is chosen yet, and how to choose one.",
        Arc::new(object(json!({}), &[])),
    )
    .annotate(ToolAnnotations::new().read_only(true).destructive(false))
}

fn context_tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "list_contexts",
            "Kubernetes contexts Kubach knows (kubeconfig and its vault): name, API server, \
             source and whether AI agent access is on. Names only, never credentials.",
            Arc::new(object(json!({}), &[])),
        )
        .annotate(ToolAnnotations::new().read_only(true).destructive(false)),
        Tool::new(
            "use_context",
            "Work in a context from list_contexts. The user approves it in the Kubach window the \
             first time; then the session starts with the context's minimal access (by default \
             reading built-in resources in any namespace), and more is asked for when a call \
             needs it. Choosing another context ends the current session.",
            Arc::new(object(
                json!({"context": {"type": "string", "description": "A name from list_contexts"}}),
                &["context"],
            )),
        )
        .annotate(ToolAnnotations::new().read_only(true).destructive(false)),
    ]
}

fn object(properties: Value, required: &[&str]) -> Map<String, Value> {
    match json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    }) {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

impl ServerHandler for SharedServer {
    fn get_info(&self) -> ServerConfig {
        let instructions = match (self.get(), self.chooser().is_some()) {
            (Some(server), _) => server.instructions(),
            (None, true) => "Kubach gives access to Kubernetes clusters the user allows. No \
                 context is chosen yet: call list_contexts, then use_context; the user approves \
                 it in the Kubach window. Anything beyond the context's minimal access is asked \
                 for later, when a call needs it."
                .to_owned(),
            (None, false) => format!("Kubach gives access to Kubernetes clusters. {NO_SESSION}"),
        };
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .build(),
        )
        .with_server_info(Implementation::new("kubach", kubach_core::VERSION))
        .with_instructions(instructions)
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
        let args = request.arguments.clone().unwrap_or_default();
        if let Some((text, is_error, changed)) =
            self.call_context_tool(&client, &request.name, &args).await
        {
            if changed {
                let _ = context.peer.notify_tool_list_changed().await;
            }
            let content = vec![ContentBlock::text(text)];
            return Ok(if is_error {
                CallToolResult::error(content)
            } else {
                CallToolResult::success(content)
            }
            .into());
        }
        let Some(server) = self.get() else {
            let text = match self.ended() {
                // The client still lists the session's tools: refresh it.
                Some(ended) => {
                    let _ = context.peer.notify_tool_list_changed().await;
                    ended
                }
                None if self.chooser().is_some() => {
                    "No context chosen yet: call list_contexts, then use_context.".into()
                }
                None => NO_SESSION.into(),
            };
            return Ok(CallToolResult::error(vec![ContentBlock::text(text)]).into());
        };
        // The session's own handler: progress of long calls included.
        server.call_tool(request, context).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Mutex;

    use serde_json::{Map, Value};

    use super::{ContextChooser, ContextEntry, SharedServer};
    use crate::confirm::DenyAll;
    use crate::identity::Identity;
    use crate::server::McpServer;
    use crate::session::Session;
    use crate::settings::Profile;
    use crate::testutil::{catalog, permissive, scope_with};
    use kubach_core::plugin::BoxFuture;
    use kubach_core::sync;

    /// Allows `dev`, refuses everything else; records what it was asked.
    #[derive(Default)]
    struct Chooser {
        asked: Mutex<Vec<String>>,
    }

    impl ContextChooser for Chooser {
        fn list(&self) -> BoxFuture<'_, Vec<ContextEntry>> {
            Box::pin(async {
                vec![ContextEntry {
                    name: "dev".into(),
                    server: "https://127.0.0.1:6443".into(),
                    source: "kubeconfig".into(),
                    agent_access: true,

                    unavailable: None,
                    current: false,
                }]
            })
        }

        fn choose<'a>(
            &'a self,
            context: &'a str,
            _client: &'a str,
        ) -> BoxFuture<'a, Result<McpServer, String>> {
            sync::lock(&self.asked).push(context.to_owned());
            Box::pin(async move {
                if context != "dev" {
                    return Err(format!("the user did not allow working in {context}"));
                }
                let mut scope = scope_with(Profile::ReadOnly, &permissive(), &[]);
                scope.context = "dev".into();
                let client = kube::Client::try_from(kube::Config::new(
                    "http://127.0.0.1:9".parse().unwrap(),
                ))
                .unwrap();
                let session = Session::from_parts(
                    scope,
                    Identity::ClientOnly { client },
                    catalog(),
                    HashSet::new(),
                );
                Ok(McpServer::new(session, std::sync::Arc::new(DenyAll), None))
            })
        }
    }

    fn names(shared: &SharedServer) -> Vec<String> {
        shared.tools().iter().map(|t| t.name.to_string()).collect()
    }

    fn context(name: &str) -> Map<String, Value> {
        let mut args = Map::new();
        args.insert("context".into(), Value::String(name.into()));
        args
    }

    #[tokio::test]
    async fn the_agent_picks_the_context_and_the_user_decides() {
        let shared = SharedServer::new();
        // Without a chooser nothing is offered, as before.
        assert!(names(&shared).is_empty());
        let chooser = std::sync::Arc::new(Chooser::default());
        shared.set_chooser(chooser.clone());
        assert_eq!(
            names(&shared),
            ["list_contexts", "use_context", "session_info"]
        );
        let (text, error, _) = shared
            .call_context_tool("agent", "list_contexts", &Map::new())
            .await
            .unwrap();
        assert!(!error);
        let listed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(listed["contexts"][0]["name"], "dev");
        // A refusal starts nothing.
        let (text, error, changed) = shared
            .call_context_tool("agent", "use_context", &context("prod"))
            .await
            .unwrap();
        assert!(error && !changed, "{text}");
        assert!(shared.get().is_none());
        // A yes starts the session; its tools join the context tools.
        let (text, error, changed) = shared
            .call_context_tool("agent", "use_context", &context("dev"))
            .await
            .unwrap();
        assert!(!error && changed, "{text}");
        assert!(text.contains("The user allowed context dev"));
        assert_eq!(shared.get().unwrap().session().scope().context, "dev");
        let tools = names(&shared);
        assert!(tools.contains(&"list_resources".to_owned()));
        assert!(tools.contains(&"use_context".to_owned()));
        // The same context again is not asked again.
        shared
            .call_context_tool("agent", "use_context", &context("dev"))
            .await
            .unwrap();
        assert_eq!(*sync::lock(&chooser.asked), ["prod", "dev"]);
        // Other tools go to the session.
        assert!(
            shared
                .call_context_tool("agent", "list_resources", &Map::new())
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn an_ended_session_tells_the_agent_why() {
        let shared = SharedServer::new();
        shared.set_chooser(std::sync::Arc::new(Chooser::default()));
        shared
            .call_context_tool("agent", "use_context", &context("dev"))
            .await
            .unwrap();
        assert!(shared.ended().is_none());
        // The app ends it (revoked, vault locked, TTL): the reason stays.
        shared.end("the user ended it in Kubach");
        assert!(shared.get().is_none());
        let ended = shared.ended().unwrap();
        assert!(ended.contains("context dev ended"), "{ended}");
        assert!(ended.contains("the user ended it in Kubach"), "{ended}");
        assert!(ended.contains("use_context"), "{ended}");
        let (text, error, _) = shared
            .call_context_tool("agent", "session_info", &Map::new())
            .await
            .unwrap();
        assert!(!error);
        assert_eq!(text, ended);
        // A new session forgets it.
        shared
            .call_context_tool("agent", "use_context", &context("dev"))
            .await
            .unwrap();
        assert!(shared.ended().is_none());
        // A session closed under the shared server is noticed on the way.
        let session = shared.get().unwrap().session().clone();
        session.close().await.unwrap();
        assert!(shared.get().is_none());
        assert!(shared.ended().unwrap().contains("it was closed"));
    }
}
