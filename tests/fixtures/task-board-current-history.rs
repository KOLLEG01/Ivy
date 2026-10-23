use super::*;
use ivy_store::rusqlite::types::ValueRef;
use ivy_task_board::journal_store::persistent_reads::{
    local_history::{HistoryCollections, HistoryDocuments, HistoryQueries, LocalHistory},
    operation_receipt::operation_receipt,
};
use std::rc::Rc;

// No Send/Sync/Clone/Display/Error requirement on an owner's raw failure.
enum Cause {
    Raw { value: Json, identity: Rc<()> },
    Sqlite(Box<ivy_store::rusqlite::Error>),
    Parser(Box<IvyError>),
}
type Error = ReadFailure<Cause>;
fn observed(
    result: Result<Json, Error>,
    source: &HistorySource<'_>,
    native_identity: &Cell<usize>,
) -> Json {
    match result {
        Ok(value) => Json::object([("value", value)]),
        Err(error) => match error.cause() {
            ReadCause::Ivy(error) => Json::object([("ivy", error.to_wire())]),
            ReadCause::Source(Cause::Raw { value, identity }) => {
                assert!(Rc::ptr_eq(identity, &source.identity));
                assert_eq!(error.cause() as *const _ as usize, source.raw_cause.get());
                Json::object([("source", value.clone()), ("identity", true.into())])
            }
            ReadCause::Source(Cause::Sqlite(error)) => {
                assert_eq!(&**error as *const _ as usize, native_identity.get());
                Json::object([("sqliteRejected", true.into())])
            }
            ReadCause::Source(Cause::Parser(error)) => {
                assert_eq!(&**error as *const _ as usize, native_identity.get());
                Json::object([("parserRejected", true.into())])
            }
        },
    }
}
fn exact(a: &Json, b: &Json, name: &str) {
    assert_eq!(
        canonical(a, usize::MAX).unwrap(),
        canonical(b, usize::MAX).unwrap(),
        "{name}"
    );
}
fn seed_rows(connection: &Connection, input: &Json) {
    for row in items(f(input, "rows")) {
        let id = text(f(row, "id"));
        connection
            .execute(
                "INSERT INTO documents VALUES(?,?,?,?,?,?,?)",
                params![
                    id,
                    text(f(row, "key")),
                    text(f(row, "name")),
                    f(row, "parentId")
                        .as_js_string()
                        .map(|s| String::from_utf16_lossy(&s.0)),
                    f(row, "revision").as_f64().unwrap(),
                    text(f(row, "createdAt")),
                    text(f(row, "updatedAt"))
                ],
            )
            .unwrap();
        let raw = row
            .get("raw")
            .map(text)
            .unwrap_or_else(|| canonical(f(row, "value"), usize::MAX).unwrap());
        connection
            .execute(
                "INSERT INTO revisions VALUES(?,?,?,?)",
                params![
                    id,
                    f(row, "revision").as_f64().unwrap(),
                    raw,
                    text(f(row, "createdAt"))
                ],
            )
            .unwrap();
        for old in items(f(row, "older")) {
            let raw = old
                .get("raw")
                .map(text)
                .unwrap_or_else(|| canonical(f(old, "value"), usize::MAX).unwrap());
            connection
                .execute(
                    "INSERT INTO revisions VALUES(?,?,?,?)",
                    params![
                        id,
                        f(old, "revision").as_f64().unwrap(),
                        raw,
                        text(f(row, "createdAt"))
                    ],
                )
                .unwrap();
        }
    }
    for sql in items(f(input, "sql")) {
        connection.execute_batch(&text(sql)).unwrap();
    }
}
fn snapshot(connection: &Connection, tables: &Json) -> Json {
    Json::object(items(tables).iter().map(|name| {
        let name = text(name);
        let mut statement = connection
            .prepare(&format!("SELECT * FROM {name} ORDER BY rowid"))
            .unwrap();
        let names = statement
            .column_names()
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        let rows = statement
            .query_map([], |row| {
                Ok(Json::object(names.iter().enumerate().map(|(i, key)| {
                    let value = match row.get_ref(i).unwrap() {
                        ValueRef::Null => Json::Null,
                        ValueRef::Integer(n) => Json::Number(n as f64),
                        ValueRef::Real(n) => Json::Number(n),
                        ValueRef::Text(s) => String::from_utf8_lossy(s).into_owned().into(),
                        ValueRef::Blob(bytes) => Json::object([
                            ("type", "Buffer".into()),
                            (
                                "data",
                                Json::Array(
                                    bytes.iter().map(|b| Json::Number(*b as f64)).collect(),
                                ),
                            ),
                        ]),
                    };
                    (key.as_str(), value)
                })))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        (name, Json::Array(rows))
    }))
}
struct HistorySource<'a> {
    input: Json,
    connection: &'a Connection,
    trace: RefCell<Vec<Json>>,
    queries: Cell<usize>,
    identity: Rc<()>,
    raw_cause: Cell<usize>,
}
impl HistorySource<'_> {
    async fn request(&self, method: &str, params: Json) -> Result<Json, Error> {
        let rpc = {
            let mut trace = self.trace.borrow_mut();
            trace.push(Json::object([
                ("method", method.into()),
                ("params", params.clone()),
            ]));
            trace.len()
        };
        if f(f(&self.input, "mutation"), "rpc").as_f64() == Some(rpc as f64) {
            seed_rows(self.connection, f(&self.input, "mutation"));
        }
        tokio::task::yield_now().await;
        if method == "objects.query" {
            if let Some(error) = self.input.get("queryError") {
                return Err(ivy(error).into());
            }
            let index = self.queries.replace(self.queries.get() + 1);
            return Ok(items(f(&self.input, "queryPages"))[index].clone());
        }
        assert_eq!(method, "objects.read");
        if let Some(raw) = self.input.get("rawSource") {
            let error = Error::source(Cause::Raw {
                value: f(raw, "value").clone(),
                identity: self.identity.clone(),
            });
            self.raw_cause.set(error.cause() as *const _ as usize);
            tokio::task::yield_now().await;
            return Err(error);
        }
        if let Some(error) = self.input.get("readError") {
            return Err(ivy(error).into());
        }
        if let Some(steps) = self.input.get("readSteps") {
            let offset = self
                .trace
                .borrow()
                .iter()
                .filter(|row| f(row, "method") == &Json::from("objects.read"))
                .count()
                - 1;
            let step = &items(steps)[offset];
            if let Some(error) = step.get("error") {
                return Err(ivy(error).into());
            }
            return Ok(f(step, "value").clone());
        }
        Ok(f(&self.input, "records")
            .get_key(f(&params, "objectId").as_js_string().unwrap())
            .unwrap()
            .clone())
    }
}
impl ObjectReads<Error> for HistorySource<'_> {
    async fn read(&self, params: Option<Json>) -> Result<Json, Error> {
        self.request("objects.read", params.unwrap()).await
    }
}
impl HistoryQueries<Error> for HistorySource<'_> {
    async fn query(&self, params: Json) -> Result<Json, Error> {
        self.request("objects.query", params).await
    }
}

#[test]
fn current_collection_and_compact_receipts_match_frozen_store() {
    run_collection_fixture(
        "IVY_TASK_BOARD_HISTORY_FIXTURE",
        "IVY_TASK_BOARD_HISTORY_NATIVE",
        (36, 42, 8),
    );
}
#[test]
fn current_recovery_lineage_and_cooperative_index_match_frozen_store() {
    run_collection_fixture(
        "IVY_TASK_BOARD_HISTORY_EXTRA_FIXTURE",
        "IVY_TASK_BOARD_HISTORY_EXTRA_NATIVE",
        (0, 7, 0),
    );
}
#[test]
fn current_collection_preserves_the_raw_string_coercion_boundary() {
    run_collection_fixture(
        "IVY_TASK_BOARD_HISTORY_COERCION_FIXTURE",
        "IVY_TASK_BOARD_HISTORY_COERCION_NATIVE",
        (0, 7, 0),
    );
}
fn run_collection_fixture(fixture_var: &str, native_var: &str, counts: (usize, usize, usize)) {
    let fixture_path = std::env::var(fixture_var).unwrap();
    let fixture = parse(&std::fs::read_to_string(fixture_path).unwrap(), usize::MAX).unwrap();
    assert_eq!(items(f(&fixture, "receipts")).len(), counts.0);
    assert_eq!(items(f(&fixture, "cases")).len(), counts.1);
    assert_eq!(items(f(&fixture, "readCases")).len(), counts.2);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let catalog = Catalog(f(&fixture, "contracts").clone());
    let validator = validator();
    let mut observations = vec![];
    runtime.block_on(async {
        for case in items(f(&fixture, "receipts")) {
            let connection = Connection::open_in_memory().unwrap();
            let source = HistorySource {
                input: Json::Null,
                connection: &connection,
                trace: RefCell::default(),
                queries: Cell::new(0),
                identity: Rc::new(()),
                raw_cause: Cell::new(0),
            };
            let result = observed(
                operation_receipt::<Error>(f(case, "value")),
                &source,
                &Cell::new(0),
            );
            exact(&result, f(case, "expected"), &text(f(case, "name")));
        }
        for case in items(f(&fixture, "cases"))
            .iter()
            .chain(items(f(&fixture, "readCases")))
        {
            let connection = Connection::open_in_memory().unwrap();
            initialize(&connection).unwrap();
            let is_read = case.get("args").is_some();
            let input = if is_read { case } else { f(case, "input") };
            seed_rows(&connection, input);
            if !is_read {
                for id in items(f(input, "cleanup")) {
                    connection
                        .execute(
                            "INSERT INTO cleanup_roots VALUES(?,?)",
                            params![text(id), text(f(&fixture, "at"))],
                        )
                        .unwrap();
                }
            }
            let native_identity = Cell::new(0);
            let parser_inputs = RefCell::new(vec![]);
            let parse_record = |raw: &str| {
                parser_inputs.borrow_mut().push(Json::from(raw));
                parse(raw, usize::MAX).map_err(|error| {
                    let error = Box::new(error);
                    native_identity.set(&*error as *const _ as usize);
                    Error::source(Cause::Parser(error))
                })
            };
            let sql_failure = |error| {
                let error = Box::new(error);
                native_identity.set(&*error as *const _ as usize);
                Error::source(Cause::Sqlite(error))
            };
            let local = SqliteReads {
                journal: LocalJournal {
                    connection: &connection,
                    root_object_id: f(input, "root").as_js_string().cloned(),
                },
                contracts: &catalog,
                parse: parse_record,
                sqlite_failure: sql_failure,
            };
            let source = HistorySource {
                input: input.clone(),
                connection: &connection,
                trace: RefCell::default(),
                queries: Cell::new(0),
                identity: Rc::new(()),
                raw_cause: Cell::new(0),
            };
            let name = text(f(case, "name"));
            if is_read {
                let args = items(f(case, "args"));
                let result = observed(
                    local
                        .retired_operation_receipt::<Error>(args.first(), args.get(1))
                        .map(|v| v.unwrap_or(Json::Null)),
                    &source,
                    &native_identity,
                );
                exact(&result, f(case, "expected"), &name);
                observations.push(Json::object([("name", name.into()), ("result", result)]));
                continue;
            }
            let comments_source = Arc::new(Source {
                input: Json::object([] as [(&str, Json); 0]),
                trace: Mutex::default(),
                identity: Arc::new(()),
                payload: AtomicUsize::new(0),
                cause: AtomicUsize::new(0),
                writes: AtomicUsize::new(0),
            });
            let comments = TaskComments::new(comments_source.clone(), runtime.handle().clone());
            let validate = |step: &Json| -> Result<(), Error> {
                assert!(validator.matches(step)?);
                Ok(())
            };
            let now = || f(&fixture, "at").as_js_string().unwrap().clone();
            let condition = |task: &Json, steps: Option<&Json>| task_condition(&local, task, steps);
            let policies = ConversationPolicies::current_raw(&validate, &now, &condition);
            let bind = |_: WireFailure| -> Error {
                panic!("Collection fixture never reads a comment tree")
            };
            let store = PersistentReadStore {
                local: &local,
                objects: &source,
                policies: &policies,
                comments: &comments,
                source_failure: &bind,
                cache: None,
            };
            let collections = HistoryCollections::default();
            let history = LocalHistory::new(
                &local,
                &store,
                &source,
                &collections,
                HistoryLimits {
                    count: f(input, "limitRows").as_f64().unwrap() as u64,
                    bytes: f(input, "limitBytes").as_f64().unwrap() as u64,
                },
            );
            let work = history.collect::<Cause>(
                f(input, "reservedBytes").as_f64().unwrap_or(0.0) as u64,
                f(input, "reservedRows").as_f64().unwrap_or(0.0) as u64,
                f(input, "protectedId").as_js_string(),
                input.get("incoming"),
            );
            let result = if let Some(mutation) = input.get("cooperativeMutation") {
                let mut work = Box::pin(work);
                assert!(futures_util::poll!(work.as_mut()).is_pending());
                assert!(parser_inputs.borrow().is_empty());
                assert!(futures_util::poll!(work.as_mut()).is_pending());
                assert_eq!(
                    parser_inputs.borrow().len(),
                    f(mutation, "afterParses").as_f64().unwrap() as usize
                );
                seed_rows(&connection, mutation);
                work.await
            } else {
                work.await
            };
            let result = observed(result.map(|()| Json::Null), &source, &native_identity);
            exact(&result, f(case, "expected"), &name);
            let trace = Json::Array(source.trace.borrow().clone());
            exact(&trace, f(case, "trace"), &format!("{name} RPC"));
            let tables = snapshot(&connection, f(&fixture, "tables"));
            exact(&tables, f(case, "tables"), &format!("{name} tables"));
            let mut receipts = vec![];
            for (caller, id) in [
                ("caller", "original-id"),
                ("other", "original-id"),
                ("caller", "other"),
            ] {
                receipts.push(observed(
                    local
                        .retired_operation_receipt::<Error>(Some(&caller.into()), Some(&id.into()))
                        .map(|v| v.unwrap_or(Json::Null)),
                    &source,
                    &native_identity,
                ));
            }
            exact(
                &Json::Array(receipts.clone()),
                f(case, "receipts"),
                &format!("{name} retained reads"),
            );
            if let Some(expected) = case.get("parserInputs") {
                exact(
                    &Json::Array(parser_inputs.borrow().clone()),
                    expected,
                    &format!("{name} current-head decodes"),
                );
            }
            observations.push(Json::object([
                ("name", name.into()),
                ("result", result),
                ("trace", trace),
                ("tables", tables),
                ("receipts", Json::Array(receipts)),
                ("parserInputs", Json::Array(parser_inputs.into_inner())),
            ]));
        }
    });
    std::fs::write(
        std::env::var(native_var).unwrap(),
        stringify(&Json::Array(observations), usize::MAX).unwrap(),
    )
    .unwrap();
}

struct QueueSource {
    queries: Cell<usize>,
    release: tokio::sync::Notify,
    failure: Option<Error>,
}
impl HistoryQueries<Error> for QueueSource {
    async fn query(&self, _: Json) -> Result<Json, Error> {
        let offset = self.queries.replace(self.queries.get() + 1);
        if offset == 0 {
            self.release.notified().await;
            if let Some(error) = &self.failure {
                return Err(error.clone());
            }
        }
        Ok(Json::object([
            ("items", Json::Array(vec![])),
            ("nextCursor", Json::Null),
        ]))
    }
}
impl ObjectReads<Error> for QueueSource {
    async fn read(&self, _: Option<Json>) -> Result<Json, Error> {
        panic!("Queue controls have no remote owner")
    }
}
struct NoDocuments;
impl HistoryDocuments<Error> for NoDocuments {
    async fn history_document(
        &self,
        _: &str,
        _: Option<&Json>,
        _: bool,
        _: bool,
    ) -> Result<Json, Error> {
        panic!("Queue controls have no primary or recovery read")
    }
}
#[test]
fn current_collection_queue_rechecks_budget_and_preserves_a_failed_predecessor() {
    let fixture = parse(
        &std::fs::read_to_string(std::env::var("IVY_TASK_BOARD_HISTORY_FIXTURE").unwrap()).unwrap(),
        usize::MAX,
    )
    .unwrap();
    let expected = parse(
        &std::fs::read_to_string(std::env::var("IVY_TASK_BOARD_HISTORY_QUEUE").unwrap()).unwrap(),
        usize::MAX,
    )
    .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut controls = vec![];
    runtime.block_on(async {
        for mode in ["recheck", "failure"] {
            let connection = Connection::open_in_memory().unwrap();
            initialize(&connection).unwrap();
            let row = items(f(f(&items(f(&fixture, "cases"))[0], "input"), "rows"))[0].clone();
            seed_rows(
                &connection,
                &Json::object([("rows", Json::Array(vec![row]))]),
            );
            connection
                .execute(
                    "INSERT INTO cleanup_roots VALUES('closed',?)",
                    [text(f(&fixture, "at"))],
                )
                .unwrap();
            let catalog = Catalog(f(&fixture, "contracts").clone());
            let local = SqliteReads {
                journal: LocalJournal {
                    connection: &connection,
                    root_object_id: Some("root".into()),
                },
                contracts: &catalog,
                parse: |raw: &str| {
                    parse(raw, usize::MAX).map_err(|e| Error::source(Cause::Parser(Box::new(e))))
                },
                sqlite_failure: |e| Error::source(Cause::Sqlite(Box::new(e))),
            };
            let failure = Error::source(Cause::Raw {
                value: Json::String(JsString(vec![0xd800])),
                identity: Rc::new(()),
            });
            let source = QueueSource {
                queries: Cell::new(0),
                release: tokio::sync::Notify::new(),
                failure: if mode == "failure" {
                    Some(failure.clone())
                } else {
                    None
                },
            };
            let collections = HistoryCollections::default();
            let history = LocalHistory::new(
                &local,
                &NoDocuments,
                &source,
                &collections,
                HistoryLimits { count: 0, bytes: 0 },
            );
            let mut first = Box::pin(history.collect::<Cause>(0, 0, None, None));
            assert!(futures_util::poll!(first.as_mut()).is_pending());
            assert_eq!(source.queries.get(), 1);
            let second_facade = LocalHistory::new(
                &local,
                &NoDocuments,
                &source,
                &collections,
                HistoryLimits { count: 0, bytes: 0 },
            );
            let mut second = Box::pin(second_facade.collect::<Cause>(0, 0, None, None));
            assert!(futures_util::poll!(second.as_mut()).is_pending());
            assert_eq!(source.queries.get(), 1);
            if mode == "recheck" {
                connection
                    .execute_batch("DELETE FROM revisions;DELETE FROM documents")
                    .unwrap();
            }
            source.release.notify_one();
            let (one, two) = futures_util::join!(first, second);
            two.unwrap();
            let one = if mode == "failure" {
                assert!(one.unwrap_err().ptr_eq(&failure));
                Json::object([("identity", true.into())])
            } else {
                one.unwrap();
                Json::object([("ok", true.into())])
            };
            let remaining: f64 = connection
                .query_row("SELECT COUNT(*) FROM documents", [], |r| r.get(0))
                .unwrap();
            let receipts: f64 = connection
                .query_row("SELECT COUNT(*) FROM retired_operation_receipts", [], |r| {
                    r.get(0)
                })
                .unwrap();
            controls.push(Json::object([
                ("mode", mode.into()),
                (
                    "results",
                    Json::Array(vec![one, Json::object([("ok", true.into())])]),
                ),
                ("queries", Json::Number(source.queries.get() as f64)),
                ("remaining", Json::Number(remaining)),
                ("receipts", Json::Number(receipts)),
            ]));
        }
    });
    exact(
        &Json::Array(controls.clone()),
        &expected,
        "Frozen queue controls",
    );
    std::fs::write(
        std::env::var("IVY_TASK_BOARD_HISTORY_QUEUE_NATIVE").unwrap(),
        stringify(&Json::Array(controls), usize::MAX).unwrap(),
    )
    .unwrap();
}
