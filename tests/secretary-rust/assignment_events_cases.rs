#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use super::*;
use ivy_contracts::{json::canonical, operation::parse_operation_id_js};
use ivy_sdk::clock::Clock;
use ivy_secretary::{
    assignment_import::{SubscriptionAssignment, import_event_subscription},
    assignment_scheduler::AssignmentScheduler,
    inbox_evidence::InboxEvidence,
    inbox_recovery::RecoveryScan,
    journal::{Journal, Limits},
    store::Store,
};
use ivy_store::rusqlite::params;
struct EventClock(i64);
impl Clock for EventClock {
    fn utc_millis(&self) -> i64 {
        self.0
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
fn technical(journal: &Journal) -> IvyJson {
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
async fn original_event_matching_revision_rebase_and_durable_ack_order() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = reference_data::read(include_bytes!("assignment-events-progress-cases.json.gz"));
    let corpus = parse(
        std::str::from_utf8(&output).unwrap(),
        32 * 1024 * 1024,
    )
    .unwrap();
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 66);
    for (i, row) in rows.iter().enumerate() {
        let input = row.required("input").unwrap();
        let journal = Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
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
        let tools = Arc::new(ToolClients::new(client.clone()));
        let mut issues = RecoveryScan {
            cursor: None,
            issues: input
                .required("issues")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|p| {
                    let p = p.as_array().unwrap();
                    (p[0].as_str().unwrap(), p[1].as_str().unwrap())
                })
                .collect(),
        };
        let mut outcomes = Vec::new();
        let mut snapshots = Vec::new();
        for time in input.required("times").unwrap().as_array().unwrap() {
            let clock = EventClock(time.as_f64().unwrap() as i64);
            let scheduler = AssignmentScheduler {
                evidence: &evidence,
                tools: tools.clone(),
                clock: &clock,
                fallback_execution: None,
            };
            if i == 0 {
                let matches = corpus.required("matches").unwrap().as_array().unwrap();
                assert_eq!(matches.len(), 77);
                for (n, row) in matches.iter().enumerate() {
                    assert_eq!(
                        IvyJson::Bool(
                            scheduler
                                .event_matches(
                                    row.required("assignment").unwrap(),
                                    row.required("event").unwrap()
                                )
                                .unwrap()
                        ),
                        *row.required("expected").unwrap(),
                        "Matching case {n}"
                    );
                }
            }
            let result = if input.get("action") == Some(&"revisionAt".into()) {
                scheduler
                    .revision_at(
                        input.required("assignment").unwrap(),
                        input.required("at").unwrap().as_js_string().unwrap(),
                    )
                    .await
                    .map(|v| v.unwrap_or(IvyJson::Null))
            } else {
                async {
                    if input
                        .required("documents")?
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|d| d.get("key") == Some(&"secretary/event-subscription".into()))
                    {
                        if import_event_subscription(
                            &scheduler,
                            SubscriptionAssignment::Active {
                                assignment: input.required("assignment")?,
                                configuration: input.required("configuration")?,
                            },
                            &mut issues,
                        )
                        .await?
                        .is_some_and(|v| !v.completed)
                        {
                            return Ok(IvyJson::Bool(false));
                        }
                    }
                    scheduler
                        .events(
                            input.required("assignment").unwrap(),
                            input.required("configuration").unwrap(),
                            input.required("budget").unwrap().as_f64().unwrap() as usize,
                            &mut issues,
                        )
                        .await
                        .map(IvyJson::Bool)
                }
                .await
            };
            outcomes.push(match result {
                Ok(value) => IvyJson::object([("value", value)]),
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
            });
            snapshots.push(IvyJson::object([
                (
                    "issues",
                    IvyJson::Array(
                        issues
                            .issues
                            .iter()
                            .map(|(id, code)| {
                                IvyJson::Array(vec![id.clone().into(), code.clone().into()])
                            })
                            .collect(),
                    ),
                ),
                ("technical", technical(&journal)),
            ]));
        }
        let mut trace = client.trace.lock().unwrap().clone();
        let mut ids = Vec::new();
        for call in &mut trace {
            if ["objects.write", "events.unsubscribe"]
                .iter()
                .any(|m| call.get("method") == Some(&(*m).into()))
            {
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
                params.insert("mutationId", format!("<operation-{n}>").into());
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
