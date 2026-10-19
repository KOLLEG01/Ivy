//! Publication dispatch and root-level configure, plan and Task creation.
//! Current creation checks and Task-written policy are required owner bindings.
use crate::{
    allocation::plan_semantics,
    configuration::{CONFIGURATION_NAME, check_configuration, configuration_view},
    coordination::{GraphError, Parent, validate_parent},
    engine_execution::ExecutionPublication,
    journal_store::{Contracts, JournalStore},
    lifecycle_journal::{LifecycleJournal, LifecyclePages},
    native_lifecycle::{
        LifecycleEngine, LifecycleOperations, LifecyclePublication, LifecycleStore, NativeLifecycle,
    },
    native_plan::resolved_plan,
    native_publication::{
        NativePublicationEngine, NativePublicationEvidence, NativePublicationStore,
        NativePublisher, VerifiedPublicationTranscript,
    },
    native_turn_evidence::Transcript,
    operations::{Document, Operations},
    publication::{Publication, TaskWritten},
    task_keys,
    task_written::TaskWriteSink,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts, hash::hash_json, json::canonical,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::RpcClient;

#[allow(async_fn_in_trait)]
pub trait RootPublicationStore: LifecycleStore {
    async fn allocate_task_key(&self, operation: &JsString) -> IvyResult<JsString>;
}
impl<
    R: RpcClient + 'static,
    P: Contracts,
    C: Fn() -> String,
    I: Fn() -> JsString,
    Q: LifecyclePages,
> RootPublicationStore for LifecycleJournal<'_, '_, R, P, C, I, Q>
{
    async fn allocate_task_key(&self, operation: &JsString) -> IvyResult<JsString> {
        task_keys::allocate(self, &self.journal.now, operation).await
    }
}
#[allow(async_fn_in_trait)]
pub trait CreationChecks {
    async fn fields(&self, fields: &Json) -> IvyResult<()>;
    async fn initial_state(
        &self,
        request: &Json,
        fields: &Json,
        actor: &Json,
        at: &JsString,
    ) -> IvyResult<Json>;
    async fn execution_options(&self, fields: &Json, allow_unavailable: bool) -> IvyResult<()>;
}
#[allow(async_fn_in_trait)]
pub trait RootPublicationChecks: CreationChecks {
    async fn configuration(&self, value: &Json) -> IvyResult<()>;
}
/// Bind configuration to the actual journal validator/discovery; the accepted
/// current Task creation policy remains explicit and has no frozen fallback.
pub struct RootChecks<'a, S, P> {
    pub journal: &'a S,
    pub creation: &'a P,
}
impl<S, P: CreationChecks> CreationChecks for RootChecks<'_, S, P> {
    async fn fields(&self, fields: &Json) -> IvyResult<()> {
        self.creation.fields(fields).await
    }
    async fn initial_state(
        &self,
        request: &Json,
        fields: &Json,
        actor: &Json,
        at: &JsString,
    ) -> IvyResult<Json> {
        self.creation
            .initial_state(request, fields, actor, at)
            .await
    }
    async fn execution_options(&self, fields: &Json, unavailable: bool) -> IvyResult<()> {
        self.creation.execution_options(fields, unavailable).await
    }
}
impl<R: RpcClient + 'static, V: Contracts, C, I, P: CreationChecks> RootPublicationChecks
    for RootChecks<'_, JournalStore<'_, R, V, C, I>, P>
{
    async fn configuration(&self, value: &Json) -> IvyResult<()> {
        check_configuration(self.journal, value, None, false).await
    }
}
#[allow(async_fn_in_trait)]
pub trait PublicationRoutes<T> {
    async fn native_update<P: LifecyclePublication>(
        &self,
        operation: &Document,
        steps: &mut P,
        transcript: Option<&T>,
    ) -> IvyResult<Json>;
    async fn lifecycle<P: LifecyclePublication>(
        &self,
        operation: &Document,
        steps: &mut P,
        outcome: Json,
    ) -> IvyResult<Json>;
    async fn existing_task<P: LifecyclePublication>(
        &self,
        operation: &Document,
        steps: &mut P,
        outcome: Json,
        normalized_category: Option<&JsString>,
    ) -> IvyResult<Json>;
}
#[allow(async_fn_in_trait)]
pub trait ExistingTaskPublication {
    async fn publish<P: LifecyclePublication>(
        &self,
        operation: &Document,
        steps: &mut P,
        outcome: Json,
        normalized_category: Option<&JsString>,
    ) -> IvyResult<Json>;
}
/// Reuse the delivered native publishers with the same retained step sequence.
pub struct EngineRoutes<'a, NS, NE, NV, LS, LE, LO, A> {
    pub native: &'a NativePublisher<'a, NS, NE, NV>,
    pub lifecycle: &'a NativeLifecycle<'a, LS, LE, LO>,
    pub actions: &'a A,
}
impl<
    NS: NativePublicationStore,
    NE: NativePublicationEngine,
    NV: NativePublicationEvidence,
    LS: LifecycleStore,
    LE: LifecycleEngine,
    LO: LifecycleOperations,
    A: ExistingTaskPublication,
> PublicationRoutes<Transcript> for EngineRoutes<'_, NS, NE, NV, LS, LE, LO, A>
{
    async fn native_update<P: LifecyclePublication>(
        &self,
        operation: &Document,
        steps: &mut P,
        transcript: Option<&Transcript>,
    ) -> IvyResult<Json> {
        let verified = transcript.map(VerifiedPublicationTranscript::from_verified);
        self.native
            .publish(operation, steps, verified.as_ref())
            .await
    }
    async fn lifecycle<P: LifecyclePublication>(
        &self,
        operation: &Document,
        steps: &mut P,
        outcome: Json,
    ) -> IvyResult<Json> {
        self.lifecycle.publish(operation, steps, outcome).await
    }
    async fn existing_task<P: LifecyclePublication>(
        &self,
        operation: &Document,
        steps: &mut P,
        outcome: Json,
        normalized: Option<&JsString>,
    ) -> IvyResult<Json> {
        self.actions
            .publish(operation, steps, outcome, normalized)
            .await
    }
}
pub struct EnginePublisher<'a, S, C, P, H> {
    pub operations: &'a Operations<S, C>,
    pub catalogs: &'a CheckedNativeContracts,
    pub checks: &'a P,
    pub routes: &'a H,
    /// Observe categories, publish current user notices, and release reserved
    /// gates under the owner's accepted policy. Every Task write uses it.
    pub task_written: &'a TaskWritten<'a>,
}
/// Bind the shared observer separately for each publication. The underlying
/// step callback keeps its existing pin/value interface and receives the exact
/// original Operation through this call's borrowed scope, including on replay.
pub struct ObservedPublication<'a, S, C, P, H, W> {
    pub operations: &'a Operations<S, C>,
    pub catalogs: &'a CheckedNativeContracts,
    pub checks: &'a P,
    pub routes: &'a H,
    pub observer: &'a W,
}
impl<S: RootPublicationStore, C: Fn() -> String, P: RootPublicationChecks, H, W: TaskWriteSink>
    ObservedPublication<'_, S, C, P, H, W>
{
    pub async fn publish<T>(
        &self,
        operation: &Document,
        transcript: Option<&T>,
        normalized: Option<&JsString>,
    ) -> IvyResult<Json>
    where
        H: PublicationRoutes<T>,
    {
        let observer = self.observer;
        let written = |pin: Json,
                       task: Json|
         -> std::pin::Pin<
            Box<dyn std::future::Future<Output = IvyResult<()>> + '_>,
        > {
            Box::pin(async move { observer.written(operation, &pin, &task).await })
        };
        EnginePublisher {
            operations: self.operations,
            catalogs: self.catalogs,
            checks: self.checks,
            routes: self.routes,
            task_written: &written,
        }
        .publish(operation, transcript, normalized)
        .await
    }
}
impl<
    S: RootPublicationStore,
    C: Fn() -> String,
    P: RootPublicationChecks,
    H: PublicationRoutes<T>,
    W: TaskWriteSink,
    T,
> ExecutionPublication<T> for ObservedPublication<'_, S, C, P, H, W>
{
    async fn publish(
        &self,
        operation: &Document,
        transcript: Option<&T>,
        normalized: Option<&JsString>,
    ) -> IvyResult<Json> {
        ObservedPublication::publish(self, operation, transcript, normalized).await
    }
}
fn f<'a>(v: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(key).unwrap_or(&NULL)
}
fn s<'a>(v: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    v.required(key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn eq(v: &Json, key: &str, want: &str) -> bool {
    v.get(key) == Some(&Json::from(want))
}
fn truthy(v: &Json) -> bool {
    match v {
        Json::Null => false,
        Json::Bool(v) => *v,
        Json::Number(v) => *v != 0.0 && !v.is_nan(),
        Json::String(v) => !v.0.is_empty(),
        _ => true,
    }
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn spread(target: &mut Json, fields: &Json) -> IvyResult<()> {
    for (key, value) in fields.as_object().ok_or_else(IvyError::internal)? {
        target.insert(key.clone(), value.clone());
    }
    Ok(())
}
pub fn empty_outcome(operation_id: &JsString) -> Json {
    let mut outcome = Json::object([("operationId", Json::String(operation_id.clone()))]);
    for key in [
        "task",
        "plan",
        "run",
        "history",
        "result",
        "review",
        "attachment",
    ] {
        outcome.insert(key, Json::Null);
    }
    outcome
}
pub fn history(
    operation: &Document,
    task: &JsString,
    previous: Option<&Document>,
    kind: &str,
    detail: &JsString,
) -> Json {
    Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("taskId", Json::String(task.clone())),
        ("operationId", f(&operation.value, "operationId").clone()),
        ("actor", f(&operation.value, "actor").clone()),
        ("createdAt", f(&operation.value, "createdAt").clone()),
        ("kind", kind.into()),
        ("detail", Json::String(detail.clone())),
        (
            "previousTask",
            previous.map(|d| d.pin.clone()).unwrap_or(Json::Null),
        ),
        (
            "previousHistory",
            previous
                .map(|d| f(&d.value, "lastHistory").clone())
                .unwrap_or(Json::Null),
        ),
        ("fromTarget", Json::Null),
        ("toTarget", Json::Null),
        (
            "priorRun",
            previous
                .map(|d| f(&d.value, "lastRun").clone())
                .unwrap_or(Json::Null),
        ),
        ("result", Json::Null),
        ("review", Json::Null),
    ])
}
async fn parent<S: LifecycleStore>(store: &S, id: &JsString) -> IvyResult<()> {
    validate_parent(id.clone(), None, |id| async move {
        let document = store
            .lifecycle_read("task-board/task", &Json::String(id), true, false)
            .await?;
        Ok::<_, IvyError>(Parent {
            archived: truthy(f(&document.metadata, "effectivelyArchived")),
            agent_controlled: eq(f(&document.document.value, "fields"), "control", "agent"),
            terminal: ["done", "cancelled"]
                .iter()
                .any(|p| eq(&document.document.value, "workflowState", p)),
            parent: f(&document.document.value, "parentTaskId")
                .as_js_string()
                .filter(|s| !s.0.is_empty())
                .cloned(),
        })
    })
    .await
    .map_err(|e| match e {
        GraphError::Read(e) => e,
        e => {
            let (code, message) = e.policy_error().unwrap();
            IvyError::new(code, message)
        }
    })
}
impl<S: RootPublicationStore, C: Fn() -> String, P: RootPublicationChecks, H>
    EnginePublisher<'_, S, C, P, H>
{
    async fn root<Q: LifecyclePublication>(
        &self,
        operation: &Document,
        steps: &mut Q,
        mut outcome: Json,
        normalized: Option<&JsString>,
    ) -> IvyResult<Json> {
        let request = operation.value.required("request")?;
        let actor = operation.value.required("actor")?;
        let at = s(&operation.value, "createdAt")?;
        let first = operation
            .value
            .required("writes")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .is_empty();
        if eq(request, "action", "configure") {
            if first {
                let current = configuration_view(&self.operations.store).await?;
                require(
                    canonical(f(&current, "object"), MANAGEMENT_FRAME_BYTES)?
                        == canonical(f(request, "configuration"), MANAGEMENT_FRAME_BYTES)?,
                    "revision_conflict",
                    "Reload the current TaskBoard settings before saving.",
                )?;
                self.checks.configuration(f(request, "value")).await?;
            }
            let pin = f(request, "configuration");
            let destination = if truthy(pin) {
                Json::object([
                    ("objectId", f(pin, "objectId").clone()),
                    ("expectedRevision", f(pin, "revision").clone()),
                ])
            } else {
                Json::object([(
                    "create",
                    Json::object([
                        (
                            "parentId",
                            self.operations
                                .store
                                .root_object_id()
                                .map(Json::String)
                                .unwrap_or(Json::Null),
                        ),
                        ("name", CONFIGURATION_NAME.into()),
                    ]),
                )])
            };
            outcome.insert(
                "configuration",
                steps
                    .write(
                        "task-board/configuration",
                        f(request, "value"),
                        &destination,
                    )
                    .await?,
            );
            return Ok(outcome);
        }
        if eq(request, "action", "savePlan") {
            let plan = resolved_plan(&self.operations.store, self.catalogs, f(request, "plan"))?;
            let mut retained = request.clone();
            retained.remove("draftHash");
            retained.insert("plan", plan.clone());
            require(
                f(request, "draftHash") == &Json::from(hash_json(&retained)?),
                "task_board_native_intent_mismatch",
                "Saved parameter revisions must reconstruct the exact original plan draft.",
            )?;
            plan_semantics(&plan)?;
            outcome.insert(
                "plan",
                steps
                    .create(
                        "task-board/native-plan",
                        f(request, "plan"),
                        &"Native plan".into(),
                    )
                    .await?,
            );
            return Ok(outcome);
        }
        let fields = request.required("fields")?;
        let mut submitted = fields.clone();
        let contact = if !f(fields, "userContact").is_null() {
            f(fields, "userContact").clone()
        } else if truthy(f(request, "parentTaskId")) {
            "ticket".into()
        } else {
            let current = configuration_view(&self.operations.store).await?;
            let value = f(f(f(&current, "configuration"), "defaults"), "userContact");
            if value.is_null() {
                "ticket".into()
            } else {
                value.clone()
            }
        };
        submitted.insert("userContact", contact);
        let parallel = if !f(fields, "allowParallel").is_null() {
            f(fields, "allowParallel").clone()
        } else {
            let current = configuration_view(&self.operations.store).await?;
            let value = f(f(f(&current, "configuration"), "defaults"), "allowParallel");
            if value.is_null() {
                false.into()
            } else {
                value.clone()
            }
        };
        submitted.insert("allowParallel", parallel);
        if first && !f(&submitted, "category").is_null() {
            require(
                normalized.is_some(),
                "task_board_category_projection_pending",
                "A categorized Task needs a current canonical category before publication.",
            )?;
        }
        let initial = if first {
            if truthy(f(request, "parentTaskId")) {
                parent(&self.operations.store, s(request, "parentTaskId")?).await?;
            }
            self.checks.fields(&submitted).await?;
            let state = self
                .checks
                .initial_state(request, &submitted, actor, at)
                .await?;
            let unavailable = ["host", "capability"]
                .iter()
                .any(|r| eq(f(&state, "waiting"), "reason", r));
            self.checks
                .execution_options(&submitted, unavailable)
                .await?;
            let mut initial = Json::object([
                ("schemaVersion", Json::Number(1.0)),
                (
                    "taskKey",
                    Json::String(
                        self.operations
                            .store
                            .allocate_task_key(s(&operation.pin, "objectId")?)
                            .await?,
                    ),
                ),
            ]);
            if truthy(f(request, "parentTaskId")) {
                initial.insert("parentTaskId", f(request, "parentTaskId").clone());
            }
            let mut published_fields = submitted;
            published_fields.insert(
                "category",
                if f(&published_fields, "category").is_null() {
                    Json::Null
                } else {
                    Json::String(normalized.ok_or_else(IvyError::internal)?.clone())
                },
            );
            initial.insert("fields", published_fields);
            spread(&mut initial, &state)?;
            initial.insert("publication", steps.pin()?);
            for key in [
                "primaryResourceRef",
                "claim",
                "lastRun",
                "lastHistory",
                "latestResult",
                "acceptedReview",
            ] {
                initial.insert(key, Json::Null);
            }
            for key in ["attemptCount", "agentCommentCount", "workRevision"] {
                initial.insert(key, Json::Number(0.0));
            }
            for key in ["comments", "commentDeliveries", "attachments"] {
                initial.insert(key, Json::Array(vec![]));
            }
            initial.insert("createdAt", Json::String(at.clone()));
            initial.insert("updatedAt", Json::String(at.clone()));
            initial
        } else {
            let write = &operation
                .value
                .required("writes")?
                .as_array()
                .ok_or_else(IvyError::internal)?[0];
            self.operations
                .store
                .read("task-board/task", f(f(write, "payload"), "object"), false)
                .await?
                .value
        };
        let pin = steps
            .write(
                "task-board/task",
                &initial,
                &Json::object([(
                    "create",
                    Json::object([
                        (
                            "parentId",
                            self.operations
                                .store
                                .root_object_id()
                                .map(Json::String)
                                .unwrap_or(Json::Null),
                        ),
                        ("name", f(&initial, "taskKey").clone()),
                    ]),
                )]),
            )
            .await?;
        if truthy(f(request, "initialResult")) {
            outcome.insert(
                "result",
                steps
                    .create(
                        "task-board/result",
                        &Json::object([
                            ("schemaVersion", Json::Number(1.0)),
                            ("taskId", f(&pin, "objectId").clone()),
                            ("run", Json::Null),
                            ("kind", "final".into()),
                            ("content", f(request, "initialResult").clone()),
                            ("actor", actor.clone()),
                            ("createdAt", Json::String(at.clone())),
                            ("operationId", f(request, "operationId").clone()),
                        ]),
                        &"Initial manual result".into(),
                    )
                    .await?,
            );
        }
        outcome.insert(
            "history",
            steps
                .create(
                    "task-board/history",
                    &history(
                        operation,
                        s(&pin, "objectId")?,
                        None,
                        "created",
                        s(fields, "title")?,
                    ),
                    &"History".into(),
                )
                .await?,
        );
        let mut final_task = initial;
        final_task.insert("latestResult", f(&outcome, "result").clone());
        final_task.insert("lastHistory", f(&outcome, "history").clone());
        final_task.insert("publication", Json::Null);
        outcome.insert(
            "task",
            steps
                .write(
                    "task-board/task",
                    &final_task,
                    &Json::object([
                        ("objectId", f(&pin, "objectId").clone()),
                        ("expectedRevision", f(&pin, "revision").clone()),
                    ]),
                )
                .await?,
        );
        Ok(outcome)
    }
    pub async fn publish<T>(
        &self,
        operation: &Document,
        transcript: Option<&T>,
        normalized: Option<&JsString>,
    ) -> IvyResult<Json>
    where
        H: PublicationRoutes<T>,
    {
        let request = operation.value.required("request")?;
        let written = |pin,
                       task|
         -> std::pin::Pin<
            Box<dyn std::future::Future<Output = IvyResult<()>> + '_>,
        > { Box::pin(async move { (self.task_written)(pin, task).await }) };
        let mut steps = Publication {
            operations: self.operations,
            operation,
            task_written: Some(&written),
            index: 0.0,
        };
        let outcome = empty_outcome(s(request, "operationId")?);
        if eq(request, "action", "nativeUpdate") {
            return self
                .routes
                .native_update(operation, &mut steps, transcript)
                .await;
        }
        if ["archive", "newThread", "recoverThread"]
            .iter()
            .any(|a| eq(request, "action", a))
        {
            return self.routes.lifecycle(operation, &mut steps, outcome).await;
        }
        if ["configure", "savePlan", "create"]
            .iter()
            .any(|a| eq(request, "action", a))
        {
            return self.root(operation, &mut steps, outcome, normalized).await;
        }
        self.routes
            .existing_task(operation, &mut steps, outcome, normalized)
            .await
    }
}
impl<
    S: RootPublicationStore,
    C: Fn() -> String,
    P: RootPublicationChecks,
    H: PublicationRoutes<T>,
    T,
> ExecutionPublication<T> for EnginePublisher<'_, S, C, P, H>
{
    async fn publish(
        &self,
        operation: &Document,
        transcript: Option<&T>,
        normalized: Option<&JsString>,
    ) -> IvyResult<Json> {
        EnginePublisher::publish(self, operation, transcript, normalized).await
    }
}
