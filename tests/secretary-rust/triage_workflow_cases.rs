use super::{
    microsoft_store_cases::{retained, seed},
    *,
};
use ivy_contracts::{checked_native_contract::CheckedNativeContracts, json::canonical};
use ivy_sdk::clock::{Clock, SystemClock};
use ivy_secretary::{
    engine::SecretaryEngine,
    inbox_evidence::InboxEvidence,
    inbox_recovery::RecoveryScan,
    journal::{Journal, Limits},
    store::Store,
    triage::SecretaryTriage,
    triage_completion::TriageCompletion,
    triage_native::TriageNative,
};
use ivy_store::rusqlite::params;
use std::{
    cell::RefCell,
    io::Write,
    path::Path,
    process::{Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
};

struct Fixed {
    value: i64,
    step: i64,
    calls: AtomicUsize,
}
impl Clock for Fixed {
    fn utc_millis(&self) -> i64 {
        self.value + self.calls.fetch_add(1, Ordering::Relaxed) as i64 * self.step
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
struct Yielding(Scripted, String, Vec<ivy_contracts::JsString>);
impl RpcClient for Yielding {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            tokio::task::yield_now().await;
            if params.get("qualifiedName").is_some_and(|name| {
                ["agent.operation", "agent.invoke"]
                    .iter()
                    .any(|n| name == &IvyJson::from(*n))
            }) {
                let actual = params
                    .required("arguments")?
                    .required("operationId")?
                    .as_js_string()
                    .unwrap();
                let mut steps = self.0.steps.lock().unwrap();
                if let Some(step) = steps.front_mut() {
                    let selected = step
                        .get("value")
                        .and_then(|v| v.get("operationId"))
                        .or_else(|| {
                            step.get("error")
                                .and_then(|e| e.get("details"))
                                .and_then(|d| d.get("operationId"))
                        })
                        .and_then(IvyJson::as_js_string)
                        .cloned();
                    if let Some(expected) = selected.filter(|id| !self.2.contains(id)) {
                        fn align(
                            value: &mut IvyJson,
                            expected: &ivy_contracts::JsString,
                            actual: &ivy_contracts::JsString,
                        ) {
                            match value {
                                IvyJson::String(s) if s == expected => *s = actual.clone(),
                                IvyJson::Array(values) => {
                                    for value in values {
                                        align(value, expected, actual);
                                    }
                                }
                                IvyJson::Object(values) => {
                                    for (_, value) in values {
                                        align(value, expected, actual);
                                    }
                                }
                                _ => (),
                            }
                        }
                        align(step, &expected, actual);
                    }
                }
            }
            assert_eq!(
                self.0
                    .steps
                    .lock()
                    .unwrap()
                    .front()
                    .and_then(|r| r.get("method")),
                Some(&method.into()),
                "RPC order in {} after {} calls",
                self.1,
                self.0.trace.lock().unwrap().len()
            );
            self.0.request(method, params, options).await
        })
    }
}
fn result(value: Result<IvyJson, IvyError>) -> IvyJson {
    match value {
        Ok(value) => IvyJson::object([("value", value)]),
        Err(error) => {
            let mut value = IvyJson::object([
                ("code", IvyJson::String(error.code_js())),
                ("message", IvyJson::String(error.message_js())),
                ("outcome", error.outcome.as_str().into()),
            ]);
            if let Some(details) = error.details {
                value.insert("details", details);
            }
            value
        }
    }
}
fn text(value: &IvyJson, key: &str) -> String {
    value.required(key).unwrap().as_str().unwrap()
}
fn node(path: &str) -> Command {
    let mut command = Command::new("node");
    command
        .arg(path)
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}
struct Directory(std::path::PathBuf);
impl Directory {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("secretary-triage-rust-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let actual = std::fs::canonicalize(&self.0).unwrap();
        assert_eq!(
            actual.parent(),
            Some(
                std::fs::canonicalize(std::env::temp_dir())
                    .unwrap()
                    .as_path()
            )
        );
        assert!(
            actual
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("secretary-triage-rust-")
        );
        if let Err(error) = std::fs::remove_dir_all(actual) {
            if !std::thread::panicking() {
                panic!("Owned fixture cleanup: {error}")
            }
        }
    }
}
#[tokio::test]
async fn original_triage_workflow_native_completion_assessment_and_reopen() {
    let output = node("tests/secretary-rust/triage-workflow-oracle.mjs")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let corpus = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        64 * 1024 * 1024,
    )
    .unwrap();
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert!(rows.len() >= 75);
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    // Both sides see the same retained distribution. A source-tree catalog
    // would bypass the original reference distribution's live fallback reads.
    let oracle_root = std::env::var_os("IVY_SECRETARY_ORACLE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join(".local/build/secretary"));
    let contracts = CheckedNativeContracts::new(oracle_root);
    let selected = IvyJson::object([
        ("objectId", "00000000-0000-4000-8000-000000000002".into()),
        ("revision", IvyJson::Number(1.0)),
    ]);
    let mut actual_rows = Vec::new();
    let mut physical_reopens = 0;
    for row in rows {
        let started_at = SystemClock.utc_millis();
        let input = row.required("input").unwrap();
        let label = text(input, "label");
        let reopened = input.get("reopen") == Some(&true.into());
        let folder = Directory::new();
        let path = if reopened {
            folder.0.join("secretary.sqlite")
        } else {
            ":memory:".into()
        };
        let mut journal = Journal::open(&path, Limits::default()).unwrap();
        seed(&journal, input);
        for d in input.required("core").unwrap().as_array().unwrap() {
            let value = canonical(d.required("value").unwrap(), 32 * 1024 * 1024).unwrap();
            let revision = d.required("revision").unwrap().as_f64().unwrap() as i64;
            journal
                .db
                .execute(
                    "INSERT INTO documents VALUES(?,?,?,?,?,?,?)",
                    params![
                        text(d, "id"),
                        text(d, "key"),
                        text(d, "name"),
                        revision,
                        0,
                        Option::<i64>::None,
                        value
                    ],
                )
                .unwrap();
            journal
                .db
                .execute(
                    "INSERT INTO revisions VALUES(?,?,?)",
                    params![text(d, "id"), revision, value],
                )
                .unwrap();
        }
        let client = Arc::new(Yielding(
            Scripted {
                steps: Mutex::new(
                    input
                        .required("steps")
                        .unwrap()
                        .as_array()
                        .unwrap()
                        .iter()
                        .cloned()
                        .collect(),
                ),
                trace: Mutex::new(vec![]),
            },
            label.clone(),
            input
                .required("documents")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|d| {
                    d.get("value")
                        .and_then(|v| v.get("operationId"))
                        .and_then(IvyJson::as_js_string)
                        .cloned()
                })
                .collect(),
        ));
        let tools = Arc::new(ToolClients::new(client.clone()));
        let clock = Fixed {
            value: corpus.required("time").unwrap().as_f64().unwrap() as i64,
            step: input
                .get("clockStep")
                .and_then(IvyJson::as_f64)
                .unwrap_or(0.0) as i64,
            calls: AtomicUsize::new(0),
        };
        let wall = Fixed {
            value: corpus.required("wall").unwrap().as_f64().unwrap() as i64,
            step: 0,
            calls: AtomicUsize::new(0),
        };
        let issues = RefCell::new(RecoveryScan::default());
        if let Some(saved) = input.get("issues").and_then(IvyJson::as_array) {
            for issue in saved {
                let values = issue.as_array().unwrap();
                issues
                    .borrow_mut()
                    .issue(&values[0].as_str().unwrap(), &values[1].as_str().unwrap());
            }
        }
        let attempts = input.required("attempts").unwrap().as_f64().unwrap() as usize;
        let mut results = Vec::new();
        for segment in 0..if reopened { attempts } else { 1 } {
            if segment > 0 {
                drop(journal);
                journal = Journal::open(&path, Limits::default()).unwrap();
                physical_reopens += 1;
            }
            let store = Store {
                client: client.clone(),
                root: corpus
                    .required("rootId")
                    .unwrap()
                    .as_js_string()
                    .unwrap()
                    .clone(),
                journal: &journal,
            };
            let evidence = InboxEvidence {
                store: &store,
                settings: input.required("settings").unwrap(),
                service_node_id: corpus.required("nodeId").unwrap().as_js_string().unwrap(),
                cancellation: RequestOptions::default(),
            };
            let engine = SecretaryEngine {
                evidence: &evidence,
                clock: &clock,
                operation_clock: &wall,
                runtime_epoch: epoch_fixture(&"fixture-epoch".into()),
                new_uuid: ivy_secretary::engine::random_uuid,
                tools: &tools,
                issues: &issues,
            };
            let worker = TriageNative::new(
                &engine,
                input.required("configuration").unwrap(),
                tools.clone(),
                corpus
                    .required("notificationPolicy")
                    .unwrap()
                    .as_js_string()
                    .unwrap()
                    .clone(),
                None,
            )
            .map(|native| SecretaryTriage::new(native, &contracts));
            for _ in 0..if reopened { 1 } else { attempts } {
                let outcome = async {
                    let worker = worker.as_ref().map_err(Clone::clone)?;
                    let native = &worker.native;
                    let mode = text(input, "mode");
                    if mode == "constructor" {
                        return Ok(IvyJson::Null);
                    }
                    if mode == "work" {
                        return Ok(native.work(&selected)?.map_or(IvyJson::Null, |d| d.value()));
                    }
                    if mode == "prepare" {
                        return Ok(native.prepare(&selected).await?.value());
                    }
                    let work = native.work(&selected)?.unwrap();
                    let plan = native.plan(&work)?;
                    Ok(match mode.as_str() {
                        "plan" => plan.value(),
                        "begin" => native.begin_turn(&plan).await?.map_or(IvyJson::Null, |s| {
                            IvyJson::object([
                                ("turn", s.turn.value()),
                                ("threadId", IvyJson::String(s.thread_id)),
                                ("epoch", IvyJson::String(s.epoch)),
                            ])
                        }),
                        "release" => native.release_closed(&plan).await?.into(),
                        "completion" => {
                            let call = native
                                .call(
                                    &plan,
                                    &"turn".into(),
                                    &"turn/start".into(),
                                    Some(&"original-thread".into()),
                                    false,
                                )
                                .await?;
                            TriageCompletion {
                                native,
                                plan: &plan,
                                retained: &contracts,
                            }
                            .collect(
                                &call,
                                &"original-thread".into(),
                                &input
                                    .get("epoch")
                                    .and_then(IvyJson::as_js_string)
                                    .cloned()
                                    .unwrap_or_else(|| "epoch".into()),
                            )
                            .await?
                            .map_or(IvyJson::Null, |d| d.value())
                        }
                        "step" => worker.step(&selected).await?.into(),
                        "tick" => {
                            if input.get("concurrent") == Some(&true.into()) {
                                let (first, second) = tokio::join!(worker.tick(), worker.tick());
                                first?;
                                second?;
                                IvyJson::Array(vec![IvyJson::Null, IvyJson::Null])
                            } else {
                                worker.tick().await?;
                                IvyJson::Null
                            }
                        }
                        "advance" => {
                            let call = native
                                .call(&plan, &"thread".into(), &"thread/start".into(), None, false)
                                .await?;
                            native.advance(&plan, &call).await?.value()
                        }
                        _ => panic!("Unknown triage mode {mode}"),
                    })
                }
                .await;
                results.push(result(outcome));
            }
        }
        let (retained, _) = retained(&journal);
        let core = journal
            .db
            .prepare("SELECT id,key,name,current_revision,value_json,completed_at FROM documents")
            .unwrap()
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                ))
            })
            .unwrap()
            .map(|r| {
                let (id, key, name, revision, value, completed) = r.unwrap();
                IvyJson::object([
                    ("id", id.into()),
                    ("key", key.into()),
                    ("name", name.into()),
                    ("revision", IvyJson::Number(revision as f64)),
                    ("value", parse(&value, 32 * 1024 * 1024).unwrap()),
                    (
                        "completedAt",
                        completed.map_or(IvyJson::Null, |n| IvyJson::Number(n as f64)),
                    ),
                ])
            })
            .collect();
        let cleanup = journal
            .db
            .prepare("SELECT root_id,delete_after FROM technical_cleanup")
            .unwrap()
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .unwrap()
            .map(|r| {
                let (id, at) = r.unwrap();
                IvyJson::object([
                    ("id", id.into()),
                    ("deleteAfter", IvyJson::Number(at as f64)),
                ])
            })
            .collect();
        let state = IvyJson::object([
            (
                "clockBounds",
                IvyJson::Array(vec![
                    IvyJson::Number(started_at as f64),
                    IvyJson::Number(SystemClock.utc_millis() as f64),
                ]),
            ),
            ("results", IvyJson::Array(results)),
            (
                "trace",
                IvyJson::Array(client.0.trace.lock().unwrap().clone()),
            ),
            (
                "issues",
                IvyJson::Array(
                    issues
                        .borrow()
                        .issues
                        .iter()
                        .map(|(id, code)| {
                            IvyJson::Array(vec![id.clone().into(), code.clone().into()])
                        })
                        .collect(),
                ),
            ),
            ("retained", retained),
            ("core", IvyJson::Array(core)),
            ("cleanup", IvyJson::Array(cleanup)),
        ]);
        actual_rows.push(IvyJson::object([
            ("input", input.clone()),
            ("state", state),
        ]));
    }
    let mut values = Vec::new();
    for batch in actual_rows.chunks(8) {
        let mut command = node("tests/secretary-rust/triage-state.mjs");
        let mut child = command
            .arg("--normalize")
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
                canonical(&IvyJson::Array(batch.to_vec()), 32 * 1024 * 1024)
                    .unwrap()
                    .as_bytes(),
            )
            .unwrap();
        let normalized = child.wait_with_output().unwrap();
        assert!(
            normalized.status.success(),
            "{}",
            String::from_utf8_lossy(&normalized.stderr)
        );
        let normalized = parse(
            std::str::from_utf8(&normalized.stdout).unwrap(),
            64 * 1024 * 1024,
        )
        .unwrap();
        values.extend_from_slice(normalized.as_array().unwrap());
    }
    assert_eq!(rows.len(), values.len());
    for (row, actual) in rows.iter().zip(&values) {
        let label = text(row.required("input").unwrap(), "label");
        for field in ["results", "trace", "retained", "issues", "core", "cleanup"] {
            assert_eq!(
                canonical(actual.required(field).unwrap(), 32 * 1024 * 1024).unwrap(),
                canonical(
                    row.required("expected").unwrap().required(field).unwrap(),
                    32 * 1024 * 1024
                )
                .unwrap(),
                "{label} / {field}"
            );
        }
    }
    println!(
        "triage workflow original comparisons: {} cases / {} attempts / {} physical reopens",
        rows.len(),
        rows.iter()
            .map(|r| r
                .required("input")
                .unwrap()
                .required("attempts")
                .unwrap()
                .as_f64()
                .unwrap() as usize)
            .sum::<usize>(),
        physical_reopens
    );
}
