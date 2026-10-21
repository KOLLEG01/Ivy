use ivy_contracts::{
    IvyError, IvyJson as Json, JsString, Outcome,
    json::{canonical, parse, stringify},
    limits::MANAGEMENT_FRAME_BYTES,
    schema::{CompiledSchema, SchemaValidators},
};
use ivy_sdk::task_comments::{CancellationToken, CommentReadFuture, CommentSource};
use ivy_task_board::{
    coordination::next_steps::{NextStepStore, assess_steps, validate_next_steps},
    engine_read::{
        comment_reads::read_task,
        condition_policy::{ConditionStore, task_condition},
        conversation::ConversationPolicies,
        read_failure::{ReadCause, ReadFailure},
    },
    journal_store::persistent_reads::{
        read_ports::{ReadPorts, TreeReadMode},
        record_upgrades::upgrade_stored_with,
        task_comments::TaskComments,
    },
};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    future::{pending, poll_fn},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::Poll,
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
fn fixture(name: &str) -> Json {
    parse(
        &std::fs::read_to_string(std::env::var(name).unwrap()).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap()
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
        "completed" => Outcome::Completed,
        "unknown" => Outcome::Unknown,
        _ => Outcome::NotExecuted,
    };
    error.details = v.get("details").cloned();
    error
}
// Deliberately neither Clone, Debug, Display, Error nor Sync. Shared futures
// clone only ReadFailure's identity carrier; the original payload stays here.
struct Opaque {
    value: Box<Json>,
    identity: Arc<()>,
    _not_sync: Cell<usize>,
}
enum SourceError {
    Raw(Opaque),
    Ivy(IvyError),
}
type Failure = ReadFailure<Opaque>;
fn observed(result: Result<Json, Failure>, source: Option<&Source>) -> Json {
    match result {
        Ok(value) => Json::object([("value", value)]),
        Err(error) => {
            if let Some(source) = source {
                let pin = source.returned_cause.load(Ordering::SeqCst);
                if pin != 0 {
                    assert_eq!(
                        error.cause() as *const _ as usize,
                        pin,
                        "Cached error identity"
                    );
                }
            }
            match error.cause() {
                ReadCause::Ivy(error) => Json::object([("ivy", error.to_wire())]),
                ReadCause::JavaScript(error) => Json::object([(
                    "javascript",
                    Json::object([
                        ("kind", error.kind.into()),
                        ("message", Json::String(error.message.clone())),
                    ]),
                )]),
                ReadCause::Source(raw) => {
                    let source = source.unwrap();
                    assert!(Arc::ptr_eq(&raw.identity, &source.identity));
                    assert_eq!(
                        &*raw.value as *const _ as usize,
                        source.returned_payload.load(Ordering::SeqCst)
                    );
                    Json::object([("source", (*raw.value).clone()), ("identity", true.into())])
                }
            }
        }
    }
}
struct Source {
    input: Json,
    node: Json,
    trace: Mutex<Vec<Json>>,
    identity: Arc<()>,
    returned_payload: AtomicUsize,
    returned_cause: AtomicUsize,
}
impl Source {
    fn failure(&self) -> SourceError {
        if let Some(error) = self.input.get("throwIvy") {
            return SourceError::Ivy(ivy(error));
        }
        let value = Box::new(f(&self.input, "throwValue").clone());
        self.returned_payload
            .store(&*value as *const _ as usize, Ordering::SeqCst);
        SourceError::Raw(Opaque {
            value,
            identity: self.identity.clone(),
            _not_sync: Cell::new(0),
        })
    }
    fn bind(&self, error: SourceError) -> Failure {
        let error = match error {
            SourceError::Raw(error) => Failure::source(error),
            SourceError::Ivy(error) => error.into(),
        };
        self.returned_cause
            .store(error.cause() as *const _ as usize, Ordering::SeqCst);
        error
    }
    async fn rpc(&self, params: Option<Json>) -> Result<Json, SourceError> {
        let mut row = Json::object([("method", "objects.read".into())]);
        if let Some(params) = &params {
            row.insert("params", params.clone());
        }
        self.trace.lock().unwrap().push(row);
        let id = text(f(params.as_ref().unwrap(), "objectId"));
        if id == "node" {
            if yes(&self.input, "failNode") {
                return Err(self.failure());
            }
            return Ok(Json::object([
                (
                    "object",
                    Json::object([
                        ("contractKey", "task-board/comment-node".into()),
                        ("parentId", "self".into()),
                        ("ownerObjectId", "self".into()),
                    ]),
                ),
                (
                    "revision",
                    Json::object([
                        ("contractVersion", "1.0.0".into()),
                        ("revision", Json::Number(1.0)),
                    ]),
                ),
                (
                    "content",
                    Json::object([("encoding", "json".into()), ("value", self.node.clone())]),
                ),
            ]));
        }
        if id == "artifact" {
            return Ok(Json::object([(
                "revision",
                Json::object([
                    ("contentHash", "sha256:a".into()),
                    ("mediaType", "application/json".into()),
                    ("byteLength", Json::Number(9.0)),
                ]),
            )]));
        }
        assert_eq!(id, "self");
        Ok(Json::object([
            ("object", Json::object([("id", "self".into())])),
            (
                "content",
                Json::object([("value", f(f(&self.input, "header"), "value").clone())]),
            ),
        ]))
    }
}
impl CommentSource for Source {
    type Error = SourceError;
    fn read<'a>(
        &'a self,
        pin: Json,
        cancellation: Option<CancellationToken>,
    ) -> CommentReadFuture<'a, SourceError> {
        assert!(cancellation.is_none());
        Box::pin(self.rpc(Some(pin)))
    }
}
impl ConditionStore<Failure> for Source {
    fn failed_handoff(&self, id: &JsString) -> Result<Option<Json>, Failure> {
        self.trace.lock().unwrap().push(Json::object([
            ("method", "failedHandoff".into()),
            ("runId", Json::String(id.clone())),
        ]));
        if yes(&self.input, "failCondition") {
            return Err(self.bind(self.failure()));
        }
        Ok(None)
    }
}
struct NoRecords;
impl NextStepStore<Failure> for NoRecords {
    async fn read(&self, _: &str, _: &Json, _: bool) -> Result<Json, Failure> {
        panic!("Tree value materialization cannot read another record")
    }
}
struct Records<'a, 'p> {
    source: &'a Source,
    comments: &'a TaskComments<Source>,
    policies: &'a ConversationPolicies<'p, Failure>,
    kind: &'a str,
}
impl NextStepStore<Failure> for Records<'_, '_> {
    async fn read(&self, key: &str, address: &Json, retained: bool) -> Result<Json, Failure> {
        let source = self.source;
        source.trace.lock().unwrap().push(Json::object([
            ("method", "read".into()),
            ("key", key.into()),
            ("id", address.clone()),
            (
                "flags",
                Json::Array(if retained {
                    vec![true.into(), true.into(), true.into()]
                } else if self.kind == "store" {
                    vec![true.into(), true.into(), false.into()]
                } else {
                    vec![true.into(), true.into()]
                }),
            ),
        ]));
        let id = address
            .as_js_string()
            .or_else(|| f(address, "objectId").as_js_string())
            .unwrap();
        if Some(id) == source.input.get("stalled").and_then(Json::as_js_string) {
            return pending().await;
        }
        let mut first = true;
        poll_fn(|cx| {
            if first {
                first = false;
                cx.waker().wake_by_ref();
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;
        if Some(id) == source.input.get("failRead").and_then(Json::as_js_string) {
            return Err(source.bind(source.failure()));
        }
        assert_eq!(key, "task-board/task");
        let prepared = yes(&source.input, "prepared") && id == &JsString::from("local");
        let (value, hydration_id) = if prepared {
            (
                f(f(&source.input, "header"), "value").clone(),
                f(f(f(&source.input, "operation"), "request"), "taskId")
                    .as_js_string()
                    .unwrap()
                    .clone(),
            )
        } else {
            let params = match address {
                Json::String(_) => Json::object([("objectId", address.clone())]),
                _ => address.clone(),
            };
            let saved = source
                .rpc(Some(params))
                .await
                .map_err(|error| source.bind(error))?;
            (
                f(f(&saved, "content"), "value").clone(),
                f(f(&saved, "object"), "id").as_js_string().unwrap().clone(),
            )
        };
        let mode = if retained {
            TreeReadMode::Retained
        } else if prepared {
            TreeReadMode::Prepared(&hydration_id)
        } else {
            TreeReadMode::Remote(&hydration_id)
        };
        let failure = |error| source.bind(error);
        let ports = ReadPorts {
            records: &NoRecords,
            comments: self.comments,
            source_failure: &failure,
        };
        let full = ports
            .read_tree_record(&value, mode, self.policies, |params| async move {
                source.rpc(params).await.map_err(|error| source.bind(error))
            })
            .await?;
        Ok(full
            .expect("These captured value cases are tree-backed")
            .into_owned())
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
    let schema = Json::object([
        ("$ref", "#/$defs/TaskBoardNextStep".into()),
        ("$defs", f(f(task, "jsonSchema"), "$defs").clone()),
    ]);
    SchemaValidators::new().compile(&schema).unwrap()
}
#[test]
fn lossless_ports_compose_actual_tree_resolver_modes_and_condition_store() {
    let fixture = fixture("IVY_TASK_BOARD_READ_PORTS_FIXTURE");
    let validator = validator();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut observations = Vec::new();
        for case in f(&fixture, "cases").as_array().unwrap() {
            let source = Arc::new(Source {
                input: f(case, "input").clone(),
                node: f(&fixture, "node").clone(),
                trace: Mutex::default(),
                identity: Arc::new(()),
                returned_payload: AtomicUsize::new(0),
                returned_cause: AtomicUsize::new(0),
            });
            let before = source.input.clone();
            let clocks = Cell::new(0);
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
            let now = || {
                clocks.set(clocks.get() + 1);
                f(&fixture, "at").as_js_string().unwrap().clone()
            };
            let condition =
                |task: &Json, steps: Option<&Json>| task_condition(&*source, task, steps);
            let policies = ConversationPolicies::current_raw(&validate, &now, &condition);
            let comments = TaskComments::new(source.clone(), runtime.handle().clone());
            let kind = text(f(case, "kind"));
            let records = Records {
                source: &source,
                comments: &comments,
                policies: &policies,
                kind: &kind,
            };
            let bind = |error| source.bind(error);
            let ports = ReadPorts {
                records: &records,
                comments: &comments,
                source_failure: &bind,
            };
            let input = &source.input;
            let result = match kind.as_str() {
                "assess" => assess_steps(
                    &ports,
                    &policies,
                    f(input, "task"),
                    f(&fixture, "at").as_js_string().unwrap(),
                    &HashMap::new(),
                    None,
                )
                .await
                .map(|values| Json::Array(values.iter().map(|v| v.as_json()).collect())),
                "validate" => validate_next_steps(
                    &ports,
                    &policies,
                    None,
                    f(input, "steps").as_array().unwrap(),
                    Some(f(input, "task")),
                    f(&fixture, "at").as_js_string().unwrap(),
                )
                .await
                .map(|_| Json::Null),
                "store" => {
                    let address = if yes(input, "prepared") {
                        Json::from("local")
                    } else {
                        f(f(input, "header"), "pin").clone()
                    };
                    ports
                        .read("task-board/task", &address, yes(input, "retained"))
                        .await
                }
                _ => read_task(&ports, &policies, f(input, "header"), f(input, "query")).await,
            };
            let result = observed(result, Some(&source));
            let name = text(f(case, "name"));
            same(&result, f(case, "expected"), &name);
            let trace = Json::Array(source.trace.lock().unwrap().clone());
            same(&trace, f(case, "trace"), &name);
            assert_eq!(
                clocks.get() as f64,
                f(case, "clocks").as_f64().unwrap(),
                "{name}"
            );
            same(input, &before, "Original input is not mutated");
            observations.push(Json::object([
                ("name", name.into()),
                ("result", result),
                ("trace", trace),
            ]));
        }
        assert_eq!(observations.len(), 13);
        if let Ok(path) = std::env::var("IVY_TASK_BOARD_READ_PORTS_NATIVE") {
            std::fs::write(
                path,
                stringify(&Json::Array(observations), MANAGEMENT_FRAME_BYTES).unwrap(),
            )
            .unwrap();
        }
    });
}
#[test]
fn raw_backfill_ports_keep_optional_parameters_failure_order_and_shared_upgrades() {
    let fixture = fixture("IVY_TASK_BOARD_RAW_RECORD_FIXTURE");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut observations = Vec::new();
        for case in f(&fixture, "cases").as_array().unwrap() {
            let input = f(case, "input");
            let before = input.clone();
            let remaining = RefCell::new(
                f(input, "rpc")
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect::<VecDeque<_>>(),
            );
            let trace = RefCell::new(Vec::new());
            let result = upgrade_stored_with::<Failure, _, _>(
                &text(f(input, "key")),
                f(input, "value"),
                |params| {
                    let mut row = Json::object([("method", "objects.read".into())]);
                    if let Some(params) = params {
                        row.insert("params", params);
                    }
                    trace.borrow_mut().push(row);
                    let next = remaining
                        .borrow_mut()
                        .pop_front()
                        .expect("Unexpected evidence read");
                    async move {
                        if let Some(error) = next.get("error") {
                            Err(ivy(error).into())
                        } else {
                            Ok(f(&next, "value").clone())
                        }
                    }
                },
            )
            .await
            .map(|value| value.into_owned());
            let result = observed(result, None);
            let name = text(f(case, "name"));
            same(&result, f(case, "expected"), &name);
            let trace = Json::Array(trace.into_inner());
            same(&trace, f(case, "trace"), &name);
            assert_eq!(
                remaining.borrow().len() as f64,
                f(case, "rpcRemaining").as_f64().unwrap()
            );
            same(input, &before, "Backfill input is not mutated");
            observations.push(Json::object([
                ("name", name.into()),
                ("result", result),
                ("trace", trace),
            ]));
        }
        assert_eq!(observations.len(), 45);
        if let Ok(path) = std::env::var("IVY_TASK_BOARD_RAW_RECORD_NATIVE") {
            std::fs::write(
                path,
                stringify(&Json::Array(observations), MANAGEMENT_FRAME_BYTES).unwrap(),
            )
            .unwrap();
        }
    });
}
