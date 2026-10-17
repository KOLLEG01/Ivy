use ivy_agent_manager::interactions::{Limits, NativeInteractions};
use ivy_contracts::{
    IvyError, IvyJson, Outcome,
    json::{parse, stringify},
};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::oneshot;

fn json(s: &str) -> IvyJson {
    parse(s, 1024 * 1024).unwrap()
}
fn limits(count: usize) -> Limits {
    Limits {
        count,
        bytes: 1024 * 1024,
        ttl_ms: 100,
    }
}
fn reply() -> IvyJson {
    json(r#"{"result":{"turn":{"id":"turn"}}}"#)
}

#[tokio::test]
async fn concurrent_duplicates_join_and_dropped_waiters_do_not_cancel_dispatch() {
    let cache = NativeInteractions::new("epoch".into(), limits(10));
    let (send, receive) = oneshot::channel();
    let first = cache
        .run(
            "alice".into(),
            "id".into(),
            "turn/start".into(),
            IvyJson::Null,
            |request| async move {
                assert!(request.request_id.starts_with("interaction-"));
                assert!(request.detach_on_timeout && request.interaction_priority);
                receive.await.unwrap()
            },
        )
        .unwrap();
    let second = cache
        .run(
            "alice".into(),
            "id".into(),
            "turn/start".into(),
            IvyJson::Null,
            |_| async { panic!("duplicate dispatch") },
        )
        .unwrap();
    assert_eq!(
        cache
            .run(
                "alice".into(),
                "id".into(),
                "turn/start".into(),
                true.into(),
                |_| async { panic!("conflicting dispatch") }
            )
            .unwrap_err()
            .code,
        "mutation_conflict"
    );
    let missing = cache.lookup(&"bob".into(), &"id".into()).unwrap_err();
    assert_eq!(missing.code, "interaction_expired");
    assert_eq!(missing.outcome, Outcome::Unknown);
    drop(first);
    send.send(Ok(reply())).unwrap();
    let result = second.wait().await.unwrap();
    assert_eq!(
        result,
        json(r#"{"operationId":"id","epoch":"epoch","reply":{"result":{"turn":{"id":"turn"}}}}"#)
    );
    assert_eq!(
        cache
            .lookup(&"alice".into(), &"id".into())
            .unwrap()
            .wait()
            .await
            .unwrap(),
        result
    );
}

#[tokio::test]
async fn unexecuted_failure_retries_but_unknown_failure_is_retained() {
    let cache = NativeInteractions::new("epoch".into(), limits(10));
    let first = cache
        .run(
            "c".into(),
            "i".into(),
            "m".into(),
            IvyJson::Null,
            |_| async { Err(IvyError::new("native_capacity", "Busy")) },
        )
        .unwrap()
        .wait()
        .await
        .unwrap_err();
    assert_eq!(first.outcome, Outcome::NotExecuted);
    assert!(cache.lookup(&"c".into(), &"i".into()).is_err());
    let unknown = cache
        .run(
            "c".into(),
            "i".into(),
            "m".into(),
            IvyJson::Null,
            |_| async {
                Err(IvyError::new("outcome_unknown", "Lost").with_outcome(Outcome::Unknown))
            },
        )
        .unwrap()
        .wait()
        .await
        .unwrap_err();
    let repeated = cache
        .run(
            "c".into(),
            "i".into(),
            "m".into(),
            IvyJson::Null,
            |_| async { panic!("uncertain effect executed twice") },
        )
        .unwrap()
        .wait()
        .await
        .unwrap_err();
    assert_eq!(repeated, unknown);
}

#[tokio::test]
async fn pending_work_is_never_evicted_to_admit_another_call() {
    let cache = NativeInteractions::new("epoch".into(), limits(1));
    let (send, receive) = oneshot::channel();
    let first = cache
        .run(
            "c".into(),
            "one".into(),
            "m".into(),
            IvyJson::Null,
            |_| async { receive.await.unwrap() },
        )
        .unwrap();
    assert_eq!(
        cache
            .run(
                "c".into(),
                "two".into(),
                "m".into(),
                IvyJson::Null,
                |_| async { panic!("capacity admitted") }
            )
            .unwrap_err()
            .code,
        "native_capacity"
    );
    send.send(Ok(reply())).unwrap();
    first.wait().await.unwrap();
    // The original pruner keeps size strictly below the limit after completion.
    assert!(cache.lookup(&"c".into(), &"one".into()).is_err());
}

#[tokio::test]
async fn ttl_is_measured_from_completion_and_prior_identity_precedes_pruning() {
    let now = Arc::new(AtomicU64::new(100));
    let clock = now.clone();
    let cache = NativeInteractions::with_clock(
        "epoch".into(),
        limits(3),
        Arc::new(move || clock.load(Ordering::Relaxed)),
    );
    let (send, receive) = oneshot::channel();
    let first = cache
        .run(
            "c".into(),
            "one".into(),
            "m".into(),
            IvyJson::Null,
            |_| async { receive.await.unwrap() },
        )
        .unwrap();
    now.store(10_000, Ordering::Relaxed);
    assert!(cache.lookup(&"c".into(), &"one".into()).is_ok());
    send.send(Ok(reply())).unwrap();
    let original = first.wait().await.unwrap();
    now.store(10_100, Ordering::Relaxed);
    assert!(cache.lookup(&"c".into(), &"one".into()).is_err());
    let repeated = cache
        .run(
            "c".into(),
            "one".into(),
            "m".into(),
            IvyJson::Null,
            |_| async { panic!("expired retained identity replayed") },
        )
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert_eq!(repeated, original);
    cache
        .run(
            "c".into(),
            "two".into(),
            "m".into(),
            IvyJson::Null,
            |_| async { Ok(reply()) },
        )
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert!(cache.lookup(&"c".into(), &"one".into()).is_err());
}

#[tokio::test]
async fn cache_charges_escaped_utf16_reply_bytes_at_exact_boundary() {
    let id = json(r#""\ud800""#).as_js_string().unwrap().clone();
    let value = json(
        r#"{"operationId":"\ud800","epoch":"epoch","reply":{"result":{"turn":{"id":"turn"}}}}"#,
    );
    let bytes = stringify(&value, 1024).unwrap().len();
    for budget in [bytes, bytes + 1] {
        let cache = NativeInteractions::new(
            "epoch".into(),
            Limits {
                bytes: budget,
                ..limits(10)
            },
        );
        assert_eq!(
            cache
                .run(
                    "c".into(),
                    id.clone(),
                    "m".into(),
                    IvyJson::Null,
                    |_| async { Ok(reply()) }
                )
                .unwrap()
                .wait()
                .await
                .unwrap(),
            value
        );
        assert_eq!(cache.lookup(&"c".into(), &id).is_ok(), budget > bytes);
    }
}

#[tokio::test]
async fn fifo_eviction_and_caller_scoping_match_the_typescript_cache() {
    let cache = NativeInteractions::new("epoch".into(), limits(3));
    for (caller, id) in [("alice", "one"), ("bob", "one"), ("alice", "two")] {
        cache
            .run(
                caller.into(),
                id.into(),
                "m".into(),
                IvyJson::Null,
                |_| async { Ok(reply()) },
            )
            .unwrap()
            .wait()
            .await
            .unwrap();
    }
    assert!(cache.lookup(&"alice".into(), &"one".into()).is_err());
    assert!(cache.lookup(&"bob".into(), &"one".into()).is_ok());
    assert!(cache.lookup(&"alice".into(), &"two".into()).is_ok());
}

#[tokio::test]
async fn unchanged_typescript_cache_matches_error_replay_ttl_and_fifo_trace() {
    let now = Arc::new(AtomicU64::new(100));
    let clock = now.clone();
    let cache = NativeInteractions::with_clock(
        "epoch".into(),
        Limits {
            bytes: 1024,
            ..limits(3)
        },
        Arc::new(move || clock.load(Ordering::Relaxed)),
    );
    let calls = Arc::new(AtomicU64::new(0));
    let mut trace = Vec::new();
    for action in [
        "run-one",
        "run-one",
        "run-one",
        "other",
        "expired",
        "run-one",
        "run-two",
        "lookup-one",
        "run-three",
        "run-four",
        "lookup-two",
    ] {
        let pending = match action {
            "other" => cache.lookup(&"other".into(), &"one".into()),
            "expired" => {
                now.store(200, Ordering::Relaxed);
                cache.lookup(&"c".into(), &"one".into())
            }
            "lookup-one" => cache.lookup(&"c".into(), &"one".into()),
            "lookup-two" => cache.lookup(&"c".into(), &"two".into()),
            _ => {
                let calls = calls.clone();
                cache.run(
                    "c".into(),
                    action.strip_prefix("run-").unwrap().into(),
                    "m".into(),
                    IvyJson::Null,
                    move |_| async move {
                        match calls.fetch_add(1, Ordering::Relaxed) {
                            0 => Err(IvyError::new("native_capacity", "Busy")),
                            1 => Err(IvyError::new("outcome_unknown", "Lost")
                                .with_outcome(Outcome::Unknown)),
                            _ => Ok(json(r#"{"result":"\ud800"}"#)),
                        }
                    },
                )
            }
        };
        let result = match pending {
            Ok(pending) => pending.wait().await,
            Err(error) => Err(error),
        };
        trace.push(result.unwrap_or_else(|error| {
            IvyJson::object([
                ("code", error.code.into()),
                ("outcome", error.outcome.as_str().into()),
            ])
        }));
    }
    let rust = IvyJson::object([
        ("result", IvyJson::Array(trace)),
        (
            "calls",
            IvyJson::Number(calls.load(Ordering::Relaxed) as f64),
        ),
    ]);
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .arg("tests/fixtures/agent-interactions-oracle.mjs")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(rust, json(std::str::from_utf8(&output.stdout).unwrap()));
}
