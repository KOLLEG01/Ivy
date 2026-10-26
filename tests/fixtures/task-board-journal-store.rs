#[path = "reference-data.rs"]
mod reference_data;
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
    inspection_journal::InspectionJournal,
    journal_store::{Contracts, JournalStore, ReadCache},
    local_journal::{self, LocalJournal},
    local_retention::HistoryLimits,
    native_inspector::{InspectionStore, NativeInspector},
    operations::{Document, Operations, Store},
    read_evidence::ReadEvidenceStore,
};
use std::{
    cell::Cell,
    collections::HashSet,
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
};

const TIME: &str = "2026-01-02T03:04:05.000Z";
#[path = "task-board-journal-client.rs"]
mod client;
#[path = "task-board-local-pages.rs"]
mod pages;

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
            if self.input.get("pendingCleanup") == Some(&Json::Bool(true)) {
                return std::future::pending().await;
            }
            let failure = match method {
                "objects.read" => self.input.get("readError"),
                "objects.write" => self.input.get("writeError"),
                "serviceNodes.get" => self.input.get("ownerError"),
                _ => None,
            };
            if let Some(failure) = failure {
                return Err(IvyError::new(
                    text(failure, "code"),
                    if method == "serviceNodes.get" {
                        "Fixture owner read."
                    } else {
                        "Fixture RPC failure."
                    },
                )
                .with_outcome(
                    if failure.get("outcome") == Some(&Json::from("unknown")) {
                        Outcome::Unknown
                    } else {
                        Outcome::NotExecuted
                    },
                ));
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
                "serviceNodes.get" => {
                    let count = self
                        .trace
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|entry| {
                            entry.as_array().unwrap()[0] == Json::from("serviceNodes.get")
                        })
                        .count();
                    self.input
                        .get("owners")
                        .and_then(Json::as_array)
                        .and_then(|owners| owners.get(count - 1))
                        .unwrap_or(self.input.required("owner")?)
                        .clone()
                }
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
async fn inspection_owner_and_saved_primary_guards_use_the_journal_connection() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = reference_data::read(include_bytes!("task-board-inspection-owner-cases.json.gz"));
    let rows = parse(
        std::str::from_utf8(&output).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    let catalogs = ivy_contracts::checked_native_contract::CheckedNativeContracts::new(root);
    for row in rows.as_array().unwrap() {
        let input = row.required("input").unwrap();
        let db = Connection::open_in_memory().unwrap();
        local_journal::initialize(&db).unwrap();
        let client = Arc::new(Client {
            input: input.clone(),
            trace: Mutex::default(),
        });
        let store = JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            client: client.clone(),
            contracts: FixtureContracts {
                local: HashSet::new(),
                allowed: [hash_json(input.required("run").unwrap()).unwrap()].into(),
            },
            limits: HistoryLimits::default(),
            now: || TIME.into(),
            new_id: || panic!("An inspection guard must not create a record"),
            cache: None,
        };
        let inspection = InspectionJournal::new(
            &store,
            string(input, "principal"),
            string(input, "serviceNodeId"),
        );
        let mut outcomes = vec![];
        for _ in 0..input.get("repeat").and_then(Json::as_f64).unwrap_or(1.0) as usize {
            let result = if input
                .get("context")
                .and_then(Json::as_bool)
                .unwrap_or(false)
            {
                NativeInspector {
                    store: &inspection,
                    catalogs: &catalogs,
                }
                .context(input.required("pin").unwrap())
                .await
                .map(|context| context.value())
            } else {
                inspection.verify_owner().await.map(|_| Json::Null)
            };
            outcomes.push(match result {
                Ok(value) => value,
                Err(error) => Json::object([(
                    "error",
                    Json::object([
                        ("code", Json::String(error.code_js())),
                        ("message", Json::String(error.message_js())),
                        ("outcome", error.outcome.to_json()),
                    ]),
                )]),
            });
        }
        let actual = Json::object([
            ("outcomes", Json::Array(outcomes)),
            ("trace", Json::Array(client.trace.lock().unwrap().clone())),
        ]);
        assert_eq!(
            canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap(),
            canonical(row.required("expected").unwrap(), MANAGEMENT_FRAME_BYTES).unwrap(),
            "{}",
            text(row, "name")
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM documents", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn inspection_evidence_delegates_to_the_same_validated_sqlite_journal() {
    let db = Connection::open_in_memory().unwrap();
    local_journal::initialize(&db).unwrap();
    let value = Json::object([("state", "retained".into())]);
    let client = Arc::new(Client {
        input: Json::Null,
        trace: Mutex::default(),
    });
    let store = JournalStore {
        journal: LocalJournal {
            connection: &db,
            root_object_id: Some("root".into()),
        },
        client: client.clone(),
        contracts: FixtureContracts {
            local: ["task-board/native-read-evidence".into()].into(),
            allowed: [hash_json(&value).unwrap()].into(),
        },
        limits: HistoryLimits::default(),
        now: || TIME.into(),
        new_id: || "evidence".into(),
        cache: None,
    };
    let inspection = InspectionJournal::new(&store, "caller".into(), "board".into());
    let destination = Json::object([(
        "create",
        Json::object([("parentId", "root".into()), ("name", "record".into())]),
    )]);
    let pin = inspection
        .write_document(
            "task-board/native-read-evidence",
            &value,
            "mutation",
            &destination,
        )
        .await
        .unwrap();
    assert_eq!(
        inspection
            .named_document("task-board/native-read-evidence", "record", &"root".into())
            .await
            .unwrap()
            .unwrap()
            .pin,
        pin
    );
    assert_eq!(
        inspection
            .read_document("task-board/native-read-evidence", &pin, true)
            .await
            .unwrap()
            .value,
        value
    );
    assert_eq!(
        inspection
            .write_document(
                "task-board/native-read-evidence",
                &Json::Null,
                "other",
                &destination
            )
            .await
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    assert_eq!(
        ReadEvidenceStore::validate(&inspection, "unknown", &value)
            .unwrap_err()
            .code,
        "contract_not_found"
    );
    let chunk = BlobStore::save(
        &inspection,
        ChunkKey::NativeReadEvidence,
        "part",
        &"evidence".into(),
        b"exact",
    )
    .unwrap();
    assert_eq!(
        BlobStore::read(
            &inspection,
            ChunkKey::NativeReadEvidence,
            &chunk,
            &"evidence".into()
        )
        .unwrap(),
        b"exact"
    );
    assert_eq!(
        BlobStore::read(
            &inspection,
            ChunkKey::NativeReadEvidence,
            &chunk,
            &"other".into()
        )
        .unwrap_err()
        .code,
        "task_board_evidence_mismatch"
    );
    assert!(client.trace.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn concrete_journal_routes_match_original_storage_and_rpc_traces() {
    let encoded = if let Some(path) = std::env::var_os("IVY_TASK_BOARD_JOURNAL_FIXTURE") {
        std::fs::read_to_string(path).unwrap()
    } else {
        let output = reference_data::read(include_bytes!("task-board-journal-store-cases.json.gz"));
        String::from_utf8(output).unwrap()
    };
    let cases = parse(&encoded, MANAGEMENT_FRAME_BYTES).unwrap();
    let mut captured = vec![];
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
        for root in input
            .get("cleanupRoots")
            .and_then(Json::as_array)
            .unwrap_or(&[])
        {
            db.execute(
                "INSERT INTO cleanup_roots VALUES(?,?)",
                params![text(root, "root"), TIME],
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
        let id_allocations = Cell::new(0);
        let mut store = JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: input.required("root").unwrap().as_js_string().cloned(),
            },
            client: client.clone(),
            contracts,
            now: || TIME.into(),
            new_id: || {
                id_allocations.set(id_allocations.get() + 1);
                if let Some(winner) = input.get("allocationWinner") {
                    db.execute(
                        "INSERT INTO documents VALUES(?,?,?,?,?,?,?)",
                        params![
                            text(winner, "id"),
                            text(winner, "key"),
                            text(winner, "name"),
                            winner
                                .required("parent")
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
                            text(winner, "id"),
                            1,
                            canonical(winner.required("value").unwrap(), MANAGEMENT_FRAME_BYTES)
                                .unwrap(),
                            TIME
                        ],
                    )
                    .unwrap();
                }
                "new-document".into()
            },
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
                bytes: input
                    .get("limitBytes")
                    .and_then(Json::as_f64)
                    .unwrap_or(268435456.0) as u64,
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
                        ("outcome", error.outcome.to_json()),
                    ]),
                )]),
            });
        }
        let actual = Json::object([
            ("outcomes", Json::Array(outcomes)),
            ("trace", Json::Array(client.trace.lock().unwrap().clone())),
            ("idAllocations", Json::Number(id_allocations.get() as f64)),
            ("tables", snapshot(&db)),
        ]);
        assert_eq!(
            canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap(),
            canonical(case.required("expected").unwrap(), MANAGEMENT_FRAME_BYTES).unwrap(),
            "{}",
            text(case, "name")
        );
        captured.push(Json::object([
            ("name", Json::String(string(case, "name"))),
            ("actual", actual),
        ]));
    }
    if let Some(path) = std::env::var_os("IVY_TASK_BOARD_JOURNAL_NATIVE") {
        std::fs::write(
            path,
            canonical(&Json::Array(captured), MANAGEMENT_FRAME_BYTES).unwrap(),
        )
        .unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn local_writes_progress_without_running_history_collection() {
    use std::time::Duration;
    async fn write(
        store: &impl Store,
        key: &str,
        value: &Json,
        destination: &Json,
    ) -> IvyResult<Json> {
        let mutation = JsString::from("retained");
        tokio::time::timeout(
            Duration::from_secs(1),
            store.write_prepared(key, value, &mutation, destination),
        )
        .await
        .expect("Local workflow write waited for cleanup")
    }
    for pending in [false, true] {
        let db = Connection::open_in_memory().unwrap();
        local_journal::initialize(&db).unwrap();
        let journal = LocalJournal {
            connection: &db,
            root_object_id: Some("root".into()),
        };
        let active = Json::object([(
            "activeOperation",
            Json::object([
                ("objectId", "ongoing".into()),
                ("revision", Json::Number(1.0)),
            ]),
        )]);
        let released = Json::object([("activeOperation", Json::Null)]);
        let old = Json::object([
            ("phase", "succeeded".into()),
            ("actor", Json::object([("source", "scheduler".into())])),
            (
                "request",
                Json::object([
                    ("taskId", "task".into()),
                    ("expectedRevision", Json::Number(1.0)),
                ]),
            ),
            ("writes", Json::Array(vec![])),
        ]);
        let old_key = if pending {
            "task-board/operation"
        } else {
            "task-board/native-plan"
        };
        journal
            .create(
                old_key,
                &"old".into(),
                Some(&"root".into()),
                &old,
                &TIME.into(),
                &"old".into(),
            )
            .unwrap();
        journal
            .create(
                "task-board/coordination",
                &"gate".into(),
                Some(&"root".into()),
                &active,
                &TIME.into(),
                &"gate".into(),
            )
            .unwrap();
        db.execute(
            "INSERT INTO cleanup_roots VALUES(?,?)",
            params!["old", TIME],
        )
        .unwrap();
        let client = Arc::new(Client {
            input: Json::object([("pendingCleanup", pending.into())]),
            trace: Mutex::default(),
        });
        let value = Json::object([("value", "new".into())]);
        let different = Json::object([("value", "different".into())]);
        let store = JournalStore {
            journal,
            client: client.clone(),
            contracts: FixtureContracts {
                local: [
                    "task-board/native-plan".into(),
                    "task-board/coordination".into(),
                ]
                .into(),
                allowed: [&value, &different, &released]
                    .into_iter()
                    .map(|v| hash_json(v).unwrap())
                    .collect(),
            },
            now: || TIME.into(),
            new_id: || "fresh".into(),
            cache: None,
            limits: HistoryLimits { count: 0, bytes: 0 },
        };
        let create = Json::object([(
            "create",
            Json::object([("name", "fresh".into()), ("parentId", "root".into())]),
        )]);
        let update = Json::object([
            ("objectId", "gate".into()),
            ("expectedRevision", Json::Number(1.0)),
        ]);
        let fresh = write(&store, "task-board/native-plan", &value, &create)
            .await
            .unwrap();
        assert_eq!(
            fresh,
            Json::object([
                ("objectId", "fresh".into()),
                ("revision", Json::Number(1.0))
            ])
        );
        assert_eq!(
            write(&store, "task-board/native-plan", &value, &create)
                .await
                .unwrap(),
            fresh
        );
        assert_eq!(
            write(&store, "task-board/native-plan", &different, &create)
                .await
                .unwrap_err()
                .code,
            "revision_conflict"
        );
        assert_eq!(
            write(&store, "task-board/coordination", &released, &update)
                .await
                .unwrap(),
            Json::object([("objectId", "gate".into()), ("revision", Json::Number(2.0))])
        );
        assert_eq!(
            write(&store, "task-board/coordination", &released, &update)
                .await
                .unwrap_err()
                .code,
            "revision_conflict"
        );
        assert!(client.trace.lock().unwrap().is_empty());
        assert_eq!(
            hash_json(
                &store
                    .journal
                    .read(old_key, &Json::from("old"), true)
                    .unwrap()
                    .value
            )
            .unwrap(),
            hash_json(&old).unwrap()
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM cleanup_roots WHERE root_id='old'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM active_gates", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        if pending {
            assert!(
                tokio::time::timeout(Duration::from_secs(1), store.collect_local())
                    .await
                    .is_err()
            );
            assert_eq!(
                *client.trace.lock().unwrap(),
                vec![Json::Array(vec![
                    "objects.read".into(),
                    Json::object([("objectId", "task".into())])
                ])]
            );
        } else {
            store.collect_local().await.unwrap();
            assert!(store.journal.key(&"old".into()).unwrap().is_none());
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM cleanup_roots", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
            assert_eq!(
                store
                    .journal
                    .read("task-board/native-plan", &fresh, true)
                    .unwrap()
                    .value,
                value
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn original_operation_values_complete_through_real_journal_cas_and_cleanup() {
    let output = reference_data::read(include_bytes!("task-board-operations-cases.json.gz"));
    let cases = parse(
        std::str::from_utf8(&output).unwrap(),
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
async fn result_publication_keeps_current_artifacts_and_retained_finding_evidence() {
    let artifact = |id: &str| {
        Json::object([(
            "object",
            Json::object([("objectId", id.into()), ("revision", Json::Number(1.0))]),
        )])
    };
    for retained_findings in [false, true] {
        let db = Connection::open_in_memory().unwrap();
        local_journal::initialize(&db).unwrap();
        let direct = artifact("direct-artifact");
        let evidence = artifact("retained-evidence");
        let mut content = Json::object([("artifacts", Json::Array(vec![direct.clone()]))]);
        if retained_findings {
            content.insert(
                "checks",
                Json::Array(vec![Json::object([(
                    "evidence",
                    Json::Array(vec![evidence.clone()]),
                )])]),
            );
        }
        let value = Json::object([("content", content)]);
        let client = Arc::new(Client {
            input: Json::object([(
                "writeResult",
                Json::object([
                    ("object", Json::object([("id", "published-result".into())])),
                    ("revision", Json::object([("revision", Json::Number(1.0))])),
                ]),
            )]),
            trace: Mutex::default(),
        });
        // This case exercises the real reference publication path; the public
        // package acceptance separately admits the complete current contract.
        let store = JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            client: client.clone(),
            contracts: FixtureContracts {
                local: HashSet::new(),
                allowed: [hash_json(&value).unwrap()].into(),
            },
            limits: HistoryLimits::default(),
            now: || TIME.into(),
            new_id: || panic!("Remote publication must retain its Hive identity"),
            cache: None,
        };
        store
            .write(
                "task-board/result",
                &value,
                "result-publication",
                &Json::object([(
                    "create",
                    Json::object([("parentId", "root".into()), ("name", "Result".into())]),
                )]),
            )
            .await
            .unwrap();
        let trace = client.trace.lock().unwrap();
        let writes = trace
            .iter()
            .filter(|entry| entry.as_array().unwrap()[0] == Json::from("objects.write"))
            .collect::<Vec<_>>();
        assert_eq!(writes.len(), 1);
        let sent = &writes[0].as_array().unwrap()[1];
        assert_eq!(
            sent.required("content").unwrap().required("value").unwrap(),
            &value
        );
        let mut expected =
            Json::object([("artifact-0", direct.required("object").unwrap().clone())]);
        if retained_findings {
            expected.insert("artifact-1", evidence.required("object").unwrap().clone());
        }
        assert_eq!(sent.required("references").unwrap(), &expected);
    }
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
