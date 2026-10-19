use super::microsoft_store_cases::{normalize, retained};
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{IvyResult, JsString, json::canonical, operation::parse_operation_id_js};
use ivy_sdk::clock::Clock;
use ivy_secretary::{
    engine::SecretaryEngine,
    inbox_evidence::InboxEvidence,
    inbox_recovery::RecoveryScan,
    journal::{Journal, Limits},
    media_spool::SpoolJournal,
    outlook_media_native::{
        DownloadFuture, DownloadedMedia, MediaNativeValidation, OutlookMediaDownloader,
        OutlookMediaNative,
    },
    outlook_source::OutlookSource,
    store::Store,
};
use ivy_store::rusqlite::{Connection, params};
use std::{cell::RefCell, path::Path, process::Command, rc::Rc};

thread_local! { static SPOOL: RefCell<Option<Rc<RefCell<SpoolJournal<MediaNativeValidation>>>>> = const { RefCell::new(None) }; }
fn spool_is_released() {
    SPOOL.with(|slot| {
        if let Some(spool) = slot.borrow().as_ref() {
            assert!(
                spool.try_borrow_mut().is_ok(),
                "A spool borrow crossed an async boundary"
            );
        }
    });
}
struct Fixed(i64);
impl Clock for Fixed {
    fn utc_millis(&self) -> i64 {
        self.0
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
struct Yielding {
    scripted: Scripted,
    label: String,
}
impl RpcClient for Yielding {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            spool_is_released();
            tokio::task::yield_now().await;
            spool_is_released();
            assert_eq!(
                self.scripted
                    .steps
                    .lock()
                    .unwrap()
                    .front()
                    .unwrap()
                    .get("method"),
                Some(&method.into()),
                "{} after {} calls",
                self.label,
                self.scripted.trace.lock().unwrap().len()
            );
            self.scripted.request(method, params, options).await
        })
    }
}
struct Downloader {
    pending: RefCell<VecDeque<IvyJson>>,
}
impl OutlookMediaDownloader for Downloader {
    fn download<'a>(
        &'a self,
        materialization: &'a IvyJson,
        _: &'a RequestOptions,
    ) -> DownloadFuture<'a> {
        Box::pin(async move {
            spool_is_released();
            tokio::task::yield_now().await;
            spool_is_released();
            let original = self
                .pending
                .borrow_mut()
                .pop_front()
                .expect("An extra acquisition was attempted");
            assert_eq!(
                canonical(materialization, 32 * 1024 * 1024).unwrap(),
                canonical(original.required("materialization")?, 32 * 1024 * 1024).unwrap()
            );
            if let Some(code) = original.get("errorCode") {
                return Err(IvyError::new(
                    code.as_str()?,
                    "Original synthetic download failure.",
                ));
            }
            Ok(DownloadedMedia {
                bytes: STANDARD
                    .decode(original.required("bytes")?.as_str()?)
                    .unwrap(),
                evidence: original.required("evidence")?.clone(),
            })
        })
    }
}
fn result(value: IvyResult<IvyJson>) -> IvyJson {
    match value {
        Ok(value) => IvyJson::object([("value", value)]),
        Err(error) => {
            let mut value = IvyJson::object([
                ("code", IvyJson::String(error.code_js())),
                ("message", IvyJson::String(error.message_js())),
                ("outcome", error.outcome.as_str().into()),
            ]);
            if let Some(details) = error.details {
                value.insert("details", details);
            }
            value
        }
    }
}
fn snapshot(
    spool: &SpoolJournal<MediaNativeValidation>,
    plan: &IvyJson,
    journal: &Journal,
) -> IvyJson {
    let mut query = spool
        .db
        .prepare("SELECT value,seen,observation FROM read_calls ORDER BY plan_hash,slot")
        .unwrap();
    let calls = query
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .unwrap()
        .map(|row| {
            let (call, seen, observation) = row.unwrap();
            IvyJson::object([
                ("call", parse(&call, 32 * 1024 * 1024).unwrap()),
                (
                    "seen",
                    seen.map(|value| parse(&value, 32 * 1024 * 1024).unwrap())
                        .unwrap_or(IvyJson::Null),
                ),
                (
                    "observation",
                    observation
                        .map(|value| parse(&value, 32 * 1024 * 1024).unwrap())
                        .unwrap_or(IvyJson::Null),
                ),
            ])
        })
        .collect();
    let mut query=spool.db.prepare("SELECT operation_id,intent,materialization,phase,evidence,payload,error_code FROM attempts ORDER BY operation_id").unwrap();
    let attempts = query
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<Vec<u8>>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })
        .unwrap()
        .map(|row| {
            let (id, intent, materialization, phase, evidence, bytes, error) = row.unwrap();
            IvyJson::object([
                ("operationId", id.into()),
                ("intent", parse(&intent, 32 * 1024 * 1024).unwrap()),
                (
                    "materialization",
                    parse(&materialization, 32 * 1024 * 1024).unwrap(),
                ),
                ("phase", phase.into()),
                (
                    "evidence",
                    evidence
                        .map(|value| parse(&value, 32 * 1024 * 1024).unwrap())
                        .unwrap_or(IvyJson::Null),
                ),
                (
                    "bytes",
                    bytes
                        .map(|bytes| STANDARD.encode(bytes).into())
                        .unwrap_or(IvyJson::Null),
                ),
                (
                    "errorCode",
                    error.map(IvyJson::from).unwrap_or(IvyJson::Null),
                ),
            ])
        })
        .collect();
    let (technical, _) = retained(journal);
    IvyJson::object([
        ("calls", IvyJson::Array(calls)),
        ("attempts", IvyJson::Array(attempts)),
        (
            "plan",
            spool
                .read_plan(plan.required("item").unwrap())
                .unwrap()
                .unwrap_or(IvyJson::Null),
        ),
        (
            "usedBytes",
            IvyJson::Number(spool.used_bytes().unwrap() as f64),
        ),
        (
            "technical",
            technical.required("documents").unwrap().clone(),
        ),
    ])
}

#[tokio::test]
async fn original_outlook_media_native_account_cleanup_recovery_and_coverage() {
    let mut command = Command::new("node");
    command
        .arg("tests/secretary-rust/outlook-media-native-oracle.mjs")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command
        .output()
        .expect("Original media native oracle required");
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
        let label = input.required("label").unwrap().as_str().unwrap();
        let journal = Journal::open(Path::new(":memory:"), Limits::default()).unwrap();
        for row in input.required("locals").unwrap().as_array().unwrap() {
            let text = |key| row.required(key).unwrap().as_str().unwrap();
            journal
                .db
                .execute(
                    "INSERT INTO documents VALUES(?,?,?,?,?,?,?)",
                    params![
                        text("id"),
                        text("key"),
                        text("name"),
                        row.required("revision").unwrap().as_f64().unwrap() as i64,
                        0,
                        if text("key") == "secretary/operation" {
                            Some(1i64)
                        } else {
                            None
                        },
                        canonical(row.required("value").unwrap(), 32 * 1024 * 1024).unwrap()
                    ],
                )
                .unwrap();
            for revision in row.required("history").unwrap().as_array().unwrap() {
                journal
                    .db
                    .execute(
                        "INSERT INTO revisions VALUES(?,?,?)",
                        params![
                            text("id"),
                            revision.required("revision").unwrap().as_f64().unwrap() as i64,
                            canonical(revision.required("value").unwrap(), 32 * 1024 * 1024)
                                .unwrap()
                        ],
                    )
                    .unwrap();
            }
        }
        let spool = Rc::new(RefCell::new(
            SpoolJournal::from_owned_connection(
                Connection::open_in_memory().unwrap(),
                input.required("spoolOwner").unwrap(),
                MediaNativeValidation,
            )
            .unwrap(),
        ));
        let initial = input.required("spool").unwrap();
        if !initial.required("plan").unwrap().is_null() {
            spool
                .borrow()
                .prepare_read(initial.required("plan").unwrap())
                .unwrap();
        }
        for row in initial.required("calls").unwrap().as_array().unwrap() {
            let call = row.required("call").unwrap();
            let value = |key| {
                row.get(key)
                    .filter(|value| !value.is_null())
                    .map(|value| canonical(value, 32 * 1024 * 1024).unwrap())
            };
            spool
                .borrow()
                .db
                .execute(
                    "INSERT INTO read_calls VALUES(?,?,?,?,?)",
                    params![
                        call.required("planHash").unwrap().as_str().unwrap(),
                        call.required("slot").unwrap().as_f64().unwrap() as i64,
                        canonical(call, 32 * 1024 * 1024).unwrap(),
                        value("seen"),
                        value("observation")
                    ],
                )
                .unwrap();
        }
        SPOOL.with(|slot| *slot.borrow_mut() = Some(spool.clone()));
        let client = Arc::new(Yielding {
            label: label.clone(),
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
                trace: Mutex::default(),
            },
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
        let mut cancellation = RequestOptions::default();
        if input.get("cancelled") == Some(&true.into()) {
            cancellation.cancellation.cancel();
            cancellation.cancel_error = IvyError::new("fixture_cancelled", "Fixture cancelled.");
        }
        let evidence = InboxEvidence {
            store: &store,
            settings: input.required("settings").unwrap(),
            service_node_id: corpus.required("nodeId").unwrap().as_js_string().unwrap(),
            cancellation,
        };
        let clock = Fixed(corpus.required("time").unwrap().as_f64().unwrap() as i64);
        let wall = Fixed(corpus.required("wall").unwrap().as_f64().unwrap() as i64);
        let tools = Arc::new(ToolClients::new(client.clone()));
        let issues = RefCell::new(RecoveryScan::default());
        let engine = SecretaryEngine {
            evidence: &evidence,
            clock: &clock,
            operation_clock: &wall,
            runtime_epoch: epoch_fixture(&"fixture-epoch".into()),
            new_uuid: || "00000000-0000-4000-8000-000000000fff".into(),
            tools: &tools,
            issues: &issues,
        };
        let source = OutlookSource::new(
            &engine,
            input.required("configuration").unwrap(),
            tools.clone(),
        )
        .unwrap();
        let downloader = Downloader {
            pending: RefCell::new(
                input
                    .required("downloads")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect(),
            ),
        };
        let worker = OutlookMediaNative::new(&source, &spool, &downloader);
        let plan = input.required("plan").unwrap();
        let mut results = vec![];
        for action in input.required("actions").unwrap().as_array().unwrap() {
            results.push(result(
                match action
                    .required("method")
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .as_str()
                {
                    "prepare" => {
                        worker
                            .prepare(
                                plan.required("admission").unwrap(),
                                plan.required("item").unwrap(),
                                plan.required("captureOperationId").unwrap(),
                            )
                            .await
                    }
                    "verify" => worker
                        .verify_coverage(plan, action.required("coverage").unwrap())
                        .await
                        .map(|()| IvyJson::Null),
                    _ => worker.step(plan).await,
                },
            ));
        }
        let retained = snapshot(&spool.borrow(), plan, &journal);
        let (_, mut ids) = super::microsoft_store_cases::retained(&journal);
        if input.get("noCalls") == Some(&true.into()) {
            for row in retained.required("calls").unwrap().as_array().unwrap() {
                let call = row.required("call").unwrap();
                let id = call
                    .required("operationId")
                    .unwrap()
                    .as_js_string()
                    .unwrap();
                assert_eq!(
                    parse_operation_id_js(id).unwrap().runtime_epoch,
                    JsString::from("fixture-epoch")
                );
                ids.insert(
                    id.clone(),
                    format!(
                        "fresh-media-slot-{}",
                        call.required("slot").unwrap().as_f64().unwrap() as usize
                    )
                    .into(),
                );
            }
        }
        let mut trace = client.scripted.trace.lock().unwrap().clone();
        for row in &mut trace {
            if row.get("method") == Some(&"objects.write".into()) {
                let mut params = row.required("params").unwrap().clone();
                assert_eq!(
                    parse_operation_id_js(
                        params
                            .required("mutationId")
                            .unwrap()
                            .as_js_string()
                            .unwrap()
                    )
                    .unwrap()
                    .runtime_epoch,
                    JsString::from("fixture-epoch")
                );
                params.insert("mutationId", "<new-operation>".into());
                row.insert("params", params);
            }
        }
        let actual = normalize(
            &IvyJson::object([
                ("results", IvyJson::Array(results)),
                ("trace", IvyJson::Array(trace)),
                ("retained", retained),
            ]),
            &ids,
        );
        for field in ["results", "trace", "retained"] {
            assert_eq!(
                canonical(actual.required(field).unwrap(), 32 * 1024 * 1024).unwrap(),
                canonical(
                    row.required("expected").unwrap().required(field).unwrap(),
                    32 * 1024 * 1024
                )
                .unwrap(),
                "{label} / {field}"
            );
        }
        assert!(
            client.scripted.steps.lock().unwrap().is_empty(),
            "Unconsumed original RPC in {label}"
        );
        assert!(
            downloader.pending.borrow().is_empty(),
            "Missing original acquisition in {label}"
        );
        SPOOL.with(|slot| *slot.borrow_mut() = None);
        spool.borrow().ensure_idle().unwrap();
    }
    println!(
        "outlook-media-native original comparisons: {} cases / {} actions",
        rows.len(),
        rows.iter()
            .map(|row| row
                .required("input")
                .unwrap()
                .required("actions")
                .unwrap()
                .as_array()
                .unwrap()
                .len())
            .sum::<usize>()
    );
}
