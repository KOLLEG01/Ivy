use super::*;
use ivy_contracts::{JsString, checked_native_contract::CheckedNativeContracts, json::canonical};
use ivy_sdk::clock::Clock;
use ivy_secretary::{
    engine::SecretaryEngine,
    follow_up::{SecretaryFollowUp, follow_up_prompt},
    follow_up_import::import_pending_context,
    inbox_evidence::InboxEvidence,
    inbox_recovery::RecoveryScan,
    journal::{Journal, Limits},
    store::Store,
};
use ivy_store::rusqlite::params;
use std::{
    cell::RefCell,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

struct FollowUpClient {
    scripted: Scripted,
    cancellation: RequestOptions,
    cancel_after: Option<usize>,
    concurrent: Option<(usize, PathBuf, String)>,
    calls: AtomicUsize,
}
impl RpcClient for FollowUpClient {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let result = self.scripted.request(method, params, options).await;
            let count = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
            if let Some((at, path, name)) = &self.concurrent {
                if *at == count {
                    let other = Journal::open(path, Limits::default())?;
                    let saved = other.named("secretary/follow-up", name, true)?.unwrap();
                    let mut next = saved.value.clone();
                    next.insert("errorCode", "concurrent-change".into());
                    other.amend_technical("secretary/follow-up", &saved.pin, &next)?;
                }
            }
            if self.cancel_after == Some(count) {
                self.cancellation.cancellation.cancel();
            }
            result
        })
    }
}
struct FixedClock {
    time: i64,
    step: i64,
    calls: AtomicUsize,
}
impl Clock for FixedClock {
    fn utc_millis(&self) -> i64 {
        self.time + self.calls.fetch_add(1, Ordering::Relaxed) as i64 * self.step
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
fn js_text(value: &IvyJson) -> JsString {
    value.as_js_string().unwrap().clone()
}
fn sql_text(value: &IvyJson) -> String {
    String::from_utf16_lossy(&js_text(value).0)
}
struct TemporaryJournal(PathBuf);
impl Drop for TemporaryJournal {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}
#[tokio::test]
async fn original_follow_up_contexts_calls_recovery_and_authorization() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut command = std::process::Command::new("node");
    command
        .arg("tests/secretary-rust/follow-up-oracle.mjs")
        .current_dir(&root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().expect("Original Node oracle required");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let corpus = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        32 * 1024 * 1024,
    )
    .unwrap();
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 106);
    for row in rows {
        let input = row.required("input").unwrap();
        let label = sql_text(input.required("label").unwrap());
        let temporary = TemporaryJournal(std::env::temp_dir().join(format!(
            "secretary-follow-up-{}.sqlite",
            uuid::Uuid::new_v4()
        )));
        let concurrent_at = input
            .get("concurrentAt")
            .and_then(IvyJson::as_f64)
            .map(|n| n as usize);
        let path = if concurrent_at.is_some() {
            temporary.0.as_path()
        } else {
            std::path::Path::new(":memory:")
        };
        let journal = Journal::open(path, Limits::default()).unwrap();
        for (array, technical) in [("documents", true), ("locals", false)] {
            for d in input.required(array).unwrap().as_array().unwrap() {
                let id = d.required("id").unwrap().as_str().unwrap();
                let key = d.required("key").unwrap().as_str().unwrap();
                let name = sql_text(d.required("name").unwrap());
                let encoded = canonical(d.required("value").unwrap(), 1_048_576).unwrap();
                if technical {
                    journal
                        .db
                        .execute(
                            "INSERT INTO technical_documents VALUES(?,?,?,?,?)",
                            params![id, key, name, 1, encoded],
                        )
                        .unwrap();
                    journal
                        .db
                        .execute(
                            "INSERT INTO technical_revisions VALUES(?,?,?)",
                            params![id, 1, encoded],
                        )
                        .unwrap();
                } else {
                    journal
                        .db
                        .execute(
                            "INSERT INTO documents VALUES(?,?,?,?,?,?,?)",
                            params![id, key, name, 1, 0, Option::<i64>::None, encoded],
                        )
                        .unwrap();
                    journal
                        .db
                        .execute(
                            "INSERT INTO revisions VALUES(?,?,?)",
                            params![id, 1, encoded],
                        )
                        .unwrap();
                }
            }
        }
        let expired = input.required("expiredBefore").unwrap().as_f64().unwrap();
        if expired != 0.0 {
            journal
                .db
                .execute(
                    "INSERT INTO journal_state VALUES(?,?)",
                    params!["expiredBefore", format!("{expired:.0}")],
                )
                .unwrap();
        }
        let cancellation = RequestOptions {
            cancel_error: IvyError::new("fixture_cancelled", "Fixture cancelled."),
            ..RequestOptions::default()
        };
        if input.get("cancel") == Some(&true.into()) {
            cancellation.cancellation.cancel();
        }
        let caller = js_text(input.required("caller").unwrap());
        let request = input.required("request").unwrap();
        let name = format!(
            "follow-up-{}-{}",
            sql_text(input.required("caller").unwrap()),
            sql_text(request.required("operationId").unwrap())
        );
        let client = Arc::new(FollowUpClient {
            scripted: Scripted {
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
                trace: Mutex::default(),
            },
            cancellation: cancellation.clone(),
            cancel_after: input
                .get("cancelAfter")
                .and_then(IvyJson::as_f64)
                .map(|n| n as usize),
            concurrent: concurrent_at.map(|at| (at, temporary.0.clone(), name.clone())),
            calls: AtomicUsize::new(0),
        });
        let store = Store {
            client: client.clone(),
            root: js_text(corpus.required("rootId").unwrap()),
            journal: &journal,
        };
        let node = "fixture.secretary".into();
        let evidence = InboxEvidence {
            store: &store,
            settings: input.required("settings").unwrap(),
            service_node_id: &node,
            cancellation,
        };
        let tools = Arc::new(ToolClients::new(client.clone()));
        let clock = FixedClock {
            time: input.required("time").unwrap().as_f64().unwrap() as i64,
            step: input.required("clockStep").unwrap().as_f64().unwrap() as i64,
            calls: AtomicUsize::new(0),
        };
        let wall = FixedClock {
            time: input.required("wall").unwrap().as_f64().unwrap() as i64,
            step: 0,
            calls: AtomicUsize::new(0),
        };
        let issues = RefCell::new(RecoveryScan {
            issues: input
                .required("issues")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|p| {
                    let p = p.as_array().unwrap();
                    (sql_text(&p[0]), sql_text(&p[1]))
                })
                .collect(),
            ..RecoveryScan::default()
        });
        let engine = SecretaryEngine {
            evidence: &evidence,
            clock: &clock,
            operation_clock: &wall,
            runtime_epoch: epoch_fixture(corpus.required("epoch").unwrap().as_js_string().unwrap()),
            new_uuid: ivy_secretary::engine::random_uuid,
            tools: &tools,
            issues: &issues,
        };
        let retained = CheckedNativeContracts::new(root.clone());
        let worker = SecretaryFollowUp::new(&engine, tools.clone(), &retained);
        let mut results = Vec::new();
        for action in input.required("actions").unwrap().as_array().unwrap() {
            let args = action.get("request").unwrap_or(request);
            let who = action
                .get("caller")
                .map(js_text)
                .unwrap_or_else(|| caller.clone());
            let operation_id = action
                .get("operationId")
                .unwrap_or(args.required("operationId").unwrap());
            let selected_name = action
                .get("name")
                .map(sql_text)
                .unwrap_or_else(|| name.clone());
            let result = match action
                .required("method")
                .unwrap()
                .as_str()
                .unwrap()
                .as_str()
            {
                "run" => worker.run(args, &who).await,
                "read" => worker.read(&js_text(operation_id), &who).await,
                "advance" => worker.advance(&selected_name).await.map(|_| IvyJson::Null),
                "importAndAdvance" => {
                    let imported = import_pending_context(&worker, &selected_name)
                        .await
                        .unwrap()
                        .expect("Legacy copied context must be converted");
                    assert_eq!(imported.before.pin.object_id, imported.after.pin.object_id);
                    assert_eq!(
                        journal
                            .read(
                                "secretary/follow-up-context",
                                &imported.before.pin.object_id,
                                Some(imported.before.pin.revision),
                                true
                            )
                            .unwrap()
                            .value,
                        imported.before.value,
                        "Explicit import retains the original context revision"
                    );
                    assert_eq!(
                        imported.after.pin.revision,
                        imported.before.pin.revision + 1
                    );
                    let mut original = imported.after.value.clone();
                    original.remove("start");
                    original.remove("createdFor");
                    assert_eq!(
                        original, imported.before.value,
                        "Import must retain all original fields"
                    );
                    let trace_before = client.scripted.trace.lock().unwrap().len();
                    assert!(
                        import_pending_context(&worker, &selected_name)
                            .await
                            .unwrap()
                            .is_none()
                    );
                    assert_eq!(
                        client.scripted.trace.lock().unwrap().len(),
                        trace_before,
                        "Explicit import is idempotent without a second binding request"
                    );
                    worker.advance(&selected_name).await.map(|_| IvyJson::Null)
                }
                "tick" => worker.tick().await.map(|_| IvyJson::Null),
                "prompt" => Ok(IvyJson::String(follow_up_prompt(
                    &js_text(args.required("itemId").unwrap()),
                    &js_text(args.required("question").unwrap()),
                ))),
                method => panic!("Unexpected {method}"),
            };
            results.push(match result {
                Ok(value) => IvyJson::object([("value", value)]),
                Err(e) => IvyJson::object([
                    ("code", e.code.into()),
                    ("message", e.message.into()),
                    ("outcome", e.outcome.as_str().into()),
                ]),
            });
        }
        let mut statement = journal.db.prepare("SELECT key,name,current_revision,value_json FROM technical_documents ORDER BY key,name").unwrap();
        let remaining = statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .unwrap()
            .map(|r| {
                let (key, name, revision, value) = r.unwrap();
                IvyJson::object([
                    ("key", key.into()),
                    ("name", name.into()),
                    ("revision", IvyJson::Number(revision as f64)),
                    ("value", parse(&value, 1_048_576).unwrap()),
                ])
            })
            .collect();
        let cleanup = journal.db.prepare("SELECT d.key,d.name FROM technical_cleanup c JOIN technical_documents d ON d.id=c.root_id ORDER BY d.key,d.name").unwrap().query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).unwrap().map(|r| { let (key,name) = r.unwrap(); IvyJson::object([("key",key.into()),("name",name.into())]) }).collect();
        let diagnostics = issues
            .borrow()
            .issues
            .iter()
            .map(|(key, code)| IvyJson::Array(vec![key.clone().into(), code.clone().into()]))
            .collect();
        let actual = IvyJson::object([
            ("results", IvyJson::Array(results)),
            (
                "trace",
                IvyJson::Array(client.scripted.trace.lock().unwrap().clone()),
            ),
            ("remaining", IvyJson::Array(remaining)),
            ("cleanup", IvyJson::Array(cleanup)),
            ("issues", IvyJson::Array(diagnostics)),
            ("cursor", worker.cursor().into()),
            (
                "clockCalls",
                IvyJson::Number(clock.calls.load(Ordering::Relaxed) as f64),
            ),
        ]);
        assert!(
            client.scripted.steps.lock().unwrap().is_empty(),
            "Unconsumed RPCs: {label}"
        );
        assert_eq!(
            canonical(&actual, 32 * 1024 * 1024).unwrap(),
            canonical(row.required("expected").unwrap(), 32 * 1024 * 1024).unwrap(),
            "{label}"
        );
    }
}
