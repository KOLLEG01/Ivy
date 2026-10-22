use futures_util::{SinkExt, StreamExt};
use ivy_contracts::{
    json::{parse, stringify},
    limits::CONNECTION_IN_FLIGHT_REQUESTS,
};
use ivy_sdk::{IvyError, IvyJson, JsString, Outcome, rpc::*, service::*};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{net::TcpListener, sync::mpsc, task::JoinHandle};
use tokio_tungstenite::{accept_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;

struct Peer {
    connection: ServiceConnection,
    frames: mpsc::UnboundedReceiver<IvyJson>,
    received: Arc<AtomicUsize>,
    stop: CancellationToken,
    server: JoinHandle<()>,
}
impl Peer {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/ivy/", listener.local_addr().unwrap());
        let (sender, frames) = mpsc::unbounded_channel();
        let received = Arc::new(AtomicUsize::new(0));
        let count = received.clone();
        let stop = CancellationToken::new();
        let stopped = stop.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            loop {
                let item =
                    tokio::select! {biased;_=stopped.cancelled()=>break,item=socket.next()=>item};
                let Some(Ok(Message::Text(text))) = item else {
                    break;
                };
                let frame = parse(text.as_str(), 1_000_000).unwrap();
                count.fetch_add(1, Ordering::SeqCst);
                if sender.send(frame.clone()).is_err() {
                    break;
                }
                if frame.get("method").and_then(IvyJson::as_js_string)
                    == Some(&JsString::from("echo"))
                {
                    let mut result = IvyJson::object([(
                        "present",
                        IvyJson::Bool(frame.get("params").is_some()),
                    )]);
                    if let Some(params) = frame.get("params") {
                        result.insert("value", params.clone());
                    }
                    let reply = IvyJson::object([
                        ("jsonrpc", "2.0".into()),
                        ("id", frame.get("id").unwrap().clone()),
                        ("result", result),
                    ]);
                    socket
                        .send(Message::Text(stringify(&reply, 1_000_000).unwrap().into()))
                        .await
                        .unwrap();
                }
            }
        });
        let connection = ServiceConnection::connect(
            &url,
            "fixture-token",
            ConnectionOptions::new("fixture-node".into()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        connection.set_generation(17);
        connection.set_ready(true);
        Self {
            connection,
            frames,
            received,
            stop,
            server,
        }
    }
    async fn frame(&mut self) -> IvyJson {
        tokio::time::timeout(Duration::from_secs(3), self.frames.recv())
            .await
            .unwrap()
            .unwrap()
    }
    async fn finish(self) {
        self.connection.close();
        self.stop.cancel();
        self.server.await.unwrap();
    }
}
fn options() -> RequestOptions {
    RequestOptions {
        timeout: Duration::from_secs(3),
        ..Default::default()
    }
}

#[tokio::test]
async fn actual_optional_wire_distinguishes_omission_null_and_existing_json_calls() {
    let mut peer = Peer::start().await;
    let client = peer.connection.clone();
    let rpc: &dyn OptionalRpcClient = &client;
    assert_eq!(rpc.service_connection().unwrap().generation(), 17);
    let omitted = rpc.request_optional("echo", None, options()).await.unwrap();
    assert_eq!(omitted, IvyJson::object([("present", false.into())]));
    let frame = peer.frame().await;
    assert_eq!(frame.as_object().unwrap().len(), 3);
    assert!(frame.get("params").is_none());
    let mut ids = vec![frame.get("id").unwrap().clone()];
    for params in [
        IvyJson::Null,
        false.into(),
        IvyJson::Number(0.0),
        IvyJson::String(JsString(vec![0xd800])),
        IvyJson::Array(vec![IvyJson::Null]),
        IvyJson::object([("unknown", "retained".into())]),
    ] {
        for legacy in [false, true] {
            let result = if legacy {
                rpc.request("echo", params.clone(), options()).await
            } else {
                rpc.request_optional("echo", Some(params.clone()), options())
                    .await
            }
            .unwrap();
            assert_eq!(
                result,
                IvyJson::object([("present", true.into()), ("value", params.clone())])
            );
            let frame = peer.frame().await;
            assert_eq!(frame.as_object().unwrap().len(), 4);
            assert_eq!(frame.get("params"), Some(&params));
            assert!(!ids.contains(frame.get("id").unwrap()));
            ids.push(frame.get("id").unwrap().clone());
        }
    }
    assert_eq!(client.generation(), 17);
    assert!(client.ready());
    peer.finish().await;
}

#[tokio::test]
async fn optional_admission_preserves_closed_before_cancelled_and_exact_pre_send_errors() {
    let peer = Peer::start().await;
    let mut cancelled = options();
    cancelled.cancellation.cancel();
    let mut reason = IvyError::new_js("cancelled-fixture".into(), JsString(vec![0xd800, 65]))
        .with_outcome(Outcome::Completed);
    reason.details = Some(IvyJson::String(JsString(vec![0xdfff])));
    cancelled.cancel_error = reason.clone();
    for params in [None, Some(IvyJson::Null)] {
        let error = peer
            .connection
            .request_optional("hold", params, cancelled.clone())
            .await
            .unwrap_err();
        assert_eq!(error.to_wire(), reason.to_wire());
    }
    peer.connection.close_with(reason);
    for params in [None, Some(IvyJson::Null)] {
        let error = peer
            .connection
            .request_optional("hold", params, cancelled.clone())
            .await
            .unwrap_err();
        assert_eq!(error.code, "service_unavailable");
        assert_eq!(error.outcome, Outcome::NotExecuted);
    }
    assert_eq!(peer.received.load(Ordering::SeqCst), 0);
    peer.finish().await;
}

#[tokio::test]
async fn optional_pending_slots_cleanup_on_cancel_timeout_drop_and_connection_close() {
    let mut peer = Peer::start().await;
    let client = peer.connection.clone();
    let cancelled = options();
    let signal = cancelled.cancellation.clone();
    let mut pending = client.request_optional("hold", None, cancelled);
    assert!(futures_util::poll!(pending.as_mut()).is_pending());
    assert!(peer.frame().await.get("params").is_none());
    signal.cancel();
    let error = pending.await.unwrap_err();
    assert_eq!(error.code, "outcome_unknown");
    assert_eq!(error.outcome, Outcome::Unknown);
    let mut pending = client.request_optional(
        "hold",
        Some(IvyJson::Null),
        RequestOptions {
            timeout: Duration::from_millis(10),
            ..options()
        },
    );
    assert!(futures_util::poll!(pending.as_mut()).is_pending());
    assert!(peer.frame().await.get("params").unwrap().is_null());
    let error = pending.await.unwrap_err();
    assert_eq!(error.code, "deadline_exceeded");
    assert_eq!(error.outcome, Outcome::Unknown);
    let mut dropped = client.request_optional("hold", None, options());
    assert!(futures_util::poll!(dropped.as_mut()).is_pending());
    peer.frame().await;
    drop(dropped);
    let mut full = Vec::new();
    for _ in 0..CONNECTION_IN_FLIGHT_REQUESTS {
        let mut request = client.request_optional("hold", None, options());
        assert!(futures_util::poll!(request.as_mut()).is_pending());
        full.push(request);
    }
    for params in [None, Some(IvyJson::Null)] {
        let error = client
            .request_optional("echo", params, options())
            .await
            .unwrap_err();
        assert_eq!(error.code, "limit_exceeded");
        assert_eq!(error.outcome, Outcome::NotExecuted);
    }
    drop(full.pop());
    assert_eq!(
        client
            .request_optional("echo", Some(IvyJson::Null), options())
            .await
            .unwrap()
            .get("value"),
        Some(&IvyJson::Null)
    );
    drop(full);
    client
        .request_optional("echo", None, options())
        .await
        .unwrap();
    assert_eq!(
        peer.received.load(Ordering::SeqCst),
        CONNECTION_IN_FLIGHT_REQUESTS + 5
    );
    let mut first = client.request_optional("hold", None, options());
    let mut second = client.request_optional("hold", Some(IvyJson::Null), options());
    assert!(futures_util::poll!(first.as_mut()).is_pending());
    assert!(futures_util::poll!(second.as_mut()).is_pending());
    let mut reason = IvyError::new_js("closed-fixture".into(), JsString(vec![0xdfff, 66]))
        .with_outcome(Outcome::Completed);
    reason.details = Some(false.into());
    client.close_with(reason.clone());
    assert_eq!(first.await.unwrap_err().to_wire(), reason.to_wire());
    assert_eq!(second.await.unwrap_err().to_wire(), reason.to_wire());
    peer.finish().await;
}
