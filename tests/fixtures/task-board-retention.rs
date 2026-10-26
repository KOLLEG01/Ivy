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
    local_retention::{HistoryLimits, LocalRetention},
};
use std::{
    path::PathBuf,
    sync::Mutex,
};

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
        };
        let retention = LocalRetention {
            journal: LocalJournal {
                connection: &db,
                root_object_id: None,
            },
            client: &client,
            limits: HistoryLimits {
                count: number(input, "limitRows"),
                bytes: number(input, "limitBytes"),
            },
        };
        let protected = input.get("protectedId").and_then(Json::as_js_string);
        let outcome = retention
            .collect(
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
            canonical(case.required("expected").unwrap(), MANAGEMENT_FRAME_BYTES).unwrap(),
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
    let client = Client {
        input: Json::Null,
        path: PathBuf::new(),
        time: now.to_utf8().unwrap(),
        trace: Mutex::default(),
    };
    let retention = LocalRetention {
        journal,
        client: &client,
        limits: HistoryLimits::default(),
    };
    retention
        .complete_workflow(&id, &now, Some(&"dependent".into()))
        .unwrap();
    retention
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
