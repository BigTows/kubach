//! Shared tokio runtime for Kubernetes I/O (ADR 0011).
//!
//! kube-rs needs tokio, while the UI runs on GPUI's own executor. Work is
//! spawned here and the returned [`BackgroundTask`] is awaited from the UI
//! side: it is a plain future and works on any executor.
//!
//! A task belongs to whoever holds its [`BackgroundTask`]: dropping the
//! handle aborts the task, so a closed screen, a replaced request or a
//! cancelled dialog stops its work (and kills a process started through
//! [`crate::process`], which is `kill_on_drop`). Work that must outlive its
//! owner — a change already sent to the cluster, cleanup on shutdown — is
//! started with [`spawn_detached`] instead.

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::task::{Context, Poll};

use tokio::runtime::{Builder, Runtime};
use tokio::task::{AbortHandle, JoinError, JoinHandle};

fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("kubach-io")
            .enable_all()
            .build()
            .expect("failed to start tokio runtime")
    })
}

/// A task on the I/O runtime that is aborted when dropped, so its owner going
/// away also stops the work (and closes the connection or kills the process
/// behind it). Awaiting it yields the task's output, or the [`JoinError`] of
/// a task that panicked or was aborted.
#[must_use = "the task is aborted when dropped; use `spawn_detached` for work that must outlive its owner"]
pub struct BackgroundTask<T = ()>(JoinHandle<T>);

impl<T> BackgroundTask<T> {
    /// A handle that aborts the task without owning it (a "Stop" button,
    /// an entry in a shared table).
    pub fn abort_handle(&self) -> AbortHandle {
        self.0.abort_handle()
    }

    /// Whether the task has finished (completed, panicked or aborted).
    pub fn is_finished(&self) -> bool {
        self.0.is_finished()
    }
}

impl<T> Drop for BackgroundTask<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl<T> Future for BackgroundTask<T> {
    type Output = Result<T, JoinError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx)
    }
}

/// Runs `future` on the I/O runtime until it completes or the returned
/// handle is dropped.
pub fn spawn<F>(future: F) -> BackgroundTask<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    BackgroundTask(runtime().spawn(future))
}

/// Runs `future` on the I/O runtime to completion even if nobody waits for
/// it: dropping the returned [`JoinHandle`] only stops waiting. Only for work
/// that must outlive its owner (say why at the call site): a change that is
/// already being applied, closing sessions on shutdown.
pub fn spawn_detached<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    runtime().spawn(future)
}

/// Tasks alive on the I/O runtime, for the debug overlay (#291).
#[cfg(debug_assertions)]
pub fn alive_tasks() -> usize {
    runtime().metrics().num_alive_tasks()
}

/// Runs CPU-heavy or blocking `work` (decoding, compression, file I/O) on
/// tokio's blocking pool, off the two I/O threads that drive watches and
/// requests. Must be awaited inside a tokio runtime. A panic in `work` is
/// resumed in the caller; blocking work can't be aborted, so dropping the
/// future lets it finish unobserved.
pub async fn blocking<T, F>(work: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(value) => value,
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        // Only when the runtime shuts down, which drops the caller as well.
        Err(error) => panic!("blocking work was cancelled: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{blocking, spawn, spawn_detached};
    use futures::channel::oneshot;
    use futures::executor::block_on;

    #[test]
    fn awaiting_returns_the_output() {
        assert_eq!(block_on(spawn(async { 40 + 2 })).unwrap(), 42);
    }

    #[test]
    fn dropping_the_task_aborts_it() {
        let (tx, rx) = oneshot::channel::<()>();
        let (started_tx, started_rx) = oneshot::channel::<()>();
        let task = spawn(async move {
            // Held until the task is dropped by the abort.
            let _tx = tx;
            started_tx.send(()).ok();
            std::future::pending::<()>().await;
        });
        block_on(started_rx).unwrap();
        drop(task);
        // The sender went away without sending: the future was dropped.
        assert!(block_on(rx).is_err());
    }

    #[test]
    fn aborted_task_reports_cancellation() {
        let task = spawn(std::future::pending::<()>());
        task.abort_handle().abort();
        assert!(block_on(task).unwrap_err().is_cancelled());
    }

    #[test]
    fn detached_task_outlives_its_handle() {
        let (go_tx, go_rx) = oneshot::channel::<()>();
        let (tx, rx) = oneshot::channel();
        drop(spawn_detached(async move {
            go_rx.await.ok();
            tx.send(7).ok();
        }));
        go_tx.send(()).unwrap();
        assert_eq!(block_on(rx).unwrap(), 7);
    }

    #[test]
    fn blocking_work_returns_its_value() {
        let task = spawn(async { blocking(|| 6 * 7).await });
        assert_eq!(block_on(task).unwrap(), 42);
    }
}
