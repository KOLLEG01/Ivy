use super::*;
use control::signals;
use driver::{count, exact_evidence, frames, reached};
use ivy_task_board::engine_invocations::InvocationRequest;

type Records = Vec<(&'static str, Json, &'static str)>;
pub(super) struct Scenario {
    plan: Json,
    replacement: bool,
    pub(super) ready: Gate,
    prompts: Cell<u64>,
    reviewed: RefCell<Option<Records>>,
    replay: RefCell<Option<Json>>,
    response: RefCell<Option<InvocationRequest>>,
    replay_counts: Cell<(i64, i64)>,
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
fn run(fixture: &Fixture) -> (String, u64, Json) {
    fixture
        .rows("task-board/run")
        .into_iter()
        .find(|r| r.0 == "run")
        .unwrap()
}
fn settled(fixture: &Fixture) -> bool {
    fixture
        .rows("task-board/operation")
        .iter()
        .all(|r| f(&r.2, "phase") == &"succeeded".into())
}
impl Scenario {
    fn initial(&self) -> (Json, Json) {
        let (mut task, mut run) = native::baseline();
        run.insert("phase", "running".into());
        run.insert("plan", self.plan.clone());
        let mut claim = f(&task, "claim").clone();
        claim.insert("phase", "running".into());
        task.insert("claim", claim);
        (task, run)
    }
    fn active_task(&self) -> Json {
        let mut task = self.initial().0;
        task.insert("workRevision", Json::Number(1.0));
        task
    }
    pub(super) fn workflow_state(&self) -> Json {
        if self.reviewed.borrow().is_some() {
            "review".into()
        } else {
            "in_progress".into()
        }
    }
    pub(super) fn prompt(
        &self,
        task: &Json,
        owner: &JsString,
        coordination: bool,
    ) -> IvyResult<JsString> {
        assert_eq!(
            hash_json(task).unwrap(),
            hash_json(&self.active_task()).unwrap()
        );
        assert_eq!(owner, &JsString::from("fixture-task-board"));
        assert!(!coordination);
        self.prompts.set(self.prompts.get() + 1);
        Ok("advance prompt".into())
    }
    fn create(context: &Context<'_>, key: &str, value: &Json, id: &str) -> IvyResult<()> {
        let version = context.journal.contracts.version(key)?;
        context
            .journal
            .contracts
            .validate_record(key, &version.into(), value)?;
        context.journal.journal.create(
            key,
            &id.into(),
            None,
            value,
            &clock().into(),
            &id.into(),
        )?;
        Ok(())
    }
    pub(super) fn bind(&self, context: &Context<'_>) -> IvyResult<()> {
        let journal = &context.journal.journal;
        if context.owner.generation == 1.0 {
            let (task, run) = self.initial();
            Self::create(context, "task-board/task", &task, "task")?;
            Self::create(context, "task-board/run", &run, "run")?;
            let active = self.active_task();
            admission("Task", &active)?;
            journal.update(
                "task-board/task",
                &"task".into(),
                1.0,
                &active,
                &clock().into(),
            )?;
            journal.connection.execute(
                "INSERT INTO workspace_reservations VALUES('fixture-native-host','/fixture','task','current-workspace')", [],
            ).unwrap();
        }
        if context.owner.generation == if self.replacement { 3.0 } else { 2.0 } {
            // An externally supplied admitted snapshot enters between owner
            // graphs. This finite setup does not qualify its publication action.
            let mut records = retirement::reviewed_records();
            for (key, value, id) in &records[..2] {
                Self::create(context, key, value, id)?;
            }
            let current = journal.read("task-board/task", &"task".into(), true)?;
            let mut task = current.value;
            for (key, value) in [
                ("workflowState", "review".into()),
                ("claim", Json::Null),
                ("waiting", Json::Null),
                ("latestResult", pin("current-result", 1)),
                ("lastRun", pin("newer-run", 1)),
                ("attemptCount", Json::Number(2.0)),
            ] {
                task.insert(key, value);
            }
            admission("Task", &task)?;
            journal.update(
                "task-board/task",
                &"task".into(),
                f(&current.pin, "revision").as_f64().unwrap(),
                &task,
                &clock().into(),
            )?;
            records[2].1 = journal.read("task-board/run", &"run".into(), true)?.value;
            records[3].1 = task;
            *self.reviewed.borrow_mut() = Some(records);
        }
        if context.owner.generation == 3.0 {
            let revisions = journal
                .connection
                .query_row("SELECT count(*) FROM revisions", [], |r| r.get(0))
                .unwrap();
            let blobs = journal
                .connection
                .query_row("SELECT count(*) FROM blobs", [], |r| r.get(0))
                .unwrap();
            self.replay_counts.set((revisions, blobs));
            *self.response.borrow_mut() = Some(
                context
                    .calls
                    .native_update(self.replay.borrow().as_ref().unwrap())?,
            );
        }
        Ok(())
    }
    fn assert_review(&self, fixture: &Fixture) {
        control::assert_review_records(fixture, self.reviewed.borrow().as_ref().unwrap());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn steering_recovers_lost_receipts_or_prevents_replacement_input_without_replacing_review() {
    let data = control::data();
    let original = data
        .as_array()
        .unwrap()
        .iter()
        .find(|g| f(f(f(g, "base"), "run"), "plan").get("nativeVersion") == Some(&"0.159.2".into()))
        .unwrap();
    for name in [
        "steer dispatch preserves turn identity",
        "steer replacement epoch prevents input",
    ] {
        assert!(
            f(original, "rows")
                .as_array()
                .unwrap()
                .iter()
                .any(|r| f(r, "name") == &Json::from(name))
        );
    }
    let base = f(original, "base");
    let plan = f(f(base, "run"), "plan").clone();
    let intended = f(f(base, "calls"), "steer");
    for replacement in [false, true] {
        let peer = Peer::start_mode(Some("--factory-control")).await;
        let mut fixture = Fixture::new(false);
        fixture.steering = Some(Scenario {
            plan: plan.clone(),
            replacement,
            ready: Gate::default(),
            prompts: Cell::new(0),
            reviewed: RefCell::default(),
            replay: RefCell::default(),
            response: RefCell::default(),
            replay_counts: Cell::new((0, 0)),
        });
        let scenario = fixture.steering.as_ref().unwrap();
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
                Json::object([
                    ("plan", plan.clone()),
                    ("replacement", replacement.into()),
                    ("steering", true.into()),
                ]),
            )
            .await
            .unwrap();
            scenario.ready.release();
            let stage = if replacement {
                "owner-unavailable"
            } else {
                "receipt-unavailable"
            };
            reached(
                &fixture,
                &peer,
                "retained steering preparation and uncertain owner",
                || !frames(&peer, stage).is_empty() && settled(&fixture),
            )
            .await;
            let calls = fixture.rows("task-board/native-call");
            assert_eq!(calls.len(), 1);
            let prepared = calls[0].clone();
            assert_eq!(prepared.1, 1);
            assert_eq!(f(&prepared.2, "state"), &"prepared".into());
            assert_eq!(f(&prepared.2, "evidence"), &Json::Null);
            for key in ["request", "expectedDefinitionHash"] {
                assert_eq!(
                    hash_json(f(&prepared.2, key)).unwrap(),
                    hash_json(f(intended, key)).unwrap(),
                    "Original steer intent {key}"
                );
            }
            let expected_origin = Json::object([
                ("kind", "steer".into()),
                ("commentIds", Json::Array(vec![])),
                ("workRevision", Json::Number(1.0)),
                ("peersHash", hash_json(&Json::Array(vec![])).unwrap().into()),
                ("predecessor", Json::Null),
            ]);
            assert_eq!(
                hash_json(f(&prepared.2, "origin")).unwrap(),
                hash_json(&expected_origin).unwrap()
            );
            assert_eq!(
                f(f(&run(&fixture).2, "calls"), "steer"),
                &pin(&prepared.0, 1)
            );
            assert_eq!(
                scenario.prompts.get(),
                2,
                "Original prepare and publication verification render current steering intent"
            );
            assert_eq!(
                frames(&peer, "native-invoke").len(),
                if replacement { 0 } else { 1 }
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
            reached(
                &fixture,
                &peer,
                "original steering observation or exact prevention",
                || {
                    let calls = fixture.rows("task-board/native-call");
                    calls[0].1 == 2
                        && f(f(&run(&fixture).2, "calls"), "steer") == &pin(&prepared.0, 2)
                        && settled(&fixture)
                        && (!replacement
                            || f(f(&run(&fixture).2, "externalOutcome"), "code")
                                == &"native_active_owner_unconfirmed".into())
                        && signals(&fixture).iter().any(|v| {
                            f(v, "epoch")
                                == &Json::from(if replacement {
                                    "replacement-epoch"
                                } else {
                                    "epoch"
                                })
                        })
                },
            )
            .await;
            let checkpoint = signals(&fixture);
            assert_eq!(checkpoint.len(), 1);
            assert_eq!(f(&checkpoint[0], "inputs"), &Json::Array(vec![]));
            assert_eq!(f(&checkpoint[0], "activity"), &Json::Array(vec![]));
            assert_eq!(f(&checkpoint[0], "notificationCursor"), &Json::Number(0.0));
            assert_eq!(
                f(&checkpoint[0], "notificationGap"),
                &Json::Bool(replacement)
            );
            let call = &fixture.rows("task-board/native-call")[0].2;
            for key in [
                "request",
                "expectedDefinitionHash",
                "operationId",
                "origin",
                "preparedAt",
            ] {
                assert_eq!(
                    hash_json(f(call, key)).unwrap(),
                    hash_json(f(&prepared.2, key)).unwrap()
                );
            }
            let phase = if replacement { "failed" } else { "succeeded" };
            assert_eq!(f(call, "state"), &"observed".into());
            assert_eq!(f(call, "ownerPhase"), &Json::from(phase));
            let expected_epoch = if replacement {
                Json::Null
            } else {
                "epoch".into()
            };
            let expected_code = if replacement {
                "request_prevented".into()
            } else {
                Json::Null
            };
            assert_eq!(f(call, "epoch"), &expected_epoch);
            assert_eq!(f(call, "code"), &expected_code);
            let committed = frames(&peer, "committed-operation");
            assert_eq!(committed.len(), 1);
            assert_eq!(
                f(f(&committed[0], "operation"), "operationId"),
                f(call, "operationId")
            );
            assert_eq!(
                frames(&peer, "native-invoke").len(),
                if replacement { 0 } else { 1 }
            );
            assert_eq!(
                frames(&peer, "prevented-operation").len(),
                if replacement { 1 } else { 0 }
            );
            assert_eq!(
                frames(&peer, "original-operation").len(),
                if replacement { 0 } else { 1 }
            );
            exact_evidence(&fixture, &peer, 1, phase);
            for key in ["nativeEpoch", "turnId"] {
                assert_eq!(f(&run(&fixture).2, key), f(&scenario.initial().1, key));
            }
            if replacement {
                // Claim is still active: the changed epoch, independently of
                // retirement, must be what prevents input to the old turn.
                let task = &fixture.rows("task-board/task")[0].2;
                assert_eq!(f(f(f(task, "claim"), "run"), "objectId"), &"run".into());
                assert_eq!(f(task, "workflowState"), &"in_progress".into());
                assert!(fixture.rows("task-board/result").is_empty());
            } else {
                scenario.assert_review(&fixture);
            }
            let operations = fixture.rows("task-board/operation");
            let cached = operations
                .iter()
                .find(|r| f(f(f(&r.2, "request"), "change"), "kind") == &"observeCall".into())
                .unwrap();
            let outcome = f(&cached.2, "outcome").clone();
            *scenario.replay.borrow_mut() = Some(f(&cached.2, "request").clone());
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
            let (revisions, blobs) = scenario.replay_counts.get();
            assert_eq!(count(&fixture, "SELECT count(*) FROM revisions"), revisions);
            assert_eq!(count(&fixture, "SELECT count(*) FROM blobs"), blobs);
            assert_eq!(fixture.notices.get(), notices);
            assert_eq!(scenario.prompts.get(), 2);
            scenario.assert_review(&fixture);
            assert_eq!(frames(&peer, "committed-operation").len(), 1);
            assert_eq!(
                frames(&peer, "native-invoke").len(),
                if replacement { 0 } else { 1 }
            );
            exact_evidence(&fixture, &peer, 1, phase);
            bounded(service.close()).await.unwrap();
        };
        let (result, ()) = tokio::join!(owner.run(&factory), script);
        result.unwrap();
        assert_eq!(fixture.drops.borrow().as_slice(), [1, 2, 3]);
        assert_eq!(count(&fixture, "SELECT count(*) FROM active_gates"), 0);
        peer.finish().await;
    }
}
