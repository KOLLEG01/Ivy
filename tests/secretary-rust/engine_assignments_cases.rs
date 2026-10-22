use super::*;
use ivy_contracts::{hash::hash_json, json::canonical};
use ivy_sdk::clock::Clock;
use ivy_secretary::{
    assignment_workflow::WORKFLOW_OPERATION_KEY,
    engine::SecretaryEngine,
    inbox_evidence::{InboxEvidence, operation_name},
    inbox_recovery::RecoveryScan,
    journal::{Journal, Limits},
    store::Store,
};
use ivy_store::rusqlite::{OptionalExtension, params};
use std::cell::RefCell;
struct FixedClock(i64);
impl Clock for FixedClock {
    fn utc_millis(&self) -> i64 {
        self.0
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
#[tokio::test]
async fn original_public_assignment_writes_and_operation_reads() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut command = std::process::Command::new("node");
    command
        .arg("tests/secretary-rust/engine-assignments-oracle.mjs")
        .current_dir(root);
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
        8 * 1024 * 1024,
    )
    .unwrap();
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 80);
    for row in rows {
        let input = row.required("input").unwrap();
        let label = input.required("label").unwrap().as_str().unwrap();
        let client = Arc::new(Scripted {
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
        });
        let journal = Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
        let root_id = corpus
            .required("rootId")
            .unwrap()
            .as_js_string()
            .unwrap()
            .clone();
        let caller = corpus.required("caller").unwrap();
        let id = corpus.required("id").unwrap();
        let name = operation_name(caller, id).unwrap().to_utf8().unwrap();
        if let Some(saved) = input.get("saved").filter(|v| !v.is_null()) {
            journal
                .create_technical(WORKFLOW_OPERATION_KEY, &name, saved)
                .unwrap();
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
        for local in input.required("locals").unwrap().as_array().unwrap() {
            let id = local.required("id").unwrap().as_str().unwrap();
            let value = local.required("value").unwrap();
            let first = local.required("first").unwrap();
            let revision = if hash_json(value).unwrap() == hash_json(first).unwrap() {
                1
            } else {
                2
            };
            let encoded = canonical(value, 1_048_576).unwrap();
            journal
                .db
                .execute(
                    "INSERT INTO documents VALUES(?,?,?,?,?,?,?)",
                    params![
                        id,
                        local.required("key").unwrap().as_str().unwrap(),
                        local.required("name").unwrap().as_str().unwrap(),
                        revision,
                        0,
                        (value.get("phase") == Some(&"succeeded".into())).then_some(1_i64),
                        encoded
                    ],
                )
                .unwrap();
            journal
                .db
                .execute(
                    "INSERT INTO revisions VALUES(?,?,?)",
                    params![id, 1, canonical(first, 1_048_576).unwrap()],
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
            root: root_id,
            journal: &journal,
        };
        let cancellation = RequestOptions {
            cancel_error: IvyError::new("fixture_cancelled", "Fixture cancelled."),
            ..RequestOptions::default()
        };
        if input.get("cancelled") == Some(&IvyJson::Bool(true)) {
            cancellation.cancellation.cancel();
        }
        let node = "fixture.secretary".into();
        let evidence = InboxEvidence {
            store: &store,
            settings: input.required("settings").unwrap(),
            service_node_id: &node,
            cancellation,
        };
        let clock = FixedClock(input.required("time").unwrap().as_f64().unwrap() as i64);
        let operation_clock = FixedClock(input.required("wall").unwrap().as_f64().unwrap() as i64);
        let tools = ToolClients::new(client.clone());
        let issues = RefCell::new(RecoveryScan::default());
        let engine = SecretaryEngine {
            evidence: &evidence,
            clock: &clock,
            operation_clock: &operation_clock,
            runtime_epoch: epoch_fixture(corpus.required("epoch").unwrap().as_js_string().unwrap()),
            new_uuid: ivy_secretary::engine::random_uuid,
            tools: &tools,
            issues: &issues,
        };
        let mut results = Vec::new();
        for _ in 0..input.required("repeats").unwrap().as_f64().unwrap() as usize {
            let caller = input.required("caller").unwrap();
            let request = input.required("request").unwrap();
            let result = match input.required("method").unwrap().as_str().unwrap().as_str() {
                "saveAssignment" => engine.save_assignment(caller, request).await,
                "saveConfiguration" => engine.save_configuration(caller, request).await,
                "operationRead" => {
                    engine
                        .operation_read(caller, input.required("scope").unwrap(), id)
                        .await
                }
                method => panic!("Unexpected {method}"),
            };
            results.push(match result {
                Ok(value) => IvyJson::object([("value", value)]),
                Err(error) => IvyJson::object([
                    ("code", error.code.into()),
                    ("message", error.message.into()),
                    ("outcome", error.outcome.to_json()),
                ]),
            });
        }
        let retained = journal
            .named(WORKFLOW_OPERATION_KEY, &name, true)
            .unwrap()
            .map(|row| {
                let cleanup: Option<i64> = journal
                    .db
                    .query_row(
                        "SELECT delete_after FROM technical_cleanup WHERE root_id=?",
                        params![row.pin.object_id],
                        |r| r.get(0),
                    )
                    .optional()
                    .unwrap();
                IvyJson::object([
                    ("value", row.value.clone()),
                    ("revision", IvyJson::Number(row.pin.revision as f64)),
                    (
                        "cleanup",
                        cleanup
                            .map(|v| IvyJson::Number(v as f64))
                            .unwrap_or(IvyJson::Null),
                    ),
                ])
            })
            .unwrap_or(IvyJson::Null);
        let actual = IvyJson::object([
            ("results", IvyJson::Array(results)),
            (
                "trace",
                IvyJson::Array(client.trace.lock().unwrap().clone()),
            ),
            ("retained", retained),
        ]);
        assert!(
            client.steps.lock().unwrap().is_empty(),
            "Unconsumed RPC: {label}"
        );
        assert_eq!(
            canonical(&actual, 8 * 1024 * 1024).unwrap(),
            canonical(row.required("expected").unwrap(), 8 * 1024 * 1024).unwrap(),
            "case {label}"
        );
    }
}
