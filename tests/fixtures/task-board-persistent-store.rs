use futures_util::{
    FutureExt,
    future::{LocalBoxFuture, join_all},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    json::{canonical, parse, stringify},
    limits::MANAGEMENT_FRAME_BYTES,
    schema::{CompiledSchema, SchemaValidators},
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_sdk::task_comments::{CancellationToken, CommentReadFuture, CommentSource};
use ivy_store::rusqlite::{Connection, params};
use ivy_task_board::{
    coordination::next_steps::assess_steps,
    engine_read::{
        comment_reads::read_task,
        condition_policy::task_condition,
        conversation::ConversationPolicies,
        read_failure::{ReadCause, ReadFailure},
    },
    journal_store::{
        Contracts, JournalStore, ReadScopes,
        persistent_reads::{
            local_reads::SqliteReads,
            store::{ObjectReads, PersistentReadStore},
            task_comments::TaskComments,
        },
    },
    local_journal::{LocalJournal, initialize},
    local_retention::HistoryLimits,
    operations::Store,
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

fn f<'a>(v: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(key).unwrap_or(&NULL)
}
fn text(v: &Json) -> String {
    v.as_str().unwrap()
}
fn yes(v: &Json, key: &str) -> bool {
    v.get(key) == Some(&Json::Bool(true))
}
fn items(v: &Json) -> &[Json] {
    v.as_array().unwrap_or(&[])
}
fn same(a: &Json, b: &Json, name: &str) {
    assert_eq!(
        stringify(a, MANAGEMENT_FRAME_BYTES).unwrap(),
        stringify(b, MANAGEMENT_FRAME_BYTES).unwrap(),
        "{name}"
    );
}
fn ivy(v: &Json) -> IvyError {
    let mut error = IvyError::new_js(
        f(v, "code").as_js_string().unwrap().clone(),
        f(v, "message").as_js_string().unwrap().clone(),
    );
    error.outcome = match text(f(v, "outcome")).as_str() {
        "unknown" => Outcome::Unknown,
        "completed" => Outcome::Completed,
        _ => Outcome::NotExecuted,
    };
    error.details = v.get("details").cloned();
    error
}
// These source payloads intentionally have no Clone/Debug/Display/Error/Sync.
struct Opaque {
    value: Box<Json>,
    identity: Arc<()>,
    _not_sync: Cell<usize>,
}
enum WireFailure {
    Ivy(IvyError),
    Raw(Opaque),
}
enum NativeFailure {
    Raw(Opaque),
    Sqlite(Box<ivy_store::rusqlite::Error>),
    Parse(Box<IvyError>),
}
type Failure = ReadFailure<NativeFailure>;
struct Source {
    input: Json,
    trace: Mutex<Vec<Json>>,
    identity: Arc<()>,
    payload: AtomicUsize,
    cause: AtomicUsize,
    writes: AtomicUsize,
}
impl Source {
    async fn rpc(&self, method: &str, params: Option<Json>) -> Result<Json, WireFailure> {
        let mut row = Json::object([("method", method.into())]);
        if let Some(params) = &params {
            row.insert("params", params.clone());
        }
        self.trace.lock().unwrap().push(row);
        if method == "objects.write" {
            let index = self.writes.fetch_add(1, Ordering::SeqCst);
            let result = &items(f(&self.input, "writeResults"))[index];
            if let Some(error) = result.get("error") {
                return Err(WireFailure::Ivy(ivy(error)));
            }
            return Ok(result.clone());
        }
        assert_eq!(method, "objects.read");
        if yes(&self.input, "yieldReads") {
            tokio::task::yield_now().await;
        }
        let id = params.as_ref().and_then(|p| p.get("objectId"));
        if let Some(raw) = self.input.get("rawSource") {
            if id == raw.get("id") {
                let value = Box::new(f(raw, "value").clone());
                self.payload
                    .store(&*value as *const _ as usize, Ordering::SeqCst);
                return Err(WireFailure::Raw(Opaque {
                    value,
                    identity: self.identity.clone(),
                    _not_sync: Cell::new(0),
                }));
            }
        }
        if let Some(id) = id.and_then(Json::as_js_string) {
            if let Some(error) = f(&self.input, "sourceErrors").get_key(id) {
                return Err(WireFailure::Ivy(ivy(error)));
            }
        }
        if params.as_ref().is_none_or(Json::is_null) {
            if let Some(record) = self.input.get("optionalRecord") {
                return Ok(record.clone());
            }
        }
        if let Some(id) = id.and_then(Json::as_js_string) {
            if let Some(record) = f(&self.input, "records").get_key(id) {
                return Ok(record.clone());
            }
        }
        assert!(
            yes(&self.input, "autoRecords"),
            "Unexpected objects.read: {params:?}"
        );
        let id = id.unwrap().clone();
        let revision = f(params.as_ref().unwrap(), "revision").clone();
        Ok(Json::object([
            (
                "object",
                Json::object([
                    ("id", id),
                    ("contractKey", "task-board/review".into()),
                    ("parentId", Json::Null),
                    ("effectivelyArchived", false.into()),
                    ("name", "Source".into()),
                    ("extra", Json::String(JsString(vec![0xd800]))),
                ]),
            ),
            (
                "revision",
                Json::object([
                    ("revision", revision),
                    ("contractVersion", "1.0.0".into()),
                    ("contentHash", "sha256:retained".into()),
                    ("mediaType", "application/json".into()),
                    ("byteLength", Json::Number(123.0)),
                    (
                        "references",
                        Json::object([(
                            "keep",
                            Json::object([
                                ("objectId", "evidence".into()),
                                ("revision", Json::Number(3.0)),
                            ]),
                        )]),
                    ),
                ]),
            ),
            (
                "content",
                Json::object([
                    ("encoding", "json".into()),
                    (
                        "value",
                        Json::object([
                            ("schemaVersion", Json::Number(1.0)),
                            ("keep", Json::String(JsString(vec![0xdfff]))),
                        ]),
                    ),
                ]),
            ),
        ]))
    }
    fn bind(&self, error: WireFailure) -> Failure {
        let error = match error {
            WireFailure::Ivy(error) => Failure::from(error),
            WireFailure::Raw(error) => Failure::source(NativeFailure::Raw(error)),
        };
        self.cause
            .store(error.cause() as *const _ as usize, Ordering::SeqCst);
        error
    }
}
impl ObjectReads<Failure> for Source {
    async fn read(&self, params: Option<Json>) -> Result<Json, Failure> {
        self.rpc("objects.read", params)
            .await
            .map_err(|error| self.bind(error))
    }
}
impl RpcClient for Source {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            assert!(!options.cancellation.is_cancelled() && options.bound_read.is_none());
            self.rpc(method, Some(params))
                .await
                .map_err(|error| match error {
                    WireFailure::Ivy(error) => error,
                    WireFailure::Raw(_) => {
                        panic!("Only typed prepared-write failures are exercised")
                    }
                })
        })
    }
}
impl CommentSource for Source {
    type Error = WireFailure;
    fn read<'a>(
        &'a self,
        pin: Json,
        cancellation: Option<CancellationToken>,
    ) -> CommentReadFuture<'a, Self::Error> {
        Box::pin(async move {
            assert!(cancellation.as_ref().is_none_or(|c| !c.is_cancelled()));
            self.rpc("objects.read", Some(pin)).await
        })
    }
}
struct Catalog(Json);
impl Contracts for Catalog {
    fn version(&self, key: &str) -> IvyResult<String> {
        Ok(text(f(f(&self.0, key), "version")))
    }
    fn local(&self, key: &str) -> IvyResult<bool> {
        Ok(yes(f(&self.0, key), "local"))
    }
    fn readable_version(&self, key: &str, version: &JsString) -> IvyResult<bool> {
        Ok(items(f(f(&self.0, key), "readable")).contains(&Json::String(version.clone())))
    }
    fn validate_record(&self, key: &str, version: &JsString, value: &Json) -> IvyResult<()> {
        // Writes use the actual current definition. Reads never call this guard.
        assert_eq!(key, "task-board/review");
        let definitions = ivy_task_board::schema::task_board_contracts()?;
        let definition = definitions
            .iter()
            .find(|d| {
                f(d, "key") == &Json::from(key) && f(d, "version").as_js_string() == Some(version)
            })
            .unwrap();
        canonical(value, MANAGEMENT_FRAME_BYTES)?;
        assert!(
            SchemaValidators::new()
                .compile(f(definition, "jsonSchema"))?
                .matches(value)?
        );
        Ok(())
    }
    fn validate_definition(&self, _: &str, _: &Json) -> IvyResult<()> {
        panic!("No default validation is supplied")
    }
}
fn observe(result: Result<Json, Failure>, source: &Source, sql_cause: &Cell<usize>) -> Json {
    match result {
        Ok(value) => Json::object([("value", value)]),
        Err(error) => match error.cause() {
            ReadCause::Ivy(error) => Json::object([("ivy", error.to_wire())]),
            ReadCause::JavaScript(error) => Json::object([(
                "javascript",
                Json::object([
                    ("kind", error.kind.into()),
                    ("message", Json::String(error.message.clone())),
                ]),
            )]),
            ReadCause::Source(NativeFailure::Raw(raw)) => {
                assert_eq!(
                    error.cause() as *const _ as usize,
                    source.cause.load(Ordering::SeqCst)
                );
                assert_eq!(
                    &*raw.value as *const _ as usize,
                    source.payload.load(Ordering::SeqCst)
                );
                assert!(Arc::ptr_eq(&raw.identity, &source.identity));
                Json::object([("source", (*raw.value).clone()), ("identity", true.into())])
            }
            ReadCause::Source(NativeFailure::Sqlite(error)) => {
                assert_eq!(&**error as *const _ as usize, sql_cause.get());
                Json::object([(
                    "javascript",
                    Json::object([
                        ("kind", "Error".into()),
                        ("message", error.to_string().into()),
                    ]),
                )])
            }
            ReadCause::Source(NativeFailure::Parse(error)) => {
                assert_eq!(&**error as *const _ as usize, sql_cause.get());
                Json::object([("parserRejected", true.into()), ("identity", true.into())])
            }
        },
    }
}
type ReadOne<'a> = dyn Fn(Json) -> LocalBoxFuture<'a, Result<Json, Failure>> + 'a;
fn steps<'a>(
    items: Vec<Json>,
    scopes: &'a ReadScopes,
    read: &'a ReadOne<'a>,
    source: &'a Source,
    sql: &'a Cell<usize>,
) -> LocalBoxFuture<'a, Json> {
    async move {
        let mut rows = Vec::new();
        for item in items {
            let kind = text(f(&item, "kind"));
            let result = if kind == "action" || kind == "nested" {
                Ok(scopes
                    .with_read_cache(steps(items_from(&item), scopes, read, source, sql))
                    .await)
            } else if kind == "parallel" {
                let values = join_all(items_from(&item).into_iter().map(|item| async move {
                    let result = steps(vec![item], scopes, read, source, sql).await;
                    result.as_array().unwrap()[0].clone()
                }))
                .await;
                Ok(Json::Array(values))
            } else {
                read(item.clone()).await
            };
            let mut result = observe(result, source, sql);
            rows.push(result.clone());
            if yes(&item, "mutate") {
                let mut value = f(&result, "value").clone();
                value.insert("value", Json::object([("changed", true.into())]));
                let mut metadata = f(&value, "metadata").clone();
                metadata.insert("extra", "changed".into());
                value.insert("metadata", metadata);
                let mut revision = f(&value, "revision").clone();
                revision.insert("contentHash", "changed".into());
                value.insert("revision", revision);
                result.insert("value", value);
            }
        }
        Json::Array(rows)
    }
    .boxed_local()
}
fn items_from(value: &Json) -> Vec<Json> {
    items(f(value, "steps")).to_vec()
}
fn seed(connection: &Connection, input: &Json) {
    initialize(connection).unwrap();
    for row in items(f(input, "rows")) {
        let (id, key) = (text(f(row, "id")), text(f(row, "key")));
        let parent = f(row, "parentId")
            .as_js_string()
            .map(|s| String::from_utf16_lossy(&s.0));
        connection
            .execute(
                "INSERT INTO documents VALUES(?,?,?,?,?,?,?)",
                params![
                    id,
                    key,
                    text(f(row, "name")),
                    parent,
                    f(row, "revision").as_f64().unwrap(),
                    text(f(row, "createdAt")),
                    text(f(row, "updatedAt"))
                ],
            )
            .unwrap();
        let encoded = row
            .get("raw")
            .map(text)
            .unwrap_or_else(|| stringify(f(row, "value"), MANAGEMENT_FRAME_BYTES).unwrap());
        connection
            .execute(
                "INSERT INTO revisions VALUES(?,?,?,?)",
                params![
                    id,
                    f(row, "revision").as_f64().unwrap(),
                    encoded,
                    text(f(row, "createdAt"))
                ],
            )
            .unwrap();
        for old in items(f(row, "older")) {
            connection
                .execute(
                    "INSERT INTO revisions VALUES(?,?,?,?)",
                    params![
                        id,
                        f(old, "revision").as_f64().unwrap(),
                        text(f(old, "raw")),
                        text(f(old, "createdAt"))
                    ],
                )
                .unwrap();
        }
    }
    for row in items(f(input, "blobs")) {
        let id = text(f(row, "id"));
        connection
            .execute(
                "INSERT INTO blobs VALUES(?,?,?,NULL,?)",
                params![id, text(f(row, "key")), id, Vec::<u8>::new()],
            )
            .unwrap();
    }
    for sql in items(f(input, "sql")) {
        connection.execute_batch(&text(sql)).unwrap();
    }
    if yes(input, "missingTables") {
        connection
            .execute_batch("DROP TABLE revisions;DROP TABLE documents;")
            .unwrap();
    }
}
fn validator() -> Arc<CompiledSchema> {
    let definitions = ivy_task_board::schema::task_board_contracts().unwrap();
    let task = definitions
        .iter()
        .find(|d| {
            f(d, "key") == &Json::from("task-board/task") && f(d, "version") == &Json::from("2.3.0")
        })
        .unwrap();
    SchemaValidators::new()
        .compile(&Json::object([
            ("$ref", "#/$defs/TaskBoardNextStep".into()),
            ("$defs", f(f(task, "jsonSchema"), "$defs").clone()),
        ]))
        .unwrap()
}
#[test]
fn actual_persistent_store_matches_original_materialization_scope_and_cache() {
    run_fixture(
        "IVY_TASK_BOARD_PERSISTENT_STORE_FIXTURE",
        "IVY_TASK_BOARD_PERSISTENT_STORE_NATIVE",
        63,
    );
}
#[test]
fn actual_local_reads_prefix_and_write_cache_match_original() {
    run_fixture(
        "IVY_TASK_BOARD_READ_BOUNDARY_FIXTURE",
        "IVY_TASK_BOARD_READ_BOUNDARY_NATIVE",
        41,
    );
}
fn run_fixture(fixture_env: &str, native_env: &str, expected_count: usize) {
    let fixture = parse(
        &std::fs::read_to_string(std::env::var(fixture_env).unwrap()).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    let validator = validator();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut observations = Vec::new();
        for case in items(f(&fixture, "cases")) {
            let input = f(case, "input");
            let before = input.clone();
            let connection = Connection::open_in_memory().unwrap();
            seed(&connection, input);
            let source = Arc::new(Source {
                input: input.clone(),
                trace: Mutex::default(),
                identity: Arc::new(()),
                payload: AtomicUsize::new(0),
                cause: AtomicUsize::new(0),
                writes: AtomicUsize::new(0),
            });
            let catalog = Catalog(f(&fixture, "contracts").clone());
            let sql_cause = Cell::new(0);
            let parser_observations = RefCell::new(Vec::new());
            let parse_record = |text: &str| {
                parse(text, usize::MAX).map_err(|error| {
                    let error = Box::new(error);
                    sql_cause.set(&*error as *const _ as usize);
                    parser_observations.borrow_mut().push(Json::object([
                        ("text", text.into()),
                        ("nativeError", error.to_wire()),
                    ]));
                    Failure::source(NativeFailure::Parse(error))
                })
            };
            let sqlite_failure = |error| {
                let error = Box::new(error);
                sql_cause.set(&*error as *const _ as usize);
                Failure::source(NativeFailure::Sqlite(error))
            };
            let local = SqliteReads {
                journal: LocalJournal {
                    connection: &connection,
                    root_object_id: f(input, "root").as_js_string().cloned(),
                },
                contracts: &catalog,
                parse: parse_record,
                sqlite_failure,
            };
            let validate = |step: &Json| -> Result<(), Failure> {
                canonical(step, MANAGEMENT_FRAME_BYTES)?;
                if validator.matches(step)? {
                    Ok(())
                } else {
                    Err(IvyError::new(
                        "invalid_arguments",
                        "Value does not match its TaskBoard workflow contract.",
                    )
                    .into())
                }
            };
            let now = || f(&fixture, "at").as_js_string().unwrap().clone();
            let condition = |task: &Json, steps: Option<&Json>| task_condition(&local, task, steps);
            let policies = ConversationPolicies::current_raw(&validate, &now, &condition);
            let comments = TaskComments::new(source.clone(), runtime.handle().clone());
            let bind = |error| source.bind(error);
            let scopes = ReadScopes::default();
            let store = PersistentReadStore {
                local: &local,
                objects: &*source,
                policies: &policies,
                comments: &comments,
                source_failure: &bind,
                cache: Some(scopes.cache()),
            };
            let writer = JournalStore {
                journal: LocalJournal {
                    connection: &connection,
                    root_object_id: f(input, "root").as_js_string().cloned(),
                },
                client: source.clone(),
                contracts: &catalog,
                limits: HistoryLimits::default(),
                now: || text(f(&fixture, "at")),
                new_id: || panic!("Remote prepared writes must not allocate a local identity"),
                cache: Some(scopes.cache()),
            };
            let read_one = |item: Json| {
                let store = &store;
                let writer = &writer;
                let policies = &policies;
                let at = f(&fixture, "at").as_js_string().unwrap();
                async move {
                    match text(f(&item, "kind")).as_str() {
                        "write" => writer
                            .write_prepared(
                                &text(f(&item, "key")),
                                f(&item, "value"),
                                f(&item, "mutation").as_js_string().unwrap(),
                                f(&item, "destination"),
                            )
                            .await
                            .map_err(Failure::from),
                        "condition" => task_condition(store, f(&item, "task"), None),
                        "assess" => assess_steps(
                            store,
                            policies,
                            f(&item, "task"),
                            at,
                            &HashMap::new(),
                            None,
                        )
                        .await
                        .map(|r| Json::Array(r.iter().map(|v| v.as_json()).collect())),
                        "view" => {
                            let header = store
                                .read_document(
                                    "task-board/task",
                                    f(&item, "address"),
                                    true,
                                    true,
                                    true,
                                )
                                .await?;
                            read_task(store, policies, &header, &Json::Object(vec![])).await
                        }
                        _ => {
                            store
                                .read_document(
                                    &text(f(&item, "key")),
                                    f(&item, "address"),
                                    yes(&item, "primary"),
                                    yes(&item, "includeArchived"),
                                    yes(&item, "retained"),
                                )
                                .await
                        }
                    }
                }
                .boxed_local()
            };
            let work = steps(items_from(input), &scopes, &read_one, &source, &sql_cause);
            let result = if f(input, "scope") == &Json::from("action") {
                scopes.with_read_cache(work).await
            } else {
                work.await
            };
            let name = text(f(case, "name"));
            same(&result, f(case, "expected"), &name);
            let trace = Json::Array(source.trace.lock().unwrap().clone());
            same(&trace, f(case, "trace"), &name);
            same(input, &before, "Source input retained");
            let parser_observations = parser_observations.into_inner();
            let original_inputs = items(f(case, "parseErrors"))
                .iter()
                .map(|e| f(e, "text").clone())
                .collect();
            let native_inputs = parser_observations
                .iter()
                .map(|e| f(e, "text").clone())
                .collect();
            same(
                &Json::Array(original_inputs),
                &Json::Array(native_inputs),
                "Original parser inputs retained",
            );
            observations.push(Json::object([
                ("name", name.into()),
                ("result", result),
                ("trace", trace),
                ("parserDiagnostics", Json::Array(parser_observations)),
            ]));
        }
        assert_eq!(observations.len(), expected_count);
        if let Ok(path) = std::env::var(native_env) {
            std::fs::write(
                path,
                stringify(&Json::Array(observations), MANAGEMENT_FRAME_BYTES).unwrap(),
            )
            .unwrap();
        }
    });
}
