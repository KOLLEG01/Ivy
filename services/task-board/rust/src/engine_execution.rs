//! Retained publication recovery and coalesced execution on the scoped owner.
//! Domain publication, current read scope and graph evidence stay explicit.
use crate::{
    journal_store::{Contracts, JournalStore, ReadScopes},
    lifecycle_journal::LifecycleJournal,
    local_workspaces::{LocalWorkspaces, workspace_key},
    operations::{Document, Operations, Store},
    publication_coordination::{CoordinationStore, PublicationCoordination},
    reconciler::ReconcileExecutor,
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, Outcome};
use ivy_sdk::rpc::RpcClient;
use std::{
    cell::RefCell,
    collections::HashMap,
    future::{Future, poll_fn},
    pin::Pin,
    rc::Rc,
    task::{Context, Poll, Waker},
};
pub trait ExecutionStore: CoordinationStore {
    fn release_workspace_operation(&self, id: &JsString) -> IvyResult<()>;
}
impl<R: RpcClient + 'static, P: Contracts, C: Fn() -> String, I: Fn() -> JsString> ExecutionStore
    for JournalStore<'_, R, P, C, I>
{
    fn release_workspace_operation(&self, id: &JsString) -> IvyResult<()> {
        LocalWorkspaces {
            connection: self.journal.connection,
            store: self,
            normalize: workspace_key,
        }
        .release_operation(id)
    }
}
impl<R: RpcClient + 'static, P: Contracts, C: Fn() -> String, I: Fn() -> JsString, Q> ExecutionStore
    for LifecycleJournal<'_, '_, R, P, C, I, Q>
{
    fn release_workspace_operation(&self, id: &JsString) -> IvyResult<()> {
        self.journal.release_workspace_operation(id)
    }
}
#[allow(async_fn_in_trait)]
pub trait ExecutionServices {
    type Transcript;
    async fn with_read_cache<F: Future<Output = IvyResult<Json>>>(
        &self,
        work: F,
    ) -> IvyResult<Json>;
    async fn owner_stopped(&self, run: &Json) -> IvyResult<bool>;
    async fn verify_transcript(&self, run: &Json, artifact: &Json) -> IvyResult<Self::Transcript>;
    async fn normalize(&self, category: &JsString) -> IvyResult<JsString>;
    async fn revalidate(&self, category: &JsString) -> IvyResult<JsString>;
    async fn publish(
        &self,
        operation: &Document,
        transcript: Option<&Self::Transcript>,
        normalized_category: Option<&JsString>,
    ) -> IvyResult<Json>;
}
#[allow(async_fn_in_trait)]
pub trait ExecutionCategories {
    async fn normalize(&self, value: &JsString) -> IvyResult<JsString>;
    async fn revalidate(&self, value: &JsString) -> IvyResult<JsString>;
}
impl<
    'a,
    S: crate::category_projection::CategoryStore + 'a,
    L: crate::category_projection::CategoryLocale + 'a,
    X: ReconcileExecutor<'a>,
> ExecutionCategories for crate::category_projection::CategoryProjection<'a, '_, S, L, X>
{
    async fn normalize(&self, v: &JsString) -> IvyResult<JsString> {
        crate::category_projection::CategoryProjection::normalize(self, Some(v))
            .await?
            .as_js_string()
            .cloned()
            .ok_or_else(IvyError::internal)
    }
    async fn revalidate(&self, v: &JsString) -> IvyResult<JsString> {
        crate::category_projection::CategoryProjection::revalidate(self, v)
            .await?
            .as_js_string()
            .cloned()
            .ok_or_else(IvyError::internal)
    }
}
#[allow(async_fn_in_trait)]
pub trait ExecutionProofs {
    type Transcript;
    async fn owner_stopped(&self, run: &Json) -> IvyResult<bool>;
    async fn verify_transcript(&self, run: &Json, artifact: &Json) -> IvyResult<Self::Transcript>;
}
pub struct NativeExecutionProofs<'a, I, S> {
    pub inspector: &'a crate::native_inspector::NativeInspector<'a, I>,
    pub turns: crate::native_turn_evidence::TurnEvidence<'a, S>,
}
impl<
    I: crate::native_inspector::InspectionStore,
    S: crate::native_turn_evidence::TurnEvidenceStore + Store,
> ExecutionProofs for NativeExecutionProofs<'_, I, S>
{
    type Transcript = crate::native_turn_evidence::Transcript;
    async fn owner_stopped(&self, run: &Json) -> IvyResult<bool> {
        self.inspector.owner_stopped(run).await
    }
    async fn verify_transcript(&self, run: &Json, artifact: &Json) -> IvyResult<Self::Transcript> {
        crate::full_turn_evidence::verify_native_transcript(
            crate::native_turn_evidence::TurnEvidence {
                store: self.turns.store,
                catalogs: self.turns.catalogs,
                principal_id: self.turns.principal_id,
            },
            run,
            artifact,
        )
        .await
    }
}
#[allow(async_fn_in_trait)]
pub trait ExecutionPublication<T> {
    async fn publish(
        &self,
        operation: &Document,
        transcript: Option<&T>,
        normalized: Option<&JsString>,
    ) -> IvyResult<Json>;
}
/// The owner binds a fresh action scope, shared only by its nested work. There
/// is no service-lifetime cache or uncached production fallback here.
#[allow(async_fn_in_trait)]
pub trait ExecutionReadScope {
    async fn with_read_cache<F: Future<Output = IvyResult<Json>>>(
        &self,
        work: F,
    ) -> IvyResult<Json>;
}
impl ExecutionReadScope for ReadScopes {
    async fn with_read_cache<F: Future<Output = IvyResult<Json>>>(
        &self,
        work: F,
    ) -> IvyResult<Json> {
        ReadScopes::with_read_cache(self, work).await
    }
}
/// Keep async-local read context when an action submits category, execution or
/// reconciliation work to the same owner. The underlying executor retains it.
pub struct ReadScopeExecutor<'a, 'x, X> {
    pub scopes: &'a ReadScopes,
    pub executor: &'x X,
}
impl<'a, X: ReconcileExecutor<'a>> ReconcileExecutor<'a> for ReadScopeExecutor<'a, '_, X> {
    fn spawn(&self, work: Pin<Box<dyn Future<Output = ()> + 'a>>) {
        self.executor.spawn(Box::pin(self.scopes.inherit(work)));
    }
}
pub struct ExecutionComponents<'a, K, N, P, R> {
    pub categories: &'a K,
    pub proofs: &'a N,
    pub publication: &'a P,
    pub read_scope: &'a R,
}
impl<
    K: ExecutionCategories,
    N: ExecutionProofs,
    P: ExecutionPublication<N::Transcript>,
    R: ExecutionReadScope,
> ExecutionServices for ExecutionComponents<'_, K, N, P, R>
{
    type Transcript = N::Transcript;
    async fn with_read_cache<F: Future<Output = IvyResult<Json>>>(
        &self,
        work: F,
    ) -> IvyResult<Json> {
        self.read_scope.with_read_cache(work).await
    }
    async fn owner_stopped(&self, run: &Json) -> IvyResult<bool> {
        self.proofs.owner_stopped(run).await
    }
    async fn verify_transcript(&self, run: &Json, artifact: &Json) -> IvyResult<Self::Transcript> {
        self.proofs.verify_transcript(run, artifact).await
    }
    async fn normalize(&self, value: &JsString) -> IvyResult<JsString> {
        self.categories.normalize(value).await
    }
    async fn revalidate(&self, value: &JsString) -> IvyResult<JsString> {
        self.categories.revalidate(value).await
    }
    async fn publish(
        &self,
        operation: &Document,
        transcript: Option<&Self::Transcript>,
        normalized: Option<&JsString>,
    ) -> IvyResult<Json> {
        self.publication
            .publish(operation, transcript, normalized)
            .await
    }
}
pub struct CachedExecution<'a, S, C, P> {
    pub operations: &'a Operations<S, C>,
    pub principal_id: &'a JsString,
    pub services: &'a P,
}
fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn s<'a>(v: &'a Json, k: &str) -> IvyResult<&'a JsString> {
    v.required(k)?.as_js_string().ok_or_else(IvyError::internal)
}
fn a(v: &Json) -> &[Json] {
    v.as_array().unwrap_or(&[])
}
fn eq(v: &Json, k: &str, t: &str) -> bool {
    v.get(k) == Some(&Json::from(t))
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn outcome(v: &Json) -> Outcome {
    match v.as_str().unwrap_or_default().as_str() {
        "unknown" => Outcome::Unknown,
        "completed" => Outcome::Completed,
        _ => Outcome::NotExecuted,
    }
}
fn category(v: &Json) -> Option<&JsString> {
    if eq(v, "action", "create") || eq(v, "action", "edit") {
        f(f(v, "fields"), "category").as_js_string()
    } else {
        None
    }
}
impl<S: ExecutionStore, C: Fn() -> String, P: ExecutionServices> CachedExecution<'_, S, C, P> {
    fn coordination(&self) -> PublicationCoordination<'_, S, C> {
        PublicationCoordination {
            store: &self.operations.store,
            principal_id: self.principal_id,
            now: &self.operations.now,
        }
    }
    pub async fn execute_cached(&self, mut operation: Document) -> IvyResult<Json> {
        let id = s(&operation.pin, "objectId")?.clone();
        let gates = self.coordination();
        if eq(&operation.value, "phase", "succeeded") {
            gates.release(&id, false).await?;
            return Ok(operation.value.required("outcome")?.clone());
        }
        if eq(&operation.value, "phase", "failed") {
            gates.release(&id, false).await?;
            let e = f(&operation.value, "error");
            return Err(
                IvyError::new_js(s(e, "code")?.clone(), s(e, "message")?.clone())
                    .with_outcome(outcome(f(e, "execution"))),
            );
        }
        let request = operation.value.required("request")?.clone();
        let mut normalized = None;
        let mut transcript = None;
        if a(f(&operation.value, "writes")).is_empty()
            && eq(&request, "action", "nativeUpdate")
            && eq(f(&request, "change"), "kind", "unavailable")
            && eq(f(&request, "change"), "code", "native_process_lost")
        {
            let preflight = async {
                let run = self
                    .operations
                    .store
                    .read("task-board/run", f(&request, "run"), true)
                    .await?;
                require(
                    self.services.owner_stopped(&run.value).await?,
                    "task_board_outcome_unresolved",
                    "The original native process loss is not confirmed by its AgentManager.",
                )
            }
            .await;
            if let Err(e) = preflight {
                return self.failed_publication(&id, e).await;
            }
        }
        if eq(&request, "action", "nativeUpdate") && eq(f(&request, "change"), "kind", "transcript")
        {
            match self
                .services
                .verify_transcript(f(&request, "run"), f(f(&request, "change"), "evidence"))
                .await
            {
                Ok(v) => transcript = Some(v),
                Err(e) => return self.failed_publication(&id, e).await,
            }
        }
        if a(f(&operation.value, "writes")).is_empty() {
            if let Some(c) = category(&request) {
                normalized = Some(
                    self.services
                        .normalize(c)
                        .await
                        .map_err(|e| e.with_outcome(Outcome::Unknown))?,
                );
            }
        }
        let acquired = async {
            if !gates.reserved(&operation).await? {
                gates.acquire(&operation).await?
            }
            Ok(())
        }
        .await;
        acquired.map_err(|e: IvyError| e.with_outcome(Outcome::Unknown))?;
        let mut released_category_retry = false;
        let result = async {
            operation = self
                .operations
                .store
                .read("task-board/operation", &Json::String(id.clone()), true)
                .await?;
            if gates.reserved(&operation).await? {
                gates.release(&id, true).await?
            }
            if eq(&operation.value, "phase", "succeeded") {
                gates.release(&id, false).await?;
                return Ok(operation.value.required("outcome")?.clone());
            }
            require(
                !eq(&operation.value, "phase", "failed"),
                "task_board_operation_failed",
                "The accepted action has already failed conclusively.",
            )?;
            if a(f(&operation.value, "writes")).is_empty() {
                if let Some(c) = category(&request) {
                    match self.services.revalidate(c).await {
                        Ok(v) => normalized = Some(v),
                        Err(e) => {
                            released_category_retry = gates.release_unstarted(&id).await?;
                            return Err(e.with_outcome(Outcome::Unknown));
                        }
                    }
                }
            }
            let published = match self
                .services
                .publish(&operation, transcript.as_ref(), normalized.as_ref())
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    let concurrent = self
                        .operations
                        .store
                        .read("task-board/operation", &Json::String(id.clone()), true)
                        .await?;
                    if e.code != "revision_conflict"
                        || a(f(&concurrent.value, "writes")).len()
                            <= a(f(&operation.value, "writes")).len()
                    {
                        return Err(e);
                    }
                    if eq(&concurrent.value, "phase", "succeeded") {
                        gates.release(&id, false).await?;
                        return Ok(concurrent.value.required("outcome")?.clone());
                    }
                    self.services
                        .publish(&concurrent, transcript.as_ref(), normalized.as_ref())
                        .await?
                }
            };
            let result = self.operations.finish(&id, &published).await?;
            gates.release(&id, false).await?;
            Ok(result)
        }
        .await;
        match result {
            Ok(v) => Ok(v),
            Err(e) if released_category_retry => Err(e),
            Err(e) => self.failed_publication(&id, e).await,
        }
    }
    pub async fn failed_publication(&self, id: &JsString, cause: IvyError) -> IvyResult<Json> {
        let saved = self
            .operations
            .store
            .read("task-board/operation", &Json::String(id.clone()), true)
            .await?;
        if eq(&saved.value, "phase", "succeeded") {
            self.coordination().release(id, false).await?;
            return Ok(saved.value.required("outcome")?.clone());
        }
        let writes = a(f(&saved.value, "writes"));
        let reserved = writes
            .first()
            .is_some_and(|w| eq(w, "contractKey", "task-board/task"));
        let uncertain = cause.outcome == Outcome::Unknown
            || a(f(&saved.value, "nativeCalls")).iter().any(|call| {
                eq(call, "method", "thread/start")
                    && !f(call, "observation").is_null()
                    && !eq(f(call, "observation"), "phase", "failed")
            })
            || writes.iter().any(|w| !f(w, "outcome").is_null())
            || reserved && cause.outcome != Outcome::NotExecuted;
        let reported = if uncertain {
            cause.with_outcome(Outcome::Unknown)
        } else {
            cause
        };
        self.operations.failure(id, &reported, uncertain).await?;
        if !uncertain {
            self.operations.store.release_workspace_operation(id)?;
            self.coordination().release(id, false).await?
        }
        Err(reported)
    }
}
#[allow(async_fn_in_trait)]
pub trait ExecutionProgram {
    async fn run(&self, operation: Document) -> IvyResult<Json>;
}
impl<S: ExecutionStore, C: Fn() -> String, P: ExecutionServices> ExecutionProgram
    for CachedExecution<'_, S, C, P>
{
    async fn run(&self, operation: Document) -> IvyResult<Json> {
        self.services
            .with_read_cache(self.execute_cached(operation))
            .await
    }
}
#[derive(Default)]
struct Completion {
    result: Option<IvyResult<Json>>,
    waiters: Vec<Waker>,
}
#[derive(Default)]
struct State {
    active: HashMap<JsString, Rc<RefCell<Completion>>>,
    waiters: Vec<Waker>,
}
struct ActiveExecution {
    id: JsString,
    state: Rc<RefCell<State>>,
    completion: Rc<RefCell<Completion>>,
}
impl Drop for ActiveExecution {
    fn drop(&mut self) {
        let replies = {
            let mut completion = self.completion.borrow_mut();
            completion.result.get_or_insert_with(|| {
                Err(
                    IvyError::new("internal_error", "Service invocation failed.")
                        .with_outcome(Outcome::Unknown),
                )
            });
            std::mem::take(&mut completion.waiters)
        };
        let drained = {
            let mut state = self.state.borrow_mut();
            if state
                .active
                .get(&self.id)
                .is_some_and(|c| Rc::ptr_eq(c, &self.completion))
            {
                state.active.remove(&self.id);
            }
            if state.active.is_empty() {
                std::mem::take(&mut state.waiters)
            } else {
                vec![]
            }
        };
        for waiter in replies.into_iter().chain(drained) {
            waiter.wake();
        }
    }
}
pub struct ExecutionRequest {
    completion: Rc<RefCell<Completion>>,
}
impl Future for ExecutionRequest {
    type Output = IvyResult<Json>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut c = self.completion.borrow_mut();
        if let Some(v) = &c.result {
            Poll::Ready(v.clone())
        } else {
            if !c.waiters.iter().any(|w| w.will_wake(cx.waker())) {
                c.waiters.push(cx.waker().clone())
            }
            Poll::Pending
        }
    }
}
pub struct ExecutionQueue<'a, 'x, P, X> {
    program: &'a P,
    executor: &'x X,
    state: Rc<RefCell<State>>,
}
impl<'a, 'x, P: ExecutionProgram + 'a, X: ReconcileExecutor<'a>> ExecutionQueue<'a, 'x, P, X> {
    pub fn new(program: &'a P, executor: &'x X) -> Self {
        Self {
            program,
            executor,
            state: Rc::new(RefCell::new(State::default())),
        }
    }
    /// Coalescing is eager and dropping any caller never cancels admitted work.
    pub fn execute(&self, operation: Document) -> IvyResult<ExecutionRequest> {
        let id = s(&operation.pin, "objectId")?.clone();
        if let Some(c) = self.state.borrow().active.get(&id) {
            return Ok(ExecutionRequest {
                completion: c.clone(),
            });
        }
        let completion = Rc::new(RefCell::new(Completion::default()));
        self.state
            .borrow_mut()
            .active
            .insert(id.clone(), completion.clone());
        // Create the guard before spawning, so an owner interruption before
        // the first poll also releases the in-memory identity and observers.
        let active = ActiveExecution {
            id,
            state: self.state.clone(),
            completion: completion.clone(),
        };
        let program = self.program;
        self.executor.spawn(Box::pin(async move {
            let result = program.run(operation).await;
            active.completion.borrow_mut().result = Some(result);
            drop(active);
        }));
        Ok(ExecutionRequest { completion })
    }
    /// The owner stops admissions before draining and closing the journal.
    pub async fn settled(&self) {
        poll_fn(|cx| {
            let mut state = self.state.borrow_mut();
            if state.active.is_empty() {
                Poll::Ready(())
            } else {
                if !state.waiters.iter().any(|w| w.will_wake(cx.waker())) {
                    state.waiters.push(cx.waker().clone())
                }
                Poll::Pending
            }
        })
        .await
    }
}
