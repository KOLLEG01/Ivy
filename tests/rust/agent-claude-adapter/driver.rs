use super::*;
use futures_util::FutureExt;
use ivy_agent_manager::claude::{
    bridge::{Bridge, BridgeLaunch},
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

fn adapter(home: &Path) -> Adapter {
    let clock: ivy_agent_manager::claude::store::Clock = Arc::new(|| 1700000000.0);
    let millis: ivy_agent_manager::claude::store::Clock = Arc::new(|| 1700000000123.0);
    let next = Arc::new(Mutex::new(0usize));
    let ids: Arc<dyn Fn() -> JsString + Send + Sync> = Arc::new(move || {
        let mut n = next.lock().unwrap();
        let id = format!("runtime-{}", *n);
        *n += 1;
        id.into()
    });
    let locale = ivy_sdk::collation::LocaleRuntime::load(Path::new(
        &std::env::var_os("IVY_TEST_LOCALE_RUNTIME").unwrap(),
    ))
    .unwrap()
    .executing_default()
    .unwrap();
    let compare = Arc::new(move |a: &JsString, b: &JsString| locale.compare(a, b));
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
            read_base64: Arc::new(|_| Err(AdapterError::new("Unexpected file read"))),
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
async fn call(bridge: &Bridge, method: &str, params: J) -> J {
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
            environment: std::env::vars_os()
                .filter(|(k, _)| {
                    matches!(k.to_str(), Some("SystemRoot" | "WINDIR" | "TEMP" | "TMP"))
                })
                .collect::<BTreeMap<_, _>>(),
        },
        Arc::new(|_| {}),
    )
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
