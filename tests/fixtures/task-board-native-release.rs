use super::*;
use ivy_task_board::native_release;

fn completed() -> World {
    let mut run = base_run();
    run.insert("phase", "completed".into());
    run.insert("result", pin(900));
    run.insert(
        "externalOutcome",
        Json::object([("state", "succeeded".into())]),
    );
    run.insert(
        "contextRelease",
        Json::object([
            ("state", "pending".into()),
            ("expectedOperationId", "last-turn".into()),
            ("commentCount", 0.0.into()),
            ("workRevision", 3.0.into()),
            ("call", Json::Null),
            ("code", Json::Null),
        ]),
    );
    let mut task = base_task();
    task.insert("claim", Json::Null);
    task.insert("workflowState", "review".into());
    task.insert("contextRelease", pin(2));
    task.insert("latestResult", pin(900));
    World::new(task, run)
}

#[tokio::test]
async fn accepted_unpublished_work_defers_release_attachment_without_a_receipt_or_task_write() {
    let w = completed();
    w.put("task-board/native-call", pin(71), release_call());
    w.pending_operations.set(true);
    let error = update(
        &w,
        Json::object([("kind", "attachRelease".into()), ("call", pin(71))]),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "task_board_context_release_pending");
    assert!(w.writes.borrow().is_empty());
    assert!(w.calls.borrow().is_empty());
    assert_eq!(f(&w.final_task(), "contextRelease"), &pin(2));
    assert!(native_release::call(&w.saved_run()).is_null());
    w.pending_operations.set(false);
    attach(&w).await;
    assert_eq!(native_release::call(&w.saved_run()), &pin(71));
}

#[tokio::test]
async fn archived_release_retention_and_original_observation_preserve_membership_results_and_primary()
 {
    for observed in [false, true] {
        let w = completed();
        if observed {
            let mut call = attach(&w).await;
            let mut receipt = owner("succeeded", Json::Null);
            receipt.insert("requestId", Json::Null);
            receipt.insert("reply", Json::Null);
            receipt.insert(
                "release",
                Json::object([
                    ("kind", "logical".into()),
                    ("otherSubscribers", true.into()),
                ]),
            );
            for (key, value) in [
                ("state", "observed".into()),
                ("ownerPhase", "succeeded".into()),
                ("epoch", "epoch".into()),
                ("observedAt", AT.into()),
                ("evidence", proof(171)),
            ] {
                call.insert(key, value);
            }
            let mut next = pin(71);
            next.insert("revision", 2.0.into());
            w.observe(next.clone(), call, receipt);
            w.archived.set(true);
            update(
                &w,
                Json::object([("kind", "observeRelease".into()), ("call", next)]),
            )
            .await
            .unwrap();
        } else {
            w.archived.set(true);
            w.put("task-board/native-call", pin(71), release_call());
            assert_eq!(
                update(
                    &w,
                    Json::object([("kind", "attachRelease".into()), ("call", pin(71))])
                )
                .await
                .unwrap_err()
                .code,
                "task_board_context_needed"
            );
            assert!(w.writes.borrow().is_empty());
            update(
                &w,
                Json::object([
                    ("kind", "retainContext".into()),
                    ("code", "native_context_needed".into()),
                ]),
            )
            .await
            .unwrap();
            assert!(w.calls.borrow().is_empty());
        }
        assert!(w.archived.get());
        let task = w.final_task();
        assert_eq!(f(&task, "primaryResourceRef"), &primary());
        assert_eq!(f(&task, "latestResult"), &pin(900));
        assert_eq!(f(&task, "workflowState"), &Json::from("review"));
        assert!(f(&task, "contextRelease").is_null());
        let writes = w.writes.borrow();
        let final_writes: Vec<_> = writes
            .iter()
            .rev()
            .filter(|(key, _, _)| key == "task-board/task")
            .take(2)
            .collect();
        assert_eq!(final_writes.len(), 2);
        assert!(
            final_writes
                .iter()
                .all(|(_, _, destination)| destination.get("expectedArchived")
                    == Some(&Json::Bool(true)))
        );
    }
}
async fn update(w: &World, change: Json) -> IvyResult<Json> {
    let task = w.lookup("task-board/task", &Json::String(id(1)))?;
    let run = w.lookup("task-board/run", &Json::String(id(2)))?;
    let op = doc(
        pin(3),
        Json::object([
            (
                "request",
                Json::object([
                    ("action", "nativeUpdate".into()),
                    ("operationId", "original-release-update".into()),
                    ("taskId", Json::String(id(1))),
                    ("expectedRevision", Json::Number(task.pin.revision)),
                    ("run", run.pin.to_json()),
                    ("change", change),
                ]),
            ),
            ("actor", actor("native")),
            ("callerPrincipalId", "board".into()),
            ("createdAt", AT.into()),
            ("writes", Json::Array(vec![])),
        ]),
    );
    NativePublisher {
        store: w,
        engine: w,
        evidence: w,
    }
    .publish(&op, &mut Steps(w), None)
    .await
}
fn release_call() -> Json {
    let mut call = native_call(
        71,
        "agent.release",
        Json::object([("kind", "release".into())]),
        None,
    );
    set(
        &mut call,
        "request",
        "params",
        Json::object([
            ("threadId", "thread".into()),
            ("expectedEpoch", "epoch".into()),
            ("expectedOperationId", "last-turn".into()),
        ]),
    );
    call
}
async fn attach(w: &World) -> Json {
    let call = release_call();
    w.put("task-board/native-call", pin(71), call.clone());
    update(
        w,
        Json::object([("kind", "attachRelease".into()), ("call", pin(71))]),
    )
    .await
    .unwrap();
    call
}

#[test]
fn context_release_requires_the_exact_completion_result_and_current_work_watermark() {
    let w = completed();
    let task = w.final_task();
    let run = w.saved_run();
    assert!(native_release::eligible(&task, &run, &pin(2)));
    for (key, value) in [
        ("claim", Json::object([("run", pin(999))])),
        ("publication", pin(8)),
        ("lastRun", pin(999)),
        (
            "primaryResourceRef",
            Json::object([("nativeId", "different".into())]),
        ),
        ("workRevision", 4.0.into()),
        ("executionChangeRequested", true.into()),
        (
            "completionIssue",
            Json::object([("code", "pending".into())]),
        ),
        (
            "runtimeWait",
            Json::object([("reason", "external_outcome".into())]),
        ),
        (
            "comments",
            Json::Array(vec![comment("new", Json::Null, "update")]),
        ),
    ] {
        let mut changed = task.clone();
        changed.insert(key, value);
        assert!(!native_release::eligible(&changed, &run, &pin(2)), "{key}");
    }
    for (key, value) in [
        ("phase", "running".into()),
        ("result", Json::Null),
        ("cancellation", Json::object([("reason", "work".into())])),
    ] {
        let mut changed = run.clone();
        changed.insert(key, value);
        assert!(!native_release::eligible(&task, &changed, &pin(2)), "{key}");
    }
}

#[tokio::test]
async fn completion_only_declares_release_after_an_explicit_volatile_state_handoff_and_known_calls()
{
    for disposition in [None, Some("retain"), Some("release")] {
        let mut task = base_task();
        let mut handoff = handoff("review");
        if let Some(value) = disposition {
            set(&mut handoff, "handoff", "nativeContext", value.into());
        }
        task.insert("comments", Json::Array(vec![handoff]));
        let mut run = base_run();
        set(&mut run, "calls", "turn", pin(70));
        let w = World::new(task, run);
        let call = native_call(
            70,
            "turn/start",
            Json::object([("kind", "initial".into())]),
            Some("succeeded"),
        );
        let mut observed = owner("succeeded", Json::Object(vec![]));
        observed.insert("operationId", "last-turn".into());
        observed.insert("requestId", 5.0.into());
        w.observe(pin(70), call, observed);
        w.complete().await.unwrap();
        let run = w.saved_run();
        let task = w.final_task();
        assert_eq!(
            native_release::pending(&run),
            disposition == Some("release")
        );
        assert_eq!(
            !f(&task, "contextRelease").is_null(),
            disposition == Some("release")
        );
        assert_eq!(f(&task, "primaryResourceRef"), &primary());
        assert_eq!(f(&task, "latestResult"), f(&run, "result"));
        if disposition == Some("release") {
            assert!(native_release::eligible(&task, &run, f(&task, "lastRun")));
        }
    }
}

#[tokio::test]
async fn context_only_handoff_preserves_review_done_results_and_acceptance_before_release() {
    for state in ["review", "done", "waiting"] {
        let mut task = base_task();
        let mut handoff = handoff("review");
        handoff.insert(
            "handoff",
            Json::object([("nativeContext", "release".into())]),
        );
        task.insert("comments", Json::Array(vec![handoff]));
        let mut run = base_run();
        set(&mut run, "executionContext", "workflowState", state.into());
        set(&mut run, "calls", "turn", pin(70));
        let w = World::new(task, run);
        let call = native_call(
            70,
            "turn/start",
            Json::object([("kind", "initial".into())]),
            Some("succeeded"),
        );
        let mut observed = owner("succeeded", Json::Object(vec![]));
        observed.insert("operationId", "native-70".into());
        observed.insert("requestId", 5.0.into());
        w.observe(pin(70), call, observed);
        w.complete().await.unwrap();
        let task = w.final_task();
        let run = w.saved_run();
        assert_eq!(f(&task, "workflowState"), &Json::from(state));
        assert_eq!(f(&task, "latestResult"), &pin(901));
        assert_eq!(f(&task, "acceptedReview"), &pin(902));
        assert_eq!(f(&task, "workRevision"), &Json::Number(3.0));
        assert!(!f(&run, "result").is_null());
        assert!(native_release::eligible(&task, &run, f(&task, "lastRun")));
    }
}

#[tokio::test]
async fn original_release_observations_preserve_results_new_work_and_logical_native_distinction() {
    for phase in [
        "accepted",
        "dispatched",
        "outcome_unknown",
        "succeeded",
        "failed",
    ] {
        let w = completed();
        let mut call = attach(&w).await;
        let mut observed = owner(phase, Json::object([("status", "unsubscribed".into())]));
        if phase == "succeeded" {
            observed.insert(
                "release",
                Json::object([
                    ("kind", "logical".into()),
                    ("otherSubscribers", true.into()),
                ]),
            );
            observed.insert("reply", Json::Null);
            observed.insert("requestId", Json::Null);
        }
        call.insert(
            "state",
            if phase == "outcome_unknown" {
                "outcome_unknown"
            } else {
                "observed"
            }
            .into(),
        );
        call.insert("ownerPhase", phase.into());
        call.insert("epoch", "epoch".into());
        call.insert("observedAt", AT.into());
        call.insert("evidence", proof(171));
        let mut call_pin = pin(71);
        call_pin.insert("revision", 2.0.into());
        w.observe(call_pin.clone(), call, observed.clone());
        // A newer claim/comment must survive observation of the old release.
        let mut newer = w.final_task();
        newer.insert("claim", Json::object([("run", pin(999))]));
        newer.insert("lastRun", pin(999));
        newer.insert(
            "comments",
            Json::Array(vec![comment("new", Json::Null, "update")]),
        );
        let current = w.lookup("task-board/task", &Json::String(id(1))).unwrap();
        let mut next_pin = current.pin.to_json();
        next_pin.insert("revision", (current.pin.revision + 1.0).into());
        w.put("task-board/task", next_pin, newer);
        update(
            &w,
            Json::object([
                ("kind", "observeRelease".into()),
                ("call", call_pin.clone()),
            ]),
        )
        .await
        .unwrap();
        let task = w.final_task();
        let run = w.saved_run();
        assert_eq!(f(&task, "workflowState"), &Json::from("review"));
        assert_eq!(f(&task, "latestResult"), &pin(900));
        assert_eq!(f(&task, "lastRun"), &pin(999));
        assert_eq!(f(f(&task, "claim"), "run"), &pin(999));
        assert_eq!(f(&task, "primaryResourceRef"), &primary());
        assert!(f(&task, "contextRelease").is_null());
        assert_eq!(native_release::call(&run), &call_pin);
        assert_eq!(
            native_release::pending(&run),
            ["accepted", "dispatched", "outcome_unknown"].contains(&phase)
        );
        assert!(w.creates.borrow().is_empty());
        assert_eq!(w.calls.borrow().last().unwrap().1, observed);
    }
}

#[tokio::test]
async fn a_new_comment_cancels_unsent_release_without_a_native_success_or_changed_result() {
    let w = completed();
    let mut task = w.final_task();
    task.insert(
        "comments",
        Json::Array(vec![comment("new", Json::Null, "update")]),
    );
    w.put("task-board/task", pin(1), task);
    w.put("task-board/native-call", pin(71), release_call());
    let error = update(
        &w,
        Json::object([("kind", "attachRelease".into()), ("call", pin(71))]),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "task_board_context_needed");
    assert!(w.writes.borrow().is_empty());
    update(
        &w,
        Json::object([
            ("kind", "retainContext".into()),
            ("code", "native_context_needed".into()),
        ]),
    )
    .await
    .unwrap();
    assert_eq!(
        f(f(&w.saved_run(), "contextRelease"), "state"),
        &Json::from("retained")
    );
    assert!(f(&w.final_task(), "contextRelease").is_null());
    assert_eq!(f(&w.final_task(), "latestResult"), &pin(900));
    assert!(w.calls.borrow().is_empty());
}
