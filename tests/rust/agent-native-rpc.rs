#[path = "../../packages/host-runtime/rust/src/codex_admission.rs"]
mod codex_admission;
#[path = "../../packages/host-runtime/rust/src/codex_framing.rs"]
mod codex_framing;
#[path = "../../packages/host-runtime/rust/src/codex_rpc.rs"]
mod codex_rpc;
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
        NativeRpc::start(&catalog(), input, output, events).unwrap(),
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
async fn wire_and_hook_transcript_matches_unchanged_typescript() {
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
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .arg(root.join("tests/fixtures/native-rpc-oracle.mjs"))
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        IvyJson::Array(trace.lock().unwrap().clone()),
        value(std::str::from_utf8(&output.stdout).unwrap())
    );
}
