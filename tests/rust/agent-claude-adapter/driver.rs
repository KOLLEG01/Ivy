use super::*;
use futures_util::FutureExt;
use ivy_agent_manager::claude::{
    bridge::{Bridge, BridgeLaunch, SdkReply},
    driver::{self, Bindings, Handle},
    protocol::{Adapter, AdapterClock, Environment},
    sdk_options::SdkEnvironment,
    store::SessionStore,
};
use std::collections::BTreeMap;
use tokio::{
    sync::{mpsc, oneshot},
    time::{Duration, timeout},
};

pub(super) fn adapter(home: &Path) -> Adapter {
    let clock: ivy_agent_manager::claude::store::Clock = Arc::new(|| 1700000000.0);
    let millis: ivy_agent_manager::claude::store::Clock = Arc::new(|| 1700000000123.0);
    let next = Arc::new(Mutex::new(0usize));
    let ids: Arc<dyn Fn() -> JsString + Send + Sync> = Arc::new(move || {
        let mut n = next.lock().unwrap();
        let id = format!("runtime-{}", *n);
        *n += 1;
        id.into()
    });
    let compare = project_comparator().unwrap();
    let projects =
        ProjectStore::open(home, Style::native(), compare, clock.clone(), ids.clone()).unwrap();
    let store = SessionStore::with_clock(home, &home.join("state.sqlite"), clock.clone()).unwrap();
    Adapter::new(
        store,
        projects,
        RateLimitStore::open(home),
        Environment {
            environment: J::Null,
            discovered_router_models: vec![],
            cwd: node_path::from_native(home),
            codex_home: node_path::from_native(home),
            platform_os: "windows".into(),
            cpu: "x86_64".into(),
            compat_version: "0.142.3".into(),
            version_suffix: "claude-codex".into(),
            default_model: "sonnet".into(),
            default_effort: "medium".into(),
            mcp_servers: J::Null,
            startup_statuses: vec![],
            config_overrides: v("{}"),
            config_path: home.join("config.json"),
            prepare_worktree: Arc::new(|_, cwd| Ok(cwd.clone())),
            read_base64: Arc::new(|_, _| Err(AdapterError::new("Unexpected file read"))),
            subagent_completed: Some(true),
            subagent_watchdog_ms: Some(1_000),
        },
        AdapterClock {
            seconds: clock,
            millis,
        },
        ids,
    )
}

#[tokio::test]
async fn production_file_reads_fit_the_original_native_response() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use ivy_agent_manager::claude::runtime::{self, Launch};
    use ivy_contracts::{json::stringify_size, limits::NATIVE_FRAME_BYTES};
    use ivy_host_runtime::codex_rpc::{Events, NativeFrame, NativeRpc, RequestOptions};

    let root = Root::new();
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let node = Command::new("node")
        .args([
            "--input-type=module",
            "-e",
            "process.stdout.write(process.execPath)",
        ])
        .output()
        .unwrap();
    assert!(node.status.success());
    let (sent, mut replies) = mpsc::unbounded_channel();
    let runtime = runtime::start(
        Launch {
            home: root.0.clone(),
            codex_home: root.0.join("codex-home"),
            account_home: root.0.clone(),
            compat_version: "0.142.3".into(),
            default_model: "sonnet".into(),
            project_compare: project_comparator().unwrap(),
            sdk: BridgeLaunch {
                node_executable: PathBuf::from(String::from_utf8(node.stdout).unwrap()),
                entry: repository.join("tests/fixtures/agent-claude-bridge-transport.mjs"),
                cwd: repository,
                job_launcher: Some(
                    PathBuf::from(env!("CARGO_BIN_EXE_ivy-agent-manager"))
                        .parent()
                        .unwrap()
                        .join(if cfg!(windows) {
                            "ivy-job.exe"
                        } else {
                            "ivy-job"
                        }),
                ),
                environment: std::env::vars_os()
                    .filter(|(key, _)| {
                        key.to_str().is_some_and(|key| {
                            matches!(
                                key.to_ascii_lowercase().as_str(),
                                "systemroot" | "windir" | "temp" | "tmp"
                            )
                        })
                    })
                    .collect(),
            },
            mcp_servers_path: None,
            auto_worktree: false,
            worktree_root: None,
            subagent_completed: None,
            subagent_watchdog_ms: None,
        },
        Arc::new(move |peer, message| {
            assert_eq!(peer, JsString::from("file-owner"));
            sent.send(message)
                .map_err(|_| AdapterError::new("File reply owner closed"))
        }),
    )
    .await
    .unwrap();
    let scenario = std::panic::AssertUnwindSafe(async {
        let path = root.0.join("sample-ä.bin");
        for length in [0, 1, 2, 3, 256, 12_289] {
            let bytes: Vec<u8> = (0..length).map(|i| i as u8).collect();
            fs::write(&path, &bytes).unwrap();
            request_file(&runtime.handle, "small".into(), "path", &path).await;
            let reply = timeout(Duration::from_secs(10), replies.recv()).await.unwrap().unwrap();
            assert_eq!(f(&reply, "id"), &J::from("small"));
            assert_eq!(f(f(&reply, "result"), "dataBase64"), &J::from(STANDARD.encode(bytes)));
        }
        for path in [root.0.join("missing"), root.0.clone()] {
            request_file(&runtime.handle, "io-error".into(), "filePath", &path).await;
            let reply = timeout(Duration::from_secs(10), replies.recv()).await.unwrap().unwrap();
            assert_eq!(f(&reply, "id"), &J::from("io-error"));
            assert_eq!(f(f(&reply, "error"), "code"), &J::Number(-32000.0));
        }
        // The original escaped UTF-16 ID contributes to the response budget.
        for id in [JsString::from("plain"),
            JsString(vec![0xd800, b'"' as u16, b'\\' as u16, 0].repeat(64))] {
        let empty = J::object([
            ("jsonrpc", "2.0".into()), ("id", id.clone().into()),
            ("result", J::object([("dataBase64", "".into())])),
        ]);
        let available = (NATIVE_FRAME_BYTES - stringify_size(&empty, NATIVE_FRAME_BYTES).unwrap())
            .min(NativeFrame::MAXIMUM_VALUE_BYTES.saturating_sub(empty.retained_bytes()) / 2);
        let maximum_bytes = available / 4 * 3;
        let file = fs::File::create(&path).unwrap();
        file.set_len(maximum_bytes as u64 + 1).unwrap();
        drop(file);
        request_file(&runtime.handle, id.clone(), "filePath", &path).await;
        let reply = timeout(Duration::from_secs(30), replies.recv()).await.unwrap().unwrap();
        assert_eq!(f(&reply, "id"), &J::from(id.clone()));
        assert!(reply.get("error").is_some(),
            "The production reader materialized a file that cannot fit its original native response");
        assert!(stringify_size(&reply, NATIVE_FRAME_BYTES).is_ok());
        drop(reply);

        fs::OpenOptions::new().write(true).open(&path).unwrap()
            .set_len(maximum_bytes as u64).unwrap();
        request_file(&runtime.handle, id.clone(), "path", &path).await;
        let reply = timeout(Duration::from_secs(30), replies.recv()).await.unwrap().unwrap();
        assert_eq!(f(&reply, "id"), &J::from(id.clone()));
        let data = s(f(f(&reply, "result"), "dataBase64"));
        assert_eq!(data.0.len(), maximum_bytes / 3 * 4);
        assert!(data.0.iter().all(|unit| *unit == b'A' as u16));
        assert!(stringify_size(&reply, NATIVE_FRAME_BYTES).is_ok());
        // Exercise the real retained-frame reservation and original reply
        // receiver as well as the serialized size; no vendor process is used.
        let (rpc, mut outgoing, incoming) = NativeRpc::start_frames(Arc::new(v(include_str!(
            "../../../specs/native/codex-0.142.3/catalog.json"
        ))), Events::default()).unwrap();
        let caller = rpc.clone();
        let params = J::object([("path", node_path::from_native(&path).into())]);
        let original = tokio::spawn(async move {
            caller.request("fs/readFile".into(), params, RequestOptions {
                request_id: Some(id), ..Default::default()
            }).await
        });
        drop(timeout(Duration::from_secs(10), outgoing.recv()).await.unwrap().unwrap());
        incoming.try_send(reply).expect("The admitted file response exceeded the native value budget");
        let result = timeout(Duration::from_secs(10), original).await.unwrap().unwrap().unwrap();
        assert_eq!(s(f(f(&result, "result"), "dataBase64")).0.len(), maximum_bytes / 3 * 4);
        drop(result);
        rpc.close("test-complete").await;
        }
        assert!(replies.try_recv().is_err());
    }).catch_unwind().await;
    timeout(Duration::from_secs(10), runtime.handle.stop())
        .await
        .unwrap();
    timeout(Duration::from_secs(10), runtime.owner)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        ivy_host_runtime::process::owned_process_tree_stopped(&runtime.process)
            .await
            .unwrap()
    );
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
}

async fn request_file(handle: &Handle, id: JsString, key: &str, path: &Path) {
    timeout(
        Duration::from_secs(30),
        handle.receive(
            "file-owner".into(),
            J::object([
                ("id", id.into()),
                ("method", "fs/readFile".into()),
                (
                    "params",
                    J::object([(key, node_path::from_native(path).into())]),
                ),
            ]),
        ),
    )
    .await
    .unwrap()
    .unwrap();
}

struct Trace {
    input: mpsc::UnboundedReceiver<J>,
    messages: Vec<J>,
}
impl Trace {
    async fn until(&mut self, predicate: impl Fn(&J) -> bool) -> J {
        timeout(Duration::from_secs(10), async {
            loop {
                let value = self.input.recv().await.expect("Native sender closed");
                let found = predicate(&value);
                self.messages.push(value.clone());
                if found {
                    return value;
                }
            }
        })
        .await
        .expect("Native message did not arrive")
    }
    async fn response(&mut self, id: &str) -> J {
        self.until(|v| f(f(v, "message"), "id") == &J::from(id))
            .await
            .get("message")
            .unwrap()
            .clone()
    }
    fn drain(&mut self) {
        while let Ok(value) = self.input.try_recv() {
            self.messages.push(value)
        }
    }
}
async fn request(handle: &Handle, id: &str, method: &str, params: J) {
    timeout(
        Duration::from_secs(10),
        handle.receive(
            "peer".into(),
            J::object([
                ("id", id.into()),
                ("method", method.into()),
                ("params", params),
            ]),
        ),
    )
    .await
    .unwrap()
    .unwrap()
}
async fn call(bridge: &Bridge, method: &str, params: J) -> SdkReply {
    timeout(Duration::from_secs(10), bridge.call(method, params))
        .await
        .unwrap()
        .unwrap()
}
async fn start(handle: &Handle, bridge: &Bridge, trace: &mut Trace, thread: &J, label: &str) -> J {
    request(
        handle,
        label,
        "turn/start",
        J::object([
            ("threadId", thread.clone()),
            ("input", v(r#"[{"type":"text","text":"fixture turn"}]"#)),
        ]),
    )
    .await;
    let response = trace.response(label).await;
    let turn = f(f(&response, "result"), "turn").get("id").unwrap().clone();
    call(
        bridge,
        "fixture/awaitTurn",
        J::object([("turnId", turn.clone())]),
    )
    .await;
    turn
}
async fn emit(bridge: &Bridge, turn: &J, message: J) {
    call(
        bridge,
        "fixture/emit",
        J::object([("turnId", turn.clone()), ("message", message)]),
    )
    .await;
}
async fn completed(trace: &mut Trace, turn: &J) -> J {
    trace
        .until(|v| {
            is_method(v, "turn/completed")
                && f(f(f(v, "message"), "params"), "turn").get("id") == Some(turn)
        })
        .await
}
fn is_method(value: &J, method: &str) -> bool {
    f(f(value, "message"), "method") == &J::from(method)
}
#[tokio::test]
async fn sdk_cancellation_resolves_original_native_requests_once_without_granting_late_answers() {
    let root = Root::new();
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let node = Command::new("node")
        .args([
            "--input-type=module",
            "-e",
            "process.stdout.write(process.execPath)",
        ])
        .output()
        .unwrap();
    assert!(node.status.success());
    let (bridge, events) = Bridge::spawn(
        BridgeLaunch {
            node_executable: PathBuf::from(String::from_utf8(node.stdout).unwrap()),
            entry: repository.join("tests/fixtures/agent-claude-bridge-transport.mjs"),
            cwd: repository,
            job_launcher: Some(
                PathBuf::from(env!("CARGO_BIN_EXE_ivy-agent-manager"))
                    .parent()
                    .unwrap()
                    .join(if cfg!(windows) {
                        "ivy-job.exe"
                    } else {
                        "ivy-job"
                    }),
            ),
            environment: std::env::vars_os()
                .filter(|(k, _)| {
                    k.to_str().is_some_and(|s| {
                        matches!(
                            s.to_ascii_lowercase().as_str(),
                            "systemroot" | "windir" | "temp" | "tmp"
                        )
                    })
                })
                .collect(),
        },
        Arc::new(|_| {}),
    )
    .await
    .unwrap();
    let (native_tx, native_rx) = mpsc::unbounded_channel();
    let (handle, mut owner) = driver::spawn(
        adapter(&root.0),
        bridge.clone(),
        events,
        Bindings {
            sdk: SdkEnvironment {
                home: node_path::from_native(&root.0),
                environment: v(r#"{"CLAUDE_CODEX_CLI":"fixture-only-not-executed"}"#),
            },
            workflow_files: super::event_cases::files(),
            send: Arc::new(move |peer, message| {
                native_tx
                    .send(J::object([("peer", J::String(peer)), ("message", message)]))
                    .map_err(|_| AdapterError::new("Fixture native owner closed"))
            }),
            git_diff: Arc::new(|_| Box::pin(async { Ok(JsString::default()) })),
        },
    );
    let mut trace = Trace {
        input: native_rx,
        messages: vec![],
    };
    let scenario = std::panic::AssertUnwindSafe(async {
        request(&handle, "thread", "thread/start", v(r#"{"approvalPolicy":"on-request","sandbox":"workspace-write"}"#)).await;
        let thread = f(f(f(&trace.response("thread").await, "result"), "thread"), "id").clone();
        let turn = start(&handle, &bridge, &mut trace, &thread, "cancelled").await;
        let permission_bridge = bridge.clone();
        let permission_turn = turn.clone();
        let permission = tokio::spawn(async move { call(&permission_bridge, "fixture/permission", J::object([
            ("turnId", permission_turn), ("input", v(r#"{"command":"cancelled command\ud800"}"#)), ("toolUseId", "cancelled-bash".into()),
        ])).await });
        let approval = trace.until(|v| is_method(v, "item/commandExecution/requestApproval")).await;
        let approval_id = f(f(&approval, "message"), "id").clone();
        let ask_bridge = bridge.clone();
        let ask_turn = turn.clone();
        let ask = tokio::spawn(async move { call(&ask_bridge, "fixture/permission", J::object([
            ("turnId", ask_turn), ("toolName", "AskUserQuestion".into()), ("toolUseId", "cancelled-question".into()),
            ("input", v(r#"{"questions":[{"question":"Which?\ud800","options":[{"label":"A"}]}]}"#)),
        ])).await });
        let question = trace.until(|v| is_method(v, "item/tool/requestUserInput")).await;
        let question_id = f(f(&question, "message"), "id").clone();
        request(&handle, "interrupt", "turn/interrupt", J::object([("threadId", thread.clone())])).await;
        trace.response("interrupt").await;
        for id in [&approval_id, &question_id] {
            let resolved = |v: &J| is_method(v, "serverRequest/resolved") && f(f(f(v, "message"), "params"), "requestId") == id;
            if !trace.messages.iter().any(resolved) { trace.until(resolved).await; }
        }
        for callback in [permission, ask] {
            let result = timeout(Duration::from_secs(10), callback).await.unwrap().unwrap();
            assert_eq!(&*result, &v(r#"{"behavior":"deny","message":"turn already finished"}"#));
        }
        for (id, answer) in [(approval_id.clone(), v(r#"{"decision":"acceptForSession"}"#)), (question_id.clone(), v(r#"{"answers":{"q0":{"answers":["A"]}}}"#))] {
            handle.receive("peer".into(), J::object([("id", id), ("result", answer)])).await.unwrap();
        }
        let next = start(&handle, &bridge, &mut trace, &thread, "next").await;
        let next_bridge = bridge.clone();
        let next_turn = next.clone();
        let next_permission = tokio::spawn(async move { call(&next_bridge, "fixture/permission", J::object([
            ("turnId", next_turn), ("input", v(r#"{"command":"cancelled command\ud800"}"#)), ("toolUseId", "fresh-bash".into()),
        ])).await });
        let approval = trace.until(|v| is_method(v, "item/commandExecution/requestApproval")).await;
        let next_id = f(f(&approval, "message"), "id").clone();
        assert_ne!(next_id, approval_id);
        assert_ne!(next_id, question_id);
        handle.receive("peer".into(), J::object([("id", next_id.clone()), ("result", v(r#"{"decision":"acceptForSession"}"#))])).await.unwrap();
        let result = timeout(Duration::from_secs(10), next_permission).await.unwrap().unwrap();
        assert_eq!(&*result, &v(r#"{"behavior":"allow","updatedInput":{"command":"cancelled command\ud800"}}"#));
        emit(&bridge, &next, v(r#"{"type":"result","subtype":"success","result":"original next turn"}"#)).await;
        completed(&mut trace, &next).await;
        trace.drain();
        for id in [approval_id, question_id, next_id] {
            assert_eq!(trace.messages.iter().filter(|v| is_method(v, "serverRequest/resolved") && f(f(f(v, "message"), "params"), "requestId") == &id).count(), 1);
        }
        let stored = SessionStore::with_clock(&root.0, &root.0.join("state.sqlite"), Arc::new(|| 1700000000.0)).unwrap();
        assert_eq!(f(&stored.get_turn(s(&turn)).unwrap().unwrap(), "status"), &J::from("interrupted"));
    }).catch_unwind().await;
    let stopped = timeout(Duration::from_secs(10), handle.stop()).await;
    if stopped.is_err() {
        owner.abort();
    }
    let joined = timeout(Duration::from_secs(10), &mut owner).await;
    if joined.is_err() {
        owner.abort();
        let _ = owner.await;
    }
    let closed = bridge.stop().await;
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
    stopped.unwrap();
    joined.unwrap().unwrap().unwrap();
    closed.unwrap();
}
#[tokio::test]
async fn cancelled_frame_waiter_keeps_original_native_dispatch_and_identity() {
    use ivy_host_runtime::codex_rpc::{Events, NativeRpc, RequestOptions};
    use std::{
        future::{Future, poll_fn},
        task::Poll,
    };

    let root = Root::new();
    let (bridge, events) =
        super::command_cases::transport_entry("tests/fixtures/agent-claude-bridge-transport.mjs")
            .await;
    let catalog = Arc::new(v(include_str!(
        "../../../specs/native/codex-0.154.0/catalog.json"
    )));
    let (rpc, mut outgoing, incoming) =
        NativeRpc::start_frames(catalog, Events::default()).unwrap();
    let ids = Arc::new(Mutex::new(Vec::new()));
    let seen = ids.clone();
    let (handle, owner) = driver::spawn(
        adapter(&root.0),
        bridge.clone(),
        events,
        Bindings {
            sdk: SdkEnvironment {
                home: node_path::from_native(&root.0),
                environment: v(r#"{"CLAUDE_CODEX_CLI":"fixture-only-not-executed"}"#),
            },
            workflow_files: super::event_cases::files(),
            send: Arc::new(move |_, message| {
                if let Some(id) = message.get("id") {
                    seen.lock().unwrap().push(id.clone());
                }
                incoming
                    .try_send(message)
                    .map_err(|e| AdapterError::new(e.message_js()))
            }),
            git_diff: Arc::new(|_| Box::pin(async { Ok("".into()) })),
        },
    );
    let scenario = std::panic::AssertUnwindSafe(async {
        let mut units = Vec::with_capacity(4 * 1024 * 1024);
        units.extend([65, 0xd800]);
        let client = rpc.clone();
        let original = tokio::spawn(async move {
            client
                .request(
                    "initialize".into(),
                    J::object([
                        (
                            "clientInfo",
                            v(r#"{"name":"original-client","version":"fixture"}"#),
                        ),
                        ("retained", JsString(units).into()),
                    ]),
                    RequestOptions {
                        request_id: Some("original-native-init".into()),
                        ..Default::default()
                    },
                )
                .await
        });
        let frame = timeout(Duration::from_secs(10), outgoing.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(f(&frame, "id"), &J::from("original-native-init"));
        assert_eq!(
            s(f(f(&frame, "params"), "retained")).0.capacity(),
            4 * 1024 * 1024
        );
        let mut waiter = Box::pin(handle.receive_frame("manager".into(), frame));
        poll_fn(|cx| match waiter.as_mut().poll(cx) {
            Poll::Pending => Poll::Ready(()),
            Poll::Ready(result) => panic!("Native frame did not enter its owner queue: {result:?}"),
        })
        .await;
        drop(waiter);
        let response = timeout(Duration::from_secs(10), original)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            f(f(&response, "result"), "userAgent")
                .as_str()
                .unwrap()
                .contains("original-client")
        );
        assert_eq!(
            ids.lock().unwrap().as_slice(),
            &[J::from("original-native-init")]
        );
    })
    .catch_unwind()
    .await;
    handle.stop().await;
    owner.await.unwrap().unwrap();
    rpc.close("test-complete").await;
    bridge.stop().await.unwrap();
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
    assert!(
        ivy_host_runtime::process::owned_process_tree_stopped(&bridge.owned_process())
            .await
            .unwrap()
    );
}
#[tokio::test]
async fn async_owner_keeps_native_controls_live_through_approvals_diff_and_backpressure() {
    let root = Root::new();
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let node = Command::new("node")
        .args([
            "--input-type=module",
            "-e",
            "process.stdout.write(process.execPath)",
        ])
        .output()
        .unwrap();
    assert!(node.status.success());
    let (bridge, events) = Bridge::spawn(
        BridgeLaunch {
            node_executable: PathBuf::from(String::from_utf8(node.stdout).unwrap()),
            entry: repository.join("tests/fixtures/agent-claude-bridge-transport.mjs"),
            cwd: repository,
            job_launcher: Some(
                PathBuf::from(env!("CARGO_BIN_EXE_ivy-agent-manager"))
                    .parent()
                    .unwrap()
                    .join(if cfg!(windows) {
                        "ivy-job.exe"
                    } else {
                        "ivy-job"
                    }),
            ),
            environment: std::env::vars_os()
                .filter(|(k, _)| {
                    k.to_str().is_some_and(|s| {
                        matches!(
                            s.to_ascii_lowercase().as_str(),
                            "systemroot" | "windir" | "temp" | "tmp"
                        )
                    })
                })
                .collect::<BTreeMap<_, _>>(),
        },
        Arc::new(|_| {}),
    )
    .await
    .unwrap();
    let (native_tx, native_rx) = mpsc::unbounded_channel();
    let (diff_tx, mut diff_rx) = mpsc::unbounded_channel::<oneshot::Sender<JsString>>();
    let bindings = Bindings {
        sdk: SdkEnvironment {
            home: node_path::from_native(&root.0),
            environment: v(r#"{"CLAUDE_CODEX_CLI":"fixture-only-not-executed"}"#),
        },
        workflow_files: super::event_cases::files(),
        send: Arc::new(move |peer, message| {
            native_tx
                .send(J::object([("peer", J::String(peer)), ("message", message)]))
                .map_err(|_| AdapterError::new("Fixture native owner closed"))
        }),
        git_diff: Arc::new(move |_| {
            let sender = diff_tx.clone();
            Box::pin(async move {
                let (tx, rx) = oneshot::channel();
                sender
                    .send(tx)
                    .map_err(|_| AdapterError::new("Fixture diff owner closed"))?;
                rx.await
                    .map_err(|_| AdapterError::new("Fixture diff cancelled"))
            })
        }),
    };
    let (handle, mut owner) = driver::spawn(adapter(&root.0), bridge.clone(), events, bindings);
    let mut trace = Trace {
        input: native_rx,
        messages: vec![],
    };
    let scenario = std::panic::AssertUnwindSafe(async {
    request(
        &handle,
        "thread",
        "thread/start",
        J::object([
            ("cwd", J::String(node_path::from_native(&root.0))),
            ("model", "sonnet".into()),
            ("approvalPolicy", "on-request".into()),
            ("sandbox", "workspace-write".into()),
        ]),
    )
    .await;
    let response = trace.response("thread").await;
    let thread = f(f(f(&response, "result"), "thread"), "id").clone();
    assert!(!thread.is_null());
    let turn = start(&handle, &bridge, &mut trace, &thread, "first").await;
    emit(&bridge,&turn,v(r#"{"type":"assistant","message":{"id":"first","content":[{"type":"text","text":"complete after diff\ud800"}]}}"#)).await;
    emit(&bridge,&turn,v(r#"{"type":"result","subtype":"success","result":"done","usage":{"input_tokens":3},"duration_api_ms":2}"#)).await;
    let diff = timeout(Duration::from_secs(10), diff_rx.recv())
        .await
        .unwrap()
        .unwrap();
    request(&handle, "during-diff", "model/list", v("{}")).await;
    let response = trace.response("during-diff").await;
    assert_eq!(
        f(f(&response, "result"), "data").as_array().unwrap().len(),
        1
    );
    trace.drain();
    assert!(
        !trace
            .messages
            .iter()
            .any(|v| is_method(v, "turn/completed"))
    );
    diff.send("qualified diff".into()).unwrap();
    let done = completed(&mut trace, &turn).await;
    assert_eq!(
        f(f(f(f(&done, "message"), "params"), "turn"), "status"),
        &J::from("completed")
    );
    let diff_at = trace
        .messages
        .iter()
        .position(|v| is_method(v, "turn/diff/updated"))
        .unwrap();
    let finish_at = trace
        .messages
        .iter()
        .position(|v| is_method(v, "turn/completed"))
        .unwrap();
    assert!(diff_at < finish_at);

    let turn2 = start(&handle, &bridge, &mut trace, &thread, "second").await;
    let permission_bridge = bridge.clone();
    let permission_turn = turn2.clone();
    let permission = tokio::spawn(async move {
        call(
            &permission_bridge,
            "fixture/permission",
            J::object([("turnId", permission_turn)]),
        )
        .await
    });
    let approval = trace
        .until(|v| is_method(v, "item/commandExecution/requestApproval"))
        .await;
    let request_id = f(f(&approval, "message"), "id").clone();
    request(&handle, "during-approval", "model/list", v("{}")).await;
    trace.response("during-approval").await;
    handle
        .receive(
            "peer".into(),
            J::object([
                ("id", request_id),
                ("result", v(r#"{"decision":"acceptForSession"}"#)),
            ]),
        )
        .await
        .unwrap();
    let accepted = timeout(Duration::from_secs(10), permission)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(f(&accepted, "behavior"), &J::from("allow"));
    // A rejected native approval must reject the SDK callback promise; AskUser
    // Question uses its separate deny-with-answer/error conversion.
    let denied_bridge = bridge.clone();
    let denied_turn = turn2.clone();
    let rejected = tokio::spawn(async move {
        timeout(
            Duration::from_secs(10),
            denied_bridge.call(
                "fixture/permission",
                J::object([
                    ("turnId", denied_turn),
                    ("input", v(r#"{"command":"another fixture"}"#)),
                    ("toolUseId", "reject".into()),
                ]),
            ),
        )
        .await
        .unwrap()
    });
    let approval = trace
        .until(|v| is_method(v, "item/commandExecution/requestApproval"))
        .await;
    let request_id = f(f(&approval, "message"), "id").clone();
    handle
        .receive(
            "peer".into(),
            J::object([
                ("id", request_id),
                (
                    "error",
                    v(r#"{"code":-32000,"message":"fixture native rejection"}"#),
                ),
            ]),
        )
        .await
        .unwrap();
    let error = timeout(Duration::from_secs(10), rejected)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.message, JsString::from("fixture native rejection"));
    emit(
        &bridge,
        &turn2,
        v(r#"{"type":"result","subtype":"success","result":"second done"}"#),
    )
    .await;
    timeout(Duration::from_secs(10), diff_rx.recv())
        .await
        .unwrap()
        .unwrap()
        .send(JsString::default())
        .unwrap();
    completed(&mut trace, &turn2).await;

    let turn3 = start(&handle, &bridge, &mut trace, &thread, "third").await;
    emit(
        &bridge,
        &turn3,
        v(r#"{"type":"result","subtype":"success","result":"racing diff"}"#),
    )
    .await;
    let held = timeout(Duration::from_secs(10), diff_rx.recv())
        .await
        .unwrap()
        .unwrap();
    request(
        &handle,
        "interrupt",
        "turn/interrupt",
        J::object([("threadId", thread.clone()), ("turnId", turn3.clone())]),
    )
    .await;
    let done = completed(&mut trace, &turn3).await;
    assert_eq!(
        f(f(f(f(&done, "message"), "params"), "turn"), "status"),
        &J::from("interrupted")
    );
    trace.response("interrupt").await;
    held.send("stale final diff".into()).unwrap();
    // The next native round-trip is also independent of that stale completion.
    request(
        &handle,
        "after-interrupt",
        "thread/read",
        J::object([("threadId", thread.clone())]),
    )
    .await;
    trace.response("after-interrupt").await;

    let inspection=call(&bridge,"fixture/inspect",v("{}")).await;
    let interrupted_before:f64=inspection.as_array().unwrap().iter().map(|entry|f(entry,"interrupts").as_f64().unwrap()).sum();
    let workflow = start(&handle, &bridge, &mut trace, &thread, "workflow-watchdog").await;
    emit(&bridge,&workflow,v(r#"{"type":"assistant","message":{"id":"pending-workflow","content":[{"type":"tool_use","id":"pending-workflow","name":"Workflow","input":{}}]}}"#)).await;
    let done=completed(&mut trace,&workflow).await;
    let failed=f(f(f(&done,"message"),"params"),"turn");
    assert_eq!(f(failed,"status"),&J::from("failed"));
    assert_eq!(f(f(failed,"error"),"message"),&J::from("Subagent did not publish a terminal result within 1 second."));
    timeout(Duration::from_secs(3),async {
        loop {
            let inspection=call(&bridge,"fixture/inspect",v("{}")).await;
            let interrupted:f64=inspection.as_array().unwrap().iter().map(|entry|f(entry,"interrupts").as_f64().unwrap()).sum();
            if interrupted>interrupted_before {break}
            tokio::task::yield_now().await;
        }
    }).await.expect("watchdog did not independently interrupt the SDK pipe");
    let turn4 = start(&handle, &bridge, &mut trace, &thread, "backpressure").await;
    emit(&bridge,&turn4,v(r#"{"type":"assistant","message":{"id":"tool","content":[{"type":"tool_use","id":"backpressure-tool","name":"Bash","input":{"command":"fixture"}}]}}"#)).await;
    emit(&bridge,&turn4,v(r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"backpressure-tool","content":"fixture result"}]}}"#)).await;
    let outstanding = timeout(Duration::from_secs(10), diff_rx.recv())
        .await
        .unwrap()
        .unwrap();
    call(
        &bridge,
        "fixture/burst",
        J::object([("turnId", turn4.clone()), ("count", J::Number(512.0))]),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    timeout(Duration::from_secs(10), handle.stop())
        .await
        .unwrap();
    drop(outstanding);
    assert!(bridge.call("fixture/inspect", v("{}")).await.is_err());
    trace.drain();
    let stale = trace
        .messages
        .iter()
        .filter(|v| {
            is_method(v, "turn/completed")
                && f(f(f(v, "message"), "params"), "turn").get("id") == Some(&turn3)
        })
        .count();
    assert_eq!(stale, 1);
    assert!(
        !trace
            .messages
            .iter()
            .any(|v| is_method(v, "turn/diff/updated")
                && f(f(f(v, "message"), "params"), "diff") == &J::from("stale final diff"))
    );
    let stored = SessionStore::with_clock(
        &root.0,
        &root.0.join("state.sqlite"),
        Arc::new(|| 1700000000.0),
    )
    .unwrap();
    let interrupted = stored.get_turn(s(&turn4)).unwrap().unwrap();
    assert_eq!(f(&interrupted, "status"), &J::from("interrupted"));
    let proof = J::object([
        ("nativeControlsDuringApprovalAndDiff", true.into()),
        ("callbackRejection", true.into()),
        ("finalDiffBeforeCompletion", true.into()),
        ("interruptWinsFinalDiff", true.into()),
        ("stoppedWithBoundedQueues", true.into()),
        ("workflowLaunchWatchdogInterrupt", true.into()),
        ("messages", J::Array(trace.messages)),
    ]);
    // The assertions above qualify integration invariants; this row records
    // the actual trace separately from frozen-original differential rows.
    if let Some(path) = std::env::var_os("IVY_CLAUDE_ADAPTER_RUNTIME_PROOF") {
        fs::write(path, stringify(&proof, usize::MAX).unwrap()).unwrap();
    }
    }).catch_unwind().await;
    // Always close the exact runtime/child before the disposable SQLite root,
    // including when an assertion fails inside the integration scenario.
    let stopped = timeout(Duration::from_secs(10), handle.stop()).await;
    if stopped.is_err() {
        owner.abort();
    }
    let joined = timeout(Duration::from_secs(10), &mut owner).await;
    if joined.is_err() {
        owner.abort();
        let _ = owner.await;
    }
    let _ = bridge.stop().await;
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
    stopped.unwrap();
    joined.unwrap().unwrap().unwrap();
}

#[tokio::test]
async fn sdk_callback_inputs_share_native_heap_admission_and_release_after_original_answers() {
    let root = Root::new();
    let (bridge, events) =
        super::command_cases::transport_entry("tests/fixtures/agent-claude-bridge-transport.mjs")
            .await;
    let (native_tx, native_rx) = mpsc::unbounded_channel();
    let (handle, mut owner) = driver::spawn(
        adapter(&root.0),
        bridge.clone(),
        events,
        Bindings {
            sdk: SdkEnvironment {
                home: node_path::from_native(&root.0),
                environment: v(r#"{"CLAUDE_CODEX_CLI":"fixture-only-not-executed"}"#),
            },
            workflow_files: super::event_cases::files(),
            send: Arc::new(move |peer, mut message| {
                // Keep only identities in this pressure trace; item bodies stay
                // in the real adapter/SQLite path, never in a fixture history cache.
                if message.get("id").is_some() {
                    if let Some(mut params) = message.remove("params") {
                        params.remove("questions");
                        message.insert("params", params);
                    }
                    native_tx
                        .send(J::object([("peer", peer.into()), ("message", message)]))
                        .map_err(|_| AdapterError::new("Fixture native owner closed"))?;
                }
                Ok(())
            }),
            git_diff: Arc::new(|_| Box::pin(async { Ok(JsString::default()) })),
        },
    );
    let mut trace = Trace {
        input: native_rx,
        messages: vec![],
    };
    let mut callbacks = vec![];
    let mut joined = None;
    let scenario = std::panic::AssertUnwindSafe(async {
        request(&handle, "thread", "thread/start", v(r#"{"approvalPolicy":"on-request","sandbox":"workspace-write"}"#)).await;
        let thread = f(f(f(&trace.response("thread").await, "result"), "thread"), "id").clone();
        let turn = start(&handle, &bridge, &mut trace, &thread, "pressure").await;
        let mut units = vec![65; 192 * 1024];
        *units.last_mut().unwrap() = 0xd800;
        let input = J::object([("questions", J::Array(vec![J::object([
            ("question", JsString(units).into()),
            ("options", J::Array(vec![J::object([
                ("label", "B".repeat(192 * 1024).into()),
                ("description", "C".repeat(192 * 1024).into()),
            ])])),
        ])]))]);
        let mut original_ids = vec![];
        for index in 0..32 {
            let client = bridge.clone();
            let params = J::object([
                ("turnId", turn.clone()),
                ("toolName", "AskUserQuestion".into()),
                ("toolUseId", format!("original-pressure-{index}").into()),
                ("input", input.clone()),
            ]);
            callbacks.push(Some(tokio::spawn(async move {
                client.call("fixture/permission", params).await
            })));
            let next = tokio::select! {
                biased;
                result = &mut owner => { joined = Some(result); None },
                next = trace.until(|v| is_method(v, "item/tool/requestUserInput")) => Some(next),
            };
            let Some(next) = next else { break };
            original_ids.push(f(f(&next, "message"), "id").clone());
            if index == 13 {
                request(&handle, "during-pressure", "model/list", v("{}")).await;
                assert!(trace.response("during-pressure").await.get("result").is_some());
                for (resolved, id) in original_ids.iter().take(8).enumerate() {
                    handle.receive("peer".into(), J::object([
                        ("id", id.clone()),
                        ("result", v(r#"{"answers":{"q0":{"answers":["Other"],"notes":"chosen\ud800"}}}"#)),
                    ])).await.unwrap();
                    let result = timeout(Duration::from_secs(10), callbacks[resolved].take().unwrap())
                        .await.unwrap().unwrap().unwrap();
                    assert_eq!(f(&result, "behavior"), &J::from("deny"));
                    let payload = parse(&s(f(&result, "message")).to_utf8().unwrap(), usize::MAX).unwrap();
                    assert_eq!(f(&payload, "questions"), f(&input, "questions"));
                    assert_eq!(f(&payload, "answers").as_object().unwrap().len(), 1);
                    assert_eq!(f(&payload, "answers").as_object().unwrap()[0].1, v(r#""chosen\ud800""#));
                }
            }
        }
        assert!(joined.is_some(), "Driver retained all 24 original unresolved inputs plus both question projections without shared heap admission");
        assert!(original_ids.len() > 22, "Original answers did not release the shared payload budget for eight replacement callbacks");
        let error = joined.as_ref().unwrap().as_ref().unwrap().as_ref().unwrap_err();
        assert_eq!(error.name, "native_capacity");
        let stored = SessionStore::with_clock(&root.0, &root.0.join("state.sqlite"), Arc::new(|| 1700000000.0)).unwrap();
        assert_eq!(f(&stored.get_turn(s(&turn)).unwrap().unwrap(), "status"), &J::from("interrupted"));
        assert_eq!(original_ids.iter().map(s).collect::<std::collections::HashSet<_>>().len(), original_ids.len());
    }).catch_unwind().await;
    if joined.is_none() {
        timeout(Duration::from_secs(10), handle.stop())
            .await
            .unwrap();
        joined = Some(timeout(Duration::from_secs(10), &mut owner).await.unwrap());
    }
    bridge.stop().await.unwrap();
    for callback in callbacks.into_iter().flatten() {
        // Closing the original transport settles outstanding calls. Nothing
        // may fabricate permission success after the owner's capacity failure.
        if let Ok(value) = timeout(Duration::from_secs(10), callback)
            .await
            .unwrap()
            .unwrap()
        {
            assert_ne!(f(&value, "behavior"), &J::from("allow"));
        }
    }
    assert!(
        ivy_host_runtime::process::owned_process_tree_stopped(&bridge.owned_process())
            .await
            .unwrap()
    );
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
    joined.unwrap().unwrap().unwrap_err();
}
