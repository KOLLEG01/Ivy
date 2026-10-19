use super::*;
use ivy_contracts::{json::canonical, operation::parse_operation_id_js};
use ivy_sdk::clock::Clock;
use ivy_secretary::{
    assignment_import::{
        SubscriptionAssignment, import_event_subscription, import_schedule_progress,
    },
    assignment_progress::SchedulerAssignments,
    assignment_scheduler::AssignmentScheduler,
    inbox_evidence::InboxEvidence,
    inbox_recovery::RecoveryScan,
    journal::{Journal, Limits},
    store::Store,
};
struct ProgressClock(i64);
impl Clock for ProgressClock {
    fn utc_millis(&self) -> i64 {
        self.0
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
fn corpus() -> IvyJson {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut command = std::process::Command::new("node");
    command
        .arg("tests/secretary-rust/assignment-progress-oracle.mjs")
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
    parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        32 * 1024 * 1024,
    )
    .unwrap()
}
fn outcome(value: Result<IvyJson, IvyError>) -> IvyJson {
    match value {
        Ok(value) => IvyJson::object([("value", value)]),
        Err(e) => {
            let mut result = IvyJson::object([
                ("code", e.code.clone().into()),
                ("message", e.message.into()),
            ]);
            if e.code != "secretary_execution_unconfigured" {
                result.insert("outcome", e.outcome.as_str().into());
            }
            result
        }
    }
}
#[test]
fn original_schedule_occurrences_with_shared_time_zone() {
    let corpus = corpus();
    let journal = Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
    let client = Arc::new(Scripted {
        steps: Mutex::new(VecDeque::new()),
        trace: Mutex::new(Vec::new()),
    });
    let store = Store {
        client: client.clone(),
        root: "root".into(),
        journal: &journal,
    };
    let service_id = "fixture.secretary".into();
    let evidence = InboxEvidence {
        store: &store,
        settings: corpus.required("settings").unwrap(),
        service_node_id: &service_id,
        cancellation: RequestOptions::default(),
    };
    let clock = ProgressClock(0);
    let scheduler = AssignmentScheduler {
        evidence: &evidence,
        tools: Arc::new(ToolClients::new(client)),
        clock: &clock,
        fallback_execution: None,
    };
    let rows = corpus.required("occurrences").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 59);
    for (i, row) in rows.iter().enumerate() {
        let actual = outcome(scheduler.occurrences(
            row.required("assignment").unwrap(),
            row.required("time").unwrap().as_f64().unwrap() as i64,
            row.required("checkedAt").unwrap().as_js_string().unwrap(),
        ));
        assert_eq!(
            canonical(&actual, 4 * 1024 * 1024).unwrap(),
            canonical(row.required("expected").unwrap(), 4 * 1024 * 1024).unwrap(),
            "Occurrence {i}"
        );
    }
}
#[tokio::test]
async fn original_assignment_retirement_and_durable_schedule_progress() {
    let corpus = corpus();
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 46);
    for (i, row) in rows.iter().enumerate() {
        let input = row.required("input").unwrap();
        let journal = Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
        for doc in input.required("technical").unwrap().as_array().unwrap() {
            journal
                .create_technical(
                    &doc.required("key").unwrap().as_str().unwrap(),
                    &doc.required("name").unwrap().as_str().unwrap(),
                    doc.required("value").unwrap(),
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
        let cancellation = RequestOptions::default();
        if input.get("cancel") == Some(&IvyJson::Bool(true)) {
            cancellation.cancellation.cancel();
        }
        let service_id = "fixture.secretary".into();
        let evidence = InboxEvidence {
            store: &store,
            settings: input.required("settings").unwrap(),
            service_node_id: &service_id,
            cancellation,
        };
        let clock = ProgressClock(input.required("time").unwrap().as_f64().unwrap() as i64);
        let scheduler = AssignmentScheduler {
            evidence: &evidence,
            tools: Arc::new(ToolClients::new(client.clone())),
            clock: &clock,
            fallback_execution: None,
        };
        let mut scan = SchedulerAssignments {
            cursor: input.get("cursor").and_then(IvyJson::as_js_string).cloned(),
            ..SchedulerAssignments::default()
        };
        for pair in input.required("retired").unwrap().as_array().unwrap() {
            let pair = pair.as_array().unwrap();
            scan.retired.insert(
                pair[0].as_js_string().unwrap().clone(),
                pair[1].as_js_string().unwrap().clone(),
            );
        }
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
        let result: Result<IvyJson, IvyError> = async {
            if let Some(row) = input.get("archiveImportRow").filter(|v| !v.is_null()) {
                import_event_subscription(
                    &scheduler,
                    SubscriptionAssignment::Archived(row),
                    &mut issues,
                )
                .await?;
            }
            if input.get("action") != Some(&"assignments".into())
                && input
                    .required("technical")?
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|d| d.get("key") == Some(&"secretary/schedule-cursor".into()))
            {
                import_schedule_progress(&scheduler, input.required("assignment")?, clock.0)
                    .await?;
            }
            let mut results = Vec::new();
            for _ in 0..input.required("scans")?.as_f64().unwrap() as usize {
                if input.get("action") == Some(&"assignments".into()) {
                    results.push(IvyJson::Array(
                        scan.assignments(&scheduler, &mut issues).await?,
                    ));
                } else {
                    scheduler
                        .schedules(
                            input.required("assignment")?,
                            input.required("configuration")?,
                            clock.0,
                        )
                        .await?;
                }
            }
            Ok(if input.get("action") == Some(&"assignments".into()) {
                IvyJson::Array(results)
            } else {
                IvyJson::Null
            })
        }
        .await;
        let mut trace = client.trace.lock().unwrap().clone();
        let mut mutation_ids = Vec::new();
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
                let selected = mutation_ids
                    .iter()
                    .position(|p| p == &id)
                    .unwrap_or_else(|| {
                        mutation_ids.push(id);
                        mutation_ids.len() - 1
                    });
                params.insert(
                    "mutationId",
                    format!("<scoped-operation-{selected}>").into(),
                );
                call.insert("params", params);
            }
        }
        let mut retired: Vec<_> = scan.retired.into_iter().collect();
        retired.sort_by(|a, b| a.0.cmp(&b.0));
        let state = IvyJson::object([
            (
                "cursor",
                scan.cursor.map(IvyJson::String).unwrap_or(IvyJson::Null),
            ),
            (
                "retired",
                IvyJson::Array(
                    retired
                        .into_iter()
                        .map(|(id, r)| {
                            IvyJson::Array(vec![IvyJson::String(id), IvyJson::String(r)])
                        })
                        .collect(),
                ),
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
        let mut keys = Vec::new();
        for doc in input.required("technical").unwrap().as_array().unwrap() {
            let key = doc.required("key").unwrap().as_str().unwrap();
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        let mut technical = Vec::new();
        for key in keys {
            for doc in journal.list(&key, true).unwrap() {
                technical.push(IvyJson::object([
                    ("key", key.clone().into()),
                    ("name", doc.name.into()),
                    ("value", doc.value),
                ]));
            }
        }
        technical.sort_by_key(|d| d.required("name").unwrap().as_str().unwrap());
        assert!(
            client.steps.lock().unwrap().is_empty(),
            "Case {i}: {:?}",
            input.get("label")
        );
        let actual = IvyJson::object([
            ("result", outcome(result)),
            ("trace", IvyJson::Array(trace)),
            ("state", state),
            ("technical", IvyJson::Array(technical)),
        ]);
        assert_eq!(
            canonical(&actual, 32 * 1024 * 1024).unwrap(),
            canonical(row.required("expected").unwrap(), 32 * 1024 * 1024).unwrap(),
            "Case {i}: {:?}",
            input.get("label")
        );
    }
}
