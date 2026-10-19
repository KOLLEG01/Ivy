use super::*;
use ivy_task_board::engine_invocations::InvocationRequest;

pub(super) struct Scenario {
    cancelling: bool,
    current_run: bool,
    pub(super) ready: Gate,
    replay: RefCell<Option<Json>>,
    response: RefCell<Option<InvocationRequest>>,
}
fn f<'a>(v: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(key).unwrap_or(&NULL)
}
fn pin(id: &str) -> Json {
    Json::object([("objectId", id.into()), ("revision", Json::Number(1.0))])
}
impl Scenario {
    fn records(&self) -> Vec<(&'static str, Json, &'static str)> {
        let (mut task, mut run) = native::baseline();
        task.insert("workflowState", "review".into());
        task.insert("claim", Json::Null);
        task.insert("latestResult", pin("current-result"));
        task.insert("attemptCount", Json::Number(2.0));
        run.insert("phase", "starting".into());
        run.insert("turnId", Json::Null);
        run.insert("nativeEpoch", Json::Null);
        run.insert("primaryResourceRef", Json::Null);
        if self.cancelling {
            run.insert("phase", "cancel_requested".into());
            run.insert(
                "cancellation",
                Json::object([
                    ("operationId", "saved-cancel".into()),
                    ("reason", "Original unstarted cancellation.".into()),
                    ("requestedAt", clock().into()),
                    ("actor", f(&run, "owner").clone()),
                ]),
            );
        }
        let current_result = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("taskId", "task".into()),
            ("run", Json::Null),
            ("kind", "final".into()),
            (
                "content",
                create().required("initialResult").unwrap().clone(),
            ),
            ("createdAt", clock().into()),
            ("actor", f(&run, "owner").clone()),
            ("operationId", "saved-current-result".into()),
        ]);
        // A distinct newer terminal Run is a full admitted seed, not an
        // execution/evidence claim. Retiring the old Run may not replace it.
        let mut current = run.clone();
        let mut request = f(&current, "originalRequest").clone();
        request.insert("operationId", "newer-start".into());
        current.insert("operationId", "newer-start".into());
        current.insert("requestHash", hash_json(&request).unwrap().into());
        current.insert("originalRequest", request);
        current.insert("attempt", Json::Number(2.0));
        current.insert("phase", "completed".into());
        current.insert("primaryResourceRef", f(&task, "primaryResourceRef").clone());
        current.insert("turnId", "newer-turn".into());
        current.insert("nativeEpoch", "epoch".into());
        current.insert("result", pin("current-result"));
        current.insert("cancellation", Json::Null);
        current.insert("finishedAt", clock().into());
        current.insert(
            "externalOutcome",
            Json::object([
                ("state", "succeeded".into()),
                ("observedAt", clock().into()),
                ("evidence", Json::Null),
                ("code", Json::Null),
            ]),
        );
        if self.current_run {
            task.insert("lastRun", pin("newer-run"));
        }
        let records = vec![
            ("task-board/result", current_result, "current-result"),
            ("task-board/run", current, "newer-run"),
            ("task-board/run", run, "run"),
            ("task-board/task", task, "task"),
        ];
        for (key, value, id) in &records {
            let definition = match *key {
                "task-board/task" => "Task",
                "task-board/run" => "Run",
                "task-board/result" => "Result",
                _ => unreachable!(),
            };
            admission(definition, value)
                .unwrap_or_else(|error| panic!("Invalid retirement seed {id}: {error:?}"));
        }
        records
    }
    pub(super) fn bind(&self, context: &Context<'_>) -> IvyResult<()> {
        if context.owner.generation == 3.0 {
            // The real retained internal receiver executes this exact cached
            // nativeUpdate on a fresh graph and returns its original outcome.
            *self.response.borrow_mut() = Some(
                context
                    .calls
                    .native_update(self.replay.borrow().as_ref().unwrap())?,
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
        context.journal.journal.connection.execute(
            "INSERT INTO workspace_reservations VALUES('fixture-native-host','/fixture','task','current-workspace')",
            [],
        ).unwrap();
        Ok(())
    }
    pub(super) fn notice(
        &self,
        generation: f64,
        task: &Json,
        operation: &JsString,
    ) -> IvyResult<Option<Json>> {
        if generation == 1.0
            && f(task, "publication").is_null()
            && !f(task, "lastHistory").is_null()
        {
            return Err(
                IvyError::new("fixture_notice", "Uncertain terminal Run publication.")
                    .with_outcome(Outcome::Unknown),
            );
        }
        TaskCommentNotices.comment(task, operation)
    }
}
fn operation(fixture: &Fixture) -> Option<(String, u64, Json)> {
    let rows = fixture.rows("task-board/operation");
    assert!(
        rows.len() <= 1,
        "Retirement must retain one original operation"
    );
    rows.into_iter().next()
}
fn count(fixture: &Fixture, sql: &str) -> i64 {
    fixture.database().query_row(sql, [], |r| r.get(0)).unwrap()
}
pub(super) fn reviewed_records() -> Vec<(&'static str, Json, &'static str)> {
    Scenario {
        cancelling: false,
        current_run: false,
        ready: Gate::default(),
        replay: RefCell::default(),
        response: RefCell::default(),
    }
    .records()
}
fn assert_current(fixture: &Fixture, scenario: &Scenario) {
    let expected = scenario.records();
    assert_reviewed_current(fixture, &expected);
}
pub(super) fn assert_reviewed_current(fixture: &Fixture, expected: &[(&str, Json, &str)]) {
    let task = &fixture.rows("task-board/task")[0].2;
    let original = &expected.iter().find(|(_, _, id)| *id == "task").unwrap().1;
    for key in [
        "fields",
        "workflowState",
        "waiting",
        "claim",
        "primaryResourceRef",
        "latestResult",
        "acceptedReview",
        "comments",
        "commentDeliveries",
        "agentCommentCount",
        "workRevision",
        "attemptCount",
    ] {
        assert_eq!(
            hash_json(f(task, key)).unwrap(),
            hash_json(f(original, key)).unwrap(),
            "Changed current Task {key}"
        );
    }
    assert_eq!(f(task, "publication"), &Json::Null);
    let current = fixture
        .rows("task-board/result")
        .into_iter()
        .find(|r| r.0 == "current-result")
        .unwrap();
    assert_eq!(current.1, 1);
    assert_eq!(
        hash_json(&current.2).unwrap(),
        hash_json(&expected[0].1).unwrap()
    );
    let newer = fixture
        .rows("task-board/run")
        .into_iter()
        .find(|r| r.0 == "newer-run")
        .unwrap();
    assert_eq!(newer.1, 1);
    assert_eq!(
        hash_json(&newer.2).unwrap(),
        hash_json(&expected[1].1).unwrap()
    );
    assert_eq!(
        count(
            fixture,
            "SELECT count(*) FROM workspace_reservations WHERE task_id='task' AND operation_id='current-workspace'"
        ),
        1
    );
    assert!(fixture.rows("task-board/delivery").is_empty());
    assert!(fixture.rows("task-board/native-call").is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn retired_unstarted_runs_recover_terminal_publication_without_replacing_current_review() {
    for cancelling in [false, true] {
        let name = if cancelling {
            "cancelUnstarted retained"
        } else {
            "retireUnstarted retained"
        };
        let row = crate::native_publication::data()
            .as_array()
            .unwrap()
            .iter()
            .find(|r| f(r, "name") == &Json::from(name))
            .unwrap();
        let source_content = f(row, "boundaries")
            .as_array()
            .unwrap()
            .iter()
            .find(|e| {
                f(e, "kind") == &"create".into()
                    && f(e, "args").as_array().unwrap()[0] == Json::from("task-board/result")
            })
            .unwrap()
            .required("args")
            .unwrap()
            .as_array()
            .unwrap()[1]
            .required("content")
            .unwrap()
            .clone();
        for current_run in [false, true] {
            let peer = Peer::start_mode(Some("--factory-retirement")).await;
            let mut fixture = Fixture::new(false);
            fixture.retirement = Some(Scenario {
                cancelling,
                current_run,
                ready: Gate::default(),
                replay: RefCell::default(),
                response: RefCell::default(),
            });
            let scenario = fixture.retirement.as_ref().unwrap();
            // Refuse an invalid seed before starting an asynchronous owner.
            let _ = scenario.records();
            let contracts = ContractsBinding;
            let catalogs =
                CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
            let factory = make_factory(&fixture, &contracts, &catalogs);
            let (service, owner) = make_service(configured(&peer), &contracts).unwrap();
            let script = async {
                service.start();
                let first = bounded(service.client.wait_ready(None)).await.unwrap();
                scenario.ready.release();
                until(|| {
                    operation(&fixture)
                        .is_some_and(|o| f(&o.2, "phase") == &"needs_attention".into())
                })
                .await;
                let original = operation(&fixture).unwrap();
                assert_eq!(
                    f(f(&original.2, "request"), "change"),
                    &Json::object([(
                        "kind",
                        if cancelling {
                            "cancelUnstarted"
                        } else {
                            "retireUnstarted"
                        }
                        .into()
                    )])
                );
                assert_eq!(f(f(&original.2, "actor"), "generation"), &Json::Number(1.0));
                assert_eq!(f(f(&original.2, "actor"), "source"), &"native".into());
                assert_eq!(
                    f(f(&original.2, "actor"), "principalId"),
                    &"fixture-principal".into()
                );
                assert_eq!(fixture.rows("task-board/task")[0].1, 3);
                let retired = fixture
                    .rows("task-board/run")
                    .into_iter()
                    .find(|r| r.0 == "run")
                    .unwrap();
                assert_eq!(retired.1, 2);
                assert_eq!(
                    f(&retired.2, "phase"),
                    &Json::from(if cancelling { "cancelled" } else { "failed" })
                );
                assert_current(&fixture, scenario);
                assert_eq!(
                    count(
                        &fixture,
                        "SELECT count(*) FROM cleanup_roots WHERE root_id='run'"
                    ),
                    0,
                    "Run retention must follow the final publication callback"
                );

                // An abandoned projection job keeps the actual old graph alive
                // until the replacement connection has synchronized.
                let control = service.control.clone();
                std::thread::spawn(move || {
                    drop(control.handler("categories")(
                        Json::Null,
                        c(1, "retired-drain"),
                    ))
                })
                .join()
                .unwrap();
                until(|| fixture.page_waiting.get()).await;
                rpc(
                    &first,
                    "test.close",
                    Json::object(Vec::<(&str, Json)>::new()),
                )
                .await
                .unwrap();
                until(|| peer.methods(2).contains(&"registry.sync".into())).await;
                assert_eq!(fixture.opens.get(), 1);
                assert!(fixture.drops.borrow().is_empty());
                fixture.page_held.release();
                let second = bounded(service.client.wait_ready(None)).await.unwrap();
                assert_eq!(second.generation(), 2);
                until(|| {
                    operation(&fixture).is_some_and(|o| f(&o.2, "phase") == &"succeeded".into())
                })
                .await;
                let recovered = operation(&fixture).unwrap();
                assert_eq!(original.0, recovered.0);
                for key in ["actor", "request", "requestHash", "writes"] {
                    assert_eq!(
                        hash_json(f(&original.2, key)).unwrap(),
                        hash_json(f(&recovered.2, key)).unwrap(),
                        "Changed original {key}"
                    );
                }
                let writes = f(&recovered.2, "writes").as_array().unwrap();
                assert_eq!(writes.len(), 5);
                assert_eq!(
                    writes
                        .iter()
                        .map(|w| f(w, "contractKey").clone())
                        .collect::<Vec<_>>(),
                    [
                        "task-board/task",
                        "task-board/result",
                        "task-board/run",
                        "task-board/history",
                        "task-board/task"
                    ]
                    .map(Json::from)
                );
                let outcome = f(&recovered.2, "outcome").clone();
                let task = fixture.rows("task-board/task")[0].clone();
                assert_eq!(task.1, 3);
                let last_run = if current_run {
                    pin("newer-run")
                } else {
                    f(&outcome, "run").clone()
                };
                assert_eq!(f(&task.2, "lastRun"), &last_run);
                assert_eq!(fixture.rows("task-board/history").len(), 1);
                assert_eq!(fixture.rows("task-board/result").len(), 2);
                let result = fixture
                    .rows("task-board/result")
                    .into_iter()
                    .find(|r| r.0 != "current-result")
                    .unwrap();
                assert_eq!(result.1, 1);
                assert_eq!(f(&result.2, "actor"), f(&recovered.2, "actor"));
                assert_eq!(f(&result.2, "run"), &pin("run"));
                // The original source emits the same content; only its external
                // Run proof becomes an omitted local proof in this real journal.
                let mut expected = source_content.clone();
                expected.insert("artifacts", Json::Array(vec![]));
                let checks = f(&expected, "checks")
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned()
                    .map(|mut c| {
                        c.insert("evidence", Json::Array(vec![]));
                        c
                    })
                    .collect();
                expected.insert("checks", Json::Array(checks));
                assert_eq!(
                    hash_json(f(&result.2, "content")).unwrap(),
                    hash_json(&expected).unwrap()
                );
                assert_eq!(
                    count(
                        &fixture,
                        "SELECT count(*) FROM cleanup_roots WHERE root_id='run'"
                    ),
                    1
                );
                let dependency: String = fixture
                    .database()
                    .query_row(
                        "SELECT operation_id FROM cleanup_dependencies WHERE root_id='run'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(dependency, recovered.0);
                assert_current(&fixture, scenario);
                let revisions = count(&fixture, "SELECT count(*) FROM revisions");
                let callbacks = fixture.notices.get();
                *scenario.replay.borrow_mut() = Some(f(&recovered.2, "request").clone());
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
                let cached = bounded(response).await.unwrap();
                assert_eq!(hash_json(&cached).unwrap(), hash_json(&outcome).unwrap());
                assert_eq!(count(&fixture, "SELECT count(*) FROM revisions"), revisions);
                assert_eq!(fixture.notices.get(), callbacks);
                assert_current(&fixture, scenario);
                assert_eq!(
                    count(
                        &fixture,
                        "SELECT count(*) FROM cleanup_dependencies WHERE root_id='run'"
                    ),
                    1
                );
                bounded(service.close()).await.unwrap();
            };
            let (result, ()) = tokio::join!(owner.run(&factory), script);
            result.unwrap();
            assert_eq!(fixture.drops.borrow().as_slice(), [1, 2, 3]);
            assert_eq!(count(&fixture, "SELECT count(*) FROM active_gates"), 0);
            let frames = peer.frames.lock().unwrap().clone();
            assert!(!frames.iter().any(|v| {
                ["agent.invoke", "agent.status", "agent.inputs"]
                    .iter()
                    .any(|name| f(f(v, "params"), "qualifiedName") == &Json::from(*name))
            }));
            peer.finish().await;
        }
    }
}
