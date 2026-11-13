#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use super::*;
use ivy_contracts::{json::canonical, operation::parse_operation_id_js};
use ivy_secretary::{
    assignment_runner::{
        AssignmentRunner, execution_failure_disposition, release_calls, release_lease,
    },
    inbox_evidence::InboxEvidence,
    journal::{Journal, Limits},
    store::Store,
};
use ivy_store::rusqlite::params;

fn retained(journal: &Journal) -> (IvyJson, IvyJson) {
    let mut statement = journal
        .db
        .prepare(
            "SELECT d.id,d.key,d.name,d.current_revision,r.value_json FROM technical_documents d LEFT JOIN technical_revisions r ON r.id=d.id AND r.revision=d.current_revision ORDER BY d.id",
        )
        .unwrap();
    let documents = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .unwrap()
        .map(|r| {
            let (id, key, name, revision, value) = r.unwrap();
            IvyJson::object([
                ("id", id.into()),
                ("key", key.into()),
                ("name", name.into()),
                ("revision", IvyJson::Number(revision as f64)),
                ("value", parse(&value, 1_048_576).unwrap()),
            ])
        })
        .collect();
    let mut statement = journal
        .db
        .prepare("SELECT id,revision,value_json FROM technical_revisions ORDER BY id,revision")
        .unwrap();
    let revisions = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .map(|r| {
            let (id, revision, value) = r.unwrap();
            IvyJson::object([
                ("id", id.into()),
                ("revision", IvyJson::Number(revision as f64)),
                ("value", parse(&value, 1_048_576).unwrap()),
            ])
        })
        .collect();
    (IvyJson::Array(documents), IvyJson::Array(revisions))
}

#[tokio::test]
async fn cleanup_failure_preserves_the_original_completion_and_delete_call() {
    use ivy_secretary::assignment_cleanup::{CleanupAction, blocked, identity_missing};
    let original = IvyJson::object([
        ("phase", "deleting".into()), ("result", "original final answer".into()),
        ("errorCode", IvyJson::Null), ("completedAt", "2026-11-05T05:17:42.633Z".into()),
        ("threadId", "original-thread".into()),
        ("nativeOperations", IvyJson::object([("delete", "original-operation".into())])),
    ]);
    let now = "2026-11-12T05:25:28.863Z".into();
    for code in ["native_backend_required", "request_timeout"] {
        let next = CleanupAction::Delete.issue(&original, code, &now, identity_missing(code));
        for key in ["phase", "result", "errorCode", "completedAt", "threadId", "nativeOperations"] {
            assert_eq!(next.get(key), original.get(key));
        }
        assert_eq!(blocked(&next), code == "native_backend_required");
        assert_eq!(next.required("cleanup").unwrap().get("state"), Some(&if code == "request_timeout" { "outcome_unknown" } else { "blocked" }.into()));
        let mut confirmed = next;
        CleanupAction::Delete.confirmed(&mut confirmed, &now);
        assert!(!blocked(&confirmed));
        assert_eq!(confirmed.required("cleanup").unwrap().get("state"), Some(&"confirmed".into()));
        assert_eq!(confirmed.get("nativeOperations"), original.get("nativeOperations"));
    }
}
#[test]
fn historical_cleanup_requires_the_original_success_and_exact_owner_and_operation() {
    use ivy_secretary::assignment_cleanup::failed_delete_matches;
    let prior = IvyJson::object([
        ("phase", "deleting".into()), ("result", "saved answer".into()), ("errorCode", IvyJson::Null),
        ("completedAt", "2026-11-05T05:17:42.633Z".into()), ("archivedAt", "2026-11-05T05:17:43.187Z".into()),
        ("executionId", "original-execution".into()), ("serviceNodeId", "original-service".into()),
        ("threadId", "original-thread".into()), ("turnId", "original-turn".into()),
        ("nativeTarget", IvyJson::object([("serviceNodeId", "original-owner".into())])),
        ("nativeOperations", IvyJson::object([("delete", "original-delete".into())])),
    ]);
    let mut current = prior.clone(); current.insert("phase", "failed".into()); current.insert("errorCode", "native_backend_required".into());
    assert!(failed_delete_matches(&prior, &current));
    for key in ["result", "completedAt", "executionId", "nativeTarget", "nativeOperations", "threadId", "turnId"] {
        let mut changed = current.clone(); changed.insert(key, "changed".into());
        assert!(!failed_delete_matches(&prior, &changed), "{key}");
    }
    let mut failed = prior.clone(); failed.insert("errorCode", "native_turn_failed".into());
    assert!(!failed_delete_matches(&failed, &current));
}
#[tokio::test]
async fn original_execution_failure_and_retained_resource_policy() {
    let output = reference_data::read(include_bytes!("assignment-runner-cases.json.gz"));
    let corpus = parse(
        std::str::from_utf8(&output).unwrap(),
        32 * 1024 * 1024,
    )
    .unwrap();
    let policy = corpus.required("policy").unwrap().as_array().unwrap();
    assert_eq!(policy.len(), 630);
    for row in policy {
        assert_eq!(
            execution_failure_disposition(
                &row.required("code").unwrap().as_str().unwrap(),
                row.required("attempts").unwrap().as_f64().unwrap(),
                row.required("age").unwrap().as_f64().unwrap(),
            ),
            row.required("result").unwrap().as_str().unwrap(),
            "{row:?}"
        );
    }
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 82);
    for (index, row) in rows.iter().enumerate() {
        let input = row.required("input").unwrap();
        let journal = Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
        for d in input.required("documents").unwrap().as_array().unwrap() {
            let id = d.required("id").unwrap().as_str().unwrap();
            let key = d.required("key").unwrap().as_str().unwrap();
            let name = d.required("name").unwrap().as_str().unwrap();
            let revision = d.required("revision").unwrap().as_f64().unwrap() as i64;
            let value = canonical(d.required("value").unwrap(), 1_048_576).unwrap();
            journal
                .db
                .execute(
                    "INSERT INTO technical_documents VALUES(?,?,?,?,?)",
                    params![id, key, name, revision, value],
                )
                .unwrap();
            journal
                .db
                .execute(
                    "INSERT INTO technical_revisions VALUES(?,?,?)",
                    params![id, revision, value],
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
        let current = input.required("current").unwrap();
        let outcome = match input.required("kind").unwrap().as_str().unwrap().as_str() {
            "lease" => release_lease(&journal, current).map(|()| IvyJson::Null),
            "calls" => {
                release_calls(&journal, current.required("value").unwrap()).map(|()| IvyJson::Null)
            }
            "update" => {
                AssignmentRunner {
                    evidence: &evidence,
                }
                .update(current, input.required("next").unwrap())
                .await
            }
            _ => panic!("Unexpected fixture kind"),
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
                call.insert("params", params);
            }
        }
        assert!(client.steps.lock().unwrap().is_empty(), "Case {index}");
        let (remaining, revisions) = retained(&journal);
        let actual = IvyJson::object([
            ("result", result),
            ("trace", IvyJson::Array(trace)),
            ("remaining", remaining),
            ("revisions", revisions),
        ]);
        assert_eq!(
            &actual,
            row.required("expected").unwrap(),
            "Case {index}: {:?}",
            input.get("label")
        );
    }
}
