use super::microsoft_store_cases::{normalize, retained, seed};
use super::*;
use ivy_contracts::{json::canonical, operation::parse_operation_id_js};
use ivy_sdk::clock::Clock;
use ivy_secretary::{
    engine::SecretaryEngine,
    inbox_evidence::InboxEvidence,
    inbox_recovery::RecoveryScan,
    journal::{Journal, Limits},
    outlook_native::OutlookNative,
    store::Store,
};
use ivy_store::rusqlite::params;
use std::{cell::RefCell, path::Path, process::Command};

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
                ("outcome", error.outcome.to_json()),
            ]);
            if let Some(details) = error.details {
                value.insert("details", details);
            }
            value
        }
    }
}
#[tokio::test]
async fn original_outlook_native_account_gates_reconciliation_and_cleanup() {
    let mut command = Command::new("node");
    command
        .arg("tests/secretary-rust/outlook-native-oracle.mjs")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command
        .output()
        .expect("Original Outlook native oracle required");
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
    assert_eq!(rows.len(), 59);
    for row in rows {
        let input = row.required("input").unwrap();
        let label = input.required("label").unwrap().as_str().unwrap();
        let journal = Journal::open(Path::new(":memory:"), Limits::default()).unwrap();
        seed(&journal, input);
        for row in input.required("core").unwrap().as_array().unwrap() {
            let encoded = canonical(row.required("value").unwrap(), 32 * 1024 * 1024).unwrap();
            let text = |key| row.required(key).unwrap().as_str().unwrap();
            let revision = row.required("revision").unwrap().as_f64().unwrap() as i64;
            journal
                .db
                .execute(
                    "INSERT INTO documents VALUES(?,?,?,?,?,?,?)",
                    params![
                        text("id"),
                        text("key"),
                        text("name"),
                        revision,
                        0,
                        Option::<i64>::None,
                        encoded
                    ],
                )
                .unwrap();
            journal
                .db
                .execute(
                    "INSERT INTO revisions VALUES(?,?,?)",
                    params![text("id"), revision, encoded],
                )
                .unwrap();
        }
        let client = Arc::new(Yielding {
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
                trace: Mutex::new(vec![]),
            },
        });
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
        let operation_clock = Fixed(corpus.required("wall").unwrap().as_f64().unwrap() as i64);
        let tools = Arc::new(ToolClients::new(client.clone()));
        let issues = RefCell::new(RecoveryScan::default());
        let engine = SecretaryEngine {
            evidence: &evidence,
            clock: &clock,
            operation_clock: &operation_clock,
            runtime_epoch: epoch_fixture(&"fixture-epoch".into()),
            new_uuid: || "00000000-0000-4000-8000-000000000fff".into(),
            tools: &tools,
            issues: &issues,
        };
        let plan_pin = IvyJson::object([
            ("objectId", "plan".into()),
            ("revision", IvyJson::Number(1.0)),
        ]);
        let mut results = vec![];
        for _ in 0..input.required("attempts").unwrap().as_f64().unwrap() as usize {
            let outcome = async {
                let native =
                    OutlookNative::new(&engine, input.required("configuration")?, tools.clone())?;
                Ok(match input.required("mode")?.as_str()?.as_str() {
                    "constructor" => IvyJson::Null,
                    "head" => native.raw_head().await?.value(),
                    "plan" => native.plan(&plan_pin)?.value(),
                    "prepare" => native
                        .prepare(&native.raw_head().await?)
                        .await?
                        .map_or(IvyJson::Null, |d| d.value()),
                    "evidence" => native
                        .evidence(&IvyJson::object([
                            ("objectId", "proof".into()),
                            ("revision", IvyJson::Number(1.0)),
                        ]))
                        .await?
                        .value(),
                    "collect" => match native.collect(&native.plan(&plan_pin)?).await? {
                        None => IvyJson::Null,
                        Some(proof) => {
                            if input.get("nextHead") == Some(&true.into()) {
                                let head = input
                                    .required("documents")?
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .find(|r| {
                                        r.get("key") == Some(&"secretary/outlook-head".into())
                                    })
                                    .unwrap();
                                IvyJson::object([
                                    (
                                        "nextHead",
                                        native.next_head(
                                            head.required("value")?,
                                            &native.plan(&plan_pin)?,
                                            &proof,
                                        )?,
                                    ),
                                    ("proof", proof.value()),
                                ])
                            } else {
                                proof.value()
                            }
                        }
                    },
                    other => panic!("Unknown native fixture {other}"),
                })
            }
            .await;
            results.push(result(outcome));
        }
        let (retained, mut ids) = retained(&journal);
        if input.get("noCalls") == Some(&true.into()) {
            for row in retained
                .required("documents")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .filter(|r| r.get("key") == Some(&"secretary/outlook-call".into()))
            {
                let call = row.required("value").unwrap();
                let id = call
                    .required("operationId")
                    .unwrap()
                    .as_js_string()
                    .unwrap();
                let parsed = parse_operation_id_js(id).unwrap();
                assert_eq!(
                    parsed.runtime_epoch,
                    ivy_contracts::JsString::from("fixture-epoch")
                );
                ids.insert(
                    id.clone(),
                    format!(
                        "fresh-native-slot-{}",
                        call.required("slot").unwrap().as_f64().unwrap() as usize
                    )
                    .into(),
                );
            }
        }
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
            "Unconsumed original RPC in {label}"
        );
        // Native collection has no authority to advance the Secretary source.
        for source in input.required("core").unwrap().as_array().unwrap() {
            let saved = journal
                .read(
                    "secretary/source",
                    &source.required("id").unwrap().as_str().unwrap(),
                    None,
                    false,
                )
                .unwrap();
            assert_eq!(
                canonical(&saved.value, 32 * 1024 * 1024).unwrap(),
                canonical(source.required("value").unwrap(), 32 * 1024 * 1024).unwrap()
            );
        }
    }
    println!(
        "outlook-native original comparisons: {} cases / {} attempts",
        rows.len(),
        rows.iter()
            .map(|r| r
                .required("input")
                .unwrap()
                .required("attempts")
                .unwrap()
                .as_f64()
                .unwrap() as usize)
            .sum::<usize>()
    );
}
