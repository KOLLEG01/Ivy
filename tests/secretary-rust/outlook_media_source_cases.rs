use super::microsoft_store_cases::seed;
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{IvyResult, hash::hash_json, json::canonical};
use ivy_sdk::clock::Clock;
use ivy_secretary::{
    engine::SecretaryEngine,
    inbox_evidence::InboxEvidence,
    inbox_recovery::RecoveryScan,
    journal::{Journal, Limits},
    media_input::{self, AcquiredMediaPreparer, PrepareFuture, Source},
    media_spool::SpoolJournal,
    outlook_media_native::{
        DownloadFuture, DownloadedMedia, MediaNativeValidation, OutlookMediaDownloader,
    },
    outlook_media_source::OutlookMediaSource,
    outlook_source::OutlookSource,
    store::Store,
};
use ivy_store::rusqlite::{Connection, params};
use std::{cell::RefCell, path::Path, process::Command, rc::Rc};

struct Directory(std::path::PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "secretary-media-source-rust-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let actual = std::fs::canonicalize(&self.0).unwrap();
        assert_eq!(
            actual.parent(),
            Some(
                std::fs::canonicalize(std::env::temp_dir())
                    .unwrap()
                    .as_path()
            )
        );
        assert!(
            actual
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("secretary-media-source-rust-")
        );
        std::fs::remove_dir_all(actual).unwrap();
    }
}

thread_local! {static SPOOL: RefCell<Option<Rc<RefCell<SpoolJournal<MediaNativeValidation>>>>> = const {RefCell::new(None)};}
fn released() {
    SPOOL.with(|slot| {
        if let Some(spool) = slot.borrow().as_ref() {
            assert!(
                spool.try_borrow_mut().is_ok(),
                "Spool borrow crossed an async boundary"
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
    tamper_after_attach: std::sync::atomic::AtomicBool,
}
impl RpcClient for Yielding {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            released();
            tokio::task::yield_now().await;
            released();
            assert_eq!(
                self.scripted
                    .steps
                    .lock()
                    .unwrap()
                    .front()
                    .and_then(|step| step.get("method")),
                Some(&method.into()),
                "{} after {} RPCs",
                self.label,
                self.scripted.trace.lock().unwrap().len()
            );
            let attach = method == "objects.write" && params.get("objectId").is_some();
            let result = self.scripted.request(method, params, options).await;
            if attach
                && self
                    .tamper_after_attach
                    .swap(false, std::sync::atomic::Ordering::Relaxed)
            {
                SPOOL.with(|slot| {
                    let slot = slot.borrow();
                    let spool = slot.as_ref().unwrap().borrow();
                    let encoded: String = spool
                        .db
                        .query_row(
                            "SELECT observation FROM read_calls WHERE slot=38",
                            [],
                            |row| row.get(0),
                        )
                        .unwrap();
                    let mut observation = parse(&encoded, 32 * 1024 * 1024).unwrap();
                    let mut reply = observation.required("reply").unwrap().clone();
                    let mut value = reply.required("result").unwrap().clone();
                    value.insert("status", "stillLoaded".into());
                    reply.insert("result", value);
                    observation.insert("reply", reply);
                    spool
                        .db
                        .execute(
                            "UPDATE read_calls SET observation=? WHERE slot=38",
                            [canonical(&observation, 32 * 1024 * 1024).unwrap()],
                        )
                        .unwrap();
                });
            }
            result
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
            released();
            tokio::task::yield_now().await;
            released();
            let original = self
                .pending
                .borrow_mut()
                .pop_front()
                .expect("Replacement acquisition attempted");
            assert_eq!(
                canonical(materialization, 32 * 1024 * 1024)?,
                canonical(original.required("materialization")?, 32 * 1024 * 1024)?
            );
            Ok(DownloadedMedia {
                bytes: STANDARD
                    .decode(original.required("bytes")?.as_str()?)
                    .unwrap(),
                evidence: original.required("evidence")?.clone(),
            })
        })
    }
}
struct Preparer {
    calls: RefCell<Vec<IvyJson>>,
}
impl AcquiredMediaPreparer for Preparer {
    fn prepare<'a>(
        &'a self,
        source: &'a Source,
        bytes: &'a [u8],
        budget: f64,
    ) -> PrepareFuture<'a> {
        Box::pin(async move {
            released();
            tokio::task::yield_now().await;
            released();
            self.calls.borrow_mut().push(IvyJson::object([
                ("source", source.to_json()),
                ("bytes", STANDARD.encode(bytes).into()),
                ("budget", IvyJson::Number(budget)),
            ]));
            media_input::prepare(source, bytes, budget)
        })
    }
}
fn result(value: IvyResult<IvyJson>) -> IvyJson {
    match value {
        Ok(value) => IvyJson::object([("value", value)]),
        Err(error) => IvyJson::object([
            ("code", IvyJson::String(error.code_js())),
            ("outcome", error.outcome.as_str().into()),
        ]),
    }
}
fn spool_snapshot(spool: &SpoolJournal<MediaNativeValidation>) -> IvyJson {
    IvyJson::object(
        [
            ("plans", "read_plans"),
            ("calls", "read_calls"),
            ("attempts", "attempts"),
        ]
        .into_iter()
        .map(|(key, table)| {
            let n: i64 = spool
                .db
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            (key, IvyJson::Number(n as f64))
        })
        .chain([(
            "usedBytes",
            IvyJson::Number(spool.used_bytes().unwrap() as f64),
        )]),
    )
}
fn core(journal: &Journal) -> IvyJson {
    let mut query = journal
        .db
        .prepare("SELECT key,name,current_revision,value_json FROM documents ORDER BY key,name")
        .unwrap();
    IvyJson::Array(
        query
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .unwrap()
            .map(|row| {
                let (key, name, revision, value) = row.unwrap();
                IvyJson::object([
                    ("key", key.into()),
                    ("name", name.into()),
                    ("revision", IvyJson::Number(revision as f64)),
                    ("value", parse(&value, 32 * 1024 * 1024).unwrap()),
                ])
            })
            .collect(),
    )
}
fn seed_core(journal: &Journal, input: &IvyJson) {
    seed(journal, input);
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
                        canonical(revision.required("value").unwrap(), 32 * 1024 * 1024).unwrap()
                    ],
                )
                .unwrap();
        }
    }
}
fn open_spool(path: &Path, input: &IvyJson) -> Rc<RefCell<SpoolJournal<MediaNativeValidation>>> {
    Rc::new(RefCell::new(
        SpoolJournal::from_owned_connection(
            Connection::open(path).unwrap(),
            input.required("spoolOwner").unwrap(),
            MediaNativeValidation,
        )
        .unwrap(),
    ))
}
fn seed_spool(spool: &SpoolJournal<MediaNativeValidation>, input: &IvyJson) {
    spool.prepare_read(input.required("plan").unwrap()).unwrap();
    for row in input
        .required("spool")
        .unwrap()
        .required("calls")
        .unwrap()
        .as_array()
        .unwrap()
    {
        let call = row.required("call").unwrap();
        let value = |key| {
            row.get(key)
                .filter(|value| !value.is_null())
                .map(|value| canonical(value, 32 * 1024 * 1024).unwrap())
        };
        spool
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
}
fn corpus() -> IvyJson {
    let mut command = Command::new("node");
    command
        .arg("tests/secretary-rust/outlook-media-source-oracle.mjs")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command
        .output()
        .expect("Original media source oracle required");
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
async fn segment(
    journal: &Journal,
    spool: &RefCell<SpoolJournal<MediaNativeValidation>>,
    client: Arc<Yielding>,
    downloader: &Downloader,
    preparer: &Preparer,
    corpus: &IvyJson,
    input: &IvyJson,
    actions: &[IvyJson],
    results: &mut Vec<IvyJson>,
) {
    let store = Store {
        client: client.clone(),
        root: corpus
            .required("rootId")
            .unwrap()
            .as_js_string()
            .unwrap()
            .clone(),
        journal,
    };
    let evidence = InboxEvidence {
        store: &store,
        settings: input.required("settings").unwrap(),
        service_node_id: corpus.required("nodeId").unwrap().as_js_string().unwrap(),
        cancellation: RequestOptions::default(),
    };
    let clock = Fixed(corpus.required("time").unwrap().as_f64().unwrap() as i64);
    let wall = Fixed(corpus.required("wall").unwrap().as_f64().unwrap() as i64);
    let tools = Arc::new(ToolClients::new(client));
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
    let mut worker = OutlookMediaSource::new(&source, spool, downloader, preparer);
    let split = actions
        .iter()
        .position(|action| action.get("method") == Some(&"context".into()))
        .unwrap_or(actions.len());
    for action in &actions[..split] {
        assert_eq!(action.get("method"), Some(&"step".into()));
        results.push(result(worker.step().await.map(Into::into)));
        results.push(IvyJson::object([
            ("spool", spool_snapshot(&spool.borrow())),
            (
                "issue",
                worker
                    .issue
                    .clone()
                    .map(IvyJson::String)
                    .unwrap_or(IvyJson::Null),
            ),
        ]));
    }
    let Some(_) = actions.get(split) else { return };
    let context = worker
        .context(input.required("plan").unwrap().required("item").unwrap())
        .await;
    results.push(result(
        context
            .as_ref()
            .map(|value| {
                value
                    .as_ref()
                    .map(|context| {
                        IvyJson::object([
                            ("item", context.item.clone()),
                            ("coverage", context.coverage.clone()),
                        ])
                    })
                    .unwrap_or(IvyJson::Null)
            })
            .map_err(Clone::clone),
    ));
    if let Ok(Some(context)) = context {
        for action in &actions[split + 1..] {
            let index = action.required("index").unwrap().as_f64().unwrap();
            results.push(result(
                if action.get("method") == Some(&"attachment".into()) {
                    context.read_attachment(index).await.map(|value| {
                        value
                            .map(|payload| {
                                IvyJson::object([
                                    ("pin", payload.pin),
                                    ("value", payload.value),
                                    ("bytes", STANDARD.encode(payload.bytes).into()),
                                ])
                            })
                            .unwrap_or(IvyJson::Null)
                    })
                } else {
                    context
                        .input(index, action.required("budget").unwrap().as_f64().unwrap())
                        .await
                        .map(|value| value.map(|value| value.to_json()).unwrap_or(IvyJson::Null))
                },
            ));
        }
    }
}
async fn compare_case(corpus: &IvyJson, row: &IvyJson, fair: u8) {
    let input = row.required("input").unwrap();
    let label = input.required("label").unwrap().as_str().unwrap();
    let directory = Directory::new();
    let journal_path = directory.path().join("core.sqlite");
    let spool_path = directory.path().join("media.sqlite");
    let mut journal = Journal::open(&journal_path, Limits::default()).unwrap();
    seed_core(&journal, input);
    if fair > 0 {
        let original = input
            .required("documents")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row.get("key") == Some(&"secretary/outlook-publication".into()))
            .unwrap();
        for index in 0..8 {
            let id = format!("publication-{index:04}");
            let mut value = original.required("value").unwrap().clone();
            value.insert("captured", IvyJson::Array(vec![]));
            if fair == 2 && index == 0 {
                value.insert("schemaVersion", IvyJson::Number(2.0));
            }
            let encoded = canonical(&value, 32 * 1024 * 1024).unwrap();
            journal
                .db
                .execute(
                    "INSERT INTO technical_documents VALUES(?,?,?,?,?)",
                    params![
                        id,
                        "secretary/outlook-publication",
                        format!("older-{index}"),
                        1,
                        encoded
                    ],
                )
                .unwrap();
            journal
                .db
                .execute(
                    "INSERT INTO technical_revisions VALUES(?,?,?)",
                    params![id, 1, encoded],
                )
                .unwrap();
        }
    }
    let mut spool = open_spool(&spool_path, input);
    seed_spool(&spool.borrow(), input);
    SPOOL.with(|slot| *slot.borrow_mut() = Some(spool.clone()));
    let client = Arc::new(Yielding {
        label: label.clone(),
        tamper_after_attach: std::sync::atomic::AtomicBool::new(
            input.required("options").unwrap().get("afterAttachTamper") == Some(&true.into()),
        ),
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
    let preparer = Preparer {
        calls: RefCell::new(vec![]),
    };
    if fair > 0 {
        let owner = input.required("steps").unwrap().as_array().unwrap()[0].clone();
        {
            let mut steps = client.scripted.steps.lock().unwrap();
            steps.push_front(owner.clone());
            steps.push_back(owner);
        }
        let mut skipped = vec![];
        segment(
            &journal,
            &spool,
            client.clone(),
            &downloader,
            &preparer,
            corpus,
            input,
            &[IvyJson::object([("method", "step".into())])],
            &mut skipped,
        )
        .await;
        if fair == 1 {
            assert_eq!(skipped[0].get("value"), Some(&"idle".into()));
        } else {
            assert!(skipped[0].get("code").is_some());
        }
        let cursor: String = journal
            .db
            .query_row(
                "SELECT value FROM journal_state WHERE key LIKE 'secretary/outlook-media-scan/%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            cursor,
            if fair == 1 {
                "publication-0007"
            } else {
                "publication-0000"
            }
        );
        SPOOL.with(|slot| *slot.borrow_mut() = None);
        drop(spool);
        drop(journal);
        journal = Journal::open(&journal_path, Limits::default()).unwrap();
        spool = open_spool(&spool_path, input);
        SPOOL.with(|slot| *slot.borrow_mut() = Some(spool.clone()));
    }
    let mut results = vec![];
    let actions = input.required("actions").unwrap().as_array().unwrap();
    let mut start = 0;
    for (index, action) in actions.iter().enumerate() {
        if action.get("method") != Some(&"restart".into()) {
            continue;
        }
        segment(
            &journal,
            &spool,
            client.clone(),
            &downloader,
            &preparer,
            &corpus,
            input,
            &actions[start..index],
            &mut results,
        )
        .await;
        SPOOL.with(|slot| *slot.borrow_mut() = None);
        drop(spool);
        drop(journal);
        journal = Journal::open(&journal_path, Limits::default()).unwrap();
        spool = open_spool(&spool_path, input);
        SPOOL.with(|slot| *slot.borrow_mut() = Some(spool.clone()));
        start = index + 1;
    }
    segment(
        &journal,
        &spool,
        client.clone(),
        &downloader,
        &preparer,
        &corpus,
        input,
        &actions[start..],
        &mut results,
    )
    .await;
    if fair > 0 {
        let cursor: String = journal
            .db
            .query_row(
                "SELECT value FROM journal_state WHERE key LIKE 'secretary/outlook-media-scan/%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cursor, "publication-0008");
        let mut wrapped = vec![];
        segment(
            &journal,
            &spool,
            client.clone(),
            &downloader,
            &preparer,
            corpus,
            input,
            &[IvyJson::object([("method", "step".into())])],
            &mut wrapped,
        )
        .await;
        if fair == 1 {
            assert_eq!(wrapped[0].get("value"), Some(&"idle".into()));
        } else {
            assert!(wrapped[0].get("code").is_some());
        }
        let count:i64=journal.db.query_row("SELECT COUNT(*) FROM technical_documents WHERE key='secretary/outlook-publication'",[],|row|row.get(0)).unwrap();
        assert_eq!(count, 9);
    }
    let mut trace = client.scripted.trace.lock().unwrap().clone();
    if fair > 0 {
        trace.remove(0);
        trace.pop();
    }
    for row in &mut trace {
        if row.get("method") == Some(&"objects.write".into()) {
            let mut params = row.required("params").unwrap().clone();
            params.insert("mutationId", "<new-operation>".into());
            row.insert("params", params);
        }
    }
    let actual = IvyJson::object([
        ("results", IvyJson::Array(results)),
        ("trace", IvyJson::Array(trace)),
        ("prepared", IvyJson::Array(preparer.calls.into_inner())),
        (
            "retained",
            IvyJson::object([
                ("spool", spool_snapshot(&spool.borrow())),
                ("core", core(&journal)),
            ]),
        ),
    ]);
    for field in ["results", "trace", "prepared", "retained"] {
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
#[tokio::test]
async fn original_media_source_selection_publication_retirement_and_context() {
    let corpus = corpus();
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 27);
    for row in rows {
        compare_case(&corpus, row, 0).await;
    }
    println!(
        "outlook-media-source original comparisons: {} cases / {} actions",
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
#[tokio::test]
async fn fair_media_scan_survives_restart_without_eviction_or_a_poisoned_first_entry() {
    let corpus = corpus();
    let baseline = corpus.required("fairScanOriginal").unwrap();
    assert!(
        baseline
            .required("input")
            .unwrap()
            .required("downloads")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        baseline
            .required("expected")
            .unwrap()
            .required("results")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|value| value.get("value"))
            .all(|value| value == &"idle".into())
    );
    assert_eq!(
        baseline
            .required("expected")
            .unwrap()
            .required("retained")
            .unwrap()
            .required("spool")
            .unwrap()
            .get("plans"),
        Some(&IvyJson::Number(1.0))
    );
    let row = &corpus.required("rows").unwrap().as_array().unwrap()[0];
    compare_case(&corpus, row, 1).await;
    compare_case(&corpus, row, 2).await;
    println!(
        "SEC08: bounded eight-entry scan, physical core/spool reopen, ninth capture, wrap, poisoned entry, zero eviction and one original GET per scenario"
    );
}
