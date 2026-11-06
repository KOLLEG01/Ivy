use ivy_host_runtime::{codex_admission, codex_rpc};
#[path = "agent-native-commands.rs"]
mod command_cases;
#[path = "agent-native-replies.rs"]
mod completion_cases;
#[path = "agent-native-frame-owner.rs"]
mod owner_cases;
#[path = "codex-transport.rs"]
mod primitives;

use codex_rpc::{Events, NativeRpc, RequestOptions};
use ivy_contracts::{
    IvyError, IvyJson, JsString, Outcome,
    json::{parse, stringify},
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream},
    task::JoinHandle,
};

fn value(text: &str) -> IvyJson {
    parse(text, 32 * 1024 * 1024).unwrap()
}
fn catalog() -> IvyJson {
    value(include_str!(
        "../../specs/native/codex-0.154.0/catalog.json"
    ))
}
#[tokio::test]
async fn in_process_moves_lossless_values_and_preserves_commit_order() {
    let (rpc, mut peer, incoming) =
        NativeRpc::start_frames(Arc::new(catalog()), Events::default()).unwrap();
    let mut units = Vec::with_capacity(1024 * 1024);
    units.extend([65, 0xd800, 0xdc01, 0xdfff]);
    let pointer = units.as_ptr() as usize;
    let capacity = units.capacity();
    let sent = Arc::new(AtomicUsize::new(0));
    let committed = Arc::new(AtomicUsize::new(0));
    let sent_hook = sent.clone();
    let committed_hook = committed.clone();
    let client = rpc.clone();
    let call = tokio::spawn(async move {
        client
            .request(
                "account/logout".into(),
                IvyJson::String(JsString(units)),
                RequestOptions {
                    request_id: Some("local".into()),
                    before_send: Some(Arc::new(move |_| {
                        sent_hook.fetch_add(1, Ordering::SeqCst);
                        Box::pin(async { Ok(()) })
                    })),
                    before_resolve: Some(Arc::new(move |_, reply| {
                        assert_eq!(reply.get("result"), Some(&value(r#""\udfff\ud800""#)));
                        committed_hook.fetch_add(1, Ordering::SeqCst);
                        Box::pin(async { Ok(()) })
                    })),
                    ..Default::default()
                },
            )
            .await
    });
    let frame = peer.recv().await.unwrap();
    assert_eq!(sent.load(Ordering::SeqCst), 1);
    let text = frame.get("params").unwrap().as_js_string().unwrap();
    assert_eq!(
        text.0.as_ptr() as usize,
        pointer,
        "request allocation was copied or encoded"
    );
    assert_eq!(text.0.capacity(), capacity);
    incoming
        .try_send(IvyJson::object([
            ("id", "local".into()),
            ("result", value(r#""\udfff\ud800""#)),
        ]))
        .unwrap();
    assert!(result(call).await.is_ok());
    assert_eq!(committed.load(Ordering::SeqCst), 1);
    rpc.close("complete").await;
}

#[tokio::test]
async fn in_process_heap_limits_reject_before_hooks_and_reserve_controls() {
    use codex_admission::Priority;
    let (rpc, mut peer, incoming) =
        NativeRpc::start_frames(Arc::new(catalog()), Events::default()).unwrap();
    let hooks = Arc::new(AtomicUsize::new(0));
    let (observed, mut observations) = tokio::sync::mpsc::unbounded_channel();
    let mut pending = vec![];
    for index in 0..7 {
        let mut text = Vec::with_capacity(4 * 1024 * 1024);
        text.push(65);
        let client = rpc.clone();
        let hooks = hooks.clone();
        let observed = observed.clone();
        let call = tokio::spawn(async move {
            client
                .request(
                    "account/logout".into(),
                    IvyJson::String(JsString(text)),
                    RequestOptions {
                        request_id: Some(format!("bulk-{index}").into()),
                        before_send: Some(Arc::new(move |_| {
                            hooks.fetch_add(1, Ordering::SeqCst);
                            observed.send(()).unwrap();
                            Box::pin(async { Ok(()) })
                        })),
                        ..Default::default()
                    },
                )
                .await
        });
        if index < 6 {
            tokio::time::timeout(Duration::from_secs(5), observations.recv())
                .await
                .unwrap()
                .unwrap();
            pending.push(call);
        } else {
            let error = result(call).await.unwrap_err();
            assert_eq!(error.code, "native_capacity");
            assert_eq!(error.outcome, Outcome::NotExecuted);
        }
    }
    assert_eq!(hooks.load(Ordering::SeqCst), 6);
    let control = request(
        &rpc,
        "control",
        RequestOptions {
            priority: Priority::Interaction,
            ..Default::default()
        },
    );
    for _ in 0..6 {
        let _ = peer.recv().await.unwrap();
    }
    let frame = peer.recv().await.unwrap();
    assert_eq!(frame.get("id"), Some(&"control".into()));
    incoming
        .try_send(IvyJson::object([
            ("id", "control".into()),
            ("result", IvyJson::Null),
        ]))
        .unwrap();
    assert!(result(control).await.is_ok());

    let mut huge = Vec::with_capacity(25 * 1024 * 1024);
    huge.push(65);
    assert_eq!(
        incoming
            .try_send(IvyJson::String(JsString(huge)))
            .unwrap_err()
            .code,
        "native_capacity"
    );
    // Queue consumption returned its reservations, including the rejected frame.
    let retry = request(&rpc, "bulk-6", RequestOptions::default());
    let frame = peer.recv().await.unwrap();
    assert_eq!(frame.get("id"), Some(&"bulk-6".into()));
    incoming
        .try_send(IvyJson::object([
            ("id", "bulk-6".into()),
            ("result", IvyJson::Null),
        ]))
        .unwrap();
    assert!(result(retry).await.is_ok());
    drop(incoming);
    assert_eq!(rpc.closed().await, "native_connection_closed");
    for call in pending {
        assert_eq!(result(call).await.unwrap_err().outcome, Outcome::Unknown);
    }
}
struct Peer {
    input: BufReader<DuplexStream>,
    output: DuplexStream,
}
impl Peer {
    async fn sent(&mut self) -> IvyJson {
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(5), self.input.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        value(&line)
    }
    async fn emit(&mut self, frame: IvyJson) {
        self.output
            .write_all(format!("{}\n", stringify(&frame, 32 * 1024 * 1024).unwrap()).as_bytes())
            .await
            .unwrap();
    }
    async fn reply(&mut self, id: IvyJson) {
        self.emit(IvyJson::object([("id", id), ("result", IvyJson::Null)]))
            .await;
    }
}
fn fixture(events: Events) -> (NativeRpc, Peer) {
    let (input, sent) = tokio::io::duplex(65536);
    let (emit, output) = tokio::io::duplex(65536);
    (
        NativeRpc::start(Arc::new(catalog()), input, output, events).unwrap(),
        Peer {
            input: BufReader::new(sent),
            output: emit,
        },
    )
}
fn request(
    rpc: &NativeRpc,
    id: &str,
    mut options: RequestOptions,
) -> JoinHandle<Result<IvyJson, IvyError>> {
    options.request_id = Some(id.into());
    let rpc = rpc.clone();
    tokio::spawn(async move {
        rpc.request("account/logout".into(), IvyJson::Null, options)
            .await
    })
}
async fn result(task: JoinHandle<Result<IvyJson, IvyError>>) -> Result<IvyJson, IvyError> {
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn failed_intent_sends_no_bytes_and_releases_original_id() {
    let (rpc, mut peer) = fixture(Events::default());
    let rejected = request(
        &rpc,
        "same",
        RequestOptions {
            before_send: Some(Arc::new(|_| {
                Box::pin(async { Err(IvyError::new("storage_failed", "fixture")) })
            })),
            ..Default::default()
        },
    );
    assert_eq!(result(rejected).await.unwrap_err().code, "storage_failed");
    let retry = request(&rpc, "same", RequestOptions::default());
    let sent = peer.sent().await;
    assert_eq!(sent.get("id"), Some(&IvyJson::from("same")));
    peer.reply(sent.get("id").unwrap().clone()).await;
    result(retry).await.unwrap();
    rpc.close("fixture_closed").await;
}

#[tokio::test]
async fn dropped_waiter_still_commits_and_async_hooks_fence_bytes_and_result() {
    let (rpc, mut peer) = fixture(Events::default());
    let release = Arc::new(tokio::sync::Notify::new());
    let committed = Arc::new(tokio::sync::Notify::new());
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let entered = Arc::new(Mutex::new(Some(entered_tx)));
    let task = request(
        &rpc,
        "owned",
        RequestOptions {
            before_send: Some(Arc::new({
                let release = release.clone();
                move |_| {
                    let entered = entered.clone();
                    let release = release.clone();
                    Box::pin(async move {
                        entered.lock().unwrap().take().unwrap().send(()).unwrap();
                        release.notified().await;
                        Ok(())
                    })
                }
            })),
            before_resolve: Some(Arc::new({
                let committed = committed.clone();
                move |_, _| {
                    let committed = committed.clone();
                    Box::pin(async move {
                        committed.notify_one();
                        Ok(())
                    })
                }
            })),
            ..Default::default()
        },
    );
    entered_rx.await.unwrap();
    let mut byte = [0];
    use tokio::io::AsyncReadExt;
    assert!(
        tokio::time::timeout(Duration::from_millis(20), peer.input.read(&mut byte))
            .await
            .is_err()
    );
    release.notify_one();
    let sent = peer.sent().await;
    task.abort(); // Detach the Rust waiter, not the original native request.
    peer.reply(sent.get("id").unwrap().clone()).await;
    tokio::time::timeout(Duration::from_secs(5), committed.notified())
        .await
        .unwrap();
    let retry = request(&rpc, "owned", RequestOptions::default());
    let sent = peer.sent().await;
    peer.reply(sent.get("id").unwrap().clone()).await;
    result(retry).await.unwrap();
    rpc.close("done").await;
}

#[tokio::test]
async fn failed_receipt_closes_pending_requests_after_recovery_commit() {
    let closed = Arc::new(AtomicUsize::new(0));
    let events = Events {
        on_close: Arc::new({
            let closed = closed.clone();
            move |_| {
                let closed = closed.clone();
                Box::pin(async move {
                    closed.store(1, Ordering::SeqCst);
                    Ok(())
                })
            }
        }),
        ..Default::default()
    };
    let (rpc, mut peer) = fixture(events);
    let first = request(
        &rpc,
        "one",
        RequestOptions {
            before_resolve: Some(Arc::new(|_, _| {
                Box::pin(async { Err(IvyError::internal()) })
            })),
            ..Default::default()
        },
    );
    peer.sent().await;
    let second = request(&rpc, "two", RequestOptions::default());
    peer.sent().await;
    peer.reply("one".into()).await;
    for task in [first, second] {
        let error = result(task).await.unwrap_err();
        assert_eq!(error.code, "native_outcome_commit_failed");
        assert_eq!(error.outcome, Outcome::Unknown);
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }
    assert_eq!(rpc.closed().await, "native_outcome_commit_failed");
}

#[tokio::test]
async fn detached_timeout_retains_id_until_valid_late_reply_and_never_commits_it() {
    let (rpc, mut peer) = fixture(Events::default());
    let commits = Arc::new(AtomicUsize::new(0));
    let first = request(
        &rpc,
        "late",
        RequestOptions {
            timeout_ms: 30,
            detach_on_timeout: true,
            before_resolve: Some(Arc::new({
                let commits = commits.clone();
                move |_, _| {
                    let commits = commits.clone();
                    Box::pin(async move {
                        commits.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    })
                }
            })),
            ..Default::default()
        },
    );
    peer.sent().await;
    let active = request(&rpc, "active", RequestOptions::default());
    peer.sent().await;
    assert_eq!(
        result(first).await.unwrap_err().code,
        "native_observation_deadline"
    );
    assert!(rpc.connected());
    assert_eq!(
        result(request(&rpc, "late", RequestOptions::default()))
            .await
            .unwrap_err()
            .code,
        "native_state_conflict"
    );
    peer.reply("late".into()).await;
    peer.reply("active".into()).await;
    result(active).await.unwrap();
    assert_eq!(commits.load(Ordering::SeqCst), 0);
    let reuse = request(&rpc, "late", RequestOptions::default());
    peer.sent().await;
    peer.reply("late".into()).await;
    result(reuse).await.unwrap();
    rpc.close("done").await;
}

#[tokio::test]
async fn reserved_lanes_and_connection_deadlines_preserve_uncertainty() {
    let (rpc, mut peer) = fixture(Events::default());
    let mut pending = Vec::new();
    for n in 0..16 {
        pending.push(request(
            &rpc,
            &format!("background-{n}"),
            RequestOptions {
                detach_on_timeout: true,
                ..Default::default()
            },
        ));
        peer.sent().await;
    }
    assert_eq!(
        result(request(
            &rpc,
            "full",
            RequestOptions {
                detach_on_timeout: true,
                ..Default::default()
            }
        ))
        .await
        .unwrap_err()
        .code,
        "native_capacity"
    );
    let foreground = request(
        &rpc,
        "foreground",
        RequestOptions {
            priority: codex_admission::Priority::Interaction,
            detach_on_timeout: true,
            ..Default::default()
        },
    );
    peer.sent().await;
    peer.reply("foreground".into()).await;
    result(foreground).await.unwrap();
    let deadline = request(
        &rpc,
        "deadline",
        RequestOptions {
            timeout_ms: 20,
            ..Default::default()
        },
    );
    peer.sent().await;
    assert_eq!(
        result(deadline).await.unwrap_err().code,
        "native_deadline_exceeded"
    );
    for p in pending {
        assert_eq!(result(p).await.unwrap_err().outcome, Outcome::Unknown);
    }
}

#[tokio::test]
async fn expired_automatic_observations_release_slots_and_keep_late_replies_safe() {
    let (rpc, mut peer) = fixture(Events::default());
    let mut ids = Vec::new();
    // One more than the existing 16 background slots must still be admitted.
    for _ in 0..=16 {
        let owner = rpc.clone();
        let pending = tokio::spawn(async move {
            owner
                .request(
                    "account/logout".into(),
                    IvyJson::Null,
                    RequestOptions {
                        timeout_ms: 10,
                        detach_on_timeout: true,
                        ..Default::default()
                    },
                )
                .await
        });
        let sent = peer.sent().await;
        ids.push(sent.required("id").unwrap().clone());
        assert_eq!(
            result(pending).await.unwrap_err().code,
            "native_observation_deadline"
        );
    }
    assert!(rpc.connected());
    let reused = request(&rpc, &ids[0].as_str().unwrap(), RequestOptions::default());
    assert_eq!(
        result(reused).await.unwrap_err().code,
        "native_state_conflict"
    );
    peer.reply(ids[0].clone()).await;
    peer.reply(ids.last().unwrap().clone()).await;
    let active = request(&rpc, "active", RequestOptions::default());
    peer.sent().await;
    peer.reply("active".into()).await;
    result(active).await.unwrap();
    rpc.close("done").await;
}

#[tokio::test]
async fn framing_and_write_failures_close_without_replaying() {
    for (bytes, code) in [
        (b"{\"result\":null}".as_slice(), "native_truncated_frame"),
        (b"\xff\n".as_slice(), "native_invalid_frame"),
        (
            b"{\"id\":\"unknown\",\"result\":null}\n".as_slice(),
            "native_unknown_response",
        ),
    ] {
        let (rpc, mut peer) = fixture(Events::default());
        let pending = request(&rpc, "owned", RequestOptions::default());
        peer.sent().await;
        peer.output.write_all(bytes).await.unwrap();
        peer.output.shutdown().await.unwrap();
        assert_eq!(result(pending).await.unwrap_err().code, code);
        assert_eq!(rpc.closed().await, code);
    }
    let (rpc, peer) = fixture(Events::default());
    drop(peer.input);
    assert_eq!(
        result(request(&rpc, "owned", RequestOptions::default()))
            .await
            .unwrap_err()
            .outcome,
        Outcome::Unknown
    );
    assert_eq!(rpc.closed().await, "native_write_failed");
}

#[tokio::test]
async fn wire_and_hook_transcript_preserves_ids_order_and_reply_bytes() {
    let trace = Arc::new(Mutex::new(Vec::<IvyJson>::new()));
    let events = Events {
        on_request: Arc::new({
            let trace = trace.clone();
            move |r| {
                trace.lock().unwrap().push(IvyJson::object([(
                    "request",
                    IvyJson::object([
                        ("id", r.id),
                        ("method", IvyJson::String(r.method)),
                        ("params", r.params),
                    ]),
                )]));
                Box::pin(async { Ok(()) })
            }
        }),
        on_notification: Arc::new({
            let trace = trace.clone();
            move |r| {
                trace.lock().unwrap().push(IvyJson::object([(
                    "notification",
                    IvyJson::object([("method", IvyJson::String(r.method)), ("params", r.params)]),
                )]));
                Box::pin(async { Ok(()) })
            }
        }),
        on_close: Arc::new({
            let trace = trace.clone();
            move |c| {
                trace
                    .lock()
                    .unwrap()
                    .push(IvyJson::object([("close", c.into())]));
                Box::pin(async { Ok(()) })
            }
        }),
    };
    let (rpc, mut peer) = fixture(events);
    let id = JsString(vec![b'i' as u16, 0xd800]);
    let pending = {
        let rpc = rpc.clone();
        let trace1 = trace.clone();
        let trace2 = trace.clone();
        tokio::spawn(async move {
            rpc.request(
                "account/logout".into(),
                IvyJson::Null,
                RequestOptions {
                    request_id: Some(id),
                    before_send: Some(Arc::new(move |_| {
                        trace1.lock().unwrap().push("intent".into());
                        Box::pin(async { Ok(()) })
                    })),
                    before_resolve: Some(Arc::new(move |_, _| {
                        trace2.lock().unwrap().push("receipt".into());
                        Box::pin(async { Ok(()) })
                    })),
                    ..Default::default()
                },
            )
            .await
        })
    };
    let sent = peer.sent().await;
    trace
        .lock()
        .unwrap()
        .push(IvyJson::object([("sent", sent.clone())]));
    peer.emit(IvyJson::object([
        ("id", sent.get("id").unwrap().clone()),
        ("result", value(r#"{"untouched":["Grüße",null,false]}"#)),
    ]))
    .await;
    let reply = result(pending).await.unwrap();
    trace
        .lock()
        .unwrap()
        .push(IvyJson::object([("reply", reply)]));
    rpc.notify("initialized".into(), IvyJson::Null)
        .await
        .unwrap();
    let sent = peer.sent().await;
    trace
        .lock()
        .unwrap()
        .push(IvyJson::object([("sent", sent)]));
    for id in [IvyJson::Number(-2.0), "-2".into()] {
        peer.emit(IvyJson::object([
            ("id", id),
            ("method", "currentTime/read".into()),
            ("params", value(r#"{"threadId":"original"}"#)),
        ]))
        .await;
    }
    peer.emit(value(
        r#"{"method":"serverRequest/resolved","params":{"requestId":-2,"threadId":"original"}}"#,
    ))
    .await;
    // A response to a separate request fences preceding observation callbacks.
    let fence = request(&rpc, "fence", RequestOptions::default());
    peer.sent().await;
    peer.reply("fence".into()).await;
    result(fence).await.unwrap();
    for id in [IvyJson::Number(-2.0), "-2".into()] {
        rpc.answer(
            "currentTime/read".into(),
            id,
            value(r#"{"result":{}}"#),
            Arc::new({
                let trace = trace.clone();
                move |_| {
                    trace.lock().unwrap().push("answer-intent".into());
                    Box::pin(async { Ok(()) })
                }
            }),
        )
        .await
        .unwrap();
        let sent = peer.sent().await;
        trace
            .lock()
            .unwrap()
            .push(IvyJson::object([("sent", sent)]));
    }
    rpc.close("fixture_closed").await;
    assert_eq!(
        IvyJson::Array(trace.lock().unwrap().clone()),
        value(
            r#"["intent",{"sent":{"id":"i\ud800","method":"account/logout","params":null}},"receipt",{"reply":{"result":{"untouched":["Grüße",null,false]}}},{"sent":{"method":"initialized"}},{"request":{"id":-2,"method":"currentTime/read","params":{"threadId":"original"}}},{"request":{"id":"-2","method":"currentTime/read","params":{"threadId":"original"}}},{"notification":{"method":"serverRequest/resolved","params":{"requestId":-2,"threadId":"original"}}},"answer-intent",{"sent":{"id":-2,"result":{}}},"answer-intent",{"sent":{"id":"-2","result":{}}},{"close":"fixture_closed"}]"#
        )
    );
}
