use futures_util::{SinkExt, StreamExt};
use ivy_contracts::{IvyError, IvyJson, IvyResult, json::parse, limits::MANAGEMENT_FRAME_BYTES};
use ivy_hive::{
    admission::{RequestBudget, RequestBudgetOptions},
    browser_session::Peers as _,
    live_routing::LiveRouting,
    native_runtime::NativeRuntime,
    server_peers::{self, Peers},
    server_rpc::{Reply, Storage},
    storage::{Request, RequestOptions},
    tool_binding_diagnostics::ToolBindingDiagnostics,
    ws_socket_io::Sockets,
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{oneshot, watch},
    time::timeout,
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        protocol::{Role, WebSocketConfig},
    },
};

fn json(text: &str) -> IvyJson {
    parse(text, MANAGEMENT_FRAME_BYTES).unwrap()
}
fn context() -> IvyJson {
    json(r#"{"principalId":"fixture","credentialDigest":"credential","transport":"ws"}"#)
}
struct NoStorage;
impl Storage for NoStorage {
    fn dispatch(&self, _: Request, _: RequestOptions) -> Reply {
        panic!("Unexpected DB dispatch")
    }
}
fn peers(
    storage: Arc<dyn Storage>,
    requests: RequestBudget,
    direct: server_peers::Direct,
) -> (Peers, Arc<Mutex<Vec<IvyError>>>) {
    let unhandled = Arc::new(Mutex::new(vec![]));
    (
        Peers::new(
            server_peers::Owners {
                storage,
                requests,
                direct,
                routing: Arc::new(Mutex::new(LiveRouting::new(false, None))),
                diagnostics: Arc::new(Mutex::new(ToolBindingDiagnostics::new(
                    Box::new(|_| Ok(())),
                    Arc::new(|| 10000.0),
                    Arc::new(NativeRuntime),
                ))),
                now: Arc::new(|| 10000.0),
                ids: Arc::new(|| "fixture-id".into()),
                provider_error: Arc::new(|_| panic!("Unexpected provider error")),
                wake_browser_push: Arc::new(|| {}),
                clear_authentication: Arc::new(|| {}),
                unhandled: {
                    let log = unhandled.clone();
                    Arc::new(move |_, error| log.lock().unwrap().push(error))
                },
            },
            Default::default(),
        ),
        unhandled,
    )
}
async fn pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let (server, client) = tokio::join!(
        listener.accept(),
        TcpStream::connect(listener.local_addr().unwrap())
    );
    let server = server.unwrap().0;
    let client = client.unwrap();
    server.set_nodelay(true).unwrap();
    client.set_nodelay(true).unwrap();
    (server, client)
}
async fn client<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    stream: S,
) -> WebSocketStream<S> {
    WebSocketStream::from_raw_socket(
        stream,
        Role::Client,
        Some(
            WebSocketConfig::default()
                .max_frame_size(Some(MANAGEMENT_FRAME_BYTES))
                .max_message_size(Some(MANAGEMENT_FRAME_BYTES))
                .write_buffer_size(0),
        ),
    )
    .await
}
async fn text<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    client: &mut WebSocketStream<S>,
) -> IvyJson {
    let frame = timeout(Duration::from_secs(10), client.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let Message::Text(text) = frame else {
        panic!("Expected text reply: {frame:?}")
    };
    json(&text)
}

#[tokio::test]
async fn production_tcp_accepts_large_valid_frames_and_preserves_control_and_binary_policy() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let (peers, log) = peers(
        Arc::new(NoStorage),
        RequestBudget::default(),
        Arc::new(move |_, _, _, _| {
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(Some(Box::pin(async { Ok("ok".into()) })))
        }),
    );
    let (server, stream) = pair().await;
    let (stop, stopping) = watch::channel(false);
    let sockets = Sockets::default();
    let work = tokio::spawn(async move { sockets.run(server, peers, context(), stopping).await });
    let mut client = client(stream).await;
    // The old library default rejected a single frame above 16 MiB even though
    // the management contract admits 32 MiB. No DB or provider effect is used.
    let request = format!(
        r#"{{"jsonrpc":"2.0","id":"large","method":"fixture.read","params":{{"padding":"{}"}}}}"#,
        "x".repeat(17 * 1024 * 1024)
    );
    client.send(Message::Text(request.into())).await.unwrap();
    let reply = text(&mut client).await;
    assert_eq!(reply.get("id"), Some(&"large".into()));
    assert_eq!(reply.get("result"), Some(&"ok".into()));
    client
        .send(Message::Binary(vec![1, 2, 3].into()))
        .await
        .unwrap();
    let reply = text(&mut client).await;
    assert_eq!(
        reply
            .required("error")
            .unwrap()
            .required("data")
            .unwrap()
            .get("code"),
        Some(&"invalid_frame".into())
    );
    client
        .send(Message::Ping(vec![4, 5, 6].into()))
        .await
        .unwrap();
    assert_eq!(
        timeout(Duration::from_secs(5), client.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        Message::Pong(vec![4, 5, 6].into())
    );
    client.close(None).await.unwrap();
    assert!(matches!(
        timeout(Duration::from_secs(5), client.next())
            .await
            .unwrap(),
        Some(Ok(Message::Close(_)))
    ));
    timeout(Duration::from_secs(5), work)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(log.lock().unwrap().is_empty());
    drop(stop);
}

#[tokio::test]
async fn oversized_advertised_frame_closes_before_receiving_its_payload() {
    let (peers, log) = peers(
        Arc::new(NoStorage),
        RequestBudget::default(),
        Arc::new(|_, _, _, _| panic!("Oversized frame reached dispatch")),
    );
    let (server, mut client) = pair().await;
    let (_stop, stopping) = watch::channel(false);
    let sockets = Sockets::default();
    let work = tokio::spawn(async move { sockets.run(server, peers, context(), stopping).await });
    let mut header = vec![0x81, 0xff];
    header.extend_from_slice(&((MANAGEMENT_FRAME_BYTES + 1) as u64).to_be_bytes());
    header.extend_from_slice(&[1, 2, 3, 4]);
    client.write_all(&header).await.unwrap();
    assert!(
        timeout(Duration::from_secs(5), work)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    let mut byte = [0];
    assert_eq!(
        timeout(Duration::from_secs(5), client.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert!(log.lock().unwrap().is_empty());
}

#[tokio::test]
async fn shutdown_and_peer_termination_interrupt_a_blocked_production_send() {
    for global in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let (peers, log) = peers(
            Arc::new(NoStorage),
            RequestBudget::default(),
            Arc::new(move |_, _, _, _| {
                seen.fetch_add(1, Ordering::SeqCst);
                Ok(Some(Box::pin(async {
                    Ok("x".repeat(8 * 1024 * 1024).into())
                })))
            }),
        );
        let observed = peers.clone();
        let (server, stream) = tokio::io::duplex(1024);
        let (stop, stopping) = watch::channel(false);
        let sockets = Sockets::default();
        let work =
            tokio::spawn(async move { sockets.run(server, peers, context(), stopping).await });
        let mut client = client(stream).await;
        client
            .send(Message::Text(
                r#"{"jsonrpc":"2.0","id":"blocked","method":"fixture.read","params":{}}"#.into(),
            ))
            .await
            .unwrap();
        // Reading just its first wire byte proves the writer started and cannot
        // fit its payload into this actual, deliberately stalled duplex pipe.
        let mut byte = [0];
        timeout(
            Duration::from_secs(5),
            client.get_mut().read_exact(&mut byte),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(byte[0], 0x81);
        if global {
            // A blocked response must not stop ingress on the same connection.
            client
                .send(Message::Text(
                    r#"{"jsonrpc":"2.0","id":"second","method":"fixture.read","params":{}}"#.into(),
                ))
                .await
                .unwrap();
            timeout(Duration::from_secs(5), async {
                while calls.load(Ordering::SeqCst) != 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            stop.send_replace(true);
        } else {
            observed.health().unwrap();
            observed.health().unwrap();
        }
        timeout(Duration::from_secs(5), work)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(observed.iter().count(), 0);
        assert!(log.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn actual_close_releases_connection_but_retains_accepted_peer_work() {
    struct Pending {
        complete: Arc<AtomicBool>,
        cancelled: Arc<AtomicBool>,
    }
    impl Drop for Pending {
        fn drop(&mut self) {
            if !self.complete.load(Ordering::SeqCst) {
                self.cancelled.store(true, Ordering::SeqCst);
            }
        }
    }
    struct Worker {
        control: Mutex<Option<oneshot::Sender<IvyResult<IvyJson>>>>,
        complete: Arc<AtomicBool>,
        cancelled: Arc<AtomicBool>,
    }
    impl Storage for Worker {
        fn dispatch(&self, mut request: Request, _: RequestOptions) -> Reply {
            request.prepare().unwrap();
            let (send, receive) = oneshot::channel();
            *self.control.lock().unwrap() = Some(send);
            let guard = Pending {
                complete: self.complete.clone(),
                cancelled: self.cancelled.clone(),
            };
            Box::pin(async move {
                let _guard = guard;
                let result = receive.await.unwrap();
                _guard.complete.store(true, Ordering::SeqCst);
                request.complete(result)
            })
        }
    }
    let worker = Arc::new(Worker {
        control: Mutex::new(None),
        complete: Arc::new(AtomicBool::new(false)),
        cancelled: Arc::new(AtomicBool::new(false)),
    });
    let requests = RequestBudget::new(RequestBudgetOptions {
        maximum: 1,
        per_consumer: 1,
        ..Default::default()
    });
    let connections = RequestBudget::new(RequestBudgetOptions {
        maximum: 1,
        per_consumer: 1,
        ..Default::default()
    });
    let connection = connections.acquire(&"client".into(), None).unwrap();
    let (peers, log) = peers(
        worker.clone(),
        requests.clone(),
        Arc::new(|_, _, _, _| Ok(None)),
    );
    let observed = peers.clone();
    let (server, stream) = pair().await;
    let (_stop, stopping) = watch::channel(false);
    let sockets = Sockets::default();
    let work = tokio::spawn(async move {
        let mut connection = connection;
        let result = sockets.run(server, peers, context(), stopping).await;
        connection.release();
        result
    });
    let mut client = client(stream).await;
    client
        .send(Message::Text(
            r#"{"jsonrpc":"2.0","id":"pending","method":"fixture.wait","params":{}}"#.into(),
        ))
        .await
        .unwrap();
    timeout(Duration::from_secs(5), async {
        while worker.control.lock().unwrap().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(connections.acquire(&"client".into(), None).is_err());
    assert!(requests.acquire(&"credential".into(), None).is_err());
    drop(client);
    assert!(
        timeout(Duration::from_secs(5), work)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(observed.iter().count(), 0);
    let mut released = connections.acquire(&"client".into(), None).unwrap();
    released.release();
    assert!(requests.acquire(&"credential".into(), None).is_err());
    assert!(!worker.complete.load(Ordering::SeqCst));
    assert!(!worker.cancelled.load(Ordering::SeqCst));
    worker
        .control
        .lock()
        .unwrap()
        .take()
        .unwrap()
        .send(Ok(json(r#"{"kind":"result","value":{"ok":true}}"#)))
        .unwrap();
    timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(mut lease) = requests.acquire(&"credential".into(), None) {
                lease.release();
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(worker.complete.load(Ordering::SeqCst));
    assert!(!worker.cancelled.load(Ordering::SeqCst));
    assert!(log.lock().unwrap().is_empty());
}
