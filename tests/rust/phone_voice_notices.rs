use super::*;
use crate::journal::{PhoneJournalLimits, PhoneJournalOptions, PhoneJournalOwner};
use ivy_contracts::Outcome;
use ivy_sdk::rpc::RequestFuture;
use std::{
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicI64, Ordering},
    },
    time::Instant,
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ivy-phone-notices-{}", uuid::Uuid::new_v4()));
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
            max_concurrent_calls: 1,
        })
        .await
        .unwrap()
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Time(AtomicI64);
impl Clock for Time {
    fn utc_millis(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
#[derive(Default)]
struct Client {
    writes: Mutex<Vec<Json>>,
    reads: Mutex<Vec<String>>,
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            if method == "objects.write" {
                self.writes.lock().unwrap().push(params);
                Err(
                    IvyError::new("deadline_exceeded", "Original write reply was lost.")
                        .with_outcome(Outcome::Unknown),
                )
            } else {
                self.reads.lock().unwrap().push(method.to_owned());
                assert_eq!(method, "system.status");
                Ok(Json::object([("runtimeEpoch", "fixture-epoch".into())]))
            }
        })
    }
}
#[tokio::test]
async fn question_write_retries_keep_one_identity_across_restart_and_stop_when_stale_or_exhausted()
{
    let root = Root::new();
    let journal = root.open().await;
    let clock = Arc::new(Time(AtomicI64::new(1_800_000_000_000)));
    let mock = Arc::new(Client::default());
    let rpc: Arc<dyn RpcClient> = mock.clone();
    let factory: super::Client = Arc::new(move || Ok(rpc.clone()));
    let mut notices = PhoneVoiceNotices {
        client: factory.clone(),
        journal: journal.clone(),
        service_node_id: "fixture.phone".into(),
        clock: clock.clone(),
    };
    let work = Json::object([
        ("threadId", "10000000-0000-4000-8000-000000000001".into()),
        ("turnId", "original-turn".into()),
        ("state", "waiting_input".into()),
        ("attachedCallId", Json::Null),
        ("endedAt", Json::from(1_800_000_000_000.0)),
        ("deadline", Json::from(1_800_000_300_000.0)),
        ("revision", 1.0.into()),
        ("error", Json::Null),
        (
            "callIds",
            Json::Array(vec!["10000000-0000-4000-8000-000000000002".into()]),
        ),
    ]);
    let token = format!("sha256:{}", "a".repeat(64));
    let input = Json::object([
        ("inputToken", token.clone().into()),
        ("id", 7.0.into()),
        ("method", "item/tool/requestUserInput".into()),
    ]);
    assert!(
        notices
            .notify(work.clone(), input.clone(), Arc::new(|| false))
            .await
            .unwrap()
    );
    assert!(mock.writes.lock().unwrap().is_empty());
    assert!(journal.voice_notice(&token).await.unwrap().is_null());
    assert_eq!(
        notices
            .notify(work.clone(), input.clone(), Arc::new(|| true))
            .await
            .unwrap_err()
            .code,
        "deadline_exceeded"
    );
    assert!(
        !notices
            .notify(work.clone(), input.clone(), Arc::new(|| true))
            .await
            .unwrap()
    );
    assert_eq!(mock.writes.lock().unwrap().len(), 1);
    journal.close().wait().await.unwrap();
    let reopened = root.open().await;
    notices = PhoneVoiceNotices {
        client: factory,
        journal: reopened.clone(),
        service_node_id: "fixture.phone".into(),
        clock: clock.clone(),
    };
    for attempt in 2..=6 {
        clock.0.fetch_add(60_000, Ordering::SeqCst);
        let result = notices
            .notify(work.clone(), input.clone(), Arc::new(|| true))
            .await;
        if attempt == 6 {
            assert!(result.unwrap());
        } else {
            assert_eq!(result.unwrap_err().code, "deadline_exceeded");
        }
    }
    assert!(
        notices
            .notify(work, input, Arc::new(|| true))
            .await
            .unwrap()
    );
    let writes = mock.writes.lock().unwrap();
    assert_eq!(writes.len(), 6);
    let original = canonical(&writes[0], usize::MAX).unwrap();
    assert!(
        writes
            .iter()
            .all(|value| canonical(value, usize::MAX).unwrap() == original)
    );
    assert_eq!(writes[0].get("contractVersion"), Some(&"1.1.0".into()));
    assert_eq!(mock.reads.lock().unwrap().as_slice(), ["system.status"]);
    let saved = reopened.voice_notice(&token).await.unwrap();
    assert_eq!(saved.get("attempts"), Some(&6.0.into()));
    assert_eq!(saved.get("submitted"), Some(&false.into()));
    drop(writes);
    reopened.close().wait().await.unwrap();
}
