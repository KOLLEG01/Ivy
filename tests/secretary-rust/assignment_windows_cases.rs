#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use super::*;
use ivy_contracts::{json::canonical, operation::parse_operation_id_js};
use ivy_sdk::clock::{Clock, parse_date};
use ivy_secretary::{
    assignment_message::MESSAGE_WINDOW_KEY,
    assignment_scheduler::AssignmentScheduler,
    inbox_evidence::InboxEvidence,
    journal::{Journal, Limits},
    store::Store,
};
use ivy_store::rusqlite::params;
struct WindowClock(i64);
impl Clock for WindowClock {
    fn utc_millis(&self) -> i64 {
        self.0
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
fn snapshot(journal: &Journal) -> IvyJson {
    let mut statement = journal.db.prepare("SELECT key,name,current_revision,value_json FROM technical_documents ORDER BY key,name").unwrap();
    let rows = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .unwrap();
    IvyJson::Array(
        rows.map(|r| {
            let (key, name, revision, value) = r.unwrap();
            IvyJson::object([
                ("key", key.into()),
                ("name", name.into()),
                ("revision", IvyJson::Number(revision as f64)),
                ("value", parse(&value, 4 * 1024 * 1024).unwrap()),
            ])
        })
        .collect(),
    )
}
#[tokio::test]
async fn original_object_message_windows_and_retry_boundaries() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = reference_data::read(include_bytes!("assignment-windows-cases.json.gz"));
    let corpus = parse(
        std::str::from_utf8(&output).unwrap(),
        32 * 1024 * 1024,
    )
    .unwrap();
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 75);
    for (i, row) in rows.iter().enumerate() {
        let input = row.required("input").unwrap();
        let mut journal =
            Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
        for doc in input.required("documents").unwrap().as_array().unwrap() {
            let id = doc.required("id").unwrap().as_str().unwrap();
            let key = doc.required("key").unwrap().as_str().unwrap();
            let name = doc.required("name").unwrap().as_str().unwrap();
            let value = canonical(doc.required("value").unwrap(), 4 * 1024 * 1024).unwrap();
            journal
                .db
                .execute(
                    "INSERT INTO technical_documents VALUES(?,?,?,?,?)",
                    params![id, key, name, 1, value],
                )
                .unwrap();
            journal
                .db
                .execute(
                    "INSERT INTO technical_revisions VALUES(?,?,?)",
                    params![id, 1, value],
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
        let tools = Arc::new(ToolClients::new(client.clone()));
        let mut outcomes = Vec::new();
        let mut snapshots = Vec::new();
        for action in input.required("actions").unwrap().as_array().unwrap() {
            journal.limits = action
                .get("limits")
                .map(|l| Limits {
                    count: l.required("count").unwrap().as_f64().unwrap() as i64,
                    bytes: l.required("bytes").unwrap().as_f64().unwrap() as i64,
                })
                .unwrap_or_default();
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
            let service_id = "fixture.secretary".into();
            let evidence = InboxEvidence {
                store: &store,
                settings: input.required("settings").unwrap(),
                service_node_id: &service_id,
                cancellation: RequestOptions::default(),
            };
            let clock = WindowClock(
                action
                    .get("enqueueAt")
                    .and_then(IvyJson::as_js_string)
                    .map(parse_date)
                    .unwrap_or(1_767_225_600_000.0) as i64,
            );
            let scheduler = AssignmentScheduler {
                evidence: &evidence,
                tools: tools.clone(),
                clock: &clock,
                fallback_execution: None,
            };
            let assignment = action
                .get("assignment")
                .unwrap_or(input.required("assignment").unwrap());
            let configuration = action
                .get("configuration")
                .unwrap_or(input.required("configuration").unwrap());
            let now = action
                .get("now")
                .and_then(IvyJson::as_js_string)
                .map(parse_date)
                .unwrap_or(1_767_226_200_000.0) as i64;
            let result = match action.required("kind").unwrap().as_str().unwrap().as_str() {
                "collectObjectEvent" => {
                    scheduler
                        .collect_object_event(assignment, action.required("event").unwrap())
                        .await
                }
                "collectMessageEvent" => {
                    scheduler
                        .collect_message_event(
                            assignment,
                            action.required("event").unwrap(),
                            configuration,
                        )
                        .await
                }
                "flushDueObjectWindows" => {
                    scheduler
                        .flush_due_object_windows(assignment, configuration, now)
                        .await
                }
                "flushDueMessageWindows" => {
                    scheduler
                        .flush_due_message_windows(assignment, configuration, now)
                        .await
                }
                "flushMessageWindow" => {
                    let saved = journal
                        .named(
                            MESSAGE_WINDOW_KEY,
                            &action.required("name").unwrap().as_str().unwrap(),
                            true,
                        )
                        .unwrap()
                        .unwrap();
                    scheduler.flush_message_window(&saved, configuration).await
                }
                _ => panic!("Unexpected window method"),
            };
            let outcome = match result {
                Ok(()) => IvyJson::object([("value", IvyJson::Null)]),
                Err(e) => {
                    let mut result = IvyJson::object([
                        ("code", e.code.clone().into()),
                        ("message", e.message.into()),
                    ]);
                    if e.code != "secretary_execution_unconfigured" {
                        result.insert("outcome", e.outcome.to_json());
                    }
                    result
                }
            };
            outcomes.push(outcome);
            snapshots.push(snapshot(&journal));
        }
        let mut trace = client.trace.lock().unwrap().clone();
        let mut ids = Vec::new();
        for call in &mut trace {
            if call.get("method") == Some(&"objects.write".into()) {
                let mut params = call.required("params").unwrap().clone();
                let id = params
                    .required("mutationId")
                    .unwrap()
                    .as_js_string()
                    .unwrap()
                    .clone();
                parse_operation_id_js(&id).unwrap();
                let n = ids.iter().position(|p| p == &id).unwrap_or_else(|| {
                    ids.push(id);
                    ids.len() - 1
                });
                params.insert("mutationId", format!("<scoped-operation-{n}>").into());
                call.insert("params", params);
            }
        }
        assert!(
            client.steps.lock().unwrap().is_empty(),
            "Case {i}: {:?}",
            input.get("label")
        );
        let actual = IvyJson::object([
            ("outcomes", IvyJson::Array(outcomes)),
            ("snapshots", IvyJson::Array(snapshots)),
            ("trace", IvyJson::Array(trace)),
        ]);
        assert_eq!(
            canonical(&actual, 32 * 1024 * 1024).unwrap(),
            canonical(row.required("expected").unwrap(), 32 * 1024 * 1024).unwrap(),
            "Case {i}: {:?}",
            input.get("label")
        );
    }
}
