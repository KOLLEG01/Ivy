use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    builtin_schema::SchemaBundle,
    checked_native_contract::CheckedNativeContracts,
    hash::hash_json,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_store::rusqlite::{Connection, params};
use ivy_task_board::{
    evidence_bytes::{BlobStore, ChunkKey},
    journal_store::{Contracts, JournalStore},
    local_journal::{self, LocalJournal},
    local_retention::HistoryLimits,
    native_evidence::NativeEvidence,
    native_intent::NativeIntent,
    native_recovery::ContextRecovery,
    operations::{Document, Store},
    read_evidence::ReadEvidenceStore,
};
use std::{
    cell::RefCell,
    collections::HashSet,
    path::Path,
    process::Command,
    sync::{Arc, OnceLock},
};

fn s(v: &Json, k: &str) -> JsString {
    v.required(k).unwrap().as_js_string().unwrap().clone()
}
fn pin(n: u64, revision: f64) -> Json {
    Json::object([
        (
            "objectId",
            format!("00000000-0000-4000-8000-{n:012}").into(),
        ),
        ("revision", Json::Number(revision)),
    ])
}
fn same(a: &Json, b: &Json) -> bool {
    canonical(a, MANAGEMENT_FRAME_BYTES).unwrap() == canonical(b, MANAGEMENT_FRAME_BYTES).unwrap()
}
fn bytes(hex: &Json) -> Vec<u8> {
    hex.as_str()
        .unwrap()
        .as_bytes()
        .chunks_exact(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}
fn validate(name: &str, value: &Json) -> IvyResult<()> {
    static BUNDLE: OnceLock<SchemaBundle> = OnceLock::new();
    let bundle = BUNDLE.get_or_init(|| {
        SchemaBundle::new(&[
            include_str!("../../specs/schemas/task-board.schema.json"),
            include_str!("../../specs/schemas/chat.schema.json"),
            include_str!("../../specs/schemas/agent.schema.json"),
            include_str!("../../specs/schemas/hive-wire.schema.json"),
        ])
        .unwrap()
    });
    canonical(value, MANAGEMENT_FRAME_BYTES)?;
    if bundle.matches(
        &format!("https://ivy.invalid/schemas/task-board.schema.json#/$defs/{name}"),
        value,
    )? {
        Ok(())
    } else {
        Err(IvyError::new(
            "invalid_arguments",
            "Value does not match its TaskBoard workflow contract.",
        ))
    }
}
struct Policy(HashSet<String>);
impl Contracts for Policy {
    fn version(&self, _: &str) -> IvyResult<String> {
        Ok("1.0.0".into())
    }
    // Route only this finite fixture's records through the real local journal.
    // This is not production contract registration or public Task routing.
    fn local(&self, _: &str) -> IvyResult<bool> {
        Ok(true)
    }
    fn readable_version(&self, _: &str, version: &JsString) -> IvyResult<bool> {
        Ok(version == &"1.0.0".into())
    }
    fn validate_record(&self, key: &str, _: &JsString, value: &Json) -> IvyResult<()> {
        if key == "task-board/native-evidence" {
            validate("NativeEvidence", value)
        } else if self.0.contains(&hash_json(value)?) {
            Ok(())
        } else {
            Err(IvyError::new(
                "invalid_arguments",
                "Fixture record not admitted.",
            ))
        }
    }
    fn validate_definition(&self, name: &str, value: &Json) -> IvyResult<()> {
        validate(name, value)
    }
}
struct NoRpc;
impl RpcClient for NoRpc {
    fn request<'a>(&'a self, method: &'a str, _: Json, _: RequestOptions) -> RequestFuture<'a> {
        panic!("Recovery unexpectedly made RPC: {method}")
    }
}
struct Trace<'a, S> {
    inner: &'a S,
    row: &'a Json,
    events: RefCell<Vec<Json>>,
}
impl<S> Trace<'_, S> {
    fn trace(&self, values: impl IntoIterator<Item = Json>) {
        self.events
            .borrow_mut()
            .push(Json::Array(values.into_iter().collect()));
    }
    fn fail(&self, key: &str, address: Option<&Json>) -> IvyResult<()> {
        let Some(f) = self.row.get("fail") else {
            return Ok(());
        };
        if f.get("key").is_none_or(|v| v == &Json::from(key))
            && f.get("address")
                .is_none_or(|v| address.is_some_and(|a| same(a, v)))
        {
            return Err(IvyError::new(
                s(f, "code").to_utf8().unwrap(),
                "Fixture unavailable.",
            ));
        }
        Ok(())
    }
}
impl<S: ReadEvidenceStore> BlobStore for Trace<'_, S> {
    type Pin = Json;
    type Error = IvyError;
    fn save(&self, _: ChunkKey, _: &str, _: &JsString, _: &[u8]) -> IvyResult<Json> {
        panic!("Recovery wrote a blob")
    }
    fn read(&self, key: ChunkKey, pin: &Json, parent: &JsString) -> IvyResult<Vec<u8>> {
        self.trace([
            "blob-read".into(),
            key.as_str().into(),
            pin.clone(),
            Json::String(parent.clone()),
        ]);
        self.fail("blob", None)?;
        BlobStore::read(self.inner, key, pin, parent)
    }
}
impl<S: Store + ReadEvidenceStore> ReadEvidenceStore for Trace<'_, S> {
    fn validate(&self, name: &str, value: &Json) -> IvyResult<()> {
        self.inner.validate(name, value)
    }
    async fn read_document(&self, key: &str, address: &Json, upgrade: bool) -> IvyResult<Document> {
        Store::read(self, key, address, upgrade).await
    }
    async fn write_document(&self, _: &str, _: &Json, _: &str, _: &Json) -> IvyResult<Json> {
        panic!("Recovery wrote evidence")
    }
}
impl<S: Store + ReadEvidenceStore> Store for Trace<'_, S> {
    fn root_object_id(&self) -> Option<JsString> {
        self.inner.root_object_id()
    }
    fn contract_version(&self, _: &str) -> IvyResult<String> {
        panic!("Recovery requested write version")
    }
    fn retired_operation(&self, _: &JsString, _: &JsString) -> IvyResult<bool> {
        panic!("Recovery checked unrelated operation")
    }
    fn local_key(&self, _: &JsString) -> IvyResult<Option<String>> {
        panic!("Recovery checked unrelated key")
    }
    fn complete_local_workflow(&self, _: &JsString, _: &JsString) -> IvyResult<()> {
        panic!("Recovery completed workflow")
    }
    async fn named(&self, _: &str, _: &str, _: Option<&JsString>) -> IvyResult<Option<Document>> {
        panic!("Recovery searched by name")
    }
    async fn read(&self, key: &str, address: &Json, upgrade: bool) -> IvyResult<Document> {
        self.trace([
            "read".into(),
            key.into(),
            address.clone(),
            Json::Bool(upgrade),
        ]);
        self.fail(key, Some(address))?;
        Store::read(self.inner, key, address, upgrade).await
    }
    async fn write(&self, _: &str, _: &Json, _: &str, _: &Json) -> IvyResult<Json> {
        panic!("Recovery wrote a document")
    }
    async fn prepared_mutation_id(&self, _: &str) -> IvyResult<JsString> {
        panic!("Recovery prepared mutation")
    }
    async fn write_prepared(&self, _: &str, _: &Json, _: &JsString, _: &Json) -> IvyResult<Json> {
        panic!("Recovery published mutation")
    }
}
fn oracle() -> Json {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/task-board-context-recovery-oracle.mjs",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap()
}
fn records(base: &Json, row: &Json) -> Vec<Json> {
    base.required("records")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            row.get("documents")
                .and_then(Json::as_array)
                .and_then(|list| {
                    list.iter()
                        .find(|v| same(v.required("pin").unwrap(), d.required("pin").unwrap()))
                })
                .unwrap_or(d)
                .clone()
        })
        .collect()
}
fn seed(database: &Connection, docs: &[Json], base: &Json, row: &Json) {
    local_journal::initialize(database).unwrap();
    let journal = LocalJournal {
        connection: database,
        root_object_id: Some("root".into()),
    };
    let mut sorted = docs.to_vec();
    sorted.sort_by_key(|d| {
        d.required("pin")
            .unwrap()
            .required("revision")
            .unwrap()
            .as_f64()
            .unwrap() as u64
    });
    for d in &sorted {
        let address = d.required("pin").unwrap();
        let revision = address.required("revision").unwrap().as_f64().unwrap();
        if revision == 1.0 {
            journal
                .create(
                    &s(d, "key").to_utf8().unwrap(),
                    &s(address, "objectId"),
                    Some(&s(d, "parentId")),
                    d.required("value").unwrap(),
                    &"2026-01-02T03:04:05.006Z".into(),
                    &s(address, "objectId"),
                )
                .unwrap();
        } else {
            journal
                .update(
                    &s(d, "key").to_utf8().unwrap(),
                    &s(address, "objectId"),
                    revision - 1.0,
                    d.required("value").unwrap(),
                    &"2026-01-02T03:04:05.006Z".into(),
                )
                .unwrap();
        }
    }
    for b in base.required("blobs").unwrap().as_array().unwrap() {
        let b = row
            .get("blobs")
            .and_then(Json::as_array)
            .and_then(|list| {
                list.iter()
                    .find(|v| same(v.required("object").unwrap(), b.required("object").unwrap()))
            })
            .unwrap_or(b);
        database
            .execute(
                "INSERT INTO blobs VALUES(?,?,?,?,?)",
                params![
                    s(b.required("object").unwrap(), "objectId")
                        .to_utf8()
                        .unwrap(),
                    s(b, "key").to_utf8().unwrap(),
                    s(b, "name").to_utf8().unwrap(),
                    s(b, "parent").to_utf8().unwrap(),
                    bytes(b.required("body").unwrap())
                ],
            )
            .unwrap();
    }
}
#[tokio::test(flavor = "current_thread")]
async fn recovery_proof_selection_matches_original_over_exact_journal_and_receipts() {
    let data = oracle();
    let base = data.required("base").unwrap();
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let rows = data.required("rows").unwrap().as_array().unwrap();
    assert!(rows.len() >= 160);
    for row in rows {
        let docs = records(base, row);
        let database = Connection::open_in_memory().unwrap();
        seed(&database, &docs, base, row);
        let store = JournalStore {
            journal: LocalJournal {
                connection: &database,
                root_object_id: Some("root".into()),
            },
            client: Arc::new(NoRpc),
            contracts: Policy(
                docs.iter()
                    .map(|d| hash_json(d.required("value").unwrap()).unwrap())
                    .collect(),
            ),
            limits: HistoryLimits::default(),
            now: || String::from("2026-01-02T03:04:05.006Z"),
            new_id: || JsString::from(uuid::Uuid::new_v4().to_string()),
            cache: None,
        };
        let trace = Trace {
            inner: &store,
            row,
            events: RefCell::default(),
        };
        let mut task = base.required("task").unwrap().clone();
        if let Some(patch) = row.get("task").and_then(Json::as_object) {
            let mut value = task.required("value").unwrap().clone();
            for (k, v) in patch {
                value.insert(k.clone(), v.clone());
            }
            task.insert("value", value);
        }
        let task = Document {
            pin: task.required("pin").unwrap().clone(),
            value: task.required("value").unwrap().clone(),
            parent_id: Some("root".into()),
            content_hash: String::new(),
        };
        let guard = |value: &Json, run_id: &JsString| {
            trace.trace([
                "question".into(),
                value.clone(),
                Json::String(run_id.clone()),
            ]);
            match row
                .get("guard")
                .and_then(Json::as_js_string)
                .map(JsString::to_utf8)
                .transpose()
                .unwrap()
                .as_deref()
            {
                Some("blocked") => Ok(true),
                Some("failed") => Err(IvyError::new(
                    "fixture_failed",
                    "Question guard unavailable.",
                )),
                Some("not_found") => Err(IvyError::new("not_found", "Question guard unavailable.")),
                _ => Ok(false),
            }
        };
        let subject = ContextRecovery {
            store: &trace,
            catalogs: &catalogs,
            principal_id: "caller".into(),
            unanswered_question: guard,
        };
        let before = database.total_changes();
        let result = if row.required("mode").unwrap() == &Json::from("archived") {
            subject.unstarted_archived_context(&task).await
        } else {
            subject.unstarted_missing_context(&task).await
        };
        let result = match result {
            Ok(v) => Json::object([("ok", Json::Bool(v))]),
            Err(e) => Json::object([(
                "error",
                Json::object([
                    ("code", Json::String(e.code_js())),
                    ("message", Json::String(e.message_js())),
                ]),
            )]),
        };
        let actual = Json::object([
            ("result", result),
            ("trace", Json::Array(trace.events.into_inner())),
        ]);
        assert_eq!(
            hash_json(&actual).unwrap(),
            s(row, "expectedHash").to_utf8().unwrap(),
            "{}: {}",
            s(row, "name").to_utf8().unwrap(),
            canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap()
        );
        assert_eq!(
            database.total_changes(),
            before,
            "Recovery mutated its journal"
        );
        assert!(database.is_autocommit());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn recovery_uses_rust_saved_receipt_and_refuses_actual_retained_snapshot_loss() {
    let data = oracle();
    let base = data.required("base").unwrap();
    let row = Json::object([] as [(&str, Json); 0]);
    let docs = records(base, &row);
    let database = Connection::open_in_memory().unwrap();
    seed(&database, &docs, base, &row);
    let mut store = JournalStore {
        journal: LocalJournal {
            connection: &database,
            root_object_id: Some("root".into()),
        },
        client: Arc::new(NoRpc),
        contracts: Policy(
            docs.iter()
                .map(|d| hash_json(d.required("value").unwrap()).unwrap())
                .collect(),
        ),
        limits: HistoryLimits::default(),
        now: || String::from("2026-01-02T03:04:05.006Z"),
        new_id: || JsString::from(uuid::Uuid::new_v4().to_string()),
        cache: None,
    };
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let saved = Store::read(&store, "task-board/run", &pin(200, 1.0), true)
        .await
        .unwrap();
    let mut call = Store::read(&store, "task-board/native-call", &pin(300, 2.0), true)
        .await
        .unwrap();
    let identity = NativeIntent {
        store: &store,
        catalogs: &catalogs,
    }
    .evidence_identity(&saved.value, &call.value, &"caller".into())
    .unwrap();
    let observation = parse(
        std::str::from_utf8(&bytes(
            &base.required("blobs").unwrap().as_array().unwrap()[0]
                .required("body")
                .unwrap(),
        ))
        .unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    let artifact = NativeEvidence {
        store: &store,
        catalogs: &catalogs,
    }
    .save(&call.pin, &observation, &identity)
    .await
    .unwrap();
    call.value.insert("evidence", artifact);
    store.contracts.0.insert(hash_json(&call.value).unwrap());
    store
        .journal
        .update(
            "task-board/native-call",
            &s(&call.pin, "objectId"),
            2.0,
            &call.value,
            &"2026-01-02T03:04:05.006Z".into(),
        )
        .unwrap();
    let task = base.required("task").unwrap();
    let task = Document {
        pin: task.required("pin").unwrap().clone(),
        value: task.required("value").unwrap().clone(),
        parent_id: Some("root".into()),
        content_hash: String::new(),
    };
    let subject = ContextRecovery {
        store: &store,
        catalogs: &catalogs,
        principal_id: "caller".into(),
        unanswered_question: |_: &Json, _: &JsString| Ok(false),
    };
    let before = database.total_changes();
    assert!(subject.unstarted_archived_context(&task).await.unwrap());
    assert_eq!(database.total_changes(), before);
    // Lose the exact pre-closure Run revision while retaining its terminal Run.
    database
        .execute(
            "DELETE FROM revisions WHERE id=? AND revision=1",
            [s(&pin(200, 1.0), "objectId").to_utf8().unwrap()],
        )
        .unwrap();
    let before = database.total_changes();
    let error = subject.unstarted_archived_context(&task).await.unwrap_err();
    assert_eq!(error.code, "task_board_evidence_mismatch");
    assert_eq!(error.message, "The local workflow revision is unavailable.");
    assert_eq!(database.total_changes(), before);
}
