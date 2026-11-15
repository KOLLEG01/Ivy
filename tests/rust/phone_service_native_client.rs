use super::*;
use crate::service_test_support::{equal, fixture, id, observed, timed, until};
use tokio::{
    io::{AsyncBufReadExt, BufReader, DuplexStream},
    sync::Semaphore,
};
fn object<const N: usize>(v: [(&str, IvyJson); N]) -> IvyJson {
    IvyJson::object(v)
}
fn uuid(n: u32) -> JsString {
    id(n).as_js_string().unwrap().clone()
}
fn text<'a>(v: &'a IvyJson, k: &str) -> &'a JsString {
    v.get(k).unwrap().as_js_string().unwrap()
}
struct Hooks {
    events: Arc<Mutex<Vec<IvyJson>>>,
    before: Arc<Semaphore>,
    saved: Arc<Semaphore>,
    name: String,
}
impl PhoneReceiptHooks for Hooks {
    fn before_send(&self, intent: IvyJson) -> PhoneReceiptWork {
        let events = self.events.clone();
        let before = self.before.clone();
        let hold = ["intent-gate", "close-during-intent"].contains(&self.name.as_str());
        Box::pin(async move {
            events
                .lock()
                .unwrap()
                .push(object([("kind", "intent".into()), ("intent", intent)]));
            if hold {
                before.acquire().await.unwrap().forget();
            }
            Ok(())
        })
    }
    fn before_resolve(&self, intent: IvyJson, reply: IvyJson) -> PhoneReceiptWork {
        let events = self.events.clone();
        let saved = self.saved.clone();
        let hold = ["slow-receipt", "duplicate"].contains(&self.name.as_str());
        let fail = self.name == "save-failed";
        Box::pin(async move {
            events.lock().unwrap().push(object([
                ("kind", "receipt".into()),
                ("intent", intent),
                ("reply", reply),
            ]));
            if hold {
                saved.acquire().await.unwrap().forget();
            }
            if fail {
                return Err(IvyError::internal());
            }
            events
                .lock()
                .unwrap()
                .push(object([("kind", "saved".into())]));
            Ok(())
        })
    }
}
struct Fixture {
    client: PhoneNativeClient,
    output: Option<DuplexStream>,
    requests: Arc<Mutex<Vec<IvyJson>>>,
    hooks: Arc<Hooks>,
    reader: tokio::task::JoinHandle<()>,
}
impl Fixture {
    fn new(name: &str) -> Self {
        let (input, peer_input) = tokio::io::duplex(262144);
        let (output, read) = tokio::io::duplex(262144);
        let hooks = Arc::new(Hooks {
            events: Arc::new(Mutex::new(vec![])),
            before: Arc::new(Semaphore::new(0)),
            saved: Arc::new(Semaphore::new(0)),
            name: name.into(),
        });
        let client = PhoneNativeClient::start(uuid(1), input, read, hooks.clone()).unwrap();
        let requests = Arc::new(Mutex::new(vec![]));
        let captured = requests.clone();
        let reader = tokio::spawn(async move {
            let mut input = BufReader::new(peer_input);
            loop {
                let mut line = String::new();
                if input.read_line(&mut line).await.unwrap() == 0 {
                    break;
                }
                captured
                    .lock()
                    .unwrap()
                    .push(parse(&line, FRAME_BYTES + 1).unwrap());
            }
        });
        Self {
            client,
            output: Some(output),
            requests,
            hooks,
            reader,
        }
    }
    fn request(
        &self,
        method: &str,
        args: IvyJson,
        operation: Option<JsString>,
        timeout: u64,
    ) -> PhoneNativeRequest {
        match self.client.request(method, args, operation, timeout) {
            Ok(work) => work,
            Err(error) => {
                let (send, receive) = watch::channel(None);
                send.send_replace(Some(Err(error)));
                PhoneNativeRequest(receive)
            }
        }
    }
    async fn sent(&self, count: usize) {
        until(|| self.requests.lock().unwrap().len() >= count).await;
    }
    fn response(&self, index: usize, result: IvyJson) -> IvyJson {
        let request = self.requests.lock().unwrap()[index].clone();
        object([
            ("version", 1.0.into()),
            ("epoch", id(1)),
            ("requestId", request.get("requestId").unwrap().clone()),
            ("ok", true.into()),
            ("result", result),
            ("error", IvyJson::Null),
        ])
    }
    async fn raw(&mut self, bytes: &[u8]) {
        timed(self.output.as_mut().unwrap().write_all(bytes))
            .await
            .unwrap();
    }
    async fn reply(&mut self, index: usize, result: IvyJson) {
        let reply = self.response(index, result);
        let mut bytes = canonical(&reply, FRAME_BYTES).unwrap().into_bytes();
        bytes.push(10);
        self.raw(&bytes).await;
    }
    async fn receipt(&self, count: usize) {
        until(|| {
            self.hooks
                .events
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.get("kind") == Some(&"saved".into()))
                .count()
                >= count
        })
        .await;
    }
    async fn finish(self) {
        self.hooks.before.add_permits(2);
        self.hooks.saved.add_permits(2);
        self.client.close("phone_owner_stopping");
        drop(self.output);
        timed(self.reader).await.unwrap();
    }
}
fn call_args() -> IvyJson {
    object([("callId", id(3))])
}
fn empty() -> IvyJson {
    object([])
}

#[tokio::test]
async fn byte_pressure_survives_out_of_order_and_late_reads_without_extra_requests() {
    let mut f = Fixture::new("receipt-bytes");
    let usage = |retained: f64| {
        object([
            ("receipts", 256.0.into()),
            ("retainedBytes", retained.into()),
            ("reservedBytes", 0.0.into()),
            ("limitBytes", 16_777_216.0.into()),
        ])
    };
    assert!(!f.client.receipt_rotation_due());
    assert!(f.client.can_rotate_receipts());
    let first = f.request("status", empty(), None, 5000);
    let second = f.request("status", empty(), None, 5000);
    f.sent(2).await;
    let mut reply = f.response(1, empty());
    reply.insert("receiptUsage", usage(8_388_608.0));
    f.raw(format!("{}\n", canonical(&reply, FRAME_BYTES).unwrap()).as_bytes())
        .await;
    timed(second.wait()).await.unwrap();
    assert!(f.client.receipt_rotation_due());
    let mut reply = f.response(0, empty());
    reply.insert("receiptUsage", usage(32.0));
    f.raw(format!("{}\n", canonical(&reply, FRAME_BYTES).unwrap()).as_bytes())
        .await;
    timed(first.wait()).await.unwrap();
    assert!(
        f.client.receipt_rotation_due(),
        "An older observation cannot erase completed-byte pressure in the same epoch"
    );
    assert_eq!(f.requests.lock().unwrap().len(), 2);
    assert!(f.hooks.events.lock().unwrap().is_empty());
    f.finish().await;

    // Capacity pressure never permits retirement while the original result's
    // durable acknowledgement is still blocked, even after native completion.
    let mut f = Fixture::new("slow-receipt");
    let effect = f.request("call.release", call_args(), Some(uuid(8)), 5000);
    f.sent(1).await;
    assert!(!f.client.can_rotate_receipts());
    let mut reply = f.response(0, object([("released", true.into())]));
    reply.insert("receiptUsage", usage(8_388_608.0));
    f.raw(format!("{}\n", canonical(&reply, FRAME_BYTES).unwrap()).as_bytes())
        .await;
    until(|| f.client.receipt_rotation_due()).await;
    assert!(!f.client.can_rotate_receipts());
    f.hooks.saved.add_permits(1);
    timed(effect.wait()).await.unwrap();
    assert!(f.client.can_rotate_receipts());
    f.finish().await;

    // A fresh process epoch starts without pressure. Its original expired read
    // can still report byte pressure; no repeat observation/effect is dispatched.
    let mut f = Fixture::new("late-receipt-bytes");
    assert!(!f.client.receipt_rotation_due());
    let late = f.request("status", empty(), None, 1);
    f.sent(1).await;
    assert_eq!(
        timed(late.wait()).await.unwrap_err().code,
        "phone_request_timeout"
    );
    assert!(!f.client.is_closed());
    let mut reply = f.response(0, empty());
    reply.insert("receiptUsage", usage(8_388_608.0));
    f.raw(format!("{}\n", canonical(&reply, FRAME_BYTES).unwrap()).as_bytes())
        .await;
    until(|| f.client.receipt_rotation_due()).await;
    assert_eq!(f.requests.lock().unwrap().len(), 1);
    f.finish().await;

    let mut f = Fixture::new("invalid-receipt-bytes");
    let read = f.request("status", empty(), None, 5000);
    f.sent(1).await;
    let mut reply = f.response(0, empty());
    let mut invalid = usage(16_777_216.0);
    invalid.insert("reservedBytes", 65_024.0.into());
    reply.insert("receiptUsage", invalid);
    f.raw(format!("{}\n", canonical(&reply, FRAME_BYTES).unwrap()).as_bytes())
        .await;
    assert_eq!(
        timed(read.wait()).await.unwrap_err().code,
        "phone_invalid_frame"
    );
    assert!(!f.client.receipt_rotation_due());
    f.finish().await;
}

#[tokio::test]
async fn one_poll_shares_call_reads_and_the_next_poll_observes_fresh_status() {
    let mut f = Fixture::new("poll-snapshot");
    let native = f.client.clone();
    let first = tokio::spawn(async move {
        let root = object([("call", object([("id", id(3))]))]);
        let mut snapshot = PhoneNativeSnapshot::new(root);
        assert_eq!(
            snapshot
                .for_call(&native, &uuid(3))
                .await
                .unwrap()
                .get("call")
                .unwrap()
                .get("id"),
            Some(&id(3))
        );
        for _ in 0..4 {
            assert!(
                snapshot
                    .for_call(&native, &uuid(4))
                    .await
                    .unwrap()
                    .get("call")
                    .unwrap()
                    .is_null()
            );
        }
    });
    f.sent(1).await;
    assert_eq!(
        f.requests.lock().unwrap()[0].get("params"),
        Some(&object([("callId", id(4))]))
    );
    let status = object([
        ("configured", true.into()),
        ("endpoint", "127.0.0.1:5060".into()),
        ("call", IvyJson::Null),
        ("media", IvyJson::Null),
        ("registration", IvyJson::Null),
    ]);
    f.reply(0, status).await;
    timed(first).await.unwrap();
    assert_eq!(f.requests.lock().unwrap().len(), 1);
    let native = f.client.clone();
    let next = tokio::spawn(async move {
        let mut snapshot = PhoneNativeSnapshot::new(object([("call", IvyJson::Null)]));
        snapshot
            .for_call(&native, &uuid(4))
            .await
            .unwrap()
            .get("endpoint")
            .unwrap()
            .clone()
    });
    f.sent(2).await;
    f.reply(
        1,
        object([
            ("configured", true.into()),
            ("endpoint", "127.0.0.1:5070".into()),
            ("call", IvyJson::Null),
            ("media", IvyJson::Null),
            ("registration", IvyJson::Null),
        ]),
    )
    .await;
    assert_eq!(timed(next).await.unwrap(), "127.0.0.1:5070".into());
    assert_eq!(f.requests.lock().unwrap().len(), 2);
    f.finish().await;
}

#[tokio::test]
async fn realtime_errors_retain_the_original_receipt_and_leave_the_pipe_usable() {
    for code in ["phone_webrtc_failed", "phone_webrtc_sdp_invalid"] {
        let mut f = Fixture::new("realtime-error");
        let work = f.request(
            "call.realtime.answer",
            object([
                ("callId", id(3)),
                ("generation", 0.0.into()),
                ("sdp", "invalid answer".into()),
            ]),
            Some(uuid(2)),
            15000,
        );
        f.sent(1).await;
        let mut reply = f.response(0, IvyJson::Null);
        reply.insert("ok", false.into());
        reply.insert("error", code.into());
        f.raw(format!("{}\n", canonical(&reply, FRAME_BYTES).unwrap()).as_bytes())
            .await;
        equal(&timed(work.wait()).await.unwrap(), &reply, code);
        f.receipt(1).await;
        let receipt = f
            .hooks
            .events
            .lock()
            .unwrap()
            .iter()
            .find(|event| event.get("kind") == Some(&"receipt".into()))
            .cloned()
            .unwrap();
        equal(receipt.get("reply").unwrap(), &reply, code);
        let heartbeat = f.request("heartbeat", empty(), None, 15000);
        f.sent(2).await;
        f.reply(1, object([("ok", true.into())])).await;
        timed(heartbeat.wait()).await.unwrap();
        assert!(!f.client.is_closed());
        f.finish().await;
    }
}

#[tokio::test]
async fn original_pipe_receipts_late_replies_capacity_and_scoped_observations_agree() {
    let cases = fixture("native");
    assert_eq!(cases.as_array().unwrap().len(), 31);
    for case in cases.as_array().unwrap() {
        let name = text(case, "name").to_utf8().unwrap();
        let mut f = Fixture::new(&name);
        let mut before_dispatch = IvyJson::Null;
        let mut before_receipt = IvyJson::Null;
        let result;
        if name == "capacity" {
            let mut pending = Vec::new();
            for _ in 0..32 {
                pending.push(f.request(
                    "desktop.observe",
                    object([("appUserModelId", "Fixture!UI".into())]),
                    None,
                    15000,
                ));
            }
            let ordinary = observed(
                f.request("call.prepare", call_args(), Some(uuid(2)), 15000)
                    .wait()
                    .await,
            );
            for _ in 0..14 {
                pending.push(f.request("status", empty(), None, 15000));
            }
            pending.push(f.request("call.hangup", call_args(), Some(uuid(2)), 15000));
            f.sent(47).await;
            pending.push(f.request("call.release", call_args(), Some(uuid(2)), 15000));
            f.sent(48).await;
            let control = observed(f.request("heartbeat", empty(), None, 15000).wait().await);
            for n in 0..48 {
                f.reply(n, empty()).await;
            }
            let mut completed = 0;
            for work in pending {
                completed += usize::from(timed(work.wait()).await.is_ok());
            }
            result = object([
                ("ordinary", ordinary),
                ("control", control),
                ("completed", (completed as f64).into()),
            ]);
        } else if name.starts_with("invalid-") {
            let method = if name == "invalid-read-operation" {
                "status"
            } else {
                "call.prepare"
            };
            let args = if name == "invalid-args" {
                object([("callId", id(3)), ("extra", true.into())])
            } else if name == "invalid-call" {
                object([("callId", "bad".into())])
            } else {
                call_args()
            };
            result = observed(
                f.request(
                    method,
                    args,
                    if name == "invalid-operation" {
                        None
                    } else {
                        Some(uuid(2))
                    },
                    if name == "invalid-timeout" { 0 } else { 15000 },
                )
                .wait()
                .await,
            );
        } else if name == "heartbeat-rejected" {
            f.client.start_heartbeat().unwrap();
            f.client.start_heartbeat().unwrap();
            f.sent(1).await;
            let mut reply = f.response(0, IvyJson::Null);
            reply.insert("ok", false.into());
            reply.insert("error", "runtime_not_ready".into());
            f.raw(format!("{}\n", canonical(&reply, FRAME_BYTES).unwrap()).as_bytes())
                .await;
            assert_eq!(timed(f.client.closed()).await, "phone_heartbeat_rejected");
            result = observed(Ok(IvyJson::Null));
        } else if [
            "desktop-match",
            "desktop-mismatch",
            "capture-mismatch",
            "process-mismatch",
            "controls-duplicate",
            "status-mismatch",
        ]
        .contains(&name.as_str())
        {
            let client = f.client.clone();
            let task_name = name.clone();
            let work = tokio::spawn(async move {
                let desktop = object([
                    ("pid", 123.0.into()),
                    ("startTimeUtcTicks", "1".into()),
                    ("imagePath", "C:\\fixture.exe".into()),
                    ("appUserModelId", "Fixture!UI".into()),
                ]);
                match task_name.as_str() {
                    "desktop-match" | "desktop-mismatch" => {
                        client.observe_desktop("Fixture!UI".into()).await
                    }
                    "capture-mismatch" => {
                        client
                            .observe_desktop_capture(desktop, "capture".into())
                            .await
                    }
                    "process-mismatch" => client.observe_audio_process(desktop, 123.0).await,
                    "controls-duplicate" => {
                        client
                            .observe_controls(desktop, vec!["Stop voice chat".into()])
                            .await
                    }
                    _ => client.observe(None).await,
                }
            });
            f.sent(1).await;
            let desktop = object([
                ("pid", 123.0.into()),
                ("startTimeUtcTicks", "1".into()),
                ("imagePath", "C:\\fixture.exe".into()),
                (
                    "appUserModelId",
                    if name == "desktop-match" {
                        "Fixture!UI"
                    } else {
                        "Other!UI"
                    }
                    .into(),
                ),
            ]);
            let control = object([
                ("id", IvyJson::Array(vec![42.0.into(), 1.0.into()])),
                ("parentId", IvyJson::Array(vec![42.0.into(), 0.0.into()])),
                ("name", "Stop voice chat".into()),
                ("enabled", true.into()),
                ("offscreen", false.into()),
                ("toggle", IvyJson::Null),
            ]);
            let window = object([
                ("handle", "123".into()),
                ("minimized", false.into()),
                ("controls", IvyJson::Array(vec![control])),
            ]);
            let reply = match name.as_str() {
                "desktop-match" | "desktop-mismatch" => {
                    object([("state", "ready".into()), ("identity", desktop)])
                }
                "capture-mismatch" => object([
                    ("endpointId", "other".into()),
                    ("state", "none".into()),
                    ("identity", IvyJson::Null),
                ]),
                "process-mismatch" => object([("pid", 124.0.into()), ("state", "owned".into())]),
                "controls-duplicate" => object([
                    ("state", "observed".into()),
                    ("windows", IvyJson::Array(vec![window.clone(), window])),
                ]),
                _ => object([
                    ("configured", false.into()),
                    ("endpoint", "127.0.0.1:5060".into()),
                    ("call", IvyJson::Null),
                    ("media", IvyJson::Null),
                    ("registration", IvyJson::Null),
                ]),
            };
            f.reply(0, reply).await;
            result = observed(timed(work).await.unwrap());
        } else if name == "late-read" {
            let live = f.request("call.prepare", call_args(), Some(uuid(2)), 15000);
            f.sent(1).await;
            let read = f.request("status", empty(), None, 20);
            result = observed(timed(read.wait()).await);
            f.sent(2).await;
            f.reply(1, empty()).await;
            f.reply(0, empty()).await;
            timed(live.wait()).await.unwrap();
        } else {
            let work = f.request(
                if name == "read" {
                    "status"
                } else {
                    "call.prepare"
                },
                if name == "read" { empty() } else { call_args() },
                if name == "read" { None } else { Some(uuid(2)) },
                if ["slow-receipt", "late-effect"].contains(&name.as_str()) {
                    20
                } else {
                    15000
                },
            );
            if ["intent-gate", "close-during-intent"].contains(&name.as_str()) {
                until(|| !f.hooks.events.lock().unwrap().is_empty()).await;
                assert!(f.requests.lock().unwrap().is_empty());
                before_dispatch = 0.0.into();
                if name == "close-during-intent" {
                    f.client.close("fixture_before_dispatch");
                }
                f.hooks.before.add_permits(1);
            }
            if name == "close-during-intent" {
                result = observed(timed(work.wait()).await);
            } else {
                f.sent(1).await;
                if name == "late-effect" {
                    result = observed(timed(work.wait()).await);
                    f.reply(0, empty()).await;
                    f.receipt(1).await;
                } else if ["oversize", "utf8", "empty", "truncated", "eof"].contains(&name.as_str())
                {
                    match name.as_str() {
                        "oversize" => f.raw(&vec![32; 65537]).await,
                        "utf8" => f.raw(&[0xc0, 0x80, 10]).await,
                        "empty" => f.raw(b"\n").await,
                        "truncated" => {
                            f.raw(b"{").await;
                            f.output.take();
                        }
                        _ => {
                            f.output.take();
                        }
                    }
                    result = observed(timed(work.wait()).await);
                } else {
                    if name == "wrong-epoch" {
                        let mut reply = f.response(0, empty());
                        reply.insert("epoch", id(4));
                        f.raw(format!("{}\n", canonical(&reply, FRAME_BYTES).unwrap()).as_bytes())
                            .await;
                    } else if ["fragmented", "bom"].contains(&name.as_str()) {
                        let reply = f.response(0, object([("text", "Grüße".into())]));
                        let bytes = format!(
                            "{}{}\r\n",
                            if name == "bom" { "\u{feff}" } else { "" },
                            canonical(&reply, FRAME_BYTES).unwrap()
                        )
                        .into_bytes();
                        for chunk in bytes.chunks(3) {
                            f.raw(chunk).await;
                        }
                    } else {
                        f.reply(0, empty()).await;
                        if name == "duplicate" {
                            f.reply(0, empty()).await;
                        }
                    }
                    if name == "slow-receipt" {
                        until(|| {
                            f.hooks
                                .events
                                .lock()
                                .unwrap()
                                .iter()
                                .any(|r| r.get("kind") == Some(&"receipt".into()))
                        })
                        .await;
                        before_receipt = false.into();
                        assert!(work.0.borrow().is_none());
                        result = observed(timed(work.wait()).await);
                        f.hooks.saved.add_permits(1);
                        f.receipt(1).await;
                    } else if name == "duplicate" {
                        result = observed(timed(work.wait()).await);
                        f.hooks.saved.add_permits(1);
                        f.receipt(1).await;
                    } else if name == "observer-loss" {
                        drop(work);
                        f.receipt(1).await;
                        result = object([("discarded", true.into())]);
                    } else {
                        result = observed(timed(work.wait()).await);
                    }
                }
            }
        }
        let closed = f
            .client
            .0
            .state
            .lock()
            .unwrap()
            .closed
            .clone()
            .map_or(IvyJson::Null, Into::into);
        let requests = f.requests.lock().unwrap().clone();
        let events = f.hooks.events.lock().unwrap().clone();
        let actual = object([
            ("result", result),
            ("requests", IvyJson::Array(requests)),
            ("events", IvyJson::Array(events)),
            ("beforeDispatch", before_dispatch),
            ("beforeReceipt", before_receipt),
            ("closed", closed),
        ]);
        equal(&actual, case.get("expected").unwrap(), &name);
        f.finish().await;
    }
}

#[tokio::test]
async fn real_journal_hooks_retain_original_success_and_native_errors_across_restart() {
    use crate::journal::{PhoneJournalLimits, PhoneJournalOptions, PhoneJournalOwner};
    let parent = std::env::var_os("IVY_TEST_TEMP")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let root = parent.join(format!("ivy-phone-client-journal-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let options = || PhoneJournalOptions {
        data_root: root.clone(),
        owner: PhoneJournalOwner {
            host_id: "phone-fixture-host".into(),
            service_node_id: "phone-fixture-node".into(),
        },
        limits: PhoneJournalLimits::default(),
        max_concurrent_calls: 1,
    };
    let journal = PhoneJournal::open(options()).await.unwrap();
    journal.begin_epoch(uuid(1)).await.unwrap();
    let (input, peer_input) = tokio::io::duplex(65536);
    let (mut output, read) = tokio::io::duplex(65536);
    let client = PhoneNativeClient::start(uuid(1), input, read, Arc::new(journal.clone())).unwrap();
    let operation = uuid(2);
    let work = client
        .request(
            "configure",
            object([
                (
                    "binding",
                    object([
                        ("address", "127.0.0.1".into()),
                        ("port", 0.0.into()),
                        ("transport", "udp".into()),
                    ]),
                ),
                (
                    "codecs",
                    object([
                        ("preferences", IvyJson::Array(vec!["PCMA".into()])),
                        ("packetMs", 20.0.into()),
                    ]),
                ),
                ("incomingPeers", IvyJson::Array(vec![])),
                ("registration", IvyJson::Null),
                ("password", IvyJson::Null),
            ]),
            Some(operation.clone()),
            15000,
        )
        .unwrap();
    drop(work);
    let mut peer = BufReader::new(peer_input);
    let mut line = String::new();
    timed(peer.read_line(&mut line)).await.unwrap();
    let request = parse(&line, 65536).unwrap();
    assert_eq!(
        journal
            .operation(operation.clone())
            .await
            .unwrap()
            .unwrap()
            .get("phase"),
        Some(&"submitted".into())
    );
    let reply = object([
        ("version", 1.0.into()),
        ("epoch", id(1)),
        ("requestId", request.get("requestId").unwrap().clone()),
        ("ok", true.into()),
        ("result", object([("configured", true.into())])),
        ("error", IvyJson::Null),
    ]);
    output
        .write_all(format!("{}\n", canonical(&reply, 65536).unwrap()).as_bytes())
        .await
        .unwrap();
    timed(async {
        loop {
            if journal
                .operation(operation.clone())
                .await
                .unwrap()
                .unwrap()
                .get("phase")
                == Some(&"result".into())
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    let mut retained = vec![(
        operation.clone(),
        journal.operation(operation).await.unwrap().unwrap(),
    )];
    let schema = parse(
        include_str!("../../specs/schemas/phone-ipc.schema.json"),
        FRAME_BYTES,
    )
    .unwrap();
    let codes = schema
        .get("$defs")
        .unwrap()
        .get("Response")
        .unwrap()
        .get("properties")
        .unwrap()
        .get("error")
        .unwrap()
        .get("enum")
        .unwrap()
        .as_array()
        .unwrap();
    for (index, code) in codes.iter().filter(|code| !code.is_null()).enumerate() {
        let operation = uuid(200 + index as u32);
        let arguments = object([
            ("callId", id(100 + index as u32)),
            ("generation", 0.0.into()),
            ("sdp", "invalid answer".into()),
        ]);
        let work = client
            .request(
                "call.realtime.answer",
                arguments.clone(),
                Some(operation.clone()),
                15000,
            )
            .unwrap();
        line.clear();
        timed(peer.read_line(&mut line)).await.unwrap();
        let request = parse(&line, FRAME_BYTES).unwrap();
        assert_eq!(request.get("method"), Some(&"call.realtime.answer".into()));
        let reply = object([
            ("version", 1.0.into()),
            ("epoch", id(1)),
            ("requestId", request.get("requestId").unwrap().clone()),
            ("ok", false.into()),
            ("result", IvyJson::Null),
            ("error", code.clone()),
        ]);
        validate_phone("Response", &reply).unwrap();
        output
            .write_all(format!("{}\n", canonical(&reply, FRAME_BYTES).unwrap()).as_bytes())
            .await
            .unwrap();
        equal(
            &timed(work.wait()).await.unwrap(),
            &reply,
            &code.as_str().unwrap(),
        );
        let saved = journal.operation(operation.clone()).await.unwrap().unwrap();
        assert_eq!(saved.get("phase"), Some(&"result".into()));
        assert_eq!(saved.get("receipt").unwrap().get("error"), Some(code));
        retained.push((operation.clone(), saved));
        let duplicate = client
            .request("call.realtime.answer", arguments, Some(operation), 15000)
            .unwrap();
        assert_eq!(
            timed(duplicate.wait()).await.unwrap_err().code,
            "phone_operation_retained"
        );
        assert!(!client.is_closed());
    }
    let heartbeat = client.request("heartbeat", empty(), None, 15000).unwrap();
    line.clear();
    timed(peer.read_line(&mut line)).await.unwrap();
    let request = parse(&line, FRAME_BYTES).unwrap();
    assert_eq!(request.get("method"), Some(&"heartbeat".into()));
    let reply = object([
        ("version", 1.0.into()),
        ("epoch", id(1)),
        ("requestId", request.get("requestId").unwrap().clone()),
        ("ok", true.into()),
        ("result", object([("ok", true.into())])),
        ("error", IvyJson::Null),
    ]);
    output
        .write_all(format!("{}\n", canonical(&reply, FRAME_BYTES).unwrap()).as_bytes())
        .await
        .unwrap();
    equal(
        &timed(heartbeat.wait()).await.unwrap(),
        &reply,
        "usable pipe",
    );
    assert!(!client.is_closed());
    client.close("phone_owner_stopping");
    drop(output);
    drop(peer);
    journal.close().wait().await.unwrap();
    let reopened = PhoneJournal::open(options()).await.unwrap();
    for (operation, original) in retained {
        equal(
            &reopened.operation(operation).await.unwrap().unwrap(),
            &original,
            "retained original after restart",
        );
        assert_eq!(
            reopened
                .submit(original.get("intent").unwrap().clone())
                .await
                .unwrap_err()
                .code,
            "phone_operation_retained"
        );
    }
    reopened.close().wait().await.unwrap();
    assert!(
        root.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("ivy-phone-client-journal-")
    );
    std::fs::remove_dir_all(root).unwrap();
}
