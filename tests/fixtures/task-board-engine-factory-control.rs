use super::*;
use driver::{count, evidence as all_evidence, exact_evidence, frames, reached};
use ivy_task_board::engine_invocations::InvocationRequest;

pub(super) struct Scenario {
    plan: Json,
    pub(super) ready: Gate,
    replay: RefCell<Option<Json>>,
    response: RefCell<Option<InvocationRequest>>,
}

fn control_calls(fixture: &Fixture) -> Vec<(String, u64, Json)> {
    fixture
        .rows("task-board/native-call")
        .into_iter()
        .filter(|row| row.0 != driver::ATTACHMENT)
        .collect()
}
fn evidence(fixture: &Fixture) -> Vec<(String, String, Json)> {
    all_evidence(fixture)
        .into_iter()
        .filter(|row| row.1 != driver::ATTACHMENT)
        .collect()
}
fn f<'a>(v: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(key).unwrap_or(&NULL)
}
fn pin(id: &str, revision: u64) -> Json {
    Json::object([
        ("objectId", id.into()),
        ("revision", Json::Number(revision as f64)),
    ])
}
impl Scenario {
    fn records(&self) -> Vec<(&'static str, Json, &'static str)> {
        let mut records = retirement::reviewed_records();
        let (_, original) = native::baseline();
        let run = &mut records[2].1;
        for key in [
            "primaryResourceRef",
            "turnId",
            "nativeEpoch",
            "externalOutcome",
        ] {
            run.insert(key, f(&original, key).clone());
        }
        run.insert("plan", self.plan.clone());
        let mut calls = f(&run, "calls").clone();
        calls.insert(
            "thread",
            Json::object([
                ("objectId", driver::ATTACHMENT.into()),
                ("revision", Json::Number(2.0)),
            ]),
        );
        run.insert("calls", calls);
        run.insert("phase", "cancel_requested".into());
        run.insert(
            "cancellation",
            Json::object([
                ("operationId", "saved-cancel".into()),
                ("reason", "stop".into()),
                ("requestedAt", clock().into()),
                ("actor", f(run, "owner").clone()),
            ]),
        );
        admission("Run", run).unwrap();
        // The old Run is superseded by a distinct newer attempt. Late control
        // observations may advance the old Run, never this current review pin.
        records[3].1.insert("lastRun", pin("newer-run", 1));
        admission("Task", &records[3].1).unwrap();
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
            "INSERT INTO workspace_reservations VALUES('fixture-native-host','/fixture','task','current-workspace')", [],
        ).unwrap();
        Ok(())
    }
}
fn run(fixture: &Fixture) -> (String, u64, Json) {
    fixture
        .rows("task-board/run")
        .into_iter()
        .find(|r| r.0 == "run")
        .unwrap()
}
fn assert_review(fixture: &Fixture, scenario: &Scenario) {
    let records = scenario.records();
    assert_review_records(fixture, &records);
}
pub(super) fn assert_review_records(fixture: &Fixture, records: &[(&str, Json, &str)]) {
    if fixture.attachment.borrow().is_some() {
        let attachment = fixture
            .rows("task-board/native-call")
            .into_iter()
            .find(|row| row.0 == driver::ATTACHMENT)
            .unwrap();
        assert_eq!(attachment.1, 2);
        assert_eq!(f(&attachment.2, "ownerPhase"), &"succeeded".into());
        assert_eq!(f(&attachment.2, "epoch"), &"epoch".into());
        assert_eq!(
            f(f(&run(fixture).2, "calls"), "thread"),
            &pin(driver::ATTACHMENT, 2)
        );
    }
    let expected = &records[3].1;
    let task = &fixture.rows("task-board/task")[0].2;
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
        "lastRun",
        "publication",
    ] {
        assert_eq!(
            hash_json(f(task, key)).unwrap(),
            hash_json(f(expected, key)).unwrap(),
            "Changed reviewed Task {key}"
        );
    }
    let histories = fixture.rows("task-board/history");
    let operations = fixture.rows("task-board/operation");
    for (_, _, history) in &histories {
        assert_eq!(f(history, "taskId"), &"task".into());
        assert_eq!(f(f(history, "priorRun"), "objectId"), &"run".into());
        assert!(
            operations
                .iter()
                .any(
                    |(_, _, operation)| f(operation, "operationId") == f(history, "operationId")
                        && f(f(operation, "request"), "run") == f(history, "priorRun")
                )
        );
    }
    if histories.is_empty() {
        assert_eq!(f(task, "lastHistory"), &Json::Null);
    } else {
        assert!(
            histories
                .iter()
                .any(|(id, revision, _)| f(task, "lastHistory") == &pin(id, *revision))
        );
    }
    for (key, expected, id) in &records[..2] {
        let current = fixture.rows(key).into_iter().find(|r| r.0 == *id).unwrap();
        assert_eq!(current.1, 1);
        assert_eq!(hash_json(&current.2).unwrap(), hash_json(expected).unwrap());
    }
    assert_eq!(fixture.rows("task-board/result").len(), 1);
    assert!(fixture.rows("task-board/delivery").is_empty());
    assert_eq!(
        count(
            fixture,
            "SELECT count(*) FROM workspace_reservations WHERE task_id='task' AND operation_id='current-workspace'"
        ),
        1
    );
    assert_eq!(
        count(
            fixture,
            "SELECT count(*) FROM cleanup_roots WHERE root_id='run'"
        ),
        0
    );
}
pub(super) fn data() -> Json {
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/task-board-native-advance-oracle.mjs",
        ])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    json(std::str::from_utf8(&output.stdout).unwrap())
}
pub(super) fn signals(fixture: &Fixture) -> Vec<Json> {
    let db = fixture.database();
    let mut statement = db.prepare("SELECT r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='task-board/native-signals' AND d.parent_id='run'").unwrap();
    statement
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|row| json(&row.unwrap()))
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn interrupt_rechecks_the_owner_epoch_after_inspection_without_replacing_current_review() {
    let data = data();
    let original = data
        .as_array()
        .unwrap()
        .iter()
        .find(|g| f(f(f(g, "base"), "run"), "plan").get("nativeVersion") == Some(&"0.159.2".into()))
        .unwrap();
    assert!(
        f(original, "rows")
            .as_array()
            .unwrap()
            .iter()
            .any(|r| f(r, "name") == &"interrupt refuses replacement epoch".into())
    );
    let base = f(original, "base");
    let plan = f(f(base, "run"), "plan").clone();
    let intended = f(f(base, "calls"), "interrupt");
    let peer = Peer::start_mode(Some("--factory-control")).await;
    let mut fixture = Fixture::new(false);
    fixture.control = Some(Scenario {
        plan: plan.clone(),
        ready: Gate::default(),
        replay: RefCell::default(),
        response: RefCell::default(),
    });
    let scenario = fixture.control.as_ref().unwrap();
    let contracts = ContractsBinding;
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let factory = make_factory(&fixture, &contracts, &catalogs);
    let (service, owner) = make_service(configured(&peer), &contracts).unwrap();
    let guard = |v: &Json| {
        f(f(v, "request"), "change").get("code") == Some(&"task_board_native_epoch_changed".into())
    };
    let settled = || {
        fixture
            .rows("task-board/operation")
            .iter()
            .all(|r| f(&r.2, "phase") == &"succeeded".into())
    };
    let quiet = || {
        assert!(frames(&peer, "native-invoke").is_empty());
        assert!(frames(&peer, "committed-operation").is_empty());
        assert!(frames(&peer, "prevented-operation").is_empty());
        assert!(evidence(&fixture).is_empty());
    };
    let script = async {
        service.start();
        let first = bounded(service.client.wait_ready(None)).await.unwrap();
        rpc(
            &first,
            "test.control",
            Json::object([
                ("plan", plan.clone()),
                ("replacement", true.into()),
                ("interruptRace", true.into()),
            ]),
        )
        .await
        .unwrap();
        scenario.ready.release();
        reached(
            &fixture,
            &peer,
            "owner epoch changed at the interrupt dispatch guard",
            || {
                settled()
                    && fixture
                        .rows("task-board/operation")
                        .iter()
                        .any(|r| guard(&r.2))
                    && signals(&fixture)
                        .iter()
                        .any(|v| f(v, "epoch") == &"replacement-epoch".into())
            },
        )
        .await;
        let calls = control_calls(&fixture);
        assert_eq!(calls.len(), 1);
        let prepared = calls[0].clone();
        assert_eq!(prepared.1, 1);
        assert_eq!(f(&prepared.2, "state"), &"prepared".into());
        assert_eq!(f(&prepared.2, "evidence"), &Json::Null);
        for key in ["request", "expectedDefinitionHash", "origin"] {
            assert_eq!(
                hash_json(f(&prepared.2, key)).unwrap(),
                hash_json(f(intended, key)).unwrap()
            );
        }
        let operations = fixture.rows("task-board/operation");
        let guarded: Vec<_> = operations.iter().filter(|r| guard(&r.2)).collect();
        assert_eq!(guarded.len(), 1);
        let guarded = guarded[0];
        let change = f(f(&guarded.2, "request"), "change");
        assert_eq!(f(change, "kind"), &"unavailable".into());
        assert_eq!(f(change, "reason"), &"external_outcome".into());
        assert_eq!(f(change, "detail"), &"Native reconciliation is waiting: task_board_native_epoch_changed. An old turn cannot be interrupted as if the replacement owner controlled it.".into());
        let trace = peer.frames.lock().unwrap().clone();
        let switch = trace
            .iter()
            .position(|v| f(v, "kind") == &"epoch-switch".into())
            .unwrap();
        assert!(
            trace[..switch]
                .iter()
                .any(|v| f(v, "kind") == &"control-observation".into()
                    && f(f(v, "observation"), "method") == &"thread/turns/list".into()
                    && f(f(v, "observation"), "epoch") == &"epoch".into())
        );
        assert_eq!(f(&trace[switch], "previousEpoch"), &"epoch".into());
        assert_eq!(f(&trace[switch], "epoch"), &"replacement-epoch".into());
        assert_eq!(
            f(&trace[switch], "operationId"),
            f(&prepared.2, "operationId")
        );
        let absence = trace
            .iter()
            .enumerate()
            .skip(switch + 1)
            .find(|(_, v)| f(v, "kind") == &"absence".into())
            .unwrap();
        assert_eq!(
            f(f(absence.1, "absence"), "epoch"),
            &"replacement-epoch".into()
        );
        assert_eq!(
            f(f(absence.1, "absence"), "operationId"),
            f(&prepared.2, "operationId")
        );
        assert!(
            trace[absence.0 + 1..]
                .iter()
                .any(|v| f(v, "kind") == &"control-status".into()
                    && f(f(v, "status"), "epoch") == &"replacement-epoch".into())
        );
        assert_eq!(frames(&peer, "epoch-switch").len(), 1);
        assert_eq!(frames(&peer, "absence").len(), 1);
        assert_eq!(
            f(f(&run(&fixture).2, "externalOutcome"), "state"),
            &"unknown".into()
        );
        assert_review(&fixture, scenario);
        quiet();
        let replay = f(&guarded.2, "request").clone();
        let outcome = f(&guarded.2, "outcome").clone();
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
            "replacement inspection retains unresolved old turn",
            || {
                settled()
                    && f(f(&run(&fixture).2, "externalOutcome"), "code")
                        == &"native_active_owner_unconfirmed".into()
            },
        )
        .await;
        let current = control_calls(&fixture);
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].0, prepared.0);
        assert_eq!(current[0].1, 1);
        assert_eq!(
            hash_json(&current[0].2).unwrap(),
            hash_json(&prepared.2).unwrap()
        );
        assert_eq!(f(&run(&fixture).2, "nativeEpoch"), &"epoch".into());
        assert_eq!(f(&run(&fixture).2, "turnId"), &"turn".into());
        assert_review(&fixture, scenario);
        quiet();
        *scenario.replay.borrow_mut() = Some(replay);
        let revisions = count(&fixture, "SELECT count(*) FROM revisions");
        let blobs = count(&fixture, "SELECT count(*) FROM blobs");
        let notices = fixture.notices.get();
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
        assert_eq!(fixture.notices.get(), notices);
        assert_eq!(frames(&peer, "absence").len(), 1);
        assert_eq!(frames(&peer, "epoch-switch").len(), 1);
        assert_review(&fixture, scenario);
        quiet();
        bounded(service.close()).await.unwrap();
    };
    let (result, ()) = tokio::join!(owner.run(&factory), script);
    result.unwrap();
    assert_eq!(fixture.drops.borrow().as_slice(), [1, 2, 3]);
    assert_eq!(count(&fixture, "SELECT count(*) FROM active_gates"), 0);
    peer.finish().await;
}

#[tokio::test(flavor = "current_thread")]
async fn retained_interrupt_recovers_only_the_original_epoch_and_preserves_current_review() {
    let data = data();
    let original = data
        .as_array()
        .unwrap()
        .iter()
        .find(|g| f(f(f(g, "base"), "run"), "plan").get("nativeVersion") == Some(&"0.159.2".into()))
        .unwrap();
    let base = f(original, "base");
    let plan = f(f(base, "run"), "plan").clone();
    // These exact original source rows define inactive cancellation dispatch
    // and the replacement epoch refusal. The factory adds the real journal,
    // retained invocation/execution queues and reconnecting SDK transport.
    for name in [
        "inactive cancellation can interrupt",
        "interrupt refuses replacement epoch",
    ] {
        assert!(
            f(original, "rows")
                .as_array()
                .unwrap()
                .iter()
                .any(|r| f(r, "name") == &Json::from(name))
        );
    }
    let intended = f(f(base, "calls"), "interrupt");
    for replacement in [false, true] {
        let peer = Peer::start_mode(Some("--factory-control")).await;
        let mut fixture = Fixture::new(false);
        fixture.control = Some(Scenario {
            plan: plan.clone(),
            ready: Gate::default(),
            replay: RefCell::default(),
            response: RefCell::default(),
        });
        let scenario = fixture.control.as_ref().unwrap();
        let contracts = ContractsBinding;
        let catalogs =
            CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
        let factory = make_factory(&fixture, &contracts, &catalogs);
        let (service, owner) = make_service(configured(&peer), &contracts).unwrap();
        let script = async {
            service.start();
            let first = bounded(service.client.wait_ready(None)).await.unwrap();
            rpc(
                &first,
                "test.control",
                Json::object([("plan", plan.clone()), ("replacement", replacement.into())]),
            )
            .await
            .unwrap();
            scenario.ready.release();
            reached(
                &fixture,
                &peer,
                "committed interrupt with lost reply",
                || !frames(&peer, "receipt-unavailable").is_empty(),
            )
            .await;
            let prepared = control_calls(&fixture);
            assert_eq!(prepared.len(), 1);
            let prepared = &prepared[0];
            assert_eq!(prepared.1, 1);
            assert_eq!(f(&prepared.2, "state"), &"prepared".into());
            assert_eq!(f(&prepared.2, "evidence"), &Json::Null);
            for key in ["request", "expectedDefinitionHash", "origin"] {
                assert_eq!(
                    hash_json(f(&prepared.2, key)).unwrap(),
                    hash_json(f(intended, key)).unwrap(),
                    "Original interrupt intent {key}"
                );
            }
            let committed = frames(&peer, "committed-operation");
            assert_eq!(committed.len(), 1);
            assert_eq!(
                f(f(&committed[0], "operation"), "operationId"),
                f(&prepared.2, "operationId")
            );
            assert_review(&fixture, scenario);
            assert!(evidence(&fixture).is_empty());
            assert_eq!(
                f(f(&run(&fixture).2, "calls"), "interrupt"),
                &pin(&prepared.0, 1)
            );
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
            reached(&fixture, &peer, "replacement epoch decision", || {
                let rows = control_calls(&fixture);
                let outcome = run(&fixture).2;
                rows[0].1 == (if replacement { 1 } else { 2 })
                    && (!replacement
                        || f(f(&outcome, "externalOutcome"), "code")
                            == &"native_active_owner_unconfirmed".into())
                    && fixture
                        .rows("task-board/operation")
                        .iter()
                        .all(|r| f(&r.2, "phase") == &"succeeded".into())
                    && fixture.rows("task-board/operation").iter().any(|row| {
                        f(f(f(&row.2, "request"), "change"), "kind")
                            == &Json::from(if replacement {
                                "attachCall"
                            } else {
                                "observeCall"
                            })
                            && f(&row.2, "phase") == &"succeeded".into()
                    })
                    && (replacement
                        || f(f(&outcome, "calls"), "interrupt").get("revision")
                            == Some(&Json::Number(2.0)))
                    && frames(&peer, "control-observation")
                        .iter()
                        .any(|v| f(v, "generation") == &Json::Number(2.0))
            })
            .await;
            let calls = control_calls(&fixture);
            assert_eq!(calls.len(), 1);
            for key in [
                "request",
                "expectedDefinitionHash",
                "operationId",
                "origin",
                "preparedAt",
            ] {
                assert_eq!(
                    hash_json(f(&calls[0].2, key)).unwrap(),
                    hash_json(f(&prepared.2, key)).unwrap()
                );
            }
            assert_eq!(f(&run(&fixture).2, "nativeEpoch"), &"epoch".into());
            assert_eq!(f(&run(&fixture).2, "turnId"), &"turn".into());
            assert_eq!(frames(&peer, "committed-operation").len(), 1);
            assert_eq!(frames(&peer, "absence").len(), 1);
            if replacement {
                assert_eq!(f(&calls[0].2, "state"), &"prepared".into());
                assert_eq!(f(&calls[0].2, "evidence"), &Json::Null);
                assert!(evidence(&fixture).is_empty());
                assert!(frames(&peer, "original-operation").is_empty());
            } else {
                assert_eq!(f(&calls[0].2, "state"), &"observed".into());
                assert_eq!(f(&calls[0].2, "ownerPhase"), &"succeeded".into());
                assert_eq!(f(&calls[0].2, "epoch"), &"epoch".into());
                let recovered = frames(&peer, "original-operation");
                assert_eq!(recovered.len(), 1);
                assert_eq!(
                    hash_json(f(&recovered[0], "operation")).unwrap(),
                    hash_json(f(&committed[0], "operation")).unwrap()
                );
                exact_evidence(&fixture, &peer, 1, "succeeded");
            }
            assert_review(&fixture, scenario);
            let operations = fixture.rows("task-board/operation");
            let cached = operations
                .iter()
                .find(|r| {
                    f(f(f(&r.2, "request"), "change"), "kind")
                        == &Json::from(if replacement {
                            "attachCall"
                        } else {
                            "observeCall"
                        })
                })
                .unwrap();
            let outcome = f(&cached.2, "outcome").clone();
            *scenario.replay.borrow_mut() = Some(f(&cached.2, "request").clone());
            let revisions = count(&fixture, "SELECT count(*) FROM revisions");
            let blobs = count(&fixture, "SELECT count(*) FROM blobs");
            let notices = fixture.notices.get();
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
            assert_eq!(fixture.notices.get(), notices);
            assert_eq!(frames(&peer, "committed-operation").len(), 1);
            assert_review(&fixture, scenario);
            bounded(service.close()).await.unwrap();
        };
        let (result, ()) = tokio::join!(owner.run(&factory), script);
        result.unwrap();
        assert_eq!(fixture.drops.borrow().as_slice(), [1, 2, 3]);
        assert_eq!(count(&fixture, "SELECT count(*) FROM active_gates"), 0);
        peer.finish().await;
    }
}
