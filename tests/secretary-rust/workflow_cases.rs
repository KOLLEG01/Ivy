use super::*;
use ivy_sdk::clock::Clock;
use ivy_secretary::{
    assignment_workflow::*,
    inbox_evidence::{InboxEvidence, operation_name},
    journal::{Journal, Limits},
    store::Store,
};
use ivy_store::rusqlite::OptionalExtension;
struct WorkflowClock(i64);
impl Clock for WorkflowClock {
    fn utc_millis(&self) -> i64 {
        self.0
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
#[tokio::test]
async fn original_assignment_write_receipts_and_retry_boundaries() {
    let mut command = std::process::Command::new("node");
    command
        .arg("tests/secretary-rust/workflow-oracle.mjs")
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
        16 * 1024 * 1024,
    )
    .unwrap();
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert!(rows.len() >= 40);
    for (index, row) in rows.iter().enumerate() {
        let input = row.required("input").unwrap();
        let args = input.required("args").unwrap().as_array().unwrap();
        let method = input.required("method").unwrap().as_str().unwrap();
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
        let journal = Journal::open(
            std::path::Path::new(":memory:"),
            Limits {
                count: input.required("capacity").unwrap().as_f64().unwrap() as i64,
                ..Default::default()
            },
        )
        .unwrap();
        let boundary = input.required("expiredBefore").unwrap().as_f64().unwrap() as u64;
        if boundary != 0 {
            journal
                .db
                .execute(
                    "INSERT INTO journal_state VALUES('expiredBefore',?)",
                    [boundary.to_string()],
                )
                .unwrap();
        }
        let name = operation_name(
            corpus.required("caller").unwrap(),
            corpus.required("id").unwrap(),
        )
        .unwrap()
        .to_utf8()
        .unwrap();
        if let Some(saved) = input.get("saved").filter(|v| !v.is_null()) {
            journal
                .create_technical(WORKFLOW_OPERATION_KEY, &name, saved)
                .unwrap();
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
        let settings = IvyJson::object([("sources", IvyJson::Array(Vec::new()))]);
        let service_node_id = "fixture.secretary".into();
        let evidence = InboxEvidence {
            store: &store,
            settings: &settings,
            service_node_id: &service_node_id,
            cancellation: RequestOptions::default(),
        };
        let clock = WorkflowClock(corpus.required("now").unwrap().as_f64().unwrap() as i64);
        let epoch = epoch_fixture(corpus.required("epoch").unwrap().as_js_string().unwrap());
        let workflow = AssignmentWorkflow {
            evidence: &evidence,
            runtime_epoch: &epoch,
            clock: &clock,
        };
        let result = match method.as_str() {
            "validateOperationId" => workflow
                .validate_operation_id(args[0].as_js_string().unwrap())
                .await
                .map(|_| IvyJson::Null),
            "workflowReceipt" => workflow
                .receipt(&args[0])
                .await
                .map(|v| v.unwrap_or(IvyJson::Null)),
            "saveWorkflow" => {
                let action = match args[2].as_str().unwrap().as_str() {
                    "create_assignment" => WorkflowAction::CreateAssignment,
                    "update_assignment" => WorkflowAction::UpdateAssignment,
                    "configure_execution" => WorkflowAction::ConfigureExecution,
                    _ => panic!("Unknown action"),
                };
                workflow
                    .save(
                        &args[0],
                        &args[1],
                        action,
                        args.get(3).filter(|v| !v.is_null()),
                    )
                    .await
            }
            _ => panic!("Unknown method {method}"),
        };
        let result = match result {
            Ok(value) => IvyJson::object([("value", value)]),
            Err(e) => IvyJson::object([
                ("code", e.code.into()),
                ("message", e.message.into()),
                ("outcome", e.outcome.as_str().into()),
            ]),
        };
        let saved = journal.named(WORKFLOW_OPERATION_KEY, &name, true).unwrap();
        let (retained, cleanup) = match saved {
            None => (IvyJson::Null, IvyJson::Null),
            Some(saved) => {
                let cleanup: Option<i64> = journal
                    .db
                    .query_row(
                        "SELECT delete_after FROM technical_cleanup WHERE root_id=?",
                        [&saved.pin.object_id],
                        |r| r.get(0),
                    )
                    .optional()
                    .unwrap();
                (
                    IvyJson::object([
                        ("value", saved.value),
                        ("revision", IvyJson::Number(saved.pin.revision as f64)),
                    ]),
                    cleanup
                        .map(|v| IvyJson::Number(v as f64))
                        .unwrap_or(IvyJson::Null),
                )
            }
        };
        let actual = IvyJson::object([
            ("result", result),
            (
                "trace",
                IvyJson::Array(client.trace.lock().unwrap().clone()),
            ),
            (
                "remaining",
                IvyJson::Number(client.steps.lock().unwrap().len() as f64),
            ),
            ("retained", retained),
            ("cleanup", cleanup),
        ]);
        assert_eq!(
            &actual,
            row.required("expected").unwrap(),
            "workflow case {index}: {method}"
        );
    }
}
