use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, json::parse};
use ivy_sdk::{clock::Clock, operation_ids::start_runtime_epoch, rpc::*};
use ivy_secretary::{
    assignment_workflow::{AssignmentWorkflow, WORKFLOW_OPERATION_KEY, WorkflowAction},
    inbox_evidence::{InboxEvidence, operation_name},
    journal::{Journal, Limits},
    store::{Store, conflict},
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Instant,
};

struct FixedClock;
impl Clock for FixedClock {
    fn utc_millis(&self) -> i64 {
        1_800_000_000_000
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
struct Scripted {
    steps: Mutex<VecDeque<(&'static str, IvyResult<IvyJson>)>>,
    trace: Mutex<Vec<(String, IvyJson)>>,
}
impl RpcClient for Scripted {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            if method == "system.status" {
                return Ok(IvyJson::object([("runtimeEpoch", "fixture-epoch".into())]));
            }
            self.trace.lock().unwrap().push((method.to_owned(), params));
            let (expected, result) = self
                .steps
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected replay");
            assert_eq!(method, expected);
            result
        })
    }
}
fn json(text: &str) -> IvyJson {
    parse(text, 100_000).unwrap()
}

#[tokio::test]
async fn raw_outcomes_preserve_durable_write_recovery_and_strict_conflict_classification() {
    for outcome in [
        r#""not_executed""#,
        r#""unknown""#,
        r#""completed""#,
        r#"["not_executed"]"#,
        r#"[["not_executed"]]"#,
        r#"["unknown"]"#,
        r#"["completed"]"#,
        "null",
        "false",
        "0",
        r#"{"value":"not_executed"}"#,
    ]
    .map(json)
    {
        let failed = outcome == IvyJson::from("not_executed");
        let failure = IvyError::new_js("revision_conflict".into(), JsString(vec![65, 0xd800]))
            .with_provider_outcome(outcome.clone())
            .with_details(IvyJson::Null);
        assert_eq!(conflict(&failure), failed);
        for recovered_receipt in [false, true] {
            let caller = IvyJson::from("caller");
            let id = IvyJson::from("fixture-epoch:1800000000000:workflow");
            let request = IvyJson::object([("operationId", id.clone()), ("value", true.into())]);
            let mutation = ivy_contracts::operation::derive_operation_id_js(
                id.as_js_string().unwrap(),
                &IvyJson::Array(vec!["secretary-workflow".into(), caller.clone()]),
            )
            .unwrap();
            let root = "00000000-0000-4000-8000-000000000001";
            let object_id = "00000000-0000-4000-8000-000000000002";
            let write = IvyJson::object([
                ("mutationId", IvyJson::String(mutation.clone())),
                (
                    "create",
                    IvyJson::object([("name", "assignment-fixture".into())]),
                ),
            ]);
            let receipt = IvyJson::object([
                (
                    "object",
                    IvyJson::object([
                        ("id", object_id.into()),
                        ("parentId", root.into()),
                        ("contractKey", "secretary/assignment".into()),
                        ("name", "assignment-fixture".into()),
                    ]),
                ),
                (
                    "revision",
                    IvyJson::object([("revision", IvyJson::Number(1.0))]),
                ),
            ]);
            let mut steps = VecDeque::from([
                ("objects.write", Err(failure.clone())),
                (
                    "objects.writeReceipt",
                    Ok(if recovered_receipt {
                        receipt
                    } else {
                        IvyJson::Null
                    }),
                ),
            ]);
            if !recovered_receipt && !failed {
                steps.push_back(("objects.write", Err(failure.clone())));
            }
            let client = Arc::new(Scripted {
                steps: Mutex::new(steps),
                trace: Mutex::new(Vec::new()),
            });
            let journal =
                Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
            let store = Store {
                client: client.clone(),
                root: root.into(),
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
            let epoch = start_runtime_epoch(client.clone());
            let workflow = AssignmentWorkflow {
                evidence: &evidence,
                runtime_epoch: &epoch,
                clock: &FixedClock,
            };
            let first = workflow
                .save(
                    &caller,
                    &request,
                    WorkflowAction::CreateAssignment,
                    Some(&write),
                )
                .await
                .unwrap_err();
            assert_eq!(first, failure);
            assert_eq!(
                first
                    .to_wire()
                    .required("data")
                    .unwrap()
                    .required("outcome")
                    .unwrap(),
                &outcome
            );
            let name = operation_name(&caller, &id).unwrap().to_utf8().unwrap();
            let saved = journal
                .named(WORKFLOW_OPERATION_KEY, &name, true)
                .unwrap()
                .unwrap();
            assert_eq!(
                saved.value.get("phase"),
                Some(&IvyJson::from(if failed { "failed" } else { "prepared" }))
            );
            assert_eq!(
                saved.value.get("errorCode"),
                Some(&if failed {
                    "revision_conflict".into()
                } else {
                    IvyJson::Null
                })
            );
            let cleanup: i64 = journal
                .db
                .query_row(
                    "SELECT count(*) FROM technical_cleanup WHERE root_id=?",
                    [&saved.pin.object_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(cleanup, i64::from(failed));
            let next = workflow
                .save(&caller, &request, WorkflowAction::CreateAssignment, None)
                .await;
            if recovered_receipt {
                assert_eq!(
                    next.unwrap(),
                    IvyJson::object([
                        ("objectId", object_id.into()),
                        ("revision", IvyJson::Number(1.0))
                    ])
                );
                assert_eq!(
                    journal
                        .named(WORKFLOW_OPERATION_KEY, &name, true)
                        .unwrap()
                        .unwrap()
                        .value
                        .get("phase"),
                    Some(&IvyJson::from("succeeded"))
                );
            } else if failed {
                assert_eq!(next.unwrap_err().code, "revision_conflict");
            } else {
                assert_eq!(next.unwrap_err(), failure);
                assert_eq!(
                    journal
                        .named(WORKFLOW_OPERATION_KEY, &name, true)
                        .unwrap()
                        .unwrap()
                        .value
                        .get("phase"),
                    Some(&IvyJson::from("prepared"))
                );
            }
            let final_cleanup: i64 = journal
                .db
                .query_row(
                    "SELECT count(*) FROM technical_cleanup WHERE root_id=?",
                    [&saved.pin.object_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(final_cleanup, i64::from(recovered_receipt || failed));
            assert!(client.steps.lock().unwrap().is_empty());
            let trace = client.trace.lock().unwrap();
            assert_eq!(trace[0], ("objects.write".to_owned(), write.clone()));
            assert_eq!(trace[1].0, "objects.writeReceipt");
            assert_eq!(
                trace[1].1.get("mutationId"),
                Some(&IvyJson::String(mutation))
            );
            assert_eq!(trace.len(), if recovered_receipt || failed { 2 } else { 3 });
            if trace.len() == 3 {
                assert_eq!(
                    trace[2],
                    (
                        "objects.write".to_owned(),
                        IvyJson::object([
                            ("create", write.required("create").unwrap().clone()),
                            ("mutationId", write.required("mutationId").unwrap().clone()),
                        ])
                    )
                );
            }
        }
    }
}
