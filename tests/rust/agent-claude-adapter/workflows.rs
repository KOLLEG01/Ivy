use super::*;
use ivy_agent_manager::claude::{
    sdk_events::SdkTurn,
    workflow::{EventSink, WorkflowFiles},
};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::{
    sync::Semaphore,
    time::{Duration, timeout},
};

async fn until(predicate: impl Fn() -> bool) {
    timeout(Duration::from_secs(8), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(5)).await
        }
    })
    .await
    .expect("workflow fixture timed out");
}
async fn send(turn: &mut SdkTurn, message: J) {
    turn.message(&message).await.unwrap()
}
fn result() -> J {
    v(r#"{"type":"result","subtype":"success","result":"parent"}"#)
}
fn terminal() -> J {
    v(
        r#"{"type":"system","subtype":"task_notification","task_id":"task","status":"completed","summary":"finished"}"#,
    )
}
fn assistant(text: &str) -> J {
    J::object([
        ("type", "assistant".into()),
        (
            "message",
            J::object([
                ("id", text.into()),
                (
                    "content",
                    J::Array(vec![J::object([
                        ("type", "text".into()),
                        ("text", text.into()),
                    ])]),
                ),
            ]),
        ),
    ])
}
async fn native(root: &Path) -> J {
    let mut rows = vec![];
    for kind in [
        "shared-flush",
        "failure-deferred",
        "hidden-stop",
        "late-terminal",
        "failure-terminal",
        "rejected-start",
    ] {
        let base = root.join(kind);
        let directory = base.join("subagents/workflows/run");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("journal.jsonl"),
            "\n{\"type\":\"result\",\"agentId\":\"abcd1234\",\"result\":\"individual\"}\n",
        )
        .unwrap();
        fs::write(
            directory.join("agent-abcd1234.jsonl"),
            "{\"message\":{\"role\":\"user\",\"content\":\"First task\\nMore\"}}\n",
        )
        .unwrap();
        let io = Arc::new(Semaphore::new(0));
        let callback = Arc::new(Semaphore::new(0));
        let reads = Arc::new(AtomicUsize::new(0));
        let events = Arc::new(Mutex::new(vec![]));
        let fixture = super::event_cases::files();
        let read = fixture.realpath.clone();
        let dir = node_path::normalize(Style::native(), &node_path::from_native(&directory));
        let held = dir.clone();
        let io_reader = io.clone();
        let read_count = reads.clone();
        let files = WorkflowFiles {
            realpath: Arc::new(move |path| {
                let read = read.clone();
                let io = io_reader.clone();
                let count = read_count.clone();
                let held = held.clone();
                Box::pin(async move {
                    if path == held {
                        count.fetch_add(1, Ordering::SeqCst);
                        io.acquire().await.unwrap().forget();
                        if kind == "failure-deferred" || kind == "failure-terminal" {
                            return Err(AdapterError::new("fixture journal read failed"));
                        }
                    }
                    read(path).await
                })
            }),
            read_regular: fixture.read_regular,
        };
        let observed = events.clone();
        let callback_wait = callback.clone();
        let sink: EventSink = Arc::new(move |event| {
            let observed = observed.clone();
            let callback = callback_wait.clone();
            Box::pin(async move {
                let started = f(&event, "type") == &v("\"tool_use\"")
                    && f(&event, "toolUseId") == &v("\"workflow-agent:task:abcd1234\"");
                observed.lock().unwrap().push(event);
                if started {
                    if kind == "shared-flush" {
                        callback.acquire().await.unwrap().forget()
                    }
                    if kind == "rejected-start" {
                        return Err(AdapterError::new("fixture consumer rejected"));
                    }
                }
                Ok(())
            })
        });
        let mut turn = SdkTurn::new(
            J::object([
                ("threadId", "thread".into()),
                ("turnId", "turn".into()),
                ("cwd", J::String(node_path::from_native(root))),
                ("prompt", "fixture".into()),
                ("outputFormat", J::Null),
            ]),
            &node_path::from_native(root),
            &J::object([(
                "CLAUDE_CONFIG_DIR",
                J::String(node_path::from_native(&base)),
            )]),
            files,
            sink,
        );
        send(&mut turn,v(r#"{"type":"assistant","message":{"id":"workflow","content":[{"type":"tool_use","id":"launch","name":"Workflow","input":{}}]}}"#)).await;
        let mut launch = v(
            r#"{"type":"user","tool_use_result":{"status":"async_launched","taskId":"task","runId":"run","workflowName":"Journal"},"message":{"content":[{"type":"tool_result","tool_use_id":"launch","content":"launched"}]}}"#,
        );
        let mut launch_result = f(&launch, "tool_use_result").clone();
        launch_result.insert("transcriptDir", J::String(dir));
        launch.insert("tool_use_result", launch_result);
        send(&mut turn, launch).await;
        timeout(Duration::from_millis(500),send(&mut turn,v(r#"{"type":"system","subtype":"task_started","task_type":"local_workflow","task_id":"task","tool_use_id":"launch"}"#))).await.expect("attach blocked on its first read");
        until(|| reads.load(Ordering::SeqCst) == 1).await;
        let projected = || {
            events
                .lock()
                .unwrap()
                .iter()
                .any(|e| f(e, "toolUseId") == &v("\"workflow-agent:task:abcd1234\""))
        };
        let mut checks = J::object([("attachReturnedWhileReadHeld", (!projected()).into())]);
        if kind == "shared-flush" {
            let a = turn.poll();
            let b = turn.poll();
            let (_, _, shared_read) = tokio::join!(a, b, async {
                tokio::time::sleep(Duration::from_millis(30)).await;
                let shared_read = reads.load(Ordering::SeqCst) == 1;
                send(&mut turn, assistant("visible")).await;
                io.add_permits(1);
                until(projected).await;
                send(&mut turn, assistant("suppressed")).await;
                callback.add_permits(1);
                shared_read
            });
            checks.insert("sharedRead", shared_read.into());
            checks.insert(
                "suppressedDuringCallback",
                (!events
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|e| f(e, "delta") == &v("\"suppressed\"")))
                .into(),
            );
            send(&mut turn, result()).await;
            checks.insert("waitsForTerminal", (!turn.is_finished()).into());
            send(&mut turn, terminal()).await;
        } else if kind == "hidden-stop" {
            timeout(Duration::from_millis(900),send(&mut turn,v(r#"{"type":"system","subtype":"task_started","task_type":"local_workflow","task_id":"task","skip_transcript":true}"#))).await.expect("stop did not respect its bound");
            checks.insert(
                "boundedStopBeforeReadReleased",
                (reads.load(Ordering::SeqCst) == 1).into(),
            );
            io.add_permits(1);
            turn.poll().await.unwrap();
            tokio::time::sleep(Duration::from_millis(30)).await;
            send(&mut turn, result()).await;
            checks.insert("noLateAgents", (!projected()).into());
        } else {
            send(&mut turn, result()).await;
            checks.insert("waitsForTerminal", (!turn.is_finished()).into());
            if kind == "late-terminal" || kind == "failure-terminal" {
                let (_, count) = tokio::join!(send(&mut turn, terminal()), async {
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    let count = reads.load(Ordering::SeqCst);
                    io.add_permits(1);
                    count
                });
                checks.insert("sharedRead", (count == 1).into());
            } else {
                io.add_permits(1)
            }
        }
        until(|| turn.is_finished()).await;
        let outcome = match turn.outcome().unwrap() {
            Ok(()) => v(r#"{"success":true}"#),
            Err(e) => J::object([(
                "error",
                J::object([("name", e.name.into()), ("message", J::String(e.message))]),
            )]),
        };
        rows.push(J::object([
            ("kind", kind.into()),
            ("checks", checks),
            ("events", J::Array(events.lock().unwrap().clone())),
            ("outcome", outcome),
        ]));
    }
    J::object([("rows", J::Array(rows))])
}
#[tokio::test]
async fn background_workflow_io_failure_stop_and_terminal_races_match_original() {
    let root = Root::new();
    let original = Root::new();
    let mut child = Command::new("node")
        .arg("tests/fixtures/agent-claude-workflow-concurrency-oracle.mjs")
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            stringify(
                &J::object([("root", J::String(node_path::from_native(&original.0)))]),
                usize::MAX,
            )
            .unwrap()
            .as_bytes(),
        )
        .unwrap();
    let actual = native(&root.0).await;
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = v(std::str::from_utf8(&output.stdout).unwrap());
    compare("background-workflow-concurrency", actual, expected);
}
