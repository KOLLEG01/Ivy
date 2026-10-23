use super::*;
use driver::{count, frames, reached};
use ivy_task_board::engine_invocations::InvocationRequest;

pub(super) struct Scenario {
    due: Json,
    future: Json,
    pub(super) ready: Gate,
    replay: RefCell<Option<Json>>,
    response: RefCell<Option<InvocationRequest>>,
    probes: RefCell<Vec<Json>>,
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
    fixture.rows(key).into_iter().find(|r| r.0 == id).unwrap()
}
impl Scenario {
    fn new() -> Self {
        let output = std::process::Command::new("node")
            .args([
                "--experimental-transform-types",
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/task-board-scheduler-oracle.mjs",
            ])
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let source = json(std::str::from_utf8(&output.stdout).unwrap());
        let group = source
            .as_array()
            .unwrap()
            .iter()
            .find(|g| f(g, "mode") == &"task".into())
            .unwrap();
        let row = |name: &str| {
            f(group, "rows")
                .as_array()
                .unwrap()
                .iter()
                .find(|r| f(r, "name") == &Json::from(name))
                .unwrap()
                .clone()
        };
        Self {
            due: row("factory due task without a ready host retains defer"),
            future: row("factory future task defers before allocation"),
            ready: Gate::default(),
            replay: RefCell::default(),
            response: RefCell::default(),
            probes: RefCell::default(),
        }
    }
    fn source(&self, id: &str) -> &Json {
        match id {
            "due-task" => &self.due,
            "future-task" => &self.future,
            _ => panic!("Unbound scheduler task {id}"),
        }
    }
    fn request(&self, id: &str) -> Json {
        let boundary = f(self.source(id), "boundaries")
            .as_array()
            .unwrap()
            .iter()
            .find(|v| f(v, "kind") == &"schedule".into())
            .unwrap();
        f(boundary, "args").as_array().unwrap()[0].clone()
    }
    fn records(&self) -> Vec<(&'static str, Json, &'static str)> {
        let mut records: Vec<_> = retirement::reviewed_records()
            .into_iter()
            .filter(|r| r.2 != "run")
            .collect();
        records[2].1.insert("lastRun", pin("newer-run", 1));
        let mut fields = f(&records[2].1, "fields").clone();
        fields.insert("priority", Json::Number(1.0));
        records[2].1.insert("fields", fields);
        for (id, key) in [("due-task", "TASK-0002"), ("future-task", "TASK-0003")] {
            let mut task = records[2].1.clone();
            task.insert("taskKey", key.into());
            task.insert("workflowState", "todo".into());
            task.insert("attemptCount", Json::Number(0.0));
            for k in [
                "primaryResourceRef",
                "latestResult",
                "acceptedReview",
                "lastRun",
                "lastHistory",
            ] {
                task.insert(k, Json::Null);
            }
            let source = f(f(f(self.source(id), "initial"), "task"), "value");
            let mut fields = f(&task, "fields").clone();
            for k in ["priority", "nextReviewAt", "executionRequirement"] {
                fields.insert(k, f(f(source, "fields"), k).clone());
            }
            task.insert("fields", fields);
            records.push(("task-board/task", task, id));
        }
        for (key, value, _) in &records {
            admission(
                match *key {
                    "task-board/task" => "Task",
                    "task-board/run" => "Run",
                    "task-board/result" => "Result",
                    _ => unreachable!(),
                },
                value,
            )
            .unwrap();
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
        context.journal.journal.connection.execute("INSERT INTO workspace_reservations VALUES('fixture-native-host','/fixture','task','current-workspace')", []).unwrap();
        Ok(())
    }
    pub(super) fn page(&self, fixture: &Fixture, options: &Json) -> IvyResult<Json> {
        let expected = json(
            r#"{"where":{"op":"and","args":[{"op":"in","field":"data:/workflowState","value":["backlog","todo","in_progress","waiting","review","done","cancelled"]}]},"limit":50,"orderBy":[{"field":"data:/fields/priority","direction":"desc"},{"field":"object.id","direction":"asc"}]}"#,
        );
        assert_eq!(hash_json(options)?, hash_json(&expected)?);
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
        assert_eq!(
            rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
            ["due-task", "future-task", "task"]
        );
        Ok(Json::object([
            (
                "items",
                Json::Array(rows.into_iter().map(|r| pin(&r.0, r.1)).collect()),
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
        self.source(f(&task.pin, "objectId").as_str().unwrap().as_str());
        assert!(f(&task.value, "lastRun").is_null());
        assert!(f(&task.value, "primaryResourceRef").is_null());
        Ok(false)
    }
    pub(super) fn prompt(
        &self,
        task: &Json,
        owner: &JsString,
        coordination: Option<&Json>,
    ) -> IvyResult<JsString> {
        assert_eq!(owner, &JsString::from("fixture-task-board"));
        assert_eq!(coordination, None);
        let id = match f(task, "taskKey").as_str().unwrap().as_str() {
            "TASK-0002" => "due-task",
            "TASK-0003" => "future-task",
            _ => panic!("Review must not render an execution prompt"),
        };
        let prompt = f(self.source(id), "boundaries")
            .as_array()
            .unwrap()
            .iter()
            .find(|v| f(v, "kind") == &"prompt".into())
            .unwrap();
        self.probes.borrow_mut().push(Json::object([
            ("kind", "prompt".into()),
            ("task", id.into()),
        ]));
        Ok(f(prompt, "value").as_js_string().unwrap().clone())
    }
    pub(super) fn notice(&self, _: f64, _: &Json, _: &JsString) -> IvyResult<Option<Json>> {
        panic!("A refused frozen Operation must not publish a Task notice")
    }
    fn preserved(&self, fixture: &Fixture) {
        for (key, value, id) in self.records() {
            let row = current(fixture, key, id);
            assert_eq!(row.1, 1);
            assert_eq!(hash_json(&row.2).unwrap(), hash_json(&value).unwrap());
        }
        assert_eq!(fixture.rows("task-board/result").len(), 1);
        assert_eq!(fixture.rows("task-board/run").len(), 1);
        for key in [
            "task-board/native-plan",
            "task-board/native-call",
            "task-board/native-evidence",
            "task-board/delivery",
            "task-board/operation",
            "task-board/history",
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
        assert_eq!(fixture.notices.get(), 0);
    }
}
fn refused(peer: &Peer, generation: f64) -> bool {
    peer.frames.lock().unwrap().iter().any(|frame| {
        f(frame, "generation") == &Json::Number(generation)
            && f(frame, "method") == &"service.heartbeat".into()
            && f(f(frame, "params"), "ready") == &Json::Bool(true)
            && f(f(frame, "params"), "diagnostics")
                .as_array()
                .is_some_and(|diagnostics| {
                    ["due-task", "future-task"].iter().all(|task| {
                        diagnostics.iter().any(|d| {
                            f(d, "code") == &"invalid_arguments".into()
                                && f(d, "message") == &"Frozen Operation admission failed.".into()
                                && f(f(d, "resource"), "objectId") == &Json::from(*task)
                        })
                    })
                })
    })
}
fn attempts(fixture: &Fixture) -> Vec<Json> {
    let db = fixture.database();
    let mut query=db.prepare("SELECT d.id,d.current_revision,d.parent_id,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='task-board/scheduler-attempt' ORDER BY d.parent_id").unwrap();
    query
        .query_map([], |r| {
            Ok(Json::object([
                (
                    "object",
                    pin(&r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64),
                ),
                ("parent", r.get::<_, String>(2)?.into()),
                ("value", json(&r.get::<_, String>(3)?)),
            ]))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}
fn verify_attempts(fixture: &Fixture, scenario: &Scenario) -> Vec<Json> {
    let rows = attempts(fixture);
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(f(f(row, "object"), "revision"), &Json::Number(1.0));
        let value = f(row, "value");
        let request = f(value, "request");
        let task = f(row, "parent").as_str().unwrap();
        assert_eq!(f(value, "task"), &pin(&task, 1));
        assert_eq!(f(value, "principalId"), &"fixture-principal".into());
        assert_eq!(f(value, "sequence"), &Json::Number(1.0));
        assert_eq!(f(value, "previous"), &Json::Null);
        assert_eq!(f(value, "previousOperation"), &Json::Null);
        assert_eq!(f(value, "preparedAt"), &Json::from(clock()));
        assert_eq!(
            f(value, "requestHash"),
            &Json::from(hash_json(request).unwrap())
        );
        let mut expected = scenario.request(&task);
        expected.insert("operationId", f(request, "operationId").clone());
        expected.insert("taskId", task.as_str().into());
        expected.insert("expectedRevision", Json::Number(1.0));
        assert_eq!(hash_json(request).unwrap(), hash_json(&expected).unwrap());
        let id = f(request, "operationId").as_str().unwrap();
        let parts: Vec<_> = id.split(':').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], "11111111-1111-4111-8111-111111111111");
        assert!(parts[1].parse::<u64>().is_ok());
        assert!(uuid::Uuid::parse_str(parts[2]).is_ok());
        // Preserve the known frozen oneOf overlap. Both branches admit the
        // original defer request, so DurableRequest must refuse it. This does
        // not qualify Operation publication or cached completion.
        admission("ActionRequest", request).unwrap();
        admission("SchedulerRequest", request).unwrap();
        let error = admission("DurableRequest", request).unwrap_err();
        assert_eq!(error.code, "invalid_arguments");
        assert_eq!(error.message, "Frozen DurableRequest admission failed.");
    }
    rows
}

#[tokio::test(flavor = "current_thread")]
async fn scheduler_retains_defer_intents_and_refuses_ambiguous_frozen_operations_without_touching_review()
 {
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
        reached(
            &fixture,
            &peer,
            "retained original intents and frozen Operation refusals",
            || attempts(&fixture).len() == 2 && refused(&peer, 1.0),
        )
        .await;
        let retained = verify_attempts(&fixture, scenario);
        let original = f(f(&retained[0], "value"), "request").clone();
        assert!(!frames(&peer, "scheduler-hosts").is_empty());
        assert_eq!(
            scenario
                .probes
                .borrow()
                .iter()
                .filter(|p| f(p, "kind") == &"prompt".into())
                .count(),
            2
        );
        scenario.preserved(&fixture);
        rpc(
            &first,
            "test.close",
            Json::object(Vec::<(&str, Json)>::new()),
        )
        .await
        .unwrap();
        until(|| peer.methods(2).contains(&"registry.sync".into())).await;
        let second = bounded(service.client.wait_ready(None)).await.unwrap();
        assert_eq!(second.generation(), 2);
        reached(
            &fixture,
            &peer,
            "same retained intents refused after reconnect",
            || refused(&peer, 2.0),
        )
        .await;
        assert_eq!(
            hash_json(&Json::Array(verify_attempts(&fixture, scenario))).unwrap(),
            hash_json(&Json::Array(retained.clone())).unwrap()
        );
        scenario.preserved(&fixture);
        assert_eq!(
            count(
                &fixture,
                "SELECT count(*) FROM revisions WHERE json_extract(value_json,'$.request.sourceTaskId') IS NOT NULL AND json_extract(value_json,'$.request.commentId') LIKE 'delegated-review-%'"
            ),
            0,
            "A root task has no delegated result to relay"
        );
        *scenario.replay.borrow_mut() = Some(original);
        let revisions = count(&fixture, "SELECT count(*) FROM revisions");
        let blobs = count(&fixture, "SELECT count(*) FROM blobs");
        rpc(
            &second,
            "test.close",
            Json::object(Vec::<(&str, Json)>::new()),
        )
        .await
        .unwrap();
        until(|| scenario.response.borrow().is_some()).await;
        let third = bounded(service.client.wait_ready(None)).await.unwrap();
        assert_eq!(third.generation(), 3);
        let response = scenario.response.borrow_mut().take().unwrap();
        let error = bounded(response).await.unwrap_err();
        assert_eq!(error.code, "invalid_arguments");
        assert_eq!(error.message, "Frozen Operation admission failed.");
        assert_eq!(error.outcome, Outcome::NotExecuted);
        assert_eq!(count(&fixture, "SELECT count(*) FROM revisions"), revisions);
        assert_eq!(count(&fixture, "SELECT count(*) FROM blobs"), blobs);
        assert_eq!(
            hash_json(&Json::Array(verify_attempts(&fixture, scenario))).unwrap(),
            hash_json(&Json::Array(retained)).unwrap()
        );
        assert!(
            frames(&peer, "scheduler-hosts")
                .iter()
                .all(|p| f(p, "generation") == &Json::Number(1.0))
        );
        assert_eq!(
            scenario
                .probes
                .borrow()
                .iter()
                .filter(|p| f(p, "kind") == &"prompt".into())
                .count(),
            2
        );
        scenario.preserved(&fixture);
        assert!(
            !peer
                .frames
                .lock()
                .unwrap()
                .iter()
                .any(|v| f(v, "method") == &"tools.call".into())
        );
        bounded(service.close()).await.unwrap();
    };
    let (result, ()) = tokio::join!(owner.run(&factory), script);
    result.unwrap();
    assert_eq!(fixture.drops.borrow().as_slice(), [1, 2, 3]);
    assert_eq!(count(&fixture, "SELECT count(*) FROM active_gates"), 0);
    peer.finish().await;
}
