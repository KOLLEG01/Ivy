//! Accepted AgentManager work survives its caller and drains at owner shutdown.
use ivy_contracts::{IvyError, IvyJson, IvyResult};
use ivy_sdk::service::{ServiceFuture, ToolHandler};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

#[derive(Clone)]
pub struct AgentRun(watch::Receiver<Option<IvyResult<IvyJson>>>);
impl AgentRun {
    pub async fn wait(mut self) -> IvyResult<IvyJson> {
        loop {
            if let Some(result) = self.0.borrow_and_update().clone() {
                return result;
            }
            self.0.changed().await.map_err(|_| IvyError::internal())?;
        }
    }
}
#[derive(Default)]
struct State {
    next: u64,
    pending: BTreeMap<u64, watch::Receiver<bool>>,
}
#[derive(Clone, Default)]
pub struct AgentWork(Arc<Mutex<State>>);
struct Completion {
    owner: AgentWork,
    index: u64,
    released: bool,
    done: Option<watch::Sender<bool>>,
}
impl Completion {
    fn release(&mut self) {
        if !self.released {
            self.owner
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .pending
                .remove(&self.index);
            self.released = true;
        }
    }
    fn finish(&mut self) {
        if let Some(sender) = self.done.take() {
            sender.send_replace(true);
        }
    }
}
impl Drop for Completion {
    fn drop(&mut self) {
        self.release();
        self.finish();
    }
}
impl AgentWork {
    pub fn pending(&self) -> IvyResult<usize> {
        Ok(self
            .0
            .lock()
            .map_err(|_| IvyError::internal())?
            .pending
            .len())
    }
    /// Mirrors track(promise): admitting the future owns its execution. Dropping
    /// an invocation or joining waiter cannot cancel the underlying operation.
    pub fn track(&self, future: ServiceFuture<IvyJson>) -> IvyResult<AgentRun> {
        let (result, receiver) = watch::channel(None);
        let (done, complete) = watch::channel(false);
        let index = {
            let mut state = self.0.lock().map_err(|_| IvyError::internal())?;
            while state.pending.contains_key(&state.next) {
                state.next = state.next.wrapping_add(1);
            }
            let index = state.next;
            state.next = state.next.wrapping_add(1);
            state.pending.insert(index, complete);
            index
        };
        let completion = Completion {
            owner: self.clone(),
            index,
            released: false,
            done: Some(done),
        };
        tokio::spawn(async move {
            let mut completion = completion;
            let value = future.await;
            // The original registers its Set deletion reaction before a caller
            // can await the returned promise. Drain completion follows the reply.
            completion.release();
            result.send_replace(Some(value));
            completion.finish();
        });
        Ok(AgentRun(receiver))
    }
    /// Capture the current work set immediately. Later requests are not silently
    /// added to this snapshot; the enclosing service owns invocation retirement.
    pub fn drain_snapshot(&self) -> ServiceFuture<()> {
        let snapshot = self
            .0
            .lock()
            .map(|state| state.pending.values().cloned().collect::<Vec<_>>())
            .map_err(|_| IvyError::internal());
        Box::pin(async move {
            for mut complete in snapshot? {
                loop {
                    if *complete.borrow_and_update() {
                        break;
                    }
                    if complete.changed().await.is_err() {
                        break;
                    }
                }
            }
            Ok(())
        })
    }
    pub fn handler(&self, handler: ToolHandler) -> ToolHandler {
        let owner = self.clone();
        Arc::new(
            move |args, context| match owner.track(handler(args, context)) {
                Ok(run) => Box::pin(run.wait()),
                Err(error) => Box::pin(async move { Err(error) }),
            },
        )
    }
}
