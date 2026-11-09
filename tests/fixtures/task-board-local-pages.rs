use super::*;
use ivy_store::rusqlite::hooks::{AuthAction, AuthContext, Authorization};

fn fixture() -> Json {
    parse(
        include_str!("task-board-local-pages.json"),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap()
}
fn no_clock() -> String {
    panic!("Queries do not read the clock")
}
fn no_identity() -> JsString {
    panic!("Queries do not allocate identities")
}
type QueryStore<'a> = JournalStore<'a, Client, FixtureContracts, fn() -> String, fn() -> JsString>;
fn setup<'a>(db: &'a Connection, input: &Json) -> (QueryStore<'a>, Arc<Client>) {
    local_journal::initialize(db).unwrap();
    let journal = LocalJournal {
        connection: db,
        root_object_id: Some("root".into()),
    };
    for row in input.required("documents").unwrap().as_array().unwrap() {
        let id = string(row, "id");
        let parent = row.required("parent").unwrap().as_js_string();
        let value = row.required("value").unwrap();
        journal
            .create(
                &text(row, "key"),
                &id,
                parent,
                row.get("initialValue").unwrap_or(value),
                &TIME.into(),
                &id,
            )
            .unwrap();
        if row.get("initialValue").is_some() {
            journal
                .update(&text(row, "key"), &id, 1.0, value, &TIME.into())
                .unwrap();
        }
    }
    let client = Arc::new(Client {
        input: Json::Null,
        trace: Mutex::default(),
    });
    (
        JournalStore {
            journal,
            client: client.clone(),
            contracts: FixtureContracts {
                local: [text(input, "key")].into(),
                allowed: HashSet::new(),
            },
            limits: HistoryLimits::default(),
            now: no_clock,
            new_id: no_identity,
            cache: None,
        },
        client,
    )
}
fn ids(page: &Json) -> Vec<String> {
    page.required("items")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|row| text(row, "objectId"))
        .collect()
}
fn index(db: &Connection) -> Vec<(String, i64)> {
    db.prepare("SELECT id,revision FROM pending_operations ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}
fn phase(name: &str) -> Json {
    Json::object([("phase", name.into())])
}

#[tokio::test(flavor = "current_thread")]
async fn pending_operations_read_only_current_index_without_bodies_or_hive() {
    let input = fixture();
    let db = Connection::open_in_memory().unwrap();
    let (store, client) = setup(&db, &input);
    let changes = db.total_changes();
    db.authorizer(Some(|context: AuthContext<'_>| match context.action {
        AuthAction::Read {
            table_name: "documents" | "revisions",
            ..
        } => Authorization::Deny,
        _ => Authorization::Allow,
    }))
    .unwrap();
    let mut options = input.required("options").unwrap().clone();
    options.insert("contractVersions", Json::Array(vec![]));
    options.insert("select", Json::Array(vec!["data:/message".into()]));
    let page = store
        .page("task-board/operation", &options, &[])
        .await
        .unwrap();
    assert_eq!(ids(&page), ["id-a", "id-c", "id-e"]);
    let rows = page.required("items").unwrap().as_array().unwrap();
    assert_eq!(rows[0].required("revision").unwrap(), &Json::Number(2.0));
    assert!(
        rows.iter()
            .all(|row| row.get("values") == Some(&Json::Object(vec![])))
    );
    assert!(page.required("nextCursor").unwrap().is_null());
    assert!(client.trace.lock().unwrap().is_empty());
    assert_eq!(db.total_changes(), changes);
    assert!(db.is_autocommit());
}

#[tokio::test(flavor = "current_thread")]
async fn operation_keysets_survive_completion_deletion_and_new_heads_between_pages() {
    let input = fixture();
    let db = Connection::open_in_memory().unwrap();
    let (store, client) = setup(&db, &input);
    let mut options = input.required("options").unwrap().clone();
    options.insert("limit", Json::Number(1.0));
    let first = store
        .page("task-board/operation", &options, &[])
        .await
        .unwrap();
    assert_eq!(ids(&first), ["id-a"]);
    options.insert("cursor", first.required("nextCursor").unwrap().clone());
    store
        .journal
        .update(
            "task-board/operation",
            &"id-a".into(),
            2.0,
            &phase("succeeded"),
            &TIME.into(),
        )
        .unwrap();
    db.execute("DELETE FROM documents WHERE id='id-c'", [])
        .unwrap();
    store
        .journal
        .update(
            "task-board/operation",
            &"id-b".into(),
            1.0,
            &phase("needs_attention"),
            &TIME.into(),
        )
        .unwrap();
    store
        .journal
        .create(
            "task-board/operation",
            &"new".into(),
            None,
            &phase("preparing"),
            &TIME.into(),
            &"id-0".into(),
        )
        .unwrap();
    let second = store
        .page("task-board/operation", &options, &[])
        .await
        .unwrap();
    assert_eq!(ids(&second), ["id-b"]);
    options.insert("cursor", second.required("nextCursor").unwrap().clone());
    db.execute("DELETE FROM revisions WHERE id='id-b' AND revision=1", [])
        .unwrap();
    db.execute("DELETE FROM documents WHERE id='id-b'", [])
        .unwrap();
    let last = store
        .page("task-board/operation", &options, &[])
        .await
        .unwrap();
    assert_eq!(ids(&last), ["id-e"]);
    assert!(last.required("nextCursor").unwrap().is_null());
    options.remove("cursor");
    let next_scan = store
        .page("task-board/operation", &options, &[])
        .await
        .unwrap();
    assert_eq!(ids(&next_scan), ["id-0"]);
    assert!(client.trace.lock().unwrap().is_empty());
    assert!(db.is_autocommit());
}

#[test]
fn operation_index_tracks_both_write_orders_cas_rollback_and_retention() {
    let input = fixture();
    let db = Connection::open_in_memory().unwrap();
    let (store, _) = setup(&db, &input);
    let before = index(&db);
    assert_eq!(
        store
            .journal
            .update(
                "task-board/operation",
                &"id-c".into(),
                0.0,
                &phase("succeeded"),
                &TIME.into()
            )
            .unwrap_err()
            .code,
        "revision_conflict"
    );
    assert_eq!(index(&db), before);
    db.execute_batch(
        "BEGIN IMMEDIATE;
        UPDATE documents SET current_revision=2 WHERE id='id-c';
        INSERT INTO revisions VALUES('id-c',2,'{\"phase\":\"succeeded\"}','now');
    ",
    )
    .unwrap();
    assert!(!index(&db).iter().any(|(id, _)| id == "id-c"));
    db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(index(&db), before);
    db.execute_batch(
        "BEGIN IMMEDIATE;
        INSERT INTO revisions VALUES('id-c',2,'{\"phase\":\"publishing\"}','now');
        UPDATE documents SET current_revision=2 WHERE id='id-c';
        INSERT INTO revisions VALUES('id-c',3,'{\"phase\":\"failed\"}','now');
        DELETE FROM revisions WHERE id='id-c' AND revision=1;
        COMMIT;",
    )
    .unwrap();
    assert!(index(&db).contains(&("id-c".into(), 2)));
    db.execute_batch(
        "BEGIN IMMEDIATE;
        UPDATE documents SET current_revision=3 WHERE id='id-c';
        COMMIT;",
    )
    .unwrap();
    assert!(!index(&db).iter().any(|(id, _)| id == "id-c"));
    db.execute_batch("BEGIN IMMEDIATE;
        INSERT INTO revisions VALUES('revision-first',1,'{\"phase\":\"accepted\"}','now');
        INSERT INTO documents VALUES('revision-first','task-board/operation','new',NULL,1,'now','now');
        COMMIT;").unwrap();
    assert!(index(&db).contains(&("revision-first".into(), 1)));
    db.execute("DELETE FROM revisions WHERE id='revision-first'", [])
        .unwrap();
    assert!(!index(&db).iter().any(|(id, _)| id == "revision-first"));
    assert!(db.is_autocommit());
}

#[test]
fn operation_index_backfill_preserves_original_bytes_and_reopens_idempotently() {
    let input = fixture();
    let db = Connection::open_in_memory().unwrap();
    let (_store, _) = setup(&db, &input);
    let original: Vec<(String, i64, String)> = db
        .prepare("SELECT id,revision,value_json FROM revisions ORDER BY id,revision")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    db.execute_batch(
        "DROP TRIGGER pending_operation_revision_insert;
        DROP TRIGGER pending_operation_document_insert;
        DROP TRIGGER pending_operation_document_update;
        DROP TRIGGER pending_operation_document_delete;
        DROP TRIGGER pending_operation_revision_delete;
        DROP TABLE pending_operations;
        DELETE FROM markers WHERE key='pending-operations-index';",
    )
    .unwrap();
    local_journal::initialize(&db).unwrap();
    assert_eq!(
        index(&db),
        [("id-a".into(), 2), ("id-c".into(), 1), ("id-e".into(), 1)]
    );
    let after: Vec<(String, i64, String)> = db
        .prepare("SELECT id,revision,value_json FROM revisions ORDER BY id,revision")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(after, original);
    local_journal::initialize(&db).unwrap();
    assert_eq!(
        index(&db),
        [("id-a".into(), 2), ("id-c".into(), 1), ("id-e".into(), 1)]
    );
    assert!(db.is_autocommit());
}

#[tokio::test(flavor = "current_thread")]
async fn unsupported_local_filters_and_stale_or_malformed_cursors_never_fall_back() {
    let input = fixture();
    let db = Connection::open_in_memory().unwrap();
    let (store, client) = setup(&db, &input);
    let changes = db.total_changes();
    for cursor in [
        "Mg",
        "LTE",
        "bm9ubnVtZXJpYw",
        "cGVuZGluZy1vcGVyYXRpb25zOg",
        "!",
    ] {
        let mut options = input.required("options").unwrap().clone();
        options.insert("cursor", cursor.into());
        assert_eq!(
            store
                .page("task-board/operation", &options, &[])
                .await
                .unwrap_err()
                .code,
            "invalid_cursor"
        );
    }
    for limit in [
        Json::Number(0.0),
        Json::Number(-1.0),
        Json::Number(1.5),
        "1".into(),
        Json::Number(f64::INFINITY),
    ] {
        let mut options = input.required("options").unwrap().clone();
        options.insert("limit", limit);
        assert_eq!(
            store
                .page("task-board/operation", &options, &[])
                .await
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
    }
    for options in [
        Json::Object(vec![]),
        Json::object([("where", phase("accepted"))]),
    ] {
        assert_eq!(
            store
                .page("task-board/operation", &options, &[])
                .await
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
    }
    assert!(client.trace.lock().unwrap().is_empty());
    assert_eq!(db.total_changes(), changes);
    assert!(db.is_autocommit());
}

#[tokio::test(flavor = "current_thread")]
async fn local_sqlite_failure_never_falls_back_to_hive_or_writes_replacement_state() {
    let db = Connection::open_in_memory().unwrap();
    let input = fixture();
    let (store, client) = setup(&db, &input);
    db.execute_batch("DROP TABLE pending_operations").unwrap();
    let changes = db.total_changes();
    assert_eq!(
        store
            .page(
                "task-board/operation",
                input.required("options").unwrap(),
                &[]
            )
            .await
            .unwrap_err()
            .code,
        "storage_unavailable"
    );
    assert!(client.trace.lock().unwrap().is_empty());
    assert_eq!(db.total_changes(), changes);
    assert!(db.is_autocommit());
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM revisions", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        6
    );
}
