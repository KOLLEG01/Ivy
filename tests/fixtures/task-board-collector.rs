#[path = "reference-data.rs"]
mod reference_data;
use super::*;
use ivy_sdk::tools::ToolClients;
use ivy_task_board::{
    inspection_journal::InspectionJournal,
    native_collector::{CollectionStore, FullTurnCollector, NativeCollection, NativeCollector},
    native_inspector::InspectionStore,
    operations::Store,
};
use std::sync::atomic::{AtomicBool, Ordering};

fn data() -> &'static Json {
    static DATA: OnceLock<Json> = OnceLock::new();
    DATA.get_or_init(|| {
        let output = reference_data::read(include_bytes!("task-board-collector-cases.json.gz"));
        parse(
            std::str::from_utf8(&output).unwrap(),
            MANAGEMENT_FRAME_BYTES,
        )
        .unwrap()
    })
}
struct Client {
    row: Json,
    log: Arc<Mutex<Vec<Json>>>,
    reads: Mutex<usize>,
    statuses: Mutex<usize>,
    owner_valid: AtomicBool,
}
impl Client {
    fn new(row: &Json, log: Arc<Mutex<Vec<Json>>>) -> Self {
        Self {
            row: row.clone(),
            log,
            reads: Mutex::new(0),
            statuses: Mutex::new(0),
            owner_valid: AtomicBool::new(true),
        }
    }
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            self.log.lock().unwrap().push(Json::Array(vec![
                "rpc".into(),
                method.into(),
                params.clone(),
            ]));
            if method == "serviceNodes.get" {
                assert_eq!(
                    params.required("serviceNodeId")?,
                    &Json::from("task-board-node")
                );
                return Ok(Json::object([
                    (
                        "principalId",
                        if self.owner_valid.load(Ordering::Relaxed) {
                            "caller"
                        } else {
                            "replacement"
                        }
                        .into(),
                    ),
                    ("serviceName", "task-board".into()),
                    ("connected", true.into()),
                    ("synced", true.into()),
                ]));
            }
            if method == "tools.list" {
                let name = params.required("namePrefix")?.as_str()?;
                return Ok(Json::object([
                    (
                        "provider",
                        Json::object([(
                            "node",
                            Json::object([("serviceNodeId", "native-agent".into())]),
                        )]),
                    ),
                    (
                        "items",
                        Json::Array(vec![Json::object([
                            ("qualifiedName", format!("agent.{name}").into()),
                            (
                                "definition",
                                Json::object([
                                    ("namespace", "agent".into()),
                                    ("name", name.into()),
                                    ("interfaceVersion", "1.0.0".into()),
                                    ("annotations", Json::object([("readOnlyHint", true.into())])),
                                    ("inputSchema", Json::Object(vec![])),
                                    ("outputSchema", Json::Object(vec![])),
                                ]),
                            ),
                            (
                                "definitionHash",
                                format!("sha256:{}", "a".repeat(64)).into(),
                            ),
                            ("catalogHash", format!("sha256:{}", "b".repeat(64)).into()),
                        ])]),
                    ),
                    ("nextCursor", Json::Null),
                ]));
            }
            assert_eq!(method, "tools.call", "Unexpected non-native RPC");
            if params.required("qualifiedName")? == &Json::from("agent.status") {
                let mut index = self.statuses.lock().unwrap();
                let result =
                    self.row.required("statusReplies")?.as_array().unwrap()[*index].clone();
                *index += 1;
                return Ok(result);
            }
            assert_eq!(params.required("qualifiedName")?, &Json::from("agent.read"));
            let mut index = self.reads.lock().unwrap();
            let result = self.row.required("observations")?.as_array().unwrap()[*index].clone();
            *index += 1;
            let args = params.required("arguments")?;
            for key in ["method", "params", "nativeVersion"] {
                assert_eq!(args.get(key), result.get(key));
            }
            Ok(result)
        })
    }
}
struct CollectionTrace<'a, S> {
    base: Trace<'a, S>,
    tools: ToolClients,
    db: &'a Connection,
    input: &'a Json,
    owners: Cell<usize>,
    raced: Cell<bool>,
}
impl<S: ReadEvidenceStore> BlobStore for CollectionTrace<'_, S> {
    type Pin = Json;
    type Error = IvyError;
    fn save(&self, k: ChunkKey, n: &str, p: &JsString, b: &[u8]) -> IvyResult<Json> {
        self.base.save(k, n, p, b)
    }
    fn read(&self, k: ChunkKey, p: &Json, parent: &JsString) -> IvyResult<Vec<u8>> {
        self.base.read(k, p, parent)
    }
}
impl<S: ReadEvidenceStore> ReadEvidenceStore for CollectionTrace<'_, S> {
    fn validate(&self, n: &str, v: &Json) -> IvyResult<()> {
        self.base.validate(n, v)
    }
    async fn read_document(&self, k: &str, a: &Json, p: bool) -> IvyResult<Document> {
        self.base.read_document(k, a, p).await
    }
    async fn write_document(&self, k: &str, v: &Json, m: &str, d: &Json) -> IvyResult<Json> {
        let result = self.base.write_document(k, v, m, d).await?;
        if !self.raced.get()
            && ((self.input.get("race") == Some(&Json::from("begin"))
                && k == "task-board/native-transcript"
                && d.get("create").is_some())
                || (self.input.get("race") == Some(&Json::from("full"))
                    && k == "task-board/native-full-turn-transcript"))
        {
            self.raced.set(true);
            return Err(IvyError::new(
                "mutation_conflict",
                "Fixture competing writer.",
            ));
        }
        Ok(result)
    }
}
impl<S: TurnEvidenceStore> TurnEvidenceStore for CollectionTrace<'_, S> {
    async fn read_named_document(
        &self,
        k: &str,
        a: &Json,
        p: bool,
    ) -> IvyResult<(Document, JsString)> {
        self.base.read_named_document(k, a, p).await
    }
}
impl<S: TurnEvidenceStore + Store> InspectionStore for CollectionTrace<'_, S> {
    async fn verify_owner(&self) -> IvyResult<()> {
        self.base.event(["owner".into()]);
        let n = self.owners.get() + 1;
        self.owners.set(n);
        if self.input.get("failOwner").and_then(Json::as_f64) == Some(n as f64) {
            Err(IvyError::new("fixture_failed", "owner"))
        } else {
            Ok(())
        }
    }
    async fn named_document(&self, k: &str, n: &str, p: &JsString) -> IvyResult<Option<Document>> {
        self.base
            .event(["named".into(), k.into(), n.into(), Json::String(p.clone())]);
        self.base.inner.named(k, n, Some(p)).await
    }
    fn tools(&self) -> &ToolClients {
        &self.tools
    }
    fn principal_id(&self) -> JsString {
        "caller".into()
    }
    fn now(&self) -> JsString {
        "2026-01-02T03:04:05.006Z".into()
    }
}
impl<S: TurnEvidenceStore + Store> CollectionStore for CollectionTrace<'_, S> {
    async fn named_receipt(
        &self,
        k: &str,
        n: &JsString,
        p: &JsString,
    ) -> IvyResult<Option<Document>> {
        self.base.event([
            "named".into(),
            k.into(),
            Json::String(n.clone()),
            Json::String(p.clone()),
        ]);
        let document = LocalJournal {
            connection: self.db,
            root_object_id: Some("root".into()),
        }
        .named(k, n, Some(p))?;
        if let Some(d) = &document {
            validate(definition(k), &d.value)?;
        }
        Ok(document)
    }
    fn document_name(&self, d: &Document) -> IvyResult<JsString> {
        Ok(self
            .db
            .query_row(
                "SELECT name FROM documents WHERE id=?",
                [s(&d.pin, "objectId").to_utf8().unwrap()],
                |r| r.get::<_, String>(0),
            )
            .unwrap()
            .into())
    }
}
fn result(v: NativeCollection) -> Json {
    match v {
        NativeCollection::Pending { transcript } => {
            Json::object([("kind", "pending".into()), ("transcript", transcript)])
        }
        NativeCollection::Complete { artifact } => {
            Json::object([("kind", "complete".into()), ("artifact", artifact)])
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn collectors_match_original_traces_checkpoints_and_partial_effects_over_actual_journal_and_sdk()
 {
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    assert!(!data().as_array().unwrap().is_empty());
    for group in data().as_array().unwrap() {
        for row in group.required("rows").unwrap().as_array().unwrap() {
            let db = Connection::open_in_memory().unwrap();
            local_journal::initialize(&db).unwrap();
            for b in row.required("blobs").unwrap().as_array().unwrap() {
                db.execute(
                    "INSERT INTO blobs(id,key,name,parent_id,bytes) VALUES(?,?,?,?,?)",
                    params![
                        s(b.required("pin").unwrap(), "objectId").to_utf8().unwrap(),
                        s(b, "key").to_utf8().unwrap(),
                        s(b, "name").to_utf8().unwrap(),
                        s(b, "parent").to_utf8().unwrap(),
                        hex(&s(b, "hex").to_utf8().unwrap())
                    ],
                )
                .unwrap();
            }
            let run = row.required("run").unwrap();
            let log = Arc::default();
            let client = Arc::new(Client::new(row, Arc::clone(&log)));
            let current_id = RefCell::new(id(400));
            let store = JournalStore {
                journal: LocalJournal {
                    connection: &db,
                    root_object_id: Some("root".into()),
                },
                client: client.clone(),
                contracts: Policy {
                    allowed: HashSet::from([hash_json(run).unwrap()]),
                },
                limits: HistoryLimits::default(),
                now: || "2026-01-02T03:04:05.006Z".to_owned(),
                new_id: || current_id.borrow().clone(),
                cache: None,
            };
            store
                .journal
                .create(
                    "task-board/run",
                    &id(200),
                    Some(&"root".into()),
                    run,
                    &"2026-01-02T03:04:05.006Z".into(),
                    &id(200),
                )
                .unwrap();
            store
                .journal
                .update(
                    "task-board/run",
                    &id(200),
                    1.0,
                    run,
                    &"2026-01-02T03:04:05.006Z".into(),
                )
                .unwrap();
            let trace = CollectionTrace {
                base: Trace {
                    inner: &store,
                    log: Arc::clone(&log),
                    write_ids: row.required("writeIds").unwrap().as_array().unwrap(),
                    write_index: Cell::new(0),
                    current_id: &current_id,
                    fault: RefCell::default(),
                },
                tools: ToolClients::new(client.clone()),
                db: &db,
                input: row.required("input").unwrap(),
                owners: Cell::new(0),
                raced: Cell::new(false),
            };
            let collector = NativeCollector {
                store: &trace,
                catalogs: &catalogs,
            };
            let principal = JsString::from("caller");
            let turns = TurnEvidence {
                store: &trace,
                catalogs: &catalogs,
                principal_id: &principal,
            };
            let mut results = vec![];
            for action in row.required("actions").unwrap().as_array().unwrap() {
                let a = action.required("args").unwrap().as_array().unwrap();
                let actual: IvyResult<Json> = match s(action, "method").to_utf8().unwrap().as_str()
                {
                    "saveSnapshot" => turns.save_snapshot(&a[0], &a[1], &a[2]).await,
                    "begin" => collector.begin(&a[0], &a[1]).await,
                    "advance" => collector
                        .advance(&a[0], a[1].as_js_string().unwrap(), a[2].as_f64().unwrap())
                        .await
                        .map(result),
                    "directFull" => {
                        let document = store
                            .journal
                            .read("task-board/native-transcript", &a[1], false)
                            .unwrap();
                        FullTurnCollector {
                            store: &trace,
                            catalogs: &catalogs,
                        }
                        .collect(&a[0], &document, &a[2], a[3].as_f64().unwrap())
                        .await
                        .map(|v| v.unwrap_or(Json::Null))
                    }
                    "fault" => {
                        trace
                            .base
                            .fault
                            .replace(Some(a[0].as_js_string().unwrap().clone()));
                        Ok(Json::Null)
                    }
                    "patch" => {
                        let d = store
                            .journal
                            .read(&a[0].as_str().unwrap(), &a[1], false)
                            .unwrap();
                        let mut v = d.value;
                        for (k, x) in a[2].as_object().unwrap() {
                            if k == &JsString::from("evidence") {
                                let mut r = v.required("evidence").unwrap().clone();
                                for (k, v) in x.as_object().unwrap() {
                                    r.insert(k.clone(), v.clone());
                                }
                                v.insert(k.clone(), r);
                            } else {
                                v.insert(k.clone(), x.clone());
                            }
                        }
                        db.execute(
                            "UPDATE revisions SET value_json=? WHERE id=? AND revision=?",
                            params![
                                canonical(&v, MANAGEMENT_FRAME_BYTES).unwrap(),
                                s(&d.pin, "objectId").to_utf8().unwrap(),
                                num(&d.pin, "revision")
                            ],
                        )
                        .unwrap();
                        Ok(Json::Null)
                    }
                    m => panic!("Unexpected collection action {m}"),
                };
                results.push(match actual {
                    Ok(v) => Json::object([("ok", v)]),
                    Err(e) => Json::object([(
                        "error",
                        Json::object([
                            ("code", Json::String(e.code_js())),
                            ("message", Json::String(e.message_js())),
                            ("outcome", e.outcome.to_json()),
                        ]),
                    )]),
                });
            }
            let actual = Json::object([
                ("results", Json::Array(results)),
                ("trace", Json::Array(log.lock().unwrap().clone())),
                ("documents", documents(&db)),
            ]);
            assert_eq!(
                hash_json(&actual).unwrap(),
                s(row, "expectedHash").to_utf8().unwrap(),
                "{} / {}: {}",
                s(group, "path").to_utf8().unwrap(),
                s(row, "name").to_utf8().unwrap(),
                canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap()
            );
            assert_eq!(
                *client.reads.lock().unwrap(),
                row.required("observations")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .len()
            );
            assert_eq!(
                *client.statuses.lock().unwrap(),
                row.required("statusReplies")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .len()
            );
            assert!(db.is_autocommit());
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn inspection_journal_collects_with_fresh_ids_reopens_and_refuses_owner_loss_and_corruption()
{
    struct Temp(std::path::PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let group = &data().as_array().unwrap()[0];
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    for name in [
        "paged completion and idempotent replay",
        "exact first unsupported item falls back",
    ] {
        let row = group
            .required("rows")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|r| s(r, "name") == &JsString::from(name))
            .unwrap();
        let run = row.required("run").unwrap();
        let temp = Temp(std::env::temp_dir().join(format!(
            "task-board-collection-{}.sqlite",
            uuid::Uuid::new_v4()
        )));
        let mut saved = None;
        for round in 0..2 {
            let db = Connection::open(&temp.0).unwrap();
            local_journal::initialize(&db).unwrap();
            let log = Arc::default();
            let client = Arc::new(Client::new(row, Arc::clone(&log)));
            let store = JournalStore {
                journal: LocalJournal {
                    connection: &db,
                    root_object_id: Some("root".into()),
                },
                client: client.clone(),
                contracts: Policy {
                    allowed: HashSet::from([hash_json(run).unwrap()]),
                },
                limits: HistoryLimits::default(),
                now: || "2026-01-02T03:04:05.006Z".to_owned(),
                new_id: || uuid::Uuid::new_v4().to_string().into(),
                cache: None,
            };
            let inspected =
                InspectionJournal::new(&store, "caller".into(), "task-board-node".into());
            let collector = NativeCollector {
                store: &inspected,
                catalogs: &catalogs,
            };
            if round == 0 {
                store
                    .journal
                    .create(
                        "task-board/run",
                        &id(200),
                        Some(&"root".into()),
                        run,
                        &"2026-01-02T03:04:05.006Z".into(),
                        &id(200),
                    )
                    .unwrap();
                store
                    .journal
                    .update(
                        "task-board/run",
                        &id(200),
                        1.0,
                        run,
                        &"2026-01-02T03:04:05.006Z".into(),
                    )
                    .unwrap();
                let first = &row.required("actions").unwrap().as_array().unwrap()[0]
                    .required("args")
                    .unwrap()
                    .as_array()
                    .unwrap();
                let principal = JsString::from("caller");
                let snapshot = TurnEvidence {
                    store: &store,
                    catalogs: &catalogs,
                    principal_id: &principal,
                }
                .save_snapshot(&first[0], &first[1], &first[2])
                .await
                .unwrap();
                let collection = collector.begin(&pin(200, 2.0), &snapshot).await.unwrap();
                let result = collector
                    .advance(&pin(200, 2.0), s(&collection, "objectId"), 8.0)
                    .await
                    .unwrap();
                let artifact = match result {
                    NativeCollection::Complete { artifact } => artifact,
                    _ => panic!("Expected complete real collection"),
                };
                let verified = verify_native_transcript(
                    TurnEvidence {
                        store: &store,
                        catalogs: &catalogs,
                        principal_id: &principal,
                    },
                    &pin(200, 2.0),
                    &artifact,
                )
                .await
                .unwrap();
                assert_eq!(verified.completion.summary, JsString::from("answer"));
                saved = Some((collection, artifact));
            } else {
                let (collection, original) = saved.as_ref().unwrap();
                let before = db.total_changes();
                let actual = collector
                    .advance(&pin(200, 2.0), s(collection, "objectId"), 8.0)
                    .await
                    .unwrap();
                assert_eq!(
                    result(actual),
                    Json::object([("kind", "complete".into()), ("artifact", original.clone())])
                );
                assert_eq!(*client.reads.lock().unwrap(), 0);
                assert_eq!(*client.statuses.lock().unwrap(), 0);
                assert_eq!(db.total_changes(), before);
                client.owner_valid.store(false, Ordering::Relaxed);
                let error = collector
                    .advance(&pin(200, 2.0), s(collection, "objectId"), 8.0)
                    .await
                    .err()
                    .unwrap();
                assert_eq!(error.code, "task_board_principal_mismatch");
                assert_eq!(db.total_changes(), before);
                client.owner_valid.store(true, Ordering::Relaxed);
                let (blob, mut bytes): (String, Vec<u8>) = db
                    .query_row(
                        "SELECT id,bytes FROM blobs ORDER BY rowid LIMIT 1",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap();
                bytes[0] ^= 1;
                db.execute("UPDATE blobs SET bytes=? WHERE id=?", params![bytes, blob])
                    .unwrap();
                let corrupt_at = db.total_changes();
                let error = collector
                    .advance(&pin(200, 2.0), s(collection, "objectId"), 8.0)
                    .await
                    .err()
                    .unwrap();
                assert_eq!(error.code, "task_board_evidence_mismatch");
                assert_eq!(db.total_changes(), corrupt_at);
                assert_eq!(*client.reads.lock().unwrap(), 0);
            }
            assert!(db.is_autocommit());
        }
    }
}
