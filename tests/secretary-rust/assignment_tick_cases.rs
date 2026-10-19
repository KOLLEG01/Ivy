use super::*;
use ivy_contracts::{IvyResult, json::canonical, operation::parse_operation_id_js};
use ivy_sdk::clock::Clock;
use ivy_secretary::{
    assignment_import::import_event_gap,
    assignment_progress::SchedulerAssignments,
    assignment_scheduler::AssignmentScheduler,
    inbox_evidence::InboxEvidence,
    inbox_recovery::RecoveryScan,
    journal::{Journal, Limits},
    store::Store,
};
use ivy_store::rusqlite::params;
use std::{
    cell::RefCell,
    sync::atomic::{AtomicUsize, Ordering},
};
struct TickClock {
    times: Vec<i64>,
    next: AtomicUsize,
    samples: Mutex<Vec<IvyJson>>,
}
impl Clock for TickClock {
    fn utc_millis(&self) -> i64 {
        let index = self
            .next
            .fetch_add(1, Ordering::SeqCst)
            .min(self.times.len() - 1);
        let time = self.times[index];
        self.samples
            .lock()
            .unwrap()
            .push(IvyJson::Number(time as f64));
        time
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
struct TickClient {
    scripted: Scripted,
    abort_after: Option<usize>,
    abort: RequestOptions,
    waiting: tokio::sync::mpsc::UnboundedSender<tokio::sync::oneshot::Sender<()>>,
}
impl RpcClient for TickClient {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let (resume, resumed) = tokio::sync::oneshot::channel();
            self.waiting.send(resume).unwrap();
            resumed.await.unwrap();
            let result = self.scripted.request(method, params, options).await;
            if self.abort_after == Some(self.scripted.trace.lock().unwrap().len()) {
                self.abort.cancellation.cancel();
            }
            result
        })
    }
}
fn technical(journal: &Journal) -> IvyJson {
    let mut statement=journal.db.prepare("SELECT key,name,current_revision,value_json FROM technical_documents ORDER BY key,name").unwrap();
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
async fn original_scheduler_tick_composition_and_catch_site_diagnostics() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut command = std::process::Command::new("node");
    command
        .arg("tests/secretary-rust/assignment-tick-oracle.mjs")
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
    let corpus = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        32 * 1024 * 1024,
    )
    .unwrap();
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 47);
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
        let cancellation = RequestOptions {
            cancel_error: IvyError::new("fixture_cancelled", "Fixture cancelled."),
            ..RequestOptions::default()
        };
        if input.get("cancel") == Some(&IvyJson::Bool(true)) {
            cancellation.cancellation.cancel();
        }
        let (waiting, mut requests) = tokio::sync::mpsc::unbounded_channel();
        let client = Arc::new(TickClient {
            scripted: Scripted {
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
            abort_after: input
                .get("cancelAfter")
                .and_then(IvyJson::as_f64)
                .map(|n| n as usize),
            abort: cancellation.clone(),
            waiting,
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
            cancellation,
        };
        let clock = TickClock {
            times: input
                .required("clockTimes")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t.as_f64().unwrap() as i64)
                .collect(),
            next: AtomicUsize::new(0),
            samples: Mutex::new(Vec::new()),
        };
        let scheduler = AssignmentScheduler {
            evidence: &evidence,
            tools: Arc::new(ToolClients::new(client.clone())),
            clock: &clock,
            fallback_execution: input.get("fallback").filter(|v| !v.is_null()),
        };
        let mut state = SchedulerAssignments {
            cursor: input.get("cursor").and_then(IvyJson::as_js_string).cloned(),
            ..SchedulerAssignments::default()
        };
        let issues = RefCell::new(RecoveryScan {
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
        });
        let mut outcomes = Vec::new();
        let mut snapshots = Vec::new();
        for _ in 0..input.required("ticks").unwrap().as_f64().unwrap() as usize {
            let mut work = Box::pin(async {
                for assignment in input.required("gapImports")?.as_array().unwrap() {
                    import_event_gap(&scheduler, assignment, &issues).await?;
                }
                state.tick_shared(&scheduler, &issues).await
            });
            let result: IvyResult<()> = loop {
                tokio::select! {
                    result = &mut work => break result,
                    Some(resume) = requests.recv() => {
                        // An independent worker can update/read the actual
                        // shared diagnostics at every suspended scheduler RPC.
                        // This would panic if a RefMut survived an await.
                        let saved = issues.borrow().issues.clone();
                        // Full diagnostic sets admit only their overflow marker.
                        // Update an existing key there rather than assuming an
                        // unrelated worker can bypass the original size limit.
                        let key = saved.first().map(|(key, _)| key.as_str()).unwrap_or("independent-worker");
                        issues.borrow_mut().issue(key, "fixture_interleaving");
                        assert!(issues.borrow().issues.iter().any(|(id, code)| id == key && code == "fixture_interleaving"));
                        issues.borrow_mut().issues = saved;
                        resume.send(()).unwrap();
                    },
                }
            };
            drop(work);
            outcomes.push(match result {
                Ok(()) => IvyJson::object([("value", IvyJson::Null)]),
                Err(e) => IvyJson::object([
                    ("code", e.code.into()),
                    ("message", e.message.into()),
                    ("outcome", e.outcome.as_str().into()),
                ]),
            });
            let mut retired: Vec<_> = state.retired.iter().collect();
            retired.sort_by(|a, b| a.0.cmp(b.0));
            snapshots.push(IvyJson::object([
                (
                    "cursor",
                    state
                        .cursor
                        .clone()
                        .map(IvyJson::String)
                        .unwrap_or(IvyJson::Null),
                ),
                (
                    "retired",
                    IvyJson::Array(
                        retired
                            .iter()
                            .map(|(id, r)| {
                                IvyJson::Array(vec![
                                    IvyJson::String((*id).clone()),
                                    IvyJson::String((*r).clone()),
                                ])
                            })
                            .collect(),
                    ),
                ),
                (
                    "issues",
                    IvyJson::Array(
                        issues
                            .borrow()
                            .issues
                            .iter()
                            .map(|(id, c)| {
                                IvyJson::Array(vec![id.clone().into(), c.clone().into()])
                            })
                            .collect(),
                    ),
                ),
                ("technical", technical(&journal)),
            ]));
        }
        let mut trace = client.scripted.trace.lock().unwrap().clone();
        let mut ids = Vec::new();
        for call in &mut trace {
            if ["objects.write", "events.unsubscribe"]
                .iter()
                .any(|m| call.get("method") == Some(&(*m).into()))
            {
                let mut p = call.required("params").unwrap().clone();
                let id = p
                    .required("mutationId")
                    .unwrap()
                    .as_js_string()
                    .unwrap()
                    .clone();
                parse_operation_id_js(&id).unwrap();
                let n = ids.iter().position(|v| v == &id).unwrap_or_else(|| {
                    ids.push(id);
                    ids.len() - 1
                });
                p.insert("mutationId", format!("<operation-{n}>").into());
                call.insert("params", p);
            }
        }
        assert!(
            client.scripted.steps.lock().unwrap().is_empty(),
            "Case {i}: {:?}",
            input.get("label")
        );
        let actual = IvyJson::object([
            ("outcomes", IvyJson::Array(outcomes)),
            ("snapshots", IvyJson::Array(snapshots)),
            ("trace", IvyJson::Array(trace)),
            (
                "nowSamples",
                IvyJson::Array(clock.samples.lock().unwrap().clone()),
            ),
        ]);
        assert_eq!(
            canonical(&actual, 32 * 1024 * 1024).unwrap(),
            canonical(row.required("expected").unwrap(), 32 * 1024 * 1024).unwrap(),
            "Case {i}: {:?}",
            input.get("label")
        );
    }
}
