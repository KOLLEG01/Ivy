#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use super::*;
use ivy_contracts::{JsString, hash::hash_json, json::canonical};
use ivy_sdk::clock::Clock;
use ivy_secretary::{
    assignment_workflow::WORKFLOW_OPERATION_KEY,
    engine::SecretaryEngine,
    inbox_evidence::{InboxEvidence, operation_name},
    inbox_recovery::RecoveryScan,
    journal::{Journal, Limits},
    store::Store,
};
use ivy_store::rusqlite::params;
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
fn fixed_uuid() -> JsString {
    "00000000-0000-4000-8000-000000000fff".into()
}

#[tokio::test]
async fn original_public_inbox_admission_and_retained_history() {
    let output = reference_data::read(include_bytes!("engine-inbox-cases.json.gz"));
    let corpus = parse(
        std::str::from_utf8(&output).unwrap(),
        32 * 1024 * 1024,
    )
    .unwrap();
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 166);
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
        for local in input.required("locals").unwrap().as_array().unwrap() {
            let id = local.required("id").unwrap().as_str().unwrap();
            let key = local.required("key").unwrap().as_str().unwrap();
            let value = local.required("value").unwrap();
            let first = local.required("first").unwrap();
            let revision = if hash_json(value).unwrap() == hash_json(first).unwrap() {
                1
            } else {
                2
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
                        value
                            .get("phase")
                            .filter(|v| *v != &IvyJson::from("accepted"))
                            .map(|_| 1_i64),
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
        let request = input.required("request").unwrap();
        if input.get("workflow") == Some(&IvyJson::Bool(true)) {
            journal
                .create_technical(
                    WORKFLOW_OPERATION_KEY,
                    &operation_name(
                        &"producer".into(),
                        &"fixture-epoch:1800000000000:inbox".into(),
                    )
                    .unwrap()
                    .to_utf8()
                    .unwrap(),
                    &IvyJson::object([("fixture", true.into())]),
                )
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
        let wall = FixedClock(input.required("wall").unwrap().as_f64().unwrap() as i64);
        let tools = ToolClients::new(client.clone());
        let issues = RefCell::new(RecoveryScan::default());
        let engine = SecretaryEngine {
            evidence: &evidence,
            clock: &clock,
            operation_clock: &wall,
            runtime_epoch: epoch_fixture(corpus.required("epoch").unwrap().as_js_string().unwrap()),
            new_uuid: fixed_uuid,
            tools: &tools,
            issues: &issues,
        };
        let mut results = Vec::new();
        for _ in 0..input.required("repeats").unwrap().as_f64().unwrap() as usize {
            let caller = input.required("caller").unwrap();
            let result = if input.get("method") == Some(&"operation".into()) {
                engine
                    .operation(
                        caller,
                        input.required("scope").unwrap(),
                        &"fixture-epoch:1800000000000:inbox".into(),
                    )
                    .await
            } else {
                engine.action(caller, request).await
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
        let mut statement = journal.db.prepare("SELECT id,key,name,current_revision,value_json,completed_at FROM documents ORDER BY key,name").unwrap();
        let documents = statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let retained = documents
            .into_iter()
            .map(|(id, key, name, revision, value, completed)| {
                let mut statement = journal
                    .db
                    .prepare(
                        "SELECT revision,value_json FROM revisions WHERE id=? ORDER BY revision",
                    )
                    .unwrap();
                let history = statement
                    .query_map(params![id], |r| {
                        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                    })
                    .unwrap()
                    .map(|r| {
                        let (revision, value) = r.unwrap();
                        IvyJson::object([
                            ("revision", IvyJson::Number(revision as f64)),
                            ("value", parse(&value, 32 * 1024 * 1024).unwrap()),
                        ])
                    })
                    .collect();
                IvyJson::object([
                    ("key", key.into()),
                    ("name", name.into()),
                    ("revision", IvyJson::Number(revision as f64)),
                    ("value", parse(&value, 32 * 1024 * 1024).unwrap()),
                    ("completed", completed.is_some().into()),
                    ("history", IvyJson::Array(history)),
                ])
            })
            .collect();
        let mut statement = journal
            .db
            .prepare("SELECT capture_id FROM capture_acknowledgements ORDER BY capture_id")
            .unwrap();
        let acknowledgements = statement
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| IvyJson::from(r.unwrap()))
            .collect();
        let actual = IvyJson::object([
            ("results", IvyJson::Array(results)),
            (
                "trace",
                IvyJson::Array(client.trace.lock().unwrap().clone()),
            ),
            ("retained", IvyJson::Array(retained)),
            ("acknowledgements", IvyJson::Array(acknowledgements)),
        ]);
        assert!(
            client.steps.lock().unwrap().is_empty(),
            "Unconsumed RPC in {label}"
        );
        assert_eq!(
            canonical(&actual, 32 * 1024 * 1024).unwrap(),
            canonical(row.required("expected").unwrap(), 32 * 1024 * 1024).unwrap(),
            "case {label}"
        );
    }
}
