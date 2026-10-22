use super::*;
use base64::Engine;
use ivy_contracts::{IvyResult, JsString, json::canonical};
use ivy_sdk::clock::Clock;
use ivy_secretary::{
    assignment_import::{
        ImportedEventGap, ImportedSchedule, ImportedSubscription, SubscriptionAssignment,
        import_event_gap, import_event_subscription, import_schedule_progress,
    },
    assignment_scheduler::AssignmentScheduler,
    inbox_evidence::InboxEvidence,
    inbox_recovery::RecoveryScan,
    journal::{Document, Journal, Limits},
    store::Store,
};
use std::{cell::RefCell, path::PathBuf, process::Command};

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct ImportClock(i64);
impl Clock for ImportClock {
    fn utc_millis(&self) -> i64 {
        self.0
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
struct Restarted(Arc<Scripted>);
impl RpcClient for Restarted {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        self.0.request(method, params, options)
    }
}
fn document(value: Document) -> IvyJson {
    IvyJson::object([
        (
            "pin",
            IvyJson::object([
                ("objectId", value.pin.object_id.into()),
                ("revision", IvyJson::Number(value.pin.revision as f64)),
            ]),
        ),
        ("name", value.name.into()),
        ("value", value.value),
    ])
}
fn schedule(value: ImportedSchedule) -> IvyJson {
    IvyJson::object([
        ("source", document(value.source)),
        ("before", value.before.unwrap_or(IvyJson::Null)),
        ("after", value.after),
    ])
}
fn gap(value: ImportedEventGap) -> IvyJson {
    IvyJson::object([
        ("before", document(value.before)),
        ("after", document(value.after)),
    ])
}
fn subscription(value: ImportedSubscription) -> IvyJson {
    IvyJson::object([
        ("source", document(value.source)),
        ("before", value.before.unwrap_or(IvyJson::Null)),
        ("after", value.after.unwrap_or(IvyJson::Null)),
        ("completed", value.completed.into()),
    ])
}
fn result(value: IvyResult<IvyJson>) -> IvyJson {
    match value {
        Ok(value) => IvyJson::object([("value", value)]),
        Err(error) => IvyJson::object([(
            "error",
            IvyJson::object([
                ("code", IvyJson::String(error.code_js())),
                ("message", IvyJson::String(error.message_js())),
                ("outcome", error.outcome.to_json()),
            ]),
        )]),
    }
}
fn original_tables(mut value: IvyJson) -> IvyJson {
    let mut tables = value
        .required("tables")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    for table in &mut tables {
        if table.get("name") != Some(&"journal_state".into()) {
            continue;
        }
        let rows = table
            .required("rows")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| {
                // SQLite hex(CAST(key AS BLOB)): only new import receipts are
                // compared as parsed values below, preserving ID normalization.
                !row.as_array().unwrap()[3]
                    .as_str()
                    .unwrap()
                    .starts_with("7365637265746172792F696D706F72742F")
            })
            .cloned()
            .collect();
        table.insert("rows", IvyJson::Array(rows));
    }
    value.insert("tables", IvyJson::Array(tables));
    value
}
fn receipts(journal: &Journal) -> IvyJson {
    let mut query = journal
        .db
        .prepare(
            "SELECT key,value FROM journal_state WHERE key LIKE 'secretary/import/%' ORDER BY key",
        )
        .unwrap();
    let rows = query
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap();
    IvyJson::Array(
        rows.map(|r| {
            let (key, value) = r.unwrap();
            IvyJson::object([
                ("key", key.into()),
                ("value", parse(&value, 33_554_432).unwrap()),
            ])
        })
        .collect(),
    )
}
fn normalize(value: &IvyJson, ids: &[JsString]) -> IvyJson {
    if let Some(text) = value.as_js_string() {
        if let Some(index) = ids.iter().position(|id| id == text) {
            return format!("<operation-{index}>").into();
        }
    }
    match value {
        IvyJson::Array(values) => {
            IvyJson::Array(values.iter().map(|v| normalize(v, ids)).collect())
        }
        IvyJson::Object(values) => IvyJson::Object(
            values
                .iter()
                .map(|(k, v)| (k.clone(), normalize(v, ids)))
                .collect(),
        ),
        _ => value.clone(),
    }
}
fn retained_rows(before: &IvyJson, after: &IvyJson, gap_import: bool) {
    assert_eq!(
        before.required("schema").unwrap(),
        after.required("schema").unwrap()
    );
    for table in before.required("tables").unwrap().as_array().unwrap() {
        let next = after
            .required("tables")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t.get("name") == table.get("name"))
            .unwrap();
        for row in table.required("rows").unwrap().as_array().unwrap() {
            // Only the converted gap's current head can change. Every original
            // revision (including its old current value) must still be present.
            if gap_import
                && table.get("name") == Some(&"technical_documents".into())
                && row.as_array().unwrap()[3].as_str().unwrap() == "6C65676163792D696D706F7274"
            {
                continue;
            }
            assert!(
                next.required("rows")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .contains(row),
                "Retained original row in {:?}",
                table.get("name")
            );
        }
    }
}

#[tokio::test]
async fn explicit_assignment_import_copies_preserve_history_and_frozen_retirement_on_restart() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut command = Command::new("node");
    command
        .arg("tests/secretary-rust/assignment-import-oracle.mjs")
        .current_dir(root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().unwrap();
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
    assert_eq!(rows.len(), 35);
    for row in rows {
        let input = row.required("input").unwrap();
        let expected = row.required("expected").unwrap();
        let directory = Directory(std::env::temp_dir().join(format!(
            "secretary-assignment-import-{}",
            uuid::Uuid::new_v4()
        )));
        std::fs::create_dir(&directory.0).unwrap();
        let source = directory.0.join("source.sqlite");
        let copy = directory.0.join("copy.sqlite");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(input.required("coreBase64").unwrap().as_str().unwrap())
            .unwrap();
        std::fs::write(&source, &bytes).unwrap();
        std::fs::copy(&source, &copy).unwrap();
        let scripted = Arc::new(Scripted {
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
        let mut results = vec![];
        let mut snapshots = vec![];
        let mut initial = None;
        let mut event_issues = RecoveryScan::default();
        let gap_issues = RefCell::new(RecoveryScan::default());
        let clock = ImportClock(input.required("time").unwrap().as_f64().unwrap() as i64);
        for _ in 0..input.required("attempts").unwrap().as_f64().unwrap() as usize {
            let limits = input.required("limits").unwrap();
            let journal = Journal::open(
                &copy,
                Limits {
                    count: limits.required("count").unwrap().as_f64().unwrap() as i64,
                    bytes: limits.required("bytes").unwrap().as_f64().unwrap() as i64,
                },
            )
            .unwrap();
            if initial.is_none() {
                initial = Some(original_tables(super::execution_import_cases::snapshot(
                    &journal.db,
                )));
            }
            let client = Arc::new(Restarted(scripted.clone()));
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
                ..Default::default()
            };
            if input.get("cancel") == Some(&true.into()) {
                cancellation.cancellation.cancel();
            }
            let node = "fixture.secretary".into();
            let evidence = InboxEvidence {
                store: &store,
                settings: input.required("settings").unwrap(),
                service_node_id: &node,
                cancellation,
            };
            let scheduler = AssignmentScheduler {
                evidence: &evidence,
                tools: Arc::new(ToolClients::new(client)),
                clock: &clock,
                fallback_execution: None,
            };
            let mode = input.required("mode").unwrap().as_str().unwrap();
            let value = match mode.as_str() {
                "schedule" => import_schedule_progress(
                    &scheduler,
                    input.required("assignment").unwrap(),
                    clock.0,
                )
                .await
                .map(|v| v.map(schedule).unwrap_or(IvyJson::Null)),
                "gap" => import_event_gap(
                    &scheduler,
                    input.required("assignment").unwrap(),
                    &gap_issues,
                )
                .await
                .map(|v| v.map(gap).unwrap_or(IvyJson::Null)),
                _ => {
                    let selected = if mode == "archived" {
                        SubscriptionAssignment::Archived(input.required("archiveRow").unwrap())
                    } else {
                        SubscriptionAssignment::Active {
                            assignment: input.required("assignment").unwrap(),
                            configuration: input.required("configuration").unwrap(),
                        }
                    };
                    import_event_subscription(&scheduler, selected, &mut event_issues)
                        .await
                        .map(|v| v.map(subscription).unwrap_or(IvyJson::Null))
                }
            };
            results.push(result(value));
            let core = original_tables(super::execution_import_cases::snapshot(&journal.db));
            retained_rows(initial.as_ref().unwrap(), &core, mode == "gap");
            let issues = if mode == "gap" {
                gap_issues.borrow().issues.clone()
            } else {
                event_issues.issues.clone()
            };
            snapshots.push(IvyJson::object([
                ("core", core),
                ("receipts", receipts(&journal)),
                (
                    "issues",
                    IvyJson::Array(
                        issues
                            .into_iter()
                            .map(|(k, v)| IvyJson::Array(vec![k.into(), v.into()]))
                            .collect(),
                    ),
                ),
            ]));
            assert_eq!(
                journal
                    .db
                    .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                37
            );
        }
        let trace = IvyJson::Array(scripted.trace.lock().unwrap().clone());
        let mut ids: Vec<JsString> = vec![];
        for call in trace.as_array().unwrap() {
            if ["objects.write", "events.unsubscribe"]
                .iter()
                .any(|m| call.get("method") == Some(&(*m).into()))
            {
                let id = call
                    .required("params")
                    .unwrap()
                    .required("mutationId")
                    .unwrap()
                    .as_js_string()
                    .unwrap()
                    .clone();
                ivy_contracts::operation::parse_operation_id_js(&id).unwrap();
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        for snapshot in &snapshots {
            for receipt in snapshot.required("receipts").unwrap().as_array().unwrap() {
                for request in receipt
                    .required("value")
                    .unwrap()
                    .get("requests")
                    .and_then(IvyJson::as_array)
                    .unwrap_or(&[])
                {
                    let id = request
                        .required("mutationId")
                        .unwrap()
                        .as_js_string()
                        .unwrap()
                        .clone();
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
            }
        }
        let actual = normalize(
            &IvyJson::object([
                ("results", IvyJson::Array(results)),
                ("snapshots", IvyJson::Array(snapshots)),
                ("trace", trace),
                ("before", initial.unwrap()),
            ]),
            &ids,
        );
        let mut expected = expected.clone();
        expected.remove("history");
        assert_eq!(
            canonical(&actual, 32 * 1024 * 1024).unwrap(),
            canonical(&expected, 32 * 1024 * 1024).unwrap(),
            "Assignment import {:?}",
            input.get("label")
        );
        assert!(scripted.steps.lock().unwrap().is_empty());
        assert_eq!(
            std::fs::read(&source).unwrap(),
            bytes,
            "Closed original source remains untouched"
        );
    }
}
