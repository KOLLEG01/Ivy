//! The existing local owner and its non-Send graph live on one SQLite thread.
//! Public calls and lifecycle commands retain their existing typed mailboxes.
use ivy_contracts::{IvyError, IvyResult, Outcome};
use std::{future::Future, panic::AssertUnwindSafe};
use tokio::sync::oneshot;

fn interrupted() -> IvyError {
    IvyError::new(
        "service_unavailable",
        "TaskBoard journal owner stopped before its completion was observed.",
    )
    .with_outcome(Outcome::Unknown)
}

/// The future is constructed on its owner thread: Rc/RefCell state and SQLite
/// never cross threads. Completion follows graph, connection and runtime drop;
/// the host then joins the thread without blocking its I/O executor.
pub(crate) async fn run<F: Future<Output = IvyResult<()>> + 'static>(
    create: impl FnOnce() -> F + Send + 'static,
) -> IvyResult<()> {
    let (completed, received) = oneshot::channel();
    let thread = std::thread::Builder::new()
        .name("task-board-journal".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| interrupted())?;
                tokio::task::LocalSet::new().block_on(&runtime, create())
            }))
            .unwrap_or_else(|_| Err(interrupted()));
            let _ = completed.send(result);
        })
        .map_err(|_| interrupted())?;
    // The coordinator owns completion and join even if its caller stops
    // observing. The host's drop guard closes admission and requests drain.
    tokio::spawn(async move {
        let result = received.await.map_err(|_| interrupted());
        tokio::task::spawn_blocking(move || thread.join())
            .await
            .map_err(|_| interrupted())?
            .map_err(|_| interrupted())?;
        result?
    })
    .await
    .map_err(|_| interrupted())?
}

#[cfg(test)]
#[path = "../../../../tests/fixtures/task-board-journal-thread.rs"]
mod tests;
