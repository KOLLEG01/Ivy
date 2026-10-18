use super::*;
use ivy_contracts::{json::canonical, operation::parse_operation_id_js};
use ivy_sdk::clock::Clock;
use ivy_secretary::{
    assignment_scheduler::{AssignmentScheduler, select_default_agent_manager},
    inbox_evidence::InboxEvidence,
    journal::{Journal, Limits},
    store::Store,
};
struct SchedulerClock(i64);
impl Clock for SchedulerClock {
    fn utc_millis(&self) -> i64 {
        self.0
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
#[tokio::test]
async fn original_scheduler_target_configuration_and_execution_admission() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut command = std::process::Command::new("node");
    command
        .arg("tests/secretary-rust/assignment-scheduler-oracle.mjs")
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
        32 * 1024 * 1024,
    )
    .unwrap();
    for row in corpus.required("selections").unwrap().as_array().unwrap() {
        let selected = select_default_agent_manager(
            row.required("nodes").unwrap().as_array().unwrap(),
            row.required("hostId").unwrap().as_js_string().unwrap(),
        );
        assert_eq!(
            canonical(selected.unwrap_or(&IvyJson::Null), 1_048_576).unwrap(),
            canonical(row.required("expected").unwrap(), 1_048_576).unwrap()
        );
    }
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 56);
    for (index, row) in rows.iter().enumerate() {
        let input = row.required("input").unwrap();
        let journal = Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
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
            settings: input.required("settings").unwrap(),
            service_node_id: &service_node_id,
            cancellation,
        };
        let clock = SchedulerClock(input.required("time").unwrap().as_f64().unwrap() as i64);
        let scheduler = AssignmentScheduler {
            evidence: &evidence,
            tools: Arc::new(ToolClients::new(client.clone())),
            clock: &clock,
            fallback_execution: input.get("fallback").filter(|v| !v.is_null()),
        };
        let action = input.required("action").unwrap().as_str().unwrap();
        let outcome = match action.as_str() {
            "defaultExecution" => scheduler
                .default_execution()
                .await
                .map(|v| v.unwrap_or(IvyJson::Null)),
            "initialize" => scheduler.initialize().await.map(|()| IvyJson::Null),
            "configuration" => scheduler.configuration().await,
            "enqueue" => {
                scheduler
                    .enqueue(
                        input.required("assignment").unwrap(),
                        input.required("trigger").unwrap(),
                        input.required("configuration").unwrap(),
                    )
                    .await
            }
            _ => panic!("Unexpected scheduler action"),
        };
        let result = match outcome {
            Ok(value) => IvyJson::object([("value", value)]),
            Err(e) => {
                let mut value = IvyJson::object([
                    ("code", e.code.clone().into()),
                    ("message", e.message.into()),
                ]);
                // The original unconfigured branch throws a plain Error carrying
                // its scheduler diagnostic code; it has no Ivy outcome member.
                if e.code != "secretary_execution_unconfigured" {
                    value.insert("outcome", e.outcome.as_str().into());
                }
                value
            }
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
                call.insert("params", params);
            }
        }
        assert!(
            client.steps.lock().unwrap().is_empty(),
            "Case {index}: {:?}",
            input.get("label")
        );
        let actual = IvyJson::object([("result", result), ("trace", IvyJson::Array(trace))]);
        assert_eq!(
            canonical(&actual, 32 * 1024 * 1024).unwrap(),
            canonical(row.required("expected").unwrap(), 32 * 1024 * 1024).unwrap(),
            "Case {index}: {:?}",
            input.get("label")
        );
    }
}
