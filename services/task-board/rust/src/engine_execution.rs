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
    future::Future,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll},
};
#[path = "execution_completion.rs"]
pub(crate) mod completion;

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
fn s<'a>(v: &'a Json, k: &str) -> IvyResult<&'a JsString> {
    v.required(k)?.as_js_string().ok_or_else(IvyError::internal)
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
        let gates = self.coordination();
        let id = s(&operation.pin, "objectId")?.clone();
        if is(&operation.value, "phase", "succeeded") {
            gates.release(&id, false).await?;
            return Ok(operation.value.required("outcome")?.clone());
        }
        if is(&operation.value, "phase", "failed") {
            gates.release(&id, false).await?;
            let saved = operation.value.required("error")?;
            let mut error =
                IvyError::new_js(s(saved, "code")?.clone(), s(saved, "message")?.clone());
            if let Some(execution) = saved.get("execution") {
                error = error.with_provider_outcome(execution.clone());
            }
            return Err(error);
        }
        let request = operation.value.required("request")?.clone();
        let mut transcript = None;
        if write_count(&operation.value)? == 0
            && is(&request, "action", "nativeUpdate")
            && is(field(&request, "change"), "kind", "unavailable")
            && is(field(&request, "change"), "code", "native_process_lost")
        {
            let preflight = async {
                let run = self
                    .operations
                    .store
                    .read("task-board/run", request.required("run")?, true)
                    .await?;
                if !self.services.owner_stopped(&run.value).await? {
                    return Err(IvyError::new(
                        "task_board_outcome_unresolved",
                        "The original native process loss is not confirmed by its AgentManager.",
                    ));
                }
                Ok(())
            }
            .await;
            if let Err(error) = preflight {
                return self.failed_publication(&id, error).await;
            }
        }
        if is(&request, "action", "nativeUpdate")
            && is(field(&request, "change"), "kind", "transcript")
        {
            match self
                .services
                .verify_transcript(
                    request.required("run")?,
                    request.required("change")?.required("evidence")?,
                )
                .await
            {
                Ok(value) => transcript = Some(value),
                Err(error) => return self.failed_publication(&id, error).await,
            }
        }
        let category = if is(&request, "action", "create") || is(&request, "action", "edit") {
            field(field(&request, "fields"), "category")
                .as_js_string()
                .cloned()
        } else {
            None
        };
        let mut normalized = None;
        if write_count(&operation.value)? == 0 {
            if let Some(category) = &category {
                normalized = Some(self.services.normalize(category).await.map_err(unknown)?);
            }
        }
        async {
            if !gates.reserved(&operation).await? {
                gates.acquire(&operation).await?;
            }
            Ok::<_, IvyError>(())
        }
        .await
        .map_err(unknown)?;
        let mut released_category_retry = false;
        let result = async {
            operation = self
                .operations
                .store
                .read("task-board/operation", &Json::String(id.clone()), true)
                .await?;
            if gates.reserved(&operation).await? {
                gates.release(&id, true).await?;
            }
            if is(&operation.value, "phase", "succeeded") {
                gates.release(&id, false).await?;
                return Ok(operation.value.required("outcome")?.clone());
            }
            if is(&operation.value, "phase", "failed") {
                return Err(IvyError::new(
                    "task_board_operation_failed",
                    "The accepted action has already failed conclusively.",
                ));
            }
            if write_count(&operation.value)? == 0 {
                if let Some(category) = &category {
                    match self.services.revalidate(category).await {
                        Ok(value) => normalized = Some(value),
                        Err(error) => {
                            released_category_retry = gates.release_unstarted(&id).await?;
                            return Err(unknown(error));
                        }
                    }
                }
            }
            let published = match self
                .services
                .publish(&operation, transcript.as_ref(), normalized.as_ref())
                .await
            {
                Ok(value) => value,
                Err(error) => {
                    let concurrent = self
                        .operations
                        .store
                        .read("task-board/operation", &Json::String(id.clone()), true)
                        .await?;
                    if error.code != "revision_conflict"
                        || write_count(&concurrent.value)? <= write_count(&operation.value)?
                    {
                        return Err(error);
                    }
                    if is(&concurrent.value, "phase", "succeeded") {
                        gates.release(&id, false).await?;
                        return Ok(concurrent.value.required("outcome")?.clone());
                    }
                    self.services
                        .publish(&concurrent, transcript.as_ref(), normalized.as_ref())
                        .await?
                }
            };
            let outcome = self.operations.finish(&id, &published).await?;
            gates.release(&id, false).await?;
            Ok(outcome)
        }
        .await;
        match result {
            Ok(outcome) => Ok(outcome),
            Err(error) if released_category_retry => Err(error),
            Err(error) => self.failed_publication(&id, error).await,
        }
    }
    pub async fn failed_publication(&self, id: &JsString, cause: IvyError) -> IvyResult<Json> {
        let saved = self
            .operations
            .store
            .read("task-board/operation", &Json::String(id.clone()), true)
            .await?;
        let gates = self.coordination();
        if is(&saved.value, "phase", "succeeded") {
            gates.release(id, false).await?;
            return Ok(saved.value.required("outcome")?.clone());
        }
        let writes = saved
            .value
            .required("writes")?
            .as_array()
            .ok_or_else(IvyError::internal)?;
        let reserved = writes
            .first()
            .is_some_and(|write| is(write, "contractKey", "task-board/task"));
        let unresolved_start = field(&saved.value, "nativeCalls")
            .as_array()
            .unwrap_or(&[])
            .iter()
            .any(|call| {
                is(call, "method", "thread/start")
                    && call
                        .get("observation")
                        .is_some_and(|o| !o.is_null() && !is(o, "phase", "failed"))
            });
        let uncertain = cause.outcome == Outcome::Unknown
            || unresolved_start
            || writes
                .iter()
                .any(|write| write.get("outcome").is_some_and(|o| !o.is_null()))
            || reserved && cause.outcome != Outcome::NotExecuted;
        let reported = if uncertain { unknown(cause) } else { cause };
        self.operations.failure(id, &reported, uncertain).await?;
        if !uncertain {
            self.operations.store.release_workspace_operation(id)?;
            gates.release(id, false).await?;
        }
        Err(reported)
    }
}
fn field<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn is(value: &Json, key: &str, expected: &str) -> bool {
    value.get(key) == Some(&expected.into())
}
fn write_count(value: &Json) -> IvyResult<usize> {
    Ok(value
        .required("writes")?
        .as_array()
        .ok_or_else(IvyError::internal)?
        .len())
}
fn unknown(error: IvyError) -> IvyError {
    error.with_outcome(Outcome::Unknown)
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
pub struct ExecutionRequest {
    request: completion::Request<Json, IvyError>,
}
impl Future for ExecutionRequest {
    type Output = IvyResult<Json>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.get_mut().request).poll(cx)
    }
}
pub struct ExecutionQueue<'a, 'x, P, X> {
    program: &'a P,
    executor: &'x X,
    state: Rc<RefCell<completion::State<Json, IvyError>>>,
}
impl<'a, 'x, P: ExecutionProgram + 'a, X: ReconcileExecutor<'a>> ExecutionQueue<'a, 'x, P, X> {
    pub fn new(program: &'a P, executor: &'x X) -> Self {
        Self {
            program,
            executor,
            state: Rc::new(RefCell::new(completion::State::default())),
        }
    }
    /// Coalescing is eager and dropping any caller never cancels admitted work.
    pub fn execute(&self, operation: Document) -> IvyResult<ExecutionRequest> {
        let id = s(&operation.pin, "objectId")?.clone();
        let (request, active) = completion::admit(&self.state, id);
        if let Some(active) = active {
            let program = self.program;
            self.executor.spawn(Box::pin(async move {
                active.complete(program.run(operation).await);
            }));
        }
        Ok(ExecutionRequest { request })
    }
    /// The owner stops admissions before draining and closing the journal.
    pub async fn settled(&self) {
        completion::settled(&self.state).await;
    }
}
