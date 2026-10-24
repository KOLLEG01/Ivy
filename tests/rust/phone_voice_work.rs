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
    async fn open(&self) -> PhoneJournal {
        PhoneJournal::open(PhoneJournalOptions {
            data_root: self.0.clone(),
            owner: PhoneJournalOwner {
                host_id: "fixture-host".into(),
                service_node_id: "fixture.phone".into(),
            },
            limits: PhoneJournalLimits::default(),
            max_concurrent_calls: 2,
        })
        .await
        .unwrap()
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
    journal.fixture_db(|db|{db.execute_batch("CREATE TRIGGER fail_work BEFORE UPDATE ON meta WHEN NEW.key='voiceWorks' BEGIN SELECT RAISE(ABORT,'injected work failure'); END;").map_err(ivy_store::sqlite::error)}).await.unwrap();
    assert!(works.claim_expired(false).await.is_err());
    assert_eq!(works.get(&thread).unwrap(), before);
    journal.close().wait().await.unwrap();
}
