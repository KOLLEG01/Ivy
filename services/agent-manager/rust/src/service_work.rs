//! Accepted AgentManager work survives its caller and drains at owner shutdown.
use ivy_contracts::{IvyError, IvyJson, IvyResult};
use ivy_sdk::service::{ServiceFuture, ToolHandler};
use std::{
    collections::BTreeMap,
    future::Future,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

#[path = "service_run.rs"]
pub(crate) mod run;
pub use run::AgentRun;
#[derive(Clone, Copy)]
enum HandlerKind {
    Request,
    Answer,
    Control,
    Invoke,
}
#[derive(Default)]
struct State {
    closed: bool,
    replies_closed: bool,
    next: u64,
    pending: BTreeMap<u64, (bool, watch::Receiver<bool>)>,
    budget: run::Budget,
}
#[derive(Clone, Default)]
pub struct AgentWork(Arc<Mutex<State>>);
struct Completion {
    owner: AgentWork,
    index: u64,
    released: bool,
    done: Option<watch::Sender<bool>>,
    report: Option<Arc<dyn Fn(IvyError) + Send + Sync>>,
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
        if let Some(report) = self.report.take() {
            report(IvyError::internal());
        }
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
        self.track_with(|| future, false, false, 0)
    }
    fn track_with(
        &self,
        create: impl FnOnce() -> ServiceFuture<IvyJson>,
        reply: bool,
        control: bool,
        input: usize,
    ) -> IvyResult<AgentRun> {
        let completion = self.reserve(reply, None)?;
        let budget = self
            .0
            .lock()
            .map_err(|_| IvyError::internal())?
            .budget
            .clone();
        let admission = budget.reserve(control, input)?;
        let (mut result, receiver) = AgentRun::channel(admission.call.clone());
        let future = create();
        tokio::spawn(async move {
            let _input = admission;
            let mut completion = completion;
            let value = future.await;
            // The original registers its Set deletion reaction before a caller
            // can await the returned promise. Drain completion follows the reply.
            completion.release();
            result.send(budget.finish(value, control));
            completion.finish();
        });
        Ok(receiver)
    }
    fn reserve(
        &self,
        reply: bool,
        report: Option<Arc<dyn Fn(IvyError) + Send + Sync>>,
    ) -> IvyResult<Completion> {
        let (done, complete) = watch::channel(false);
        let index = {
            let mut state = self.0.lock().map_err(|_| IvyError::internal())?;
            if state.replies_closed || (state.closed && !reply) {
                return Err(IvyError::new(
                    "service_unavailable",
                    "AgentManager is stopping; the request was not accepted.",
                ));
            }
            while state.pending.contains_key(&state.next) {
                state.next = state.next.wrapping_add(1);
            }
            let index = state.next;
            state.next = state.next.wrapping_add(1);
            state.pending.insert(index, (reply, complete));
            index
        };
        Ok(Completion {
            owner: self.clone(),
            index,
            released: false,
            done: Some(done),
            report,
        })
    }
    /// Receipt writers share admission/completion accounting, with no unused
    /// result channel. Reporting precedes removal and the drain notification.
    pub(crate) fn spawn_reported(
        &self,
        future: impl Future<Output = IvyResult<()>> + Send + 'static,
        report: Arc<dyn Fn(IvyError) + Send + Sync>,
    ) -> IvyResult<()> {
        let completion = self.reserve(false, Some(report))?;
        tokio::spawn(async move {
            let mut completion = completion;
            let value = future.await;
            if let (Some(report), Err(error)) = (completion.report.take(), value) {
                report(error);
            }
            completion.release();
            completion.finish();
        });
        Ok(())
    }
    /// Unlike an invocation snapshot, receipt drain includes work accepted while
    /// earlier receipts settle. The native owner closes before the final drain.
    pub(crate) async fn drain(&self) -> IvyResult<()> {
        loop {
            let next = self
                .0
                .lock()
                .map_err(|_| IvyError::internal())?
                .pending
                .first_key_value()
                .map(|(_, (_, complete))| complete.clone());
            let Some(complete) = next else {
                return Ok(());
            };
            Self::wait_complete(complete).await;
        }
    }
    /// Capture the current work set immediately. Later requests are not silently
    /// added to this snapshot; the enclosing service owns invocation retirement.
    pub fn drain_snapshot(&self) -> ServiceFuture<()> {
        let snapshot = self
            .0
            .lock()
            .map(|state| {
                state
                    .pending
                    .values()
                    .map(|(_, complete)| complete.clone())
                    .collect::<Vec<_>>()
            })
            .map_err(|_| IvyError::internal());
        Self::wait_snapshot(snapshot)
    }
    /// Retire admission and capture accepted work under the same lock. The
    /// native owner must remain available until this work has settled.
    pub fn close(&self) -> ServiceFuture<()> {
        self.close_after(Box::pin(async { Ok(()) }))
    }
    /// Asynchronous native work can outlive its accepting RPC. Keep replies
    /// admissible until that work, too, has reached its retirement boundary.
    pub fn close_after(&self, native_work: ServiceFuture<()>) -> ServiceFuture<()> {
        let snapshot = self
            .0
            .lock()
            .map(|mut state| {
                state.closed = true;
                state
                    .pending
                    .values()
                    .filter(|(reply, _)| !reply)
                    .map(|(_, complete)| complete.clone())
                    .collect::<Vec<_>>()
            })
            .map_err(|_| IvyError::internal());
        let owner = self.clone();
        Box::pin(async move {
            Self::wait_snapshot(snapshot).await?;
            native_work.await?;
            // A pending native request may need an answer before it can finish.
            // Retire answers only once the ordinary invocations have settled.
            let replies = owner
                .0
                .lock()
                .map(|mut state| {
                    state.replies_closed = true;
                    state
                        .pending
                        .values()
                        .map(|(_, complete)| complete.clone())
                        .collect::<Vec<_>>()
                })
                .map_err(|_| IvyError::internal());
            Self::wait_snapshot(replies).await
        })
    }
    fn wait_snapshot(snapshot: IvyResult<Vec<watch::Receiver<bool>>>) -> ServiceFuture<()> {
        Box::pin(async move {
            for complete in snapshot? {
                Self::wait_complete(complete).await;
            }
            Ok(())
        })
    }
    async fn wait_complete(mut complete: watch::Receiver<bool>) {
        while !*complete.borrow_and_update() {
            if complete.changed().await.is_err() {
                break;
            }
        }
    }
    pub fn handler(&self, handler: ToolHandler) -> ToolHandler {
        self.wrap(handler, HandlerKind::Request)
    }
    /// Answers to an existing native input remain admissible while calls drain.
    pub fn reply_handler(&self, handler: ToolHandler) -> ToolHandler {
        self.wrap(handler, HandlerKind::Answer)
    }
    pub(crate) fn control_handler(&self, handler: ToolHandler) -> ToolHandler {
        self.wrap(handler, HandlerKind::Control)
    }
    pub(crate) fn invoke_handler(&self, handler: ToolHandler) -> ToolHandler {
        self.wrap(handler, HandlerKind::Invoke)
    }
    fn wrap(&self, handler: ToolHandler, kind: HandlerKind) -> ToolHandler {
        let owner = self.clone();
        Arc::new(move |args, context| {
            let reply = matches!(kind, HandlerKind::Answer);
            let control = matches!(kind, HandlerKind::Answer | HandlerKind::Control)
                || (matches!(kind, HandlerKind::Invoke)
                    && args
                        .get("method")
                        .and_then(IvyJson::as_js_string)
                        .is_some_and(|method| {
                            method.eq_str("turn/interrupt") || method.eq_str("thread/unsubscribe")
                        }));
            let input = args
                .retained_bytes()
                .saturating_add(context.caller_principal_id.0.capacity().saturating_mul(2))
                .saturating_add(
                    context
                        .operation_id
                        .as_ref()
                        .map_or(0, |id| id.0.capacity().saturating_mul(2)),
                );
            match owner.track_with(|| handler(args, context), reply, control, input) {
                Ok(run) => Box::pin(run.wait()),
                Err(error) => Box::pin(async move { Err(error) }),
            }
        })
    }
}
