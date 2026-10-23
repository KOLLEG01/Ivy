use super::*;
use crate::{
    journal_store::{
        mutation_ports::LocalMutationStore, persistent_reads::mutation_journal::MutationJournal,
    },
    local_gates::LocalGates,
    operations::{
        Operations,
        local::{begin_graph, failure_graph},
    },
    publication_coordination::{local::Calls, names::names_graph},
};
use ivy_contracts::{Outcome, hash::hash_json};

fn data() -> Json {
    ivy_contracts::json::parse(
        include_str!("task-board-gate-names-journal.json"),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap()
}
fn rows(connection: &Connection, sql: &str, columns: &[&str]) -> Json {
    let mut statement = connection.prepare(sql).unwrap();
    Json::Array(
        statement
            .query_map([], |row| {
                Ok(Json::object(columns.iter().enumerate().map(|(i, key)| {
                    let value = match row.get_ref(i).unwrap() {
                        ivy_store::rusqlite::types::ValueRef::Text(value) => {
                            Json::from(std::str::from_utf8(value).unwrap())
                        }
                        ivy_store::rusqlite::types::ValueRef::Integer(value) => {
                            Json::Number(value as f64)
                        }
                        _ => panic!("projection"),
                    };
                    (*key, value)
                })))
            })
            .unwrap()
            .map(|row| row.unwrap())
            .collect(),
    )
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
    let marker = 68;
    let source = Source::new("journal", &marker);
    let connection = Connection::open_in_memory().unwrap();
    initialize(&connection).unwrap();
    let timestamp = data
        .required("timestamp")
        .unwrap()
        .as_js_string()
        .unwrap()
        .clone();
    let at = timestamp.to_utf8().unwrap();
    connection
        .execute(
            "INSERT INTO documents VALUES('task','task-board/task','Task','root',1,?,?)",
            [&at, &at],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO revisions VALUES('task',1,?,?)",
            [
                ivy_contracts::json::stringify(
                    row.required("task").unwrap(),
                    MANAGEMENT_FRAME_BYTES,
                )
                .unwrap(),
                at.clone(),
            ],
        )
        .unwrap();
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
        parse: |raw: &str| {
            ivy_contracts::json::parse(raw, MANAGEMENT_FRAME_BYTES).map_err(Failure::from)
        },
        sqlite_failure: &sql_failure,
    };
    let reads = DocumentReads {
        local: &local,
        objects: &source,
        comments: &NoTree,
        cache: None,
    };
    let allocated = row.required("allocation").unwrap().as_array().unwrap();
    let next_id = Cell::new(1);
    let new_id = || {
        let n = next_id.get();
        next_id.set(n + 1);
        allocated[n].as_js_string().unwrap().clone()
    };
    let now = || at.clone();
    let hydrate = |_: Value, _: Value| {
        std::future::ready(Err::<Value, _>(Failure::from(IvyError::internal())))
    };
    let comments = |_: Value, _: Value, _: Value, _: Value| {
        std::future::ready(Err::<Value, _>(Failure::from(IvyError::internal())))
    };
    let identity = |_: Value| async {
        panic!("local gate writes do not allocate remote identity");
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
            documents: &reads,
            writes: &writes,
        },
        now: &now,
    };
    let journal = &operations.store;
    let operation = begin_graph(
        &operations,
        &Value::from_json(row.required("request").unwrap()),
        &Value::from_json(row.required("actor").unwrap()),
    )
    .await
    .unwrap();
    let id = field(&field(&operation, "pin"), "objectId");
    let retained = LocalGates::new(&connection);
    let principal = text("board");
    let calls = Calls {
        store: journal,
        principal: &principal,
        named: |name| async move {
            journal
                .named_graph("task-board/coordination", &name, None)
                .await
        },
        gates: |operation: Value| {
            let retained = &retained;
            let sql_failure = &sql_failure;
            async move {
                names_graph(
                    journal,
                    &operation,
                    |id| {
                        let Value::String(id) = id else {
                            panic!("admitted operation ID")
                        };
                        Ok(retained
                            .operation_gates(&id)
                            .map_err(sql_failure)?
                            .into_iter()
                            .map(Value::String)
                            .collect())
                    },
                    |id, names| {
                        let Value::String(id) = id else {
                            panic!("admitted operation ID")
                        };
                        let names = names
                            .into_iter()
                            .map(|name| {
                                let Value::String(name) = name else {
                                    panic!("gate name")
                                };
                                name
                            })
                            .collect::<Vec<_>>();
                        retained
                            .retain_operation_gates(&id, &names)
                            .map_err(sql_failure)
                    },
                )
                .await
            }
        },
        now: || Value::String(timestamp.clone()),
    };
    let sql_failure_case = name.ends_with("sql_failure");
    if sql_failure_case {
        connection
            .execute_batch(&data.required("trigger").unwrap().as_str().unwrap())
            .unwrap();
    }
    let result = calls.acquire(&operation).await;
    let outcome = if sql_failure_case {
        let error = result.err().expect("second retained name insert fails");
        assert!(error.ptr_eq(last_sql.borrow().as_ref().unwrap()));
        let ReadCause::Source(cause) = error.cause() else {
            panic!("native cause normalized")
        };
        assert!(std::ptr::eq(cause.borrowed, &marker));
        assert_eq!(Rc::strong_count(&cause.token), 1);
        assert_eq!(sql_count.get(), 1);
        let Some(ivy_store::rusqlite::Error::SqliteFailure(code, message)) = &cause.native else {
            panic!("native SQLite error")
        };
        assert_eq!(code.extended_code, 1811);
        assert_eq!(message.as_deref(), Some("gate retention refused"));
        Json::object([("sqlFailure", true.into())])
    } else {
        result.unwrap();
        let original = IvyError::new("invalid_arguments", "Refused before publication.")
            .with_outcome(Outcome::NotExecuted)
            .with_details(Json::object([("where", "input".into())]));
        failure_graph(&operations, &id, &original, false)
            .await
            .unwrap();
        calls.release(&id, false, true).await.unwrap();
        assert_eq!(sql_count.get(), 0);
        Json::object([("released", true.into())])
    };
    let Value::String(id_text) = &id else {
        panic!("operation ID")
    };
    let saved = journal
        .read_graph("task-board/operation", &id, true, false, false)
        .await
        .unwrap();
    let mut heads = rows(
        &connection,
        "SELECT d.id,d.name,d.current_revision AS revision,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='task-board/coordination' ORDER BY d.name",
        &["id", "name", "revision", "value_json"],
    );
    for head in match &mut heads {
        Json::Array(values) => values,
        _ => unreachable!(),
    } {
        let value = ivy_contracts::json::parse(
            &head.required("value_json").unwrap().as_str().unwrap(),
            MANAGEMENT_FRAME_BYTES,
        )
        .unwrap();
        *head = Json::object([
            ("id", head.required("id").unwrap().clone()),
            ("name", head.required("name").unwrap().clone()),
            ("revision", head.required("revision").unwrap().clone()),
            ("value", value),
        ]);
    }
    let allocation = rows(
        &connection,
        "SELECT id FROM documents ORDER BY rowid",
        &["id"],
    )
    .as_array()
    .unwrap()
    .iter()
    .map(|row| row.required("id").unwrap().clone())
    .collect();
    connection
        .execute_batch("BEGIN IMMEDIATE;ROLLBACK;")
        .unwrap();
    let mut actual = Json::object([
        ("saved", json(&field(&saved, "value"))),
        ("heads", heads),
        (
            "owners",
            rows(
                &connection,
                "SELECT gate_id,operation_id FROM active_gates ORDER BY gate_id",
                &["gate_id", "operation_id"],
            ),
        ),
        (
            "revisionCounts",
            rows(
                &connection,
                "SELECT id,COUNT(*) AS count FROM revisions GROUP BY id ORDER BY id",
                &["id", "count"],
            ),
        ),
        ("allocation", Json::Array(allocation)),
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
        ("transactionReleased", true.into()),
    ]);
    for (key, value) in outcome.as_object().unwrap() {
        actual.insert(key.clone(), value.clone());
    }
    let expected = Json::object(
        [
            "saved",
            "heads",
            "owners",
            "revisionCounts",
            "allocation",
            "retainedNames",
            "transactionReleased",
            "released",
            "sqlFailure",
        ]
        .into_iter()
        .filter_map(|key| row.get(key).map(|value| (key, value.clone()))),
    );
    assert_eq!(
        hash_json(&actual).unwrap(),
        hash_json(&expected).unwrap(),
        "{name} {actual:?}"
    );
    assert!(source.trace.borrow().is_empty());
    assert_eq!(next_id.get(), allocated.len());
}
#[tokio::test(flavor = "current_thread")]
async fn current_gate_names_derive_retain_acquire_and_release_through_real_journal() {
    case("current_native_gate_indexes").await;
}
#[tokio::test(flavor = "current_thread")]
async fn current_gate_names_sql_failure_rolls_back_retention_before_any_gate_is_acquired() {
    case("current_gate_retention_sql_failure").await;
}
