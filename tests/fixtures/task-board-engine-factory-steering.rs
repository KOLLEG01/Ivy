use super::*;
use control::signals;
use driver::{count, exact_evidence, frames, reached};
use ivy_task_board::engine_invocations::InvocationRequest;

type Records = Vec<(&'static str, Json, &'static str)>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Trigger {
    Work,
    ExecutionChange,
    Wake,
    Dependency,
    FutureWake,
}
pub(super) struct Scenario {
    plan: Json,
    replacement: bool,
    trigger: Trigger,
    pub(super) ready: Gate,
    prompts: Cell<u64>,
    reviewed: RefCell<Option<Records>>,
    replay: RefCell<Option<Json>>,
    response: RefCell<Option<InvocationRequest>>,
    replay_counts: Cell<(i64, i64)>,
}

fn steering_calls(fixture: &Fixture) -> Vec<(String, u64, Json)> {
    fixture
        .rows("task-board/native-call")
        .into_iter()
        .filter(|row| row.0 != driver::ATTACHMENT)
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
        let mut calls = f(&run, "calls").clone();
        calls.insert(
            "thread",
            Json::object([
                ("objectId", driver::ATTACHMENT.into()),
                ("revision", Json::Number(2.0)),
            ]),
        );
        run.insert("calls", calls);
        let mut claim = f(&task, "claim").clone();
        claim.insert("phase", "running".into());
        task.insert("claim", claim);
        if self.trigger == Trigger::Dependency {
            task.insert("dependencies", Json::Array(vec!["watched-task".into()]));
            task.insert("observedDependencies", Self::observed(0.0));
        }
        (task, run)
    }
    fn observed(work: f64) -> Json {
        Json::object([(
            "watched-task",
            hash_json(&Json::object([
                ("work", Json::Number(work)),
                ("review", Json::Null),
                ("archived", false.into()),
                ("terminal", Json::Null),
            ]))
            .unwrap()
            .into(),
        )])
    }
    fn expected_dependencies(&self) -> Json {
        if self.trigger == Trigger::Dependency {
            Self::observed(1.0)
        } else {
            Json::object(Vec::<(&str, Json)>::new())
        }
    }
    fn execution_change(&self) -> bool {
        self.trigger == Trigger::ExecutionChange
    }
    fn review_generation(&self) -> f64 {
        if self.replacement || self.trigger != Trigger::Work {
            3.0
        } else {
            2.0
        }
    }
    fn active_task(&self) -> Json {
        let mut task = self.initial().0;
        if self.execution_change() {
            task.insert("executionChangeRequested", true.into());
        } else if self.trigger == Trigger::Work {
            task.insert("workRevision", Json::Number(1.0));
        } else if self.trigger == Trigger::Wake {
            task.insert("wakeAt", clock().into());
        } else if self.trigger == Trigger::FutureWake {
            task.insert("wakeAt", "2026-01-03T03:04:05.006Z".into());
        }
        task
    }
    fn activate(&self, fixture: &Fixture) {
        let database = fixture.database();
        let journal = ivy_task_board::local_journal::LocalJournal {
            connection: &database,
            root_object_id: None,
        };
        if self.trigger == Trigger::Dependency {
            let mut watched = journal
                .read("task-board/task", &"watched-task".into(), true)
                .unwrap()
                .value;
            watched.insert("workRevision", Json::Number(1.0));
            admission("Task", &watched).unwrap();
            journal
                .update(
                    "task-board/task",
                    &"watched-task".into(),
                    1.0,
                    &watched,
                    &clock().into(),
                )
                .unwrap();
        }
        let active = self.active_task();
        admission("Task", &active).unwrap();
        journal
            .update(
                "task-board/task",
                &"task".into(),
                1.0,
                &active,
                &clock().into(),
            )
            .unwrap();
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
        execution_change: bool,
    ) -> IvyResult<JsString> {
        assert_eq!(
            hash_json(task).unwrap(),
            hash_json(&self.active_task()).unwrap()
        );
        assert_eq!(owner, &JsString::from("fixture-task-board"));
        assert_eq!(execution_change, self.execution_change());
        self.prompts.set(self.prompts.get() + 1);
        ivy_task_board::prompts::CurrentTaskPrompts.task_update_prompt(
            task,
            owner,
            execution_change,
        )
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
            if self.trigger == Trigger::Dependency {
                let mut watched = task.clone();
                watched.insert("taskKey", "TASK-0002".into());
                watched.insert("dependencies", Json::Array(vec![]));
                watched.insert(
                    "observedDependencies",
                    Json::object(Vec::<(&str, Json)>::new()),
                );
                for key in ["claim", "primaryResourceRef", "lastRun"] {
                    watched.insert(key, Json::Null);
                }
                watched.insert("attemptCount", Json::Number(0.0));
                let mut fields = f(&watched, "fields").clone();
                fields.insert("control", "user".into());
                watched.insert("fields", fields);
                Self::create(context, "task-board/task", &watched, "watched-task")?;
            }
            journal.connection.execute(
                "INSERT INTO workspace_reservations VALUES('fixture-native-host','/fixture','task','current-workspace')", [],
            ).unwrap();
        }
        if context.owner.generation == self.review_generation() {
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
                ("runtimeWait", Json::Null),
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
async fn factory_readiness_reports_native_silence_and_matching_activity_without_changing_claims() {
    let data = control::data();
    let original = data
        .as_array()
        .unwrap()
        .iter()
        .find(|g| f(f(f(g, "base"), "run"), "plan").get("nativeVersion") == Some(&"0.159.2".into()))
        .unwrap();
    let plan = f(f(f(original, "base"), "run"), "plan").clone();
    let peer = Peer::start_mode(Some("--factory-control")).await;
    let mut fixture = Fixture::new(false);
    fixture.clock = retention::FactoryClock::manual();
    // Start beyond the generation's initial 250ms tick throttle.
    fixture.clock.advance(1000.0);
    fixture.steering = Some(Scenario {
        plan: plan.clone(),
        replacement: false,
        trigger: Trigger::FutureWake,
        ready: Gate::default(),
        prompts: Cell::new(0),
        reviewed: RefCell::default(),
        replay: RefCell::default(),
        response: RefCell::default(),
        replay_counts: Cell::new((0, 0)),
    });
    let contracts = ContractsBinding;
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let factory = make_factory(&fixture, &contracts, &catalogs);
    let mut setup = configured(&peer);
    let mut scheduler = f(&setup.settings, "scheduler").clone();
    scheduler.insert("stallWarningMs", Json::Number(60_000.0));
    setup.settings.insert("scheduler", scheduler);
    let (service, owner) = make_service(setup, &contracts).unwrap();
    let script = async {
        service.start();
        let first = bounded(service.client.wait_ready(None)).await.unwrap();
        rpc(
            &first,
            "test.control",
            Json::object([
                ("plan", plan.clone()),
                ("replacement", false.into()),
                ("steering", true.into()),
            ]),
        )
        .await
        .unwrap();
        fixture.steering.as_ref().unwrap().ready.release();
        // Recovery traverses separate lanes on scheduled ticks. Drive those
        // ticks before measuring elapsed time for an observed native run.
        retention::drive(&fixture, || {
            signals(&fixture)
                .iter()
                .any(|v| f(v, "epoch") == &"epoch".into())
        })
        .await;
        let initial = bounded(service.control.readiness(&first)).await.unwrap();
        assert_eq!(f(&initial, "ready"), &true.into());
        let task_before = fixture.rows("task-board/task");
        let run_before = fixture.rows("task-board/run");
        let revisions_before = count(&fixture, "SELECT count(*) FROM revisions");
        fixture.clock.advance(60_000.0);
        let silent = bounded(service.control.readiness(&first)).await.unwrap();
        assert_eq!(f(&silent, "ready"), &true.into());
        let diagnostics = f(&silent, "diagnostics").as_array().unwrap();
        assert_eq!(diagnostics.len(), 1);
        let warning = &diagnostics[0];
        assert_eq!(f(warning, "code"), &"task_board_native_silent".into());
        assert_eq!(
            f(warning, "resource"),
            &Json::object([("objectId", "task".into())])
        );
        assert_eq!(f(warning, "source"), &"fixture-task-board".into());
        let first_observed = f(warning, "firstObservedAt").clone();
        let before = frames(&peer, "control-observation").len();
        retention::drive(&fixture, || {
            frames(&peer, "control-observation").len() >= before + 2
        })
        .await;
        let stable = bounded(service.control.readiness(&first)).await.unwrap();
        let diagnostics = f(&stable, "diagnostics").as_array().unwrap();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(f(&diagnostics[0], "firstObservedAt"), &first_observed);
        // Native identity is conjunctive. A status-only notice wakes a read but
        // cannot turn an unchanged native status into observed activity.
        let notice = |node: &str, thread: &str, method: Json| {
            Json::object([(
                "params",
                Json::object([
                    ("serviceNodeId", node.into()),
                    (
                        "payload",
                        Json::object([
                            ("method", method),
                            ("params", Json::object([("threadId", thread.into())])),
                        ]),
                    ),
                ]),
            )])
        };
        for value in [
            notice("other-agent", "thread", "item/delta".into()),
            notice("native-agent", "other-thread", "item/delta".into()),
            notice("native-agent", "thread", "thread/status/changed".into()),
        ] {
            service.control.notification(&value);
        }
        let before = frames(&peer, "control-observation").len();
        service.control.notification(&notice(
            "native-agent",
            "thread",
            "thread/status/changed".into(),
        ));
        retention::drive(&fixture, || {
            frames(&peer, "control-observation").len() >= before + 2
        })
        .await;
        let unchanged = bounded(service.control.readiness(&first)).await.unwrap();
        let diagnostics = f(&unchanged, "diagnostics").as_array().unwrap();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(f(&diagnostics[0], "firstObservedAt"), &first_observed);
        // The activity prefix must also work with opaque UTF-16 suffixes.
        let mut method: JsString = "item/".into();
        method.0.push(0xd800);
        service
            .control
            .notification(&notice("native-agent", "thread", Json::String(method)));
        bounded(async {
            loop {
                let ready = service.control.readiness(&first).await.unwrap();
                if f(&ready, "diagnostics").as_array().unwrap().is_empty() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        let activity_at = ivy_task_board::generation::GenerationClock::now_millis(&fixture.clock);
        fixture.clock.advance(59_999.0);
        assert_eq!(
            f(
                &bounded(service.control.readiness(&first)).await.unwrap(),
                "diagnostics"
            ),
            &Json::Array(vec![])
        );
        fixture.clock.advance(1.0);
        let elapsed = bounded(service.control.readiness(&first)).await.unwrap();
        let diagnostics = f(&elapsed, "diagnostics").as_array().unwrap();
        assert_eq!(diagnostics.len(), 1);
        let expected_time =
            Json::String(ivy_sdk::clock::date_to_iso(activity_at + 60_000.0).unwrap());
        assert_eq!(f(&diagnostics[0], "firstObservedAt"), &expected_time);
        assert_eq!(f(&diagnostics[0], "lastObservedAt"), &expected_time);
        for method in ["turn/started", "turn/completed", "serverRequest/resolved"] {
            service
                .control
                .notification(&notice("native-agent", "thread", method.into()));
            bounded(async {
                loop {
                    let ready = service.control.readiness(&first).await.unwrap();
                    if f(&ready, "diagnostics").as_array().unwrap().is_empty() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await;
            fixture.clock.advance(60_000.0);
            assert_eq!(
                f(
                    &bounded(service.control.readiness(&first)).await.unwrap(),
                    "diagnostics"
                )
                .as_array()
                .unwrap()
                .len(),
                1
            );
        }
        assert_eq!(fixture.rows("task-board/task"), task_before);
        assert_eq!(fixture.rows("task-board/run"), run_before);
        assert_eq!(
            count(&fixture, "SELECT count(*) FROM revisions"),
            revisions_before
        );
        assert!(steering_calls(&fixture).is_empty());
        assert!(frames(&peer, "native-invoke").is_empty());
        assert!(fixture.rows("task-board/result").is_empty());
        bounded(service.close()).await.unwrap();
    };
    let (result, ()) = tokio::join!(owner.run(&factory), script);
    result.unwrap();
    assert_eq!(fixture.drops.borrow().as_slice(), &[1]);
    peer.finish().await;
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
    for (replacement, trigger) in [
        Trigger::Work,
        Trigger::ExecutionChange,
        Trigger::Wake,
        Trigger::Dependency,
    ]
    .into_iter()
    .flat_map(|trigger| [false, true].map(|replacement| (replacement, trigger)))
    .chain(std::iter::once((false, Trigger::FutureWake)))
    {
        let peer = Peer::start_mode(Some("--factory-control")).await;
        let mut fixture = Fixture::new(false);
        fixture.steering = Some(Scenario {
            plan: plan.clone(),
            replacement,
            trigger,
            ready: Gate::default(),
            prompts: Cell::new(0),
            reviewed: RefCell::default(),
            replay: RefCell::default(),
            response: RefCell::default(),
            replay_counts: Cell::new((0, 0)),
        });
        let scenario = fixture.steering.as_ref().unwrap();
        let execution_change = scenario.execution_change();
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
                    (
                        "steerPrompt",
                        Json::String(
                            ivy_task_board::prompts::CurrentTaskPrompts
                                .task_update_prompt(
                                    &scenario.active_task(),
                                    &"fixture-task-board".into(),
                                    execution_change,
                                )
                                .unwrap(),
                        ),
                    ),
                ]),
            )
            .await
            .unwrap();
            scenario.ready.release();
            reached(
                &fixture,
                &peer,
                "initial native signals before the steering trigger",
                || {
                    signals(&fixture)
                        .iter()
                        .any(|value| f(value, "epoch") == &"epoch".into())
                },
            )
            .await;
            scenario.activate(&fixture);
            if trigger == Trigger::FutureWake {
                let before = frames(&peer, "control-observation").len();
                reached(
                    &fixture,
                    &peer,
                    "future appointment remains unconsumed across active polls",
                    || frames(&peer, "control-observation").len() >= before + 4,
                )
                .await;
                assert!(steering_calls(&fixture).is_empty());
                assert!(frames(&peer, "native-invoke").is_empty());
                assert_eq!(scenario.prompts.get(), 0);
                assert_eq!(
                    f(&fixture.rows("task-board/task")[0].2, "wakeAt"),
                    &"2026-01-03T03:04:05.006Z".into()
                );
                assert_eq!(f(&run(&fixture).2, "nativeEpoch"), &"epoch".into());
                assert_eq!(f(&run(&fixture).2, "turnId"), &"turn".into());
                bounded(service.close()).await.unwrap();
                return;
            }
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
            let calls = steering_calls(&fixture);
            assert_eq!(calls.len(), 1);
            let prepared = calls[0].clone();
            assert_eq!(prepared.1, 1);
            assert_eq!(f(&prepared.2, "state"), &"prepared".into());
            assert_eq!(f(&prepared.2, "evidence"), &Json::Null);
            admission("NativeCall", &prepared.2).unwrap();
            let catalog = catalogs
                .checked_native_contract(
                    f(&plan, "nativeVersion").as_js_string().unwrap(),
                    Some(&Json::object([(
                        "sourceHash",
                        f(&plan, "catalogSourceHash").clone(),
                    )])),
                )
                .unwrap();
            assert_eq!(
                f(&prepared.2, "expectedDefinitionHash"),
                &Json::from(hash_json(catalog.definition(&"turn/steer".into()).unwrap()).unwrap())
            );
            let request = f(&prepared.2, "request");
            let params = ivy_sdk::native_parameters::read_native_parameters(
                &catalog,
                &"turn/steer".into(),
                f(request, "params"),
                &[],
                None,
            )
            .unwrap();
            let expected_params = Json::object([
                ("threadId", "thread".into()),
                ("expectedTurnId", "turn".into()),
                (
                    "input",
                    Json::Array(vec![Json::object([
                        ("type", "text".into()),
                        (
                            "text",
                            Json::String(
                                ivy_task_board::prompts::CurrentTaskPrompts
                                    .task_update_prompt(
                                        &scenario.active_task(),
                                        &"fixture-task-board".into(),
                                        execution_change,
                                    )
                                    .unwrap(),
                            ),
                        ),
                        ("text_elements", Json::Array(vec![])),
                    ])]),
                ),
            ]);
            assert_eq!(
                hash_json(&params).unwrap(),
                hash_json(&expected_params).unwrap()
            );
            let mut expected_origin = Json::object([
                ("kind", "steer".into()),
                ("commentIds", Json::Array(vec![])),
                ("observedDependencies", scenario.expected_dependencies()),
                (
                    "wakeAt",
                    if trigger == Trigger::Wake {
                        clock().into()
                    } else {
                        Json::Null
                    },
                ),
                (
                    "workRevision",
                    Json::Number(if trigger == Trigger::Work { 1.0 } else { 0.0 }),
                ),
                ("peersHash", hash_json(&Json::Array(vec![])).unwrap().into()),
                ("predecessor", Json::Null),
            ]);
            if execution_change {
                expected_origin.insert("executionChangeRequested", true.into());
            }
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
                    let calls = steering_calls(&fixture);
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
            let call = &steering_calls(&fixture)[0].2;
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
            } else if scenario.review_generation() == 2.0 {
                scenario.assert_review(&fixture);
            } else {
                let task = &fixture
                    .rows("task-board/task")
                    .into_iter()
                    .find(|row| row.0 == "task")
                    .unwrap()
                    .2;
                assert_eq!(
                    f(task, "observedDependencies"),
                    &scenario.expected_dependencies()
                );
                assert_eq!(f(task, "wakeAt"), &Json::Null);
                assert_eq!(f(task, "workflowState"), &"in_progress".into());
                let before = frames(&peer, "control-observation").len();
                reached(
                    &fixture,
                    &peer,
                    "one unchanged active poll after steering acknowledgement",
                    || frames(&peer, "control-observation").len() >= before + 2,
                )
                .await;
                assert_eq!(
                    steering_calls(&fixture).len(),
                    1,
                    "A consumed {trigger:?} must not prepare another steer"
                );
                assert_eq!(scenario.prompts.get(), 2);
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
        assert_eq!(
            fixture.drops.borrow().as_slice(),
            if trigger == Trigger::FutureWake {
                &[1][..]
            } else {
                &[1, 2, 3][..]
            }
        );
        assert_eq!(count(&fixture, "SELECT count(*) FROM active_gates"), 0);
        peer.finish().await;
    }
}
