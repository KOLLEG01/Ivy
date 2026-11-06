use super::*;
use ivy_agent_manager::claude::bridge::{Bridge, BridgeLaunch, SdkReply};
use std::{
    collections::{BTreeMap, HashSet},
    future::{Future, poll_fn},
    pin::Pin,
    task::Poll,
    time::Duration,
};
use tokio::time::timeout;

type Call = Pin<Box<dyn Future<Output = Result<SdkReply>> + Send>>;
fn call(bridge: &Bridge, method: &str, params: J) -> Call {
    let (bridge, method) = (bridge.clone(), method.to_owned());
    Box::pin(async move { bridge.call(&method, params).await })
}
async fn admitted(call: &mut Call) {
    poll_fn(|cx| match call.as_mut().poll(cx) {
        Poll::Pending => Poll::Ready(()),
        Poll::Ready(result) => panic!("Command was not admitted: {result:?}"),
    })
    .await;
}
async fn wait(call: Call) -> Result<SdkReply> {
    timeout(Duration::from_secs(10), call)
        .await
        .expect("SDK command did not settle")
}
pub(super) async fn transport_entry(
    entry: &str,
) -> (
    Bridge,
    tokio::sync::mpsc::Receiver<ivy_agent_manager::claude::bridge::SdkEvent>,
) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let node = Command::new("node")
        .args([
            "--input-type=module",
            "-e",
            "process.stdout.write(process.execPath)",
        ])
        .output()
        .unwrap();
    assert!(node.status.success());
    Bridge::spawn(
        BridgeLaunch {
            node_executable: PathBuf::from(String::from_utf8(node.stdout).unwrap()),
            entry: root.join(entry),
            cwd: root,
            environment: std::env::vars_os()
                .filter(|(key, _)| {
                    key.to_str().is_some_and(|key| {
                        matches!(
                            key.to_ascii_lowercase().as_str(),
                            "systemroot" | "windir" | "temp" | "tmp"
                        )
                    })
                })
                .collect::<BTreeMap<_, _>>(),
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
        },
        Arc::new(|_| {}),
    )
    .await
    .unwrap()
}
async fn transport() -> (
    Bridge,
    tokio::sync::mpsc::Receiver<ivy_agent_manager::claude::bridge::SdkEvent>,
) {
    transport_entry("tests/fixtures/agent-claude-command-pressure.mjs").await
}
async fn bridge() -> Bridge {
    transport().await.0
}

#[tokio::test]
async fn cancelled_sdk_waiters_keep_original_admission_and_leave_controls_available() {
    let bridge = bridge().await;
    for tag in 0..56 {
        let mut pending = call(
            &bridge,
            "fixture/hold",
            J::object([("tag", (tag as f64).into())]),
        );
        admitted(&mut pending).await;
        drop(pending);
    }
    assert_eq!(
        wait(call(&bridge, "fixture/hold", v("{}")))
            .await
            .unwrap_err()
            .name,
        "native_capacity"
    );
    let original = wait(call(&bridge, "permission/reply", v("{}")))
        .await
        .unwrap();
    let original = original.as_array().unwrap();
    assert_eq!(original.len(), 56);
    let mut identities = HashSet::new();
    for (tag, row) in original.iter().enumerate() {
        assert!(identities.insert(s(f(row, "id")).clone()));
        assert_eq!(f(row, "tag"), &J::Number(tag as f64));
    }
    assert_eq!(
        wait(call(
            &bridge,
            "fixture/echo",
            v(r#"{"original":"kept\ud800"}"#)
        ))
        .await
        .unwrap(),
        v(r#"{"original":"kept\ud800"}"#)
    );
    bridge.stop().await.unwrap();
}

#[tokio::test]
async fn command_bytes_cover_blocked_writes_while_the_reply_reader_and_stop_remain_live() {
    let root = Root::new();
    let release = root.0.join("release");
    let resume = root.0.join("resume");
    let bridge = bridge().await;
    let mut original = call(
        &bridge,
        "fixture/hold",
        J::object([("tag", "original".into())]),
    );
    admitted(&mut original).await;
    wait(call(
        &bridge,
        "fixture/pauseInput",
        J::object([
            ("release", node_path::from_native(&release).into()),
            ("resume", node_path::from_native(&resume).into()),
        ]),
    ))
    .await
    .unwrap();
    // Use exact owned capacity: collecting encode_utf16 can leave substantial
    // spare space, which the production heap reservation correctly includes.
    let large = || J::object([("body", JsString(vec![120; 8 * 1024 * 1024]).into())]);
    let mut first = call(&bridge, "fixture/hold", large());
    admitted(&mut first).await;
    let mut second = call(&bridge, "fixture/hold", large());
    admitted(&mut second).await;
    tokio::task::yield_now().await;
    let rejected = wait(call(&bridge, "fixture/hold", large()))
        .await
        .unwrap_err();
    assert_eq!(rejected.name, "native_capacity");
    let mut control = call(&bridge, "query/interrupt", v(r#"{"hold":true}"#));
    admitted(&mut control).await;
    // The child releases this original reply only after the large pipe write
    // is parked. A combined read/write owner cannot observe it in that state.
    fs::write(&release, b"release original").unwrap();
    let reply = wait(original).await;
    let stop = tokio::spawn({
        let bridge = bridge.clone();
        async move { bridge.stop().await }
    });
    tokio::task::yield_now().await;
    fs::write(&resume, b"permit EOF cleanup").unwrap();
    timeout(Duration::from_secs(10), stop)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(reply.unwrap(), J::from("original"));
    for pending in [first, second, control] {
        assert_eq!(wait(pending).await.unwrap_err().name, "outcome_unknown");
    }
    assert!(
        ivy_host_runtime::process::owned_process_tree_stopped(&bridge.owned_process())
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn all_accepted_sdk_calls_are_bounded_until_original_reply_or_confirmed_stop() {
    let bridge = bridge().await;
    let mut calls = Vec::new();
    for index in 0..64 {
        let method = if index < 56 {
            "fixture/hold"
        } else {
            "query/interrupt"
        };
        let mut pending = call(
            &bridge,
            method,
            J::object([("hold", true.into()), ("tag", (index as f64).into())]),
        );
        admitted(&mut pending).await;
        calls.push(pending);
    }
    assert_eq!(
        wait(call(&bridge, "permission/reply", v("{}")))
            .await
            .unwrap_err()
            .name,
        "native_capacity"
    );
    bridge.stop().await.unwrap();
    for pending in calls {
        assert_eq!(wait(pending).await.unwrap_err().name, "outcome_unknown");
    }
    assert!(
        ivy_host_runtime::process::owned_process_tree_stopped(&bridge.owned_process())
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn dropping_the_last_bridge_owner_retires_a_blocked_writer_and_its_exact_child() {
    let root = Root::new();
    let release = root.0.join("release");
    let resume = root.0.join("resume");
    let bridge = bridge().await;
    let process = bridge.owned_process();
    wait(call(
        &bridge,
        "fixture/pauseInput",
        J::object([
            ("release", node_path::from_native(&release).into()),
            ("resume", node_path::from_native(&resume).into()),
        ]),
    ))
    .await
    .unwrap();
    let mut pending = call(
        &bridge,
        "fixture/hold",
        J::object([("body", JsString(vec![120; 8 * 1024 * 1024]).into())]),
    );
    admitted(&mut pending).await;
    tokio::task::yield_now().await;
    drop(pending);
    drop(bridge);
    // Keep the peer paused: only last-owner closure can retire the writer.
    // Release the fixture after a failed deadline so a regression still cleans up.
    let completed = timeout(Duration::from_secs(10), process.completion.wait()).await;
    fs::write(&resume, b"release fixture after retirement deadline").unwrap();
    let completed = completed.expect("Dropping the last Bridge did not retire its exact child");
    assert_ne!(completed.error_code.as_deref(), Some("outcome_unknown"));
    assert!(
        ivy_host_runtime::process::owned_process_tree_stopped(&process)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn refused_sdk_callback_reply_finishes_the_owner_instead_of_forgetting_the_answer() {
    use futures_util::FutureExt;
    use ivy_agent_manager::claude::{
        driver::{self, Bindings},
        sdk_options::SdkEnvironment,
        store::SessionStore,
    };
    use tokio::sync::mpsc;

    async fn until(receiver: &mut mpsc::UnboundedReceiver<J>, predicate: impl Fn(&J) -> bool) -> J {
        timeout(Duration::from_secs(10), async {
            loop {
                let value = receiver.recv().await.expect("Native sender closed");
                if predicate(&value) {
                    return value;
                }
            }
        })
        .await
        .expect("Native message did not arrive")
    }
    let root = Root::new();
    let (bridge, events) = transport().await;
    let (sender, mut native) = mpsc::unbounded_channel();
    let bindings = Bindings {
        sdk: SdkEnvironment {
            home: node_path::from_native(&root.0),
            environment: v(r#"{"CLAUDE_CODEX_CLI":"fixture-only-not-executed"}"#),
        },
        workflow_files: super::event_cases::files(),
        send: Arc::new(move |_, message| {
            sender
                .send(message)
                .map_err(|_| AdapterError::new("Fixture native owner closed"))
        }),
        git_diff: Arc::new(|_| Box::pin(async { Ok(JsString::default()) })),
    };
    let (handle, mut owner) = driver::spawn(
        super::driver_cases::adapter(&root.0),
        bridge.clone(),
        events,
        bindings,
    );
    let scenario = std::panic::AssertUnwindSafe(async {
        handle
            .receive(
                "peer".into(),
                J::object([
                    ("id", "thread".into()),
                    ("method", "thread/start".into()),
                    (
                        "params",
                        J::object([
                            ("cwd", node_path::from_native(&root.0).into()),
                            ("model", "sonnet".into()),
                            ("approvalPolicy", "on-request".into()),
                            ("sandbox", "workspace-write".into()),
                        ]),
                    ),
                ]),
            )
            .await
            .unwrap();
        let response = until(&mut native, |value| f(value, "id") == &J::from("thread")).await;
        let thread = f(f(f(&response, "result"), "thread"), "id").clone();
        handle
            .receive(
                "peer".into(),
                J::object([
                    ("id", "turn".into()),
                    ("method", "turn/start".into()),
                    (
                        "params",
                        J::object([
                            ("threadId", thread),
                            ("input", v(r#"[{"type":"text","text":"fixture"}]"#)),
                        ]),
                    ),
                ]),
            )
            .await
            .unwrap();
        let response = until(&mut native, |value| f(value, "id") == &J::from("turn")).await;
        let turn = f(f(f(&response, "result"), "turn"), "id").clone();
        wait(call(
            &bridge,
            "fixture/awaitTurn",
            J::object([("turnId", turn.clone())]),
        ))
        .await
        .unwrap();
        wait(call(
            &bridge,
            "fixture/permission",
            J::object([("turnId", turn.clone())]),
        ))
        .await
        .unwrap();
        let approval = until(&mut native, |value| {
            f(value, "method") == &J::from("item/commandExecution/requestApproval")
        })
        .await;
        let mut calls = Vec::new();
        for index in 0..64 {
            let method = if index < 56 {
                "fixture/hold"
            } else {
                "query/interrupt"
            };
            let mut pending = call(&bridge, method, J::object([("hold", true.into())]));
            admitted(&mut pending).await;
            calls.push(pending);
        }
        handle
            .receive(
                "peer".into(),
                J::object([
                    ("id", f(&approval, "id").clone()),
                    ("result", v(r#"{"decision":"acceptForSession"}"#)),
                ]),
            )
            .await
            .unwrap();
        (turn, calls)
    })
    .catch_unwind()
    .await;
    if scenario.is_err() {
        handle.stop().await;
    }
    let result = timeout(Duration::from_secs(10), &mut owner).await;
    if result.is_err() {
        owner.abort();
        let _ = (&mut owner).await;
    }
    bridge.stop().await.unwrap();
    let (turn, calls) = match scenario {
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    assert_eq!(
        result.unwrap().unwrap().unwrap_err().name,
        "native_capacity"
    );
    for pending in calls {
        assert_eq!(wait(pending).await.unwrap_err().name, "outcome_unknown");
    }
    let store = SessionStore::with_clock(
        &root.0,
        &root.0.join("state.sqlite"),
        Arc::new(|| 1700000000.0),
    )
    .unwrap();
    let stored = store.get_turn(s(&turn)).unwrap().unwrap();
    assert_eq!(f(&stored, "status"), &J::from("interrupted"));
    assert!(
        ivy_host_runtime::process::owned_process_tree_stopped(&bridge.owned_process())
            .await
            .unwrap()
    );
}
