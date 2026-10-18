use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    hash::hash_json,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_store::rusqlite::{Connection, params, types::ValueRef};
use ivy_task_board::{
    evidence_bytes::{BlobStore, ChunkKey},
    journal_store::{Contracts, JournalStore, ReadCache},
    local_journal::{self, LocalJournal},
    local_retention::HistoryLimits,
    operations::{Document, Operations, Store},
    read_evidence::ReadEvidenceStore,
};
use std::{
    collections::HashSet,
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
};

const TIME: &str = "2026-01-02T03:04:05.000Z";
const TABLES: [&str; 11] = [
    "documents",
    "revisions",
    "blobs",
    "markers",
    "cleanup_roots",
    "cleanup_dependencies",
    "active_gates",
    "operation_gates",
    "workspace_reservations",
    "storage_usage",
    "retired_operations",
];

fn string(value: &Json, key: &str) -> JsString {
    value.required(key).unwrap().as_js_string().unwrap().clone()
}
fn text(value: &Json, key: &str) -> String {
    string(value, key).to_utf8().unwrap()
}
fn values<'a>(value: &'a Json, key: &str) -> &'a [Json] {
    value.required(key).unwrap().as_array().unwrap()
}
struct Client {
    input: Json,
    trace: Mutex<Vec<Json>>,
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            self.trace
                .lock()
                .unwrap()
                .push(Json::Array(vec![method.into(), params.clone()]));
            let failure = match method {
                "objects.read" => self.input.get("readError"),
                "objects.write" => self.input.get("writeError"),
                _ => None,
            };
            if let Some(failure) = failure {
                return Err(IvyError::new(text(failure, "code"), "Fixture RPC failure.")
                    .with_outcome(if failure.get("outcome") == Some(&Json::from("unknown")) {
                        Outcome::Unknown
                    } else {
                        Outcome::NotExecuted
                    }));
            }
            Ok(match method {
                "objects.read" => {
                    // Make overlapping reads actually suspend before filling the cache.
                    tokio::task::yield_now().await;
                    let mut result = self.input.required("readResult")?.clone();
                    if self
                        .input
                        .get("dynamicId")
                        .and_then(Json::as_bool)
                        .unwrap_or(false)
                    {
                        let mut object = result.required("object")?.clone();
                        object.insert("id", params.required("objectId")?.clone());
                        result.insert("object", object);
                    }
                    result
                }
                "objects.write" => self.input.required("writeResult")?.clone(),
                "objects.query" => self.input.get("query").cloned().unwrap_or_else(|| {
                    Json::object([
                        (
                            "items",
                            Json::Array(vec![Json::object([
                                ("objectId", "remote".into()),
                                ("revision", Json::Number(1.0)),
                            ])]),
                        ),
                        ("nextCursor", Json::Null),
                    ])
                }),
                "system.status" => Json::object([(
                    "runtimeEpoch",
                    "11111111-1111-4111-8111-111111111111".into(),
                )]),
                _ => panic!("Unexpected RPC {method}"),
            })
        })
    }
}
// Exact fixture-value admission only. This is deliberately not a substitute for
// the pending production TaskBoard schema bundle.
struct FixtureContracts {
    local: HashSet<String>,
    allowed: HashSet<String>,
}
impl Contracts for FixtureContracts {
    fn version(&self, _: &str) -> IvyResult<String> {
        Ok("1.0.0".into())
    }
    fn local(&self, key: &str) -> IvyResult<bool> {
        Ok(self.local.contains(key))
    }
    fn readable_version(&self, _: &str, version: &JsString) -> IvyResult<bool> {
        Ok(version == &JsString::from("1.0.0"))
    }
    fn validate_record(&self, _: &str, version: &JsString, value: &Json) -> IvyResult<()> {
        if version == &JsString::from("1.0.0") && self.allowed.contains(&hash_json(value)?) {
            Ok(())
        } else {
            Err(IvyError::new(
                "invalid_arguments",
                "Fixture record not admitted.",
            ))
        }
    }
    fn validate_definition(&self, _: &str, _: &Json) -> IvyResult<()> {
        Err(IvyError::new(
            "contract_not_found",
            "Fixture definition not admitted.",
        ))
    }
}
fn document(value: Document) -> Json {
    Json::object([
        ("pin", value.pin),
        ("value", value.value),
        (
            "parentId",
            value.parent_id.map(Json::String).unwrap_or(Json::Null),
        ),
        ("contentHash", value.content_hash.into()),
    ])
}
fn snapshot(db: &Connection) -> Json {
    Json::object(TABLES.into_iter().map(|table| {
        let mut statement = db
            .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
            .unwrap();
        let columns = statement
            .column_names()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let rows = statement
            .query_map([], |row| {
                Ok(Json::object(columns.iter().enumerate().map(
                    |(index, column)| {
                        let value = match row.get_ref(index).unwrap() {
                            ValueRef::Null => Json::Null,
                            ValueRef::Integer(value) => Json::Number(value as f64),
                            ValueRef::Real(value) => Json::Number(value),
                            ValueRef::Text(value) => std::str::from_utf8(value).unwrap().into(),
                            ValueRef::Blob(value) => value
                                .iter()
                                .map(|byte| format!("{byte:02x}"))
                                .collect::<String>()
                                .into(),
                        };
                        (column.clone(), value)
                    },
                )))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        (table, Json::Array(rows))
    }))
}

#[tokio::test(flavor = "current_thread")]
async fn concrete_journal_routes_match_original_storage_and_rpc_traces() {
    let output = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/task-board-journal-store-oracle.mjs",
        ])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cases = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    for case in cases.as_array().unwrap() {
        let input = case.required("input").unwrap();
        let db = Connection::open_in_memory().unwrap();
        local_journal::initialize(&db).unwrap();
        for d in values(input, "documents") {
            db.execute(
                "INSERT INTO documents VALUES(?,?,?,?,?,?,?)",
                params![
                    text(d, "id"),
                    text(d, "key"),
                    text(d, "id"),
                    d.required("parent")
                        .unwrap()
                        .as_js_string()
                        .map(|s| s.to_utf8().unwrap()),
                    1,
                    TIME,
                    TIME
                ],
            )
            .unwrap();
            db.execute(
                "INSERT INTO revisions VALUES(?,?,?,?)",
                params![
                    text(d, "id"),
                    1,
                    canonical(d.required("value").unwrap(), MANAGEMENT_FRAME_BYTES).unwrap(),
                    TIME
                ],
            )
            .unwrap();
        }
        let client = Arc::new(Client {
            input: input.clone(),
            trace: Mutex::default(),
        });
        let contracts = FixtureContracts {
            local: values(input, "localKeys")
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect(),
            allowed: values(input, "allowed")
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect(),
        };
        let first_cache = ReadCache::default();
        let second_cache = ReadCache::default();
        let mut store = JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: input.required("root").unwrap().as_js_string().cloned(),
            },
            client: client.clone(),
            contracts,
            now: || TIME.into(),
            new_id: || "new-document".into(),
            cache: input
                .get("cache")
                .and_then(Json::as_bool)
                .unwrap_or(false)
                .then_some(&first_cache),
            limits: HistoryLimits {
                count: input
                    .get("limitRows")
                    .and_then(Json::as_f64)
                    .unwrap_or(10000.0) as u64,
                ..HistoryLimits::default()
            },
        };
        let mut outcomes = vec![];
        for step in values(input, "steps") {
            if step.get("clearCache").is_some() {
                store.cache = Some(&second_cache);
                continue;
            }
            if let Some(parallel) = step.get("parallel").and_then(Json::as_array) {
                let left = &parallel[0];
                let right = &parallel[1];
                let left_key = text(left, "key");
                let right_key = text(right, "key");
                let (left, right) = tokio::join!(
                    store.read_record(&left_key, left.required("address").unwrap(), true, false),
                    store.read_record(&right_key, right.required("address").unwrap(), true, false)
                );
                outcomes.push(Json::Array(vec![
                    document(left.unwrap()),
                    document(right.unwrap()),
                ]));
                continue;
            }
            let key = text(step, "key");
            let result = match text(step, "method").as_str() {
                "read" => store
                    .read_record(
                        &key,
                        step.required("address").unwrap(),
                        step.get("primary").and_then(Json::as_bool).unwrap_or(true),
                        step.get("includeArchived")
                            .and_then(Json::as_bool)
                            .unwrap_or(false),
                    )
                    .await
                    .map(|mut selected| {
                        let original = document(selected.clone());
                        if step.get("mutate").is_some() {
                            selected.value.insert("value", "caller mutation".into());
                        }
                        original
                    }),
                "named" => store
                    .named_record(
                        &key,
                        &string(step, "name"),
                        step.get("parent")
                            .unwrap_or(input.required("root").unwrap())
                            .as_js_string(),
                    )
                    .await
                    .map(|d| d.map(document).unwrap_or(Json::Null)),
                "write" => {
                    store
                        .write(
                            &key,
                            step.required("value").unwrap(),
                            "retained-identity",
                            step.required("destination").unwrap(),
                        )
                        .await
                }
                "writePrepared" => {
                    store
                        .write_prepared(
                            &key,
                            step.required("value").unwrap(),
                            &"retained-identity".into(),
                            step.required("destination").unwrap(),
                        )
                        .await
                }
                method => panic!("Unexpected fixture method {method}"),
            };
            outcomes.push(match result {
                Ok(value) => value,
                Err(error) => Json::object([(
                    "error",
                    Json::object([
                        ("code", Json::String(error.code_js())),
                        ("message", Json::String(error.message_js())),
                        ("outcome", error.outcome.as_str().into()),
                    ]),
                )]),
            });
        }
        let actual = Json::object([
            ("outcomes", Json::Array(outcomes)),
            ("trace", Json::Array(client.trace.lock().unwrap().clone())),
            ("tables", snapshot(&db)),
        ]);
        assert_eq!(
            canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap(),
            canonical(case.required("expected").unwrap(), MANAGEMENT_FRAME_BYTES).unwrap(),
            "{}",
            text(case, "name")
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn original_operation_values_complete_through_real_journal_cas_and_cleanup() {
    let output = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/task-board-operations-oracle.mjs",
        ])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cases = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    let case = cases
        .as_array()
        .unwrap()
        .iter()
        .find(|c| text(c, "name") == "conflicting completed outcome")
        .unwrap();
    let expected = case.required("expected").unwrap();
    let allowed = values(expected, "trace")
        .iter()
        .filter_map(|row| {
            let row = row.as_array().unwrap();
            (row[0] == Json::from("write") && row[1] == Json::from("task-board/operation"))
                .then(|| hash_json(&row[2]).unwrap())
        })
        .collect();
    let db = Connection::open_in_memory().unwrap();
    local_journal::initialize(&db).unwrap();
    let client = Arc::new(Client {
        input: Json::Null,
        trace: Mutex::default(),
    });
    let operations = Operations {
        store: JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            client: client.clone(),
            contracts: FixtureContracts {
                local: ["task-board/operation".into()].into(),
                allowed,
            },
            limits: HistoryLimits::default(),
            now: || TIME.into(),
            new_id: || "operation-record".into(),
            cache: None,
        },
        now: || TIME.into(),
    };
    let steps = values(case, "steps");
    let args = values(&steps[0], "args");
    let begin = operations.begin(&args[0], &args[1]).await.unwrap();
    assert_eq!(
        hash_json(&begin.value).unwrap(),
        hash_json(values(expected, "outcomes")[0].required("value").unwrap()).unwrap()
    );
    assert_eq!(
        operations.begin(&args[0], &args[1]).await.unwrap().pin,
        begin.pin
    );
    let id = string(&begin.pin, "objectId");
    let outcome = &values(&steps[1], "args")[1];
    assert_eq!(operations.finish(&id, outcome).await.unwrap(), *outcome);
    assert_eq!(operations.finish(&id, outcome).await.unwrap(), *outcome);
    assert_eq!(
        Store::read(
            &operations.store,
            "task-board/operation",
            &Json::String(id.clone()),
            true
        )
        .await
        .unwrap()
        .value
        .required("phase")
        .unwrap(),
        &Json::from("succeeded")
    );
    assert_eq!(
        db.query_row(
            "SELECT current_revision FROM documents WHERE id=?",
            ["operation-record"],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    assert_eq!(
        db.query_row(
            "SELECT delete_after FROM cleanup_roots WHERE root_id=?",
            ["operation-record"],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        TIME
    );
    assert!(client.trace.lock().unwrap().is_empty());
    let first = operations
        .store
        .prepared_mutation_id("scope")
        .await
        .unwrap();
    assert_eq!(
        operations
            .store
            .prepared_mutation_id("scope")
            .await
            .unwrap(),
        first
    );
    assert_ne!(
        operations
            .store
            .prepared_mutation_id("other")
            .await
            .unwrap(),
        first
    );
    let trace = client.trace.lock().unwrap();
    assert_eq!(trace.len(), 1);
    assert_eq!(trace[0].as_array().unwrap()[0], Json::from("system.status"));
}

#[tokio::test(flavor = "current_thread")]
async fn retained_operations_use_the_concrete_journal_and_require_admission() {
    let db = Connection::open_in_memory().unwrap();
    local_journal::initialize(&db).unwrap();
    let client = Arc::new(Client {
        input: Json::Null,
        trace: Mutex::default(),
    });
    let value = Json::object([("value", "allowed".into())]);
    let store = JournalStore {
        journal: LocalJournal {
            connection: &db,
            root_object_id: Some("root".into()),
        },
        client: client.clone(),
        contracts: FixtureContracts {
            local: [
                "task-board/native-plan".into(),
                "task-board/operation".into(),
            ]
            .into(),
            allowed: [hash_json(&value).unwrap()].into(),
        },
        limits: HistoryLimits::default(),
        now: || TIME.into(),
        new_id: || "new-document".into(),
        cache: None,
    };
    let destination = Json::object([(
        "create",
        Json::object([("parentId", "root".into()), ("name", "record".into())]),
    )]);
    assert_eq!(
        store
            .write("task-board/native-plan", &Json::Null, "id", &destination)
            .await
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    assert!(client.trace.lock().unwrap().is_empty());
    assert_eq!(
        snapshot(&db)
            .required("documents")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let pin = store
        .write("task-board/native-plan", &value, "id", &destination)
        .await
        .unwrap();
    assert_eq!(
        Store::read(&store, "task-board/native-plan", &pin, true)
            .await
            .unwrap()
            .value,
        value
    );
    assert_eq!(
        ReadEvidenceStore::validate(&store, "unknown", &value)
            .unwrap_err()
            .code,
        "contract_not_found"
    );
    let chunk = BlobStore::save(
        &store,
        ChunkKey::NativeReadEvidence,
        "part",
        &"owner".into(),
        b"exact",
    )
    .unwrap();
    assert_eq!(
        BlobStore::read(
            &store,
            ChunkKey::NativeReadEvidence,
            &chunk,
            &"owner".into()
        )
        .unwrap(),
        b"exact"
    );
    assert_eq!(
        BlobStore::read(
            &store,
            ChunkKey::NativeReadEvidence,
            &chunk,
            &"other".into()
        )
        .unwrap_err()
        .code,
        "task_board_evidence_mismatch"
    );
    // The ordinary Operations entry point cannot persist a record the supplied
    // validator has not admitted, even when the raw SQLite journal is writable.
    let operations = Operations {
        store,
        now: || TIME.into(),
    };
    assert_eq!(
        operations
            .begin(
                &Json::object([("operationId", "request".into())]),
                &Json::object([("principalId", "caller".into())])
            )
            .await
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    assert_eq!(
        snapshot(&db)
            .required("documents")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(client.trace.lock().unwrap().is_empty());
}
