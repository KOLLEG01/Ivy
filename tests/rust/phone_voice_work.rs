use super::*;
use crate::journal::{PhoneJournalLimits, PhoneJournalOptions, PhoneJournalOwner};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicI64, Ordering},
};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ivy-phone-work-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn options(&self) -> PhoneJournalOptions {
        PhoneJournalOptions {
            data_root: self.0.clone(),
            owner: PhoneJournalOwner {
                host_id: "fixture-host".into(),
                service_node_id: "fixture.phone".into(),
            },
            limits: PhoneJournalLimits::default(),
            max_concurrent_calls: 2,
        }
    }
    async fn open(&self) -> PhoneJournal {
        PhoneJournal::open(self.options()).await.unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn call(id: &str) -> Json {
    Json::object([
        ("callId", id.into()),
        ("principalId", "caller".into()),
        ("policyHash", format!("sha256:{}", "a".repeat(64)).into()),
    ])
}
fn state(work: &Json) -> String {
    work.required("state").unwrap().as_str().unwrap()
}

#[tokio::test]
async fn matching_native_terminal_outcome_resolves_uncertainty_after_restart() {
    for (native_status, expected_state) in [
        ("completed", "completed"),
        ("interrupted", "paused_deadline"),
        ("failed", "failed"),
    ] {
        let fixture = Fixture::new();
        let journal = fixture.open().await;
        let works = PhoneVoiceWorks::with_clock(journal.clone(), Arc::new(|| 1_800_000_000_000));
        let call = call("10000000-0000-4000-8000-000000000033");
        let thread: JsString = "matching-thread".into();
        let turn: JsString = "matching-turn".into();
        works
            .started(Some(&call), &thread, &turn, "owner", "party")
            .await
            .unwrap();
        works.waiting(&thread).await.unwrap();
        works
            .detach(&text(&call, "callId").unwrap(), Some(1_799_999_700_000))
            .await
            .unwrap();
        let claim = works.claim_expired(false).await.unwrap().pop().unwrap();
        assert!(works.begin_interrupt(&claim).await.unwrap());
        let pending = works.get(&thread).unwrap();
        assert_eq!(
            pending.get("error"),
            Some(&"phone_work_interrupt_pending".into())
        );
        works
            .completed(&thread, &"older-turn".into(), &"completed".into())
            .await
            .unwrap();
        assert_eq!(works.get(&thread), Some(pending));

        works
            .completed(&thread, &turn, &native_status.into())
            .await
            .unwrap();
        let confirmed = works.get(&thread).unwrap();
        assert_eq!(state(&confirmed), expected_state);
        assert_eq!(confirmed.get("error"), Some(&Json::Null));
        assert_eq!(confirmed.get("deadline"), claim.get("deadline"));
        assert_eq!(confirmed.get("interruptId"), claim.get("interruptId"));
        works
            .interrupted(&claim, "late_native_timeout")
            .await
            .unwrap();
        assert_eq!(works.get(&thread), Some(confirmed.clone()));
        journal.close().wait().await.unwrap();
        drop(works);

        let journal = fixture.open().await;
        let restored = PhoneVoiceWorks::with_clock(journal.clone(), Arc::new(|| 1_800_000_999_999));
        restored.initialize().await.unwrap();
        assert_eq!(restored.get(&thread), Some(confirmed));
        assert!(restored.claim_expired(true).await.unwrap().is_empty());
        journal.close().wait().await.unwrap();
    }
}

#[tokio::test]
async fn original_deadline_rejoin_and_restart_keep_the_native_turn_identity() {
    let fixture = Fixture::new();
    let journal = fixture.open().await;
    let clock = Arc::new(AtomicI64::new(1_800_000_000_000));
    let now = clock.clone();
    let works = PhoneVoiceWorks::with_clock(
        journal.clone(),
        Arc::new(move || now.load(Ordering::SeqCst)),
    );
    let first = call("10000000-0000-4000-8000-000000000001");
    let second = call("10000000-0000-4000-8000-000000000002");
    let id = text(&first, "callId").unwrap();
    let thread: JsString = "original-thread".into();
    let turn: JsString = "original-turn".into();
    works
        .started(Some(&first), &thread, &turn, "owner", "party")
        .await
        .unwrap();
    works.waiting(&thread).await.unwrap();
    let ended = clock.load(Ordering::SeqCst) - 20_000;
    works.detach(&id, Some(ended)).await.unwrap();
    works
        .detach(&id, Some(clock.load(Ordering::SeqCst)))
        .await
        .unwrap();
    let original = works.get(&thread).unwrap();
    assert_eq!(
        original.get("deadline"),
        Some(&Json::from((ended + WORK_WINDOW_MS) as f64))
    );
    assert!(works.can_answer(&original));
    assert!(
        works
            .rejoin(&second, "owner", "another-party")
            .unwrap()
            .is_none()
    );
    let mut wrong = second.clone();
    wrong.insert("policyHash", "changed".into());
    assert!(works.rejoin(&wrong, "owner", "party").unwrap().is_none());
    assert!(works.rejoin(&second, "owner", "party").unwrap().is_some());
    // Exact expiry wins before a reconnect or a pending-input reply can proceed.
    clock.store(ended + WORK_WINDOW_MS, Ordering::SeqCst);
    assert!(works.rejoin(&second, "owner", "party").unwrap().is_none());
    assert!(!works.can_answer(&original));
    assert_eq!(
        works.attach(&thread, &second).await.unwrap_err().code,
        "phone_work_unavailable"
    );
    let claim = works.claim_expired(false).await.unwrap().pop().unwrap();
    assert_eq!(state(&claim), "halting");
    assert_eq!(
        works.claim_expired(false).await.unwrap()[0].get("interruptId"),
        claim.get("interruptId")
    );
    works
        .completed(&thread, &"different-turn".into(), &"completed".into())
        .await
        .unwrap();
    assert_eq!(state(&works.get(&thread).unwrap()), "halting");
    works
        .interrupted(&claim, "native_observation_deadline")
        .await
        .unwrap();
    assert_eq!(state(&works.get(&thread).unwrap()), "outcome_unknown");
    // Only an observation of this original turn confirms the deadline pause.
    works
        .completed(&thread, &turn, &"interrupted".into())
        .await
        .unwrap();
    assert_eq!(state(&works.get(&thread).unwrap()), "paused_deadline");
    assert!(works.protected(&thread));
    works
        .started(
            Some(&second),
            &"second-thread".into(),
            &"second-turn".into(),
            "owner",
            "party",
        )
        .await
        .unwrap();
    journal.close().wait().await.unwrap();
    drop(works);
    let journal = fixture.open().await;
    let now = clock.clone();
    let restored = PhoneVoiceWorks::with_clock(
        journal.clone(),
        Arc::new(move || now.load(Ordering::SeqCst)),
    );
    restored.initialize().await.unwrap();
    let work = restored.get(&"second-thread".into()).unwrap();
    assert_eq!(work.get("attachedCallId"), Some(&Json::Null));
    assert_eq!(
        work.get("deadline"),
        Some(&Json::from(clock.load(Ordering::SeqCst) as f64))
    );
    assert_eq!(
        restored.claim_expired(false).await.unwrap()[0].get("turnId"),
        Some(&Json::from("second-turn"))
    );
    journal.close().wait().await.unwrap();
}

#[tokio::test]
async fn a_failed_work_commit_cannot_publish_a_new_claim_or_extend_a_deadline() {
    let fixture = Fixture::new();
    let journal = fixture.open().await;
    let works = PhoneVoiceWorks::with_clock(journal.clone(), Arc::new(|| 1_800_000_000_000));
    let call = call("10000000-0000-4000-8000-000000000003");
    let thread: JsString = "thread".into();
    works
        .started(Some(&call), &thread, &"turn".into(), "owner", "party")
        .await
        .unwrap();
    works
        .detach(&text(&call, "callId").unwrap(), None)
        .await
        .unwrap();
    let before = works.get(&thread).unwrap();
    journal.fixture_db(|db|{db.execute_batch("CREATE TRIGGER fail_work BEFORE UPDATE ON voice_works BEGIN SELECT RAISE(ABORT,'injected work failure'); END;").map_err(ivy_store::sqlite::error)}).await.unwrap();
    assert!(works.claim_expired(false).await.is_err());
    assert_eq!(works.get(&thread).unwrap(), before);
    journal.close().wait().await.unwrap();
}

#[tokio::test]
async fn bounded_work_updates_only_the_selected_row_and_idle_checks_do_not_write() {
    let fixture = Fixture::new();
    let journal = fixture.open().await;
    let works = PhoneVoiceWorks::with_clock(journal.clone(), Arc::new(|| 1_800_000_000_000));
    let call = call("10000000-0000-4000-8000-000000000003");
    for index in 0..RECORD_LIMIT {
        works
            .started(
                Some(&call),
                &format!("thread-{index}").into(),
                &"turn".into(),
                "owner",
                "party",
            )
            .await
            .unwrap();
    }
    journal.fixture_db(|db| db.execute_batch("CREATE TABLE work_audit(thread TEXT); CREATE TRIGGER audit_work AFTER UPDATE ON voice_works BEGIN INSERT INTO work_audit VALUES(NEW.thread_id); END;").map_err(ivy_store::sqlite::error)).await.unwrap();
    works.waiting(&"thread-31".into()).await.unwrap();
    let revision = works.get(&"thread-31".into()).unwrap();
    for _ in 0..100 {
        assert!(works.has_work());
        assert!(works.next_deadline().is_none());
        assert!(works.claim_expired(false).await.unwrap().is_empty());
        assert_eq!(works.get(&"thread-31".into()), Some(revision.clone()));
    }
    works.waiting(&"thread-31".into()).await.unwrap();
    let changed = journal
        .fixture_db(|db| {
            db.query_row(
                "SELECT count(*) FROM work_audit WHERE thread='thread-31'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map_err(ivy_store::sqlite::error)
        })
        .await
        .unwrap();
    assert_eq!(changed, 1);
    journal.close().wait().await.unwrap();
}

#[tokio::test]
async fn dropping_an_admitted_observer_cannot_separate_commit_from_cache_publication() {
    let fixture = Fixture::new();
    let journal = fixture.open().await;
    let works = PhoneVoiceWorks::with_clock(journal.clone(), Arc::new(|| 1_800_000_000_000));
    works
        .started(
            Some(&call("10000000-0000-4000-8000-000000000003")),
            &"thread".into(),
            &"turn".into(),
            "owner",
            "party",
        )
        .await
        .unwrap();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let owned = works.clone();
    let observer = tokio::spawn(async move {
        owned
            .edit(move |records| {
                let mut row = records.get(&"thread".into()).unwrap().clone();
                row.state = WorkState::WaitingInput;
                row.bump()?;
                let _ = entered.send(());
                blocked.recv().unwrap();
                Ok((Changes::work(row), ()))
            })
            .await
    });
    ready.await.unwrap();
    observer.abort();
    release.send(()).unwrap();
    let state = journal
        .fixture_db(|db| {
            db.query_row(
                "SELECT json_extract(value,'$.state') FROM voice_works WHERE thread_id='thread'",
                [],
                |r| r.get::<_, String>(0),
            )
            .map_err(ivy_store::sqlite::error)
        })
        .await
        .unwrap();
    assert_eq!(state, "waiting_input");
    assert_eq!(
        super::tests::state(&works.get(&"thread".into()).unwrap()),
        state
    );
    journal.close().wait().await.unwrap();
}

#[tokio::test]
async fn the_current_saved_arrays_split_atomically_without_extending_detached_work() {
    let fixture = Fixture::new();
    let journal = fixture.open().await;
    let now = 1_800_000_000_000;
    let works = PhoneVoiceWorks::with_clock(journal.clone(), Arc::new(move || now));
    let call = call("10000000-0000-4000-8000-000000000003");
    works
        .started(
            Some(&call),
            &"thread".into(),
            &"original-turn".into(),
            "owner",
            "party",
        )
        .await
        .unwrap();
    works.waiting(&"thread".into()).await.unwrap();
    works
        .detach(&text(&call, "callId").unwrap(), Some(now - 20_000))
        .await
        .unwrap();
    let original = works.get(&"thread".into()).unwrap();
    let saved =
        ivy_contracts::json::canonical(&Json::Array(vec![original.clone()]), 256 * 1024).unwrap();
    journal.fixture_db(move |db| {
        db.execute("INSERT INTO meta(key,value) VALUES('voiceWorks',?)", [saved]).map_err(ivy_store::sqlite::error)?;
        db.execute_batch("INSERT INTO meta VALUES('voiceObservations','[]'); DELETE FROM voice_works; PRAGMA user_version=7;").map_err(ivy_store::sqlite::error)
    }).await.unwrap();
    journal.close().wait().await.unwrap();
    drop(works);
    for _ in 0..2 {
        let upgraded = PhoneJournal::upgrade_voice_storage(fixture.options())
            .await
            .unwrap();
        assert_eq!(upgraded.get("format"), Some(&8.0.into()));
        assert_eq!(upgraded.get("works"), Some(&1.0.into()));
        let journal = fixture.open().await;
        let restored = PhoneVoiceWorks::with_clock(journal.clone(), Arc::new(move || now + 30_000));
        restored.initialize().await.unwrap();
        assert_eq!(restored.get(&"thread".into()), Some(original.clone()));
        journal
            .fixture_db(|db| {
                assert_eq!(
                    db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                        .unwrap(),
                    8
                );
                assert_eq!(
                    db.query_row(
                        "SELECT count(*) FROM meta WHERE key IN ('voiceWorks','voiceObservations')",
                        [],
                        |r| r.get::<_, i64>(0)
                    )
                    .unwrap(),
                    0
                );
                Ok(())
            })
            .await
            .unwrap();
        journal.close().wait().await.unwrap();
    }
}

#[tokio::test]
async fn offline_upgrade_requires_the_existing_unlocked_original_owner() {
    let fixture = Fixture::new();
    assert_eq!(
        PhoneJournal::upgrade_voice_storage(fixture.options())
            .await
            .unwrap_err()
            .code,
        "phone_work_storage_missing"
    );
    assert!(!fixture.0.join("phone-commands.sqlite").exists());
    let journal = fixture.open().await;
    assert_eq!(
        PhoneJournal::upgrade_voice_storage(fixture.options())
            .await
            .unwrap_err()
            .code,
        "phone_owner_already_running"
    );
    journal.close().wait().await.unwrap();
    let mut foreign = fixture.options();
    foreign.owner.service_node_id = "another.phone".into();
    assert_eq!(
        PhoneJournal::upgrade_voice_storage(foreign)
            .await
            .unwrap_err()
            .code,
        "target_conflict"
    );
    let result = PhoneJournal::upgrade_voice_storage(fixture.options())
        .await
        .unwrap();
    assert_eq!(result.get("format"), Some(&8.0.into()));
}

#[tokio::test]
async fn interrupt_dispatch_is_durable_and_never_reclaimed_after_a_lost_observer() {
    let fixture = Fixture::new();
    let journal = fixture.open().await;
    let works = PhoneVoiceWorks::with_clock(journal.clone(), Arc::new(|| 1_800_000_000_000));
    let original_call = call("10000000-0000-4000-8000-000000000022");
    let call_id = text(&original_call, "callId").unwrap();
    let thread: JsString = "original-thread".into();
    let turn: JsString = "original-turn".into();
    works
        .started(Some(&original_call), &thread, &turn, "owner", "party")
        .await
        .unwrap();
    works
        .detach(&call_id, Some(1_799_999_000_000))
        .await
        .unwrap();
    let claim = works.claim_expired(false).await.unwrap().pop().unwrap();
    journal.fixture_db(|db| {
        db.execute_batch("CREATE TRIGGER fail_dispatch BEFORE UPDATE ON voice_works BEGIN SELECT RAISE(ABORT,'dispatch commit failed'); END;").map_err(ivy_store::sqlite::error)
    }).await.unwrap();
    assert!(works.begin_interrupt(&claim).await.is_err());
    assert_eq!(
        state(&works.get(&thread).unwrap()),
        "halting",
        "a failed commit cannot admit the external interrupt"
    );
    journal
        .fixture_db(|db| {
            db.execute_batch("DROP TRIGGER fail_dispatch;")
                .map_err(ivy_store::sqlite::error)
        })
        .await
        .unwrap();
    assert!(works.begin_interrupt(&claim).await.unwrap());
    assert!(!works.begin_interrupt(&claim).await.unwrap());
    let original = works.get(&thread).unwrap();
    assert_eq!(state(&original), "outcome_unknown");
    assert_eq!(original.get("interruptId"), claim.get("interruptId"));
    assert_eq!(original.get("deadline"), claim.get("deadline"));
    assert!(works.claim_expired(true).await.unwrap().is_empty());
    journal.close().wait().await.unwrap();
    drop(works);
    let journal = fixture.open().await;
    let restored = PhoneVoiceWorks::with_clock(journal.clone(), Arc::new(|| 1_800_000_999_999));
    restored.initialize().await.unwrap();
    assert_eq!(restored.get(&thread), Some(original));
    assert!(restored.claim_expired(true).await.unwrap().is_empty());
    assert!(!restored.begin_interrupt(&claim).await.unwrap());
    restored
        .interrupted(&claim, "native_observation_deadline")
        .await
        .unwrap();
    assert_eq!(
        restored.get(&thread).unwrap().get("error"),
        Some(&"native_observation_deadline".into())
    );
    restored
        .completed(&thread, &turn, &"interrupted".into())
        .await
        .unwrap();
    assert_eq!(state(&restored.get(&thread).unwrap()), "paused_deadline");
    journal.close().wait().await.unwrap();
}

#[tokio::test]
async fn malformed_offline_upgrade_keeps_the_original_arrays_and_format() {
    let fixture = Fixture::new();
    let journal = fixture.open().await;
    let original = r#"[{"threadId":"original-thread","revision":"unreadable"}]"#;
    journal.fixture_db(move |db| {
        db.execute("INSERT INTO meta(key,value) VALUES('voiceWorks',?)", [original]).map_err(ivy_store::sqlite::error)?;
        db.execute_batch("INSERT INTO meta VALUES('voiceObservations','[]'); DROP TABLE voice_works; DROP TABLE voice_observations; PRAGMA user_version=7;").map_err(ivy_store::sqlite::error)
    }).await.unwrap();
    journal.close().wait().await.unwrap();
    assert!(
        PhoneJournal::upgrade_voice_storage(fixture.options())
            .await
            .is_err()
    );
    let db =
        ivy_store::rusqlite::Connection::open(fixture.0.join("phone-commands.sqlite")).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        7
    );
    assert_eq!(
        db.query_row("SELECT value FROM meta WHERE key='voiceWorks'", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap(),
        original
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name IN ('voice_works','voice_observations')",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[tokio::test]
async fn separate_rows_preserve_the_original_total_byte_budget_and_rollback_admission() {
    let fixture = Fixture::new();
    let journal = fixture.open().await;
    let works = PhoneVoiceWorks::with_clock(journal.clone(), Arc::new(|| 1_800_000_000_000));
    let call = call("10000000-0000-4000-8000-000000000003");
    let party = "x".repeat(120_000);
    for id in ["one", "two"] {
        works
            .started(Some(&call), &id.into(), &"turn".into(), "owner", &party)
            .await
            .unwrap();
    }
    let before = works.get(&"one".into()).unwrap();
    assert_eq!(
        works
            .started(
                Some(&call),
                &"three".into(),
                &"turn".into(),
                "owner",
                &party
            )
            .await
            .unwrap_err()
            .code,
        "phone_work_capacity"
    );
    assert!(works.get(&"three".into()).is_none());
    assert_eq!(works.get(&"one".into()), Some(before));
    let bytes = journal
        .fixture_db(|db| {
            let (count, bytes) = db
                .query_row(
                    "SELECT count(*),sum(length(CAST(value AS BLOB))) FROM voice_works",
                    [],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
                )
                .map_err(ivy_store::sqlite::error)?;
            assert_eq!(count, 2);
            Ok((bytes + count + 1) as usize)
        })
        .await
        .unwrap();
    assert!(bytes <= WORK_BYTES);
    let owned = works.0.records.lock().unwrap();
    assert_eq!(owned.work_bytes + 1, bytes);
    drop(owned);
    journal.close().wait().await.unwrap();
}
