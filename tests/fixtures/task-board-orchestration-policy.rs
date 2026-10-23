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
            "notification" => Ok(subject.notification(&action[1]).into()),
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
    assert_eq!((rows.len(), golden, current), (87, 70, 17));
}
