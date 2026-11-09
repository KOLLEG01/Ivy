use ivy_contracts::{
    IvyError, IvyJson as Json, JsString,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
#[path = "reference-data.rs"]
mod reference_data;
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_store::rusqlite::{Connection, params, types::ValueRef};
use ivy_task_board::{
    local_journal::{self, LocalJournal},
    local_retention::HistoryLimits,
};
use std::{path::PathBuf, sync::Mutex};

fn text(value: &Json, key: &str) -> String {
    value.required(key).unwrap().as_str().unwrap()
}
fn array<'a>(value: &'a Json, key: &str) -> &'a [Json] {
    value.get(key).and_then(Json::as_array).unwrap_or(&[])
}
fn number(value: &Json, key: &str) -> u64 {
    value.get(key).and_then(Json::as_f64).unwrap_or(0.0) as u64
}
fn sql_value(value: &Json) -> String {
    value.as_str().unwrap()
}
struct Client {
    input: Json,
    path: PathBuf,
    time: String,
    trace: Mutex<Vec<Json>>,
    queries: std::sync::atomic::AtomicUsize,
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            if method == "objects.query" {
                assert_eq!(params.get("contractKey"), Some(&"task-board/task".into()));
                assert_eq!(params.get("includeArchived"), Some(&true.into()));
                assert_eq!(params.get("limit"), Some(&Json::Number(100.0)));
                self.queries
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return Ok(Json::object([
                    ("items", Json::Array(vec![])),
                    ("nextCursor", Json::Null),
                ]));
            }
            assert_eq!(method, "objects.read");
            self.trace
                .lock()
                .unwrap()
                .push(Json::Array(vec![method.into(), params.clone()]));
            if let Some(race) = self.input.get("race") {
                let db = Connection::open(&self.path).unwrap();
                if race.as_str()? == "revision" {
                    let value = &array(&self.input, "documents")[0].required("value")?;
                    db.execute(
                        "UPDATE documents SET current_revision=2 WHERE id=?",
                        ["root"],
                    )
                    .unwrap();
                    db.execute(
                        "INSERT INTO revisions VALUES(?,?,?,?)",
                        params![
                            "root",
                            2,
                            canonical(value, MANAGEMENT_FRAME_BYTES)?,
                            &self.time
                        ],
                    )
                    .unwrap();
                } else {
                    db.execute(
                        "INSERT INTO active_gates VALUES(?,?)",
                        params!["gate", "root"],
                    )
                    .unwrap();
                }
            }
            let id = text(&params, "objectId");
            let reply = self.input.get("remote").and_then(|remote| remote.get(&id));
            if let Some(code) = reply.and_then(|reply| reply.get("error")) {
                return Err(IvyError::new(code.as_str()?, "fixture remote"));
            }
            reply
                .cloned()
                .ok_or_else(|| IvyError::new("not_found", "fixture remote"))
        })
    }
}
fn seed(db: &Connection, input: &Json, time: &str) {
    for d in array(input, "documents") {
        let parent = d
            .required("parent")
            .unwrap()
            .as_js_string()
            .map(|s| s.to_utf8().unwrap());
        db.execute(
            "INSERT INTO documents VALUES(?,?,?,?,?,?,?)",
            params![
                text(d, "id"),
                text(d, "key"),
                text(d, "name"),
                parent,
                1,
                time,
                time
            ],
        )
        .unwrap();
        db.execute(
            "INSERT INTO revisions VALUES(?,?,?,?)",
            params![
                text(d, "id"),
                1,
                canonical(d.required("value").unwrap(), MANAGEMENT_FRAME_BYTES).unwrap(),
                time
            ],
        )
        .unwrap();
    }
    for blob in array(input, "blobs") {
        let b = blob.as_array().unwrap();
        let hex = b[4].as_str().unwrap();
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|n| u8::from_str_radix(&hex[n..n + 2], 16).unwrap())
            .collect::<Vec<_>>();
        db.execute(
            "INSERT INTO blobs VALUES(?,?,?,?,?)",
            params![
                sql_value(&b[0]),
                sql_value(&b[1]),
                sql_value(&b[2]),
                sql_value(&b[3]),
                bytes
            ],
        )
        .unwrap();
    }
    for (key, table, columns) in [
        ("gates", "active_gates", 2),
        ("operationGates", "operation_gates", 2),
        ("dependencies", "cleanup_dependencies", 2),
    ] {
        for entry in array(input, key) {
            let values = entry.as_array().unwrap();
            assert_eq!(columns, values.len());
            db.execute(
                &format!("INSERT INTO {table} VALUES(?,?)"),
                params![sql_value(&values[0]), sql_value(&values[1])],
            )
            .unwrap();
        }
    }
    for root in array(input, "roots") {
        db.execute(
            "INSERT INTO cleanup_roots VALUES(?,?)",
            params![sql_value(root), time],
        )
        .unwrap();
    }
    if let Some(fault) = input.get("fault") {
        db.execute_batch(&fault.as_str().unwrap()).unwrap();
    }
}
fn snapshot(db: &Connection, tables: &Json) -> Json {
    let mut result = Json::Object(vec![]);
    for (table, columns) in tables.as_object().unwrap() {
        let table = table.to_utf8().unwrap();
        let columns = columns.as_str().unwrap();
        let mut statement = db
            .prepare(&format!("SELECT {columns} FROM {table} ORDER BY 1,2"))
            .unwrap();
        let count = statement.column_count();
        let rows = statement
            .query_map([], |row| {
                Ok(Json::Array(
                    (0..count)
                        .map(|index| match row.get_ref(index).unwrap() {
                            ValueRef::Null => Json::Null,
                            ValueRef::Integer(v) => Json::Number(v as f64),
                            ValueRef::Real(v) => Json::Number(v),
                            ValueRef::Text(v) => std::str::from_utf8(v).unwrap().into(),
                            _ => panic!("Unexpected blob snapshot"),
                        })
                        .collect(),
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        result.insert(table, Json::Array(rows));
    }
    result
}

#[tokio::test]
async fn graph_collection_matches_original_retention_references_and_recovery() {
    let output = reference_data::read(include_bytes!("task-board-retention-cases.json.gz"));
    let fixtures = parse(
        std::str::from_utf8(&output).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    let time = text(&fixtures, "time");
    for case in array(&fixtures, "rows") {
        let temp = super::Temporary::new();
        let path = temp.0.join("retention.sqlite");
        let db = Connection::open(&path).unwrap();
        local_journal::initialize(&db).unwrap();
        let input = case.required("input").unwrap();
        seed(&db, input, &time);
        let client = Client {
            input: input.clone(),
            path,
            time: time.clone(),
            trace: Mutex::default(),
            queries: std::sync::atomic::AtomicUsize::new(0),
        };
        let protected = input.get("protectedId").and_then(Json::as_js_string);
        let (stored_rows, stored_bytes): (i64, i64) = db
            .query_row(
                "SELECT COALESCE(SUM(rows),0),COALESCE(SUM(bytes),0) FROM storage_usage",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let stored_rows = u64::try_from(stored_rows).unwrap();
        let stored_bytes = u64::try_from(stored_bytes).unwrap();
        let over_budget = stored_rows + number(input, "reservedRows") > number(input, "limitRows")
            || stored_bytes + number(input, "reservedBytes") > number(input, "limitBytes");
        let outcome = collect(
            &db,
            &client,
            HistoryLimits {
                count: number(input, "limitRows"),
                bytes: number(input, "limitBytes"),
            },
            number(input, "reservedBytes"),
            number(input, "reservedRows"),
            protected,
            input.get("incoming"),
        )
        .await;
        let result = match outcome {
            Ok(()) => Json::Null,
            Err(e) if e.code == "storage_unavailable" => {
                Json::object([("storageError", true.into())])
            }
            Err(e) => Json::object([("code", e.code.into()), ("message", e.message.into())]),
        };
        assert_eq!(
            client.queries.load(std::sync::atomic::Ordering::SeqCst),
            usize::from(over_budget && !array(input, "roots").is_empty()),
            "The single additional recovery query is required only under pressure"
        );
        let mut expected = case.required("expected").unwrap().clone();
        let tables = expected.get_mut("tables").unwrap();
        let Json::Array(markers) = tables.get_mut("markers").unwrap() else {
            unreachable!()
        };
        markers.push(Json::Array(vec![
            "pending-operations-index".into(),
            "1".into(),
        ]));
        if text(case, "name") == "FIFO retains newer root" {
            // The existing collector leaves 10% headroom. Both eligible closed
            // graphs retire here, retaining both original identities and receipts.
            for table in ["documents", "revisions", "cleanup_roots"] {
                tables.insert(table, Json::Array(vec![]));
            }
            tables.insert(
                "storage_usage",
                Json::Array(vec![
                    Json::Array(vec!["blobs".into(), Json::Number(0.0), Json::Number(0.0)]),
                    Json::Array(vec![
                        "revisions".into(),
                        Json::Number(0.0),
                        Json::Number(0.0),
                    ]),
                ]),
            );
            let Json::Array(retired) = tables.get_mut("retired_operations").unwrap() else {
                unreachable!()
            };
            retired.insert(
                0,
                Json::Array(vec![
                    "caller".into(),
                    "other".into(),
                    "sha256:original".into(),
                    "other".into(),
                ]),
            );
            assert_eq!(
                db.query_row(
                    "SELECT COUNT(*) FROM retired_operation_receipts",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
                2
            );
        }
        let actual = Json::object([
            ("result", result),
            ("trace", Json::Array(client.trace.lock().unwrap().clone())),
            (
                "tables",
                snapshot(&db, fixtures.required("tables").unwrap()),
            ),
        ]);
        assert_eq!(
            canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap(),
            canonical(&expected, MANAGEMENT_FRAME_BYTES).unwrap(),
            "{}",
            text(case, "name")
        );
        assert!(db.is_autocommit(), "{}", text(case, "name"));
    }
}

#[test]
fn local_document_cas_keeps_gate_index_and_current_only_revisions_atomic() {
    let db = Connection::open_in_memory().unwrap();
    local_journal::initialize(&db).unwrap();
    let journal = LocalJournal {
        connection: &db,
        root_object_id: None,
    };
    let now: JsString = "2026-01-02T03:04:05.006Z".into();
    let key = "task-board/coordination";
    let id: JsString = "gate".into();
    let name: JsString = "gate name".into();
    let first = Json::object([(
        "activeOperation",
        Json::object([("objectId", "operation".into())]),
    )]);
    let created = journal.create(key, &name, None, &first, &now, &id).unwrap();
    assert_eq!(
        journal
            .create(key, &name, None, &first, &now, &"different-id".into())
            .unwrap(),
        created
    );
    assert_eq!(journal.read(key, &created, true).unwrap().value, first);
    let different = Json::object([("activeOperation", Json::Null)]);
    assert_eq!(
        journal
            .create(key, &name, None, &different, &now, &id)
            .unwrap_err()
            .code,
        "revision_conflict"
    );
    assert_eq!(
        journal
            .update(key, &id, 7.0, &different, &now)
            .unwrap_err()
            .code,
        "revision_conflict"
    );
    db.execute_batch("CREATE TRIGGER fail_revision BEFORE INSERT ON revisions WHEN NEW.revision=2 BEGIN SELECT RAISE(ABORT,'fixture interruption'); END;").unwrap();
    assert_eq!(
        journal
            .update(key, &id, 1.0, &different, &now)
            .unwrap_err()
            .code,
        "storage_unavailable"
    );
    assert_eq!(journal.read(key, &created, true).unwrap().value, first);
    assert_eq!(
        db.query_row("SELECT operation_id FROM active_gates", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "operation"
    );
    db.execute_batch("DROP TRIGGER fail_revision").unwrap();
    let updated = journal.update(key, &id, 1.0, &different, &now).unwrap();
    assert_eq!(journal.read(key, &updated, true).unwrap().value, different);
    assert_eq!(
        journal.read(key, &created, true).unwrap_err().code,
        "task_board_evidence_mismatch"
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM active_gates", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let child = journal
        .create(
            "task-board/native-evidence",
            &"child".into(),
            Some(&id),
            &Json::Null,
            &now,
            &"child-id".into(),
        )
        .unwrap();
    assert_eq!(
        journal
            .read("task-board/native-evidence", &child, true)
            .unwrap_err()
            .code,
        "task_board_scope_mismatch"
    );
    assert_eq!(
        journal
            .read("task-board/native-evidence", &child, false)
            .unwrap()
            .value,
        Json::Null
    );
    let malformed_root = JsString(vec![0xd800]);
    let scoped = LocalJournal {
        connection: &db,
        root_object_id: Some(malformed_root.clone()),
    };
    let scoped_child = scoped
        .create(
            "task-board/native-evidence",
            &"scoped child".into(),
            Some(&malformed_root),
            &Json::Null,
            &now,
            &"scoped-id".into(),
        )
        .unwrap();
    // SQL binding replaces the surrogate, while JS compares the original root
    // identity. A storage conversion must not make that scope check succeed.
    assert_eq!(
        scoped
            .read("task-board/native-evidence", &scoped_child, true)
            .unwrap_err()
            .code,
        "task_board_scope_mismatch"
    );
    journal
        .complete_workflow(&id, &now, Some(&"dependent".into()))
        .unwrap();
    journal
        .complete_workflow(&id, &now, Some(&"dependent".into()))
        .unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM cleanup_dependencies", [], |row| row
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        1
    );
}

// The retired collector's frozen cases now exercise the production collector.
// Only its additional recovery-query contract and compact receipts extend the oracle.
pub(super) struct Catalog;
impl ivy_task_board::journal_store::Contracts for Catalog {
    fn version(&self, _: &str) -> ivy_contracts::IvyResult<String> {
        Ok("1.0.0".into())
    }
    fn local(&self, key: &str) -> ivy_contracts::IvyResult<bool> {
        Ok(!matches!(
            key,
            "task-board/task" | "task-board/run" | "task-board/delivery"
        ))
    }
    fn readable_version(&self, _: &str, version: &JsString) -> ivy_contracts::IvyResult<bool> {
        Ok(version == &"1.0.0".into())
    }
    fn validate_definition(&self, _: &str, _: &Json) -> ivy_contracts::IvyResult<()> {
        panic!("Collection cannot admit an action")
    }
    fn validate_record(
        &self,
        _: &str,
        version: &JsString,
        _: &Json,
    ) -> ivy_contracts::IvyResult<()> {
        assert_eq!(version, &"1.0.0".into());
        Ok(())
    }
}
struct NoDocuments;
impl
    ivy_task_board::journal_store::persistent_reads::local_history::HistoryDocuments<
        ivy_task_board::engine_read::read_failure::ReadFailure<std::convert::Infallible>,
    > for NoDocuments
{
    async fn history_document(
        &self,
        _: &str,
        _: Option<&Json>,
        _: bool,
        _: bool,
    ) -> Result<
        Json,
        ivy_task_board::engine_read::read_failure::ReadFailure<std::convert::Infallible>,
    > {
        panic!("These original cases contain no handoff or recovery lineage")
    }
}
pub(super) async fn collect<R: ivy_sdk::rpc::OptionalRpcClient>(
    db: &Connection,
    client: &R,
    limits: HistoryLimits,
    bytes: u64,
    rows: u64,
    protected: Option<&JsString>,
    incoming: Option<&Json>,
) -> ivy_contracts::IvyResult<()> {
    use ivy_task_board::{
        engine_read::read_failure::{ReadFailure, provider_read_error},
        journal_store::persistent_reads::{
            local_history::{HistoryCollections, LocalHistory},
            local_reads::SqliteReads,
            store::RpcObjectReads,
        },
    };
    let local = SqliteReads {
        journal: LocalJournal {
            connection: db,
            root_object_id: None,
        },
        contracts: &Catalog,
        parse: |raw: &str| parse(raw, MANAGEMENT_FRAME_BYTES).map_err(ReadFailure::from),
        sqlite_failure: |error| ReadFailure::from(ivy_store::sqlite::error(error)),
    };
    let objects = RpcObjectReads { client };
    let collections = HistoryCollections::default();
    LocalHistory::new(&local, &NoDocuments, &objects, &collections, limits)
        .collect::<std::convert::Infallible>(bytes, rows, protected, incoming)
        .await
        .map_err(provider_read_error)
}

impl ivy_sdk::rpc::OptionalRpcClient for Client {
    fn request_optional<'a>(
        &'a self,
        method: &'a str,
        params: Option<Json>,
        options: ivy_sdk::rpc::RequestOptions,
    ) -> ivy_sdk::rpc::RequestFuture<'a> {
        match params {
            Some(params) => ivy_sdk::rpc::RpcClient::request(self, method, params, options),
            None => Box::pin(async { Err(ivy_contracts::IvyError::internal()) }),
        }
    }
}
