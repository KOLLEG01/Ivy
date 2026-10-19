use super::microsoft_store_cases::{normalize, retained, seed};
use super::*;
use ivy_contracts::{JsString, json::canonical};
use ivy_sdk::clock::Clock;
use ivy_secretary::{
    engine::SecretaryEngine,
    inbox_evidence::InboxEvidence,
    inbox_recovery::RecoveryScan,
    journal::{Journal, Limits},
    outlook_source::OutlookSource,
    store::Store,
};
use ivy_store::rusqlite::params;
use std::{
    cell::RefCell,
    path::Path,
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
};

struct Fixed(i64);
impl Clock for Fixed {
    fn utc_millis(&self) -> i64 {
        self.0
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
struct Yielding {
    scripted: Scripted,
    preparing: AtomicBool,
    epoch: JsString,
    label: String,
}
impl RpcClient for Yielding {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            tokio::task::yield_now().await;
            if self.preparing.load(Ordering::Relaxed) {
                assert_eq!(method, "system.status");
                return Ok(IvyJson::object([(
                    "runtimeEpoch",
                    IvyJson::String(self.epoch.clone()),
                )]));
            }
            assert_eq!(
                self.scripted
                    .steps
                    .lock()
                    .unwrap()
                    .front()
                    .unwrap()
                    .get("method"),
                Some(&method.into()),
                "RPC order in {} after {} calls",
                self.label,
                self.scripted.trace.lock().unwrap().len()
            );
            self.scripted.request(method, params, options).await
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
fn core(journal: &Journal) -> IvyJson {
    let mut query=journal.db.prepare("SELECT id,key,name,current_revision,value_json,completed_at FROM documents ORDER BY key,name").unwrap();
    let rows = query
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<i64>>(5)?,
            ))
        })
        .unwrap();
    IvyJson::Array(
        rows.map(|row| {
            let (id, key, name, revision, value, completed) = row.unwrap();
            let mut query = journal
                .db
                .prepare("SELECT revision,value_json FROM revisions WHERE id=? ORDER BY revision")
                .unwrap();
            let history = query
                .query_map([&id], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .unwrap()
                .map(|row| {
                    let (revision, value) = row.unwrap();
                    IvyJson::object([
                        ("revision", IvyJson::Number(revision as f64)),
                        ("value", parse(&value, 32 * 1024 * 1024).unwrap()),
                    ])
                })
                .collect();
            IvyJson::object([
                ("id", id.into()),
                ("key", key.into()),
                ("name", name.into()),
                ("revision", IvyJson::Number(revision as f64)),
                ("value", parse(&value, 32 * 1024 * 1024).unwrap()),
                ("completed", completed.is_some().into()),
                ("history", IvyJson::Array(history)),
            ])
        })
        .collect(),
    )
}

#[tokio::test]
async fn original_outlook_source_captures_checkpoint_recovery_and_ack_order() {
    let mut command = Command::new("node");
    command
        .arg("tests/secretary-rust/outlook-source-oracle.mjs")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command
        .output()
        .expect("Original Outlook source oracle required");
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
    assert_eq!(rows.len(), 32);
    for row in rows {
        let input = row.required("input").unwrap();
        let label = input.required("label").unwrap().as_str().unwrap();
        let journal = Journal::open(Path::new(":memory:"), Limits::default()).unwrap();
        let mut seeded = input.clone();
        let documents = input
            .required("documents")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let mut value = row.clone();
                value.insert(
                    "revisions",
                    IvyJson::Array(
                        input
                            .required("revisions")
                            .unwrap()
                            .as_array()
                            .unwrap()
                            .iter()
                            .filter(|revision| revision.get("id") == row.get("id"))
                            .map(|revision| {
                                IvyJson::object([
                                    ("revision", revision.required("revision").unwrap().clone()),
                                    ("value", revision.required("value").unwrap().clone()),
                                ])
                            })
                            .collect(),
                    ),
                );
                value
            })
            .collect();
        seeded.insert("documents", IvyJson::Array(documents));
        seed(&journal, &seeded);
        for row in input.required("core").unwrap().as_array().unwrap() {
            let text = |key| row.required(key).unwrap().as_str().unwrap();
            journal
                .db
                .execute(
                    "INSERT INTO documents VALUES(?,?,?,?,?,?,?)",
                    params![
                        text("id"),
                        text("key"),
                        text("name"),
                        row.required("revision").unwrap().as_f64().unwrap() as i64,
                        0,
                        if row.get("completed") == Some(&true.into()) {
                            Some(1i64)
                        } else {
                            None
                        },
                        canonical(row.required("value").unwrap(), 32 * 1024 * 1024).unwrap()
                    ],
                )
                .unwrap();
            for revision in row.required("history").unwrap().as_array().unwrap() {
                journal
                    .db
                    .execute(
                        "INSERT INTO revisions VALUES(?,?,?)",
                        params![
                            text("id"),
                            revision.required("revision").unwrap().as_f64().unwrap() as i64,
                            canonical(revision.required("value").unwrap(), 32 * 1024 * 1024)
                                .unwrap()
                        ],
                    )
                    .unwrap();
            }
        }
        for id in input
            .required("acknowledgements")
            .unwrap()
            .as_array()
            .unwrap()
        {
            journal
                .db
                .execute(
                    "INSERT INTO capture_acknowledgements VALUES(?)",
                    [id.as_str().unwrap()],
                )
                .unwrap();
        }
        let client = Arc::new(Yielding {
            preparing: AtomicBool::new(true),
            epoch: input
                .required("runtimeEpoch")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
            label: label.clone(),
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
        });
        // The retained prefix ran against this same live client in the original.
        // Restore only its evidenced successful epoch lookup, outside the compared suffix.
        if input
            .required("preparation")
            .unwrap()
            .required("epochLookups")
            .unwrap()
            .as_f64()
            .unwrap()
            > 0.0
        {
            ivy_sdk::operation_ids::runtime_epoch(client.clone())
                .await
                .unwrap();
        }
        client.preparing.store(false, Ordering::Relaxed);
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
        let clock = Fixed(corpus.required("time").unwrap().as_f64().unwrap() as i64);
        let wall = Fixed(corpus.required("wall").unwrap().as_f64().unwrap() as i64);
        let tools = Arc::new(ToolClients::new(client.clone()));
        let issues = RefCell::new(RecoveryScan::default());
        let engine = SecretaryEngine {
            evidence: &evidence,
            clock: &clock,
            operation_clock: &wall,
            runtime_epoch: epoch_fixture(
                input
                    .required("runtimeEpoch")
                    .unwrap()
                    .as_js_string()
                    .unwrap(),
            ),
            new_uuid: || "00000000-0000-4000-8000-000000000fff".into(),
            tools: &tools,
            issues: &issues,
        };
        let mut source = OutlookSource::new(
            &engine,
            input.required("configuration").unwrap(),
            tools.clone(),
        )
        .unwrap();
        let mut results = vec![];
        for _ in 0..input.required("attempts").unwrap().as_f64().unwrap() as usize {
            if input.get("restart") == Some(&true.into()) {
                source = OutlookSource::new(
                    &engine,
                    input.required("configuration").unwrap(),
                    tools.clone(),
                )
                .unwrap();
            }
            let outcome = async {
                if input.get("mode") == Some(&"head".into()) {
                    source.head().await.map(|document| document.value())
                } else {
                    source.step().await.map(IvyJson::from)
                }
            }
            .await;
            results.push(result(outcome));
        }
        let (mut retained, mut ids) = retained(&journal);
        let core = core(&journal);
        for row in core.as_array().unwrap() {
            ids.insert(
                row.required("id").unwrap().as_js_string().unwrap().clone(),
                format!(
                    "core:{}:{}",
                    row.required("key").unwrap().as_str().unwrap(),
                    row.required("name").unwrap().as_str().unwrap()
                )
                .into(),
            );
        }
        retained.insert("core", core);
        let mut query = journal
            .db
            .prepare("SELECT capture_id FROM capture_acknowledgements")
            .unwrap();
        let mut acknowledgements = query
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|row| normalize(&row.unwrap().into(), &ids))
            .collect::<Vec<_>>();
        acknowledgements.sort_by_key(|value| value.as_str().unwrap());
        retained.insert("acknowledgements", IvyJson::Array(acknowledgements));
        let actual = normalize(
            &IvyJson::object([
                ("results", IvyJson::Array(results)),
                (
                    "trace",
                    IvyJson::Array(client.scripted.trace.lock().unwrap().clone()),
                ),
                ("retained", retained),
            ]),
            &ids,
        );
        let expected = row.required("expected").unwrap();
        for field in ["results", "trace", "retained"] {
            assert_eq!(
                canonical(actual.required(field).unwrap(), 32 * 1024 * 1024).unwrap(),
                canonical(expected.required(field).unwrap(), 32 * 1024 * 1024).unwrap(),
                "{label} / {field}"
            );
        }
        assert!(
            client.scripted.steps.lock().unwrap().is_empty(),
            "Unconsumed original source RPC in {label}"
        );
    }
    println!(
        "outlook-source original comparisons: {} cases / {} attempts",
        rows.len(),
        rows.iter()
            .map(|row| row
                .required("input")
                .unwrap()
                .required("attempts")
                .unwrap()
                .as_f64()
                .unwrap() as usize)
            .sum::<usize>()
    );
}
