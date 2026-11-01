//! Streaming pod I/O: logs, exec/attach sessions, port-forwarding and
//! ephemeral debug containers.
//!
//! Everything that talks to the API must run on the [`runtime`](crate::runtime);
//! the handles it returns (`futures` channels and streams) can be consumed on
//! any executor, including GPUI's.

pub mod debug;
pub mod exec;
pub mod forward_target;
pub mod log_history;
pub mod logs;
pub mod node_shell;
pub mod port_forward;

pub use debug::{
    DebugContainer, DebugOptions, add_debug_container, create_debug_container,
    wait_for_debug_container,
};
pub use exec::{
    ExecCapture, ExecEvent, ExecHandle, ExecSession, ExitStatus, SHELL_CANDIDATES, SessionCommand,
    SessionOptions, ShellAttempt, StartedShell, check_exec_target, exec_blocker, exec_capture,
    is_missing_executable, is_missing_shell, pick_shell, start_session, start_shell,
};
pub use forward_target::{
    ForwardPort, ForwardSource, ResolvedTarget, TargetPod, TargetPort, fetch_source, is_pod_ready,
    pick_ready_pod, short_kind, target_pod,
};
pub use log_history::{
    HistoryRequest, HistoryState, INITIAL_TAIL_LINES, LogHistory, MAX_HISTORY_BYTES,
    MAX_HISTORY_LINES, fetch_history,
};
pub use logs::{
    CUT_LINE_MARK, FetchedLines, LineSplitter, Liveness, LogEvent, LogLine, LogOptions, LogSince,
    LogSource, LogStream, container_liveness, fetch_log_lines, sources_for_pod,
    sources_for_selector, stream_logs,
};
pub use port_forward::{
    ForwardAgent, ForwardId, ForwardInfo, ForwardState, LocalPort, PortForwardManager,
    local_port_candidates,
};

/// Errors from streaming operations.
#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    #[error("API request failed: {0}")]
    Api(#[source] Box<kube::Error>),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("pod {0} is not running")]
    PodNotRunning(String),
    #[error("container {container} in pod {pod}: {reason}")]
    Container {
        pod: String,
        container: String,
        reason: String,
    },
    /// Exec needs a running container.
    #[error("container {container} is not running ({state})")]
    ContainerNotRunning { container: String, state: String },
    /// The image has none of the [`SHELL_CANDIDATES`] (e.g. distroless).
    #[error(
        "container {container} has no shell: none of {shells} found (distroless or scratch image?). Use Debug to attach a busybox container instead",
        shells = SHELL_CANDIDATES.join(", ")
    )]
    NoShell { container: String },
    #[error("timed out: {0}")]
    Timeout(String),
    #[error("{0}")]
    Remote(String),
    /// The exact local port asked for a forward is taken.
    #[error("local port {0} is already in use")]
    PortInUse(u16),
    /// The exact local port asked for a forward needs rights the app has
    /// not (a privileged or reserved port).
    #[error("local port {0} cannot be used: permission denied")]
    PortDenied(u16),
    /// A forward target that cannot be resolved (no ready pod, unknown port).
    #[error("{0}")]
    Target(String),
    /// The pod, Service or workload of a forward no longer exists
    /// (`namespace/svc/web`).
    #[error("{0} no longer exists")]
    SourceGone(String),
}

impl From<kube::Error> for StreamError {
    fn from(error: kube::Error) -> Self {
        Self::Api(Box::new(error))
    }
}
