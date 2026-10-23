use super::*;
use crate::{
    engine_execution::local::failed_publication_graph,
    journal_store::{
        mutation_ports::{LocalMutationStore, typed_write_cause},
        persistent_reads::mutation_journal::MutationJournal,
    },
    local_gates::LocalGates,
    operations::{
        Operations,
        local::{begin_graph, failure_graph},
    },
    publication_coordination::local::Calls,
};
use ivy_contracts::{Outcome, hash::hash_json};

const OPERATION: &str = "task-board/operation";
const GATE: &str = "task-board/coordination";
const NAMES: [&str; 2] = [
    "TaskBoard publication gate",
    "TaskBoard publication gate task",
];

fn data() -> Json {
    ivy_contracts::json::parse(
        include_str!("task-board-gate-journal.json"),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap()
}
fn rows(connection: &Connection, sql: &str, columns: &[&str]) -> Json {
    let mut statement = connection.prepare(sql).unwrap();
    let rows = statement
        .query_map([], |row| {
            Ok(Json::object(columns.iter().enumerate().map(|(i, key)| {
                let value = match row.get_ref(i).unwrap() {
                    ivy_store::rusqlite::types::ValueRef::Text(value) => {
                        Json::from(std::str::from_utf8(value).unwrap())
                    }
                    ivy_store::rusqlite::types::ValueRef::Integer(value) => {
                        Json::Number(value as f64)
                    }
                    _ => panic!("unexpected projected column"),
                };
                (*key, value)
            })))
        })
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    Json::Array(rows)
}
async fn case(name: &str) {
    let data = data();
    let row = data
        .required("cases")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row.get("name") == Some(&name.into()))
        .unwrap();
    let marker = 63;
    let source = Source::new("journal", &marker);
    let connection = Connection::open_in_memory().unwrap();
    initialize(&connection).unwrap();
    let catalog = Catalog(Cell::new(0));
    let last_sql = RefCell::new(None::<Failure<'_>>);
    let sql_count = Cell::new(0);
    let sql_failure = |error| {
        sql_count.set(sql_count.get() + 1);
        let cause = Failure::source(Opaque {
            borrowed: &marker,
            token: Rc::new(()),
            native: Some(error),
        });
        *last_sql.borrow_mut() = Some(cause.clone());
        cause
    };
    let local = SqliteReads {
        journal: LocalJournal {
            connection: &connection,
            root_object_id: Some("root".into()),
        },
        contracts: &catalog,
        parse: |value: &str| {
            ivy_contracts::json::parse(value, MANAGEMENT_FRAME_BYTES).map_err(Failure::from)
        },
        sqlite_failure: &sql_failure,
    };
    let documents = DocumentReads {
        local: &local,
        objects: &source,
        comments: &NoTree,
        cache: None,
    };
    let allocated = row.required("allocated").unwrap().as_array().unwrap();
    let next_id = Cell::new(0);
    let new_id = || {
        let n = next_id.get();
        next_id.set(n + 1);
        allocated[n].as_js_string().unwrap().clone()
    };
    let timestamp = data
        .required("timestamp")
        .unwrap()
        .as_js_string()
        .unwrap()
        .clone();
    let now = || timestamp.to_utf8().unwrap();
    let hydrate = |_: Value, _: Value| {
        std::future::ready(Err::<Value, _>(Failure::from(IvyError::internal())))
    };
    let comments = |_: Value, _: Value, _: Value, _: Value| {
        std::future::ready(Err::<Value, _>(Failure::from(IvyError::internal())))
    };
    let identity = |_: Value| async {
        panic!("local gate mutations do not allocate remote mutation identities");
        #[allow(unreachable_code)]
        Ok::<_, Failure<'_>>(Value::Null)
    };
    let writes = DocumentWrites {
        source: &source,
        hydrate: &hydrate,
        comments: &comments,
        identity: &identity,
        now: &now,
        new_id: &new_id,
    };
    let operations = Operations {
        store: MutationJournal {
            documents: &documents,
            writes: &writes,
        },
        now: &now,
    };
    let journal = &operations.store;
    let begun = begin_graph(
        &operations,
        &Value::from_json(row.required("request").unwrap()),
        &Value::from_json(row.required("actor").unwrap()),
    )
    .await
    .unwrap();
    let id = field(&field(&begun, "pin"), "objectId");
    let Value::String(id_text) = &id else {
        panic!("operation ID")
    };
    let retained = LocalGates::new(&connection);
    retained
        .retain_operation_gates(id_text, &NAMES.map(JsString::from))
        .unwrap();
    let principal = text("board");
    let calls = Calls {
        store: journal,
        principal: &principal,
        named: |name| async move { journal.named_graph(GATE, &name, None).await },
        gates: |operation: Value| {
            let retained = &retained;
            let sql_failure = &sql_failure;
            async move {
                let Value::String(id) = field(&field(&operation, "pin"), "objectId") else {
                    panic!("retained operation ID")
                };
                Ok::<_, Failure<'_>>(
                    retained
                        .operation_gates(&id)
                        .map_err(sql_failure)?
                        .into_iter()
                        .map(Value::String)
                        .collect(),
                )
            }
        },
        now: || Value::String(timestamp.clone()),
    };
    calls.acquire(&begun).await.unwrap();
    let original = IvyError::new("invalid_arguments", "Refused before publication.")
        .with_outcome(Outcome::NotExecuted)
        .with_details(Json::object([("where", "input".into())]));
    if name == "release_clear_sql_failure" {
        failure_graph(&operations, &id, &original, false)
            .await
            .unwrap();
    }
    connection
        .execute_batch(&data.required("trigger").unwrap().as_str().unwrap())
        .unwrap();
    let target: String = connection
        .query_row("SELECT id FROM documents WHERE name=?", [NAMES[1]], |row| {
            row.get(0)
        })
        .unwrap();
    connection
        .execute("INSERT INTO clear_refusal VALUES(?)", [target])
        .unwrap();
    let workspace = Cell::new(0);
    let result = if name == "unstarted_clear_sql_failure" {
        calls.release_unstarted(&id).await.map(|_| Value::Null)
    } else if name == "release_clear_sql_failure" {
        calls.release(&id, false, true).await.map(|()| Value::Null)
    } else {
        failed_publication_graph(&operations,&id,original.into(),|| {
            // The original conclusive failure is already durable before release
            // starts. A later native exception must not replace that saved cause.
            let saved:String=connection.query_row("SELECT value_json FROM revisions r JOIN documents d ON d.id=r.id AND d.current_revision=r.revision WHERE d.key='task-board/operation'",[],|row|row.get(0)).unwrap();
            let saved=ivy_contracts::json::parse(&saved,MANAGEMENT_FRAME_BYTES).unwrap();
            assert_eq!(saved.get("phase"),Some(&"failed".into()));
            assert_eq!(saved.required("error").unwrap().get("code"),Some(&"invalid_arguments".into()));
            workspace.set(workspace.get()+1);Ok(())
        },||calls.release(&id,false,true)).await
    };
    let error = result.err().expect("second gate clear must fail");
    assert!(typed_write_cause(&error).is_none());
    assert!(error.ptr_eq(last_sql.borrow().as_ref().unwrap()));
    let ReadCause::Source(cause) = error.cause() else {
        panic!("native cause was normalized")
    };
    assert!(std::ptr::eq(cause.borrowed, &marker));
    assert_eq!(Rc::strong_count(&cause.token), 1);
    assert_eq!(sql_count.get(), 1);
    let Some(ivy_store::rusqlite::Error::SqliteFailure(code, message)) = &cause.native else {
        panic!("original native trigger error")
    };
    assert_eq!(code.extended_code, 1811);
    assert_eq!(message.as_deref(), Some("gate clear refused"));
    let operation = journal
        .read_graph(OPERATION, &id, true, false, false)
        .await
        .unwrap();
    let mut statement=connection.prepare("SELECT d.id,d.name,d.current_revision,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='task-board/coordination' ORDER BY d.name").unwrap();
    let heads = Json::Array(
        statement
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
                let (id, name, revision, value) = row.unwrap();
                Json::object([
                    ("id", id.into()),
                    ("name", name.into()),
                    ("revision", Json::Number(revision as f64)),
                    (
                        "value",
                        ivy_contracts::json::parse(&value, MANAGEMENT_FRAME_BYTES).unwrap(),
                    ),
                ])
            })
            .collect(),
    );
    let owners = rows(
        &connection,
        "SELECT gate_id,operation_id FROM active_gates ORDER BY gate_id",
        &["gate_id", "operation_id"],
    );
    let cleanup = rows(
        &connection,
        "SELECT root_id,delete_after FROM cleanup_roots ORDER BY root_id",
        &["root_id", "delete_after"],
    );
    let actual_allocated = rows(
        &connection,
        "SELECT id FROM documents ORDER BY rowid",
        &["id"],
    )
    .as_array()
    .unwrap()
    .iter()
    .map(|row| row.required("id").unwrap().clone())
    .collect();
    let revision_counts = rows(
        &connection,
        "SELECT id,COUNT(*) AS count FROM revisions GROUP BY id ORDER BY id",
        &["id", "count"],
    );
    assert!(connection.is_autocommit());
    connection
        .execute_batch("BEGIN IMMEDIATE; ROLLBACK")
        .unwrap();
    let output = Json::object([
        ("name", name.into()),
        ("request", row.required("request").unwrap().clone()),
        ("actor", row.required("actor").unwrap().clone()),
        ("allocated", Json::Array(actual_allocated)),
        ("sqlFailure", true.into()),
        ("saved", json(&field(&operation, "value"))),
        ("heads", heads),
        ("owners", owners),
        ("cleanup", cleanup),
        ("workspace", Json::Number(workspace.get() as f64)),
        (
            "retainedNames",
            Json::Array(
                retained
                    .operation_gates(id_text)
                    .unwrap()
                    .into_iter()
                    .map(Json::String)
                    .collect(),
            ),
        ),
        ("revisionCounts", revision_counts),
        ("transactionReleased", true.into()),
    ]);
    assert_eq!(
        hash_json(&output).unwrap(),
        hash_json(row).unwrap(),
        "{name} result {output:?}"
    );
    assert_eq!(next_id.get(), 3);
    assert!(source.trace.borrow().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn real_gate_release_retains_first_clear_and_rolls_back_second_on_native_failure() {
    case("release_clear_sql_failure").await;
}

#[tokio::test(flavor = "current_thread")]
async fn real_unstarted_gate_release_preserves_opaque_failure_after_partial_clear() {
    case("unstarted_clear_sql_failure").await;
}

#[tokio::test(flavor = "current_thread")]
async fn real_final_catch_retains_saved_refusal_when_late_gate_clear_fails() {
    case("final_catch_release_sql_failure").await;
}
