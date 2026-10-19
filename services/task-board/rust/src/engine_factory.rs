//! The borrowed production engine graph for one authenticated SDK generation.
//! Current contracts, query/locale rules and conversation policy are required;
//! retained execution and effects are assembled here on the journal owner.
use crate::{
    allocation::Allocator,
    category_projection::{CategoryLocale, CategoryProjection},
    checks::{CheckStore, TaskChecks},
    deliveries::{Deliveries, DeliveryEngine, DeliveryPolicies, DeliveryRuntime},
    engine_actions::TaskActions,
    engine_admission::{AdmissionRuntime, EngineAdmission, EngineOwner},
    engine_checks::{CheckPolicy, EngineChecks},
    engine_execution::{
        CachedExecution, ExecutionComponents, ExecutionQueue, NativeExecutionProofs,
        ReadScopeExecutor,
    },
    engine_invocations::{EngineInvocations, InternalCalls},
    engine_publication::{EngineRoutes, ObservedPublication, RootChecks},
    engine_read::{EngineReads, ReadRuntime},
    engine_service::EngineService,
    engine_validation::{ActionValidation, ActionValidationChecks},
    generation::{GenerationClock, GenerationServiceRuntime, ReconcilerRuntime},
    inspection_journal::InspectionJournal,
    journal_store::{Contracts, JournalStore, ReadScopes},
    lifecycle_journal::LifecycleJournal,
    local_executor::LocalExecutor,
    local_journal::{LocalJournal, initialize},
    local_retention::HistoryLimits,
    native_comments::{NativeCommentJournal, NativeCommentStore, sync_native_comments},
    native_driver::{DriverRuntime, NativeDriver},
    native_inspector::NativeInspector,
    native_intent::TaskPrompts,
    native_lifecycle::{
        LifecyclePolicies, LifecyclePublication, LifecycleRuntime, NativeLifecycle,
    },
    native_orchestrator::{
        NativeOrchestrator, NativeServices, OrchestrationPolicies, OrchestrationRuntime,
    },
    native_publication::{
        NativePublicationPolicies, NativePublicationRuntime, NativePublisher, PublicationEvidence,
    },
    native_signals::NativeSignals,
    native_turn_evidence::TurnEvidence,
    operations::{Document, Operations},
    publication_coordination::PublicationCoordination,
    reconciler::{ReconcilePolicies, Reconciler},
    scheduler::{Scheduler, SchedulerAllocation, SchedulerPolicies, SchedulerRuntime},
    scheduler_journal::SchedulerJournalBindings,
    service::{GenerationFactory, GenerationLaunch},
    task_written::{TaskNoticePolicy, TaskWriteObserver},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, checked_native_contract::CheckedNativeContracts,
};
use ivy_sdk::{service::ServiceConnection, tools::ToolClients};
use ivy_store::rusqlite::Connection;
use std::{cmp::Ordering, sync::Arc};

/// The host opens a fresh owned connection for each generation. Configuration,
/// path selection, free-space observation and host controls are not inferred.
pub struct OpenedJournal {
    pub connection: Connection,
    pub limits: HistoryLimits,
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
    fn peer_comment(&self, comment: &Json) -> IvyResult<bool>;
    async fn forward_delegated_review(&self, task: &Document) -> IvyResult<()>;
    async fn unstarted_missing_context(&self, task: &Document) -> IvyResult<bool>;
    fn run_handoff(&self, task: &Json, run: &JsString) -> IvyResult<bool>;
    fn claimed_workflow_state(&self, waiting: &Json) -> IvyResult<JsString>;
}

pub struct PolicyContext<'a, V, C, I, K> {
    pub journal: &'a JournalStore<'a, ServiceConnection, &'a V, &'a C, &'a I>,
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
    fn handoff(&self, t: &Json, r: &JsString) -> IvyResult<Json> {
        NativePublicationPolicies::handoff(self.0.policy, t, r)
    }
    fn retry_at(&self, t: &Json, at: &JsString) -> IvyResult<Json> {
        self.0.policy.retry_at(t, at)
    }
    fn shared_project(&self, r: &Json, w: &Json) -> IvyResult<bool> {
        self.0.policy.shared_project(r, w)
    }
    async fn aggregate(&self, t: &Json, id: &JsString) -> IvyResult<()> {
        self.0.aggregate(t, id).await
    }
    async fn finish_active<W: LifecyclePublication>(&self, c: &Json, w: &mut W) -> IvyResult<Json> {
        self.0.policy.finish_active(c, w).await
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
    current: &'a P,
    checks: &'a EngineChecks<'a, S, P>,
    comments: &'a M,
    deliveries: Deliveries<'a, S, D>,
    calls: InternalCalls<'a>,
    now: &'a C,
    clock: &'a K,
}
impl<P: TaskPrompts, S, M, D, C, K> TaskPrompts for SchedulerBindings<'_, P, S, M, D, C, K> {
    fn task_prompt(&self, t: &Json, o: &JsString, c: Option<&Json>) -> IvyResult<JsString> {
        self.current.task_prompt(t, o, c)
    }
    fn task_update_prompt(&self, t: &Json, o: &JsString, c: bool) -> IvyResult<JsString> {
        self.current.task_update_prompt(t, o, c)
    }
}
impl<P: CurrentPolicies, S: CheckStore, M, D, C: Fn() -> String, K> SchedulerPolicies
    for SchedulerBindings<'_, P, S, M, D, C, K>
{
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
    fn coordination_pending(&self, t: &Json) -> IvyResult<bool> {
        CheckPolicy::coordination_pending(self.current, t)
    }
    fn peer_comment(&self, c: &Json) -> IvyResult<bool> {
        self.current.peer_comment(c)
    }
    async fn forward_delegated_review(&self, t: &Document) -> IvyResult<()> {
        self.current.forward_delegated_review(t).await
    }
    async fn unstarted_missing_context(&self, t: &Document) -> IvyResult<bool> {
        self.current.unstarted_missing_context(t).await
    }
    async fn unstarted_archived_context(&self, t: &Document) -> IvyResult<bool> {
        CheckPolicy::unstarted_archived_context(self.current, t).await
    }
    async fn failed_rate_limit_retry_at(&self, t: &Document) -> IvyResult<Option<JsString>> {
        CheckPolicy::failed_rate_limit_retry_at(self.current, t).await
    }
    async fn check_dependencies(&self, f: &Json, t: &JsString, done: bool) -> IvyResult<()> {
        self.checks.core.dependencies(f, Some(t), done).await
    }
    async fn check_blocker(&self, b: &Json) -> IvyResult<()> {
        self.checks.core.check_blocker(Some(b)).await
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
    S: CheckStore,
    M: NativeCommentStore,
    D: DeliveryEngine,
    C: Fn() -> String,
    K: GenerationClock,
> ReconcilePolicies for SchedulerBindings<'_, P, S, M, D, C, K>
{
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
    fn run_handoff(&self, t: &Json, r: &JsString) -> IvyResult<bool> {
        self.current.run_handoff(t, r)
    }
    fn claimed_workflow_state(&self, w: &Json) -> IvyResult<JsString> {
        self.current.claimed_workflow_state(w)
    }
    async fn sync_native_comments(&self, id: &JsString) -> IvyResult<()> {
        sync_native_comments(self.comments, id).await
    }
}
struct LifecycleBindings<'a, P, S, A> {
    current: &'a P,
    checks: &'a EngineChecks<'a, S, P>,
    allocator: &'a A,
}
impl<P: CurrentPolicies, S: CheckStore, A: SchedulerAllocation> LifecyclePolicies
    for LifecycleBindings<'_, P, S, A>
{
    fn scheduler_enabled(&self) -> bool {
        self.current.scheduler_enabled()
    }
    async fn aggregate(&self, t: &Json, id: &JsString) -> IvyResult<()> {
        self.checks.aggregate(t, id).await
    }
    async fn allocate(&self, t: &Document, id: &JsString) -> IvyResult<Json> {
        self.allocator.allocate(t, id, None).await
    }
    async fn unstarted_missing_context(&self, t: &Document) -> IvyResult<bool> {
        self.current.unstarted_missing_context(t).await
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
        initialize(&opened.connection)?;
        let scopes = ReadScopes::default();
        let journal = JournalStore {
            journal: LocalJournal {
                connection: &opened.connection,
                root_object_id: root,
            },
            client: Arc::new(connection.clone()),
            contracts: self.contracts,
            limits: opened.limits,
            now: &self.now,
            new_id: &self.new_id,
            cache: Some(scopes.cache()),
        };
        let owner = EngineOwner {
            service_node_id: self.service_node_id.clone(),
            generation: connection.generation() as f64,
        };
        let tools = ToolClients::new(journal.client.clone());
        let now_js = || JsString::from((self.now)());
        // Declarations precede their borrowed job queues. InternalCalls carries
        // only the shared admission gate/channel/read context across the cycle.
        let policies;
        let operations;
        let categories;
        let checks;
        let root_checks;
        let inspection;
        let comments;
        let delivery_runtime;
        let scheduler_runtime;
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
        let admission_runtime;
        let admission;
        let driver;
        let orchestration;
        let native_services;
        let native;
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
        root_checks = RootChecks {
            journal: &journal,
            creation: &checks,
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
        scheduler_runtime = SchedulerRuntime::new(
            journal.client.clone(),
            principal.clone(),
            owner.service_node_id.clone(),
            SchedulerBindings {
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
        };
        actions = TaskActions {
            store,
            settings,
            catalogs: self.catalogs,
            policies: &validation,
            lifecycle: &lifecycle,
        };
        native_runtime = NativePublicationRuntime::new(
            journal.client.clone(),
            principal.clone(),
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
            publication: &publication,
            read_scope: &scopes,
        };
        program = CachedExecution {
            operations: &operations,
            principal_id: principal,
            services: &services,
        };
        execution = ExecutionQueue::new(&program, &execution_scoped);
        admission_runtime = AdmissionRuntime {
            client: journal.client.clone(),
            contracts: self.contracts,
            catalogs: self.catalogs,
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
        native = NativeOrchestrator::new(store, &orchestration, &native_services, self.catalogs);
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
        };
        reconciler = Reconciler::new(store, &reconcile_components, &reconcile_scoped);
        runtime = ReconcilerRuntime {
            reconciler: &reconciler,
            native: &native,
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
        invocation_jobs
            .drive(
                reconcile_jobs.drive(execution_jobs.drive(category_jobs.drive(
                    activation.run_engine(
                        &control,
                        generation,
                        &public,
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
            .await
    }
}
