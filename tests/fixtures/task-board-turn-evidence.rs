use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    builtin_schema::SchemaBundle,
    checked_native_contract::CheckedNativeContracts,
    hash::{digest, hash_json},
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_store::rusqlite::{Connection, params};
use ivy_task_board::{
    evidence_bytes::{BlobStore, ChunkKey},
    full_turn_evidence::{FullTurnEvidence, verify_native_transcript},
    journal_store::{Contracts, JournalStore},
    local_journal::{self, LocalJournal},
    local_retention::HistoryLimits,
    native_turn_evidence::{Completion, Snapshot, TurnEvidence, TurnEvidenceStore},
    operations::Document,
    read_evidence::{ReadEvidence, ReadEvidenceStore, ReadIdentity},
};
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    path::Path,
    process::Command,
    sync::{Arc, Mutex, OnceLock},
};
#[path = "task-board-collector.rs"]
mod collector;
fn s<'a>(v: &'a Json, k: &str) -> &'a JsString {
    v.required(k).unwrap().as_js_string().unwrap()
}
fn num(v: &Json, k: &str) -> f64 {
    v.required(k).unwrap().as_f64().unwrap()
}
fn id(n: u64) -> JsString {
    format!("00000000-0000-4000-8000-{n:012}").into()
}
fn pin(n: u64, r: f64) -> Json {
    Json::object([
        ("objectId", Json::String(id(n))),
        ("revision", Json::Number(r)),
    ])
}
fn data() -> &'static Json {
    static DATA: OnceLock<Json> = OnceLock::new();
    DATA.get_or_init(|| {
        let o = Command::new("node")
            .args([
                "--experimental-transform-types",
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/task-board-turn-evidence-oracle.mjs",
            ])
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        parse(
            std::str::from_utf8(&o.stdout).unwrap(),
            MANAGEMENT_FRAME_BYTES,
        )
        .unwrap()
    })
}
fn validate(name: &str, v: &Json) -> IvyResult<()> {
    static B: OnceLock<SchemaBundle> = OnceLock::new();
    let b = B.get_or_init(|| {
        SchemaBundle::new(&[
            include_str!("../../specs/schemas/task-board.schema.json"),
            include_str!("../../specs/schemas/chat.schema.json"),
            include_str!("../../specs/schemas/agent.schema.json"),
            include_str!("../../specs/schemas/hive-wire.schema.json"),
        ])
        .unwrap()
    });
    canonical(v, MANAGEMENT_FRAME_BYTES)?;
    if b.matches(
        &format!("https://ivy.invalid/schemas/task-board.schema.json#/$defs/{name}"),
        v,
    )? {
        Ok(())
    } else {
        Err(IvyError::new(
            "invalid_arguments",
            "Value does not match its TaskBoard workflow contract.",
        ))
    }
}
fn definition(key: &str) -> &str {
    match key {
        "task-board/native-read-evidence" => "NativeReadEvidence",
        "task-board/native-turn-snapshot" => "NativeTurnSnapshot",
        "task-board/native-result-page" => "NativeResultPage",
        "task-board/native-transcript" => "NativeTranscript",
        "task-board/native-full-turn-transcript" => "NativeFullTurnTranscript",
        "task-board/native-turn-search" => "NativeTurnSearch",
        _ => panic!("Unexpected contract {key}"),
    }
}
struct Policy {
    allowed: HashSet<String>,
}
impl Contracts for Policy {
    fn version(&self, _: &str) -> IvyResult<String> {
        Ok("1.0.0".into())
    }
    fn local(&self, _: &str) -> IvyResult<bool> {
        Ok(true)
    }
    fn readable_version(&self, _: &str, v: &JsString) -> IvyResult<bool> {
        Ok(v == &JsString::from("1.0.0"))
    }
    fn validate_record(&self, k: &str, _: &JsString, v: &Json) -> IvyResult<()> {
        if k == "task-board/run" {
            if self.allowed.contains(&hash_json(v)?) {
                Ok(())
            } else {
                Err(IvyError::internal())
            }
        } else {
            validate(definition(k), v)
        }
    }
    fn validate_definition(&self, n: &str, v: &Json) -> IvyResult<()> {
        validate(n, v)
    }
}
struct NoRpc;
impl RpcClient for NoRpc {
    fn request<'a>(&'a self, m: &'a str, _: Json, _: RequestOptions) -> RequestFuture<'a> {
        panic!("Turn evidence made unexpected RPC {m}")
    }
}
struct Trace<'a, S> {
    inner: &'a S,
    log: Arc<Mutex<Vec<Json>>>,
    write_ids: &'a [Json],
    write_index: Cell<usize>,
    current_id: &'a RefCell<JsString>,
    fault: RefCell<Option<JsString>>,
}
impl<S> Trace<'_, S> {
    fn event(&self, v: impl IntoIterator<Item = Json>) {
        self.log
            .lock()
            .unwrap()
            .push(Json::Array(v.into_iter().collect()));
    }
}
impl<S: ReadEvidenceStore> BlobStore for Trace<'_, S> {
    type Pin = Json;
    type Error = IvyError;
    fn save(&self, k: ChunkKey, n: &str, p: &JsString, b: &[u8]) -> IvyResult<Json> {
        self.event([
            "blob-save".into(),
            k.as_str().into(),
            n.into(),
            Json::String(p.clone()),
            digest(b).into(),
            Json::Number(b.len() as f64),
        ]);
        self.inner.save(k, n, p, b)
    }
    fn read(&self, k: ChunkKey, p: &Json, parent: &JsString) -> IvyResult<Vec<u8>> {
        self.event([
            "blob-read".into(),
            k.as_str().into(),
            p.clone(),
            Json::String(parent.clone()),
        ]);
        self.inner.read(k, p, parent)
    }
}
impl<S: ReadEvidenceStore> ReadEvidenceStore for Trace<'_, S> {
    fn validate(&self, n: &str, v: &Json) -> IvyResult<()> {
        self.inner.validate(n, v)
    }
    async fn read_document(&self, k: &str, a: &Json, p: bool) -> IvyResult<Document> {
        self.event(["read".into(), k.into(), a.clone(), p.into()]);
        self.inner.read_document(k, a, p).await
    }
    async fn write_document(&self, k: &str, v: &Json, m: &str, d: &Json) -> IvyResult<Json> {
        self.event(["write".into(), k.into(), v.clone(), m.into(), d.clone()]);
        let index = self.write_index.get();
        self.write_index.set(index + 1);
        *self.current_id.borrow_mut() = self.write_ids[index].as_js_string().unwrap().clone();
        if self.fault.borrow().as_ref() == Some(&k.into()) {
            self.fault.replace(None);
            return Err(IvyError::new(
                "fixture_failed",
                "Fixture write unavailable.",
            ));
        }
        self.inner.write_document(k, v, m, d).await
    }
}
impl<S: TurnEvidenceStore> TurnEvidenceStore for Trace<'_, S> {
    async fn read_named_document(
        &self,
        k: &str,
        a: &Json,
        p: bool,
    ) -> IvyResult<(Document, JsString)> {
        self.event(["read".into(), k.into(), a.clone(), p.into()]);
        self.inner.read_named_document(k, a, p).await
    }
}
fn completion(v: Completion) -> Json {
    Json::object([
        ("snapshot", v.snapshot.document.value),
        ("status", Json::String(v.status)),
        ("summary", Json::String(v.summary)),
    ])
}
fn snapshot(v: Snapshot) -> Json {
    Json::object([
        ("value", v.document.value),
        ("thread", v.thread),
        ("turn", v.turn),
        ("run", v.source.run.value),
    ])
}
fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
fn documents(db: &Connection) -> Json {
    let mut stmt=db.prepare("SELECT d.id,d.key,d.name,d.parent_id,d.current_revision,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key<>'task-board/run' ORDER BY d.id").unwrap();
    Json::Array(
        stmt.query_map([], |r| {
            let body: String = r.get(5)?;
            Ok(Json::object([
                ("key", Json::from(r.get::<_, String>(1)?)),
                (
                    "pin",
                    Json::object([
                        ("objectId", Json::from(r.get::<_, String>(0)?)),
                        ("revision", Json::Number(r.get(4)?)),
                    ]),
                ),
                ("name", Json::from(r.get::<_, String>(2)?)),
                ("parent", Json::from(r.get::<_, String>(3)?)),
                ("value", parse(&body, MANAGEMENT_FRAME_BYTES).unwrap()),
            ]))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap(),
    )
}
#[tokio::test(flavor = "current_thread")]
async fn terminal_snapshots_outputs_and_transcripts_match_original_over_actual_journal() {
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let mut count = 0;
    for group in data().as_array().unwrap() {
        for row in group.required("rows").unwrap().as_array().unwrap() {
            count += 1;
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
            let mut allowed = HashSet::from([hash_json(run).unwrap()]);
            for a in row.required("actions").unwrap().as_array().unwrap() {
                if s(a, "method") == &JsString::from("patch") {
                    let args = a.required("args").unwrap().as_array().unwrap();
                    if args[0] == Json::from("task-board/run") {
                        let mut v = run.clone();
                        for (k, vv) in args[2].as_object().unwrap() {
                            v.insert(k.clone(), vv.clone());
                        }
                        allowed.insert(hash_json(&v).unwrap());
                    }
                } else if s(a, "method") == &JsString::from("largeRequest") {
                    let count = a.required("args").unwrap().as_array().unwrap()[0]
                        .as_f64()
                        .unwrap() as usize;
                    let mut v = run.clone();
                    v.insert(
                        "originalRequest",
                        Json::object([("body", Json::String(JsString(vec![120; count])))]),
                    );
                    allowed.insert(hash_json(&v).unwrap());
                }
            }
            let current_id = RefCell::new(id(400));
            let store = JournalStore {
                journal: LocalJournal {
                    connection: &db,
                    root_object_id: Some("root".into()),
                },
                client: Arc::new(NoRpc),
                contracts: Policy { allowed },
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
            let trace = Trace {
                inner: &store,
                log: Arc::default(),
                write_ids: row.required("writeIds").unwrap().as_array().unwrap(),
                write_index: Cell::new(0),
                current_id: &current_id,
                fault: RefCell::default(),
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
                    "snapshot" => turns.snapshot(&a[0], &a[1]).await.map(snapshot),
                    "completion" => turns
                        .completion(&a[0], &a[1], Some(&a[2]))
                        .await
                        .map(completion),
                    "saveOutput" => {
                        turns
                            .save_output(&a[0], &a[1], a[2].as_js_string(), &a[3])
                            .await
                    }
                    "savePage" => {
                        turns
                            .save_page(
                                &a[0],
                                &a[1],
                                a[2].as_js_string().unwrap(),
                                &a[3],
                                a[4].as_f64().unwrap(),
                                a[5].as_js_string(),
                                &a[6],
                            )
                            .await
                    }
                    "verifyTranscript" => {
                        turns.verify_transcript(&a[0], &a[1]).await.map(completion)
                    }
                    "verifyFull" => FullTurnEvidence {
                        turns: TurnEvidence {
                            store: &trace,
                            catalogs: &catalogs,
                            principal_id: &principal,
                        },
                    }
                    .verify(&a[0], &a[1])
                    .await
                    .map(completion),
                    "fullTranscript" => FullTurnEvidence {
                        turns: TurnEvidence {
                            store: &trace,
                            catalogs: &catalogs,
                            principal_id: &principal,
                        },
                    }
                    .transcript(&a[0], &a[1])
                    .await
                    .map(|t| {
                        Json::object([
                            ("value", t.document.value),
                            ("status", Json::String(t.completion.status)),
                            ("summary", Json::String(t.completion.summary)),
                        ])
                    }),
                    "transcript" => turns.transcript(&a[0], &a[1]).await.map(|t| {
                        Json::object([
                            ("value", t.document.value),
                            ("status", Json::String(t.completion.status)),
                            ("summary", Json::String(t.completion.summary)),
                        ])
                    }),
                    "saveRead" => {
                        let o = &a[1];
                        ReadEvidence {
                            store: &trace,
                            catalogs: &catalogs,
                        }
                        .save(
                            &a[0],
                            o,
                            &ReadIdentity {
                                observation_id: s(o, "observationId").clone(),
                                caller_principal_id: principal.clone(),
                                service_node_id: s(o, "serviceNodeId").clone(),
                                native_version: s(o, "nativeVersion").clone(),
                                epoch: s(o, "epoch").clone(),
                                method: s(o, "method").clone(),
                                params: o.required("params").unwrap().clone(),
                            },
                        )
                        .await
                    }
                    "write" => {
                        trace
                            .write_document(
                                &a[0].as_str().unwrap(),
                                &a[1],
                                &a[2].as_str().unwrap(),
                                &a[3],
                            )
                            .await
                    }
                    "fault" => {
                        trace
                            .fault
                            .replace(Some(a[0].as_js_string().unwrap().clone()));
                        Ok(Json::Null)
                    }
                    "corrupt" => {
                        let (id, mut bytes): (String, Vec<u8>) = db
                            .query_row(
                                "SELECT id,bytes FROM blobs ORDER BY rowid LIMIT 1",
                                [],
                                |r| Ok((r.get(0)?, r.get(1)?)),
                            )
                            .unwrap();
                        bytes[0] ^= 1;
                        db.execute("UPDATE blobs SET bytes=? WHERE id=?", params![bytes, id])
                            .unwrap();
                        Ok(Json::Null)
                    }
                    "largeRequest" => {
                        for revision in [1.0, 2.0] {
                            let p = pin(200, revision);
                            let mut v = store
                                .journal
                                .read("task-board/run", &p, true)
                                .unwrap()
                                .value;
                            v.insert(
                                "originalRequest",
                                Json::object([(
                                    "body",
                                    Json::String(JsString(vec![
                                        120;
                                        a[0].as_f64().unwrap() as usize
                                    ])),
                                )]),
                            );
                            db.execute(
                                "UPDATE revisions SET value_json=? WHERE id=? AND revision=?",
                                params![
                                    canonical(&v, MANAGEMENT_FRAME_BYTES).unwrap(),
                                    id(200).to_utf8().unwrap(),
                                    revision
                                ],
                            )
                            .unwrap();
                        }
                        Ok(Json::Null)
                    }
                    "patch" => {
                        let key = a[0].as_str().unwrap();
                        let d = store.journal.read(&key, &a[1], false).unwrap();
                        let mut v = d.value;
                        for (k, x) in a[2].as_object().unwrap() {
                            v.insert(k.clone(), x.clone());
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
                        if let Some(name) = a.get(3) {
                            db.execute(
                                "UPDATE documents SET name=? WHERE id=?",
                                params![
                                    name.as_str().unwrap(),
                                    s(&d.pin, "objectId").to_utf8().unwrap()
                                ],
                            )
                            .unwrap();
                        }
                        Ok(Json::Null)
                    }
                    m => panic!("Unknown turn evidence action {m}"),
                };
                results.push(match actual {
                    Ok(v) => Json::object([("ok", v)]),
                    Err(e) => Json::object([(
                        "error",
                        Json::object([
                            ("code", Json::String(e.code_js())),
                            ("message", Json::String(e.message_js())),
                            ("outcome", e.outcome.as_str().into()),
                        ]),
                    )]),
                });
            }
            let actual = Json::object([
                ("results", Json::Array(results)),
                ("trace", Json::Array(trace.log.lock().unwrap().clone())),
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
            assert!(db.is_autocommit());
        }
    }
    assert_eq!(count, 124);
}

#[tokio::test(flavor = "current_thread")]
async fn actual_snapshot_and_transcript_reopen_without_io_or_writes_and_refuse_corrupt_receipts() {
    struct Temp(std::path::PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let temp =
        Temp(std::env::temp_dir().join(format!("task-board-turn-{}.sqlite", uuid::Uuid::new_v4())));
    let group = &data().as_array().unwrap()[0];
    let row = &group.required("rows").unwrap().as_array().unwrap()[2];
    let actions = row.required("actions").unwrap().as_array().unwrap();
    let initial = actions[0].required("args").unwrap().as_array().unwrap();
    let run = row.required("run").unwrap();
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let principal = JsString::from("caller");
    let mut saved_snapshot = None;
    let mut saved_transcript = None;
    let mut saved_full = None;
    for round in 0..2 {
        let db = Connection::open(&temp.0).unwrap();
        local_journal::initialize(&db).unwrap();
        let store = JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            client: Arc::new(NoRpc),
            contracts: Policy {
                allowed: HashSet::from([hash_json(run).unwrap()]),
            },
            limits: HistoryLimits::default(),
            now: || "2026-01-02T03:04:05.006Z".to_owned(),
            new_id: || uuid::Uuid::new_v4().to_string().into(),
            cache: None,
        };
        let turns = TurnEvidence {
            store: &store,
            catalogs: &catalogs,
            principal_id: &principal,
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
            let artifact = turns
                .save_snapshot(&initial[0], &initial[1], &initial[2])
                .await
                .unwrap();
            let snapshot = turns.snapshot(&pin(200, 2.0), &artifact).await.unwrap();
            assert_eq!(
                s(&artifact, "label"),
                &store
                    .journal
                    .name(s(artifact.required("object").unwrap(), "objectId"))
                    .unwrap()
            );
            let mut head = Json::Null;
            for action in actions
                .iter()
                .filter(|a| s(a, "method") == &JsString::from("savePage"))
            {
                let a = action.required("args").unwrap().as_array().unwrap();
                head = turns
                    .save_page(
                        &a[0],
                        &snapshot.document.value,
                        a[2].as_js_string().unwrap(),
                        &head,
                        a[4].as_f64().unwrap(),
                        a[5].as_js_string(),
                        &a[6],
                    )
                    .await
                    .unwrap();
            }
            let observed = &actions
                .iter()
                .find(|a| s(a, "method") == &JsString::from("saveRead"))
                .unwrap()
                .required("args")
                .unwrap()
                .as_array()
                .unwrap()[1];
            let read = ReadEvidence {
                store: &store,
                catalogs: &catalogs,
            }
            .save(
                &pin(200, 1.0),
                observed,
                &ReadIdentity {
                    observation_id: s(observed, "observationId").clone(),
                    caller_principal_id: principal.clone(),
                    service_node_id: s(observed, "serviceNodeId").clone(),
                    native_version: s(observed, "nativeVersion").clone(),
                    epoch: s(observed, "epoch").clone(),
                    method: s(observed, "method").clone(),
                    params: observed.required("params").unwrap().clone(),
                },
            )
            .await
            .unwrap();
            let mut value = actions
                .iter()
                .find(|a| s(a, "method") == &JsString::from("verifyTranscript"))
                .unwrap()
                .required("args")
                .unwrap()
                .as_array()
                .unwrap()[1]
                .clone();
            value.insert("snapshot", artifact.clone());
            value.insert("head", head);
            let mut verification = value.required("verification").unwrap().clone();
            verification.insert("artifact", read);
            value.insert("verification", verification);
            assert_eq!(
                turns
                    .verify_transcript(&pin(200, 2.0), &value)
                    .await
                    .unwrap()
                    .summary,
                JsString::from("answer")
            );
            let p = store
                .write_document(
                    "task-board/native-transcript",
                    &value,
                    "actual-transcript",
                    &Json::object([(
                        "create",
                        Json::object([
                            ("name", "Transcript".into()),
                            ("parentId", Json::String(id(200))),
                        ]),
                    )]),
                )
                .await
                .unwrap();
            saved_snapshot = Some(artifact);
            saved_transcript = Some(Json::object([
                ("object", p),
                ("label", "Transcript".into()),
                ("contentHash", hash_json(&value).unwrap().into()),
                ("mediaType", "application/json".into()),
            ]));
            let full_row = group
                .required("rows")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .find(|r| {
                    s(r, "name") == &JsString::from("full proof exact unsupported first page")
                })
                .unwrap();
            let full_actions = full_row.required("actions").unwrap().as_array().unwrap();
            let mut value = full_actions
                .iter()
                .find(|a| s(a, "method") == &JsString::from("verifyFull"))
                .unwrap()
                .required("args")
                .unwrap()
                .as_array()
                .unwrap()[1]
                .clone();
            value.insert("snapshot", saved_snapshot.as_ref().unwrap().clone());
            for (name, action) in ["unsupportedItems", "fullTurn", "verification"]
                .into_iter()
                .zip(
                    full_actions
                        .iter()
                        .filter(|a| s(a, "method") == &JsString::from("saveRead")),
                )
            {
                let observed = &action.required("args").unwrap().as_array().unwrap()[1];
                let artifact = turns
                    .reads()
                    .save(
                        &pin(200, 1.0),
                        observed,
                        &ReadIdentity {
                            observation_id: s(observed, "observationId").clone(),
                            caller_principal_id: principal.clone(),
                            service_node_id: s(observed, "serviceNodeId").clone(),
                            native_version: s(observed, "nativeVersion").clone(),
                            epoch: s(observed, "epoch").clone(),
                            method: s(observed, "method").clone(),
                            params: observed.required("params").unwrap().clone(),
                        },
                    )
                    .await
                    .unwrap();
                let mut reference = value.required(name).unwrap().clone();
                reference.insert("artifact", artifact);
                value.insert(name, reference);
            }
            let p = store
                .write_document(
                    "task-board/native-full-turn-transcript",
                    &value,
                    "actual-full-transcript",
                    &Json::object([(
                        "create",
                        Json::object([
                            ("name", "Full transcript".into()),
                            ("parentId", Json::String(id(200))),
                        ]),
                    )]),
                )
                .await
                .unwrap();
            saved_full = Some(Json::object([
                ("object", p),
                ("label", "Full transcript".into()),
                ("contentHash", hash_json(&value).unwrap().into()),
                ("mediaType", "application/json".into()),
            ]));
        } else {
            let before = db.total_changes();
            let snapshot = turns
                .snapshot(&pin(200, 2.0), saved_snapshot.as_ref().unwrap())
                .await
                .unwrap();
            assert_eq!(
                s(&snapshot.document.value, "status"),
                &JsString::from("completed")
            );
            assert_eq!(
                turns
                    .transcript(&pin(200, 2.0), saved_transcript.as_ref().unwrap())
                    .await
                    .unwrap()
                    .completion
                    .summary,
                JsString::from("answer")
            );
            for artifact in [
                saved_transcript.as_ref().unwrap(),
                saved_full.as_ref().unwrap(),
            ] {
                let result = verify_native_transcript(
                    TurnEvidence {
                        store: &store,
                        catalogs: &catalogs,
                        principal_id: &principal,
                    },
                    &pin(200, 2.0),
                    artifact,
                )
                .await
                .unwrap();
                assert_eq!(result.completion.summary, JsString::from("answer"));
                assert_eq!(result.completion.status, JsString::from("completed"));
            }
            assert_eq!(db.total_changes(), before);
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
            let error = turns
                .snapshot(&pin(200, 2.0), saved_snapshot.as_ref().unwrap())
                .await
                .err()
                .unwrap();
            assert_eq!(error.code, "task_board_evidence_mismatch");
            let error = verify_native_transcript(
                TurnEvidence {
                    store: &store,
                    catalogs: &catalogs,
                    principal_id: &principal,
                },
                &pin(200, 2.0),
                saved_full.as_ref().unwrap(),
            )
            .await
            .err()
            .unwrap();
            assert_eq!(error.code, "task_board_evidence_mismatch");
        }
        assert!(db.is_autocommit());
    }
}
