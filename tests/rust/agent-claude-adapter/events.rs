use super::protocol_cases::normalize;
use super::sdk_cases::replace;
use super::*;
use ivy_agent_manager::claude::{
    sdk_events::SdkTurn,
    workflow::{EventSink, WorkflowFiles},
};

pub(super) fn files() -> WorkflowFiles {
    WorkflowFiles::host()
}
async fn native(root: &Path, spec: &J) -> J {
    let mut rows = vec![];
    for row in f(spec, "rows").as_array().unwrap() {
        let row = replace(row, root);
        let directory = root.join(f(&row, "directory").as_str().unwrap());
        fs::create_dir_all(&directory).unwrap();
        for file in f(&row, "files").as_array().unwrap_or(&[]) {
            let path = directory.join(f(file, "path").as_str().unwrap());
            assert!(path.starts_with(&directory));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, f(file, "text").as_str().unwrap()).unwrap()
        }
        let events = Arc::new(Mutex::new(vec![]));
        let observed = events.clone();
        let rejected = f(&row, "rejectEventType").clone();
        let sink: EventSink = Arc::new(move |event| {
            let observed = observed.clone();
            let rejected = rejected.clone();
            Box::pin(async move {
                let fail = f(&event, "type") == &rejected;
                observed.lock().unwrap().push(event);
                if fail {
                    Err(AdapterError::new("fixture consumer rejected"))
                } else {
                    Ok(())
                }
            })
        });
        let environment = J::object([(
            "CLAUDE_CONFIG_DIR",
            J::String(node_path::from_native(&directory)),
        )]);
        let mut turn = SdkTurn::new(
            f(&row, "context").clone(),
            &node_path::from_native(root),
            &environment,
            files(),
            sink,
        );
        for step in f(&row, "steps").as_array().unwrap() {
            if turn.is_finished() {
                break;
            }
            if let Some(error) = step.get("error") {
                turn.end(Some(AdapterError::new(s(error).clone())))
                    .await
                    .unwrap();
                break;
            }
            let result = if f(step, "poll").as_bool() == Some(true) {
                turn.poll().await
            } else {
                turn.message(f(step, "message")).await
            };
            if let Err(error) = result {
                turn.end(Some(error)).await.unwrap();
                break;
            }
        }
        if !turn.is_finished() {
            turn.end(None).await.unwrap()
        }
        let outcome = match turn.outcome().unwrap() {
            Ok(()) => v(r#"{"success":true}"#),
            Err(e) => v(&format!(
                r#"{{"error":{{"name":"{}","message":{}}}}}"#,
                e.name,
                stringify(&J::String(e.message), usize::MAX).unwrap()
            )),
        };
        rows.push(J::object([
            ("events", J::Array(events.lock().unwrap().clone())),
            ("outcome", outcome),
        ]));
    }
    J::object([("rows", J::Array(rows))])
}
#[tokio::test]
async fn raw_sdk_stream_dedup_subagents_workflows_and_iterator_end_match_original() {
    let context = v(
        r#"{"threadId":"thread","turnId":"turn","cwd":"__ROOT__","prompt":"fixture","outputFormat":null}"#,
    );
    let mut rows = vec![];
    let sequences = [
        r#"[
          {"message":{"type":"system","subtype":"init","session_id":"session"}},
          {"message":{"type":"stream_event","event":{"type":"message_start","message":{"id":"m1"}}}},
          {"message":{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"first\ud800"}}}},
          {"message":{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"thinking_delta","thinking":"think"}}}},
          {"message":{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"first\ud800 tail"},{"type":"text","text":"second"},{"type":"thinking","thinking":"think more"}]}}},
          {"message":{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"third"}]}}},
          {"message":{"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"tool_use","id":"bash","name":"Bash"}}}},
          {"message":{"type":"assistant","message":{"id":"m2","content":[{"type":"tool_use","id":"bash","name":"Bash","input":{"command":"fixture"}},{"type":"tool_use","id":"ask","name":"AskUserQuestion","input":{}},{"type":"tool_use","id":"structured","name":"StructuredOutput","input":{}}]}}},
          {"message":{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"bash","content":[{"type":"text","text":"done"}],"is_error":false}]}}},
          {"message":{"type":"assistant","message":{"id":"m3","content":[{"type":"tool_use","id":"child","name":"Agent","input":{"prompt":"subtask"}},{"type":"text","text":"suppressed"}]}}},
          {"message":{"type":"assistant","parent_tool_use_id":"child","message":{"id":"nested","content":[{"type":"tool_use","id":"nested-tool","name":"Bash","input":{}},{"type":"text","text":"hidden"}]}}},
          {"message":{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"hidden"}}}},
          {"message":{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"child","content":"subtask done"}]}}},
          {"message":{"type":"assistant","message":{"id":"m4","content":[{"type":"text","text":"final"}]}}},
          {"message":{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","utilization":0.2}}},
          {"message":{"type":"rate_limit","rate_limit_info":{"status":"allowed_warning","rateLimitType":"five_hour","utilization":0.805,"resetsAt":1700000000.123}}},
          {"message":{"type":"rate_limit_event","rate_limit_info":{"status":"unknown"}}},
          {"message":{"type":"rate_limit","message":"explicit","rate_limit_info":{}}},
          {"message":{"type":"hook_event","hook_name":"PreToolUse","subtype":"running","permission_decision":"allow","reason":"hook\ud800"}},
          {"message":{"type":"system","subtype":"permission_denied","tool_name":"Bash"}},
          {"message":{"type":"result","subtype":"success","session_id":"new-session","result":"ok","usage":{"input_tokens":4},"duration_ms":8,"duration_api_ms":2,"num_turns":1,"total_cost_usd":0.125}}
        ]"#,
        r#"[{"message":{"type":"stream_event","event":{"type":"message_start","message":{"id":"s"}}}},{"message":{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"  {\"ok\":"}}}},{"message":{"type":"assistant","message":{"id":"s","content":[{"type":"text","text":"  {\"ok\":true}  "}]} }},{"message":{"type":"result","subtype":"success","result":"unused","usage":{},"duration_ms":5}}]"#,
        r#"[{"message":{"type":"assistant","message":{"id":"failed","content":[{"type":"thinking","thinking":"reason"}]}}},{"message":{"type":"result","subtype":"error_max_turns","result":"turn failed","is_error":true,"session_id":"failed-session"}}]"#,
        r#"[{"message":{"type":"system","subtype":"task_started","task_type":"unrelated","task_id":"other"}},{"message":{"type":"system","subtype":"task_notification","task_id":"other","status":"completed"}}]"#,
        r#"[{"message":{"type":"assistant","message":{"id":"wf","content":[{"type":"tool_use","id":"workflow","name":"Workflow","input":{}}]}}},{"message":{"type":"result","subtype":"success","result":"parent"}},{"message":{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"workflow","content":"launched"}]}}},{"message":{"type":"system","subtype":"task_started","task_type":"local_workflow","task_id":"task","workflow_name":"Example","description":"work","tool_use_id":"workflow"}},{"message":{"type":"system","subtype":"task_notification","task_id":"task","status":"completed","summary":"finished","usage":{"total_tokens":10,"tool_uses":2,"duration_ms":30}}}]"#,
        r#"[{"message":{"type":"system","subtype":"task_notification","task_type":"local_workflow","task_id":"early","status":"failed","summary":"failed before start"}},{"message":{"type":"system","subtype":"task_started","task_type":"local_workflow","task_id":"early"}},{"message":{"type":"result","subtype":"success","result":"parent"}}]"#,
        r#"[{"message":{"type":"system","subtype":"task_started","task_type":"local_workflow","task_id":"hidden","skip_transcript":true}},{"message":{"type":"system","subtype":"task_notification","task_type":"local_workflow","task_id":"hidden","status":"completed"}},{"message":{"type":"result","subtype":"success","result":"hidden parent"}}]"#,
        r#"[{"message":{"type":"assistant","message":{"id":"error","content":[{"type":"text","text":"partial"}]}}},{"error":"fixture SDK iterator failed"}]"#,
    ];
    for (index, steps) in sequences.iter().enumerate() {
        let mut context = context.clone();
        if index == 1 {
            context.insert("outputFormat", v(r#"{"type":"json_schema","schema":{}}"#))
        }
        rows.push(J::object([
            ("directory", format!("case-{index}").into()),
            ("context", context),
            ("steps", v(steps)),
        ]));
    }
    let mut rejected = rows[0].clone();
    rejected.insert("directory", "consumer-failure".into());
    rejected.insert("rejectEventType", "text_delta".into());
    rows.push(rejected);
    let mut journal = J::object([
        ("directory", "journal".into()),
        ("context", context.clone()),
        (
            "files",
            v(
                r#"[{"path":"subagents/workflows/run/journal.jsonl","text":"{\"type\":\"started\",\"agentId\":\"abcdef01\"}\n{\"type\":\"result\",\"agentId\":\"abcdef01\",\"result\":{\"status\":\"completed\",\"value\":3}}\n"},{"path":"subagents/workflows/run/agent-abcdef01.jsonl","text":"{\"message\":{\"role\":\"user\",\"content\":\"Investigate this\"}}\n{\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"fallback result\"}]}}\n"}]"#,
            ),
        ),
        (
            "steps",
            v(r#"[
        {"message":{"type":"assistant","message":{"id":"workflow","content":[{"type":"tool_use","id":"launch","name":"Workflow","input":{}}]}}},
        {"message":{"type":"user","tool_use_result":{"status":"async_launched","taskId":"journal-task","runId":"run","transcriptDir":"__ROOT__/journal/subagents/workflows/run","workflowName":"Journal"},"message":{"content":[{"type":"tool_result","tool_use_id":"launch","content":"launched"}]}}},
        {"message":{"type":"system","subtype":"task_started","task_type":"local_workflow","task_id":"journal-task","tool_use_id":"launch"}},
        {"poll":true},
        {"message":{"type":"result","subtype":"success","result":"parent done"}},
        {"message":{"type":"system","subtype":"task_notification","task_id":"journal-task","status":"completed","summary":"all agents done"}}
    ]"#),
        ),
    ]);
    rows.push(journal.clone());
    let mut orphan = journal.clone();
    orphan.insert("directory", "journal-orphan".into());
    orphan.insert("files",v(r#"[{"path":"subagents/workflows/run/journal.jsonl","text":"{\"type\":\"started\",\"agentId\":\"abcdef01\"}\n"},{"path":"subagents/workflows/run/agent-abcdef01.jsonl","text":"{\"message\":{\"role\":\"user\",\"content\":\"Investigate this\"}}\n"}]"#));
    let mut orphan_steps = f(&orphan, "steps").as_array().unwrap().to_vec();
    let mut launch_message = f(&orphan_steps[1], "message").clone();
    let mut launch = f(&launch_message, "tool_use_result").clone();
    launch.insert(
        "transcriptDir",
        "__ROOT__/journal-orphan/subagents/workflows/run".into(),
    );
    launch_message.insert("tool_use_result", launch);
    orphan_steps[1].insert("message", launch_message);
    let mut terminal = f(&orphan_steps[5], "message").clone();
    terminal.insert("summary", v(r#""orphan summary\ud800""#));
    orphan_steps[5].insert("message", terminal);
    orphan.insert("steps", J::Array(orphan_steps));
    rows.push(orphan);
    journal.insert("directory", "journal-end".into());
    let mut steps = f(&journal, "steps").as_array().unwrap().to_vec();
    steps.truncate(5);
    let mut message = f(&steps[1], "message").clone();
    let mut launch = f(&message, "tool_use_result").clone();
    launch.insert(
        "transcriptDir",
        "__ROOT__/journal-end/subagents/workflows/run".into(),
    );
    message.insert("tool_use_result", launch);
    steps[1].insert("message", message);
    // Keep the same admitted transcript tree; this row intentionally exercises
    // iterator EOF with a deferred parent after the individual child finished.
    journal.insert("steps", J::Array(steps));
    rows.push(journal);
    let spec = J::object([("rows", J::Array(rows))]);
    let reference = Root::new();
    let actual = Root::new();
    let mut input = spec.clone();
    input.insert("root", J::String(node_path::from_native(&reference.0)));
    let mut child = Command::new("node")
        .arg("tests/fixtures/agent-claude-events-oracle.mjs")
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
        .write_all(stringify(&input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = v(std::str::from_utf8(&output.stdout).unwrap());
    compare(
        "raw SDK messages and workflow lifetimes",
        normalize(&native(&actual.0, &spec).await, &actual.0),
        normalize(&expected, &reference.0),
    );
}
