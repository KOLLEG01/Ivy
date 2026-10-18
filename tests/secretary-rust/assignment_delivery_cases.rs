use super::*;
use ivy_contracts::{hash::hash_json, json::canonical, operation::parse_operation_id_js};
use ivy_sdk::clock::Clock;
use ivy_secretary::{
    assignment_decision::assignment_decision,
    assignment_delivery::AssignmentDelivery,
    inbox_evidence::InboxEvidence,
    inbox_recovery::RecoveryScan,
    journal::{Journal, Limits},
    store::Store,
};
use ivy_store::rusqlite::params;
struct DeliveryClock(i64);
impl Clock for DeliveryClock {
    fn utc_millis(&self) -> i64 {
        self.0
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
fn outcome(result: Result<IvyJson, IvyError>) -> IvyJson {
    match result {
        Ok(value) => IvyJson::object([("value", value)]),
        Err(e) => IvyJson::object([
            ("code", e.code.into()),
            ("message", e.message.into()),
            ("outcome", e.outcome.as_str().into()),
        ]),
    }
}
// Only a fresh, durably admitted ID is mapped between the independent runtimes.
// Retained IDs are compared exactly. Mapping response bytes also updates valid
// content metadata so the real Store still validates every read boundary.
struct AdmissionScripted {
    inner: Scripted,
    expected: Option<IvyJson>,
    admitted: Mutex<Option<IvyJson>>,
}
fn replace_id(value: &IvyJson, from: &IvyJson, to: &IvyJson) -> IvyJson {
    if value == from {
        return to.clone();
    }
    match value {
        IvyJson::Array(values) => {
            IvyJson::Array(values.iter().map(|v| replace_id(v, from, to)).collect())
        }
        IvyJson::Object(values) => IvyJson::Object(
            values
                .iter()
                .map(|(k, v)| (k.clone(), replace_id(v, from, to)))
                .collect(),
        ),
        _ => value.clone(),
    }
}
impl RpcClient for AdmissionScripted {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            if self.expected.is_some() && method == "objects.write" {
                if let Some(id) = params
                    .get("content")
                    .and_then(|v| v.get("value"))
                    .and_then(|v| v.get("delivery"))
                    .and_then(|v| v.get("request"))
                    .and_then(|v| v.get("operationId"))
                {
                    parse_operation_id_js(id.as_js_string().unwrap()).unwrap();
                    let mut admitted = self.admitted.lock().unwrap();
                    if let Some(original) = admitted.as_ref() {
                        assert_eq!(id, original, "Original admitted ID changed");
                    } else {
                        *admitted = Some(id.clone());
                    }
                }
            }
            if self.expected.is_some()
                && method == "tools.call"
                && matches!(
                    params
                        .get("qualifiedName")
                        .and_then(|v| v.as_str().ok())
                        .as_deref(),
                    Some("chat.operation" | "chat.notify")
                )
            {
                let admitted = self.admitted.lock().unwrap();
                assert!(
                    admitted.is_some(),
                    "Provider called before durable admission"
                );
                assert_eq!(
                    params.required("arguments")?.get("operationId"),
                    admitted.as_ref()
                );
                if params.get("qualifiedName") == Some(&"chat.notify".into()) {
                    assert_eq!(params.get("operationId"), admitted.as_ref());
                }
            }
            let value = self.inner.request(method, params, options).await?;
            let actual = self.admitted.lock().unwrap().clone();
            if let (Some(expected), Some(actual)) = (&self.expected, actual) {
                let mut mapped = replace_id(&value, expected, &actual);
                if let (Some(content), Some(revision)) =
                    (value.get("content"), value.get("revision"))
                {
                    if content.get("encoding") == Some(&"json".into()) {
                        let original = content.required("value")?;
                        let changed = mapped.required("content")?.required("value")?;
                        let mut metadata = revision.clone();
                        if revision.get("contentHash") == Some(&hash_json(original)?.into()) {
                            metadata.insert("contentHash", hash_json(changed)?.into());
                        }
                        if revision.get("byteLength").and_then(IvyJson::as_f64)
                            == Some(canonical(original, 32 * 1024 * 1024)?.len() as f64)
                        {
                            metadata.insert(
                                "byteLength",
                                IvyJson::Number(canonical(changed, 32 * 1024 * 1024)?.len() as f64),
                            );
                        }
                        mapped.insert("revision", metadata);
                    }
                }
                Ok(mapped)
            } else {
                Ok(value)
            }
        })
    }
}
#[tokio::test]
async fn original_assignment_main_delivery_and_decisions() {
    let mut command = std::process::Command::new("node");
    command
        .arg("tests/secretary-rust/assignment-delivery-oracle.mjs")
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
    let decisions = corpus.required("decisions").unwrap().as_array().unwrap();
    assert_eq!(decisions.len(), 103);
    for (index, row) in decisions.iter().enumerate() {
        assert_eq!(
            &outcome(assignment_decision(
                row.required("answer").unwrap().as_js_string().unwrap()
            )),
            row.required("result").unwrap(),
            "decision {index}"
        );
    }
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 109);
    let clock = DeliveryClock(corpus.required("now").unwrap().as_f64().unwrap() as i64);
    for (index, row) in rows.iter().enumerate() {
        let input = row.required("input").unwrap();
        let client = Arc::new(AdmissionScripted {
            inner: Scripted {
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
            expected: input
                .get("generatedOperationId")
                .filter(|v| !v.is_null())
                .cloned(),
            admitted: Mutex::new(None),
        });
        let journal = Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
        let execution = input.required("execution").unwrap();
        if let Some(legacy) = input.get("legacy").filter(|v| !v.is_null()) {
            let encoded = canonical(legacy, 1024 * 1024).unwrap();
            journal
                .db
                .execute(
                    "INSERT INTO technical_documents VALUES(?,?,?,?,?)",
                    params![
                        "legacy",
                        "secretary/assignment-handoff",
                        execution
                            .required("value")
                            .unwrap()
                            .required("executionId")
                            .unwrap()
                            .as_str()
                            .unwrap(),
                        1,
                        encoded
                    ],
                )
                .unwrap();
            journal
                .db
                .execute(
                    "INSERT INTO technical_revisions VALUES(?,?,?)",
                    params!["legacy", 1, encoded],
                )
                .unwrap();
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
        let tools = ToolClients::new(client.clone());
        let delivery = AssignmentDelivery {
            evidence: &evidence,
            tools: &tools,
            clock: &clock,
            minimum_notification_age_minutes: input
                .required("minimumAge")
                .unwrap()
                .as_f64()
                .unwrap(),
        };
        let mut issues = RecoveryScan::default();
        issues.issue("unrelated", "retained");
        let result = outcome(
            delivery
                .step(execution, &mut issues)
                .await
                .map(IvyJson::Bool),
        );
        let mut trace = client.inner.trace.lock().unwrap().clone();
        if let (Some(expected), Some(actual)) =
            (&client.expected, client.admitted.lock().unwrap().as_ref())
        {
            trace = trace
                .iter()
                .map(|v| replace_id(v, actual, expected))
                .collect();
        }
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
        let actual = IvyJson::object([
            ("result", result),
            ("trace", IvyJson::Array(trace)),
            (
                "remaining",
                IvyJson::Number(client.inner.steps.lock().unwrap().len() as f64),
            ),
            (
                "issues",
                IvyJson::Array(
                    issues
                        .issues
                        .into_iter()
                        .map(|(id, code)| IvyJson::Array(vec![id.into(), code.into()]))
                        .collect(),
                ),
            ),
        ]);
        assert_eq!(
            &actual,
            row.required("expected").unwrap(),
            "delivery {index} {:?}",
            input.get("label")
        );
    }
}
