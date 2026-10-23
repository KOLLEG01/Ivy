use super::*;
use ivy_contracts::hash::digest;
use ivy_task_board::{engine_invocations::InvocationRequest, evidence_bytes::ChunkKey};
#[path = "task-board-engine-factory-attachment.rs"]
mod attachment;

pub(super) struct Scenario {
    row: Json,
    pub(super) ready: Gate,
    replay: RefCell<Option<Json>>,
    response: RefCell<Option<InvocationRequest>>,
}
fn f<'a>(v: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(key).unwrap_or(&NULL)
}
fn pin(id: &str, revision: f64) -> Json {
    Json::object([
        ("objectId", id.into()),
        ("revision", Json::Number(revision)),
    ])
}
impl Scenario {
    pub(super) fn workflow_state(&self, task: &Json) -> Json {
        if f(f(&self.row, "input"), "liveAttachment").as_bool() == Some(true)
            && !f(task, "claim").is_null()
        {
            "in_progress".into()
        } else {
            "review".into()
        }
    }
    fn cancelling(&self) -> bool {
        !f(f(f(&self.row, "run"), "value"), "cancellation").is_null()
    }
    pub(super) fn expected_turn(&self) -> Json {
        let turn = f(f(f(&self.row, "turns"), "reply"), "result")
            .required("data")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|v| f(v, "id") == &"turn".into())
            .unwrap();
        Json::object([
            ("id", f(turn, "id").clone()),
            ("status", f(turn, "status").clone()),
        ])
    }
    fn records(
        &self,
        catalogs: &CheckedNativeContracts,
    ) -> Vec<(&'static str, Json, &'static str)> {
        let mut records = retirement::reviewed_records();
        let original = f(f(&self.row, "run"), "value");
        let original_plan = f(original, "plan");
        let source = catalogs
            .checked_native_contract(
                original_plan
                    .required("nativeVersion")
                    .unwrap()
                    .as_js_string()
                    .unwrap(),
                Some(&Json::object([(
                    "sourceHash",
                    f(original_plan, "catalogSourceHash").clone(),
                )])),
            )
            .unwrap();
        assert_eq!(
            f(f(&self.row, "metadata"), "catalogHash"),
            &Json::from(source.catalog_hash())
        );
        assert_eq!(
            f(f(&self.row, "metadata"), "nativeExecutableHash"),
            f(source.catalog(), "nativeExecutableHash")
        );
        let run = &mut records[2].1;
        for key in [
            "phase",
            "primaryResourceRef",
            "turnId",
            "nativeEpoch",
            "notificationCursor",
        ] {
            run.insert(key, f(original, key).clone());
        }
        let mut plan = f(run, "plan").clone();
        for key in ["nativeVersion", "catalogSourceHash"] {
            plan.insert(key, f(original_plan, key).clone());
        }
        // Bind the real retained catalog and method identities. Dispatch
        // parameter templates remain finite seeds and are never executed here.
        plan.insert(
            "definitions",
            Json::object([
                (
                    "threadStart",
                    Json::from(source.definition_hash(&"thread/start".into()).unwrap()),
                ),
                (
                    "threadResume",
                    Json::from(source.definition_hash(&"thread/resume".into()).unwrap()),
                ),
                (
                    "turnStart",
                    Json::from(source.definition_hash(&"turn/start".into()).unwrap()),
                ),
                (
                    "turnInterrupt",
                    Json::from(source.definition_hash(&"turn/interrupt".into()).unwrap()),
                ),
            ]),
        );
        run.insert("plan", plan);
        if attachment::stale(&self.row) {
            let mut calls = f(run, "calls").clone();
            calls.insert("thread", pin(driver::ATTACHMENT, 2.0));
            run.insert("calls", calls);
        }
        if self.cancelling() {
            run.insert(
                "cancellation",
                Json::object([
                    ("operationId", "saved-cancel".into()),
                    ("reason", f(f(original, "cancellation"), "reason").clone()),
                    ("requestedAt", clock().into()),
                    ("actor", f(run, "owner").clone()),
                ]),
            );
            records[3].1.insert("lastRun", pin("newer-run", 1.0));
        }
        if f(f(&self.row, "input"), "liveAttachment").as_bool() == Some(true)
            && !attachment::stale(&self.row)
        {
            let task = &mut records[3].1;
            let mut claim = f(&native::baseline().0, "claim").clone();
            claim.insert("phase", "running".into());
            task.insert("claim", claim);
            task.insert("lastRun", pin("run", 1.0));
            task.insert("workflowState", "in_progress".into());
            task.insert("attemptCount", Json::Number(1.0));
        }
        for (key, value, id) in &records {
            let definition = match *key {
                "task-board/task" => "Task",
                "task-board/run" => "Run",
                "task-board/result" => "Result",
                _ => unreachable!(),
            };
            admission(definition, value)
                .unwrap_or_else(|e| panic!("Invalid observation seed {id}: {e:?}"));
        }
        records
    }
    pub(super) fn bind(&self, context: &Context<'_>) -> IvyResult<()> {
        if context.owner.generation == 3.0 {
            *self.response.borrow_mut() = Some(
                context
                    .calls
                    .native_update(self.replay.borrow().as_ref().unwrap())?,
            );
        }
        if context.owner.generation != 1.0 {
            return Ok(());
        }
        for (key, value, id) in self.records(context.catalogs) {
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
            "INSERT INTO workspace_reservations VALUES('fixture-native-host','/fixture','task','current-workspace')", [],
        ).unwrap();
        if attachment::stale(&self.row) {
            driver::seed_attachment(context)?;
        }
        Ok(())
    }
    pub(super) fn notice(
        &self,
        generation: f64,
        task: &Json,
        operation: &JsString,
    ) -> IvyResult<Option<Json>> {
        if generation == 1.0
            && f(f(&self.row, "input"), "liveAttachment").as_bool() != Some(true)
            && f(task, "publication").is_null()
            && (f(task, "lastRun") == &pin("run", 2.0)
                || self.cancelling() && !f(task, "lastHistory").is_null())
        {
            return Err(IvyError::new(
                "fixture_notice",
                "Uncertain terminal snapshot publication.",
            )
            .with_outcome(Outcome::Unknown));
        }
        TaskCommentNotices.comment(task, operation)
    }
}
fn operation(fixture: &Fixture, kind: &str) -> Option<(String, u64, Json)> {
    let selected: Vec<_> = fixture
        .rows("task-board/operation")
        .into_iter()
        .filter(|(_, _, v)| f(f(f(v, "request"), "change"), "kind") == &Json::from(kind))
        .collect();
    assert!(selected.len() <= 1, "Duplicate {kind} Operation");
    selected.into_iter().next()
}
fn count(fixture: &Fixture, sql: &str) -> i64 {
    fixture.database().query_row(sql, [], |r| r.get(0)).unwrap()
}
async fn wait_for(mut condition: impl FnMut() -> bool) -> bool {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .is_ok()
}
fn state(fixture: &Fixture) -> String {
    let rows = ["task-board/operation", "task-board/run"]
        .into_iter()
        .flat_map(|key| fixture.rows(key))
        .map(|(id, revision, value)| {
            Json::object([
                ("object", pin(&id, revision as f64)),
                ("phase", f(&value, "phase").clone()),
                ("error", f(&value, "error").clone()),
            ])
        })
        .collect();
    ivy_contracts::json::canonical(&Json::Array(rows), 32768).unwrap()
}
async fn reached(fixture: &Fixture, stage: &str, condition: impl FnMut() -> bool) {
    assert!(
        wait_for(condition).await,
        "Native observation stage {stage}; {}",
        state(fixture)
    );
}
fn children(fixture: &Fixture, key: &str) -> Vec<(String, u64, String, Json)> {
    let db = fixture.database();
    let mut stmt = db.prepare("SELECT d.id,d.current_revision,d.parent_id,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key=? AND d.parent_id IS NOT NULL ORDER BY d.id").unwrap();
    stmt.query_map([key], |r| {
        Ok((
            r.get(0)?,
            u64::try_from(r.get::<_, i64>(1)?).unwrap(),
            r.get(2)?,
            json(&r.get::<_, String>(3)?),
        ))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}
fn check_read_bytes(fixture: &Fixture, peer: &Peer, row: &Json, expected: usize) {
    let reads = children(fixture, "task-board/native-read-evidence");
    assert_eq!(reads.len(), expected);
    let frames = peer.frames.lock().unwrap().clone();
    for (_, revision, parent, value) in reads {
        assert_eq!(revision, 1);
        assert_eq!(parent, "run");
        let mut bytes = Vec::new();
        for part in f(&value, "chunks").as_array().unwrap() {
            let object = f(f(part, "object"), "objectId").as_str().unwrap();
            let (key, owner, chunk): (String, String, Vec<u8>) = fixture
                .database()
                .query_row(
                    "SELECT key,parent_id,bytes FROM blobs WHERE id=?",
                    [object],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!(key, ChunkKey::NativeReadEvidence.as_str());
            assert_eq!(owner, "run");
            assert_eq!(f(part, "byteLength"), &Json::Number(chunk.len() as f64));
            assert_eq!(f(part, "contentHash"), &Json::from(digest(&chunk)));
            bytes.extend(chunk);
        }
        assert_eq!(f(&value, "byteLength"), &Json::Number(bytes.len() as f64));
        assert_eq!(f(&value, "contentHash"), &Json::from(digest(&bytes)));
        let observed = json(std::str::from_utf8(&bytes).unwrap());
        let sent = frames
            .iter()
            .filter(|v| f(v, "kind") == &"observation".into())
            .map(|v| f(v, "observation"))
            .find(|v| f(v, "observationId") == f(&observed, "observationId"))
            .unwrap();
        assert_eq!(hash_json(&observed).unwrap(), hash_json(sent).unwrap());
        for key in [
            "observationId",
            "callerPrincipalId",
            "serviceNodeId",
            "nativeVersion",
            "nativeExecutableHash",
            "catalogHash",
            "method",
            "requestHash",
            "epoch",
        ] {
            assert_eq!(f(&observed, key), f(&value, key));
        }
        assert_eq!(
            f(&observed, "callerPrincipalId"),
            &"fixture-principal".into()
        );
        assert_eq!(f(&observed, "epoch"), &"epoch".into());
        match f(&observed, "method").as_str().unwrap().as_str() {
            "thread/read" => assert_eq!(
                hash_json(f(&observed, "reply")).unwrap(),
                hash_json(f(f(row, "metadata"), "reply")).unwrap()
            ),
            "thread/turns/list" => assert_eq!(
                hash_json(f(&observed, "reply")).unwrap(),
                hash_json(f(f(row, "turns"), "reply")).unwrap()
            ),
            "thread/items/list" => assert_eq!(
                f(
                    f(f(f(&observed, "reply"), "result"), "data")
                        .as_array()
                        .unwrap()
                        .first()
                        .unwrap()
                        .required("item")
                        .unwrap(),
                    "text"
                ),
                &"answer".into()
            ),
            method => panic!("Unexpected saved native read {method}"),
        }
    }
}

fn observation_row(name: &str) -> Json {
    crate::orchestrator::data()
        .required("rows")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|r| f(r, "name") == &Json::from(name))
        .unwrap()
        .clone()
}

#[tokio::test(flavor = "current_thread")]
async fn started_retained_runs_publish_verified_snapshot_output_and_recover_without_replacing_review()
 {
    for unsupported in [false, true] {
        terminal_observation_case(
            observation_row("first terminal observation saves snapshot"),
            unsupported,
        )
        .await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_retained_turn_recovers_terminal_publication_without_replacing_newer_review() {
    terminal_observation_case(
        observation_row("cancelled retained terminal turn saves snapshot"),
        false,
    )
    .await;
}

async fn terminal_observation_case(row: Json, unsupported: bool) {
    let peer = Peer::start_mode(Some("--factory-observations")).await;
    let mut fixture = Fixture::new(false);
    fixture.observations = Some(Scenario {
        row: row.clone(),
        ready: Gate::default(),
        replay: RefCell::default(),
        response: RefCell::default(),
    });
    let scenario = fixture.observations.as_ref().unwrap();
    let contracts = ContractsBinding;
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let records = scenario.records(&catalogs);
    let cancelling = scenario.cancelling();
    let preserve = || {
        retirement::assert_reviewed_current(&fixture, &records);
        if cancelling {
            assert_eq!(
                f(&fixture.rows("task-board/task")[0].2, "lastRun"),
                &pin("newer-run", 1.0)
            );
        }
    };
    let factory = make_factory(&fixture, &contracts, &catalogs);
    let (service, owner) = make_service(configured(&peer), &contracts).unwrap();
    let script = async {
        service.start();
        let first = bounded(service.client.wait_ready(None)).await.unwrap();
        rpc(
            &first,
            "test.nativeObservations",
            Json::object([("row", row.clone()), ("unsupported", unsupported.into())]),
        )
        .await
        .unwrap();
        scenario.ready.release();
        reached(&fixture, "uncertain snapshot", || {
            operation(&fixture, "readSnapshot")
                .is_some_and(|o| f(&o.2, "phase") == &"needs_attention".into())
        })
        .await;
        let original = operation(&fixture, "readSnapshot").unwrap();
        assert_eq!(
            fixture.rows("task-board/task")[0].1,
            3,
            "Snapshot publication error: {:?}",
            f(&original.2, "error")
        );
        let run = fixture
            .rows("task-board/run")
            .into_iter()
            .find(|r| r.0 == "run")
            .unwrap();
        assert_eq!(run.1, 2);
        assert_eq!(f(&run.2, "phase"), &"publishing_result".into());
        assert_eq!(f(f(&original.2, "actor"), "generation"), &Json::Number(1.0));
        assert_eq!(f(f(&original.2, "actor"), "source"), &"native".into());
        let snapshot = children(&fixture, "task-board/native-turn-snapshot");
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].1, 1);
        assert_eq!(snapshot[0].2, "run");
        assert_eq!(f(&snapshot[0].3, "run"), &pin("run", 1.0));
        assert_eq!(
            f(&snapshot[0].3, "status"),
            f(&scenario.expected_turn(), "status")
        );
        assert_eq!(
            f(f(f(&original.2, "request"), "change"), "snapshot"),
            f(f(&run.2, "externalOutcome"), "evidence")
        );
        reached(&fixture, "first read receipts", || {
            peer.frames
                .lock()
                .unwrap()
                .iter()
                .filter(|v| f(v, "kind") == &"observation".into())
                .count()
                >= 3
        })
        .await;
        check_read_bytes(&fixture, &peer, &row, 2);
        preserve();
        assert_eq!(fixture.rows("task-board/result").len(), 1);
        let histories = fixture.rows("task-board/history");
        assert_eq!(histories.len(), 1);
        let snapshot_history = histories[0].clone();
        assert_eq!(f(&snapshot_history.2, "actor"), f(&original.2, "actor"));
        assert_eq!(
            count(
                &fixture,
                "SELECT count(*) FROM cleanup_roots WHERE root_id='run'"
            ),
            0
        );
        let original_reads = children(&fixture, "task-board/native-read-evidence");
        let control = service.control.clone();
        std::thread::spawn(move || {
            drop(control.handler("categories")(
                Json::Null,
                c(1, "snapshot-drain"),
            ))
        })
        .join()
        .unwrap();
        reached(&fixture, "held projection", || fixture.page_waiting.get()).await;
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
        reached(&fixture, "recovered snapshot", || {
            operation(&fixture, "readSnapshot")
                .is_some_and(|o| f(&o.2, "phase") == &"succeeded".into())
        })
        .await;
        if !wait_for(|| {
            operation(&fixture, "completion")
                .is_some_and(|o| f(&o.2, "phase") == &"succeeded".into())
        })
        .await
        {
            let readiness = match bounded(service.control.readiness(&second)).await {
                Ok(value) => value,
                Err(error) => Json::object([
                    ("code", Json::String(error.code_js())),
                    ("message", Json::String(error.message_js())),
                ]),
            };
            let reads: Vec<_> = peer
                .frames
                .lock()
                .unwrap()
                .iter()
                .filter(|v| f(v, "kind") == &"observation".into())
                .map(|v| f(f(v, "observation"), "method").clone())
                .collect();
            panic!(
                "Native observation completion; {}; {}",
                state(&fixture),
                ivy_contracts::json::canonical(
                    &Json::object([("readiness", readiness), ("reads", Json::Array(reads))]),
                    32768
                )
                .unwrap()
            );
        }
        let recovered = operation(&fixture, "readSnapshot").unwrap();
        assert_eq!(recovered.0, original.0);
        for key in ["request", "requestHash", "actor", "writes"] {
            assert_eq!(
                hash_json(f(&original.2, key)).unwrap(),
                hash_json(f(&recovered.2, key)).unwrap(),
                "Changed original snapshot {key}"
            );
        }
        assert_eq!(f(&recovered.2, "writes").as_array().unwrap().len(), 4);
        let completed = operation(&fixture, "completion").unwrap();
        assert_eq!(
            f(f(&completed.2, "actor"), "generation"),
            &Json::Number(2.0)
        );
        assert_eq!(f(&completed.2, "writes").as_array().unwrap().len(), 5);
        assert_eq!(fixture.rows("task-board/operation").len(), 2);
        let outcome = f(&completed.2, "outcome").clone();
        let run = fixture
            .rows("task-board/run")
            .into_iter()
            .find(|r| r.0 == "run")
            .unwrap();
        assert_eq!(run.1, 3);
        assert_eq!(
            f(&run.2, "phase"),
            &Json::from(if cancelling { "cancelled" } else { "completed" })
        );
        for key in [
            "cancellation",
            "nativeEpoch",
            "turnId",
            "primaryResourceRef",
        ] {
            assert_eq!(
                hash_json(f(&run.2, key)).unwrap(),
                hash_json(f(&records[2].1, key)).unwrap(),
                "Changed original cancellation identity {key}"
            );
        }
        assert_eq!(
            f(f(&run.2, "externalOutcome"), "code"),
            &if cancelling {
                Json::from("native_interrupted")
            } else {
                Json::Null
            }
        );
        assert_eq!(
            f(f(&run.2, "externalOutcome"), "state"),
            &Json::from(if cancelling { "cancelled" } else { "succeeded" })
        );
        assert_eq!(f(f(&run.2, "externalOutcome"), "evidence"), &Json::Null);
        let task = fixture.rows("task-board/task")[0].clone();
        assert_eq!(task.1, 5);
        assert_eq!(
            f(&task.2, "lastRun"),
            &if cancelling {
                pin("newer-run", 1.0)
            } else {
                pin("run", 3.0)
            }
        );
        preserve();
        let histories = fixture.rows("task-board/history");
        assert_eq!(histories.len(), 2);
        let retained = histories
            .iter()
            .find(|h| h.0 == snapshot_history.0)
            .unwrap();
        assert_eq!(retained.1, 1);
        assert_eq!(
            hash_json(&retained.2).unwrap(),
            hash_json(&snapshot_history.2).unwrap()
        );
        let final_history = histories
            .iter()
            .find(|h| h.0 != snapshot_history.0)
            .unwrap();
        assert_eq!(f(&final_history.2, "actor"), f(&completed.2, "actor"));
        if cancelling {
            assert_eq!(f(&final_history.2, "kind"), &"cancelled".into());
        }
        assert_eq!(
            f(&final_history.2, "previousHistory"),
            &pin(&snapshot_history.0, 1.0)
        );
        let results = fixture.rows("task-board/result");
        assert_eq!(results.len(), 2);
        let result = &results.iter().find(|r| r.0 != "current-result").unwrap().2;
        let content = f(result, "content");
        assert_eq!(hash_json(content).unwrap(), hash_json(&Json::object([
            ("summary", Json::from(if unsupported { "Native turn completed." } else { "answer" })),
            ("artifacts", Json::Array(vec![])), ("codeReferences", Json::Array(vec![])), ("repositoryResult", Json::Null),
            ("limitations", Json::Array(vec!["Native completion does not automatically accept the task. Native conversation history remains available from its AgentManager owner while retained there.".into()])),
        ])).unwrap());
        assert_eq!(f(result, "actor"), f(&completed.2, "actor"));
        assert_eq!(f(result, "run"), &pin("run", 2.0));
        assert_eq!(
            hash_json(&children(&fixture, "task-board/native-turn-snapshot")[0].3).unwrap(),
            hash_json(&snapshot[0].3).unwrap()
        );
        assert_eq!(
            children(&fixture, "task-board/native-turn-snapshot").len(),
            1
        );
        let reads = children(&fixture, "task-board/native-read-evidence");
        for original in &original_reads {
            let retained = reads.iter().find(|r| r.0 == original.0).unwrap();
            assert_eq!(
                hash_json(&retained.3).unwrap(),
                hash_json(&original.3).unwrap()
            );
            assert_eq!(retained.1, 1);
        }
        reached(&fixture, "output read receipt", || {
            peer.frames
                .lock()
                .unwrap()
                .iter()
                .filter(|v| f(v, "kind") == &"observation".into())
                .count()
                >= 7
        })
        .await;
        check_read_bytes(&fixture, &peer, &row, if unsupported { 2 } else { 3 });
        let dependency: String = fixture
            .database()
            .query_row(
                "SELECT operation_id FROM cleanup_dependencies WHERE root_id='run'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dependency, completed.0);
        let frames = peer.frames.lock().unwrap().clone();
        for (generation, expected) in [
            (
                1,
                vec!["thread/turns/list", "thread/read", "thread/turns/list"],
            ),
            (
                2,
                vec![
                    "thread/turns/list",
                    "thread/read",
                    "thread/turns/list",
                    "thread/items/list",
                ],
            ),
        ] {
            let methods: Vec<_> = frames
                .iter()
                .filter(|v| {
                    f(v, "generation") == &Json::Number(generation as f64)
                        && f(v, "kind") == &"observation".into()
                })
                .map(|v| f(f(v, "observation"), "method").clone())
                .collect();
            assert_eq!(
                methods,
                expected.into_iter().map(Json::from).collect::<Vec<_>>()
            );
        }
        let revisions = count(&fixture, "SELECT count(*) FROM revisions");
        let blobs = count(&fixture, "SELECT count(*) FROM blobs");
        let callbacks = fixture.notices.get();
        *scenario.replay.borrow_mut() = Some(f(&completed.2, "request").clone());
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
        assert_eq!(
            hash_json(&bounded(response).await.unwrap()).unwrap(),
            hash_json(&outcome).unwrap()
        );
        assert_eq!(count(&fixture, "SELECT count(*) FROM revisions"), revisions);
        assert_eq!(count(&fixture, "SELECT count(*) FROM blobs"), blobs);
        assert_eq!(fixture.notices.get(), callbacks);
        preserve();
        bounded(service.close()).await.unwrap();
    };
    let (result, ()) = tokio::join!(owner.run(&factory), script);
    result.unwrap();
    assert_eq!(fixture.drops.borrow().as_slice(), [1, 2, 3]);
    assert_eq!(count(&fixture, "SELECT count(*) FROM active_gates"), 0);
    let frames = peer.frames.lock().unwrap().clone();
    assert!(
        !frames
            .iter()
            .any(|v| f(f(v, "params"), "qualifiedName") == &"agent.invoke".into())
    );
    assert!(
        !frames
            .iter()
            .any(|v| f(v, "generation") == &Json::Number(3.0)
                && f(f(v, "params"), "qualifiedName") == &"agent.read".into())
    );
    peer.finish().await;
}
