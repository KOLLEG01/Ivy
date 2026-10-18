use super::*;
use ivy_contracts::{
    checked_native_contract::CheckedNativeContracts, json::canonical,
    operation::parse_operation_id_js,
};
use ivy_sdk::clock::Clock;
use ivy_secretary::{
    assignment_execution::AssignmentExecution,
    assignment_native::AssignmentNative,
    inbox_evidence::InboxEvidence,
    journal::{Journal, Limits},
    store::Store,
};
use ivy_store::rusqlite::params;

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
async fn original_assignment_execution_transitions_and_traces() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut command = std::process::Command::new("node");
    command
        .arg("tests/secretary-rust/assignment-execution-oracle.mjs")
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
    assert!(rows.len() >= 110);
    for (index, row) in rows.iter().enumerate() {
        let input = row.required("input").unwrap();
        let journal = Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
        for d in input.required("documents").unwrap().as_array().unwrap() {
            let id = d.required("id").unwrap().as_str().unwrap();
            let key = d.required("key").unwrap().as_str().unwrap();
            let name = d.required("name").unwrap().as_str().unwrap();
            let encoded = canonical(d.required("value").unwrap(), 1_048_576).unwrap();
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
        }
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
            trace: Mutex::new(Vec::new()),
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
        let cancellation = RequestOptions {
            cancel_error: IvyError::new("fixture_cancelled", "Fixture cancelled."),
            ..RequestOptions::default()
        };
        if input.get("cancel") == Some(&IvyJson::Bool(true)) {
            cancellation.cancellation.cancel();
        }
        let service_node_id = "fixture.secretary".into();
        let evidence = InboxEvidence {
            store: &store,
            settings: corpus.required("settings").unwrap(),
            service_node_id: &service_node_id,
            cancellation,
        };
        let clock = FixedClock(input.required("time").unwrap().as_f64().unwrap() as i64);
        let retained = CheckedNativeContracts::new(root.clone());
        let runner = AssignmentExecution {
            native: AssignmentNative {
                evidence: &evidence,
                tools: Arc::new(ToolClients::new(client.clone())),
            },
            clock: &clock,
            retained: &retained,
            notification_policy: corpus
                .required("notificationPolicy")
                .unwrap()
                .as_js_string()
                .unwrap(),
        };
        let current = input.required("current").unwrap();
        let outcome = match input.required("kind").unwrap().as_str().unwrap().as_str() {
            "start" => runner.start(current).await,
            "run" => runner.run(current).await,
            "archive" => runner.archive(current).await,
            "remove" => runner.remove(current).await,
            _ => panic!("Unknown transition"),
        };
        let result = match outcome {
            Ok(value) => IvyJson::object([("value", value)]),
            Err(e) => IvyJson::object([
                ("code", e.code.into()),
                ("message", e.message.into()),
                ("outcome", e.outcome.as_str().into()),
            ]),
        };
        let mut trace = client.trace.lock().unwrap().clone();
        for call in &mut trace {
            if call.get("method") == Some(&"objects.write".into()) {
                let mut params = call.required("params").unwrap().clone();
                parse_operation_id_js(
                    params
                        .required("mutationId")
                        .unwrap()
                        .as_js_string()
                        .unwrap(),
                )
                .unwrap();
                params.insert("mutationId", "<scoped-operation>".into());
                if input.get("fresh") == Some(&IvyJson::Bool(true)) {
                    let mut content = params.required("content").unwrap().clone();
                    let mut value = content.required("value").unwrap().clone();
                    let mut operations = value.required("nativeOperations").unwrap().clone();
                    let key = input.required("key").unwrap().as_str().unwrap();
                    parse_operation_id_js(
                        operations.required(&key).unwrap().as_js_string().unwrap(),
                    )
                    .unwrap();
                    operations.insert(key.as_str(), "<generated-operation>".into());
                    value.insert("nativeOperations", operations);
                    content.insert("value", value);
                    params.insert("content", content);
                }
                call.insert("params", params);
            }
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
        assert!(
            client.steps.lock().unwrap().is_empty(),
            "Case {index}: {:?}",
            input.get("label")
        );
        let actual = IvyJson::object([
            ("result", result),
            ("trace", IvyJson::Array(trace)),
            ("remaining", IvyJson::Array(remaining)),
        ]);
        assert_eq!(
            &actual,
            row.required("expected").unwrap(),
            "Case {index}: {:?}",
            input.get("label")
        );
    }
}
