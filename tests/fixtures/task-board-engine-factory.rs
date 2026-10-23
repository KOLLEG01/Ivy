use super::*;
#[path = "task-board-current-factory-contracts.rs"]
mod current_contracts;
use current_contracts::{ContractsBinding, admission};
use ivy_contracts::{checked_native_contract::CheckedNativeContracts, hash::hash_json};
use ivy_task_board::{
    category_projection::CategoryLocale,
    deliveries::DeliveryPolicies,
    engine_checks::CheckPolicy,
    engine_factory::{CurrentPolicies, EngineFactory, OpenedJournal, PolicyContext, PolicyFactory},
    lifecycle_journal::LifecyclePages,
    local_retention::HistoryLimits,
    native_intent::TaskPrompts,
    native_lifecycle::LifecyclePublication,
    native_publication::NativePublicationPolicies,
    operations::Document,
    scheduler_journal::SchedulerJournalBindings,
    task_written::{TaskCommentNotices, TaskNoticePolicy},
};
use std::{cmp::Ordering, path::PathBuf};
#[path = "task-board-engine-factory-control.rs"]
mod control;
#[path = "task-board-engine-factory-current-creation.rs"]
mod current_creation;
#[path = "task-board-engine-factory-current-reads.rs"]
mod current_reads;
#[path = "task-board-engine-factory-delivery.rs"]
mod delivery;
#[path = "task-board-engine-factory-driver.rs"]
mod driver;
#[path = "task-board-engine-factory-lifecycle.rs"]
mod lifecycle;
#[path = "task-board-engine-factory-native.rs"]
mod native;
#[path = "task-board-native-policies.rs"]
mod native_policies;
#[path = "task-board-engine-factory-observations.rs"]
mod observations;
#[path = "task-board-engine-factory-retention.rs"]
mod retention;
#[path = "task-board-engine-factory-retirement.rs"]
mod retirement;
#[path = "task-board-engine-factory-scheduler.rs"]
mod scheduler;
#[path = "task-board-engine-factory-steering.rs"]
mod steering;

fn clock() -> String {
    "2026-01-02T03:04:05.006Z".to_owned()
}
fn id() -> JsString {
    uuid::Uuid::new_v4().to_string().into()
}
type Context<'a, V = ContractsBinding> =
    PolicyContext<'a, V, fn() -> String, fn() -> JsString, retention::FactoryClock>;
struct Fixture {
    path: PathBuf,
    opens: Cell<u64>,
    held: Gate,
    page_held: Gate,
    page_waiting: Cell<bool>,
    creations: RefCell<Vec<u64>>,
    drops: RefCell<Vec<u64>>,
    pages: RefCell<Vec<(u64, String, Json)>>,
    connections: RefCell<Vec<ServiceConnection>>,
    notices: Cell<u64>,
    notice_generations: RefCell<Vec<u64>>,
    refusal: bool,
    native: Option<native::Scenario>,
    retirement: Option<retirement::Scenario>,
    observations: Option<observations::Scenario>,
    driver: Option<driver::Scenario>,
    control: Option<control::Scenario>,
    steering: Option<steering::Scenario>,
    attachment: RefCell<Option<Json>>,
    scheduler: Option<scheduler::Scenario>,
    delivery: Option<delivery::Scenario>,
    lifecycle: Option<lifecycle::Scenario>,
    retention: Option<retention::Case>,
    clock: retention::FactoryClock,
}
impl Fixture {
    fn new(refusal: bool) -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "task-board-factory-{}.sqlite",
                uuid::Uuid::new_v4()
            )),
            opens: Cell::new(0),
            held: Gate::default(),
            page_held: Gate::default(),
            page_waiting: Cell::new(false),
            creations: RefCell::default(),
            drops: RefCell::default(),
            pages: RefCell::default(),
            connections: RefCell::default(),
            notices: Cell::new(0),
            notice_generations: RefCell::default(),
            refusal,
            native: None,
            retirement: None,
            observations: None,
            driver: None,
            control: None,
            steering: None,
            attachment: RefCell::default(),
            scheduler: None,
            delivery: None,
            lifecycle: None,
            retention: None,
            clock: retention::FactoryClock::default(),
        }
    }
    fn database(&self) -> Connection {
        Connection::open(&self.path).unwrap()
    }
    fn rows(&self, key: &str) -> Vec<(String, u64, Json)> {
        let db = self.database();
        let mut statement = db.prepare("SELECT d.id,d.current_revision,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key=? AND d.parent_id IS NULL ORDER BY d.id").unwrap();
        statement
            .query_map([key], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    u64::try_from(r.get::<_, i64>(1)?).unwrap(),
                    json(&r.get::<_, String>(2)?),
                ))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }
    fn open(&self, settings: &Json) -> IvyResult<OpenedJournal> {
        admission("Settings", settings)?;
        let count = self.opens.get() + 1;
        // A new generation may not enter the provider before the preceding
        // policy and all borrowed engine queues have actually left their scope.
        assert_eq!(self.drops.borrow().len() as u64, count - 1);
        self.opens.set(count);
        OpenedJournal::new(
            self.database(),
            self.retention
                .map_or_else(HistoryLimits::default, retention::limits),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
struct Policies<'a, V = ContractsBinding> {
    fixture: &'a Fixture,
    context: Context<'a, V>,
}
impl<V> Drop for Policies<'_, V> {
    fn drop(&mut self) {
        self.fixture
            .drops
            .borrow_mut()
            .push(self.context.owner.generation as u64);
    }
}
impl PolicyFactory<ContractsBinding, fn() -> String, fn() -> JsString, retention::FactoryClock>
    for Fixture
{
    type Policies<'a> = Policies<'a>;
    fn bind<'a>(&'a self, context: Context<'a>) -> IvyResult<Policies<'a>> {
        self.connections
            .borrow_mut()
            .push(context.journal.client.service_connection().unwrap().clone());
        let policy = Policies {
            fixture: self,
            context,
        };
        if self.refusal {
            return Err(failure("policy"));
        }
        if let Some(native) = &self.native {
            native.seed(&policy.context)?;
        }
        if let Some(retirement) = &self.retirement {
            retirement.bind(&policy.context)?;
        }
        if let Some(observations) = &self.observations {
            observations.bind(&policy.context)?;
        }
        if let Some(driver) = &self.driver {
            driver.bind(&policy.context)?;
        }
        if let Some(control) = &self.control {
            control.bind(&policy.context)?;
        }
        if let Some(steering) = &self.steering {
            steering.bind(&policy.context)?;
        }
        if policy.context.owner.generation == 1.0
            && (self.control.is_some() || self.steering.is_some())
        {
            *self.attachment.borrow_mut() = Some(driver::seed_attachment(&policy.context)?);
        }
        if let Some(scheduler) = &self.scheduler {
            scheduler.bind(&policy.context)?;
        }
        if let Some(delivery) = &self.delivery {
            delivery.bind(&policy.context)?;
        }
        if let Some(lifecycle) = &self.lifecycle {
            lifecycle.bind(&policy.context)?;
        }
        // Exercise pre-graph retained internal admission on the real receiver.
        // Its abandoned observer must not discard accepted recovery work.
        drop(policy.context.calls.recover_publication()?);
        Ok(policy)
    }
}
impl<V: Contracts> CheckPolicy for Policies<'_, V> {
    async fn aggregate(&self, task: &Json, _: &JsString) -> IvyResult<()> {
        self.context
            .journal
            .contracts
            .validate_definition("Task", task)
    }
    async fn fields(&self, fields: &Json, _: Option<&JsString>) -> IvyResult<()> {
        self.context
            .journal
            .contracts
            .validate_definition("TaskFields", fields)
    }
    async fn creation(&self, _: &Json, _: &Json, _: &Json, _: &JsString) -> IvyResult<()> {
        let g = self.context.owner.generation as u64;
        self.fixture.creations.borrow_mut().push(g);
        if let Some(lifecycle) = &self.fixture.lifecycle {
            lifecycle.creations[g as usize - 1].wait().await;
        } else if g == 1 {
            self.fixture.held.wait().await;
        }
        Ok(())
    }
}
// These finite ports refuse every native prompt/completion/contact path. The
// actual factory still constructs their concrete consumers; this fixture does
// not qualify the accepted current native/question/handoff policy or accounts.
impl<V: Contracts> TaskPrompts for Policies<'_, V> {
    fn task_prompt(
        &self,
        task: &Json,
        owner: &JsString,
        coordination: Option<&Json>,
    ) -> IvyResult<JsString> {
        if let Some(driver) = &self.fixture.driver {
            return driver.prompt(task, owner, coordination);
        }
        if let Some(scheduler) = &self.fixture.scheduler {
            return scheduler.prompt(task, owner, coordination);
        }
        panic!("No native prompt")
    }
    fn task_update_prompt(
        &self,
        task: &Json,
        owner: &JsString,
        coordination: bool,
    ) -> IvyResult<JsString> {
        if let Some(steering) = &self.fixture.steering {
            return steering.prompt(task, owner, coordination);
        }
        panic!("No native prompt")
    }
}
impl<V: Contracts> NativePublicationPolicies for Policies<'_, V> {
    fn now(&self) -> JsString {
        clock().into()
    }
    async fn aggregate(&self, _: &Json, _: &JsString) -> IvyResult<()> {
        panic!("No native publication")
    }
}
impl<V: Contracts> DeliveryPolicies for Policies<'_, V> {
    fn now(&self) -> JsString {
        clock().into()
    }
    fn phone_target(&self) -> &Json {
        self.context.settings.get("phoneTarget").unwrap()
    }
    fn chat_readable_version(&self, version: &JsString) -> bool {
        if let Some(delivery) = &self.fixture.delivery {
            return delivery.chat_readable_version(version);
        }
        panic!("No contact transport")
    }
    fn handoff(&self, task: &Json, comment: &Json) -> IvyResult<JsString> {
        if let Some(delivery) = &self.fixture.delivery {
            return delivery.handoff(task, comment);
        }
        panic!("No contact handoff")
    }
    fn voice_prompt(&self, task: &Json, comment: &Json) -> IvyResult<JsString> {
        if let Some(delivery) = &self.fixture.delivery {
            return delivery.voice_prompt(task, comment);
        }
        panic!("No phone prompt")
    }
}
impl<V: Contracts> TaskNoticePolicy for Policies<'_, V> {
    fn comment(&self, task: &Json, operation: &JsString) -> IvyResult<Option<Json>> {
        self.fixture.notices.set(self.fixture.notices.get() + 1);
        self.fixture
            .notice_generations
            .borrow_mut()
            .push(self.context.owner.generation as u64);
        if let Some(native) = &self.fixture.native {
            return native.notice(self.context.owner.generation, task, operation);
        }
        if let Some(retirement) = &self.fixture.retirement {
            return retirement.notice(self.context.owner.generation, task, operation);
        }
        if let Some(observations) = &self.fixture.observations {
            return observations.notice(self.context.owner.generation, task, operation);
        }
        if let Some(scheduler) = &self.fixture.scheduler {
            return scheduler.notice(self.context.owner.generation, task, operation);
        }
        if self.fixture.delivery.is_some() {
            return TaskCommentNotices.comment(task, operation);
        }
        if self.fixture.lifecycle.is_some() {
            return TaskCommentNotices.comment(task, operation);
        }
        if self.fixture.driver.is_some()
            || self.fixture.control.is_some()
            || self.fixture.steering.is_some()
        {
            return TaskCommentNotices.comment(task, operation);
        }
        // An applied first Task step with an uncertain observation must retain
        // its original prepared write and reserved gate for the next owner.
        if self.context.owner.generation == 1.0 {
            return Err(
                IvyError::new("fixture_notice", "Uncertain post-write observation.")
                    .with_outcome(Outcome::Unknown),
            );
        }
        TaskCommentNotices.comment(task, operation)
    }
}
impl<V: Contracts> CategoryLocale for Policies<'_, V> {
    fn lowercase_default(&self, value: &JsString) -> IvyResult<JsString> {
        assert_eq!(value, &JsString::from("Clinical"));
        Ok("clinical".into())
    }
    fn compare_default(&self, a: &JsString, b: &JsString) -> Ordering {
        assert_eq!(a, b, "Only the one explicit category can be compared");
        Ordering::Equal
    }
}
impl<V: Contracts> LifecyclePages for Policies<'_, V> {
    async fn page(&self, key: &str, options: &Json) -> IvyResult<Json> {
        if let Some(lifecycle) = &self.fixture.lifecycle {
            lifecycle
                .page(self.context.owner.generation as usize, key, options)
                .await;
        }
        if let Some(delivery) = &self.fixture.delivery {
            if key == "task-board/delivery" {
                delivery.check_page(options)?;
                if self.fixture.retention.is_some() && !delivery.ready.open.get() {
                    // The finite current paging port withholds the delivery
                    // until its pending retention checkpoint has completed.
                    self.fixture.pages.borrow_mut().push((
                        self.context.owner.generation as u64,
                        key.into(),
                        options.clone(),
                    ));
                    return Ok(Json::object([
                        ("items", Json::Array(vec![])),
                        ("nextCursor", Json::Null),
                    ]));
                }
                if self.context.owner.generation == 1.0 {
                    delivery.ready.wait().await;
                }
            }
        }
        if let Some(scheduler) = &self.fixture.scheduler {
            if key == "task-board/task"
                && options.get("where").and_then(|v| v.get("op")) == Some(&"and".into())
                && self.context.owner.generation == 1.0
            {
                scheduler.ready.wait().await;
            }
        }
        if let Some(native) = &self.fixture.native {
            if key == "task-board/run" && self.context.owner.generation == 1.0 {
                native.inputs_ready.wait().await;
            }
        }
        if let Some(retirement) = &self.fixture.retirement {
            if key == "task-board/run" && self.context.owner.generation == 1.0 {
                retirement.ready.wait().await;
            }
        }
        if let Some(observations) = &self.fixture.observations {
            if key == "task-board/run" && self.context.owner.generation == 1.0 {
                observations.ready.wait().await;
            }
        }
        if let Some(driver) = &self.fixture.driver {
            if key == "task-board/run" && self.context.owner.generation == 1.0 {
                driver.ready.wait().await;
            }
        }
        if let Some(control) = &self.fixture.control {
            if key == "task-board/run" && self.context.owner.generation == 1.0 {
                control.ready.wait().await;
            }
        }
        if let Some(steering) = &self.fixture.steering {
            if key == "task-board/run" && self.context.owner.generation == 1.0 {
                steering.ready.wait().await;
            }
        }
        if self.fixture.lifecycle.is_none()
            && self.context.owner.generation == 1.0
            && options.get("select").is_some()
        {
            self.fixture.page_waiting.set(true);
            self.fixture.page_held.wait().await;
        }
        assert!(
            options.get("cursor").is_none(),
            "Finite fixture fits one page"
        );
        self.fixture.pages.borrow_mut().push((
            self.context.owner.generation as u64,
            key.into(),
            options.clone(),
        ));
        if let Some(scheduler) = &self.fixture.scheduler {
            if key == "task-board/task"
                && options.get("where").and_then(|v| v.get("op")) == Some(&"and".into())
            {
                return scheduler.page(self.fixture, options);
            }
        }
        if let Some(delivery) = &self.fixture.delivery {
            if key == "task-board/task"
                && options.get("where").and_then(|v| v.get("op")) == Some(&"and".into())
            {
                return delivery.tasks(self.fixture, options);
            }
        }
        let mut items = vec![];
        for (id, revision, value) in self.fixture.rows(key) {
            if self.fixture.steering.is_some() && id == "watched-task" {
                // The finite dependency header is read by ID; it is outside
                // this scenario's admitted task/run scheduling page.
                continue;
            }
            if let Some(filter) = options.get("where") {
                if filter.get("op") == Some(&"and".into()) {
                    assert_eq!(key, "task-board/task");
                    if let Some(steering) = &self.fixture.steering {
                        assert_eq!(value.get("workflowState"), Some(&steering.workflow_state()));
                    } else if let Some(observations) = &self.fixture.observations {
                        assert_eq!(
                            value.get("workflowState"),
                            Some(&observations.workflow_state(&value))
                        );
                    } else if self.fixture.retirement.is_some()
                        || self.fixture.control.is_some()
                        || self.fixture.lifecycle.is_some()
                    {
                        assert_eq!(value.get("workflowState"), Some(&"review".into()));
                    } else if self.fixture.native.is_some() || self.fixture.driver.is_some() {
                        assert_eq!(value.get("workflowState"), Some(&"in_progress".into()));
                    } else {
                        assert_eq!(
                            value.required("fields")?.get("control"),
                            Some(&"user".into())
                        );
                    }
                    continue;
                }
                assert_eq!(filter.get("op"), Some(&"in".into()));
                let field = filter.required("field")?.as_str().unwrap();
                let selected = match field.as_str() {
                    "object.id" => Json::from(id.clone()),
                    "data:/phase" => value.required("phase")?.clone(),
                    "data:/state" => value.required("state")?.clone(),
                    "data:/workflowState" if self.fixture.steering.is_some() => {
                        value.required("workflowState")?.clone()
                    }
                    _ => panic!("Unbound finite page field {field}"),
                };
                if !filter
                    .required("value")?
                    .as_array()
                    .unwrap()
                    .contains(&selected)
                {
                    continue;
                }
            }
            let mut item = Json::object([
                ("objectId", id.into()),
                ("revision", Json::Number(revision as f64)),
            ]);
            if options.get("select").is_some() {
                assert_eq!(key, "task-board/task");
                item.insert(
                    "values",
                    Json::object([
                        (
                            "data:/fields/category",
                            value.required("fields")?.required("category")?.clone(),
                        ),
                        ("data:/createdAt", value.required("createdAt")?.clone()),
                    ]),
                );
            }
            items.push(item);
        }
        assert!(items.len() <= options.required("limit")?.as_f64().unwrap() as usize);
        Ok(Json::object([
            ("items", Json::Array(items)),
            ("nextCursor", Json::Null),
        ]))
    }
}
impl<V: Contracts> SchedulerJournalBindings for Policies<'_, V> {
    fn free_bytes(&self) -> IvyResult<Option<f64>> {
        Ok(None)
    }
}
impl<V: Contracts> CurrentPolicies for Policies<'_, V> {
    fn scheduler_enabled(&self) -> bool {
        self.fixture.scheduler.is_some()
    }
    fn scheduler_interval_ms(&self) -> f64 {
        1000.0
    }
    fn page_size(&self) -> f64 {
        50.0
    }
}
fn settings() -> Json {
    json(
        r#"{"principalId":"fixture-principal","rootObjectId":null,"scheduler":{"enabled":false,"intervalMs":1000,"pageSize":50},"phoneTarget":null}"#,
    )
}
fn configured(peer: &Peer) -> ServiceSetup {
    let mut setup = setup(peer, false);
    setup.settings = settings();
    setup.tools.push("update".into());
    setup
}
fn create() -> Json {
    json(
        r#"{"action":"create","operationId":"factory-create","workflowState":"review","fields":{"title":"Manual factory task","description":"Original durable material","acceptanceCriteria":[],"category":null,"control":"user","priority":2,"executionRequirement":null,"workspaceRequirement":{"kind":"directory_path","path":"/fixture"},"userContact":"ticket","dueAt":null},"initialResult":{"summary":"Original final material","artifacts":[],"codeReferences":[],"repositoryResult":null,"limitations":[]}}"#,
    )
}
fn c(generation: u64, operation: &str) -> InvocationContext {
    let mut c = context(generation);
    c.operation_id = Some(operation.into());
    c
}
// Share the closure's Rust type so every case uses one compiled engine graph.
fn make_factory<'a>(
    fixture: &'a Fixture,
    contracts: &'a ContractsBinding,
    catalogs: &'a CheckedNativeContracts,
) -> EngineFactory<
    'a,
    ContractsBinding,
    Fixture,
    impl Fn(&Json) -> IvyResult<OpenedJournal> + 'a,
    fn() -> String,
    fn() -> JsString,
    retention::FactoryClock,
> {
    EngineFactory {
        contracts,
        catalogs,
        policies: fixture,
        open_journal: move |s: &Json| fixture.open(s),
        now: clock as fn() -> String,
        new_id: id as fn() -> JsString,
        clock: fixture.clock.clone(),
        service_node_id: "fixture-task-board".into(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn production_factory_recovers_abandoned_creation_after_reconnect_and_reuses_original_writes()
{
    let peer = Peer::start_mode(Some("--factory")).await;
    let fixture = Fixture::new(false);
    let contracts = ContractsBinding;
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let factory = make_factory(&fixture, &contracts, &catalogs);
    let (service, owner) = make_service(configured(&peer), &contracts).unwrap();
    let original = create();
    let script = async {
        service.start();
        let first = bounded(service.client.wait_ready(None)).await.unwrap();
        let remote = {
            let first = first.clone();
            let request = original.clone();
            tokio::spawn(async move { rpc(&first, "test.invokeAction", request).await })
        };
        until(|| fixture.creations.borrow().as_slice() == [1]).await;
        let retained = fixture.rows("task-board/operation");
        assert_eq!(retained.len(), 1);
        assert_eq!(
            hash_json(retained[0].2.required("request").unwrap()).unwrap(),
            hash_json(&original).unwrap()
        );
        remote.abort();
        assert!(remote.await.unwrap_err().is_cancelled());
        fixture.held.release();
        until(|| {
            fixture.rows("task-board/operation")[0].2.get("phase")
                == Some(&"needs_attention".into())
        })
        .await;
        let first_task = fixture.rows("task-board/task");
        assert_eq!(first_task.len(), 1);
        assert_eq!(first_task[0].1, 1);
        // A second abandoned public observer still owns a real category job.
        // Its pending page must keep this graph alive through reconnect.
        let control = service.control.clone();
        std::thread::spawn(move || {
            drop(control.handler("categories")(
                Json::Null,
                c(1, "abandoned-categories"),
            ))
        })
        .join()
        .unwrap();
        until(|| fixture.page_waiting.get()).await;
        bounded(rpc(
            &first,
            "test.close",
            Json::object(Vec::<(&str, Json)>::new()),
        ))
        .await
        .unwrap();
        until(|| peer.methods(2).contains(&"registry.sync".into())).await;
        assert_eq!(fixture.opens.get(), 1);
        assert!(fixture.drops.borrow().is_empty());
        fixture.page_held.release();
        until(|| fixture.opens.get() == 2).await;
        let second = bounded(service.client.wait_ready(None)).await.unwrap();
        assert_eq!(second.generation(), 2);
        // Startup Operation recovery uses the real Reconciler, internal channel,
        // ExecutionQueue, root checks, Task publication and observer/gates.
        until(|| {
            fixture.rows("task-board/operation")[0].2.get("phase") == Some(&"succeeded".into())
        })
        .await;
        assert_eq!(fixture.drops.borrow().as_slice(), [1]);
        assert_eq!(fixture.creations.borrow().as_slice(), [1]);
        let saved = fixture.rows("task-board/operation");
        assert_eq!(saved[0].0, retained[0].0);
        assert_eq!(saved[0].2.get("actor"), retained[0].2.get("actor"));
        assert_eq!(
            saved[0].2.get("requestHash"),
            Some(&Json::from(hash_json(&original).unwrap()))
        );
        let actor = saved[0].2.required("actor").unwrap();
        assert_eq!(actor.get("generation"), Some(&Json::Number(1.0)));
        assert_eq!(actor.get("source"), Some(&"user".into()));
        assert_eq!(
            actor.get("principalId"),
            Some(&Json::String(context(1).caller_principal_id))
        );
        let writes = saved[0].2.required("writes").unwrap().as_array().unwrap();
        assert_eq!(writes.len(), 4);
        let outcome = saved[0].2.required("outcome").unwrap().clone();
        let task = fixture.rows("task-board/task");
        assert_eq!(task.len(), 1);
        assert_eq!(task[0].0, first_task[0].0);
        assert_eq!(task[0].1, 2);
        assert_eq!(task[0].2.get("taskKey"), Some(&"TASK-0001".into()));
        assert_eq!(task[0].2.get("publication"), Some(&Json::Null));
        assert!(fixture.notices.get() >= 3);
        assert_eq!(
            fixture
                .notice_generations
                .borrow()
                .iter()
                .filter(|g| **g == 2)
                .count(),
            2
        );
        assert_eq!(fixture.rows("task-board/history").len(), 1);
        assert_eq!(fixture.rows("task-board/result").len(), 1);
        assert_eq!(fixture.rows("task-board/task-key-allocation").len(), 1);
        let count_before: i64 = fixture
            .database()
            .query_row("SELECT count(*) FROM revisions", [], |r| r.get(0))
            .unwrap();
        let notices_before = fixture.notices.get();
        let replay = service.control.handler("update")(original.clone(), c(2, "factory-create"))
            .await
            .unwrap();
        assert_eq!(hash_json(&replay).unwrap(), hash_json(&outcome).unwrap());
        let count_after: i64 = fixture
            .database()
            .query_row("SELECT count(*) FROM revisions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count_before, count_after);
        assert_eq!(fixture.notices.get(), notices_before);
        let categories = service.control.handler("categories")(Json::Null, c(2, "categories"))
            .await
            .unwrap();
        assert_eq!(
            categories,
            Json::object([("categories", Json::Array(vec![]))])
        );
        let operation = service.control.handler("operation")(
            Json::object([("operationId", "factory-create".into())]),
            c(2, "lookup"),
        )
        .await
        .unwrap();
        assert_eq!(
            hash_json(&operation).unwrap(),
            hash_json(&saved[0].2).unwrap()
        );
        let task_query = Json::object([("task", Json::from(task[0].0.clone()))]);
        let read = service.control.handler("read")(task_query, c(2, "read"))
            .await
            .unwrap();
        assert_eq!(
            read.required("task").unwrap().get("taskKey"),
            Some(&"TASK-0001".into())
        );
        bounded(service.close()).await.unwrap();
    };
    let (result, ()) = tokio::join!(owner.run(&factory), script);
    result.unwrap();
    assert_eq!(fixture.drops.borrow().as_slice(), [1, 2]);
    assert_eq!(fixture.opens.get(), 2);
    assert_eq!(
        fixture
            .database()
            .query_row("SELECT count(*) FROM active_gates", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(
        fixture
            .pages
            .borrow()
            .iter()
            .any(|(g, k, _)| *g == 2 && k == "task-board/operation")
    );
    assert!(peer.methods(2).contains(&"events.ack".into()));
    peer.finish().await;
}

#[tokio::test(flavor = "current_thread")]
async fn production_factory_policy_refusal_never_exposes_a_generation_or_leaves_an_open_graph() {
    let peer = Peer::start_mode(Some("--factory")).await;
    let fixture = Fixture::new(true);
    let contracts = ContractsBinding;
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let factory = make_factory(&fixture, &contracts, &catalogs);
    let (service, owner) = make_service(configured(&peer), &contracts).unwrap();
    let script = async {
        service.start();
        until(|| !fixture.drops.borrow().is_empty()).await;
        assert!(!service.client.ready());
        let error = service.control.handler("workspace")(Json::Null, context(1))
            .await
            .unwrap_err();
        assert_eq!(error.code, "service_not_ready");
        assert_eq!(fixture.rows("task-board/operation").len(), 0);
        bounded(service.close()).await.unwrap();
    };
    let (result, ()) = tokio::join!(owner.run(&factory), script);
    result.unwrap();
    assert_eq!(fixture.opens.get(), fixture.drops.borrow().len() as u64);
    assert!(!peer.methods(1).contains(&"events.head".into()));
    peer.finish().await;
}
