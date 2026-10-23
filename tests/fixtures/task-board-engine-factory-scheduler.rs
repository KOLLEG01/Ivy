use super::*;
use driver::{count, frames};
use ivy_task_board::engine_invocations::InvocationRequest;

pub(super) struct Scenario {
    pub(super) ready: Gate,
    replay: RefCell<Option<Json>>,
    response: RefCell<Option<InvocationRequest>>,
    prompts: Cell<usize>,
}
fn f<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn pin(id: &str, revision: u64) -> Json {
    Json::object([
        ("objectId", id.into()),
        ("revision", Json::Number(revision as f64)),
    ])
}
fn current(fixture: &Fixture, key: &str, id: &str) -> (String, u64, Json) {
    fixture
        .rows(key)
        .into_iter()
        .find(|row| row.0 == id)
        .unwrap()
}
fn reviewed() -> Vec<(&'static str, Json, &'static str)> {
    retirement::reviewed_records()
        .into_iter()
        .filter(|row| row.2 != "run")
        .map(|(key, mut value, id)| {
            if id == "task" {
                value.insert("lastRun", pin("newer-run", 1));
                let mut fields = f(&value, "fields").clone();
                fields.insert("priority", Json::Number(1.0));
                value.insert("fields", fields);
            }
            (key, value, id)
        })
        .collect()
}
impl Scenario {
    fn new() -> Self {
        Self {
            ready: Gate::default(),
            replay: RefCell::default(),
            response: RefCell::default(),
            prompts: Cell::new(0),
        }
    }
    fn records(&self) -> Vec<(&'static str, Json, &'static str)> {
        let mut records = reviewed();
        let original = records
            .iter()
            .find(|row| row.2 == "task")
            .unwrap()
            .1
            .clone();
        for (id, key, wake) in [
            ("due-task", "TASK-0002", Json::Null),
            (
                "future-task",
                "TASK-0003",
                "2026-01-02T04:00:00.000Z".into(),
            ),
        ] {
            let mut task = original.clone();
            task.insert("taskKey", key.into());
            task.insert(
                "workflowState",
                if wake.is_null() { "todo" } else { "waiting" }.into(),
            );
            task.insert("attemptCount", Json::Number(0.0));
            task.insert("wakeAt", wake);
            task.insert("runtimeWait", Json::Null);
            for name in [
                "primaryResourceRef",
                "latestResult",
                "acceptedReview",
                "lastRun",
                "lastHistory",
            ] {
                task.insert(name, Json::Null);
            }
            let mut fields = f(&task, "fields").clone();
            fields.insert("priority", Json::Number(2.0));
            task.insert("fields", fields);
            records.push(("task-board/task", task, id));
        }
        records
    }
    pub(super) fn bind(&self, context: &Context<'_>) -> IvyResult<()> {
        if context.owner.generation == 3.0 {
            *self.response.borrow_mut() = Some(
                context
                    .calls
                    .schedule(self.replay.borrow().as_ref().unwrap())?,
            );
        }
        if context.owner.generation != 1.0 {
            return Ok(());
        }
        for (key, value, id) in self.records() {
            let version = context.journal.contracts.version(key)?;
            context
                .journal
                .contracts
                .validate_record(key, &version.into(), &value)?;
            context.journal.journal.create(
                key,
                &id.into(),
                None,
                &value,
                &clock().into(),
                &id.into(),
            )?;
        }
        context.journal.journal.connection.execute("INSERT INTO workspace_reservations VALUES('fixture-native-host','/fixture','task','current-workspace')",[]).unwrap();
        Ok(())
    }
    pub(super) fn page(&self, fixture: &Fixture, options: &Json) -> IvyResult<Json> {
        assert_eq!(f(options, "limit"), &Json::Number(50.0));
        let mut rows = fixture.rows("task-board/task");
        assert_eq!(rows.len(), 3);
        rows.sort_by(|a, b| {
            f(f(&b.2, "fields"), "priority")
                .as_f64()
                .unwrap()
                .partial_cmp(&f(f(&a.2, "fields"), "priority").as_f64().unwrap())
                .unwrap()
                .then(a.0.cmp(&b.0))
        });
        Ok(Json::object([
            (
                "items",
                Json::Array(rows.into_iter().map(|row| pin(&row.0, row.1)).collect()),
            ),
            ("nextCursor", Json::Null),
        ]))
    }
    pub(super) fn coordination(&self, task: &Json) -> IvyResult<bool> {
        assert_eq!(f(task, "comments"), &Json::Array(vec![]));
        assert_eq!(f(task, "commentDeliveries"), &Json::Array(vec![]));
        Ok(false)
    }
    pub(super) fn missing(&self, task: &Document) -> IvyResult<bool> {
        assert!(f(&task.value, "lastRun").is_null());
        assert!(f(&task.value, "primaryResourceRef").is_null());
        Ok(false)
    }
    pub(super) fn prompt(
        &self,
        task: &Json,
        owner: &JsString,
        flag: Option<&Json>,
    ) -> IvyResult<JsString> {
        assert_eq!(
            f(task, "taskKey"),
            &"TASK-0002".into(),
            "A future or reviewed task must not request execution"
        );
        self.prompts.set(self.prompts.get() + 1);
        ivy_task_board::prompts::CurrentTaskPrompts.task_prompt(task, owner, flag)
    }
    pub(super) fn notice(&self, _: f64, _: &Json, _: &JsString) -> IvyResult<Option<Json>> {
        Ok(None)
    }
    fn preserved(&self, fixture: &Fixture) {
        for (key, value, id) in reviewed() {
            let row = current(fixture, key, id);
            assert_eq!(row.1, 1);
            assert_eq!(hash_json(&row.2).unwrap(), hash_json(&value).unwrap());
        }
        let future = current(fixture, "task-board/task", "future-task");
        assert_eq!(future.1, 1);
        assert_eq!(f(&future.2, "wakeAt"), &"2026-01-02T04:00:00.000Z".into());
        assert_eq!(fixture.rows("task-board/run").len(), 1);
        assert_eq!(fixture.rows("task-board/result").len(), 1);
        for key in [
            "task-board/native-plan",
            "task-board/native-call",
            "task-board/native-evidence",
            "task-board/delivery",
        ] {
            assert!(fixture.rows(key).is_empty());
        }
        assert_eq!(
            count(
                fixture,
                "SELECT count(*) FROM workspace_reservations WHERE task_id='task' AND operation_id='current-workspace'"
            ),
            1
        );
    }
}
fn attempts(fixture: &Fixture) -> Vec<Json> {
    let db = fixture.database();
    let mut query=db.prepare("SELECT d.id,d.current_revision,d.parent_id,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='task-board/scheduler-attempt' ORDER BY d.parent_id").unwrap();
    query
        .query_map([], |row| {
            Ok(Json::object([
                (
                    "object",
                    pin(&row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64),
                ),
                ("parent", row.get::<_, String>(2)?.into()),
                ("value", json(&row.get::<_, String>(3)?)),
            ]))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}
#[tokio::test(flavor = "current_thread")]
async fn current_scheduler_publishes_a_retained_defer_and_replays_the_exact_completed_operation() {
    let defer = json(
        r#"{"action":"defer","operationId":"current-defer-admission","taskId":"due-task","expectedRevision":1,"runtimeWait":{"reason":"host","detail":"No ready host.","since":"2026-01-02T03:04:05.006Z","retryAt":"2026-01-02T03:05:05.006Z"}}"#,
    );
    admission("ActionRequest", &defer).unwrap();
    admission("SchedulerRequest", &defer).unwrap();
    admission(
        "SchedulerAttempt",
        &Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("principalId", "fixture-principal".into()),
            ("task", pin("due-task", 1)),
            ("sequence", Json::Number(1.0)),
            ("request", defer.clone()),
            ("requestHash", hash_json(&defer).unwrap().into()),
            ("previous", Json::Null),
            ("previousOperation", Json::Null),
            ("preparedAt", clock().into()),
        ]),
    )
    .unwrap();
    admission("DurableRequest", &defer).expect(
        "Current defer must be admitted by its durable request union before scheduling effects",
    );
    let peer = Peer::start_mode(Some("--factory-scheduler")).await;
    let mut fixture = Fixture::new(false);
    fixture.scheduler = Some(Scenario::new());
    let scenario = fixture.scheduler.as_ref().unwrap();
    let contracts = ContractsBinding;
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let factory = make_factory(&fixture, &contracts, &catalogs);
    let mut setup = configured(&peer);
    let mut scheduling = f(&setup.settings, "scheduler").clone();
    scheduling.insert("enabled", true.into());
    setup.settings.insert("scheduler", scheduling);
    let (service, owner) = make_service(setup, &contracts).unwrap();
    let script = async {
        service.start();
        let first = bounded(service.client.wait_ready(None)).await.unwrap();
        scenario.ready.release();
        until(|| {
            fixture
                .rows("task-board/operation")
                .iter()
                .any(|row| f(&row.2, "phase") == &"succeeded".into())
        })
        .await;
        let saved = attempts(&fixture);
        assert_eq!(saved.len(), 1);
        assert_eq!(f(&saved[0], "parent"), &"due-task".into());
        let attempt = f(&saved[0], "value");
        admission("SchedulerAttempt", attempt).unwrap();
        assert_eq!(f(attempt, "sequence"), &Json::Number(1.0));
        assert_eq!(f(attempt, "previous"), &Json::Null);
        assert_eq!(f(attempt, "previousOperation"), &Json::Null);
        assert_eq!(f(attempt, "task"), &pin("due-task", 1));
        let request = f(attempt, "request").clone();
        admission("DurableRequest", &request).unwrap();
        assert_eq!(f(&request, "action"), &"defer".into());
        assert_eq!(
            f(attempt, "requestHash"),
            &Json::from(hash_json(&request).unwrap())
        );
        assert!(request.get("waiting").is_none());
        assert!(request.get("nextReviewAt").is_none());
        let operation = fixture.rows("task-board/operation");
        assert_eq!(operation.len(), 1);
        admission("Operation", &operation[0].2).unwrap();
        assert_eq!(
            f(f(&operation[0].2, "actor"), "source"),
            &"scheduler".into()
        );
        assert_eq!(f(&operation[0].2, "request"), &request);
        let outcome = f(&operation[0].2, "outcome").clone();
        let due = current(&fixture, "task-board/task", "due-task");
        assert_eq!(f(&due.2, "workflowState"), &"todo".into());
        assert_eq!(f(&due.2, "wakeAt"), &Json::Null);
        assert_eq!(f(f(&due.2, "runtimeWait"), "reason"), &"host".into());
        assert!(f(f(&due.2, "runtimeWait"), "retryAt").as_str().unwrap() > clock());
        assert_eq!(scenario.prompts.get(), 1);
        assert!(!frames(&peer, "scheduler-hosts").is_empty());
        scenario.preserved(&fixture);
        rpc(&first, "test.close", Json::Object(vec![]))
            .await
            .unwrap();
        until(|| peer.methods(2).contains(&"registry.sync".into())).await;
        let second = bounded(service.client.wait_ready(None)).await.unwrap();
        assert_eq!(second.generation(), 2);
        assert_eq!(
            hash_json(&Json::Array(attempts(&fixture))).unwrap(),
            hash_json(&Json::Array(saved.clone())).unwrap()
        );
        *scenario.replay.borrow_mut() = Some(request);
        let revisions = count(&fixture, "SELECT count(*) FROM revisions");
        rpc(&second, "test.close", Json::Object(vec![]))
            .await
            .unwrap();
        until(|| scenario.response.borrow().is_some()).await;
        let third = bounded(service.client.wait_ready(None)).await.unwrap();
        assert_eq!(third.generation(), 3);
        let response = scenario.response.borrow_mut().take().unwrap();
        assert_eq!(bounded(response).await.unwrap(), outcome);
        assert_eq!(count(&fixture, "SELECT count(*) FROM revisions"), revisions);
        assert_eq!(
            hash_json(&Json::Array(attempts(&fixture))).unwrap(),
            hash_json(&Json::Array(saved)).unwrap()
        );
        assert_eq!(scenario.prompts.get(), 1);
        scenario.preserved(&fixture);
        assert!(
            !peer
                .frames
                .lock()
                .unwrap()
                .iter()
                .any(|frame| f(frame, "method") == &"tools.call".into())
        );
        bounded(service.close()).await.unwrap();
    };
    let (result, ()) = tokio::join!(owner.run(&factory), script);
    result.unwrap();
    assert_eq!(fixture.drops.borrow().as_slice(), [1, 2, 3]);
    assert_eq!(count(&fixture, "SELECT count(*) FROM active_gates"), 0);
    peer.finish().await;
}
