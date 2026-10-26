#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use super::*;
use ivy_contracts::{json::canonical, operation::parse_operation_id_js};
use ivy_sdk::clock::Clock;
use ivy_secretary::{
    assignment_voice_engine::AssignmentVoice,
    execution_import::import_execution_delivery,
    inbox_evidence::InboxEvidence,
    journal::{Journal, Limits},
    store::Store,
};
use ivy_store::rusqlite::params;

struct VoiceClock(i64);
impl Clock for VoiceClock {
    fn utc_millis(&self) -> i64 {
        self.0
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
#[tokio::test]
async fn original_assignment_voice_durable_reconciliation() {
    let output = reference_data::read(include_bytes!("assignment-voice-engine-cases.json.gz"));
    let corpus = parse(
        std::str::from_utf8(&output).unwrap(),
        32 * 1024 * 1024,
    )
    .unwrap();
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 70);
    let clock = VoiceClock(corpus.required("now").unwrap().as_f64().unwrap() as i64);
    for (index, row) in rows.iter().enumerate() {
        let input = row.required("input").unwrap();
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
                        "secretary/assignment-voice",
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
            settings: input.required("settings").unwrap(),
            service_node_id: &service_node_id,
            cancellation,
        };
        let tools = ToolClients::new(client.clone());
        let engine = AssignmentVoice {
            evidence: &evidence,
            tools: &tools,
            clock: &clock,
        };
        if input.get("legacy").is_some_and(|value| !value.is_null()) {
            import_execution_delivery(&evidence, execution)
                .await
                .unwrap();
        }
        if execution
            .required("value")
            .unwrap()
            .get("voice")
            .is_some_and(|value| !value.is_null())
            || input.get("legacy").is_some_and(|value| !value.is_null())
        {
            poison_legacy_record(
                &journal,
                "secretary/assignment-voice",
                &execution
                    .required("value")
                    .unwrap()
                    .required("executionId")
                    .unwrap()
                    .as_str()
                    .unwrap(),
            );
        }
        let outcome = if input.get("complete") == Some(&IvyJson::Bool(true)) {
            engine
                .complete_fallback(execution)
                .await
                .map(|()| IvyJson::Null)
        } else {
            engine
                .step(
                    execution,
                    input.required("decision").unwrap(),
                    input.required("eligible").unwrap().as_bool().unwrap(),
                )
                .await
                .map(|v| v.as_str().into())
        };
        let result = match outcome {
            Ok(value) => IvyJson::object([("value", value)]),
            Err(e) => IvyJson::object([
                ("code", e.code.into()),
                ("message", e.message.into()),
                ("outcome", e.outcome.to_json()),
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
                // Only the generated UUID differs in fresh-admission cases. Retained
                // admission and every recovery case compare their original ID exactly.
                if let Some(id) = input.get("generatedOperationId").filter(|v| !v.is_null()) {
                    let mut content = params.required("content").unwrap().clone();
                    let mut value = content.required("value").unwrap().clone();
                    let mut voice = value.required("voice").unwrap().clone();
                    let mut request = voice.required("request").unwrap().clone();
                    uuid::Uuid::parse_str(
                        &request.required("operationId").unwrap().as_str().unwrap(),
                    )
                    .unwrap();
                    request.insert("operationId", id.clone());
                    voice.insert("request", request);
                    value.insert("voice", voice);
                    content.insert("value", value);
                    params.insert("content", content);
                }
                call.insert("params", params);
            }
        }
        let actual = IvyJson::object([
            ("result", result),
            ("trace", IvyJson::Array(trace)),
            (
                "remaining",
                IvyJson::Number(client.steps.lock().unwrap().len() as f64),
            ),
        ]);
        assert_eq!(
            &actual,
            row.required("expected").unwrap(),
            "Voice engine case {index}: {:?}",
            input.get("label")
        );
    }
}
