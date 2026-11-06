use super::{Published, catalog, connection, journal};
use ivy_agent_manager::{
    inventory::NativeInventory,
    inventory_manager::{InventoryManager, InventoryRun},
    notice_policy::NativeNoticePolicy,
};
use ivy_contracts::{IvyError, IvyJson, JsString, Outcome};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, Semaphore};

enum Read {
    Error(usize),
    Reply(IvyJson),
    Block(Arc<Gate>),
    Panic,
}
struct Gate {
    entered: Notify,
    release: Semaphore,
}
impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: Notify::new(),
            release: Semaphore::new(0),
        })
    }
}
struct Fixture {
    manager: InventoryManager,
    client: Arc<Published>,
    reads: Arc<Mutex<VecDeque<Read>>>,
    calls: Arc<AtomicUsize>,
    pointers: Arc<Mutex<Vec<usize>>>,
}
impl Fixture {
    fn new() -> Self {
        let reads = Arc::new(Mutex::new(VecDeque::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let pointers = Arc::new(Mutex::new(vec![]));
        let inventory = NativeInventory::with_reader(
            &catalog(),
            "epoch".into(),
            Arc::new({
                let reads = reads.clone();
                let calls = calls.clone();
                let pointers = pointers.clone();
                move |_, _, _| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let read = reads.lock().unwrap().pop_front().unwrap_or(Read::Error(0));
                    let pointers = pointers.clone();
                    Box::pin(async move {
                        let bytes = match read {
                            Read::Error(bytes) => bytes,
                            Read::Reply(value) => return Ok(value),
                            Read::Block(gate) => {
                                gate.entered.notify_one();
                                gate.release.acquire().await.unwrap().forget();
                                0
                            }
                            Read::Panic => panic!("fixture inventory producer lost"),
                        };
                        let mut value = Vec::with_capacity(bytes / 2);
                        value.extend([65, 0xd800, 66, 0xdfff]);
                        pointers.lock().unwrap().push(value.as_ptr() as usize);
                        Err(IvyError::new("fixture_inventory_error", "original")
                            .with_outcome(Outcome::Unknown)
                            .with_details(IvyJson::String(JsString(value))))
                    })
                }
            }),
            Arc::new(|| 0.0),
            Arc::new(|| "2026-01-01T00:00:00.000Z".into()),
        )
        .unwrap();
        let manager = InventoryManager::with_clock(
            Arc::new(inventory),
            journal(),
            Arc::new(Mutex::new(NativeNoticePolicy::default())),
            Arc::new(|_| Ok(())),
            Arc::new(AtomicBool::new(false)),
            Arc::new(|| 0.0),
        )
        .unwrap();
        let client = Arc::new(Published::default());
        manager.bind(connection(client.clone())).unwrap();
        Self {
            manager,
            client,
            reads,
            calls,
            pointers,
        }
    }
    fn start(&self, projects: bool) -> Result<InventoryRun, IvyError> {
        if projects {
            self.manager
                .refresh_projects(connection(self.client.clone()))
        } else {
            self.manager
                .refresh_threads(connection(self.client.clone()))
        }
    }
    async fn error(&self, projects: bool, bytes: usize) -> InventoryRun {
        self.reads.lock().unwrap().push_back(Read::Error(bytes));
        let run = self.start(projects).unwrap();
        assert_eq!(
            run.clone().wait().await.unwrap_err().code,
            "fixture_inventory_error"
        );
        run
    }
}
fn refused(result: Result<InventoryRun, IvyError>) {
    match result {
        Err(error) => {
            assert_eq!(error.code, "native_capacity");
            assert_eq!(error.outcome, Outcome::NotExecuted);
        }
        Ok(_) => panic!("Unread inventory results must retain their bounded admission"),
    }
}

#[tokio::test]
async fn unread_inventory_results_share_one_limit_across_both_lanes_and_release_on_drop() {
    let f = Fixture::new();
    let mut held = Vec::new();
    for index in 0..64 {
        held.push(f.error(index % 2 == 0, 0).await);
    }
    assert_eq!(f.calls.load(Ordering::SeqCst), 64);
    refused(f.start(true));
    refused(
        f.manager
            .clone()
            .refresh_threads(connection(f.client.clone())),
    );
    assert_eq!(
        f.calls.load(Ordering::SeqCst),
        64,
        "refusal precedes the native reader"
    );
    f.manager.drain().await.unwrap();
    drop(held.pop());
    let last = f.error(false, 0).await;
    assert_eq!(
        last.clone().wait().await.unwrap_err().outcome,
        Outcome::Unknown
    );
    refused(f.start(true));
    drop(last);
    drop(held);
    for _ in 0..80 {
        f.error(true, 0).await.wait().await.unwrap_err();
    }
    assert!(f.client.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn inventory_errors_keep_actual_capacity_until_the_last_receiver_takes_the_original() {
    let f = Fixture::new();
    let first = f.error(true, 30 * 1024 * 1024).await;
    let second = f.error(false, 30 * 1024 * 1024).await;
    f.reads
        .lock()
        .unwrap()
        .push_back(Read::Error(30 * 1024 * 1024));
    let limited = f.start(true).unwrap().wait().await.unwrap_err();
    assert_eq!(limited.code, "agent_reply_unavailable");
    assert_eq!(limited.outcome, Outcome::Unknown);
    let original = first.wait().await.unwrap_err();
    let units = original.details.as_ref().unwrap().as_js_string().unwrap();
    assert_eq!(units.0, [65, 0xd800, 66, 0xdfff]);
    assert!(units.0.capacity() * 2 >= 30 * 1024 * 1024);
    assert_eq!(units.0.as_ptr() as usize, f.pointers.lock().unwrap()[0]);
    // A returned caller-owned error no longer consumes the coordinator's pool.
    let recovered = f.error(true, 30 * 1024 * 1024).await;
    f.reads
        .lock()
        .unwrap()
        .push_back(Read::Error(49 * 1024 * 1024));
    let oversized = f.start(false).unwrap().wait().await.unwrap_err();
    assert_eq!(oversized.code, "agent_reply_unavailable");
    assert_eq!(oversized.outcome, Outcome::Unknown);
    assert_eq!(
        second.wait().await.unwrap_err().code,
        "fixture_inventory_error"
    );
    assert_eq!(
        recovered.wait().await.unwrap_err().code,
        "fixture_inventory_error"
    );
    f.manager.drain().await.unwrap();
    assert_eq!(
        f.calls.load(Ordering::SeqCst),
        5,
        "no capacity failure replays a read"
    );
}

#[tokio::test]
async fn coalesced_inventory_follow_up_survives_full_unread_capacity_until_the_existing_tick() {
    let f = Fixture::new();
    let mut held = Vec::new();
    for _ in 0..63 {
        held.push(f.error(true, 0).await);
    }
    let gate = Gate::new();
    f.reads.lock().unwrap().push_back(Read::Block(gate.clone()));
    let current = f.start(true).unwrap();
    tokio::time::timeout(Duration::from_secs(2), gate.entered.notified())
        .await
        .unwrap();
    for _ in 0..100 {
        drop(f.start(true).unwrap());
    }
    gate.release.add_permits(1);
    assert_eq!(
        current.clone().wait().await.unwrap_err().code,
        "fixture_inventory_error"
    );
    assert_eq!(
        f.calls.load(Ordering::SeqCst),
        64,
        "queued follow-up waits for capacity"
    );
    refused(
        f.manager
            .refresh_due(connection(f.client.clone()))
            .map(|_| current.clone()),
    );
    drop(held.pop());
    f.manager.refresh_due(connection(f.client.clone())).unwrap();
    tokio::time::timeout(Duration::from_secs(2), f.manager.drain())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        f.calls.load(Ordering::SeqCst),
        65,
        "one queued read at the same clock time"
    );
    f.manager.refresh_due(connection(f.client.clone())).unwrap();
    assert_eq!(f.calls.load(Ordering::SeqCst), 65);
    assert_eq!(
        current.wait().await.unwrap_err().code,
        "fixture_inventory_error"
    );
}

#[tokio::test]
async fn a_lost_inventory_producer_releases_its_lane_and_preserves_the_original_unknown() {
    let f = Fixture::new();
    f.reads.lock().unwrap().push_back(Read::Panic);
    let original = f.start(true).unwrap();
    let error = original.clone().wait().await.unwrap_err();
    assert_eq!(error.code, "internal_error");
    assert_eq!(error.outcome, Outcome::Unknown);
    let later = f.start(true).unwrap().wait().await.unwrap_err();
    assert_eq!(
        later.code, "fixture_inventory_error",
        "the failed reader cannot pin its lane"
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    assert_eq!(original.wait().await.unwrap_err(), error);
    tokio::time::timeout(Duration::from_secs(2), f.manager.drain())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        f.manager.code().unwrap(),
        Some("fixture_inventory_error".into())
    );
    assert!(f.client.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn retained_status_reports_the_published_unknown_when_the_original_result_cannot_fit() {
    use ivy_contracts::{json::canonical_size, limits::MANAGEMENT_FRAME_BYTES};
    use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient, response_value};
    fn frame(code: JsString, details: IvyJson) -> IvyJson {
        IvyJson::object([
            ("jsonrpc", "2.0".into()),
            ("id", "publication".into()),
            (
                "error",
                IvyJson::object([
                    ("code", (-32603.0).into()),
                    ("message", "publication lost".into()),
                    (
                        "data",
                        IvyJson::object([
                            ("code", IvyJson::String(code)),
                            ("outcome", "unknown".into()),
                            ("details", details),
                        ]),
                    ),
                ]),
            ),
        ])
    }
    struct Publication {
        frames: Mutex<VecDeque<IvyJson>>,
        calls: AtomicUsize,
    }
    impl RpcClient for Publication {
        fn request<'a>(
            &'a self,
            method: &'a str,
            _: IvyJson,
            _: RequestOptions,
        ) -> RequestFuture<'a> {
            assert_eq!(method, "inventory.sync");
            self.calls.fetch_add(1, Ordering::SeqCst);
            let frame = self.frames.lock().unwrap().pop_front().unwrap();
            Box::pin(async move { response_value(&frame, "publication") })
        }
    }
    // The real Hive decoder already bounds status codes before copying them.
    let code = JsString(vec![0xd800; 128]);
    let invalid = response_value(
        &frame(JsString(vec![0xd800; 129]), IvyJson::Null),
        "publication",
    )
    .unwrap_err();
    assert_eq!(invalid.code, "invalid_frame");
    assert_eq!(invalid.outcome, Outcome::Unknown);
    let details = IvyJson::String(JsString(vec![65; 25 * 1024 * 1024]));
    assert!(details.retained_bytes() > 48 * 1024 * 1024);
    let large = frame(code.clone(), details);
    assert!(canonical_size(&large, MANAGEMENT_FRAME_BYTES).unwrap() < MANAGEMENT_FRAME_BYTES);
    let client = Arc::new(Publication {
        frames: Mutex::new(VecDeque::from([frame(code.clone(), IvyJson::Null), large])),
        calls: AtomicUsize::new(0),
    });
    let f = Fixture::new();
    let mut current = connection(f.client.clone());
    current.client = client.clone();
    f.manager.bind(current.clone()).unwrap();
    for index in 0..2 {
        f.reads
            .lock()
            .unwrap()
            .push_back(Read::Reply(IvyJson::object([(
                "result",
                IvyJson::object([
                    ("data", IvyJson::Array(vec![])),
                    ("nextCursor", IvyJson::Null),
                ]),
            )])));
        let error = f
            .manager
            .refresh_projects(current.clone())
            .unwrap()
            .wait()
            .await
            .unwrap_err();
        if index == 0 {
            assert_eq!(error.code_js(), code);
            assert_eq!(f.manager.code().unwrap(), Some(code.clone()));
        } else {
            assert_eq!(error.code, "agent_reply_unavailable");
            assert_eq!(
                f.manager.code().unwrap(),
                Some(error.code_js()),
                "Health status must describe the result actually published to its original caller"
            );
        }
        assert_eq!(error.outcome, Outcome::Unknown);
    }
    assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        client.calls.load(Ordering::SeqCst),
        2,
        "lost publication is not replayed"
    );
}
