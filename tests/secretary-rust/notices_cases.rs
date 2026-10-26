#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use super::*;
use ivy_contracts::{JsString, json::canonical, operation::parse_operation_id_js};
use ivy_sdk::clock::Clock;
use ivy_secretary::{
    engine::SecretaryEngine,
    inbox_evidence::InboxEvidence,
    inbox_recovery::RecoveryScan,
    journal::{Journal, Limits},
    notices::SecretaryNotices,
    store::Store,
};
use ivy_store::rusqlite::params;
use std::{
    cell::RefCell,
    sync::atomic::{AtomicI64, AtomicUsize, Ordering},
};

struct NoticeClient {
    scripted: Scripted,
    label: String,
    cancellation: RequestOptions,
    cancel_after: Option<usize>,
    calls: AtomicUsize,
}
impl RpcClient for NoticeClient {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            assert_eq!(
                self.scripted
                    .steps
                    .lock()
                    .unwrap()
                    .front()
                    .and_then(|s| s.get("method")),
                Some(&IvyJson::from(method)),
                "RPC method in {}",
                self.label,
            );
            let result = self.scripted.request(method, params, options).await;
            if self.cancel_after == Some(self.calls.fetch_add(1, Ordering::Relaxed) + 1) {
                self.cancellation.cancellation.cancel();
            }
            result
        })
    }
}
struct NoticeClock {
    at: AtomicI64,
    step: i64,
    calls: AtomicI64,
}
impl Clock for NoticeClock {
    fn utc_millis(&self) -> i64 {
        self.at.load(Ordering::Relaxed) + self.calls.fetch_add(1, Ordering::Relaxed) * self.step
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
fn text(value: &IvyJson) -> JsString {
    value.as_js_string().unwrap().clone()
}
fn sql(value: &IvyJson) -> String {
    String::from_utf16_lossy(&text(value).0)
}
fn normalize(value: &IvyJson, generated: Option<&JsString>) -> IvyJson {
    match value {
        IvyJson::String(s) if generated == Some(s) => "<new-notice-operation>".into(),
        IvyJson::Array(values) => {
            IvyJson::Array(values.iter().map(|v| normalize(v, generated)).collect())
        }
        IvyJson::Object(values) => {
            let mut next = IvyJson::Object(
                values
                    .iter()
                    .map(|(k, v)| (k.clone(), normalize(v, generated)))
                    .collect(),
            );
            let browser = value.get("method") == Some(&"events.publish".into());
            let retry = value.get("method") == Some(&"objects.write".into())
                && generated.is_some_and(|id| {
                    value
                        .get("params")
                        .and_then(|p| p.get("content"))
                        .and_then(|p| p.get("value"))
                        .and_then(|p| p.get("notice"))
                        .and_then(|p| p.get("request"))
                        .and_then(|p| p.get("operationId"))
                        == Some(&IvyJson::String(id.clone()))
                });
            if browser || retry {
                parse_operation_id_js(
                    value
                        .required("params")
                        .unwrap()
                        .required("mutationId")
                        .unwrap()
                        .as_js_string()
                        .unwrap(),
                )
                .unwrap();
                let mut params = next.required("params").unwrap().clone();
                params.insert(
                    "mutationId",
                    if browser {
                        "<browser-operation>"
                    } else {
                        "<retry-write-operation>"
                    }
                    .into(),
                );
                next.insert("params", params);
            }
            next
        }
        _ => value.clone(),
    }
}
fn difference(actual: &IvyJson, expected: &IvyJson, label: &str) -> Option<String> {
    let a = canonical(actual, 32 * 1024 * 1024).unwrap();
    let b = canonical(expected, 32 * 1024 * 1024).unwrap();
    if a != b {
        let offset = a
            .bytes()
            .zip(b.bytes())
            .position(|(a, b)| a != b)
            .unwrap_or(a.len().min(b.len()));
        let from = offset.saturating_sub(100);
        return Some(format!(
            "{label}: first difference at byte {offset}\nactual: {}\nexpected: {}",
            String::from_utf8_lossy(&a.as_bytes()[from..a.len().min(offset + 1000)]),
            String::from_utf8_lossy(&b.as_bytes()[from..b.len().min(offset + 1000)])
        ));
    }
    None
}
#[tokio::test]
async fn original_notice_main_voice_recovery_and_retry_boundaries() {
    let output = reference_data::read(include_bytes!("notices-cases.json.gz"));
    let corpus = parse(
        std::str::from_utf8(&output).unwrap(),
        32 * 1024 * 1024,
    )
    .unwrap();
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 195);
    let mut failures = Vec::new();
    for row in rows {
        let input = row.required("input").unwrap();
        let label = sql(input.required("label").unwrap());
        let steps = input.required("steps").unwrap().as_array().unwrap();
        let cancellation = RequestOptions {
            cancel_error: IvyError::new("fixture_cancelled", "Fixture cancelled."),
            ..RequestOptions::default()
        };
        if input.get("cancelled") == Some(&true.into()) {
            cancellation.cancellation.cancel();
        }
        let client = Arc::new(NoticeClient {
            label: label.clone(),
            scripted: Scripted {
                steps: Mutex::new(steps.iter().cloned().collect()),
                trace: Mutex::default(),
            },
            cancellation: cancellation.clone(),
            cancel_after: input
                .get("cancelAfter")
                .and_then(IvyJson::as_f64)
                .map(|n| n as usize),
            calls: AtomicUsize::new(0),
        });
        let journal = Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
        for (array, technical) in [("locals", false), ("technical", true)] {
            for d in input.required(array).unwrap().as_array().unwrap() {
                let id = sql(d.required("id").unwrap());
                let key = sql(d.required("key").unwrap());
                let name = sql(d.required("name").unwrap());
                let encoded = canonical(d.required("value").unwrap(), 32 * 1024 * 1024).unwrap();
                if technical {
                    journal
                        .db
                        .execute(
                            "INSERT INTO technical_documents VALUES(?,?,?,?,?)",
                            params![id, key, name, 1, encoded],
                        )
                        .unwrap();
                    journal
                        .db
                        .execute(
                            "INSERT INTO technical_revisions VALUES(?,?,?)",
                            params![id, 1, encoded],
                        )
                        .unwrap();
                } else {
                    journal
                        .db
                        .execute(
                            "INSERT INTO documents VALUES(?,?,?,?,?,?,?)",
                            params![id, key, name, 1, 0, Option::<i64>::None, encoded],
                        )
                        .unwrap();
                    journal
                        .db
                        .execute(
                            "INSERT INTO revisions VALUES(?,?,?)",
                            params![id, 1, encoded],
                        )
                        .unwrap();
                }
            }
        }
        let store = Store {
            client: client.clone(),
            root: text(corpus.required("rootId").unwrap()),
            journal: &journal,
        };
        let node = "fixture.secretary".into();
        let evidence = InboxEvidence {
            store: &store,
            settings: input.required("settings").unwrap(),
            service_node_id: &node,
            cancellation,
        };
        let clock = NoticeClock {
            at: AtomicI64::new(input.required("time").unwrap().as_f64().unwrap() as i64),
            step: input.required("timeStep").unwrap().as_f64().unwrap() as i64,
            calls: AtomicI64::new(0),
        };
        let tools = ToolClients::new(client.clone());
        let mut initial = RecoveryScan::default();
        initial.issues = input
            .required("initialIssues")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                let pair = r.as_array().unwrap();
                (sql(&pair[0]), sql(&pair[1]))
            })
            .collect();
        let issues = RefCell::new(initial);
        let engine = SecretaryEngine {
            evidence: &evidence,
            clock: &clock,
            operation_clock: &clock,
            runtime_epoch: epoch_fixture(corpus.required("epoch").unwrap().as_js_string().unwrap()),
            new_uuid: || "00000000-0000-4000-8000-000000000fff".into(),
            tools: &tools,
            issues: &issues,
        };
        let mut results = Vec::new();
        let notices = SecretaryNotices::new(
            &engine,
            input.required("minimumAge").unwrap().as_f64().unwrap(),
        );
        match notices {
            Ok(notices) => {
                for action in input.required("actions").unwrap().as_array().unwrap() {
                    if let Some(time) = action.get("time").and_then(IvyJson::as_f64) {
                        clock.at.store(time as i64, Ordering::Relaxed);
                    }
                    results.push(outcome(
                        notices
                            .step(&"00000000-0000-4000-8000-000000000002".into())
                            .await,
                    ));
                }
            }
            Err(error) => results.push(outcome(Err(error))),
        }
        let trace = client.scripted.trace.lock().unwrap().clone();
        assert_eq!(trace.len(), steps.len(), "RPC count in {label}");
        let mut generated = None;
        let history = trace
            .iter()
            .zip(steps)
            .filter_map(|(call, step)| {
                if step.get("effect") != Some(&true.into()) {
                    return None;
                }
                let params = call.required("params").unwrap();
                let value = params
                    .required("content")
                    .unwrap()
                    .required("value")
                    .unwrap();
                if input.get("randomRetry") == Some(&true.into()) {
                    let id = text(
                        value
                            .required("notice")
                            .unwrap()
                            .required("request")
                            .unwrap()
                            .required("operationId")
                            .unwrap(),
                    );
                    parse_operation_id_js(&id).unwrap();
                    generated = Some(id);
                }
                Some(IvyJson::object([
                    (
                        "revision",
                        IvyJson::Number(
                            params
                                .required("expectedRevision")
                                .unwrap()
                                .as_f64()
                                .unwrap()
                                + 1.0,
                        ),
                    ),
                    ("value", value.clone()),
                ]))
            })
            .collect();
        let mut statement = journal.db.prepare("SELECT id,key,name,current_revision,value_json FROM technical_documents ORDER BY key,name").unwrap();
        let records = statement
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
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let technical = records.into_iter().map(|(id,key,name,revision,value)| {
            let mut statement = journal.db.prepare("SELECT revision,value_json FROM technical_revisions WHERE id=? ORDER BY revision").unwrap();
            let history = statement.query_map(params![id], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?))).unwrap().map(|r| {
                let (revision,value)=r.unwrap(); IvyJson::object([("revision", IvyJson::Number(revision as f64)),("value",parse(&value,32*1024*1024).unwrap())])
            }).collect();
            IvyJson::object([("key",key.into()),("name",name.into()),("revision",IvyJson::Number(revision as f64)),("value",parse(&value,32*1024*1024).unwrap()),("history",IvyJson::Array(history))])
        }).collect();
        let actual = IvyJson::object([
            ("results", IvyJson::Array(results)),
            ("trace", IvyJson::Array(trace)),
            ("history", IvyJson::Array(history)),
            ("technical", IvyJson::Array(technical)),
            (
                "issues",
                IvyJson::Array(
                    issues
                        .borrow()
                        .issues
                        .iter()
                        .map(|(id, code)| {
                            IvyJson::Array(vec![id.clone().into(), code.clone().into()])
                        })
                        .collect(),
                ),
            ),
            (
                "clockCalls",
                IvyJson::Number(clock.calls.load(Ordering::Relaxed) as f64),
            ),
        ]);
        assert!(
            client.scripted.steps.lock().unwrap().is_empty(),
            "Unconsumed RPC in {label}"
        );
        if let Some(failure) = difference(
            &normalize(&actual, generated.as_ref()),
            row.required("expected").unwrap(),
            &label,
        ) {
            failures.push(failure);
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
fn outcome(result: ivy_contracts::IvyResult<()>) -> IvyJson {
    match result {
        Ok(()) => IvyJson::object([("value", IvyJson::Null)]),
        Err(error) => IvyJson::object([
            ("code", error.code.into()),
            ("message", error.message.into()),
            ("outcome", error.outcome.to_json()),
        ]),
    }
}
