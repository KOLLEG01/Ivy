use super::*;
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
fn outcome(result: IvyResult<Json>) -> Json {
    match result {
        Ok(value) => Json::object([("ok", value)]),
        Err(error) => Json::object([(
            "error",
            Json::object([
                ("code", Json::String(error.code_js())),
                ("message", Json::String(error.message_js())),
                ("outcome", error.outcome.to_json()),
            ]),
        )]),
    }
}
async fn verify(filtering: bool) {
    let input = fixture();
    let db = Connection::open_in_memory().unwrap();
    let (store, client) = setup(&db, &input);
    let changes = db.total_changes();
    let mut count = 0;
    for case in input.required("cases").unwrap().as_array().unwrap() {
        if (text(case, "group") == "filter") != filtering {
            continue;
        }
        count += 1;
        let actual = outcome(
            store
                .page(&text(&input, "key"), case.required("options").unwrap(), &[])
                .await,
        );
        assert_eq!(
            hash_json(&actual).unwrap(),
            text(case, "expectedHash"),
            "{}",
            text(case, "name")
        );
        assert!(
            client.trace.lock().unwrap().is_empty(),
            "Local queries must not use Hive"
        );
        assert_eq!(
            db.total_changes(),
            changes,
            "Queries must not write the journal"
        );
        assert!(
            db.is_autocommit(),
            "No transaction may escape the synchronous query"
        );
    }
    assert_eq!(count, if filtering { 20 } else { 15 });
}
#[tokio::test(flavor = "current_thread")]
async fn current_local_pages_filter_current_heads_and_keep_internal_scope() {
    verify(true).await;
}
#[tokio::test(flavor = "current_thread")]
async fn current_local_pages_order_scalar_fields_and_resume_generated_offsets() {
    verify(false).await;
}
#[tokio::test(flavor = "current_thread")]
async fn local_sqlite_failure_never_falls_back_to_hive_or_writes_replacement_state() {
    let db = Connection::open_in_memory().unwrap();
    let input = fixture();
    let (store, client) = setup(&db, &input);
    db.execute_batch("DROP TABLE documents").unwrap();
    let changes = db.total_changes();
    let failure = store
        .page(&text(&input, "key"), &Json::Object(vec![]), &[])
        .await
        .unwrap_err();
    assert_eq!(failure.code, "storage_unavailable");
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
