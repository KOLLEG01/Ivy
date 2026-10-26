#[path = "reference-data.rs"]
mod reference_data;
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, hash::hash_json, json::parse,
    limits::MANAGEMENT_FRAME_BYTES, operation::parse_operation_id_js,
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_task_board::{
    operations::Document,
    task_written::{
        TaskCommentNotices, TaskWriteGates, TaskWriteObserver, TaskWriteProjection, TaskWriteSink,
    },
};
use std::{
    collections::VecDeque,
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
};
fn f<'a>(value: &'a Json, key: &str) -> &'a Json {
    value.required(key).unwrap()
}
fn s<'a>(value: &'a Json, key: &str) -> &'a JsString {
    f(value, key).as_js_string().unwrap()
}
fn error(value: &Json) -> IvyError {
    IvyError::new_js(s(value, "code").clone(), s(value, "message").clone())
        .with_provider_outcome(value.required("outcome").unwrap().clone())
        .with_details(f(value, "details").clone())
}
fn outcome(value: IvyResult<()>) -> Json {
    match value {
        Ok(()) => Json::object([("ok", Json::Null)]),
        Err(e) => Json::object([(
            "error",
            Json::object([
                ("code", Json::String(e.code_js())),
                ("message", Json::String(e.message_js())),
                ("outcome", e.outcome.to_json()),
                ("details", e.details.unwrap_or(Json::Null)),
            ]),
        )]),
    }
}
struct Replay {
    name: JsString,
    pending: Mutex<VecDeque<Json>>,
    mutations: Mutex<Vec<JsString>>,
}
impl Replay {
    fn call(&self, kind: &str, args: impl IntoIterator<Item = Json>) -> IvyResult<Json> {
        let args = Json::Array(args.into_iter().collect());
        let frame = self
            .pending
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("{:?}: unexpected {kind} {args:?}", self.name));
        assert_eq!(
            f(&frame, "kind"),
            &Json::from(kind),
            "{:?}: {args:?}",
            self.name
        );
        assert_eq!(
            hash_json(f(&frame, "args")).unwrap(),
            hash_json(&args).unwrap(),
            "{:?}: expected {:?}, actual {args:?}",
            self.name,
            f(&frame, "args")
        );
        match frame.get("error") {
            Some(e) => Err(error(e)),
            None => Ok(f(&frame, "value").clone()),
        }
    }
    fn normalize(&self, mut args: Json) -> Json {
        if let Some(raw) = args.get("mutationId") {
            let raw = raw.as_js_string().unwrap().clone();
            let identity = parse_operation_id_js(&raw).unwrap();
            assert_eq!(
                identity.runtime_epoch,
                JsString::from("11111111-1111-4111-8111-111111111111")
            );
            let nonce = uuid::Uuid::parse_str(&identity.nonce.to_utf8().unwrap()).unwrap();
            assert_eq!(nonce.get_version_num(), 4);
            let mut mutations = self.mutations.lock().unwrap();
            let index = mutations
                .iter()
                .position(|id| id == &raw)
                .unwrap_or_else(|| {
                    mutations.push(raw);
                    mutations.len() - 1
                });
            // Only volatile issue time/UUID are projected. The alias preserves
            // exact identity reuse, separation and every other RPC argument.
            args.insert("mutationId", format!("mutation-{}", index + 1).into());
        }
        args
    }
}
struct Client {
    replay: Arc<Replay>,
    index: usize,
}
impl RpcClient for Client {
    fn request<'a>(&'a self, method: &'a str, args: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            self.replay.call(
                "rpc",
                [
                    Json::Number(self.index as f64),
                    method.into(),
                    self.replay.normalize(args),
                ],
            )
        })
    }
}
struct Bindings(Arc<Replay>);
impl TaskWriteProjection for Bindings {
    fn observe(&self, id: &JsString, revision: f64, task: &Json) -> IvyResult<()> {
        self.0
            .call(
                "observe",
                [
                    Json::String(id.clone()),
                    Json::Number(revision),
                    task.clone(),
                ],
            )
            .map(|_| ())
    }
}
impl TaskWriteGates for Bindings {
    async fn release_reserved(&self, operation: &JsString) -> IvyResult<()> {
        self.0
            .call("release", [Json::String(operation.clone()), true.into()])
            .map(|_| ())
    }
}
#[tokio::test(flavor = "current_thread")]
async fn task_write_observer_matches_original_selection_notice_bytes_order_and_failure_replay() {
    let output = reference_data::read(include_bytes!("task-board-task-written-cases.json.gz"));
    let rows = parse(
        std::str::from_utf8(&output).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    for row in rows.as_array().unwrap() {
        let replay = Arc::new(Replay {
            name: s(row, "name").clone(),
            pending: Mutex::new(
                f(row, "boundaries")
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect(),
            ),
            mutations: Mutex::default(),
        });
        let bindings = Bindings(replay.clone());
        let notices = TaskCommentNotices;
        let node = f(row, "options")
            .get("node")
            .and_then(Json::as_js_string)
            .cloned()
            .unwrap_or_else(|| "board-node".into());
        let operation = Document {
            pin: f(f(row, "operation"), "pin").clone(),
            value: f(f(row, "operation"), "value").clone(),
            parent_id: Some("root".into()),
            content_hash: String::new(),
        };
        let mut clients: Vec<Arc<dyn RpcClient>> = vec![];
        let mut outcomes = vec![];
        for call in f(row, "calls").as_array().unwrap() {
            let index = f(call, "client").as_f64().unwrap() as usize;
            while clients.len() <= index {
                clients.push(Arc::new(Client {
                    replay: replay.clone(),
                    index: clients.len(),
                }));
            }
            let observer = TaskWriteObserver {
                projection: &bindings,
                gates: &bindings,
                notices: &notices,
                client: clients[index].clone(),
                service_node_id: &node,
            };
            outcomes.push(outcome(
                TaskWriteSink::written(&observer, &operation, f(call, "pin"), f(call, "task"))
                    .await,
            ));
        }
        let actual = Json::object([("outcomes", Json::Array(outcomes))]);
        assert_eq!(
            hash_json(&actual).unwrap(),
            s(row, "expectedHash").to_utf8().unwrap(),
            "{:?}: actual {actual:?}, expected {:?}",
            replay.name,
            f(row, "expected")
        );
        assert!(
            replay.pending.lock().unwrap().is_empty(),
            "{:?}: unconsumed observer boundaries",
            replay.name
        );
        assert_eq!(
            replay.mutations.lock().unwrap().len(),
            f(row, "rawMutations").as_array().unwrap().len(),
            "{:?}: changed advisory identity scope",
            replay.name
        );
    }
}
