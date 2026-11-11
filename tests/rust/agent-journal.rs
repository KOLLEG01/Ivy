use ivy_agent_manager::native_journal::{Identity, Limits, NativeJournal, Owner};
use ivy_agent_manager::notifications::NativeNotifications;
use ivy_contracts::{
    IvyJson, JsString,
    json::{parse, stringify},
    limits::NATIVE_FRAME_BYTES,
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::{
        Arc,
        atomic::{AtomicIsize, AtomicU64, Ordering},
    },
};

struct Counted;
static LIVE_BYTES: AtomicIsize = AtomicIsize::new(0);
unsafe impl GlobalAlloc for Counted {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            LIVE_BYTES.fetch_add(layout.size() as isize, Ordering::Relaxed);
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        LIVE_BYTES.fetch_sub(layout.size() as isize, Ordering::Relaxed);
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let next = unsafe { System.realloc(pointer, layout, size) };
        if !next.is_null() {
            LIVE_BYTES.fetch_add(size as isize - layout.size() as isize, Ordering::Relaxed);
        }
        next
    }
}
#[global_allocator]
static ALLOCATOR: Counted = Counted;
const MIB: usize = 1024 * 1024;

fn json(value: &str) -> IvyJson {
    parse(value, usize::MAX).unwrap()
}
fn journal(maximum: usize) -> NativeJournal {
    let ticks = AtomicU64::new(0);
    NativeJournal::with_clock(
        Owner {
            service_node_id: "fixture.agent-manager".into(),
            native_version: "0.154.0".into(),
            native_executable_hash: format!("sha256:{}", "0".repeat(64)).into(),
        },
        Limits {
            max_operations: 1024,
            max_journal_bytes: maximum,
            max_pending_inputs: 16,
        },
        Arc::new(move || {
            let tick = ticks.fetch_add(1, Ordering::Relaxed);
            format!("2026-01-01T00:00:{:02}.{:03}Z", tick / 1000, tick % 1000).into()
        }),
    )
    .unwrap()
}
fn id(value: &str) -> Identity {
    Identity {
        caller_principal_id: "caller".into(),
        operation_id: value.into(),
    }
}
fn input_id(epoch: &str, number: usize) -> IvyJson {
    IvyJson::object([
        ("serviceNodeId", "fixture.agent-manager".into()),
        ("epoch", epoch.into()),
        ("requestId", IvyJson::Number(number as f64)),
    ])
}
fn bytes(journal: &NativeJournal) -> usize {
    journal.status().get("bytes").unwrap().as_f64().unwrap() as usize
}

#[test]
fn scoped_inputs_reject_equal_request_ids_from_another_account_and_resolve_only_their_owner() {
    use ivy_contracts::native_backend::{BackendIdentity, NativeProvider};
    let selected =
        BackendIdentity::new("primary".into(), NativeProvider::Claude, "one".into()).unwrap();
    let foreign =
        BackendIdentity::new("primary".into(), NativeProvider::Claude, "two".into()).unwrap();
    let mut scoped = NativeJournal::new_scoped(
        Owner {
            service_node_id: "fixture.agent-manager".into(),
            native_version: "0.142.3".into(),
            native_executable_hash: format!("sha256:{}", "0".repeat(64)).into(),
        },
        Limits {
            max_operations: 100,
            max_journal_bytes: 64 * MIB,
            max_pending_inputs: 16,
        },
        Some(selected.clone()),
    )
    .unwrap();
    scoped.begin_epoch(&"epoch".into()).unwrap();
    let identity = scoped.input_identity(&"epoch".into(), &1.0.into());
    assert!(BackendIdentity::matches(&identity, Some(&selected)));
    let input = scoped
        .observe_input(
            &identity,
            &"item/tool/requestUserInput".into(),
            &json(r#"{"threadId":"thread","turnId":"turn"}"#),
        )
        .unwrap();
    assert_eq!(input.required("identity").unwrap(), &identity);
    let mut wrong = identity.clone();
    wrong.insert("backend", foreign.json());
    assert_eq!(scoped.input(&wrong).unwrap_err().code, "target_conflict");
    wrong.remove("backend");
    assert_eq!(scoped.input(&wrong).unwrap_err().code, "target_conflict");
    let resolved = scoped
        .resolve_input(&"epoch".into(), &1.0.into(), &"thread".into())
        .unwrap()
        .unwrap();
    assert!(resolved.required("state").unwrap().is_str("expired"));
    assert_eq!(resolved.required("identity").unwrap(), &identity);
}
fn finish(journal: &mut NativeJournal, identity: &Identity, reply: &IvyJson) -> IvyJson {
    journal
        .dispatch(identity, &"epoch".into(), &1.0.into())
        .unwrap();
    journal
        .finish(identity, &"epoch".into(), &1.0.into(), reply)
        .unwrap()
}
fn items(page: &IvyJson) -> &[IvyJson] {
    page.get("items").unwrap().as_array().unwrap()
}

#[test]
fn logical_release_retains_other_callers_without_manufacturing_native_evidence() {
    let mut journal = journal(64 * MIB);
    let epoch = JsString::from("epoch");
    let thread = JsString::from("thread");
    let params = json(r#"{"threadId":"thread"}"#);
    let reply = json(r#"{"result":{"thread":{"id":"thread"}}}"#);
    journal.begin_epoch(&epoch).unwrap();
    let one = id("one");
    let two = Identity {
        caller_principal_id: "foreign".into(),
        operation_id: "two".into(),
    };
    for identity in [&one, &two] {
        journal
            .observe_thread_reply(&epoch, identity, &"thread/resume".into(), &params, &reply)
            .unwrap();
    }
    let release = id("release");
    journal
        .accept(&release, &"agent.release".into(), &params)
        .unwrap();
    assert_eq!(
        journal
            .finish_logical_release(&release, &thread, &epoch, &one.operation_id)
            .unwrap_err()
            .code,
        "native_thread_busy"
    );
    journal
        .observe_thread_status(&epoch, &thread, Some(&json(r#"{"type":"idle"}"#)))
        .unwrap();
    let saved = journal
        .finish_logical_release(&release, &thread, &epoch, &one.operation_id)
        .unwrap();
    ivy_contracts::agent_schema::validate_agent("Operation", &saved).unwrap();
    assert!(saved.required("reply").unwrap().is_null());
    assert!(saved.required("requestId").unwrap().is_null());
    assert!(
        saved
            .required("release")
            .unwrap()
            .required("kind")
            .unwrap()
            .is_str("logical")
    );
    assert!(
        journal
            .thread_subscription(&thread, &one.caller_principal_id)
            .unwrap()
            .is_none()
    );
    let remaining = journal
        .thread_subscription(&thread, &two.caller_principal_id)
        .unwrap()
        .unwrap();
    assert!(remaining.attached && remaining.settled && !remaining.shared);
    assert_eq!(remaining.operation_id, &two.operation_id);
    let mut forged = saved.clone();
    forged.insert("method", "thread/unsubscribe".into());
    assert!(ivy_contracts::agent_schema::validate_agent("Operation", &forged).is_err());
    let mut forged = saved;
    forged.insert("requestId", "invented".into());
    assert!(ivy_contracts::agent_schema::validate_agent("Operation", &forged).is_err());
}

#[test]
fn release_requires_current_owner_epoch_settled_work_and_no_pending_input() {
    let mut journal = journal(64 * MIB);
    let epoch = JsString::from("epoch");
    let thread = JsString::from("thread");
    let params = json(r#"{"threadId":"thread"}"#);
    let own = id("resume");
    let release = id("release");
    journal.begin_epoch(&epoch).unwrap();
    journal
        .observe_thread_reply(
            &epoch,
            &own,
            &"thread/resume".into(),
            &params,
            &json(r#"{"result":{"thread":{"id":"thread"}}}"#),
        )
        .unwrap();
    assert!(
        !journal
            .check_thread_release(&thread, &release, &epoch, &own.operation_id)
            .unwrap()
    );
    journal
        .observe_thread_status(&epoch, &thread, Some(&json(r#"{"type":"idle"}"#)))
        .unwrap();
    journal.check_thread_idle(&thread).unwrap();
    journal
        .observe_thread_status(&epoch, &thread, None)
        .unwrap();
    assert_eq!(
        journal.check_thread_idle(&thread).unwrap_err().code,
        "native_thread_busy"
    );
    assert_eq!(
        journal
            .check_thread_release(&thread, &release, &"old".into(), &own.operation_id)
            .unwrap_err()
            .code,
        "native_state_conflict"
    );
    assert_eq!(
        journal
            .check_thread_release(&thread, &release, &epoch, &"old".into())
            .unwrap_err()
            .code,
        "native_subscription_changed"
    );
    let input = input_id("epoch", 13);
    journal
        .observe_input(&input, &"approval".into(), &params)
        .unwrap();
    assert_eq!(
        journal
            .check_thread_release(&thread, &release, &epoch, &own.operation_id)
            .unwrap_err()
            .code,
        "native_thread_busy"
    );
    journal
        .resolve_input_json(&epoch, &13.0.into(), &"thread".into())
        .unwrap();
    journal
        .observe_thread_dispatch(&epoch, &id("new-turn"), &"turn/start".into(), &params)
        .unwrap();
    assert_eq!(
        journal
            .check_thread_release(&thread, &release, &epoch, &own.operation_id)
            .unwrap_err()
            .code,
        "native_subscription_changed"
    );
    assert_eq!(
        journal
            .check_thread_release(&thread, &release, &epoch, &"new-turn".into())
            .unwrap_err()
            .code,
        "native_subscription_changed"
    );
    journal.lose_epoch(&epoch, &"lost".into()).unwrap();
    journal.begin_epoch(&"next".into()).unwrap();
    assert_eq!(
        journal
            .check_thread_release(&thread, &release, &"next".into(), &own.operation_id)
            .unwrap_err()
            .code,
        "native_subscription_unknown"
    );
}

#[test]
fn logical_release_callback_failure_keeps_the_committed_effect_and_original_receipt_together() {
    let mut journal = journal(64 * MIB);
    let epoch = JsString::from("epoch");
    let thread = JsString::from("thread");
    journal.begin_epoch(&epoch).unwrap();
    let own = id("resume");
    let other = Identity {
        caller_principal_id: "foreign".into(),
        operation_id: "other-resume".into(),
    };
    let params = json(r#"{"threadId":"thread"}"#);
    for identity in [&own, &other] {
        journal
            .observe_thread_reply(
                &epoch,
                identity,
                &"thread/resume".into(),
                &params,
                &json(r#"{"result":{"thread":{"id":"thread"}}}"#),
            )
            .unwrap();
    }
    journal
        .observe_thread_status(&epoch, &thread, Some(&json(r#"{"type":"idle"}"#)))
        .unwrap();
    let release = id("release");
    journal
        .accept(&release, &"agent.release".into(), &params)
        .unwrap();
    journal.set_on_terminal(Some(Box::new(|_| {
        Err(ivy_contracts::IvyError::new(
            "observer_failed",
            "Fixture observer failed after the commit.",
        ))
    })));
    assert_eq!(
        journal
            .finish_logical_release(&release, &thread, &epoch, &own.operation_id)
            .unwrap_err()
            .code,
        "observer_failed"
    );
    let saved = journal.get(&release).unwrap().unwrap();
    ivy_contracts::agent_schema::validate_agent("Operation", &saved).unwrap();
    assert!(saved.required("phase").unwrap().is_str("succeeded"));
    assert!(
        saved.required("requestId").unwrap().is_null()
            && saved.required("reply").unwrap().is_null()
    );
    assert!(
        journal
            .thread_subscription(&thread, &own.caller_principal_id)
            .unwrap()
            .is_none()
    );
    assert!(
        journal
            .thread_subscription(&thread, &other.caller_principal_id)
            .unwrap()
            .is_some()
    );
}

#[test]
fn connection_subscriptions_outlive_receipt_compaction_and_keep_foreign_callers() {
    let mut journal = journal(64 * MIB);
    let epoch = JsString::from("epoch");
    let thread = JsString::from("thread");
    let params = json(r#"{"threadId":"thread"}"#);
    let reply = json(r#"{"result":{"thread":{"id":"thread"}}}"#);
    journal.begin_epoch(&epoch).unwrap();
    let own = id("own-resume");
    journal
        .accept(&own, &"thread/resume".into(), &params)
        .unwrap();
    finish(&mut journal, &own, &reply);
    let observed = journal
        .thread_subscription(&thread, &own.caller_principal_id)
        .unwrap()
        .unwrap();
    assert!(observed.attached && observed.settled && !observed.shared);
    assert_eq!(observed.operation_id, &own.operation_id);
    journal.retained_in_hive(&own).unwrap();
    journal
        .prevent(&id("next"), &"account/logout".into(), &IvyJson::Null)
        .unwrap();
    assert!(journal.get(&own).unwrap().is_none());
    assert!(
        journal
            .thread_subscription(&thread, &own.caller_principal_id)
            .unwrap()
            .unwrap()
            .attached
    );

    let foreign = Identity {
        caller_principal_id: "another-caller".into(),
        operation_id: "foreign-resume".into(),
    };
    journal
        .accept(&foreign, &"thread/resume".into(), &params)
        .unwrap();
    journal.dispatch(&foreign, &epoch, &2.0.into()).unwrap();
    let observed = journal
        .thread_subscription(&thread, &foreign.caller_principal_id)
        .unwrap()
        .unwrap();
    assert!(observed.shared && !observed.attached && !observed.settled);
    journal
        .finish(&foreign, &epoch, &2.0.into(), &reply)
        .unwrap();
    let observed = journal
        .thread_subscription(&thread, &foreign.caller_principal_id)
        .unwrap()
        .unwrap();
    assert!(observed.shared && observed.attached && observed.settled);
    assert!(
        journal
            .thread_subscription(&thread, &own.caller_principal_id)
            .unwrap()
            .unwrap()
            .shared
    );
    journal.forget(&foreign).unwrap();
    assert!(
        journal
            .thread_subscription(&thread, &own.caller_principal_id)
            .unwrap()
            .unwrap()
            .shared
    );
    journal.lose_epoch(&epoch, &"closed".into()).unwrap();
    journal.begin_epoch(&"replacement".into()).unwrap();
    journal
        .observe_thread_reply(&epoch, &foreign, &"thread/resume".into(), &params, &reply)
        .unwrap();
    assert!(
        journal
            .thread_subscription(&thread, &own.caller_principal_id)
            .unwrap()
            .is_none()
    );
    assert!(
        journal
            .thread_subscription(&thread, &foreign.caller_principal_id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn pending_and_late_native_requests_preserve_the_newest_context_identity() {
    let mut journal = journal(64 * MIB);
    let epoch = JsString::from("epoch");
    let thread = JsString::from("thread");
    let params = json(r#"{"threadId":"thread"}"#);
    journal.begin_epoch(&epoch).unwrap();
    journal
        .observe_thread_reply(
            &epoch,
            &id("resume"),
            &"thread/resume".into(),
            &params,
            &json(r#"{"result":{"thread":{"id":"thread"}}}"#),
        )
        .unwrap();
    let older = id("older");
    let newer = id("newer");
    for identity in [&older, &newer] {
        journal
            .observe_thread_dispatch(&epoch, identity, &"turn/start".into(), &params)
            .unwrap();
    }
    // Transient interactions may lose their HTTP observer and receive replies
    // in a different order. They still share this connection and attribution.
    journal
        .observe_thread_reply(
            &epoch,
            &newer,
            &"turn/start".into(),
            &params,
            &json(r#"{"result":{}}"#),
        )
        .unwrap();
    let observed = journal
        .thread_subscription(&thread, &newer.caller_principal_id)
        .unwrap()
        .unwrap();
    assert!(observed.attached && !observed.settled);
    assert_eq!(observed.operation_id, &newer.operation_id);
    journal
        .observe_thread_reply(
            &epoch,
            &older,
            &"turn/start".into(),
            &params,
            &json(r#"{"result":{}}"#),
        )
        .unwrap();
    let observed = journal
        .thread_subscription(&thread, &newer.caller_principal_id)
        .unwrap()
        .unwrap();
    assert!(observed.settled);
    assert_eq!(observed.operation_id, &newer.operation_id);
    journal
        .observe_thread_dispatch(&epoch, &id("unknown"), &"turn/start".into(), &params)
        .unwrap();
    assert!(
        !journal
            .thread_subscription(&thread, &newer.caller_principal_id)
            .unwrap()
            .unwrap()
            .settled
    );
}

#[test]
fn raw_unsubscribe_releases_connection_attribution_for_all_callers_only_on_its_native_result() {
    let mut journal = journal(64 * MIB);
    let epoch = JsString::from("epoch");
    let thread = JsString::from("thread");
    let other = JsString::from("other-thread");
    let params = json(r#"{"threadId":"thread"}"#);
    journal.begin_epoch(&epoch).unwrap();
    for (caller, thread_id) in [
        ("one", "thread"),
        ("two", "thread"),
        ("one", "other-thread"),
    ] {
        let identity = Identity {
            caller_principal_id: caller.into(),
            operation_id: format!("{caller}-{thread_id}").into(),
        };
        journal
            .observe_thread_reply(
                &epoch,
                &identity,
                &"thread/resume".into(),
                &IvyJson::object([("threadId", thread_id.into())]),
                &IvyJson::object([(
                    "result",
                    IvyJson::object([("thread", IvyJson::object([("id", thread_id.into())]))]),
                )]),
            )
            .unwrap();
    }
    let identity = Identity {
        caller_principal_id: "one".into(),
        operation_id: "unsubscribe".into(),
    };
    journal
        .observe_thread_reply(
            &epoch,
            &identity,
            &"thread/unsubscribe".into(),
            &params,
            &json(r#"{"error":{"code":-1,"message":"failed"}}"#),
        )
        .unwrap();
    assert!(
        journal
            .thread_subscription(&thread, &"one".into())
            .unwrap()
            .unwrap()
            .shared
    );
    journal
        .observe_thread_reply(
            &"old-epoch".into(),
            &identity,
            &"thread/unsubscribe".into(),
            &params,
            &json(r#"{"result":{"status":"unsubscribed"}}"#),
        )
        .unwrap();
    assert!(
        journal
            .thread_subscription(&thread, &"two".into())
            .unwrap()
            .is_some()
    );
    journal
        .observe_thread_reply(
            &epoch,
            &identity,
            &"thread/unsubscribe".into(),
            &params,
            &json(r#"{"result":{"status":"unsubscribed"}}"#),
        )
        .unwrap();
    assert!(
        journal
            .thread_subscription(&thread, &"one".into())
            .unwrap()
            .is_none()
    );
    assert!(
        journal
            .thread_subscription(&thread, &"two".into())
            .unwrap()
            .is_none()
    );
    assert!(
        journal
            .thread_subscription(&other, &"one".into())
            .unwrap()
            .is_some()
    );
}

#[test]
fn incomplete_subscription_attribution_is_bounded_and_never_looks_exclusive() {
    let mut journal = journal(32 * 1024);
    let epoch = JsString::from("epoch");
    let thread = JsString::from("thread");
    let params = json(r#"{"threadId":"thread"}"#);
    journal.begin_epoch(&epoch).unwrap();
    journal
        .observe_thread_reply(
            &epoch,
            &id("own"),
            &"thread/resume".into(),
            &params,
            &json(r#"{"result":{"thread":{"id":"thread"}}}"#),
        )
        .unwrap();
    assert!(
        journal
            .thread_subscription(&thread, &"caller".into())
            .unwrap()
            .is_some()
    );
    for number in 0..256 {
        let value = JsString::from(format!("{number}-{}", "t".repeat(500)));
        journal
            .observe_thread_dispatch(
                &epoch,
                &id(&format!("work-{number}")),
                &"turn/start".into(),
                &IvyJson::object([("threadId", IvyJson::String(value))]),
            )
            .unwrap();
        assert!(bytes(&journal) <= 32 * 1024);
    }
    assert!(
        journal
            .thread_subscription(&thread, &"caller".into())
            .unwrap()
            .is_none()
    );
    journal
        .observe_thread_reply(
            &epoch,
            &id("later"),
            &"thread/resume".into(),
            &params,
            &json(r#"{"result":{"thread":{"id":"thread"}}}"#),
        )
        .unwrap();
    assert!(
        journal
            .thread_subscription(&thread, &"caller".into())
            .unwrap()
            .is_none()
    );
    journal.lose_epoch(&epoch, &"closed".into()).unwrap();
    journal.begin_epoch(&"new".into()).unwrap();
    journal
        .observe_thread_reply(
            &"new".into(),
            &id("fresh"),
            &"thread/resume".into(),
            &params,
            &json(r#"{"result":{"thread":{"id":"thread"}}}"#),
        )
        .unwrap();
    assert!(
        journal
            .thread_subscription(&thread, &"caller".into())
            .unwrap()
            .is_some()
    );
    journal.close();
    assert_eq!(bytes(&journal), 0);
}

#[test]
fn read_only_and_unattributable_native_replies_cannot_establish_release_ownership() {
    let mut journal = journal(64 * MIB);
    let epoch = JsString::from("epoch");
    let thread = JsString::from("thread");
    let params = json(r#"{"threadId":"thread"}"#);
    let reply = json(r#"{"result":{"thread":{"id":"thread"}}}"#);
    journal.begin_epoch(&epoch).unwrap();
    journal
        .observe_thread_dispatch(&epoch, &id("read"), &"thread/read".into(), &params)
        .unwrap();
    journal
        .observe_thread_reply(&epoch, &id("read"), &"thread/read".into(), &params, &reply)
        .unwrap();
    assert!(
        journal
            .thread_subscription(&thread, &"caller".into())
            .unwrap()
            .is_none()
    );
    journal
        .observe_thread_dispatch(&epoch, &id("resume"), &"thread/resume".into(), &params)
        .unwrap();
    journal
        .observe_thread_reply(
            &epoch,
            &id("resume"),
            &"thread/resume".into(),
            &params,
            &reply,
        )
        .unwrap();
    assert!(
        journal
            .thread_subscription(&thread, &"caller".into())
            .unwrap()
            .unwrap()
            .attached
    );
    journal
        .observe_thread_dispatch(&epoch, &id("later-read"), &"thread/read".into(), &params)
        .unwrap();
    journal
        .observe_thread_reply(
            &epoch,
            &id("later-read"),
            &"thread/read".into(),
            &params,
            &reply,
        )
        .unwrap();
    assert_eq!(
        journal
            .thread_subscription(&thread, &"caller".into())
            .unwrap()
            .unwrap()
            .operation_id,
        &JsString::from("resume")
    );
    journal
        .observe_thread_reply(
            &epoch,
            &id("contradictory-resume"),
            &"thread/resume".into(),
            &params,
            &json(r#"{"result":{"thread":{"id":"different-thread"}}}"#),
        )
        .unwrap();
    assert!(
        journal
            .thread_subscription(&thread, &"caller".into())
            .unwrap()
            .is_none()
    );
    assert!(
        journal
            .thread_subscription(&"different-thread".into(), &"caller".into())
            .unwrap()
            .is_none()
    );
    journal.lose_epoch(&epoch, &"closed".into()).unwrap();
    let fresh = JsString::from("fresh");
    journal.begin_epoch(&fresh).unwrap();
    journal
        .observe_thread_reply(&fresh, &id("own"), &"thread/resume".into(), &params, &reply)
        .unwrap();
    assert!(
        journal
            .thread_subscription(&thread, &"caller".into())
            .unwrap()
            .is_some()
    );
    journal
        .observe_thread_dispatch(
            &fresh,
            &id("unattributable"),
            &"thread/resume".into(),
            &json(r#"{"threadId":null}"#),
        )
        .unwrap();
    assert!(
        journal
            .thread_subscription(&thread, &"caller".into())
            .unwrap()
            .is_none()
    );
}

#[test]
fn retained_allocations_fit_reported_budget_and_lossless_outcomes_replay() {
    // Measure one process with no other test threads or unrelated allocations.
    const CHILD: &str = "IVY_JOURNAL_MEMORY_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "retained_allocations_fit_reported_budget_and_lossless_outcomes_replay",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let mut journal = journal(64 * MIB);
    journal.begin_epoch(&"epoch".into()).unwrap();
    journal
        .accept(&id("warm"), &"method".into(), &IvyJson::Null)
        .unwrap();
    drop(finish(
        &mut journal,
        &id("warm"),
        &json(r#"{"result":null}"#),
    ));
    journal.close();
    journal.begin_epoch(&"epoch".into()).unwrap();

    let params = json(r#"{"key\ud800":"\udfff","unicode":"ä😀"}"#);
    let reply = IvyJson::object([(
        "result",
        IvyJson::Array(vec![IvyJson::Number(0.0); 256 * 1024]),
    )]);
    let identity = id("dense");
    let method = JsString::from("method");
    let before = LIVE_BYTES.load(Ordering::Relaxed);
    drop(journal.accept(&identity, &method, &params).unwrap());
    drop(finish(&mut journal, &identity, &reply));
    let retained = LIVE_BYTES.load(Ordering::Relaxed) - before;
    let charged = bytes(&journal);
    assert!(retained > 0);
    assert!(
        retained as usize <= charged + 64 * 1024,
        "retained={retained}, charged={charged}"
    );
    assert!(
        charged < MIB,
        "dense payload expanded in the journal: {charged}"
    );

    let input_identity = input_id("epoch", 1);
    let before_input = LIVE_BYTES.load(Ordering::Relaxed);
    drop(
        journal
            .observe_input(&input_identity, &method, reply.get("result").unwrap())
            .unwrap(),
    );
    let retained_input = LIVE_BYTES.load(Ordering::Relaxed) - before_input;
    assert!(
        retained_input > 0 && retained_input < MIB as isize,
        "input expanded: {retained_input}"
    );
    journal
        .resolve_input_json(&"epoch".into(), &1.0.into(), &IvyJson::Null)
        .unwrap();

    let (replayed, admitted) = journal.accept(&identity, &method, &params).unwrap();
    assert!(!admitted);
    assert_eq!(replayed.get("params"), Some(&params));
    assert_eq!(replayed.get("reply"), Some(&reply));
    assert_eq!(
        journal
            .accept(&identity, &method, &true.into())
            .unwrap_err()
            .code,
        "mutation_conflict"
    );
    drop(replayed);
    drop(
        journal
            .accept(&id("after"), &method, &IvyJson::Null)
            .unwrap(),
    );
    assert!(
        journal.get(&identity).unwrap().is_some(),
        "undurable outcome was compacted"
    );
    journal.retained_in_hive(&identity).unwrap();
    drop(
        journal
            .accept(&id("cleanup"), &"thread/unsubscribe".into(), &IvyJson::Null)
            .unwrap(),
    );
    assert!(journal.get(&identity).unwrap().is_none());
    assert!(journal.get(&id("after")).unwrap().is_some());
    journal.close();
    assert_eq!(bytes(&journal), 0);
    assert!(LIVE_BYTES.load(Ordering::Relaxed) <= before + 64 * 1024);
}

#[test]
fn native_thread_observers_do_not_copy_oversized_ids_or_unrelated_request_payloads() {
    use ivy_agent_manager::native_dispatch::thread_observation_params;
    for oversized in [false, true] {
        let original = JsString(vec![65; 2 * MIB]);
        let pointer = original.0.as_ptr();
        let params = if oversized {
            IvyJson::object([
                ("threadId", IvyJson::String(original)),
                ("payload", IvyJson::Null),
            ])
        } else {
            IvyJson::object([
                ("threadId", "thread".into()),
                ("payload", IvyJson::String(original)),
            ])
        };
        let before = LIVE_BYTES.load(Ordering::Relaxed);
        let observed = thread_observation_params(&params);
        let retained = LIVE_BYTES.load(Ordering::Relaxed) - before;
        assert!(
            retained < 1024,
            "Observer retained request body bytes: {retained}"
        );
        assert_eq!(
            observed.get("threadId"),
            Some(&if oversized {
                IvyJson::Null
            } else {
                "thread".into()
            })
        );
        assert!(observed.get("payload").is_none());
        assert_eq!(
            params
                .required(if oversized { "threadId" } else { "payload" })
                .unwrap()
                .as_js_string()
                .unwrap()
                .0
                .as_ptr(),
            pointer
        );
    }
}

#[test]
fn row_keys_and_container_growth_count_towards_admission() {
    let identity = id("prevented");
    let mut generous = journal(MIB);
    let value = generous
        .prevent(&identity, &"method".into(), &IvyJson::Null)
        .unwrap();
    let encoded = stringify(&value, usize::MAX).unwrap().len();
    assert!(bytes(&generous) > encoded);
    let mut tight = journal(encoded);
    assert_eq!(
        tight
            .prevent(&identity, &"method".into(), &IvyJson::Null)
            .unwrap_err()
            .code,
        "native_journal_capacity"
    );
    assert!(tight.get(&identity).unwrap().is_none());
    assert_eq!(bytes(&tight), 0);
}

#[test]
fn running_calls_keep_cleanup_space_and_full_native_reply_reservations() {
    let mut journal = journal(64 * MIB);
    journal.begin_epoch(&"epoch".into()).unwrap();
    journal
        .accept(&id("first"), &"method".into(), &IvyJson::Null)
        .unwrap();
    assert_eq!(
        journal
            .accept(&id("full"), &"method".into(), &IvyJson::Null)
            .unwrap_err()
            .code,
        "native_journal_capacity"
    );
    assert!(journal.get(&id("first")).unwrap().is_some());
    assert!(journal.get(&id("full")).unwrap().is_none());
    journal
        .accept(&id("cleanup"), &"thread/unsubscribe".into(), &IvyJson::Null)
        .unwrap();
    let reply = IvyJson::object([("result", "x".repeat(NATIVE_FRAME_BYTES - 1024).into())]);
    let completed = finish(&mut journal, &id("first"), &reply);
    assert_eq!(completed.get("reply"), Some(&reply));
    assert!(bytes(&journal) <= 64 * MIB);
    assert_eq!(journal.get(&id("first")).unwrap(), Some(completed));
    journal.retained_in_hive(&id("first")).unwrap();
    // A durable terminal may be compacted; the other accepted call survives.
    assert!(
        journal
            .accept(&id("still-full"), &"method".into(), &IvyJson::Null)
            .is_err()
    );
    assert!(journal.get(&id("first")).unwrap().is_none());
    assert_eq!(
        journal.get(&id("cleanup")).unwrap().unwrap().get("phase"),
        Some(&"accepted".into())
    );
}

#[test]
fn epoch_loss_preserves_operation_and_input_outcome_states() {
    let mut journal = journal(128 * MIB);
    journal.begin_epoch(&"epoch".into()).unwrap();
    let pending = input_id("epoch", 1);
    let answering = input_id("epoch", 2);
    for identity in [&pending, &answering] {
        journal
            .observe_input(
                identity,
                &"approval".into(),
                &json(r#"{"threadId":"thread"}"#),
            )
            .unwrap();
    }
    journal
        .accept(&id("accepted"), &"method".into(), &IvyJson::Null)
        .unwrap();
    journal
        .accept(&id("sent"), &"method".into(), &IvyJson::Null)
        .unwrap();
    journal
        .dispatch(&id("sent"), &"epoch".into(), &3.0.into())
        .unwrap();
    let reply = json(r#"{"result":true}"#);
    let answer_params =
        IvyJson::object([("identity", answering.clone()), ("reply", reply.clone())]);
    journal
        .accept(&id("answer"), &"agent.answer".into(), &answer_params)
        .unwrap();
    journal
        .dispatch_answer(&id("answer"), &answering, &reply)
        .unwrap();
    journal
        .lose_epoch(&"epoch".into(), &"connection_lost".into())
        .unwrap();
    let accepted = journal.get(&id("accepted")).unwrap().unwrap();
    assert_eq!(accepted.get("phase"), Some(&"failed".into()));
    assert_eq!(accepted.get("code"), Some(&"request_not_dispatched".into()));
    for identity in [id("sent"), id("answer")] {
        assert_eq!(
            journal.get(&identity).unwrap().unwrap().get("phase"),
            Some(&"outcome_unknown".into())
        );
    }
    assert_eq!(
        journal.input(&pending).unwrap().unwrap().get("state"),
        Some(&"expired".into())
    );
    assert_eq!(
        journal.input(&answering).unwrap().unwrap().get("state"),
        Some(&"outcome_unknown".into())
    );
    assert_eq!(journal.pending_input_count(), 0);
    assert!(items(&journal.inputs(&json("{}")).unwrap()).is_empty());
    assert_eq!(
        items(&journal.inputs(&json(r#"{"includeExpired":true}"#)).unwrap()).len(),
        2
    );
}

#[test]
fn input_pages_keep_pending_first_order_and_exact_identity_reads() {
    let mut journal = journal(128 * MIB);
    journal.begin_epoch(&"epoch".into()).unwrap();
    let identities = (1..=3).map(|n| input_id("epoch", n)).collect::<Vec<_>>();
    for identity in &identities {
        journal
            .observe_input(identity, &"approval".into(), &IvyJson::Null)
            .unwrap();
    }
    journal
        .resolve_input_json(&"epoch".into(), &1.0.into(), &IvyJson::Null)
        .unwrap();
    let page = journal
        .inputs(&json(r#"{"includeExpired":true,"limit":2}"#))
        .unwrap();
    assert_eq!(
        items(&page)
            .iter()
            .map(|v| v.get("identity").unwrap())
            .collect::<Vec<_>>(),
        vec![&identities[2], &identities[1]]
    );
    assert_eq!(page.get("truncated"), Some(&true.into()));
    let all = journal.inputs(&json(r#"{"includeExpired":true}"#)).unwrap();
    assert_eq!(items(&all)[2].get("identity"), Some(&identities[0]));
    let exact = journal
        .inputs(&IvyJson::object([("identity", identities[0].clone())]))
        .unwrap();
    assert_eq!(items(&exact)[0].get("state"), Some(&"expired".into()));
    assert_eq!(exact.get("truncated"), Some(&false.into()));
}

#[test]
fn input_pages_obey_the_wire_budget_with_large_raw_rows() {
    let mut journal = journal(128 * MIB);
    journal.begin_epoch(&"epoch".into()).unwrap();
    let params = IvyJson::from("x".repeat(17 * MIB));
    for number in 1..=2 {
        journal
            .observe_input(&input_id("epoch", number), &"approval".into(), &params)
            .unwrap();
    }
    let page = journal.inputs(&json("{}")).unwrap();
    assert_eq!(items(&page).len(), 1);
    assert_eq!(items(&page)[0].get("identity"), Some(&input_id("epoch", 2)));
    assert_eq!(items(&page)[0].get("params"), Some(&params));
    assert_eq!(page.get("truncated"), Some(&true.into()));
    assert_eq!(journal.pending_input_count(), 2);
    assert!(bytes(&journal) <= 128 * MIB);
}

#[test]
fn method_observations_are_bounded_and_keep_the_last_failure_on_success() {
    let mut journal = journal(64 * MIB);
    journal.begin_epoch(&"epoch".into()).unwrap();
    journal
        .accept(&id("failed"), &"same".into(), &IvyJson::Null)
        .unwrap();
    journal
        .reject_before_dispatch(&id("failed"), &"blocked".into())
        .unwrap();
    journal
        .accept(&id("succeeded"), &"same".into(), &IvyJson::Null)
        .unwrap();
    finish(&mut journal, &id("succeeded"), &json(r#"{"result":null}"#));
    let observed = journal.observed_methods();
    let observed = &observed.as_array().unwrap()[0];
    assert_eq!(observed.get("lastCode"), Some(&"blocked".into()));
    assert!(!observed.get("lastSucceededAt").unwrap().is_null());
    assert!(!observed.get("lastFailedAt").unwrap().is_null());

    for n in 0..300 {
        let identity = id(&format!("observed-{n}"));
        let method = format!("method-{n}/{}", "x".repeat(4096)).into();
        journal.accept(&identity, &method, &IvyJson::Null).unwrap();
        journal
            .reject_before_dispatch(&identity, &"blocked".into())
            .unwrap();
        journal.retained_in_hive(&identity).unwrap();
    }
    assert!(journal.observed_methods().as_array().unwrap().len() < 256);
    assert!(bytes(&journal) < 2 * MIB);
    let oversized = JsString::from("x".repeat(32 * 1024));
    journal
        .accept(&id("oversized"), &oversized, &IvyJson::Null)
        .unwrap();
    journal
        .reject_before_dispatch(&id("oversized"), &"blocked".into())
        .unwrap();
    assert!(journal.get(&id("oversized")).unwrap().is_some());
    assert!(
        !journal
            .observed_methods()
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.get("method") == Some(&IvyJson::String(oversized.clone())))
    );
}

#[test]
fn notification_storage_bounds_payloads_and_spare_slots() {
    const CHILD: &str = "IVY_NOTIFICATION_MEMORY_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "notification_storage_bounds_payloads_and_spare_slots",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let params = IvyJson::Array(vec![IvyJson::Null; 4096]);
    let maximum = 64 * 1024;
    let baseline = LIVE_BYTES.load(Ordering::Relaxed);
    let mut events =
        NativeNotifications::new("fixture".into(), "0.154.0".into(), 0, 10_000, maximum);
    for _ in 0..16 {
        drop(
            events
                .observe(
                    "epoch",
                    "method",
                    params.clone(),
                    "2026-01-01T00:00:00.000Z",
                )
                .unwrap(),
        );
        let retained = LIVE_BYTES.load(Ordering::Relaxed) - baseline;
        assert!(
            retained > 0 && retained <= (maximum + 1024) as isize,
            "retained={retained}, maximum={maximum}"
        );
    }
    let page = events.page(0, 100, None).unwrap();
    assert!(page.get("gap") == Some(&true.into()));
    assert_eq!(items(&page).last().unwrap().get("params"), Some(&params));
    drop(page);
    // Small rows exercise container growth and eviction after large payloads.
    for _ in 0..1024 {
        drop(
            events
                .observe("epoch", "method", IvyJson::Null, "2026-01-01T00:00:00.000Z")
                .unwrap(),
        );
        assert!(LIVE_BYTES.load(Ordering::Relaxed) - baseline <= (maximum + 1024) as isize);
    }
    drop(events);
    assert!((LIVE_BYTES.load(Ordering::Relaxed) - baseline).abs() < 1024);
    for maximum in [0, 1, 32, 256, 512] {
        let mut events =
            NativeNotifications::new("fixture".into(), "0.154.0".into(), 0, 10_000, maximum);
        for _ in 0..8 {
            drop(
                events
                    .observe("epoch", "method", IvyJson::Null, "2026-01-01T00:00:00.000Z")
                    .unwrap(),
            );
        }
        assert!(LIVE_BYTES.load(Ordering::Relaxed) - baseline <= (maximum + 1024) as isize);
    }
}
