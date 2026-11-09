//! Send service handlers enter the existing local journal owner through one
//! mailbox. Count and retained-byte admission cover queued/running calls and
//! unobserved replies; retirement has one separate FIFO control slot.
use crate::work_budget::{Reservation as WorkReservation, WorkBudget, limit};
use futures_util::{FutureExt, StreamExt, stream::FuturesUnordered};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, Outcome};
use ivy_sdk::service::{InvocationContext, ServiceFuture, ToolHandler};
use std::{
    future::Future,
    panic::AssertUnwindSafe,
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, oneshot, watch};

pub const MAX_PENDING_CALLS: usize = 64;
pub use crate::work_budget::MAX_RETAINED_BYTES;

#[allow(async_fn_in_trait)]
pub trait LocalHandler {
    async fn handle(&self, tool: &str, args: Json, context: InvocationContext) -> IvyResult<Json>;
}
struct Call {
    tool: String,
    args: Json,
    context: InvocationContext,
    reply: oneshot::Sender<IvyResult<Json>>,
    reservation: Arc<Reservation>,
}
enum Command {
    Call(Call),
    Retire,
}
#[derive(Default)]
struct State {
    stopped: bool,
    calls: usize,
}
struct Reservation {
    state: Arc<Mutex<State>>,
    work: Arc<WorkReservation>,
    overhead: usize,
}
impl Reservation {
    fn reply(&self, result: IvyResult<Json>) -> IvyResult<Json> {
        self.work.reply(result, self.overhead)
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap();
        state.calls -= 1;
    }
}
#[derive(Clone)]
pub struct ServiceBridge {
    generation: u64,
    state: Arc<Mutex<State>>,
    budget: WorkBudget,
    sender: mpsc::Sender<Command>,
    finished: watch::Receiver<Option<IvyResult<()>>>,
}
pub struct ServiceOwner {
    state: Arc<Mutex<State>>,
    receiver: mpsc::Receiver<Command>,
    finished: watch::Sender<Option<IvyResult<()>>>,
}
fn not_ready() -> IvyError {
    IvyError::new(
        "service_not_ready",
        "TaskBoard has no reconciled current connection generation.",
    )
}
fn interrupted() -> IvyError {
    IvyError::new("internal_error", "Service invocation failed.").with_outcome(Outcome::Unknown)
}
pub fn service_bridge(generation: u64) -> (ServiceBridge, ServiceOwner) {
    let (sender, receiver) = mpsc::channel(MAX_PENDING_CALLS + 1);
    let (finished, observer) = watch::channel(None);
    let state = Arc::new(Mutex::new(State::default()));
    (
        ServiceBridge {
            generation,
            state: state.clone(),
            budget: WorkBudget::default(),
            sender,
            finished: observer,
        },
        ServiceOwner {
            state,
            receiver,
            finished,
        },
    )
}
impl ServiceBridge {
    pub fn retained_usage(&self) -> (usize, usize) {
        let state = self.state.lock().unwrap();
        (state.calls, self.budget.retained_usage().1)
    }
    pub fn work_budget(&self) -> WorkBudget {
        self.budget.clone()
    }
    /// Admission is synchronous, before the SDK can drop its reply observer.
    pub fn invoke(
        &self,
        tool: &str,
        args: Json,
        context: InvocationContext,
    ) -> ServiceFuture<Json> {
        let (reply, received) = oneshot::channel();
        let mut state = self.state.lock().unwrap();
        if state.stopped || context.generation != self.generation {
            return Box::pin(async { Err(not_ready()) });
        }
        if state.calls >= MAX_PENDING_CALLS {
            return Box::pin(async { Err(limit()) });
        }
        let overhead = 4096_usize
            .saturating_add(tool.len())
            .saturating_add(context.caller_principal_id.0.capacity().saturating_mul(2))
            .saturating_add(
                context
                    .operation_id
                    .as_ref()
                    .map_or(0, |id| id.0.capacity().saturating_mul(2)),
            );
        let bytes = overhead.saturating_add(args.retained_bytes());
        let work = match self.budget.call(bytes) {
            Ok(reservation) => reservation,
            Err(error) => return Box::pin(async { Err(error) }),
        };
        state.calls += 1;
        let reservation = Arc::new(Reservation {
            state: self.state.clone(),
            work,
            overhead,
        });
        let sent = self.sender.try_send(Command::Call(Call {
            tool: tool.to_owned(),
            args,
            context,
            reply,
            reservation: reservation.clone(),
        }));
        drop(state);
        Box::pin(async move {
            let _reservation = reservation;
            sent.map_err(|_| not_ready())?;
            received.await.map_err(|_| interrupted())?
        })
    }
    pub fn handler(&self, tool: &str) -> ToolHandler {
        let bridge = self.clone();
        let tool = tool.to_owned();
        Arc::new(move |args, context| bridge.invoke(&tool, args, context))
    }
    /// Stop entry eagerly, including if this returned observation is dropped.
    /// A single FIFO barrier follows every call admitted under the same lock.
    pub fn retire(&self) -> ServiceFuture<()> {
        let mut state = self.state.lock().unwrap();
        if !state.stopped {
            state.stopped = true;
            let _ = self.sender.try_send(Command::Retire);
        }
        let mut finished = self.finished.clone();
        Box::pin(async move {
            loop {
                if let Some(result) = finished.borrow_and_update().clone() {
                    return result;
                }
                finished.changed().await.map_err(|_| interrupted())?;
            }
        })
    }
}
impl Drop for ServiceOwner {
    fn drop(&mut self) {
        self.state.lock().unwrap().stopped = true;
        if self.finished.borrow().is_none() {
            self.finished.send_replace(Some(Err(interrupted())));
        }
    }
}
impl ServiceOwner {
    /// Retain all accepted local handlers until completion, even when a Send
    /// observer disappears. Then drain reconciliation/execution/projection. Only
    /// after this returns may the owner release its journal connection.
    pub async fn run<H: LocalHandler>(
        mut self,
        handler: &H,
        drain: impl Future<Output = IvyResult<()>>,
    ) -> IvyResult<()> {
        let mut calls = FuturesUnordered::new();
        let mut accepting = true;
        while accepting || !calls.is_empty() {
            tokio::select! {
                command = self.receiver.recv(), if accepting => match command {
                    Some(Command::Call(call)) => calls.push(async move {
                        let result = AssertUnwindSafe(async {
                            handler.handle(&call.tool, call.args, call.context).await
                        }).catch_unwind().await.unwrap_or_else(|_| Err(interrupted()));
                        if call.reply.is_closed() {
                            return;
                        }
                        let result = call.reservation.reply(result);
                        let _ = call.reply.send(result);
                    }),
                    Some(Command::Retire) | None => {
                        accepting = false;
                        self.state.lock().unwrap().stopped = true;
                        self.receiver.close();
                    }
                },
                _ = calls.next(), if !calls.is_empty() => {}
            }
        }
        let result = drain.await;
        self.finished.send_replace(Some(result.clone()));
        result
    }
}
