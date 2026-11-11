use super::*;

// Retain the historical harness's finite observations, but assert the current
// recovery decisions explicitly instead of reusing its old expected hashes.
fn row(name: &str) -> Json {
    data()
        .required("rows")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|row| f(row, "name") == &Json::from(name))
        .unwrap()
        .clone()
}
fn change(row: &mut Json, key: &str, value: Json) {
    let mut input = f(row, "input").clone();
    input.insert(key, value);
    row.insert("input", input);
}
fn methods(world: &World<'_>) -> Vec<String> {
    world
        .log
        .borrow()
        .iter()
        .map(|event| event.as_array().unwrap()[0].as_str().unwrap())
        .collect()
}
fn catalogs() -> CheckedNativeContracts {
    CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
}

#[tokio::test(flavor = "current_thread")]
async fn new_work_waits_for_original_release_admission_without_failure_or_native_dispatch() {
    let mut row = row("publication guard");
    let mut task = f(&row, "task").clone();
    let mut value = f(&task, "value").clone();
    value.insert("publication", Json::Null);
    value.insert("contextRelease", pin(999, 1.0));
    task.insert("value", value);
    row.insert("task", task);
    let world = World::new(&row);
    let catalogs = catalogs();
    NativeOrchestrator::new(&world, &world, &world, &catalogs, progress_tracker())
        .step(&id(200))
        .await
        .unwrap();
    assert!(
        !methods(&world)
            .iter()
            .any(|m| ["prepare", "advance", "find", "update"].contains(&m.as_str()))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn completed_release_recovers_original_publication_and_observation_after_restart() {
    let catalogs = catalogs();
    for mode in [
        "prepare",
        "observe",
        "unknown",
        "stale",
        "publication",
        "archived",
    ] {
        let mut row = row("publication guard");
        let mut run = f(&row, "run").clone();
        let mut value = f(&run, "value").clone();
        value.insert("phase", "completed".into());
        value.insert("result", pin(900, 1.0));
        value.insert("cancellation", Json::Null);
        value.insert(
            "externalOutcome",
            Json::object([("state", "succeeded".into())]),
        );
        value.insert(
            "contextRelease",
            Json::object([
                (
                    "state",
                    if mode == "unknown" {
                        "outcome_unknown"
                    } else {
                        "pending"
                    }
                    .into(),
                ),
                ("expectedOperationId", "last-turn".into()),
                ("commentCount", 0.0.into()),
                ("workRevision", 3.0.into()),
                (
                    "call",
                    if ["observe", "unknown"].contains(&mode) {
                        pin(300, 1.0)
                    } else {
                        Json::Null
                    },
                ),
                ("code", Json::Null),
            ]),
        );
        let source = catalogs
            .checked_native_contract(&"0.163.0".into(), None)
            .unwrap();
        value
            .get_mut("plan")
            .unwrap()
            .insert("nativeVersion", "0.163.0".into());
        value.get_mut("plan").unwrap().insert(
            "catalogSourceHash",
            source.catalog().required("sourceHash").unwrap().clone(),
        );
        let primary = f(&value, "primaryResourceRef").clone();
        run.insert("value", value);
        row.insert("run", run.clone());
        let mut task = f(&row, "task").clone();
        let mut value = f(&task, "value").clone();
        for (key, value_) in [
            ("claim", Json::Null),
            ("publication", Json::Null),
            ("runtimeWait", Json::Null),
            ("workRevision", 3.0.into()),
            ("lastRun", pin(200, n(f(&run, "pin"), "revision"))),
            ("latestResult", pin(900, 1.0)),
            ("primaryResourceRef", primary),
            ("comments", Json::Array(vec![])),
            ("commentDeliveries", Json::Array(vec![])),
        ] {
            value.insert(key, value_);
        }
        value.remove("primaryBackend");
        if mode == "unknown" {
            value.remove("contextRelease");
        } else {
            value.insert("contextRelease", pin(200, n(f(&run, "pin"), "revision")));
        }
        if mode == "stale" {
            value.insert("comments", Json::Array(vec!["new input".into()]));
        }
        task.insert("value", value);
        row.insert("task", task);
        if mode == "publication" {
            change(&mut row, "pendingNative", true.into());
        }
        if mode == "archived" {
            change(&mut row, "releaseArchived", true.into());
        }
        let world = World::new(&row);
        NativeOrchestrator::new(&world, &world, &world, &catalogs, progress_tracker())
            .step(&id(200))
            .await
            .unwrap();
        let methods = methods(&world);
        let expected = match mode {
            "prepare" => "prepare",
            "observe" | "unknown" => "advance",
            "stale" | "archived" => "update",
            _ => "recover-pending",
        };
        assert!(methods.contains(&expected.into()), "{mode}: {methods:?}");
        assert!(!methods.contains(&"find".into()));
        if ["observe", "unknown"].contains(&mode) {
            assert!(!methods.contains(&"prepare".into()));
        }
        if ["prepare", "observe", "unknown"].contains(&mode) {
            let log = world.log.borrow();
            let event = log
                .iter()
                .find(|v| v.as_array().unwrap()[0].is_str(expected))
                .unwrap();
            assert_eq!(event.as_array().unwrap()[2], Json::from("release"));
        }
        if mode == "publication" {
            assert_eq!(methods, ["owner", "read", "recover-pending"]);
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn current_recovery_precedes_task_reads_and_retained_thread_control() {
    let catalogs = catalogs();
    let mut pending = row("publication guard");
    change(&mut pending, "pendingNative", true.into());
    let world = World::new(&pending);
    NativeOrchestrator::new(&world, &world, &world, &catalogs, progress_tracker())
        .step(&id(200))
        .await
        .unwrap();
    assert_eq!(methods(&world), ["owner", "read", "recover-pending"]);
    assert_eq!(
        world.log.borrow().last().unwrap(),
        &Json::Array(vec![
            "recover-pending".into(),
            Json::String(id(100)),
            Json::String(id(200))
        ])
    );
    // Failure still belongs to that original update; do not inspect or replace it.
    world.fault.replace(Json::object([
        ("where", "pending".into()),
        ("code", "service_unavailable".into()),
        ("message", "pending original".into()),
    ]));
    assert_eq!(
        NativeOrchestrator::new(&world, &world, &world, &catalogs, progress_tracker())
            .step(&id(200))
            .await
            .unwrap_err()
            .code,
        "service_unavailable"
    );
    assert!(
        !methods(&world)
            .iter()
            .any(|v| matches!(v.as_str(), "find" | "update" | "prepare"))
    );

    for recovered in [false, true] {
        let mut retained = row("pending original thread advances");
        let mut run = f(&retained, "run").clone();
        let mut value = f(&run, "value").clone();
        value.insert("turnId", "turn".into());
        run.insert("value", value);
        retained.insert("run", run);
        change(&mut retained, "recovered", recovered.into());
        let world = World::new(&retained);
        NativeOrchestrator::new(&world, &world, &world, &catalogs, progress_tracker())
            .step(&id(200))
            .await
            .unwrap();
        assert!(methods(&world).contains(&"recover".into()));
        assert_eq!(methods(&world).contains(&"advance".into()), !recovered);
        assert!(!methods(&world).contains(&"find".into()));
    }

    // Both active observation and retained cancellation must attach before
    // inspecting flags, preparing interruption, or adopting a replacement epoch.
    for cancelled in [false, true] {
        let mut attachment = row(if cancelled {
            "prepare cancellation interrupt"
        } else {
            "unchanged active poll writes no evidence"
        });
        if cancelled {
            let mut task = f(&attachment, "task").clone();
            let mut value = f(&task, "value").clone();
            value.insert("claim", Json::Null);
            task.insert("value", value);
            attachment.insert("task", task);
        }
        let world = World::new(&attachment);
        NativeOrchestrator::new(&world, &world, &world, &catalogs, progress_tracker())
            .step(&id(200))
            .await
            .unwrap();
        let trace = world.log.borrow();
        let prepared = trace
            .iter()
            .find(|v| v.as_array().unwrap()[0] == "prepare".into())
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(prepared[2], "thread".into());
        assert_eq!(prepared[3], Json::object([("kind", "initial".into())]));
        assert!(
            !methods(&world)
                .iter()
                .any(|v| matches!(v.as_str(), "update" | "save-snapshot" | "advance"))
        );
    }

    let mut successor = row("prepare turn only after thread receipt");
    let mut run = f(&successor, "run").clone();
    let mut value = f(&run, "value").clone();
    value.insert("turnId", "turn".into());
    run.insert("value", value);
    successor.insert("run", run);
    let mut turns = f(&successor, "turns").clone();
    turns.insert("epoch", "replacement".into());
    successor.insert("turns", turns);
    let world = World::new(&successor);
    NativeOrchestrator::new(&world, &world, &world, &catalogs, progress_tracker())
        .step(&id(200))
        .await
        .unwrap();
    let trace = world.log.borrow();
    let prepared = trace.last().unwrap().as_array().unwrap();
    assert_eq!(prepared[0], "prepare".into());
    assert_eq!(prepared[2], "thread".into());
    assert_eq!(f(&prepared[3], "kind"), &"reattach".into());
    assert_eq!(
        f(&prepared[3], "predecessor"),
        f(
            f(f(&successor, "run"), "value").required("calls").unwrap(),
            "thread"
        )
    );
    assert_eq!(f(&prepared[3], "preparedEpoch"), &"replacement".into());
}

#[tokio::test(flavor = "current_thread")]
async fn current_snapshot_recovery_reconfirms_pruned_evidence_without_hiding_other_errors() {
    let catalogs = catalogs();
    for code in ["revision_pruned", "task_board_evidence_mismatch"] {
        let mut retained = row("confirmed terminal handoff skips history");
        change(
            &mut retained,
            "fault",
            Json::object([
                ("where", "snapshot".into()),
                ("code", code.into()),
                ("message", "retained proof unavailable".into()),
            ]),
        );
        let world = World::new(&retained);
        let result = NativeOrchestrator::new(&world, &world, &world, &catalogs, progress_tracker())
            .step(&id(200))
            .await;
        if code == "revision_pruned" {
            result.unwrap();
            assert!(methods(&world).contains(&"save-snapshot".into()));
            let trace = world.log.borrow();
            let update = trace.last().unwrap().as_array().unwrap();
            assert_eq!(f(f(&update[1], "change"), "kind"), &"readSnapshot".into());
        } else {
            assert_eq!(result.unwrap_err().code, code);
            assert!(
                !methods(&world)
                    .iter()
                    .any(|v| matches!(v.as_str(), "save-snapshot" | "update"))
            );
        }
    }
    // An unchanged projection still needs verified evidence before adopting a
    // different epoch, even when this old Run no longer owns the current Task.
    let mut retained = row("unchanged nonclaim poll ignores task waiting");
    let mut turns = f(&retained, "turns").clone();
    turns.insert("epoch", "replacement".into());
    retained.insert("turns", turns);
    let world = World::new(&retained);
    NativeOrchestrator::new(&world, &world, &world, &catalogs, progress_tracker())
        .step(&id(200))
        .await
        .unwrap();
    assert!(methods(&world).contains(&"save-snapshot".into()));
    assert!(!methods(&world).contains(&"prepare".into()));
}

#[tokio::test(flavor = "current_thread")]
async fn current_recovery_retries_use_error_delays_without_growing_outage_backoff() {
    let catalogs = catalogs();
    let retained = row("publication guard");
    let world = World::new(&retained);
    let subject = NativeOrchestrator::new(&world, &world, &world, &catalogs, progress_tracker());
    for (at, attempted) in [
        (0.0, true),
        (249.0, false),
        (250.0, true),
        (499.0, false),
        (500.0, true),
    ] {
        world.now.set(at);
        let before = world.log.borrow().len();
        if attempted {
            assert_eq!(
                subject.drain(&id(200)).await.unwrap_err().code,
                "revision_conflict"
            );
            assert!(world.log.borrow().len() > before);
        } else {
            assert!(!subject.drain(&id(200)).await.unwrap());
            assert_eq!(world.log.borrow().len(), before);
        }
    }
    let mut retained = row("unchanged recovering claim is ephemeral");
    let mut run = f(&retained, "run").clone();
    let mut value = f(&run, "value").clone();
    value.insert("phase", "outcome_unknown".into());
    run.insert("value", value);
    retained.insert("run", run);
    let world = World::new(&retained);
    let subject = NativeOrchestrator::new(&world, &world, &world, &catalogs, progress_tracker());
    for (at, attempted) in [
        (0.0, true),
        (4999.0, false),
        (5000.0, true),
        (9999.0, false),
        (10000.0, true),
    ] {
        world.now.set(at);
        assert_eq!(subject.drain(&id(200)).await.unwrap(), attempted);
    }
}
