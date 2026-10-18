use super::*;
use ivy_contracts::{hash::hash_json, json::canonical};
use ivy_secretary::{
    engine::SecretaryEngine,
    inbox_evidence::InboxEvidence,
    inbox_recovery::{InboxRecovery, RecoveryScan},
    journal::{Journal, Limits},
    store::Store,
};
use ivy_store::rusqlite::params;
use std::{
    cell::RefCell,
    ops::Deref,
    sync::atomic::{AtomicUsize, Ordering},
};
use tokio::sync::Notify;

struct RecoveryClient {
    scripted: Scripted,
    independent_issue: bool,
    calls: AtomicUsize,
    entered: Notify,
    released: Notify,
}
impl Deref for RecoveryClient {
    type Target = Scripted;
    fn deref(&self) -> &Scripted {
        &self.scripted
    }
}
impl RpcClient for RecoveryClient {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            if self.independent_issue && self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
                self.entered.notify_one();
                self.released.notified().await;
            }
            self.scripted.request(method, params, options).await
        })
    }
}

#[tokio::test]
async fn original_inbox_effect_recovery_and_bounded_scan() {
    let mut command = std::process::Command::new("node");
    command
        .arg("tests/secretary-rust/inbox-recovery-oracle.mjs")
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
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
    assert!(rows.len() >= 60);
    for (index, row) in rows.iter().enumerate() {
        let input = row.required("input").unwrap();
        let client = Arc::new(RecoveryClient {
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
                trace: Mutex::new(Vec::new()),
            },
            independent_issue: input.get("independentIssue") == Some(&IvyJson::Bool(true)),
            calls: AtomicUsize::new(0),
            entered: Notify::new(),
            released: Notify::new(),
        });
        let journal = Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
        for local in input.required("locals").unwrap().as_array().unwrap() {
            let id = local.required("id").unwrap().as_str().unwrap();
            let key = local.required("key").unwrap().as_str().unwrap();
            let value = local.required("value").unwrap();
            let first = local.required("first").unwrap();
            let revision = if hash_json(first).unwrap() == hash_json(value).unwrap() {
                1
            } else {
                2
            };
            let completed =
                if key == "secretary/operation" && value.get("phase") != Some(&"accepted".into()) {
                    Some(1i64)
                } else {
                    None
                };
            let encoded = canonical(value, 32 * 1024 * 1024).unwrap();
            journal
                .db
                .execute(
                    "INSERT INTO documents VALUES(?,?,?,?,?,?,?)",
                    params![
                        id,
                        key,
                        local.required("name").unwrap().as_str().unwrap(),
                        revision,
                        0,
                        completed,
                        encoded
                    ],
                )
                .unwrap();
            journal
                .db
                .execute(
                    "INSERT INTO revisions VALUES(?,?,?)",
                    params![id, 1, canonical(first, 32 * 1024 * 1024).unwrap()],
                )
                .unwrap();
            if revision == 2 {
                journal
                    .db
                    .execute(
                        "INSERT INTO revisions VALUES(?,?,?)",
                        params![id, 2, encoded],
                    )
                    .unwrap();
            }
        }
        let store = Store {
            client: client.clone(),
            root: corpus
                .required("root")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
            journal: &journal,
        };
        let cancellation = RequestOptions {
            cancel_error: IvyError::new("fixture_cancelled", "Fixture cancelled."),
            ..Default::default()
        };
        if input.get("cancelled").and_then(IvyJson::as_bool) == Some(true) {
            cancellation.cancellation.cancel();
        }
        let node = "fixture.secretary".into();
        let evidence = InboxEvidence {
            store: &store,
            settings: input.required("settings").unwrap(),
            service_node_id: &node,
            cancellation,
        };
        let recovery = InboxRecovery {
            evidence: &evidence,
        };
        let scan = RefCell::new(RecoveryScan {
            cursor: input
                .get("cursor")
                .filter(|v| !v.is_null())
                .map(|v| v.as_str().unwrap()),
            issues: input
                .required("issues")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|v| {
                    let a = v.as_array().unwrap();
                    (a[0].as_str().unwrap(), a[1].as_str().unwrap())
                })
                .collect(),
        });
        let clock = ivy_sdk::clock::SystemClock;
        let tools = ToolClients::new(client.clone());
        let runtime_epoch = epoch_fixture(&"fixture-epoch".into());
        let engine = SecretaryEngine {
            evidence: &evidence,
            clock: &clock,
            operation_clock: &clock,
            runtime_epoch,
            new_uuid: ivy_secretary::engine::random_uuid,
            tools: &tools,
            issues: &scan,
        };
        let work = async {
            let mut results = Vec::new();
            for _ in 0..input.required("repeats").unwrap().as_f64().unwrap() as usize {
                let result = if input.get("method") == Some(&"tick".into()) {
                    engine.tick().await.map(|_| IvyJson::Null)
                } else {
                    recovery
                        .apply(&IvyJson::object([(
                            "pin",
                            IvyJson::object([
                                ("objectId", "00000000-0000-4000-8000-000000000004".into()),
                                ("revision", IvyJson::Number(1.0)),
                            ]),
                        )]))
                        .await
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
            results
        };
        let observer = async {
            if client.independent_issue {
                client.entered.notified().await;
                assert_eq!(
                    scan.borrow().cursor.as_deref(),
                    Some("00000000-0000-4000-8000-000000000004")
                );
                scan.borrow_mut().issue("independent", "background_issue");
                client.released.notify_one();
            }
        };
        let (results, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(work, observer)
        })
        .await
        .expect("Recovery RPC gate budget");
        let scan = scan.into_inner();
        let retained = journal
            .db
            .prepare(
                "SELECT id,key,current_revision,value_json,completed_at FROM documents ORDER BY id",
            )
            .unwrap()
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                ))
            })
            .unwrap()
            .map(|r| {
                let (id, key, revision, value, completed) = r.unwrap();
                IvyJson::object([
                    ("id", id.into()),
                    ("key", key.into()),
                    ("revision", IvyJson::Number(revision as f64)),
                    ("value", parse(&value, 32 * 1024 * 1024).unwrap()),
                    ("completed", completed.is_some().into()),
                ])
            })
            .collect();
        let actual = IvyJson::object([
            ("results", IvyJson::Array(results)),
            (
                "trace",
                IvyJson::Array(client.trace.lock().unwrap().clone()),
            ),
            ("retained", IvyJson::Array(retained)),
            (
                "cursor",
                scan.cursor.map(IvyJson::from).unwrap_or(IvyJson::Null),
            ),
            (
                "issues",
                IvyJson::Array(
                    scan.issues
                        .into_iter()
                        .map(|(id, code)| IvyJson::Array(vec![id.into(), code.into()]))
                        .collect(),
                ),
            ),
        ]);
        assert!(
            client.steps.lock().unwrap().is_empty(),
            "unconsumed RPC steps at case {index}"
        );
        assert_eq!(
            &actual,
            row.required("expected").unwrap(),
            "recovery case {index}: {}",
            input.required("label").unwrap().as_str().unwrap()
        );
    }
}
