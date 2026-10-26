#[path = "reference-data.rs"]
mod reference_data;
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts,
    hash::hash_json,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_store::rusqlite::Connection;
use ivy_task_board::{
    evidence_bytes::{BlobStore, ChunkKey},
    journal_store::{Contracts, JournalStore},
    local_journal::{self, LocalJournal},
    local_retention::HistoryLimits,
    native_intent::{NativeIntent, NativeSlot, TaskPrompts},
    operations::{Document, Store},
    read_evidence::ReadEvidenceStore,
};
use std::{
    cell::RefCell,
    collections::HashSet,
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
};

fn s(v: &Json, k: &str) -> JsString {
    v.required(k).unwrap().as_js_string().unwrap().clone()
}
fn merge(v: &Json, patch: Option<&Json>) -> Json {
    let mut result = v.clone();
    if let Some(fields) = patch.and_then(Json::as_object) {
        for (k, v) in fields {
            result.insert(k.clone(), v.clone());
        }
    }
    result
}
struct Policy(HashSet<String>);
impl Contracts for Policy {
    fn version(&self, _: &str) -> IvyResult<String> {
        Ok("1.0.0".into())
    }
    fn local(&self, _: &str) -> IvyResult<bool> {
        Ok(false)
    }
    fn readable_version(&self, _: &str, v: &JsString) -> IvyResult<bool> {
        Ok(v == &"1.0.0".into())
    }
    fn validate_record(&self, key: &str, _: &JsString, v: &Json) -> IvyResult<()> {
        if key == "task-board/task" && self.0.contains(&hash_json(v)?) {
            Ok(())
        } else {
            Err(IvyError::new(
                "invalid_arguments",
                "Fixture record not admitted.",
            ))
        }
    }
    fn validate_definition(&self, n: &str, v: &Json) -> IvyResult<()> {
        super::native_intent::validate(n, v)
    }
}
struct Client {
    base: Json,
    input: Json,
    wire: Mutex<Vec<Json>>,
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            assert_eq!(
                method, "objects.read",
                "Preparation made an unexpected native/write RPC"
            );
            self.wire.lock().unwrap().push(params.clone());
            if let Some(code) = self.input.get("readFailure") {
                return Err(IvyError::new(code.as_str()?, "Fixture task unavailable."));
            }
            let pinned = params.get("revision").is_some();
            let value = merge(
                self.base
                    .required(if pinned { "initial" } else { "current" })?,
                self.input.get(if pinned { "initial" } else { "current" }),
            );
            let revision = if pinned { 1.0 } else { 2.0 };
            Ok(Json::object([
                (
                    "object",
                    Json::object([
                        ("id", params.required("objectId")?.clone()),
                        ("contractKey", "task-board/task".into()),
                        ("parentId", "root".into()),
                        ("effectivelyArchived", false.into()),
                    ]),
                ),
                (
                    "revision",
                    Json::object([
                        ("revision", Json::Number(revision)),
                        ("contractVersion", "1.0.0".into()),
                        ("mediaType", "application/json".into()),
                        ("contentHash", hash_json(&value)?.into()),
                        (
                            "byteLength",
                            Json::Number(canonical(&value, MANAGEMENT_FRAME_BYTES)?.len() as f64),
                        ),
                    ]),
                ),
                (
                    "content",
                    Json::object([("encoding", "json".into()), ("value", value)]),
                ),
            ]))
        })
    }
}
struct Trace<'a, S> {
    inner: &'a S,
    events: &'a RefCell<Vec<Json>>,
}
impl<S: Store + ReadEvidenceStore> Store for Trace<'_, S> {
    fn root_object_id(&self) -> Option<JsString> {
        self.inner.root_object_id()
    }
    fn contract_version(&self, _: &str) -> IvyResult<String> {
        panic!("Preparation requested a write contract")
    }
    fn retired_operation(&self, _: &JsString, _: &JsString) -> IvyResult<bool> {
        panic!("Preparation checked unrelated operation")
    }
    fn local_key(&self, _: &JsString) -> IvyResult<Option<String>> {
        panic!("Preparation checked unrelated local key")
    }
    fn complete_local_workflow(&self, _: &JsString, _: &JsString) -> IvyResult<()> {
        panic!("Preparation completed workflow")
    }
    async fn named(&self, _: &str, _: &str, _: Option<&JsString>) -> IvyResult<Option<Document>> {
        panic!("Preparation searched by name")
    }
    async fn read(&self, key: &str, address: &Json, upgrade: bool) -> IvyResult<Document> {
        self.events.borrow_mut().push(Json::Array(vec![
            "read".into(),
            key.into(),
            address.clone(),
            upgrade.into(),
        ]));
        Store::read(self.inner, key, address, upgrade).await
    }
    async fn write(&self, _: &str, _: &Json, _: &str, _: &Json) -> IvyResult<Json> {
        panic!("Preparation wrote a document")
    }
    async fn prepared_mutation_id(&self, _: &str) -> IvyResult<JsString> {
        panic!("Preparation requested mutation identity")
    }
    async fn write_prepared(&self, _: &str, _: &Json, _: &JsString, _: &Json) -> IvyResult<Json> {
        panic!("Preparation published mutation")
    }
}
impl<S: ReadEvidenceStore> BlobStore for Trace<'_, S> {
    type Pin = Json;
    type Error = IvyError;
    fn save(&self, _: ChunkKey, _: &str, _: &JsString, _: &[u8]) -> IvyResult<Json> {
        panic!("Preparation wrote a blob")
    }
    fn read(&self, _: ChunkKey, _: &Json, _: &JsString) -> IvyResult<Vec<u8>> {
        panic!("Preparation read a blob")
    }
}
impl<S: Store + ReadEvidenceStore> ReadEvidenceStore for Trace<'_, S> {
    fn validate(&self, n: &str, v: &Json) -> IvyResult<()> {
        self.events
            .borrow_mut()
            .push(Json::Array(vec!["validate".into(), n.into()]));
        self.inner.validate(n, v)
    }
    async fn read_document(&self, _: &str, _: &Json, _: bool) -> IvyResult<Document> {
        panic!("Preparation read evidence")
    }
    async fn write_document(&self, _: &str, _: &Json, _: &str, _: &Json) -> IvyResult<Json> {
        panic!("Preparation wrote evidence")
    }
}
struct Prompts<'a> {
    events: &'a RefCell<Vec<Json>>,
    input: &'a Json,
}
impl Prompts<'_> {
    fn render(&self, kind: &str, task: &Json, owner: &JsString, flag: Json) -> IvyResult<JsString> {
        self.events.borrow_mut().push(Json::Array(vec![
            "prompt".into(),
            kind.into(),
            task.clone(),
            Json::String(owner.clone()),
            flag,
        ]));
        if self.input.get("promptFailure") == Some(&Json::Bool(true)) {
            return Err(IvyError::new(
                "fixture_failed",
                "Fixture prompt unavailable.",
            ));
        }
        Ok(if self.input.get("large") == Some(&Json::Bool(true)) {
            "x".repeat(1048576).into()
        } else {
            self.input
                .get("prompt")
                .and_then(Json::as_js_string)
                .cloned()
                .unwrap_or_else(|| JsString("rendered\n".encode_utf16().chain([0xd800]).collect()))
        })
    }
}
impl TaskPrompts for Prompts<'_> {
    fn task_prompt(
        &self,
        t: &Json,
        o: &JsString,
        coordination: Option<&Json>,
    ) -> IvyResult<JsString> {
        self.render(
            "task",
            t,
            o,
            coordination
                .cloned()
                .unwrap_or_else(|| Json::object([("undefined", true.into())])),
        )
    }
    fn task_update_prompt(
        &self,
        t: &Json,
        o: &JsString,
        coordination: bool,
    ) -> IvyResult<JsString> {
        self.render("update", t, o, coordination.into())
    }
}
#[tokio::test(flavor = "current_thread")]
async fn native_preparation_matches_original_plans_task_pins_guards_and_prompt_boundaries() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = reference_data::read(include_bytes!("task-board-native-preparation-cases.json.gz"));
    let groups = parse(
        std::str::from_utf8(&output).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    let catalogs = CheckedNativeContracts::new(root);
    let mut count = 0;
    for group in groups.as_array().unwrap() {
        let base = group.required("base").unwrap();
        for row in group.required("rows").unwrap().as_array().unwrap() {
            count += 1;
            let input = row.required("input").unwrap();
            let initial = merge(base.required("initial").unwrap(), input.get("initial"));
            let current = merge(base.required("current").unwrap(), input.get("current"));
            let database = Connection::open_in_memory().unwrap();
            local_journal::initialize(&database).unwrap();
            let client = Arc::new(Client {
                base: base.clone(),
                input: input.clone(),
                wire: Mutex::default(),
            });
            let store = JournalStore {
                journal: LocalJournal {
                    connection: &database,
                    root_object_id: Some("root".into()),
                },
                client: client.clone(),
                contracts: Policy(
                    [hash_json(&initial).unwrap(), hash_json(&current).unwrap()].into(),
                ),
                limits: HistoryLimits::default(),
                now: || String::from("2026-01-02T03:04:05.006Z"),
                new_id: || JsString::from("unused"),
                cache: None,
            };
            let events = RefCell::default();
            let trace = Trace {
                inner: &store,
                events: &events,
            };
            let prompts = Prompts {
                events: &events,
                input,
            };
            let mut run = merge(base.required("run").unwrap(), input.get("run"));
            run.insert(
                "plan",
                merge(
                    base.required("run").unwrap().required("plan").unwrap(),
                    input.get("plan"),
                ),
            );
            let slot = match s(input, "slot").to_utf8().unwrap().as_str() {
                "thread" => NativeSlot::Thread,
                "turn" => NativeSlot::Turn,
                "steer" => NativeSlot::Steer,
                "interrupt" => NativeSlot::Interrupt,
                _ => panic!("Unknown fixture slot"),
            };
            assert_eq!(Json::from(slot.as_str()), *input.required("slot").unwrap());
            let before = database.total_changes();
            let result = NativeIntent {
                store: &trace,
                catalogs: &catalogs,
            }
            .intended_call(&run, slot, input.get("origin"), &prompts)
            .await;
            let result = match result {
                Ok(v) => Json::object([("ok", v)]),
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
                ("trace", Json::Array(events.into_inner())),
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
                canonical(
                    &Json::Array(client.wire.lock().unwrap().clone()),
                    MANAGEMENT_FRAME_BYTES
                )
                .unwrap(),
                canonical(row.required("reads").unwrap(), MANAGEMENT_FRAME_BYTES).unwrap(),
                "Wrong Task object/revision RPC"
            );
            assert_eq!(
                database.total_changes(),
                before,
                "Preparation changed its journal"
            );
            assert!(database.is_autocommit());
        }
    }
    assert_eq!(count, 430);
}
