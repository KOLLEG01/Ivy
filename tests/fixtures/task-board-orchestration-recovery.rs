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
