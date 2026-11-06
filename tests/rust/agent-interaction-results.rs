use ivy_agent_manager::interactions::{Limits, NativeInteractions, PendingInteraction};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, Outcome};
use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use tokio::sync::oneshot;

fn payload(mib: usize) -> IvyJson {
    let mut units = Vec::with_capacity(mib * 1024 * 1024 / 2);
    units.extend([65, 0xd800, 0xdc01, 0xdfff]);
    IvyJson::String(JsString(units))
}
fn pointer(value: &IvyJson) -> (usize, usize) {
    let units = &value.as_js_string().unwrap().0;
    (units.as_ptr() as usize, units.capacity())
}
fn start(
    cache: &NativeInteractions,
    id: &str,
    result: IvyResult<IvyJson>,
) -> (PendingInteraction, PendingInteraction) {
    let first = cache
        .run(
            "caller".into(),
            id.into(),
            "method".into(),
            IvyJson::Null,
            move |_| async move { result },
        )
        .unwrap();
    let observer = cache
        .run(
            "caller".into(),
            id.into(),
            "method".into(),
            IvyJson::Null,
            |_| async { panic!("An original interaction must dispatch only once") },
        )
        .unwrap();
    (first, observer)
}
fn refused(result: IvyResult<PendingInteraction>) {
    match result {
        Err(error) => {
            assert_eq!(error.code, "native_capacity");
            assert_eq!(error.outcome, Outcome::NotExecuted);
        }
        Ok(_) => panic!("Held interaction owners must retain their admission"),
    }
}
fn unavailable(result: IvyResult<IvyJson>) {
    match result {
        Err(error) => {
            assert_eq!(error.code, "agent_reply_unavailable");
            assert_eq!(error.outcome, Outcome::Unknown);
        }
        Ok(_) => panic!("An executed reply exceeding retained capacity must stay unknown"),
    }
}

#[tokio::test]
async fn last_handle_moves_original_success_and_error_after_fifo_or_idle_ttl_eviction() {
    let cache = NativeInteractions::new(
        "epoch".into(),
        Limits {
            count: 1,
            ..Limits::default()
        },
    );
    let value = payload(1);
    let expected = pointer(&value);
    let (original, observer) = start(&cache, "success", Ok(value));
    let observed = observer.wait().await.unwrap();
    assert_eq!(
        observed
            .required("reply")
            .unwrap()
            .as_js_string()
            .unwrap()
            .0,
        vec![65, 0xd800, 0xdc01, 0xdfff]
    );
    assert!(cache.lookup(&"caller".into(), &"success".into()).is_err());
    assert_eq!(
        pointer(original.wait().await.unwrap().required("reply").unwrap()),
        expected,
        "The last observer must move the provider's original allocation"
    );

    let mut error = IvyError::new_js(JsString(vec![0xdfff]), JsString(vec![0xd800]));
    error.details = Some(payload(1));
    error = error.with_outcome(Outcome::Unknown);
    let expected = pointer(error.details.as_ref().unwrap());
    let (original, observer) = start(&cache, "failure", Err(error));
    assert_eq!(observer.wait().await.unwrap_err().outcome, Outcome::Unknown);
    let error = original.wait().await.unwrap_err();
    assert_eq!(pointer(error.details.as_ref().unwrap()), expected);
    assert_eq!(error.code_js().0, vec![0xdfff]);
    assert_eq!(error.message_js().0, vec![0xd800]);

    let now = Arc::new(AtomicU64::new(100));
    let clock = now.clone();
    let cache = NativeInteractions::with_clock(
        "epoch".into(),
        Limits {
            ttl_ms: 100,
            ..Limits::default()
        },
        Arc::new(move || clock.load(Ordering::SeqCst)),
    );
    let value = payload(1);
    let expected = pointer(&value);
    let (original, observer) = start(&cache, "ttl", Ok(value));
    observer.wait().await.unwrap();
    assert!(cache.lookup(&"caller".into(), &"ttl".into()).is_ok());
    now.store(200, Ordering::SeqCst);
    assert_eq!(cache.prune().unwrap(), 1);
    assert_eq!(
        pointer(original.wait().await.unwrap().required("reply").unwrap()),
        expected
    );
}

#[tokio::test]
async fn evicted_unread_results_hold_all_slots_and_release_them_when_dropped() {
    let cache = NativeInteractions::new(
        "epoch".into(),
        Limits {
            count: 1,
            ..Limits::default()
        },
    );
    let mut held = Vec::new();
    for index in 0..64 {
        let id = format!("original-{index}");
        let (original, observer) = start(&cache, &id, Ok(IvyJson::Null));
        observer.wait().await.unwrap();
        assert!(cache.lookup(&"caller".into(), &id.into()).is_err());
        held.push(original);
    }
    let clone = cache.clone();
    refused(clone.run(
        "caller".into(),
        "denied".into(),
        "method".into(),
        IvyJson::Null,
        |_| async { panic!("Unadmitted interaction must never dispatch") },
    ));
    drop(held.pop());
    cache
        .run(
            "caller".into(),
            "recovered".into(),
            "method".into(),
            IvyJson::Null,
            |_| async { Ok(IvyJson::Null) },
        )
        .unwrap()
        .wait()
        .await
        .unwrap();
    drop(held);
    clone
        .run(
            "caller".into(),
            "released".into(),
            "method".into(),
            IvyJson::Null,
            |_| async { Ok(IvyJson::Null) },
        )
        .unwrap()
        .wait()
        .await
        .unwrap();
}

#[tokio::test]
async fn evicted_heap_keeps_its_budget_and_unavailable_original_is_not_redispatched() {
    let cache = NativeInteractions::new(
        "epoch".into(),
        Limits {
            bytes: 8192,
            ..Limits::default()
        },
    );
    let (first, observer) = start(&cache, "first", Ok(payload(30)));
    observer.wait().await.unwrap();
    let (second, observer) = start(&cache, "second", Ok(payload(20)));
    observer.wait().await.unwrap();
    assert!(cache.lookup(&"caller".into(), &"first".into()).is_err());
    assert!(cache.lookup(&"caller".into(), &"second".into()).is_err());
    let effects = Arc::new(AtomicUsize::new(0));
    let counted = effects.clone();
    let original = cache
        .run(
            "caller".into(),
            "full".into(),
            "method".into(),
            IvyJson::Null,
            move |_| async move {
                counted.fetch_add(1, Ordering::SeqCst);
                Ok(payload(20))
            },
        )
        .unwrap();
    unavailable(original.wait().await);
    unavailable(
        cache
            .run(
                "caller".into(),
                "full".into(),
                "method".into(),
                IvyJson::Null,
                |_| async { panic!("Unavailable original must not be replayed") },
            )
            .unwrap()
            .wait()
            .await,
    );
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(
        cache
            .run(
                "caller".into(),
                "full".into(),
                "method".into(),
                true.into(),
                |_| async { panic!("Conflicting original must not dispatch") }
            )
            .unwrap_err()
            .code,
        "mutation_conflict"
    );
    drop(first);
    let value = payload(20);
    let expected = pointer(&value);
    let (recovered, observer) = start(&cache, "recovered", Ok(value));
    observer.wait().await.unwrap();
    assert_eq!(
        pointer(recovered.wait().await.unwrap().required("reply").unwrap()),
        expected
    );
    drop(second);
    let (oversized, _) = start(&cache, "oversized", Ok(payload(49)));
    unavailable(oversized.wait().await);
}

#[tokio::test]
async fn task_inputs_remain_bounded_after_waiters_are_abandoned() {
    let cache = NativeInteractions::new(
        "epoch".into(),
        Limits {
            bytes: 128 * 1024 * 1024,
            ..Limits::default()
        },
    );
    let mut sends = Vec::new();
    for (id, mib) in [("first", 30), ("second", 20)] {
        let (send, receive) = oneshot::channel();
        let handle = cache
            .run(
                "caller".into(),
                id.into(),
                "method".into(),
                payload(mib),
                move |request| async move {
                    assert_eq!(request.params.as_js_string().unwrap().0.len(), 4);
                    drop(request);
                    receive.await.unwrap()
                },
            )
            .unwrap();
        drop(handle);
        sends.push(send);
    }
    refused(cache.run(
        "caller".into(),
        "denied".into(),
        "method".into(),
        payload(20),
        |_| async { panic!("Input capacity must be checked before dispatch") },
    ));
    let first = cache.lookup(&"caller".into(), &"first".into()).unwrap();
    let second = cache.lookup(&"caller".into(), &"second".into()).unwrap();
    for send in sends {
        send.send(Ok(IvyJson::Null)).unwrap();
    }
    first.wait().await.unwrap();
    second.wait().await.unwrap();
    refused(cache.run(
        "caller".into(),
        "oversized".into(),
        "method".into(),
        payload(49),
        |_| async { panic!("A single oversized input must not dispatch") },
    ));
    cache
        .run(
            "caller".into(),
            "recovered".into(),
            "method".into(),
            payload(20),
            |_| async { Ok(IvyJson::Null) },
        )
        .unwrap()
        .wait()
        .await
        .unwrap();
}

#[tokio::test]
async fn count_pressure_evicts_only_completed_cache_owners_after_original_identity_checks() {
    let cache = NativeInteractions::new("epoch".into(), Limits::default());
    for index in 0..64 {
        cache
            .run(
                "caller".into(),
                format!("id-{index}").into(),
                "method".into(),
                IvyJson::Null,
                |_| async { Ok(IvyJson::Null) },
            )
            .unwrap()
            .wait()
            .await
            .unwrap();
    }
    // Cached originals still join before admission or expiry checks.
    cache
        .run(
            "caller".into(),
            "id-0".into(),
            "method".into(),
            IvyJson::Null,
            |_| async { panic!("A retained identity must join despite full capacity") },
        )
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert_eq!(
        cache
            .run(
                "caller".into(),
                "id-0".into(),
                "method".into(),
                true.into(),
                |_| async { panic!("Conflicting identity must not dispatch") }
            )
            .unwrap_err()
            .code,
        "mutation_conflict"
    );
    cache
        .run(
            "caller".into(),
            "next".into(),
            "method".into(),
            IvyJson::Null,
            |_| async { Ok(IvyJson::Null) },
        )
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert!(
        cache.lookup(&"caller".into(), &"id-0".into()).is_err(),
        "Count pressure must evict a completed cache owner before refusing new work"
    );
    assert!(
        cache.lookup(&"caller".into(), &"id-1".into()).is_ok(),
        "Only the necessary oldest completed cache owner should be evicted"
    );
    assert!(cache.lookup(&"caller".into(), &"next".into()).is_ok());
}

#[tokio::test]
async fn caller_held_cached_unknown_replies_survive_rejected_new_work() {
    let cache = NativeInteractions::new("epoch".into(), Limits::default());
    let mut held = Vec::new();
    for index in 0..64 {
        let error = IvyError::new("original_unknown", "Reconcile original native state")
            .with_outcome(Outcome::Unknown);
        let (original, observer) = start(&cache, &format!("unknown-{index}"), Err(error));
        assert_eq!(observer.wait().await.unwrap_err().code, "original_unknown");
        held.push(original);
    }
    refused(cache.run(
        "caller".into(),
        "denied".into(),
        "method".into(),
        IvyJson::Null,
        |_| async { panic!("Full retained count must refuse new work") },
    ));
    for index in 0..64 {
        let original = cache
            .lookup(&"caller".into(), &format!("unknown-{index}").into())
            .expect("Evicting a caller-held cached original cannot release its slot");
        assert_eq!(original.wait().await.unwrap_err().code, "original_unknown");
    }
    drop(held.pop());
    cache
        .run(
            "caller".into(),
            "recovered".into(),
            "method".into(),
            IvyJson::Null,
            |_| async { Ok(IvyJson::Null) },
        )
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert!(
        cache
            .lookup(&"caller".into(), &"unknown-63".into())
            .is_err()
    );
    for index in 0..63 {
        assert!(
            cache
                .lookup(&"caller".into(), &format!("unknown-{index}").into())
                .is_ok()
        );
    }
    assert_eq!(
        cache
            .run(
                "caller".into(),
                "unknown-0".into(),
                "method".into(),
                true.into(),
                |_| async { panic!("Known original conflict must precede full admission") }
            )
            .unwrap_err()
            .code,
        "mutation_conflict"
    );
}

#[tokio::test]
async fn a_lost_producer_releases_input_accounting_but_keeps_its_unknown_original_protected() {
    let now = Arc::new(AtomicU64::new(100));
    let clock = now.clone();
    let cache = NativeInteractions::with_clock(
        "epoch".into(),
        Limits {
            count: 4,
            bytes: 20 * 1024 * 1024,
            ttl_ms: 100,
        },
        Arc::new(move || clock.load(Ordering::SeqCst)),
    );
    let id = JsString(vec![0xd800, 65]);
    let effects = Arc::new(AtomicUsize::new(0));
    let counted = effects.clone();
    let original = cache
        .run(
            "caller".into(),
            id.clone(),
            "method".into(),
            payload(16),
            move |request| async move {
                assert!(request.request_id.starts_with("interaction-"));
                assert!(request.detach_on_timeout && request.interaction_priority);
                counted.fetch_add(1, Ordering::SeqCst);
                drop(request);
                panic!("Fixture interaction producer lost after accepting the request");
            },
        )
        .unwrap();
    let duplicate = cache
        .run(
            "caller".into(),
            id.clone(),
            "method".into(),
            payload(16),
            |_| async { panic!("An original interaction must dispatch only once") },
        )
        .unwrap();
    let error = original.wait().await.unwrap_err();
    assert_eq!(error.outcome, Outcome::Unknown);
    assert_eq!(duplicate.wait().await.unwrap_err(), error);
    now.store(10_000, Ordering::SeqCst);
    assert_eq!(
        cache.prune().unwrap(),
        0,
        "A lost producer is not a completed native effect"
    );
    assert_eq!(
        cache
            .lookup(&"caller".into(), &id)
            .unwrap()
            .wait()
            .await
            .unwrap_err(),
        error
    );
    let next = match cache.run(
        "caller".into(),
        "next".into(),
        "method".into(),
        payload(6),
        |_| async { Ok(IvyJson::Null) },
    ) {
        Ok(run) => run,
        Err(_) => {
            panic!("Lost-producer input bytes must be released without losing the unknown original")
        }
    };
    next.wait().await.unwrap();
    for index in 0..6 {
        cache
            .run(
                "caller".into(),
                format!("next-{index}").into(),
                "method".into(),
                IvyJson::Null,
                |_| async { Ok(IvyJson::Null) },
            )
            .unwrap()
            .wait()
            .await
            .unwrap();
    }
    assert_eq!(
        cache
            .run(
                "caller".into(),
                id.clone(),
                "method".into(),
                payload(16),
                |_| async { panic!("Unknown original must never be replayed after input release") }
            )
            .unwrap()
            .wait()
            .await
            .unwrap_err(),
        error
    );
    assert_eq!(
        cache
            .run(
                "caller".into(),
                id.clone(),
                "method".into(),
                IvyJson::Null,
                |_| async { panic!("Conflicting original must never dispatch") }
            )
            .unwrap_err()
            .code,
        "mutation_conflict"
    );
    let missing = cache.lookup(&"other-caller".into(), &id).unwrap_err();
    assert_eq!(missing.code, "interaction_expired");
    assert_eq!(missing.outcome, Outcome::Unknown);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
}
