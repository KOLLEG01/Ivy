use super::*;
use futures_util::{FutureExt, SinkExt, StreamExt};
use ivy_agent_manager::browser_notices::{BrowserNoticeSettings, BrowserNotices};
use ivy_agent_manager::native_operations::ReceiptWork;
use ivy_sdk::service::{ServiceClient, ServiceOptions};
use std::{future::Future, task::Poll};
use tokio::sync::Notify;

async fn until(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

// A real provider connection to an inert local Hive. Hold its read responses,
// rather than blocking the socket, so unrelated control and notifications flow.
struct Hive {
    client: Arc<ServiceClient>,
    hold: Arc<AtomicBool>,
    held: Arc<AtomicUsize>,
    wake: Arc<Notify>,
    published: Arc<AtomicUsize>,
    inputs: Arc<AtomicUsize>,
    fail_publish: Arc<AtomicBool>,
    inventory: Arc<Mutex<Vec<IvyJson>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Hive {
    async fn new(hold_method: &'static str) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let hold = Arc::new(AtomicBool::new(true));
        let held = Arc::new(AtomicUsize::new(0));
        let wake = Arc::new(Notify::new());
        let published = Arc::new(AtomicUsize::new(0));
        let inputs = Arc::new(AtomicUsize::new(0));
        let fail_publish = Arc::new(AtomicBool::new(false));
        let inventory = Arc::new(Mutex::new(Vec::new()));
        let inventory_calls = inventory.clone();
        let (gate, pending, release, sent, observed_inputs, fail) = (
            hold.clone(),
            held.clone(),
            wake.clone(),
            published.clone(),
            inputs.clone(),
            fail_publish.clone(),
        );
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut delayed = Vec::new();
            loop {
                tokio::select! {
                    _ = release.notified() => {
                        for reply in delayed.drain(..) {
                            socket.send(tokio_tungstenite::tungstenite::Message::Text(reply)).await.unwrap();
                            pending.fetch_sub(1, Ordering::AcqRel);
                        }
                    }
                    message = socket.next() => {
                        let Some(Ok(message)) = message else { break; };
                        let Ok(body) = message.into_text() else { continue; };
                        let frame = parse(&body, BYTES).unwrap();
                        let method = field(&frame, "method").as_str().unwrap();
                        let Some(id) = frame.get("id") else {
                            if method == "service.notification" && field(field(&frame,"params"),"name").is_str("inputs") {
                                observed_inputs.fetch_add(1, Ordering::AcqRel);
                            }
                            continue;
                        };
                        let result = match method.as_str() {
                            "service.connect" => IvyJson::object([("generation", 1.0.into())]),
                            "service.heartbeat" => IvyJson::object([("ready", true.into())]),
                            "registry.sync" => IvyJson::object::<&str>([]),
                            "inventory.sync" => {
                                let params = field(&frame, "params");
                                ivy_contracts::core_schema::validate_input("inventory.sync", params).unwrap();
                                inventory_calls.lock().unwrap().push(params.clone());
                                IvyJson::object([
                                    ("snapshotRevision", field(params, "snapshotRevision").clone()),
                                    ("observedAt", field(params, "observedAt").clone()),
                                ])
                            }
                            "system.status" => IvyJson::object([("runtimeEpoch", "fixture-epoch".into())]),
                            "serviceNodes.list" => IvyJson::object([
                                ("items", IvyJson::Array(vec![IvyJson::object([
                                    ("principalId", "workflow".into()), ("serviceName", "fixture-workflow".into()),
                                ])])), ("nextCursor", IvyJson::Null),
                            ]),
                            "events.publish" => {
                                sent.fetch_add(1, Ordering::AcqRel);
                                IvyJson::object::<&str>([])
                            }
                            _ => panic!("Unexpected notice request: {method}"),
                        };
                        let mut reply = IvyJson::object([("jsonrpc", "2.0".into()), ("id", id.clone())]);
                        if method == "events.publish" && fail.load(Ordering::Acquire) {
                            reply.insert("error", IvyError::new("fixture_publish_failed", "Fixture.").to_wire());
                        } else {
                            reply.insert("result", result);
                        }
                        let reply = canonical(&reply, BYTES).unwrap().into();
                        if method == hold_method && gate.load(Ordering::Acquire) {
                            delayed.push(reply);
                            pending.fetch_add(1, Ordering::AcqRel);
                        } else {
                            socket.send(tokio_tungstenite::tungstenite::Message::Text(reply)).await.unwrap();
                        }
                    }
                }
            }
        });
        let options = ServiceOptions::new(
            url,
            Arc::new(|| Box::pin(async { Ok("fixture-only".to_owned()) })),
            field(data(), "identity").clone(),
            Arc::new(|| {
                Box::pin(async {
                    Ok(IvyJson::object([
                        ("namespaces", IvyJson::Array(vec![])),
                        ("contracts", IvyJson::Array(vec![])),
                        ("requiredContracts", IvyJson::Array(vec![])),
                    ]))
                })
            }),
            Arc::new(|_| Box::pin(async { Ok(()) })),
        );
        let client = Arc::new(ServiceClient::new(options).unwrap());
        client.start();
        client
            .wait_ready(Some(RequestOptions {
                timeout: Duration::from_secs(5),
                ..Default::default()
            }))
            .await
            .unwrap();
        Self {
            client,
            inventory,
            hold,
            held,
            wake,
            published,
            inputs,
            fail_publish,
            task,
        }
    }
    fn release(&self) {
        self.hold.store(false, Ordering::Release);
        self.wake.notify_one();
    }
}
impl Drop for Hive {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Fixture {
    root: Root,
    hive: Hive,
    callbacks: Events,
    events: ServiceEvents,
    rpc: NativeRpc,
    provider: tokio::task::JoinHandle<()>,
    journal: Arc<Mutex<NativeJournal>>,
    state: Arc<Mutex<LocalAgentState>>,
    notifications: Arc<Mutex<NativeNotifications>>,
    policy: Arc<Mutex<NativeNoticePolicy>>,
    closing: Arc<AtomicBool>,
    queries: Arc<AtomicUsize>,
    query_mode: Arc<AtomicUsize>,
    receipts: ReceiptWork,
}
impl Fixture {
    async fn new(hold_method: &'static str) -> Self {
        let root = Root::new();
        let hive = Hive::new(hold_method).await;
        let service = ServiceLink::default();
        service.bind(&hive.client).unwrap();
        let journal = journal("0.163.0".into());
        let state = local(&root);
        state
            .lock()
            .unwrap()
            .set_runtime_epoch("epoch".into())
            .unwrap();
        let operations = NativeOperations::new(
            Arc::new(SqliteOperationStore::new(state.clone()).unwrap()),
            journal.clone(),
        );
        let policy = Arc::new(Mutex::new(NativeNoticePolicy::default()));
        let closing = Arc::new(AtomicBool::new(false));
        let notifications = Arc::new(Mutex::new(NativeNotifications::new_js(
            "fixture.agent".into(),
            "0.163.0".into(),
            0,
            9007199254740991,
            2 * 1024 * 1024,
        )));
        let queries = Arc::new(AtomicUsize::new(0));
        let query_mode = Arc::new(AtomicUsize::new(0));
        let receipts = ReceiptWork::default();
        let (calls, mode) = (queries.clone(), query_mode.clone());
        let events = ServiceEvents::new(ServiceEventSettings {
            service_node_id: "fixture.agent".into(),
            service: service.clone(),
            closing: closing.clone(),
            operations: operations.clone(),
            state: state.clone(),
            receipts: receipts.clone(),
            notifications: notifications.clone(),
            policy: policy.clone(),
            query: Arc::new(move |pairs| {
                calls.fetch_add(1, Ordering::AcqRel);
                match mode.load(Ordering::Acquire) {
                    0 => ivy_agent_manager::runtime::form_query(pairs),
                    1 => Err(IvyError::new("fixture_render_failed", "Fixture.")),
                    2 => panic!("Fixture rendering panic"),
                    length => Ok(JsString(vec![120; length])),
                }
            }),
        });
        let callbacks = events.begin_epoch("epoch".into()).unwrap();
        let catalog = Arc::new(catalog("specs/native/codex-0.163.0/catalog.json"));
        let (rpc, mut requests, replies) =
            NativeRpc::start_frames(catalog.clone(), Events::default()).unwrap();
        let provider = tokio::spawn(async move {
            while let Some(frame) = requests.recv().await {
                assert_eq!(field(&frame, "method"), &"thread/read".into());
                replies
                    .try_send(IvyJson::object([
                        ("id", field(&frame, "id").clone()),
                        (
                            "result",
                            IvyJson::object([(
                                "thread",
                                IvyJson::object([
                                    ("id", field(field(&frame, "params"), "threadId").clone()),
                                    ("projectId", IvyJson::Null),
                                ]),
                            )]),
                        ),
                    ]))
                    .unwrap();
            }
        });
        let inventory = InventoryManager::new(
            Arc::new(
                NativeInventory::with_reader(
                    &catalog,
                    "epoch".into(),
                    Arc::new(|method, _, _| {
                        Box::pin(async move {
                            assert!(method.eq_str("project/list"));
                            Ok(IvyJson::object([(
                                "result",
                                IvyJson::object([
                                    ("data", IvyJson::Array(vec![])),
                                    ("nextCursor", IvyJson::Null),
                                ]),
                            )]))
                        })
                    }),
                    Arc::new(|| 1000.0),
                    Arc::new(|| "2026-01-01T00:00:00.000Z".into()),
                )
                .unwrap(),
            ),
            journal.clone(),
            policy.clone(),
            Arc::new(|_| Ok(())),
            closing.clone(),
        )
        .unwrap();
        events.attach_native(rpc.clone()).unwrap();
        events
            .install(
                inventory.clone(),
                BrowserNotices::new(BrowserNoticeSettings {
                    service,
                    closing: closing.clone(),
                    operations,
                    state: state.clone(),
                    inventory,
                    policy: policy.clone(),
                    rpc: rpc.clone(),
                }),
            )
            .unwrap();
        Self {
            root,
            hive,
            callbacks,
            events,
            rpc,
            provider,
            journal,
            state,
            notifications,
            policy,
            closing,
            queries,
            query_mode,
            receipts,
        }
    }
    fn completion(&self, index: usize, status: &str) -> NativeNotification {
        let thread = JsString::from(format!("thread-{index}"));
        self.policy.lock().unwrap().set_thread(
            thread.clone(),
            &IvyJson::object([("projectId", IvyJson::Null)]),
            None,
        );
        NativeNotification {
            method: "turn/completed".into(),
            params: IvyJson::object([
                ("threadId", IvyJson::String(thread)),
                (
                    "turn",
                    IvyJson::object([
                        ("id", format!("turn-{index}").into()),
                        ("status", status.into()),
                    ]),
                ),
            ]),
        }
    }
    async fn complete(&self, index: usize) {
        (self.callbacks.on_notification)(self.completion(index, "completed"))
            .await
            .unwrap();
    }
    fn released(&self) -> usize {
        let db =
            ivy_store::rusqlite::Connection::open(self.root.0.join("journal/agent-runtime.sqlite"))
                .unwrap();
        db.query_row(
            "SELECT count(*) FROM completion_receipts WHERE released=1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap() as usize
    }
    async fn close(self) {
        self.closing.store(true, Ordering::Release);
        self.hive.release();
        self.rpc.close("fixture_done").await;
        self.provider.await.unwrap();
        self.receipts.drain().await.unwrap();
        self.hive.client.stop().await;
        self.state.lock().unwrap().close_shared().unwrap();
    }
}

#[tokio::test]
async fn inventory_deletion_uses_the_original_recorded_notification_time() {
    let fixture = Fixture::new("unused").await;
    let reads = Arc::new(AtomicUsize::new(0));
    let counted = reads.clone();
    let inventory = InventoryManager::with_clock(
        Arc::new(
            NativeInventory::with_reader(
                &catalog("specs/native/codex-0.163.0/catalog.json"),
                "epoch".into(),
                Arc::new(move |method, _, _| {
                    counted.fetch_add(1, Ordering::AcqRel);
                    assert!(method.eq_str("thread/list") || method.eq_str("thread/loaded/list"));
                    Box::pin(async {
                        Ok(IvyJson::object([(
                            "result",
                            IvyJson::object([
                                ("data", IvyJson::Array(vec![])),
                                ("nextCursor", IvyJson::Null),
                            ]),
                        )]))
                    })
                }),
                Arc::new(|| 0.0),
                Arc::new(|| "2026-01-01T00:00:00.000Z".into()),
            )
            .unwrap(),
        ),
        fixture.journal.clone(),
        fixture.policy.clone(),
        Arc::new(|_| Ok(())),
        fixture.closing.clone(),
        Arc::new(|| 0.0),
    )
    .unwrap();
    let connection = ivy_agent_manager::inventory_manager::InventoryConnection::from_connection(
        fixture.hive.client.connection().unwrap(),
    );
    inventory.bind(connection.clone()).unwrap();
    inventory
        .refresh_threads(connection)
        .unwrap()
        .wait()
        .await
        .unwrap();
    let service = ServiceLink::default();
    service.bind(&fixture.hive.client).unwrap();
    let operations = NativeOperations::new(
        Arc::new(SqliteOperationStore::new(fixture.state.clone()).unwrap()),
        fixture.journal.clone(),
    );
    fixture
        .events
        .install(
            inventory.clone(),
            BrowserNotices::new(BrowserNoticeSettings {
                service,
                closing: fixture.closing.clone(),
                operations,
                state: fixture.state.clone(),
                inventory: inventory.clone(),
                policy: fixture.policy.clone(),
                rpc: fixture.rpc.clone(),
            }),
        )
        .unwrap();
    let before = reads.load(Ordering::Acquire);
    (fixture.callbacks.on_notification)(NativeNotification {
        method: "thread/deleted".into(),
        params: IvyJson::object([("threadId", "original-deleted-thread".into())]),
    })
    .await
    .unwrap();
    inventory.drain().await.unwrap();
    let page = fixture
        .notifications
        .lock()
        .unwrap()
        .page(0, 100, Some("epoch"))
        .unwrap();
    let events = page.required("items").unwrap().as_array().unwrap();
    assert_eq!(events.len(), 1);
    let observed = events[0].required("observedAt").unwrap().clone();
    let calls = fixture.hive.inventory.lock().unwrap().clone();
    let after = reads.load(Ordering::Acquire);
    fixture.close().await;
    assert_ne!(
        observed,
        "2026-01-01T00:00:00.000Z".into(),
        "The service and native-reader clocks must differ in this fixture"
    );
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].get("mode"), Some(&"delta".into()));
    assert_eq!(
        calls[1].get("observedAt"),
        Some(&observed),
        "The original notification observation was replaced before inventory publication"
    );
    assert_eq!(
        calls[1].get("removedNativeIds"),
        Some(&IvyJson::Array(vec!["original-deleted-thread".into()]))
    );
    assert_eq!(after, before);
}

#[tokio::test]
async fn a_notice_burst_reserves_before_rendering_and_keeps_native_events_and_inputs() {
    let fixture = Fixture::new("system.status").await;
    for index in 0..128 {
        fixture.complete(index).await;
    }
    assert_eq!(fixture.queries.load(Ordering::Acquire), 8);
    until(|| fixture.hive.held.load(Ordering::Acquire) == 8).await;
    let page = fixture
        .notifications
        .lock()
        .unwrap()
        .page(0, 200, Some("epoch"))
        .unwrap();
    assert_eq!(field(&page, "items").as_array().unwrap().len(), 128);
    (fixture.callbacks.on_request)(NativeRequest {
        id: "approval".into(),
        method: "item/tool/requestUserInput".into(),
        params: IvyJson::object([("threadId", "thread-0".into()), ("turnId", "turn-0".into())]),
    })
    .await
    .unwrap();
    until(|| fixture.hive.inputs.load(Ordering::Acquire) == 1).await;
    assert_eq!(fixture.journal.lock().unwrap().pending_input_count(), 1);
    assert_eq!(fixture.queries.load(Ordering::Acquire), 8);
    let reply = tokio::time::timeout(
        Duration::from_secs(2),
        fixture.rpc.request(
            "thread/read".into(),
            IvyJson::object([
                ("threadId", "control".into()),
                ("includeTurns", false.into()),
            ]),
            Default::default(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        field(field(field(&reply, "result"), "thread"), "id"),
        &"control".into()
    );
    fixture.hive.release();
    until(|| fixture.hive.published.load(Ordering::Acquire) == 8).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(fixture.hive.published.load(Ordering::Acquire), 8);
    assert_eq!(fixture.queries.load(Ordering::Acquire), 8);
    fixture.close().await;
}

#[tokio::test]
async fn retained_payload_budget_refuses_work_without_queueing_and_recovers_after_publication_failure()
 {
    let fixture = Fixture::new("system.status").await;
    fixture.query_mode.store(70_000, Ordering::Release);
    for index in 0..64 {
        fixture.complete(index).await;
    }
    until(|| fixture.hive.held.load(Ordering::Acquire) == 2).await;
    assert_eq!(fixture.queries.load(Ordering::Acquire), 64);
    fixture.hive.fail_publish.store(true, Ordering::Release);
    fixture.hive.release();
    until(|| fixture.hive.published.load(Ordering::Acquire) == 2).await;
    fixture.query_mode.store(0, Ordering::Release);
    fixture.complete(1000).await;
    until(|| fixture.hive.published.load(Ordering::Acquire) == 3).await;
    assert_eq!(fixture.hive.held.load(Ordering::Acquire), 0);
    fixture.close().await;
}

#[tokio::test]
async fn empty_failed_panicking_and_oversized_rendering_release_admission_and_closing_skips_rendering()
 {
    let fixture = Fixture::new("system.status").await;
    for index in 0..16 {
        (fixture.callbacks.on_notification)(fixture.completion(index, "interrupted"))
            .await
            .unwrap();
        fixture.query_mode.store(1, Ordering::Release);
        let error = (fixture.callbacks.on_notification)(fixture.completion(index, "completed"))
            .await
            .unwrap_err();
        assert_eq!(error.code, "fixture_render_failed");
        fixture.query_mode.store(2, Ordering::Release);
        let panic = std::panic::AssertUnwindSafe((fixture.callbacks.on_notification)(
            fixture.completion(index, "completed"),
        ))
        .catch_unwind()
        .await;
        assert!(panic.is_err());
        fixture.query_mode.store(300_000, Ordering::Release);
        fixture.complete(index).await;
    }
    fixture.closing.store(true, Ordering::Release);
    let before = fixture.queries.load(Ordering::Acquire);
    fixture.complete(100).await;
    assert_eq!(fixture.queries.load(Ordering::Acquire), before);
    fixture.closing.store(false, Ordering::Release);
    fixture.query_mode.store(0, Ordering::Release);
    for index in 100..108 {
        fixture.complete(index).await;
    }
    until(|| fixture.hive.held.load(Ordering::Acquire) == 8).await;
    fixture.hive.release();
    until(|| fixture.hive.published.load(Ordering::Acquire) == 8).await;
    fixture.close().await;
}

#[tokio::test]
async fn overload_retires_suppressed_completion_without_hive_and_preserves_original_receipts() {
    let fixture = Fixture::new("serviceNodes.list").await;
    let mut originals = Vec::new();
    for index in 0..9 {
        let value = IvyJson::object([
            (
                "operationId",
                format!("epoch:{}:original", 100 + index).into(),
            ),
            ("callerPrincipalId", "workflow".into()),
            ("method", "turn/start".into()),
            ("phase", "succeeded".into()),
            (
                "params",
                IvyJson::object([("threadId", format!("thread-{index}").into())]),
            ),
            (
                "reply",
                IvyJson::object([(
                    "result",
                    IvyJson::object([(
                        "turn",
                        IvyJson::object([("id", format!("turn-{index}").into())]),
                    )]),
                )]),
            ),
        ]);
        let key = format!("original-{index}");
        let saved = fixture
            .state
            .lock()
            .unwrap()
            .write_at("operation", &key, &value, None, 100 + index)
            .unwrap();
        originals.push((key, saved));
    }
    for index in 0..9 {
        fixture.complete(index).await;
    }
    until(|| fixture.hive.held.load(Ordering::Acquire) == 8).await;
    assert_eq!(fixture.queries.load(Ordering::Acquire), 8);
    assert_eq!(fixture.released(), 1);
    fixture.hive.release();
    until(|| fixture.released() == 9).await;
    assert_eq!(fixture.hive.published.load(Ordering::Acquire), 0);
    let protected =
        ivy_store::rusqlite::Connection::open(fixture.root.0.join("journal/agent-runtime.sqlite"))
            .unwrap()
            .query_row::<i64, _, _>(
                "SELECT released FROM completion_receipts WHERE operation_key=?",
                ["original-8"],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(protected, 1);
    for (key, original) in originals {
        let current = fixture
            .state
            .lock()
            .unwrap()
            .read("operation", &key)
            .unwrap()
            .unwrap();
        assert_eq!(current.pin, original.pin);
        same(&current.value, &original.value);
    }
    fixture.close().await;
}

#[tokio::test]
async fn a_dropped_suppression_waiter_still_commits_before_receipt_drain() {
    let fixture = Fixture::new("system.status").await;
    for index in 0..8 {
        fixture.complete(index).await;
    }
    until(|| fixture.hive.held.load(Ordering::Acquire) == 8).await;
    let value = IvyJson::object([
        ("operationId", "epoch:100:original".into()),
        ("callerPrincipalId", "workflow".into()),
        ("method", "turn/start".into()),
        ("phase", "succeeded".into()),
        (
            "params",
            IvyJson::object([("threadId", "thread-100".into())]),
        ),
        (
            "reply",
            IvyJson::object([(
                "result",
                IvyJson::object([("turn", IvyJson::object([("id", "turn-100".into())]))]),
            )]),
        ),
    ]);
    fixture
        .state
        .lock()
        .unwrap()
        .write_at("operation", "original", &value, None, 100)
        .unwrap();
    let lock = fixture.state.lock().unwrap();
    let mut pending = Box::pin((fixture.callbacks.on_notification)(
        fixture.completion(100, "completed"),
    ));
    futures_util::future::poll_fn(|cx| match pending.as_mut().poll(cx) {
        Poll::Pending => Poll::Ready(()),
        Poll::Ready(_) => panic!("SQL commit must wait on its current owner"),
    })
    .await;
    drop(pending);
    let mut drain = Box::pin(fixture.receipts.drain());
    futures_util::future::poll_fn(|cx| match drain.as_mut().poll(cx) {
        Poll::Pending => Poll::Ready(()),
        Poll::Ready(_) => panic!("Receipt drain must retain the accepted SQL write"),
    })
    .await;
    drop(lock);
    drain.await.unwrap();
    assert_eq!(fixture.released(), 1);
    assert_eq!(fixture.hive.held.load(Ordering::Acquire), 8);
    assert_eq!(fixture.queries.load(Ordering::Acquire), 8);
    fixture.hive.release();
    until(|| fixture.hive.published.load(Ordering::Acquire) == 8).await;
    fixture.close().await;
}
