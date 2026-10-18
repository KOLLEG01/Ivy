use super::*;
use ivy_contracts::{
    checked_native_contract::CheckedNativeContracts, json::canonical,
    operation::parse_operation_id_js,
};
use ivy_sdk::clock::Clock;
use ivy_secretary::{
    assignment_execution::AssignmentExecution,
    assignment_native::AssignmentNative,
    assignment_preflight::{AssignmentPreflight, PreflightResult},
    inbox_evidence::InboxEvidence,
    journal::{Journal, Limits},
    store::Store,
};
use std::sync::atomic::{AtomicI64, Ordering};
struct PreflightClock(AtomicI64);
impl Clock for PreflightClock {
    fn utc_millis(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
#[tokio::test]
async fn original_preflight_admission_outputs_cancellation_and_timestamps() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut command = std::process::Command::new("node");
    command
        .arg("tests/secretary-rust/assignment-preflight-oracle.mjs")
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
    assert_eq!(rows.len(), 45);
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
        if input.get("cancelBefore") == Some(&IvyJson::Bool(true)) {
            cancellation.cancellation.cancel();
        }
        let service_node_id = "fixture.secretary".into();
        let evidence = InboxEvidence {
            store: &store,
            settings: corpus.required("settings").unwrap(),
            service_node_id: &service_node_id,
            cancellation: cancellation.clone(),
        };
        let clock = PreflightClock(AtomicI64::new(
            input.required("time").unwrap().as_f64().unwrap() as i64,
        ));
        let retained = CheckedNativeContracts::new(root.clone());
        let execution = AssignmentExecution {
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
        let preflight = AssignmentPreflight {
            execution: &execution,
        };
        let current = input.required("current").unwrap();
        let mut ran = false;
        let outcome = match preflight.prepare(current).await {
            Err(error) => Err(error),
            Ok(None) => Ok(current.clone()),
            Ok(Some(prepared)) => {
                ran = true;
                clock.0.store(
                    input.required("finishedAt").unwrap().as_f64().unwrap() as i64,
                    Ordering::Relaxed,
                );
                if input.get("cancelProcess") == Some(&IvyJson::Bool(true)) {
                    cancellation.cancellation.cancel();
                }
                let result = if input.get("failed") == Some(&IvyJson::Bool(true)) {
                    let error = input.required("error").unwrap();
                    let outcome = match error
                        .required("outcome")
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .as_str()
                    {
                        "unknown" => Outcome::Unknown,
                        "completed" => Outcome::Completed,
                        _ => Outcome::NotExecuted,
                    };
                    PreflightResult::Failed {
                        error: IvyError::new(
                            error.required("code").unwrap().as_str().unwrap(),
                            error.required("message").unwrap().as_str().unwrap(),
                        )
                        .with_outcome(outcome),
                        is_ivy_error: input.get("ivyError") == Some(&IvyJson::Bool(true)),
                        stdout: input.required("stdout").unwrap().clone(),
                        stderr: input.required("stderr").unwrap().clone(),
                    }
                } else {
                    let string = |key| {
                        input
                            .get(key)
                            .and_then(IvyJson::as_js_string)
                            .cloned()
                            .unwrap_or_else(|| "".into())
                    };
                    PreflightResult::Completed {
                        stdout: string("stdout"),
                        stderr: string("stderr"),
                    }
                };
                preflight.complete(&prepared, result).await
            }
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
                call.insert("params", params);
            }
        }
        assert!(
            client.steps.lock().unwrap().is_empty(),
            "Case {index}: {:?}",
            input.get("label")
        );
        let actual = IvyJson::object([
            ("result", result),
            ("trace", IvyJson::Array(trace)),
            ("ran", IvyJson::Bool(ran)),
        ]);
        assert_eq!(
            canonical(&actual, 32 * 1024 * 1024).unwrap(),
            canonical(row.required("expected").unwrap(), 32 * 1024 * 1024).unwrap(),
            "Case {index}: {:?}",
            input.get("label")
        );
    }
}
