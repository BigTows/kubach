//! Kubernetes domain logic for Kubach. Kept free of UI dependencies.

pub mod ansi;
pub mod apply;
pub mod cache;
pub mod cluster;
pub mod data_keys;
#[cfg(debug_assertions)]
pub mod debug_stats;
pub mod deletion;
pub mod diagnostics;
pub mod discovery;
pub mod disk_cache;
pub mod drain;
pub mod env;
pub mod events;
pub mod eviction;
pub mod gitops;
pub mod helm;
pub mod images;
pub mod kubeconfig;
pub mod kustomize;
pub mod links;
pub mod list_cache;
pub mod metrics;
pub mod names;
pub mod namespace;
pub mod plugin;
pub mod probe;
pub mod process;
pub mod pty;
pub mod rbac;
pub mod redact;
pub mod runtime;
pub mod scale;
pub mod secrets;
pub mod security;
pub mod settings;
#[cfg(unix)]
pub mod shell_env;
#[cfg(unix)]
mod spawn;
pub mod state;
pub mod status_bar;
pub mod streams;
pub mod sync;
pub mod templates;
pub mod tools;
pub mod topology;
pub mod transport;
pub mod validation;
pub mod vault;
pub mod which;
#[cfg(windows)]
mod windows_acl;
pub mod workload_pods;

pub use cluster::{ClusterError, Connection, ConnectionState, ContextInfo, Contexts};
pub use discovery::{ApiCatalog, DiscoveryError, ResourceKind};

/// Application version in `YEAR.MONTH.INC` format.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
