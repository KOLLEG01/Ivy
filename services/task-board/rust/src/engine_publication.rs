//! Publication dispatch and root-level configure, plan and Task creation.
//! Current creation checks and Task-written policy are required owner bindings.
use crate::{
    allocation::plan_semantics,
    configuration::{check_configuration, configuration_view},
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
    IvyError, IvyJson as Json, IvyResult, JsString, checked_native_contract::CheckedNativeContracts,
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
#[derive(Clone, Copy)]
enum CreationPolicy {
    Legacy,
    Current,
}
pub(crate) struct CurrentPublication<'a, 'p, S, C, P, H, W>(
    &'a ObservedPublication<'p, S, C, P, H, W>,
);
impl<'p, S: RootPublicationStore, C: Fn() -> String, P: RootPublicationChecks, H, W: TaskWriteSink>
    ObservedPublication<'p, S, C, P, H, W>
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
        self.publish_with_creation(operation, transcript, normalized, CreationPolicy::Legacy)
            .await
    }
    pub(crate) fn current(&self) -> CurrentPublication<'_, 'p, S, C, P, H, W> {
        CurrentPublication(self)
    }
    async fn publish_with_creation<T>(
        &self,
        operation: &Document,
        transcript: Option<&T>,
        normalized: Option<&JsString>,
        creation: CreationPolicy,
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
        .publish_with_creation(operation, transcript, normalized, creation)
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
> ExecutionPublication<T> for CurrentPublication<'_, '_, S, C, P, H, W>
{
    async fn publish(
        &self,
        operation: &Document,
        transcript: Option<&T>,
        normalized: Option<&JsString>,
    ) -> IvyResult<Json> {
        self.0
            .publish_with_creation(operation, transcript, normalized, CreationPolicy::Current)
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

fn spread(target: &mut Json, fields: &Json) -> IvyResult<()> {
    for (key, value) in fields.as_object().ok_or_else(IvyError::internal)? {
        target.insert(key.clone(), value.clone());
    }
    Ok(())
}
fn require_same(left: &Json, right: &Json, code: &str, message: &str) -> IvyResult<()> {
    if ivy_contracts::hash::hash_json(left)? == ivy_contracts::hash::hash_json(right)? {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
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
            archived: f(&document.metadata, "effectivelyArchived").as_bool() == Some(true),
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
        creation: CreationPolicy,
    ) -> IvyResult<Json> {
        let request = operation.value.required("request")?;
        let actor = operation.value.required("actor")?;
        let at = operation.value.required("createdAt")?;
        let writes = operation
            .value
            .required("writes")?
            .as_array()
            .ok_or_else(IvyError::internal)?;
        let first = writes.is_empty();
        if eq(request, "action", "configure") {
            if first {
                let current = configuration_view(&self.operations.store).await?;
                require_same(
                    f(&current, "object"),
                    f(request, "configuration"),
                    "revision_conflict",
                    "Reload the current TaskBoard settings before saving.",
                )?;
                self.checks
                    .configuration(request.required("value")?)
                    .await?;
            }
            let destination = match request.get("configuration").filter(|p| !p.is_null()) {
                Some(pin) => Json::object([
                    ("objectId", pin.required("objectId")?.clone()),
                    ("expectedRevision", pin.required("revision")?.clone()),
                ]),
                None => self.create_destination(crate::configuration::CONFIGURATION_NAME.into()),
            };
            outcome.insert(
                "configuration",
                steps
                    .write(
                        "task-board/configuration",
                        request.required("value")?,
                        &destination,
                    )
                    .await?,
            );
            return Ok(outcome);
        }
        if eq(request, "action", "savePlan") {
            let plan = resolved_plan(
                &self.operations.store,
                self.catalogs,
                request.required("plan")?,
            )?;
            let mut retained = Json::object(
                request
                    .as_object()
                    .ok_or_else(IvyError::internal)?
                    .iter()
                    .filter(|(key, _)| key != &JsString::from("draftHash"))
                    .cloned(),
            );
            retained.insert("plan", plan.clone());
            if f(request, "draftHash") != &Json::from(ivy_contracts::hash::hash_json(&retained)?) {
                return Err(IvyError::new(
                    "task_board_native_intent_mismatch",
                    "Saved parameter revisions must reconstruct the exact original plan draft.",
                ));
            }
            plan_semantics(&plan)?;
            outcome.insert(
                "plan",
                steps
                    .create(
                        "task-board/native-plan",
                        request.required("plan")?,
                        &"Native plan".into(),
                    )
                    .await?,
            );
            return Ok(outcome);
        }
        let fields = request.required("fields")?;
        let mut submitted = fields.clone();
        if f(fields, "userContact").is_null() {
            let contact = if request.get("parentTaskId").is_some_and(|p| !p.is_null()) {
                "ticket".into()
            } else {
                let configuration = configuration_view(&self.operations.store).await?;
                f(
                    f(f(&configuration, "configuration"), "defaults"),
                    "userContact",
                )
                .as_js_string()
                .cloned()
                .unwrap_or_else(|| "ticket".into())
            };
            submitted.insert("userContact", Json::String(contact));
        }
        if f(fields, "allowParallel").is_null() {
            let configuration = configuration_view(&self.operations.store).await?;
            let parallel = f(
                f(f(&configuration, "configuration"), "defaults"),
                "allowParallel",
            )
            .as_bool()
            .unwrap_or(false);
            submitted.insert("allowParallel", parallel.into());
        }
        if first && !f(&submitted, "category").is_null() && normalized.is_none() {
            return Err(IvyError::new(
                "task_board_category_projection_pending",
                "A categorized Task needs a current canonical category before publication.",
            ));
        }
        let initial = if first {
            if let Some(parent_id) = request.get("parentTaskId").and_then(Json::as_js_string) {
                parent(&self.operations.store, parent_id).await?;
            }
            self.checks.fields(&submitted).await?;
            let state = self
                .checks
                .initial_state(
                    request,
                    &submitted,
                    actor,
                    s(&operation.value, "createdAt")?,
                )
                .await?;
            let unavailable = match creation {
                CreationPolicy::Current => {
                    eq(&state, "workflowState", "backlog") || eq(&state, "workflowState", "waiting")
                }
                CreationPolicy::Legacy => {
                    eq(f(&state, "waiting"), "reason", "host")
                        || eq(f(&state, "waiting"), "reason", "capability")
                }
            };
            self.checks
                .execution_options(&submitted, unavailable)
                .await?;
            let task_key = self
                .operations
                .store
                .allocate_task_key(s(&operation.pin, "objectId")?)
                .await?;
            let mut initial = Json::object([
                ("schemaVersion", Json::Number(1.0)),
                ("taskKey", Json::String(task_key)),
            ]);
            if let Some(parent_id) = request.get("parentTaskId").filter(|p| !p.is_null()) {
                initial.insert("parentTaskId", parent_id.clone());
            }
            if !f(&submitted, "category").is_null() {
                submitted.insert(
                    "category",
                    normalized.cloned().map(Json::String).unwrap_or(Json::Null),
                );
            }
            initial.insert("fields", submitted);
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
            initial.insert("createdAt", at.clone());
            initial.insert("updatedAt", at.clone());
            initial
        } else {
            self.operations
                .store
                .read(
                    "task-board/task",
                    writes[0].required("payload")?.required("object")?,
                    false,
                )
                .await?
                .value
        };
        let pin = steps
            .write(
                "task-board/task",
                &initial,
                &self.create_destination(initial.required("taskKey")?.clone()),
            )
            .await?;
        if let Some(content) = request.get("initialResult").filter(|r| !r.is_null()) {
            outcome.insert(
                "result",
                steps
                    .create(
                        "task-board/result",
                        &Json::object([
                            ("schemaVersion", Json::Number(1.0)),
                            ("taskId", pin.required("objectId")?.clone()),
                            ("run", Json::Null),
                            ("kind", "final".into()),
                            ("content", content.clone()),
                            ("actor", actor.clone()),
                            ("createdAt", at.clone()),
                            ("operationId", request.required("operationId")?.clone()),
                        ]),
                        &"Initial manual result".into(),
                    )
                    .await?,
            );
        }
        let record = history(
            operation,
            s(&pin, "objectId")?,
            None,
            "created",
            s(fields, "title")?,
        );
        outcome.insert(
            "history",
            steps
                .create("task-board/history", &record, &"History".into())
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
                        ("objectId", pin.required("objectId")?.clone()),
                        ("expectedRevision", pin.required("revision")?.clone()),
                    ]),
                )
                .await?,
        );
        Ok(outcome)
    }

    fn create_destination(&self, name: Json) -> Json {
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
                ("name", name),
            ]),
        )])
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
        self.publish_with_creation(operation, transcript, normalized, CreationPolicy::Legacy)
            .await
    }
    async fn publish_with_creation<T>(
        &self,
        operation: &Document,
        transcript: Option<&T>,
        normalized: Option<&JsString>,
        creation: CreationPolicy,
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
        let steps = Publication {
            operations: self.operations,
            operation,
            task_written: Some(&written),
            index: 0.0,
        };
        s(request, "operationId")?;
        let mut steps = steps;
        let outcome = empty_outcome(s(request, "operationId")?);
        match s(request, "action")?.to_utf8()?.as_str() {
            "nativeUpdate" => {
                self.routes
                    .native_update(operation, &mut steps, transcript)
                    .await
            }
            "archive" | "newThread" | "recoverThread" => {
                self.routes.lifecycle(operation, &mut steps, outcome).await
            }
            "configure" | "savePlan" | "create" => {
                self.root(operation, &mut steps, outcome, normalized, creation)
                    .await
            }
            _ => {
                self.routes
                    .existing_task(operation, &mut steps, outcome, normalized)
                    .await
            }
        }
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
