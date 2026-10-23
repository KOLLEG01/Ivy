use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{IvyError, IvyJson, Outcome, json::parse};
use ivy_hive::{
    server_peers::PeerSocket,
    ws_send::{self, Socket},
    ws_socket_io,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn obj(value: &Value) -> IvyJson {
    parse(&value.to_string(), usize::MAX).unwrap()
}
fn traces() -> Vec<Value> {
    let mut traces = Vec::new();
    for (name, value) in [
        ("empty", json!("")),
        (
            "object",
            json!({"jsonrpc":"2.0","id":1,"result":{"ok":true}}),
        ),
        ("unicode", json!({"text":"雪🦊\n\u{0000}\""})),
    ] {
        let text = value.to_string();
        let n = text.len();
        let header = if n < 126 {
            2
        } else if n < 65536 {
            4
        } else {
            10
        };
        traces.push(json!({"name":name,"steps":[{"op":"send","value":value},{"op":"snapshot","name":"sent"},{"op":"resume","bytes":n+header},{"op":"client-end"},{"op":"settle-close"},{"op":"snapshot","name":"closed"}]}));
    }
    for n in [125, 126, 127, 65535, 65536] {
        let header = if n < 126 {
            2
        } else if n < 65536 {
            4
        } else {
            10
        };
        traces.push(json!({"name":format!("length-{n}"),"steps":[{"op":"send","encodedLength":n},{"op":"snapshot","name":"sent"},{"op":"resume","bytes":n+header},{"op":"client-end"},{"op":"settle-close"},{"op":"snapshot","name":"closed"}]}));
    }
    traces.push(json!({"name":"ping-and-control","steps":[{"op":"ping"},{"op":"raw","base64":"igNhYmM="},{"op":"snapshot","name":"sent"},{"op":"resume","bytes":7},{"op":"client-end"},{"op":"settle-close"},{"op":"snapshot","name":"closed"}]}));
    traces.push(json!({"name":"terminate-once","steps":[{"op":"snapshot","name":"open"},{"op":"terminate"},{"op":"snapshot","name":"requested"},{"op":"settle-close"},{"op":"terminate"},{"op":"send","value":{"ignored":true}},{"op":"ping"},{"op":"snapshot","name":"closed"}]}));
    traces.push(json!({"name":"oversized-preencoded","steps":[{"op":"send","encodedLength":64*1024*1024+1},{"op":"snapshot","name":"requested"},{"op":"settle-close"},{"op":"snapshot","name":"closed"}]}));
    traces.push(json!({"name":"close-callback-error","closeError":true,"steps":[{"op":"terminate"},{"op":"snapshot","name":"requested"},{"op":"settle-close"},{"op":"snapshot","name":"closed"}]}));
    traces.push(json!({"name":"write-error-after-reset","steps":[{"op":"client-reset"},{"op":"send","value":{"result":"late"}},{"op":"snapshot","name":"submitted"},{"op":"ping"},{"op":"send","value":{"result":"also-late"}},{"op":"snapshot","name":"resubmitted"},{"op":"settle-close"},{"op":"snapshot","name":"closed"}]}));
    let n = 8 * 1024 * 1024;
    traces.push(json!({"name":"pressure-observer-abandoned","pause":true,"steps":[{"op":"send","encodedLength":n},{"op":"snapshot","name":"first"},{"op":"send","encodedLength":n},{"op":"drop-observer"},{"op":"snapshot","name":"second"},{"op":"resume","bytes":2*(n+10)},{"op":"snapshot","name":"drained"},{"op":"client-end"},{"op":"settle-close"},{"op":"snapshot","name":"closed"}]}));
    traces
}
async fn native(input: &Value) -> Value {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let connect = TcpStream::connect(("127.0.0.1", port));
    let (accepted, client) = tokio::join!(listener.accept(), connect);
    let (server, _) = accepted.unwrap();
    let mut client = Some(client.unwrap());
    let selected = Arc::new(Mutex::new(None));
    let closed = Arc::new(Mutex::new(0));
    let unhandled = Arc::new(Mutex::new(Vec::<Value>::new()));
    let fail = input["closeError"] == true;
    let mut stream = ws_socket_io::attach(server, {
        let selected = selected.clone();
        let closed = closed.clone();
        let unhandled = unhandled.clone();
        move |socket| {
            *selected.lock().unwrap() = Some(socket);
            ws_socket_io::Owners {
                closed: Box::new(move || {
                    *closed.lock().unwrap() += 1;
                    if fail {
                        Err(
                            IvyError::new("fixture_close_error", "Close callback failed.")
                                .with_outcome(Outcome::Unknown),
                        )
                    } else {
                        Ok(())
                    }
                }),
                unhandled: Arc::new(move |_, error| {
                    unhandled.lock().unwrap().push(
                        serde_json::from_str(
                            &ivy_contracts::json::stringify(&error.to_wire(), usize::MAX).unwrap(),
                        )
                        .unwrap(),
                    )
                }),
            }
        }
    })
    .await
    .unwrap();
    let mut socket = selected.lock().unwrap().take().unwrap();
    let mut observer = Some(socket.clone());
    let mut snapshots = Vec::new();
    let mut received = Vec::new();
    let mut client_closed = false;
    for step in input["steps"].as_array().unwrap() {
        match step["op"].as_str().unwrap() {
   "send"=>{let encoded=if let Some(n)=step["encodedLength"].as_u64(){Some(format!("\"{}\"","x".repeat(n as usize-2)))}else{step["encoded"].as_str().map(str::to_string)};ws_send::send(&mut socket,&obj(step.get("value").unwrap_or(&Value::Null)),encoded.as_deref()).unwrap();}
   "ping"=>socket.ping().unwrap(),
   "raw"=>stream.write_all(&STANDARD.decode(step["base64"].as_str().unwrap()).unwrap()).await.unwrap(),
   "snapshot"=>snapshots.push(json!({"name":step["name"],"open":socket.is_open(),"buffered":socket.buffered_amount() as u64,"closed":*closed.lock().unwrap(),"unhandled":unhandled.lock().unwrap().clone()})),
   "resume"=>{let wanted=step["bytes"].as_u64().unwrap() as usize;timeout(Duration::from_secs(5),async{while received.len()<wanted {let mut buf=vec![0;65536];let n=client.as_mut().unwrap().read(&mut buf).await.unwrap();assert!(n>0,"Premature client EOF");received.extend_from_slice(&buf[..n]);}stream.flush().await.unwrap();}).await.unwrap();}
   "drop-observer"=>observer=None,
   "terminate"=>socket.terminate().unwrap(),
   "client-end"=>{client.as_mut().unwrap().shutdown().await.unwrap();let mut buffer=[0;1];assert_eq!(stream.read(&mut buffer).await.unwrap(),0);}
   "client-reset"=>{let reset=client.take().unwrap();reset.set_zero_linger().unwrap();drop(reset);std::thread::sleep(Duration::from_millis(25));}
   "settle-close"=>{timeout(Duration::from_secs(5),async{if let Some(client)=client.as_mut(){let mut bytes=Vec::new();client.read_to_end(&mut bytes).await.unwrap();received.extend(bytes);}client_closed=true;while *closed.lock().unwrap()!=1 {tokio::task::yield_now().await;}}).await.unwrap();}
   _=>panic!("Unknown socket step"),
  }
    }
    assert!(client_closed);
    let _ = observer;
    if let Ok(root) = std::env::var("IVY_HIVE_WS_WIRES") {
        fs::write(
            Path::new(&root).join(format!("{}-native.bin", input["name"].as_str().unwrap())),
            &received,
        )
        .unwrap();
    }
    json!({"name":input["name"],"snapshots":snapshots,"wire":{"byteLength":received.len(),"sha256":digest(&received)},"closed":*closed.lock().unwrap(),"unhandled":unhandled.lock().unwrap().clone()})
}
#[test]
fn actual_tcp_send_buffers_flush_and_close_match_original_ws() {
    let cases = traces();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let node = std::env::var("IVY_TEST_NODE").unwrap_or_else(|_| "node".into());
    let mut child = Command::new(node)
        .arg(root.join("tests/fixtures/hive-ws-socket-oracle.mjs"))
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(&cases).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "Original WS oracle: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let original: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(original.len(), cases.len());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut mismatches = Vec::new();
    for (input, original) in cases.into_iter().zip(original) {
        let actual = runtime.block_on(native(&input));
        let matched = original == actual;
        if let Ok(path) = std::env::var("IVY_HIVE_WS_SOCKET_CORPUS") {
            writeln!(OpenOptions::new().create(true).append(true).open(path).unwrap(),"{}",json!({"input":input,"original":original,"native":actual,"execution":"completed","finding":{"conditionMet":matched}})).unwrap();
        }
        if !matched {
            mismatches.push(json!({"name":input["name"],"original":original,"native":actual}));
        }
    }
    assert!(
        mismatches.is_empty(),
        "Actual WS socket I/O differences: {}",
        serde_json::to_string_pretty(&mismatches).unwrap()
    );
}

#[test]
fn actual_close_releases_connection_but_retains_accepted_peer_work() {
    use ivy_hive::{
        admission::{DispatchGate, RequestBudget, RequestBudgetOptions},
        live_routing::LiveRouting,
        server_peers::{self, Peers},
        server_rpc::{Reply, Storage},
        tool_binding_diagnostics::ToolBindingDiagnostics,
        worker_client::RequestOptions,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::oneshot;
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
        control: Mutex<Option<oneshot::Sender<ivy_contracts::IvyResult<IvyJson>>>>,
        complete: Arc<AtomicBool>,
        cancelled: Arc<AtomicBool>,
    }
    impl Storage for Worker {
        fn request(&self, payload: IvyJson, _options: RequestOptions) -> Reply {
            assert_eq!(payload.get("action"), Some(&"execute".into()));
            let (send, receive) = oneshot::channel();
            *self.control.lock().unwrap() = Some(send);
            let complete = self.complete.clone();
            let guard = Pending {
                complete: complete.clone(),
                cancelled: self.cancelled.clone(),
            };
            Box::pin(async move {
                let _guard = guard;
                let result = receive.await.unwrap();
                complete.store(true, Ordering::SeqCst);
                result
            })
        }
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let worker=Arc::new(Worker{control:Mutex::new(None),complete:Arc::new(AtomicBool::new(false)),cancelled:Arc::new(AtomicBool::new(false))});
        let requests=RequestBudget::new(RequestBudgetOptions{maximum:1,per_consumer:1,..Default::default()});
        let connections=RequestBudget::new(RequestBudgetOptions{maximum:1,per_consumer:1,..Default::default()});
        let mut connection=connections.acquire(&"client".into(),None).unwrap();
        let unhandled=Arc::new(Mutex::new(Vec::new()));
        let peers = Peers::new(server_peers::Owners {
            storage: worker.clone(),
            routing: Arc::new(Mutex::new(LiveRouting::new(false, None))),
            diagnostics: Arc::new(Mutex::new(ToolBindingDiagnostics::new(
                Box::new(|_| Ok(())),
                Arc::new(|| 10000.0),
                Arc::new(super::TraceRuntime(
                    obj(&json!({"iso":[[10000.0,"1970-01-01T00:00:10.000Z"]],"dates":[]})),
                    Arc::new(Mutex::new(vec![])),
                )),
            ))),
            requests: requests.clone(),
            dispatch: DispatchGate::default(),
            direct: Arc::new(|_, _, _, _| Ok(None)),
            now: Arc::new(|| 10000.0),
            ids: Arc::new(|| "fixture-id".into()),
            provider_error: Arc::new(|_| panic!("Provider-error carrier is outside this integration case")),
            wake_browser_push: Arc::new(|| {}),
            clear_authentication: Arc::new(|| {}),
            unhandled: {
                let log = unhandled.clone();
                Arc::new(move |_, error| log.lock().unwrap().push(error))
            },
        }, Default::default());
        let listener=TcpListener::bind(("127.0.0.1",0)).await.unwrap();let port=listener.local_addr().unwrap().port();let (server,client)=tokio::join!(listener.accept(),TcpStream::connect(("127.0.0.1",port)));let (server,_)=server.unwrap();let mut client=client.unwrap();
        let handles=Arc::new(Mutex::new(None));let closed=Arc::new(AtomicBool::new(false));
        let _stream=ws_socket_io::attach(server,{let peers=peers.clone();let handles=handles.clone();let closed=closed.clone();let log=unhandled.clone();move|socket|{let peer=peers.connected(Box::new(socket.clone()),obj(&json!({"principalId":"fixture","credentialDigest":"credential","transport":"ws"})));*handles.lock().unwrap()=Some((socket,peer.clone()));ws_socket_io::Owners{closed:Box::new(move||{connection.release();peers.closed(&peer)?;closed.store(true,Ordering::SeqCst);Ok(())}),unhandled:Arc::new(move|_,error|log.lock().unwrap().push(error))}}}).await.unwrap();
        let (mut socket,peer)=handles.lock().unwrap().take().unwrap();
        peers.message(&peer,b"{\"jsonrpc\":\"2.0\",\"id\":\"pending\",\"method\":\"fixture.wait\",\"params\":{}}",false).unwrap();
        timeout(Duration::from_secs(5),async{while worker.control.lock().unwrap().is_none(){tokio::task::yield_now().await;}}).await.unwrap();
        assert!(connections.acquire(&"client".into(),None).is_err());assert!(requests.acquire(&"credential".into(),None).is_err());
        socket.terminate().unwrap();drop(socket);drop(peer);
        timeout(Duration::from_secs(5),async{while !closed.load(Ordering::SeqCst){tokio::task::yield_now().await;}}).await.unwrap();
        assert!(connections.acquire(&"client".into(),None).is_ok());
        assert!(requests.acquire(&"credential".into(),None).is_err());assert!(!worker.complete.load(Ordering::SeqCst));assert!(!worker.cancelled.load(Ordering::SeqCst));
        worker.control.lock().unwrap().take().unwrap().send(Ok(obj(&json!({"kind":"result","value":{"ok":true}})))).unwrap();
        timeout(Duration::from_secs(5),async{loop{if let Ok(mut lease)=requests.acquire(&"credential".into(),None){lease.release();break;}tokio::task::yield_now().await;}}).await.unwrap();
        assert!(worker.complete.load(Ordering::SeqCst));assert!(!worker.cancelled.load(Ordering::SeqCst));assert!(unhandled.lock().unwrap().is_empty());
        let mut received=Vec::new();timeout(Duration::from_secs(5),client.read_to_end(&mut received)).await.unwrap().unwrap();assert!(received.is_empty());
    });
}
