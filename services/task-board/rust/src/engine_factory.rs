//! The borrowed production engine graph for one authenticated SDK generation.
//! Current contracts, query/locale rules and conversation policy are required;
//! retained execution and effects are assembled here on the journal owner.
use crate::{
    allocation::Allocator,
    category_projection::{CategoryLocale, CategoryProjection},
    checks::{CheckStore, TaskChecks, current_initial_state},
    deliveries::{Deliveries, DeliveryEngine, DeliveryPolicies, DeliveryRuntime},
    engine_actions::TaskActions,
    engine_admission::{AdmissionRuntime, EngineAdmission, EngineOwner},
    engine_checks::{CheckPolicy, EngineChecks},
    engine_execution::{
        CachedExecution, ExecutionComponents, ExecutionQueue, NativeExecutionProofs,
        ReadScopeExecutor,
    },
    engine_invocations::{EngineInvocations, InternalCalls},
    engine_publication::{CreationChecks, EngineRoutes, ObservedPublication, RootChecks},
    engine_read::{
        EngineReads, ReadRuntime, comment_reads, condition_policy,
        conversation::ConversationPolicies, read_failure::ReadFailure,
    },
    engine_service::{EngineService, LocalReadFuture, ReadService},
    engine_validation::{ActionValidation, ActionValidationChecks},
    generation::{GenerationClock, GenerationServiceRuntime, ReconcilerRuntime},
    inspection_journal::InspectionJournal,
    journal_store::{
        Contracts, JournalStore, ReadScopes,
        persistent_reads::{
            comment_port::CommentPort,
            document_reads::DocumentReads,
            local_history::{HistoryCollections, LocalHistory},
            local_reads::SqliteReads,
            store::RpcObjectReads,
            task_comments::TaskComments,
        },
    },
    lifecycle_journal::LifecycleJournal,
    local_executor::LocalExecutor,
    local_journal::{LocalJournal, initialize},
    local_retention::HistoryLimits,
    native_adoption::NativeAdoption,
    native_comments::{NativeCommentJournal, NativeCommentStore, sync_native_comments},
    native_driver::{DriverRuntime, NativeDriver},
    native_inspector::NativeInspector,
    native_intent::TaskPrompts,
    native_lifecycle::{LifecyclePolicies, LifecycleRuntime, LifecycleStore, NativeLifecycle},
    native_orchestrator::{
        NativeOrchestrator, NativeServices, OrchestrationPolicies, OrchestrationRuntime,
    },
    native_progress::NativeProgress,
    native_publication::{
        NativePublicationPolicies, NativePublicationRuntime, NativePublisher, PublicationEvidence,
    },
    native_rate_limit::FailedRateLimit,
    native_recovery::ContextRecovery,
    native_signals::NativeSignals,
    native_turn_evidence::TurnEvidence,
    operations::{Document, Operations},
    publication_coordination::PublicationCoordination,
    read_evidence::ReadEvidenceStore,
    reconciler::{ReconcilePolicies, Reconciler},
    scheduler::{Scheduler, SchedulerAllocation, SchedulerPolicies, SchedulerRuntime},
    scheduler_journal::SchedulerJournalBindings,
    service::{GenerationFactory, GenerationLaunch},
    task_routing::{QuestionRouting, RoutingRuntime, forward_delegated_review},
    task_written::{TaskNoticePolicy, TaskWriteObserver},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts, json::parse, limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::{
    service::{InvocationContext, ServiceConnection},
    task_comments::RpcCommentSource,
    tools::ToolClients,
};
use ivy_store::rusqlite::Connection;
use std::{cmp::Ordering, sync::Arc};

#[path = "factory_reads.rs"]
mod reads;
use reads::{Failure as ReadError, NativeReadFailure};

// Keep current creation on the same actual read/validation owners. The public
// EngineChecks and Publication facades retain their legacy policy selection.
struct CurrentCreation<'a, S, P, R> {
    checks: &'a EngineChecks<'a, S, P>,
    reads: &'a R,
}
impl<S: CheckStore, P: CheckPolicy, R: crate::wake::WakeStore<ReadError>> CreationChecks
    for CurrentCreation<'_, S, P, R>
{
    async fn fields(&self, fields: &Json) -> IvyResult<()> {
        self.checks.policy.fields(fields, None).await?;
        self.checks.core.current_fields(fields).await
    }
    async fn initial_state(
        &self,
        request: &Json,
        fields: &Json,
        actor: &Json,
        at: &JsString,
    ) -> IvyResult<Json> {
        self.checks
            .policy
            .creation(request, fields, actor, at)
            .await?;
        current_initial_state(
            self.reads,
            request,
            fields,
            actor,
            self.checks
                .core
                .current_content(request.get("initialResult").unwrap_or(&Json::Null)),
        )
        .await
        // This is the existing Ivy-only CreationChecks compatibility edge;
        // complete raw mutation caller propagation remains a separate binding.
        .map_err(|error| crate::journal_store::mutation_ports::publication_write_cause(&error))
    }
    async fn execution_options(&self, fields: &Json, unavailable: bool) -> IvyResult<()> {
        self.checks
            .core
            .execution_options(fields, None, unavailable)
            .await
    }
}

/// One initialized, owned connection for a generation. Construction keeps schema
/// initialization in one place for native files and supplied journal openers.
pub struct OpenedJournal {
    pub(crate) connection: Connection,
    pub(crate) limits: HistoryLimits,
}
impl OpenedJournal {
    pub fn new(connection: Connection, limits: HistoryLimits) -> IvyResult<Self> {
        initialize(&connection)?;
        Ok(Self { connection, limits })
    }
    pub fn connection(&self) -> &Connection {
        &self.connection
    }
    pub fn limits(&self) -> &HistoryLimits {
        &self.limits
    }
}

/// Decisions in addition to the concrete checks/effects below. There is no
/// frozen question/handoff/check/prompt, local paging or default locale policy.
#[allow(async_fn_in_trait)]
pub trait CurrentPolicies:
    CheckPolicy
    + TaskPrompts
    + NativePublicationPolicies
    + DeliveryPolicies
    + TaskNoticePolicy
    + CategoryLocale
    + SchedulerJournalBindings
{
    fn scheduler_enabled(&self) -> bool;
    fn scheduler_interval_ms(&self) -> f64;
    fn page_size(&self) -> f64;
}

pub struct PolicyContext<'a, V, C, I, K> {
    pub journal: &'a JournalStore<
        'a,
        crate::journal_client::JournalClient<ServiceConnection>,
        &'a V,
        &'a C,
        &'a I,
    >,
    pub settings: &'a Json,
    pub owner: &'a EngineOwner,
    pub catalogs: &'a CheckedNativeContracts,
    pub calls: InternalCalls<'a>,
    pub now: &'a C,
    pub clock: &'a K,
}
pub trait PolicyFactory<V, C, I, K> {
    type Policies<'a>: CurrentPolicies + 'a
    where
        Self: 'a,
        V: 'a,
        C: 'a,
        I: 'a,
        K: 'a;
    fn bind<'a>(&'a self, context: PolicyContext<'a, V, C, I, K>) -> IvyResult<Self::Policies<'a>>;
}

pub struct EngineFactory<'a, V, B, J, C, I, K> {
    pub maintenance: Option<&'a crate::maintenance::MaintenanceContext>,
    pub contracts: &'a V,
    pub catalogs: &'a CheckedNativeContracts,
    pub policies: &'a B,
    pub open_journal: J,
    pub now: C,
    pub new_id: I,
    pub clock: K,
    pub service_node_id: JsString,
}

// Runtime adapters own their policy parameter. Borrow the same current policy
// state instead of cloning it or constructing service-lifetime policy caches.
struct PolicyRef<'a, P>(&'a P);
struct PublicationPolicies<'a, S, P>(&'a EngineChecks<'a, S, P>);
impl<S: CheckStore, P: NativePublicationPolicies + CheckPolicy> NativePublicationPolicies
    for PublicationPolicies<'_, S, P>
{
    fn now(&self) -> JsString {
        NativePublicationPolicies::now(self.0.policy)
    }
    async fn aggregate(&self, t: &Json, id: &JsString) -> IvyResult<()> {
        self.0.aggregate(t, id).await
    }
}
impl<P: DeliveryPolicies> DeliveryPolicies for PolicyRef<'_, P> {
    fn now(&self) -> JsString {
        DeliveryPolicies::now(self.0)
    }
    fn phone_target(&self) -> &Json {
        self.0.phone_target()
    }
    fn chat_readable_version(&self, v: &JsString) -> bool {
        self.0.chat_readable_version(v)
    }
    fn handoff(&self, t: &Json, c: &Json) -> IvyResult<JsString> {
        DeliveryPolicies::handoff(self.0, t, c)
    }
    fn voice_prompt(&self, t: &Json, c: &Json) -> IvyResult<JsString> {
        self.0.voice_prompt(t, c)
    }
}

struct SchedulerBindings<'a, P, S, M, D, C, K> {
    local_journal: &'a LocalJournal<'a>,
    principal_id: &'a JsString,
    current: &'a P,
    checks: &'a EngineChecks<'a, S, P>,
    comments: &'a M,
    deliveries: Deliveries<'a, S, D>,
    calls: InternalCalls<'a>,
    now: &'a C,
    clock: &'a K,
    task_read:
        &'a dyn Fn(JsString) -> futures_util::future::LocalBoxFuture<'a, IvyResult<Document>>,
    routing: RoutingRuntime<'a, S, P>,
    questions: QuestionRouting,
}
impl<P: TaskPrompts, S, M, D, C, K> TaskPrompts for SchedulerBindings<'_, P, S, M, D, C, K> {
    fn task_prompt(&self, t: &Json, o: &JsString, c: Option<&Json>) -> IvyResult<JsString> {
        self.current.task_prompt(t, o, c)
    }
    fn task_update_prompt(&self, t: &Json, o: &JsString, c: bool) -> IvyResult<JsString> {
        self.current.task_update_prompt(t, o, c)
    }
}
impl<
    P: CurrentPolicies,
    S: CheckStore + ReadEvidenceStore,
    M: NativeCommentStore,
    D,
    C: Fn() -> String,
    K,
> SchedulerPolicies for SchedulerBindings<'_, P, S, M, D, C, K>
{
    async fn read_task(&self, id: &JsString) -> IvyResult<Document> {
        (self.task_read)(id.clone()).await
    }
    fn enabled(&self) -> bool {
        self.current.scheduler_enabled()
    }
    fn interval_ms(&self) -> f64 {
        self.current.scheduler_interval_ms()
    }
    fn now(&self) -> JsString {
        (self.now)().into()
    }
    fn compare_default(&self, a: &JsString, b: &JsString) -> Ordering {
        CategoryLocale::compare_default(self.current, a, b)
    }
    async fn aggregate(&self, t: &Json, id: &JsString) -> IvyResult<()> {
        self.checks.aggregate(t, id).await
    }
    async fn forward_delegated_review(&self, t: &Document) -> IvyResult<()> {
        forward_delegated_review(&self.routing, t).await
    }
    async fn unstarted_missing_context(&self, t: &Document) -> IvyResult<bool> {
        ContextRecovery::current(
            self.checks.core.store,
            self.checks.core.catalogs,
            self.principal_id.clone(),
        )
        .unstarted_missing_context(t)
        .await
    }
    async fn unstarted_archived_context(&self, t: &Document) -> IvyResult<bool> {
        ContextRecovery::current(
            self.checks.core.store,
            self.checks.core.catalogs,
            self.principal_id.clone(),
        )
        .unstarted_archived_context(t)
        .await
    }
    async fn failed_rate_limit_retry_at(&self, t: &Document) -> IvyResult<Option<JsString>> {
        FailedRateLimit {
            store: self.checks.core.store,
            inspector: &NativeInspector {
                store: self.comments,
                catalogs: self.checks.core.catalogs,
            },
        }
        .retry_at(t)
        .await
    }
    async fn schedule(&self, r: &Json) -> IvyResult<Json> {
        self.calls.schedule(r)?.await
    }
    async fn recover_operation(&self, id: &JsString) -> IvyResult<Json> {
        self.calls.recover_operation(id)?.await
    }
}
impl<
    P: CurrentPolicies,
    S: CheckStore + ReadEvidenceStore,
    M: NativeCommentStore,
    D: DeliveryEngine,
    C: Fn() -> String,
    K: GenerationClock,
> ReconcilePolicies for SchedulerBindings<'_, P, S, M, D, C, K>
{
    async fn route_task_questions(&self, id: &JsString) -> IvyResult<()> {
        self.questions.route(&self.routing, id).await
    }
    fn page_size(&self) -> f64 {
        self.current.page_size()
    }
    fn now_millis(&self) -> f64 {
        self.clock.now_millis()
    }
    async fn sync_native_comments(&self, id: &JsString) -> IvyResult<()> {
        sync_native_comments(self.comments, id).await
    }
    async fn delivery(&self, id: &JsString) -> IvyResult<()> {
        self.deliveries.step(id).await
    }
}
impl<P: CurrentPolicies, S, M: NativeCommentStore, D, C, K> OrchestrationPolicies
    for SchedulerBindings<'_, P, S, M, D, C, K>
{
    async fn recover_pending_native_operation(
        &self,
        task: &JsString,
        run: &JsString,
    ) -> IvyResult<bool> {
        let Some(operation) = self
            .local_journal
            .pending_operation(task, "native", Some(run))?
        else {
            return Ok(false);
        };
        self.calls.recover_operation(&operation)?.await?;
        Ok(true)
    }
    async fn sync_native_comments(&self, id: &JsString) -> IvyResult<()> {
        sync_native_comments(self.comments, id).await
    }
}
struct LifecycleBindings<'a, P, S, A> {
    principal_id: &'a JsString,
    current: &'a P,
    checks: &'a EngineChecks<'a, S, P>,
    allocator: &'a A,
}
impl<P: CurrentPolicies, S: CheckStore + ReadEvidenceStore, A: SchedulerAllocation>
    LifecyclePolicies for LifecycleBindings<'_, P, S, A>
{
    fn scheduler_enabled(&self) -> bool {
        self.current.scheduler_enabled()
    }
    async fn aggregate(&self, t: &Json, id: &JsString) -> IvyResult<()> {
        self.checks.aggregate(t, id).await
    }
    async fn allocate(&self, t: &Document, id: &JsString) -> IvyResult<Json> {
        self.allocator.allocate(t, id, None, false).await
    }
    async fn unstarted_missing_context(&self, t: &Document) -> IvyResult<bool> {
        ContextRecovery::current(
            self.checks.core.store,
            self.checks.core.catalogs,
            self.principal_id.clone(),
        )
        .unstarted_missing_context(t)
        .await
    }
}

impl<
    V: Contracts,
    B: PolicyFactory<V, C, I, K>,
    J: Fn(&Json) -> IvyResult<OpenedJournal>,
    C: Fn() -> String,
    I: Fn() -> JsString,
    K: GenerationClock,
> GenerationFactory for EngineFactory<'_, V, B, J, C, I, K>
{
    async fn run(&self, launch: GenerationLaunch) -> IvyResult<()> {
        let GenerationLaunch {
            connection,
            control,
            generation,
            activation,
        } = launch;
        let settings = activation.settings();
        self.contracts.validate_definition("Settings", settings)?;
        let principal = settings
            .required("principalId")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        let root = match settings.required("rootObjectId")? {
            Json::Null => None,
            Json::String(id) => Some(id.clone()),
            _ => return Err(IvyError::internal()),
        };
        let opened = (self.open_journal)(settings)?;
        let scopes = ReadScopes::default();
        let journal = JournalStore {
            journal: LocalJournal {
                connection: &opened.connection,
                root_object_id: root,
            },
            client: Arc::new(crate::journal_client::JournalClient::new(
                connection.clone(),
            )?),
            contracts: self.contracts,
            limits: opened.limits,
            now: &self.now,
            new_id: &self.new_id,
            cache: Some(scopes.cache()),
        };
        let maintenance = self
            .maintenance
            .map(|context| crate::maintenance::Maintenance::new(&journal, context.clone()));
        let owner = EngineOwner {
            service_node_id: self.service_node_id.clone(),
            generation: connection.generation() as f64,
        };
        let tools = ToolClients::new(journal.client.clone());
        let now_js = || JsString::from((self.now)());
        let read_local = SqliteReads {
            journal: LocalJournal {
                connection: &opened.connection,
                root_object_id: journal.journal.root_object_id.clone(),
            },
            contracts: self.contracts,
            parse: |text: &str| {
                parse(text, MANAGEMENT_FRAME_BYTES)
                    .map_err(|error| ReadFailure::source(NativeReadFailure::Parse(error)))
            },
            sqlite_failure: |error| ReadFailure::source(NativeReadFailure::Sqlite(error)),
        };
        let objects = RpcObjectReads {
            client: journal.client.as_ref(),
        };
        let history_collections = HistoryCollections::default();
        let current_comments = TaskComments::new(
            Arc::new(RpcCommentSource::new(journal.client.clone())),
            tokio::runtime::Handle::current(),
        );
        let read_condition = |task: &Json, wake: Option<&Json>| {
            condition_policy::task_condition(&read_local, task, wake)
        };
        let conversation = ConversationPolicies::with_current_questions(&read_condition);
        let source_failure = ReadError::from;
        let graph_comments = CommentPort {
            comments: &current_comments,
            source_failure: &source_failure,
        };
        let current_reads = DocumentReads {
            local: &read_local,
            objects: &objects,
            comments: &graph_comments,
            cache: Some(scopes.cache()),
        };
        let history = LocalHistory::new(
            &read_local,
            &current_reads,
            &objects,
            &history_collections,
            HistoryLimits {
                count: journal.limits.count,
                bytes: journal.limits.bytes,
            },
        );
        let cleanup = || history.collect::<NativeReadFailure>(0, 0, None, None);
        // Declarations precede their borrowed job queues. InternalCalls carries
        // only the shared admission gate/channel/read context across the cycle.
        let policies;
        let operations;
        let categories;
        let checks;
        let current_creation;
        let root_checks;
        let inspection;
        let comments;
        let delivery_runtime;
        let scheduler_runtime;
        let action_task_read;
        let scheduler_task_read;
        let routing_task_read;
        let allocator;
        let lifecycle_runtime;
        let lifecycle;
        let validation;
        let actions;
        let native_runtime;
        let evidence;
        let native_publisher;
        let routes;
        let gates;
        let observer;
        let publication;
        let current_publication;
        let inspector;
        let proofs;
        let services;
        let program;
        let category_jobs = LocalExecutor::default();
        let category_scoped = ReadScopeExecutor {
            scopes: &scopes,
            executor: &category_jobs,
        };
        let execution_jobs = LocalExecutor::default();
        let execution_scoped = ReadScopeExecutor {
            scopes: &scopes,
            executor: &execution_jobs,
        };
        let execution;
        let handoff_admission;
        let admission_runtime;
        let admission;
        let driver;
        let orchestration;
        let native_services;
        let native;
        let adoption;
        let signals;
        let scheduler;
        let reconcile_components;
        let reconcile_jobs = LocalExecutor::default();
        let reconcile_scoped = ReadScopeExecutor {
            scopes: &scopes,
            executor: &reconcile_jobs,
        };
        let reconciler;
        let runtime;
        let service_runtime;
        let read_runtime;
        let public;
        let owner_read;
        let current_public;
        let invocation_jobs = LocalExecutor::default();
        let invocation_scoped = ReadScopeExecutor {
            scopes: &scopes,
            executor: &invocation_jobs,
        };
        let invocations = EngineInvocations::new(&invocation_scoped);
        let (calls, internal_owner) = invocations.internal_calls(&scopes);
        policies = self.policies.bind(PolicyContext {
            journal: &journal,
            settings,
            owner: &owner,
            catalogs: self.catalogs,
            calls: calls.clone(),
            now: &self.now,
            clock: &self.clock,
        })?;
        operations = Operations {
            store: LifecycleJournal {
                journal: &journal,
                pages: &policies,
            },
            now: &self.now,
        };
        let store = &operations.store;
        categories = CategoryProjection::new(store, principal, &policies, &category_scoped);
        checks = EngineChecks {
            core: TaskChecks {
                store,
                catalogs: self.catalogs,
                tools: &tools,
            },
            policy: &policies,
        };
        current_creation = CurrentCreation {
            checks: &checks,
            reads: &current_reads,
        };
        root_checks = RootChecks {
            journal: &journal,
            creation: &current_creation,
        };
        inspection =
            InspectionJournal::new(&journal, principal.clone(), owner.service_node_id.clone());
        let comment_calls = calls.clone();
        comments = NativeCommentJournal {
            inspection: &inspection,
            native_comment: move |request: Json| {
                let observer = comment_calls.native_comment(&request);
                async move { observer?.await }
            },
        };
        delivery_runtime = DeliveryRuntime {
            client: journal.client.clone(),
            principal_id: principal.clone(),
            service_node_id: owner.service_node_id.clone(),
            policies: PolicyRef(&policies),
        };
        action_task_read = |address: Json,
                            primary: bool|
         -> futures_util::future::LocalBoxFuture<
            '_,
            IvyResult<crate::native_lifecycle::LifecycleDocument>,
        > {
            let current_reads = &current_reads;
            Box::pin(async move {
                reads::lifecycle_document(
                    current_reads
                        .read_document("task-board/task", &address, primary, false, false)
                        .await
                        .map_err(crate::engine_read::read_failure::provider_read_error)?,
                )
            })
        };
        scheduler_task_read =
            |id: JsString| -> futures_util::future::LocalBoxFuture<'_, IvyResult<Document>> {
                Box::pin(async { Ok(action_task_read(Json::String(id), true).await?.document) })
            };
        routing_task_read = |id: JsString,
                             archived: bool|
         -> futures_util::future::LocalBoxFuture<
            '_,
            IvyResult<crate::native_lifecycle::LifecycleDocument>,
        > {
            let current_reads = &current_reads;
            Box::pin(async move {
                reads::lifecycle_document(
                    current_reads
                        .read_document("task-board/task", &Json::String(id), true, archived, false)
                        .await
                        .map_err(crate::engine_read::read_failure::provider_read_error)?,
                )
            })
        };
        scheduler_runtime = SchedulerRuntime::new(
            journal.client.clone(),
            principal.clone(),
            owner.service_node_id.clone(),
            SchedulerBindings {
                local_journal: &journal.journal,
                principal_id: principal,
                current: &policies,
                checks: &checks,
                comments: &comments,
                deliveries: Deliveries {
                    store,
                    engine: &delivery_runtime,
                },
                calls: calls.clone(),
                now: &self.now,
                clock: &self.clock,
                task_read: &scheduler_task_read,
                routing: RoutingRuntime {
                    read: &routing_task_read,
                    checks: &checks,
                    client: journal.client.clone(),
                    calls: calls.clone(),
                },
                questions: QuestionRouting::default(),
            },
        );
        allocator = Allocator {
            store,
            engine: &scheduler_runtime,
            catalogs: self.catalogs,
        };
        lifecycle_runtime = LifecycleRuntime::new(
            journal.client.clone(),
            principal.clone(),
            owner.service_node_id.clone(),
            LifecycleBindings {
                principal_id: principal,
                current: &policies,
                checks: &checks,
                allocator: &allocator,
            },
        );
        lifecycle = NativeLifecycle {
            store,
            engine: &lifecycle_runtime,
            operations: &operations,
            catalogs: self.catalogs,
        };
        validation = ActionValidation {
            store,
            checks: &checks,
            task_read: &action_task_read,
        };
        actions = TaskActions {
            store,
            settings,
            catalogs: self.catalogs,
            policies: &validation,
            lifecycle: &lifecycle,
            task_read: &action_task_read,
        };
        native_runtime = NativePublicationRuntime::new(
            journal.client.clone(),
            principal.clone(),
            settings.get("phoneTarget").cloned().unwrap_or(Json::Null),
            PublicationPolicies(&checks),
        );
        evidence = PublicationEvidence {
            store: &journal,
            catalogs: self.catalogs,
            principal_id: principal,
            prompts: &policies,
        };
        native_publisher = NativePublisher {
            store,
            engine: &native_runtime,
            evidence: &evidence,
        };
        routes = EngineRoutes {
            native: &native_publisher,
            lifecycle: &lifecycle,
            actions: &actions,
        };
        gates = PublicationCoordination {
            store,
            principal_id: principal,
            now: &self.now,
        };
        observer = TaskWriteObserver {
            projection: &categories,
            gates: &gates,
            notices: &policies,
            client: journal.client.clone(),
            service_node_id: &owner.service_node_id,
        };
        publication = ObservedPublication {
            operations: &operations,
            catalogs: self.catalogs,
            checks: &root_checks,
            routes: &routes,
            observer: &observer,
        };
        current_publication = publication.current();
        inspector = NativeInspector {
            store: &inspection,
            catalogs: self.catalogs,
        };
        proofs = NativeExecutionProofs {
            inspector: &inspector,
            turns: TurnEvidence {
                store: &journal,
                catalogs: self.catalogs,
                principal_id: principal,
            },
        };
        services = ExecutionComponents {
            categories: &categories,
            proofs: &proofs,
            publication: &current_publication,
            read_scope: &scopes,
        };
        program = CachedExecution {
            operations: &operations,
            principal_id: principal,
            services: &services,
        };
        execution = ExecutionQueue::new(&program, &execution_scoped);
        handoff_admission = |request: Json,
                             actor: Json|
         -> futures_util::future::LocalBoxFuture<'_, IvyResult<Json>> {
            let validation = &validation;
            let task_read = &action_task_read;
            Box::pin(async move {
                let task = task_read(
                    Json::object([
                        ("objectId", request.required("taskId")?.clone()),
                        ("revision", request.required("expectedRevision")?.clone()),
                    ]),
                    true,
                )
                .await?;
                let now = (self.now)();
                validation
                    .validate(
                        &request,
                        &task.document.value,
                        &actor,
                        &now.into(),
                        request
                            .required("operationId")?
                            .as_js_string()
                            .ok_or_else(IvyError::internal)?,
                    )
                    .await?;
                Ok(Json::object([
                    (
                        "run",
                        task.document
                            .value
                            .required("claim")?
                            .required("run")?
                            .clone(),
                    ),
                    (
                        "work",
                        crate::engine_actions::handoff_work(&task.document.value)?,
                    ),
                ]))
            })
        };
        admission_runtime = AdmissionRuntime {
            client: journal.client.clone(),
            contracts: self.contracts,
            catalogs: self.catalogs,
            handoff: &handoff_admission,
        };
        admission = EngineAdmission::new(
            &operations,
            settings,
            &owner,
            &admission_runtime,
            &execution,
        )?;
        let update_calls = calls.clone();
        driver = DriverRuntime::new(
            journal.client.clone(),
            principal.clone(),
            owner.service_node_id.clone(),
            move |request: Json| {
                let observer = update_calls.native_update(&request);
                async move { observer?.await }
            },
            &now_js,
        );
        orchestration = OrchestrationRuntime {
            driver: &driver,
            store,
            policies: &scheduler_runtime.policies,
            read_scopes: &scopes,
            now_millis: || self.clock.now_millis(),
        };
        native_services = NativeServices {
            driver: NativeDriver {
                store: &journal,
                runtime: &orchestration,
                prompts: &policies,
                catalogs: self.catalogs,
            },
            inspector: NativeInspector {
                store: &inspection,
                catalogs: self.catalogs,
            },
            evidence: TurnEvidence {
                store: &journal,
                catalogs: self.catalogs,
                principal_id: principal,
            },
        };
        let warning_ms = match settings.required("scheduler")?.get("stallWarningMs") {
            None | Some(Json::Null) => 300_000.0,
            Some(value) => value.as_f64().ok_or_else(IvyError::internal)?,
        };
        native = NativeOrchestrator::new(
            store,
            &orchestration,
            &native_services,
            self.catalogs,
            NativeProgress::new(owner.service_node_id.clone(), warning_ms),
        );
        adoption = NativeAdoption::new(store, &driver, journal.client.as_ref(), || {
            self.clock.now_millis()
        });
        signals = NativeSignals::new(NativeInspector {
            store: &inspection,
            catalogs: self.catalogs,
        });
        scheduler = Scheduler {
            store,
            engine: &scheduler_runtime,
            allocator: &allocator,
            operations: &operations,
        };
        reconcile_components = crate::reconciler::ReconcileComponents {
            runtime: &scheduler_runtime,
            native: &native,
            scheduler: &scheduler,
            signals: &signals,
            adoption: &adoption,
        };
        reconciler =
            Reconciler::new_with_cleanup(store, &reconcile_components, &reconcile_scoped, cleanup);
        runtime = ReconcilerRuntime {
            reconciler: &reconciler,
            native: &native,
            maintenance: maintenance
                .as_ref()
                .map(|value| value as &dyn crate::maintenance::MaintenanceTick),
        };
        service_runtime = GenerationServiceRuntime {
            runtime: &runtime,
            control: &control,
            now: &now_js,
        };
        read_runtime = ReadRuntime {
            client: journal.client.as_ref(),
            contracts: self.contracts,
            principal_id: principal,
            service_node_id: &owner.service_node_id,
            generation: owner.generation,
        };
        public = EngineService {
            admission: &admission,
            reads: EngineReads {
                operations: &operations,
                runtime: &read_runtime,
            },
            runtime: &service_runtime,
            categories: &categories,
        };
        let read_dependencies = (&admission, &current_reads, &conversation, store);
        owner_read = move |query: Json,
                           context: InvocationContext|
              -> LocalReadFuture<'_, NativeReadFailure> {
            let (admission, current_reads, conversation, store) = read_dependencies;
            Box::pin(async move {
                if context.generation as f64 != admission.owner().generation {
                    return Err(IvyError::new(
                        "service_not_ready",
                        "TaskBoard has no reconciled current connection generation.",
                    )
                    .into());
                }
                admission.verify_owner().await?;
                // taskDocument selects a retained header. The current read core
                // alone decides indexed windows versus full materialization.
                let address = query.get("task");
                let address = if let Some(Json::String(_)) = address {
                    address.unwrap().clone()
                } else {
                    let key = match address {
                        None | Some(Json::Null) => {
                            return Err(IvyError::new(
                                "invalid_arguments",
                                "A task ID or task key is required.",
                            )
                            .into());
                        }
                        Some(value) => value.get("taskKey"),
                    };
                    let mut where_clause =
                        Json::object([("op", "eq".into()), ("field", "data:/taskKey".into())]);
                    if let Some(key) = key {
                        where_clause.insert("value", key.clone());
                    }
                    let page = store
                        .page(
                            "task-board/task",
                            &Json::object([("where", where_clause), ("limit", Json::Number(2.0))]),
                        )
                        .await?;
                    let items = page
                        .required("items")?
                        .as_array()
                        .ok_or_else(IvyError::internal)?;
                    if items.len() != 1 || page.get("nextCursor").is_some_and(|v| !v.is_null()) {
                        return Err(IvyError::new(
                            "not_found",
                            "Task key was not found in this workspace.",
                        )
                        .into());
                    }
                    items[0].required("objectId")?.clone()
                };
                let header = current_reads
                    .read_document("task-board/task", &address, true, true, true)
                    .await?;
                comment_reads::read_task(current_reads, conversation, &header, &query).await
            })
        };
        current_public = ReadService {
            read: &owner_read,
            other: &public,
        };
        let result = invocation_jobs
            .drive(
                reconcile_jobs.drive(execution_jobs.drive(category_jobs.drive(
                    activation.run_engine_with_handler(
                        &control,
                        generation,
                        &current_public,
                        &admission,
                        &invocations,
                        internal_owner,
                        &runtime,
                        &self.clock,
                        async {
                            execution.settled().await;
                            categories.settled().await;
                            Ok(())
                        },
                    ),
                ))),
            )
            .await;
        // All accepted read producers and their generation have retired. A
        // failed range can still own immutable sibling requests in the SDK.
        current_comments.settled().await;
        if let Some(maintenance) = &maintenance {
            maintenance.retention.settled().await;
        }
        result
    }
}
