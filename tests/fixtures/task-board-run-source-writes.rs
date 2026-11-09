//! Resume a passive partial publication without inventing a pruned Task source.
use super::*;
use crate::journal_store::{Contracts, JournalStore, persistent_reads::local_reads::SqliteReads};
use crate::{
    local_journal::{LocalJournal, initialize},
    local_retention::HistoryLimits,
    operations::Store,
};
use ivy_contracts::{IvyResult, Outcome, hash::hash_json, json::parse};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_store::rusqlite::Connection;
use std::sync::{Arc, Mutex};

struct Source {
    before: Value,
    source_error: Option<&'static str>,
    state: Mutex<State>,
}
#[derive(Default)]
struct State {
    writes: Vec<Value>,
    receipt: Option<(String, Value)>,
    lost: bool,
}
impl Source {
    fn request(&self, method: &str, params: Value) -> IvyResult<Value> {
        match method {
            "objects.read" if params.get("objectId") == Some(&text("run")) => {
                assert_eq!(params.get("revision"), Some(&6.0.into()));
                Ok(self.before.clone())
            }
            "objects.read" => {
                assert_eq!(
                    params,
                    obj([("objectId", text("task")), ("revision", 7.0.into())])
                );
                match self.source_error {
                    Some("uncertain_pruned") => {
                        Err(IvyError::new("revision_pruned", "Uncertain source read.")
                            .with_outcome(Outcome::Unknown))
                    }
                    Some(code) => Err(IvyError::new(code, "Original source read.")),
                    None => Ok(Value::Null),
                }
            }
            "objects.write" => {
                let mut state = self.state.lock().unwrap();
                state.writes.push(params.clone());
                let hash = hash_json(&params)?;
                if let Some((original, receipt)) = &state.receipt {
                    assert_eq!(
                        &hash, original,
                        "A lost reply must retain every write argument"
                    );
                    return Ok(receipt.clone());
                }
                if params.required("references")?.get("source-task").is_some()
                    && self.source_error.is_some()
                {
                    return Err(IvyError::new(
                        "not_found",
                        "Referenced revision is not committed.",
                    ));
                }
                let receipt = obj([
                    (
                        "object",
                        obj([("id", text("run")), ("parentId", text("root"))]),
                    ),
                    ("revision", obj([("revision", 7.0.into())])),
                ]);
                state.receipt = Some((hash, receipt.clone()));
                if std::mem::take(&mut state.lost) {
                    return Err(IvyError::new("transport_lost", "Committed reply lost.")
                        .with_outcome(Outcome::Unknown));
                }
                Ok(receipt)
            }
            _ => panic!("No native action or other RPC is permitted: {method}"),
        }
    }
}
impl LocalMutationSource<()> for Source {
    async fn request_owned(&self, method: &str, params: Option<Value>) -> Result<Value, ()> {
        Source::request(self, method, params.unwrap()).map_err(Into::into)
    }
}
impl RpcClient for Source {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Value,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move { Source::request(self, method, params) })
    }
}
struct Admission(String);
impl Contracts for Admission {
    fn version(&self, _: &str) -> IvyResult<String> {
        Ok("3.1.0".into())
    }
    fn local(&self, _: &str) -> IvyResult<bool> {
        Ok(false)
    }
    fn readable_version(&self, _: &str, _: &JsString) -> IvyResult<bool> {
        Ok(true)
    }
    fn validate_record(&self, _: &str, _: &JsString, value: &Value) -> IvyResult<()> {
        assert_eq!(hash_json(value)?, self.0);
        Ok(())
    }
    fn validate_definition(&self, _: &str, _: &Value) -> IvyResult<()> {
        unreachable!()
    }
}
struct NoReads;
impl ObjectReads<WriteFailure<()>> for NoReads {
    async fn read(&self, _: Option<Value>) -> std::result::Result<Value, WriteFailure<()>> {
        unreachable!()
    }
}
impl TreeReads<WriteFailure<()>> for NoReads {
    async fn hydrate(
        &self,
        _: Value,
        _: &Value,
    ) -> std::result::Result<Option<Value>, WriteFailure<()>> {
        unreachable!()
    }
    fn summary(
        &self,
        _: &Value,
        _: &Value,
        _: &Value,
    ) -> std::result::Result<Option<Value>, WriteFailure<()>> {
        unreachable!()
    }
    async fn range(
        &self,
        _: &Value,
        _: &Value,
        _: usize,
        _: usize,
    ) -> std::result::Result<Vec<Value>, WriteFailure<()>> {
        unreachable!()
    }
}
fn original() -> Value {
    parse(r#"{"taskId":"task","phase":"outcome_unknown","originalRequest":{"expectedRevision":7},"calls":{"thread":"original-thread-call","turn":"original-turn-call"},"target":{"serviceNodeId":"original-owner"},"turnId":"original-turn","externalOutcome":{"state":"unknown","code":"unavailable"},"updatedAt":"before"}"#, 4096).unwrap()
}
fn record(value: &Value) -> Value {
    obj([
        (
            "object",
            obj([
                ("id", text("run")),
                ("contractKey", text("task-board/run")),
                ("parentId", text("root")),
                ("ownerObjectId", Value::Null),
            ]),
        ),
        (
            "revision",
            obj([
                ("objectId", text("run")),
                ("revision", 6.0.into()),
                ("contentHash", text(hash_json(value).unwrap())),
                ("references", obj([])),
            ]),
        ),
        (
            "content",
            obj([("encoding", text("json")), ("value", value.clone())]),
        ),
    ])
}
fn observation() -> Value {
    let mut v = original();
    v.insert("updatedAt", text("after"));
    v.required("externalOutcome").unwrap();
    v.insert(
        "externalOutcome",
        obj([
            ("state", text("unknown")),
            ("code", text("service_not_ready")),
        ]),
    );
    v
}
fn destination() -> Value {
    obj([("objectId", text("run")), ("expectedRevision", 6.0.into())])
}

async fn publish(
    source: Arc<Source>,
    value: &Value,
    destination: &Value,
    raw: bool,
) -> IvyResult<Value> {
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let contracts = Admission(hash_json(value)?);
    if raw {
        let store = JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            client: source,
            contracts,
            limits: HistoryLimits::default(),
            now: || "2026-01-01T00:00:00.000Z".into(),
            new_id: || "unused".into(),
            cache: None,
        };
        return store
            .write_prepared(
                "task-board/run",
                value,
                &"original-mutation".into(),
                destination,
                None,
                None,
            )
            .await;
    }
    let local = SqliteReads {
        journal: LocalJournal {
            connection: &db,
            root_object_id: Some("root".into()),
        },
        contracts: &contracts,
        parse: |v: &str| parse(v, MANAGEMENT_FRAME_BYTES).map_err(WriteFailure::<()>::from),
        sqlite_failure: |e| WriteFailure::<()>::from(ivy_store::sqlite::error(e)),
    };
    let reads = DocumentReads {
        local: &local,
        objects: &NoReads,
        comments: &NoReads,
        cache: None,
    };
    let unused = |_: Value, _: Value| {
        std::future::ready(Err(WriteFailure::<()>::from(IvyError::internal())))
    };
    let comments = |_: Value, _: Value, _: Value, _: Value| {
        std::future::ready(Err(WriteFailure::<()>::from(IvyError::internal())))
    };
    let identity =
        |_: Value| std::future::ready(Err(WriteFailure::<()>::from(IvyError::internal())));
    DocumentWrites {
        source: source.as_ref(),
        hydrate: &unused,
        comments: &comments,
        identity: &identity,
        now: &|| "2026-01-01T00:00:00.000Z".into(),
        new_id: &|| "unused".into(),
    }
    .write_graph(
        &reads,
        "task-board/run",
        value,
        &text("original-mutation"),
        destination,
        true,
        &text("3.1.0"),
        None,
    )
    .await
    .map_err(|e| typed_write_cause(&e).unwrap().clone())
}
#[tokio::test(flavor = "current_thread")]
async fn passive_partial_publication_retains_payload_and_replays_lost_reply() {
    for raw in [false, true] {
        for state in ["unknown", "succeeded", "failed"] {
            let source = Arc::new(Source {
                before: record(&original()),
                source_error: Some("revision_pruned"),
                state: Mutex::new(State {
                    lost: true,
                    ..State::default()
                }),
            });
            let mut value = observation();
            if state != "unknown" {
                value.insert("phase", text("publishing_result"));
                value.insert(
                    "externalOutcome",
                    obj([
                        ("state", text(state)),
                        ("evidence", text("verified-original-turn")),
                    ]),
                );
            }
            let error = publish(source.clone(), &value, &destination(), raw)
                .await
                .unwrap_err();
            assert_eq!(error.code, "transport_lost");
            assert_eq!(
                publish(source.clone(), &value, &destination(), raw)
                    .await
                    .unwrap()
                    .get("revision"),
                Some(&7.0.into())
            );
            let state = source.state.lock().unwrap();
            assert_eq!(state.writes.len(), 2);
            assert_eq!(state.writes[0], state.writes[1]);
            assert_eq!(state.writes[0].required("references").unwrap(), &obj([]));
            assert_eq!(
                state.writes[0]
                    .required("content")
                    .unwrap()
                    .required("value")
                    .unwrap(),
                &value
            );
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn passive_exception_never_changes_execution_or_removes_existing_protection() {
    for raw in [false, true] {
        for case in [
            "source_exists",
            "create",
            "phase",
            "calls",
            "target",
            "turnId",
            "originalRequest",
            "protected",
            "active",
            "false_completion",
            "completion_calls",
        ] {
            let mut before = original();
            let mut value = observation();
            let mut dest = destination();
            match case {
                "create" => {
                    dest = obj([(
                        "create",
                        obj([("name", text("new")), ("parentId", text("root"))]),
                    )]);
                }
                "active" => {
                    before.insert("phase", text("running"));
                }
                "source_exists" | "protected" => {}
                "false_completion" => {
                    value.insert("phase", text("publishing_result"));
                    value.insert("externalOutcome", obj([("state", text("active"))]));
                }
                "completion_calls" => {
                    value.insert("phase", text("publishing_result"));
                    value.insert("externalOutcome", obj([("state", text("succeeded"))]));
                    value.insert("calls", text("changed"));
                }
                name => {
                    value.insert(name, text("changed"));
                }
            }
            let mut saved = record(&before);
            if case == "protected" {
                saved.get_mut("revision").unwrap().insert(
                    "references",
                    obj([(
                        "source-task",
                        obj([("objectId", text("task")), ("revision", 7.0.into())]),
                    )]),
                );
            }
            let source = Arc::new(Source {
                before: saved,
                source_error: (case != "source_exists").then_some("revision_pruned"),
                state: Mutex::default(),
            });
            let result = publish(source.clone(), &value, &dest, raw).await;
            let state = source.state.lock().unwrap();
            if case == "originalRequest" || case == "phase" {
                assert!(result.is_err());
                continue;
            }
            assert_eq!(result.is_ok(), case == "source_exists", "{case}, raw={raw}");
            assert_eq!(state.writes.len(), 1, "{case}");
            assert!(
                state.writes[0]
                    .required("references")
                    .unwrap()
                    .get("source-task")
                    .is_some(),
                "{case}"
            );
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn missing_or_uncertain_reads_do_not_establish_pruning_or_an_exact_base() {
    for raw in [false, true] {
        for error in [
            "not_found",
            "transport_lost",
            "forbidden",
            "uncertain_pruned",
        ] {
            let source = Arc::new(Source {
                before: record(&original()),
                source_error: Some(error),
                state: Mutex::default(),
            });
            assert_eq!(
                publish(source.clone(), &observation(), &destination(), raw)
                    .await
                    .unwrap_err()
                    .code,
                if error == "uncertain_pruned" {
                    "revision_pruned"
                } else {
                    error
                }
            );
            assert!(source.state.lock().unwrap().writes.is_empty());
        }
        for field in [
            "id",
            "parentId",
            "ownerObjectId",
            "contractKey",
            "revision",
            "contentHash",
        ] {
            let mut before = record(&original());
            let section = if ["revision", "contentHash"].contains(&field) {
                "revision"
            } else {
                "object"
            };
            before
                .get_mut(section)
                .unwrap()
                .insert(field, text("wrong"));
            let source = Arc::new(Source {
                before,
                source_error: Some("revision_pruned"),
                state: Mutex::default(),
            });
            assert_eq!(
                publish(source.clone(), &observation(), &destination(), raw)
                    .await
                    .unwrap_err()
                    .code,
                "task_board_scope_mismatch"
            );
            assert!(source.state.lock().unwrap().writes.is_empty());
        }
    }
}
