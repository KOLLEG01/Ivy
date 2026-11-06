use super::*;

// The historical corpus remains a finite observation source. Its old hashes are
// authoritative only for unaffected cases; current attachment/steering/retry
// decisions have explicit assertions below, not regenerated golden outputs.
fn attached(row: &Json) -> Json {
    let mut row = row.clone();
    let mut run = f(&row, "run").clone();
    let mut value = f(&run, "value").clone();
    let mut calls = f(&value, "calls").clone();
    assert!(f(&calls, "thread").is_null());
    calls.insert("thread", pin(300, 2.0));
    value.insert("calls", calls);
    run.insert("value", value.clone());
    row.insert("run", run);
    let mut calls = f(&row, "calls").clone();
    calls.insert(
        id(300),
        Json::object([
            ("pin", pin(300, 2.0)),
            (
                "value",
                Json::object([
                    ("ownerPhase", "succeeded".into()),
                    ("epoch", "epoch".into()),
                    ("origin", Json::Null),
                ]),
            ),
        ]),
    );
    row.insert("calls", calls);
    current_task(&row)
}
fn current_task(row: &Json) -> Json {
    let mut row = row.clone();
    let mut task = f(&row, "task").clone();
    let mut value = f(&task, "value").clone();
    value.insert("dependencies", Json::Array(vec![]));
    task.insert("value", value);
    row.insert("task", task);
    row
}

async fn actions(world: &World<'_>, catalogs: &CheckedNativeContracts) -> Vec<Json> {
    let subject = NativeOrchestrator::new(world, world, world, catalogs, progress_tracker());
    let mut results = vec![];
    for action in f(world.row, "actions").as_array().unwrap() {
        let action = action.as_array().unwrap();
        let run = id(action.get(1).and_then(Json::as_f64).unwrap_or(200.0) as u64);
        let result: IvyResult<Json> = match action[0].as_str().unwrap().as_str() {
            "step" => subject.step(&run).await.map(|_| Json::Null),
            "drain" => subject.drain(&run).await.map(Json::Bool),
            "notification" => {
                // Historical hints omitted mandatory native event identity.
                // Supply the original observed owner without changing outcomes.
                let mut notice = action[1].clone();
                let mut params = f(&notice, "params").clone();
                let mut payload = f(&params, "payload").clone();
                payload.insert("serviceNodeId", f(&params, "serviceNodeId").clone());
                payload.insert("epoch", f(f(world.row, "context"), "epoch").clone());
                let mut body = f(&payload, "params").clone();
                body.insert("turnId", f(f(world.row, "context"), "turnId").clone());
                payload.insert("params", body);
                params.insert("payload", payload);
                notice.insert("params", params);
                Ok(subject.notification(&notice).into())
            }
            "clock" => {
                world.now.set(action[1].as_f64().unwrap());
                Ok(Json::Null)
            }
            "fault" => {
                world.fault.replace(action[1].clone());
                Ok(Json::Null)
            }
            "many" => {
                let mut result = Ok(Json::Null);
                for number in 201..201 + action[1].as_f64().unwrap() as u64 {
                    if let Err(error) = subject.drain(&id(number)).await {
                        result = Err(error);
                        break;
                    }
                }
                result
            }
            _ => panic!("Unknown finite action"),
        };
        results.push(outcome(result));
    }
    results
}

fn same(actual: &Json, expected: &Json) {
    assert_eq!(
        canonical(actual, MANAGEMENT_FRAME_BYTES).unwrap(),
        canonical(expected, MANAGEMENT_FRAME_BYTES).unwrap()
    );
}
fn methods(world: &World<'_>, expected: &[&str]) {
    let actual: Vec<String> = world
        .log
        .borrow()
        .iter()
        .map(|v| v.as_array().unwrap()[0].as_str().unwrap())
        .collect();
    assert_eq!(
        actual,
        expected,
        "{}",
        s(world.row, "name").to_utf8().unwrap()
    );
}
fn event(world: &World<'_>, method: &str) -> Json {
    let found: Vec<_> = world
        .log
        .borrow()
        .iter()
        .filter(|v| v.as_array().unwrap()[0] == Json::from(method))
        .cloned()
        .collect();
    assert_eq!(found.len(), 1, "Expected one {method}");
    found[0].clone()
}
fn reads(world: &World<'_>, expected: Vec<(&str, Json)>) {
    let actual: Vec<_> = world
        .log
        .borrow()
        .iter()
        .filter(|v| v.as_array().unwrap()[0] == Json::from("read"))
        .cloned()
        .collect();
    same(
        &Json::Array(actual),
        &Json::Array(
            expected
                .into_iter()
                .map(|(key, address)| {
                    Json::Array(vec!["read".into(), key.into(), address, true.into()])
                })
                .collect(),
        ),
    );
}
fn prepare(world: &World<'_>, slot: &str, origin: Json) {
    same(
        &event(world, "prepare"),
        &Json::Array(vec![
            "prepare".into(),
            Json::String(id(200)),
            slot.into(),
            origin,
        ]),
    );
}
fn snapshot(world: &World<'_>) {
    same(
        &event(world, "save-snapshot"),
        &Json::Array(vec![
            "save-snapshot".into(),
            pin(200, 2.0),
            f(world.row, "metadata").clone(),
            f(world.row, "turns").clone(),
        ]),
    );
    let task = pin(100, 2.0);
    let run = pin(200, 2.0);
    let change = Json::object([
        ("kind", "readSnapshot".into()),
        ("snapshot", f(data(), "artifact").clone()),
        ("notificationCursor", Json::Number(7.0)),
    ]);
    let operation = ivy_task_board::operations::mutation(
        &id(200),
        &format!(
            "reconcile:{}",
            hash_json(&Json::object([
                ("run", run.clone()),
                ("task", task),
                ("change", change.clone())
            ]))
            .unwrap()
        ),
    )
    .unwrap();
    same(
        &event(world, "update"),
        &Json::Array(vec![
            "update".into(),
            Json::object([
                ("action", "nativeUpdate".into()),
                ("operationId", operation.into()),
                ("taskId", Json::String(id(100))),
                ("expectedRevision", Json::Number(2.0)),
                ("run", run),
                ("change", change),
            ]),
        ]),
    );
}

#[tokio::test]
async fn routed_notifications_follow_observed_owner_epoch_account_and_exact_turn() {
    use ivy_contracts::native_backend::{BackendIdentity, NativeProvider};
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let source = data()
        .required("rows")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|row| f(row, "name").is_str("unchanged active poll writes no evidence"))
        .unwrap();
    for (provider, version) in [
        (NativeProvider::Codex, "0.163.0"),
        (NativeProvider::Claude, "0.142.3"),
    ] {
        for scoped in [false, true] {
            let backend = BackendIdentity::new("primary".into(), provider, "one".into()).unwrap();
            let foreign = BackendIdentity::new("primary".into(), provider, "two".into()).unwrap();
            let contract = catalogs
                .checked_native_contract(&version.into(), None)
                .unwrap();
            let mut row = attached(source);
            let mut run = f(&row, "run").clone();
            let mut value = f(&run, "value").clone();
            let mut original = f(&value, "target").clone();
            let mut plan = f(&value, "plan").clone();
            plan.insert("nativeVersion", version.into());
            plan.insert(
                "catalogSourceHash",
                contract.catalog().required("sourceHash").unwrap().clone(),
            );
            if scoped {
                original = backend.arguments(original).unwrap();
                plan = backend.arguments(plan).unwrap();
                let mut primary = f(&value, "primaryResourceRef").clone();
                primary.insert("namespace", backend.id().into());
                value.insert("primaryResourceRef", primary);
            }
            value.insert("target", original.clone());
            value.insert("plan", plan);
            run.insert("value", value.clone());
            row.insert("run", run);
            let target = |node: &str| {
                backend
                    .arguments(Json::object([
                        ("hostId", f(&original, "hostId").clone()),
                        ("serviceNodeId", node.into()),
                    ]))
                    .unwrap()
            };
            let next = target("next");
            let route_task = |mut task: Json, selected: &Json| {
                task.insert(
                    "nativeRoutes",
                    Json::Array(vec![Json::object([
                        ("source", original.clone()),
                        ("target", selected.clone()),
                    ])]),
                );
                task
            };
            let mut task = f(&row, "task").clone();
            task.insert("value", route_task(f(&task, "value").clone(), &next));
            row.insert("task", task);
            let observations = |node: &str, epoch: &str| {
                let update = |mut observation: Json| {
                    observation.insert("serviceNodeId", node.into());
                    observation.insert("epoch", epoch.into());
                    observation.insert("nativeVersion", version.into());
                    observation.insert(
                        "nativeExecutableHash",
                        contract
                            .catalog()
                            .required("nativeExecutableHash")
                            .unwrap()
                            .clone(),
                    );
                    observation.insert("catalogHash", contract.catalog_hash().into());
                    backend.arguments(observation).unwrap()
                };
                (
                    update(f(&row, "metadata").clone()),
                    update(f(&row, "turns").clone()),
                )
            };
            let world = World::new(&row);
            *world.find_observations.borrow_mut() = Some(observations("next", "epoch"));
            let subject =
                NativeOrchestrator::new(&world, &world, &world, &catalogs, progress_tracker());
            let notice = |node: &str,
                          selected: Option<&BackendIdentity>,
                          epoch: &str,
                          thread: &str,
                          turn: Option<&str>,
                          method: &str| {
                let mut body = Json::object([("threadId", thread.into())]);
                if let Some(turn) = turn {
                    body.insert("turnId", turn.into());
                }
                let event = BackendIdentity::select_arguments(
                    selected,
                    Json::object([
                        ("sequence", Json::Number(1.0)),
                        ("serviceNodeId", node.into()),
                        ("nativeVersion", version.into()),
                        ("epoch", epoch.into()),
                        ("method", method.into()),
                        ("params", body),
                        ("observedAt", "2026-01-02T03:04:05.006Z".into()),
                    ]),
                )
                .unwrap();
                Json::object([(
                    "params",
                    Json::object([("serviceNodeId", node.into()), ("payload", event)]),
                )])
            };
            assert!(subject.drain(&id(200)).await.unwrap());
            let correct = notice(
                "next",
                Some(&backend),
                "epoch",
                "thread",
                Some("turn"),
                "turn/completed",
            );
            assert!(subject.notification(&correct));
            *world.find_observations.borrow_mut() =
                Some(observations("changed-during-read", "epoch"));
            subject.step(&id(200)).await.unwrap();
            assert!(!subject.notification(&correct));
            *world.find_observations.borrow_mut() = Some(observations("next", "epoch"));
            subject.step(&id(200)).await.unwrap();
            assert!(subject.notification(&correct));
            world.now.set(300_001.0);
            assert_eq!(subject.diagnostics().unwrap().len(), 1);
            for wrong in [
                notice(
                    "native-agent",
                    None,
                    "epoch",
                    "thread",
                    Some("turn"),
                    "turn/completed",
                ),
                notice(
                    "next",
                    None,
                    "epoch",
                    "thread",
                    Some("turn"),
                    "turn/completed",
                ),
                notice(
                    "next",
                    Some(&foreign),
                    "epoch",
                    "thread",
                    Some("turn"),
                    "turn/completed",
                ),
                notice(
                    "next",
                    Some(&backend),
                    "old",
                    "thread",
                    Some("turn"),
                    "turn/completed",
                ),
                notice(
                    "next",
                    Some(&backend),
                    "epoch",
                    "other",
                    Some("turn"),
                    "turn/completed",
                ),
                notice(
                    "next",
                    Some(&backend),
                    "epoch",
                    "thread",
                    Some("other"),
                    "turn/completed",
                ),
                notice(
                    "next",
                    Some(&backend),
                    "epoch",
                    "thread",
                    Some("turn"),
                    "ignored",
                ),
            ] {
                assert!(!subject.notification(&wrong));
                assert_eq!(subject.diagnostics().unwrap().len(), 1);
            }
            for key in ["epoch", "serviceNodeId", "backend"] {
                let mut malformed = correct.clone();
                let mut params = f(&malformed, "params").clone();
                let mut event = f(&params, "payload").clone();
                event.insert(key, Json::Null);
                params.insert("payload", event);
                malformed.insert("params", params);
                assert!(!subject.notification(&malformed), "{key}");
            }
            let mut foreign_source = correct.clone();
            let mut params = f(&foreign_source, "params").clone();
            params.insert("serviceNodeId", "foreign".into());
            foreign_source.insert("params", params);
            assert!(!subject.notification(&foreign_source));
            let mut nested_turn = correct.clone();
            let mut params = f(&nested_turn, "params").clone();
            let mut event = f(&params, "payload").clone();
            let mut body = f(&event, "params").clone();
            body.insert("turn", Json::object([("id", "other".into())]));
            event.insert("params", body);
            params.insert("payload", event);
            nested_turn.insert("params", params);
            assert!(!subject.notification(&nested_turn));
            assert!(!subject.notification(&notice(
                "next",
                Some(&backend),
                "epoch",
                "thread",
                Some("turn"),
                "item/agentMessage/delta"
            )));
            assert!(subject.diagnostics().unwrap().is_empty());
            assert!(subject.notification(&notice(
                "next",
                Some(&backend),
                "epoch",
                "thread",
                None,
                "thread/status/changed"
            )));

            // Only a matching completion hint releases a delayed retry.
            *world.fault.borrow_mut() = Json::object([
                ("where", "owner".into()),
                ("code", "invalid_arguments".into()),
                ("message", "Fixture unavailable.".into()),
            ]);
            assert!(subject.drain(&id(200)).await.is_err());
            assert!(!subject.notification(&notice(
                "next",
                Some(&foreign),
                "epoch",
                "thread",
                Some("turn"),
                "turn/completed"
            )));
            assert!(!subject.drain(&id(200)).await.unwrap());
            assert!(subject.notification(&correct));
            assert!(subject.drain(&id(200)).await.is_err());
            *world.fault.borrow_mut() = Json::Null;

            let original_run = world.run(&id(200));
            let last = target("last");
            let mut current = world.task(&id(100));
            current.value = route_task(current.value, &last);
            world.tasks.borrow_mut().insert(id(100), current);
            *world.find_observations.borrow_mut() = Some(observations("last", "replacement"));
            subject.step(&id(200)).await.unwrap();
            assert!(!subject.notification(&correct));
            let last_notice = notice(
                "last",
                Some(&backend),
                "replacement",
                "thread",
                Some("turn"),
                "turn/completed",
            );
            assert!(subject.notification(&last_notice));
            *world.find_observations.borrow_mut() = Some(observations("last", "again"));
            subject.step(&id(200)).await.unwrap();
            assert!(!subject.notification(&last_notice));
            let current_notice = notice(
                "last",
                Some(&backend),
                "again",
                "thread",
                Some("turn"),
                "turn/completed",
            );
            assert!(subject.notification(&current_notice));
            assert_eq!(world.run(&id(200)).value, original_run.value);
            // A failed inspection after a new published route retires the old
            // hint; it cannot reset retries for a different current owner.
            let mut current = world.task(&id(100));
            current.value = route_task(current.value, &target("unavailable"));
            world.tasks.borrow_mut().insert(id(100), current);
            *world.fault.borrow_mut() = Json::object([
                ("where", "find".into()),
                ("code", "native_unavailable".into()),
                ("message", "Fixture unavailable.".into()),
            ]);
            subject.step(&id(200)).await.unwrap();
            assert!(!subject.notification(&current_notice));

            let mut stopped_row = row.clone();
            let mut input = f(&stopped_row, "input").clone();
            input.insert("stopped", true.into());
            stopped_row.insert("input", input);
            let stopped = World::new(&stopped_row);
            *stopped.find_observations.borrow_mut() = Some(observations("next", "epoch"));
            let subject = NativeOrchestrator::new(
                &stopped,
                &stopped,
                &stopped,
                &catalogs,
                progress_tracker(),
            );
            subject.step(&id(200)).await.unwrap();
            assert!(subject.notification(&correct));
            *stopped.find_observations.borrow_mut() = Some(observations("next", "replacement"));
            subject.step(&id(200)).await.unwrap();
            assert!(!subject.notification(&correct));
            assert!(!subject.notification(&notice(
                "next",
                Some(&backend),
                "replacement",
                "thread",
                Some("turn"),
                "turn/completed"
            )));
            assert!(stopped.log.borrow().iter().any(|row| {
                row.as_array().is_some_and(|row| {
                    row[0].is_str("update")
                        && f(f(&row[1], "change"), "code").is_str("native_process_lost")
                })
            }));
        }
    }
}

fn live_case(name: &str) -> bool {
    matches!(
        name,
        "prepare cancellation interrupt"
            | "pending interrupt advances"
            | "false interrupt result continues observation"
            | "conclusive interrupt still needs terminal read"
            | "unchanged active poll writes no evidence"
            | "work revision prepares steer"
            | "peer state prepares steer"
            | "queued comment prepares steer"
            | "initial queued comment is not resent"
            | "previous steer baseline is retained"
            | "new steering keeps predecessor and original queue"
            | "native user input projects waiting"
            | "notifications are scoped advisory retry hints"
    )
}
fn live_assertions(world: &World<'_>, name: &str) {
    let previous = matches!(
        name,
        "previous steer baseline is retained" | "new steering keeps predecessor and original queue"
    );
    let cancellation = !f(f(f(world.row, "run"), "value"), "cancellation").is_null();
    let mut order = vec!["owner", "read", "read"];
    let mut addresses = vec![
        ("task-board/run", Json::String(id(200))),
        ("task-board/task", Json::String(id(100))),
    ];
    if previous {
        order.push("read");
        addresses.push(("task-board/native-call", Json::String(id(302))));
    }
    order.extend(["read", "find", "read"]);
    addresses.extend([
        ("task-board/native-call", Json::String(id(300))),
        ("task-board/native-call", pin(300, 2.0)),
    ]);
    if cancellation {
        match name {
            "prepare cancellation interrupt" => {
                order.push("prepare");
                prepare(world, "interrupt", Json::Null);
            }
            _ => {
                order.push("read");
                addresses.push(("task-board/native-call", Json::String(id(303))));
                if name != "conclusive interrupt still needs terminal read" {
                    order.push("advance");
                    same(
                        &event(world, "advance"),
                        &Json::Array(vec![
                            "advance".into(),
                            Json::String(id(200)),
                            "interrupt".into(),
                        ]),
                    );
                }
                if name != "pending interrupt advances" {
                    order.extend(["save-snapshot", "update"]);
                    snapshot(world);
                }
            }
        }
    } else {
        if previous {
            order.push("read");
            addresses.push(("task-board/native-call", pin(302, 2.0)));
        }
        order.extend(["read", "dependencies"]);
        addresses.push(("task-board/task", pin(100, 1.0)));
        same(
            &event(world, "dependencies"),
            &Json::Array(vec![
                "dependencies".into(),
                f(f(world.row, "task"), "value").clone(),
            ]),
        );
        if matches!(
            name,
            "work revision prepares steer"
                | "queued comment prepares steer"
                | "new steering keeps predecessor and original queue"
        ) {
            order.push("prepare");
            let comments = match name {
                "queued comment prepares steer" => vec!["new".into()],
                "new steering keeps predecessor and original queue" => {
                    vec!["old".into(), "new".into()]
                }
                _ => vec![],
            };
            prepare(
                world,
                "steer",
                Json::object([
                    ("kind", "steer".into()),
                    ("commentIds", Json::Array(comments)),
                    (
                        "observedDependencies",
                        Json::object(Vec::<(&str, Json)>::new()),
                    ),
                    ("wakeAt", Json::Null),
                    (
                        "workRevision",
                        Json::Number(if name == "work revision prepares steer" {
                            2.0
                        } else {
                            1.0
                        }),
                    ),
                    ("peersHash", hash_json(&Json::Array(vec![])).unwrap().into()),
                    (
                        "predecessor",
                        if previous { pin(302, 2.0) } else { Json::Null },
                    ),
                ]),
            );
        } else if name == "native user input projects waiting" {
            order.extend(["save-snapshot", "update"]);
            snapshot(world);
        }
        // Peer hashes alone are no longer an input trigger. Only the explicit
        // current dependency snapshot is read, and unchanged polls write nothing.
    }
    methods(world, &order);
    reads(world, addresses);
}

#[tokio::test(flavor = "current_thread")]
async fn bounded_orchestration_preserves_historical_fences_and_current_native_policy() {
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let rows = f(data(), "rows").as_array().unwrap();
    let mut golden = 0;
    let mut current = 0;
    let mut mismatches = vec![];
    for original in rows {
        let name = s(original, "name").to_utf8().unwrap();
        let row = if live_case(&name) {
            attached(original)
        } else if name == "idle thread projects unknown" {
            current_task(original)
        } else {
            original.clone()
        };
        let world = World::new(&row);
        let results = actions(&world, &catalogs).await;
        if name == "missing owner confirmation projects unknown" {
            same(
                &Json::Array(results),
                &Json::Array(vec![outcome(Ok(Json::Null))]),
            );
            methods(&world, &["owner", "read", "read", "find", "prepare"]);
            reads(
                &world,
                vec![
                    ("task-board/run", Json::String(id(200))),
                    ("task-board/task", Json::String(id(100))),
                ],
            );
            prepare(&world, "thread", Json::object([("kind", "initial".into())]));
            current += 1;
        } else if name == "reattach refuses cancelled" {
            // The historical input spread retains a known turn in this row.
            // Current cancellation observes it through a successor attachment.
            assert_eq!(f(f(f(&row, "run"), "value"), "turnId"), &"turn".into());
            same(
                &Json::Array(results),
                &Json::Array(vec![outcome(Ok(Json::Null))]),
            );
            methods(
                &world,
                &["owner", "read", "read", "read", "find", "read", "prepare"],
            );
            reads(
                &world,
                vec![
                    ("task-board/run", Json::String(id(200))),
                    ("task-board/task", Json::String(id(100))),
                    ("task-board/native-call", Json::String(id(300))),
                    ("task-board/native-call", pin(300, 2.0)),
                ],
            );
            prepare(
                &world,
                "thread",
                Json::object([
                    ("kind", "reattach".into()),
                    ("predecessor", pin(300, 2.0)),
                    ("preparedEpoch", "epoch".into()),
                ]),
            );
            current += 1;
        } else if name == "idle thread projects unknown" {
            same(
                &Json::Array(results),
                &Json::Array(vec![outcome(Ok(Json::Null))]),
            );
            methods(
                &world,
                &[
                    "owner",
                    "read",
                    "read",
                    "find",
                    "read",
                    "dependencies",
                    "save-snapshot",
                    "update",
                ],
            );
            reads(
                &world,
                vec![
                    ("task-board/run", Json::String(id(200))),
                    ("task-board/task", Json::String(id(100))),
                    ("task-board/task", pin(100, 1.0)),
                ],
            );
            same(
                &event(&world, "dependencies"),
                &Json::Array(vec![
                    "dependencies".into(),
                    f(f(&row, "task"), "value").clone(),
                ]),
            );
            snapshot(&world);
            current += 1;
        } else if name == "retry growth and cap" {
            let attempted = [0, 5, 7, 11, 13, 15, 17];
            let expected: Vec<Json> = (0..20)
                .map(|index| {
                    if attempted.contains(&index) {
                        outcome(Err(IvyError::new(
                            "invalid_arguments",
                            "Fixture unavailable.",
                        )))
                    } else if [1, 3, 9, 19].contains(&index) {
                        outcome(Ok(false.into()))
                    } else {
                        outcome(Ok(Json::Null))
                    }
                })
                .collect();
            same(&Json::Array(results), &Json::Array(expected));
            methods(
                &world,
                &vec!["cache-enter", "read", "owner", "cache-exit"].repeat(attempted.len()),
            );
            reads(
                &world,
                vec![("task-board/run", Json::String(id(200))); attempted.len()],
            );
            current += 1;
        } else if name == "bounded maps preserve insertion and refresh eviction" {
            // Unstarted turns have no observed native owner to receive hints.
            // The existing bounded retry map still expires its oldest entry.
            same(
                &Json::Array(results),
                &Json::Array(vec![
                    outcome(Ok(Json::Null)),
                    outcome(Ok(false.into())),
                    outcome(Ok(false.into())),
                    outcome(Ok(false.into())),
                    outcome(Ok(true.into())),
                    outcome(Ok(true.into())),
                ]),
            );
            let log = world.log.borrow();
            let count = |name: &str| {
                log.iter()
                    .filter(|row| row.as_array().unwrap()[0].is_str(name))
                    .count()
            };
            assert_eq!(count("cache-enter"), 1026);
            assert_eq!(count("prepare"), 1026);
            assert_eq!(count("find"), 0);
            current += 1;
        } else if name == "notifications are scoped advisory retry hints" {
            let failure = || {
                outcome(Err(IvyError::new(
                    "invalid_arguments",
                    "Fixture unavailable.",
                )))
            };
            same(
                &Json::Array(results),
                &Json::Array(vec![
                    outcome(Ok(true.into())),
                    outcome(Ok(Json::Null)),
                    failure(),
                    outcome(Ok(false.into())),
                    outcome(Ok(false.into())),
                    outcome(Ok(false.into())),
                    outcome(Ok(false.into())),
                    outcome(Ok(true.into())),
                    failure(),
                ]),
            );
            methods(
                &world,
                &[
                    "cache-enter",
                    "read",
                    "owner",
                    "read",
                    "read",
                    "read",
                    "find",
                    "read",
                    "read",
                    "dependencies",
                    "read",
                    "read",
                    "cache-exit",
                    "cache-enter",
                    "read",
                    "owner",
                    "cache-exit",
                    "cache-enter",
                    "read",
                    "owner",
                    "cache-exit",
                ],
            );
            reads(
                &world,
                vec![
                    ("task-board/run", Json::String(id(200))),
                    ("task-board/run", Json::String(id(200))),
                    ("task-board/task", Json::String(id(100))),
                    ("task-board/native-call", Json::String(id(300))),
                    ("task-board/native-call", pin(300, 2.0)),
                    ("task-board/task", pin(100, 1.0)),
                    ("task-board/run", Json::String(id(200))),
                    ("task-board/native-call", Json::String(id(300))),
                    ("task-board/run", Json::String(id(200))),
                    ("task-board/run", Json::String(id(200))),
                ],
            );
            current += 1;
        } else if live_case(&name) {
            same(
                &Json::Array(results),
                &Json::Array(vec![outcome(Ok(Json::Null))]),
            );
            live_assertions(&world, &name);
            current += 1;
        } else {
            let actual = Json::object([
                ("results", Json::Array(results)),
                ("trace", Json::Array(world.log.into_inner())),
            ]);
            if hash_json(&actual).unwrap() != s(original, "expectedHash").to_utf8().unwrap() {
                mismatches.push(format!(
                    "{name}: {}",
                    canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap()
                ));
            }
            golden += 1;
        }
    }
    assert!(
        mismatches.is_empty(),
        "Historical trace changes require explicit current assertions:\n{}",
        mismatches.join("\n")
    );
    assert_eq!((rows.len(), golden, current), (87, 69, 18));
}

#[tokio::test]
async fn observed_notification_index_updates_at_capacity_without_evicting_other_runs() {
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let source = data()
        .required("rows")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|row| f(row, "name").is_str("unchanged active poll writes no evidence"))
        .unwrap();
    let row = attached(source);
    let world = World::new(&row);
    let subject = NativeOrchestrator::new(&world, &world, &world, &catalogs, progress_tracker());
    let notice = |number| {
        let run = world.run(&id(number));
        Json::object([(
            "params",
            Json::object([
                ("serviceNodeId", "native-agent".into()),
                (
                    "payload",
                    Json::object([
                        ("serviceNodeId", "native-agent".into()),
                        ("epoch", "epoch".into()),
                        ("method", "turn/completed".into()),
                        (
                            "params",
                            Json::object([
                                (
                                    "threadId",
                                    f(f(&run.value, "primaryResourceRef"), "nativeId").clone(),
                                ),
                                ("turnId", f(&run.value, "turnId").clone()),
                            ]),
                        ),
                    ]),
                ),
            ]),
        )])
    };
    for number in 201..1225 {
        subject.step(&id(number)).await.unwrap();
    }
    assert!(subject.notification(&notice(201)));
    subject.step(&id(1224)).await.unwrap();
    assert!(
        subject.notification(&notice(201)),
        "Refreshing an existing entry must retain the oldest other entry."
    );
    subject.step(&id(1225)).await.unwrap();
    assert!(!subject.notification(&notice(201)));
    assert!(subject.notification(&notice(202)));
    let mut finished = world.run(&id(202));
    finished.value.insert("phase", "completed".into());
    finished.value.insert(
        "externalOutcome",
        Json::object([("state", "succeeded".into())]),
    );
    world.runs.borrow_mut().insert(id(202), finished);
    subject.step(&id(202)).await.unwrap();
    assert!(!subject.notification(&notice(202)));
}
