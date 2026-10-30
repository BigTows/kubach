//! Kubach as an MCP server (#12): an AI agent works with the cluster through
//! tools confined to a session's namespace and profile (ADR 0005).
//!
//! - [`settings`]: per-context security settings, filled by the app.
//! - [`scope`] and [`session`]: what a session may touch and under which
//!   identity ([`identity`]) it runs; [`grants`]: the rights the user grants
//!   on request within it (#227).
//! - [`policy`]: the client-side check every call passes first.
//! - [`server`] and [`transport`]: the MCP handler over stdio or HTTP;
//!   [`shared`]: the app's HTTP handler whose session can be swapped.
//! - [`bridge`]: how `kubach mcp` (stdio) asks the running app to confirm;
//!   [`hosted`] and [`proxy`]: sessions on vault-imported contexts that the
//!   app runs for `kubach mcp`.
//! - [`confirm`], [`audit`], [`masking`]: human-in-the-loop, local audit
//!   log, credential masking; [`secret_write`]: Secrets written through
//!   the user, whose values the agent never sees (#230); [`remember`]:
//!   "don't ask again this session" for one kind of change (#30).
//!
//! UI-free: depends on `kubach-core` only.

pub mod access;
pub mod audit;
pub mod bridge;
pub mod confirm;
pub mod forwards;
pub mod grants;
pub mod helm_tools;
pub mod hosted;
pub mod identity;
pub mod masking;
pub mod names;
pub mod policy;
pub mod presence;
pub mod proxy;
pub mod remember;
pub mod rules;
pub mod scope;
pub mod server;
pub mod session;
pub mod settings;
pub mod shared;
pub mod transport;

mod debug_tools;
mod node_tools;
mod rollout_tools;
mod secret_tools;
pub mod secret_write;
mod tools;
mod topology_tools;

#[cfg(test)]
mod testutil;

pub use access::{Access, Raises};
pub use audit::{AuditEntry, AuditLog, DecidedBy};
pub use confirm::{
    ConfirmationDecision, ConfirmationRequest, Confirmer, DenyAll, RequestKind, ScriptedConfirmer,
};
pub use grants::{Grant, GrantEntry, GrantState, Grants};
pub use helm_tools::{HelmAction, HelmChange, HelmCredentials, HelmEnv, USER_IDENTITY};
pub use identity::{Identity, sweep_expired};
pub use policy::{Denial, DenialCode, NamespaceModes, Policy, Request};
pub use remember::{Remembered, RememberedAction, rememberable};
pub use scope::{NamespaceHint, NamespaceSource, SessionMode, SessionScope, SessionSpec};
pub use secret_write::{
    KeySize, SecretInput, SecretOp, SecretRequest, SecretSource, SecretUser, check_input,
};
pub use server::{CallOutcome, McpServer};
pub use session::{Activity, BoundaryInfo, Session, SessionError};
pub use settings::{
    DEFAULT_SESSION_TTL, ImpersonationTarget, MAX_SESSION_TTL, Profile, SecurityPolicy,
    SecurityPolicySource,
};
pub use shared::{ContextChooser, ContextEntry, SharedServer};
pub use transport::{
    BridgeOptions, HttpEndpoint, HttpOptions, serve_http, serve_shared_http, serve_stdio,
};
