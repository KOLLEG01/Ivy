#[path = "reference-data.rs"]
mod reference_data;
use ivy_contracts::{
    Outcome,
    IvyError, IvyJson as Json, IvyResult, JsString, hash::hash_json, json::parse,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_task_board::{
    deliveries::{Deliveries, DeliveryEngine, DeliveryPolicies, initial_delivery},
    operations::{Document, Store},
};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    path::Path,
    process::Command,
    sync::{Arc, Mutex, OnceLock},
};
#[path = "task-board-deliveries-journal.rs"]
mod journal;
fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn s<'a>(v: &'a Json, k: &str) -> &'a JsString {
    v.required(k).unwrap().as_js_string().unwrap()
}
fn doc(v: &Json) -> Document {
    Document {
        pin: f(v, "pin").clone(),
        value: f(v, "value").clone(),
        parent_id: f(f(v, "metadata"), "parentId").as_js_string().cloned(),
        content_hash: hash_json(f(v, "value")).unwrap(),
    }
}
fn json_doc(v: &Document) -> Json {
    Json::object([
        ("pin", v.pin.clone()),
        ("value", v.value.clone()),
        (
            "metadata",
            Json::object([(
                "parentId",
                v.parent_id.clone().map(Json::String).unwrap_or(Json::Null),
            )]),
        ),
    ])
}
fn error(v: &Json) -> IvyError {
    IvyError::new_js(s(v, "code").clone(), s(v, "message").clone())
        .with_provider_outcome(v.required("outcome").unwrap().clone())
        .with_details(f(v, "details").clone())
}
fn outcome(v: IvyResult<()>) -> Json {
    match v {
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
fn data() -> &'static Json {
    static D: OnceLock<Json> = OnceLock::new();
    D.get_or_init(|| {
        let o = reference_data::read(include_bytes!("task-board-deliveries-cases.json.gz"));
        parse(
            std::str::from_utf8(&o).unwrap(),
            MANAGEMENT_FRAME_BYTES,
        )
        .unwrap()
    })
}
struct Replay {
    steps: Mutex<VecDeque<Json>>,
    name: JsString,
}
impl Replay {
    fn call(&self, k: &str, args: impl IntoIterator<Item = Json>) -> IvyResult<Json> {
        let args = Json::Array(args.into_iter().collect());
        let e = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("Unexpected {k}: {:?} {args:?}", self.name));
        assert_eq!(f(&e, "kind"), &Json::from(k), "{:?} {args:?}", self.name);
        assert_eq!(
            hash_json(f(&e, "args")).unwrap(),
            hash_json(&args).unwrap(),
            "{:?} {k}: expected {:?}, actual {args:?}",
            self.name,
            f(&e, "args")
        );
        match e.get("error") {
            Some(e) => Err(error(e)),
            None => Ok(f(&e, "value").clone()),
        }
    }
}
struct Client(Arc<Replay>);
impl RpcClient for Client {
    fn request<'a>(&'a self, m: &'a str, a: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move { self.0.call("rpc", [m.into(), a]) })
    }
}
struct World<'a> {
    row: &'a Json,
    replay: Arc<Replay>,
    client: Arc<Client>,
    current: RefCell<Document>,
    pass: Cell<usize>,
    principal: JsString,
}
impl<'a> World<'a> {
    fn new(row: &'a Json) -> Self {
        let replay = Arc::new(Replay {
            steps: Mutex::new(
                f(row, "boundaries")
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect(),
            ),
            name: s(row, "name").clone(),
        });
        Self {
            row,
            client: Arc::new(Client(replay.clone())),
            replay,
            current: RefCell::new(doc(f(f(row, "initial"), "current"))),
            pass: Cell::new(0),
            principal: "caller".into(),
        }
    }
}
impl Store for World<'_> {
    fn root_object_id(&self) -> Option<JsString> {
        Some("root".into())
    }
    fn contract_version(&self, _: &str) -> IvyResult<String> {
        panic!("Unexpected contract")
    }
    fn retired_operation(&self, _: &JsString, _: &JsString) -> IvyResult<bool> {
        panic!("Unexpected operation")
    }
    fn local_key(&self, _: &JsString) -> IvyResult<Option<String>> {
        panic!("Unexpected local key")
    }
    fn complete_local_workflow(&self, _: &JsString, _: &JsString) -> IvyResult<()> {
        panic!("Unexpected completion")
    }
    async fn named(&self, _: &str, _: &str, _: Option<&JsString>) -> IvyResult<Option<Document>> {
        panic!("Unexpected named")
    }
    async fn read(&self, k: &str, a: &Json, p: bool) -> IvyResult<Document> {
        self.replay
            .call("read", [k.into(), a.clone(), p.into()])
            .map(|v| doc(&v))
    }
    async fn write(&self, k: &str, v: &Json, m: &str, d: &Json) -> IvyResult<Json> {
        let pin = self
            .replay
            .call("write", [k.into(), v.clone(), m.into(), d.clone()])?;
        *self.current.borrow_mut() = Document {
            pin: pin.clone(),
            value: v.clone(),
            parent_id: Some("root".into()),
            content_hash: hash_json(v)?,
        };
        Ok(pin)
    }
    async fn prepared_mutation_id(&self, _: &str) -> IvyResult<JsString> {
        panic!("Unexpected prepared write")
    }
    async fn write_prepared(&self, _: &str, _: &Json, _: &JsString, _: &Json) -> IvyResult<Json> {
        panic!("Unexpected prepared write")
    }
}
impl DeliveryPolicies for World<'_> {
    fn now(&self) -> JsString {
        f(self.row, "times").as_array().unwrap()[self.pass.get()]
            .as_js_string()
            .unwrap()
            .clone()
    }
    fn phone_target(&self) -> &Json {
        f(f(self.row, "initial"), "phoneTarget")
    }
    fn chat_readable_version(&self, v: &JsString) -> bool {
        [JsString::from("1.2.0"), JsString::from("1.3.0")].contains(v)
    }
    fn handoff(&self, t: &Json, c: &Json) -> IvyResult<JsString> {
        Ok(self
            .replay
            .call("handoff", [t.clone(), c.clone()])?
            .as_js_string()
            .unwrap()
            .clone())
    }
    fn voice_prompt(&self, t: &Json, c: &Json) -> IvyResult<JsString> {
        Ok(self
            .replay
            .call("voice", [t.clone(), c.clone()])?
            .as_js_string()
            .unwrap()
            .clone())
    }
}
impl DeliveryEngine for World<'_> {
    fn client(&self) -> Arc<dyn RpcClient> {
        self.client.clone()
    }
    fn principal_id(&self) -> &JsString {
        &self.principal
    }
    async fn verify_owner(&self) -> IvyResult<()> {
        self.replay.call("owner", []).map(|_| ())
    }
    async fn new_chat_operation(&self) -> IvyResult<JsString> {
        Ok(self
            .replay
            .call("new-chat", [])?
            .as_js_string()
            .unwrap()
            .clone())
    }
    fn new_phone_operation(&self) -> IvyResult<JsString> {
        Ok(self
            .replay
            .call("new-phone", [])?
            .as_js_string()
            .unwrap()
            .clone())
    }
}
#[tokio::test(flavor = "current_thread")]
async fn retained_chat_and_phone_deliveries_match_original_exact_effects_and_replies() {
    let mut cases = 0;
    let mut steps = 0;
    let mut boundaries = 0;
    let mut rpc = 0;
    for row in data().as_array().unwrap() {
        let world = World::new(row);
        cases += 1;
        let frames = f(row, "boundaries").as_array().unwrap();
        boundaries += frames.len();
        rpc += frames
            .iter()
            .filter(|b| f(b, "kind") == &Json::from("rpc"))
            .count();
        let initial = f(f(row, "initial"), "first");
        let input = f(row, "input");
        let route = if f(input, "mode")
            .as_str()
            .unwrap_or_default()
            .starts_with("phone")
        {
            "phone"
        } else {
            "chat"
        };
        let mut first = initial_delivery(
            f(initial, "phoneTarget"),
            f(initial, "publication"),
            s(initial, "taskId"),
            s(initial, "commentId"),
            &route.into(),
            s(initial, "createdAt"),
        );
        for (k, v) in f(input, "first").as_object().unwrap_or(&[]) {
            first.insert(k.clone(), v.clone());
        }
        assert_eq!(hash_json(&first).unwrap(), hash_json(initial).unwrap());
        let subject = Deliveries {
            store: &world,
            engine: &world,
        };
        let mut outcomes = vec![];
        let delivery_id = s(&world.current.borrow().pin, "objectId").clone();
        for p in 0..f(row, "times").as_array().unwrap().len() {
            steps += 1;
            world.pass.set(p);
            outcomes.push(outcome(subject.step(&delivery_id).await));
        }
        let actual = Json::object([
            ("outcomes", Json::Array(outcomes)),
            ("final", json_doc(&world.current.borrow())),
        ]);
        assert_eq!(
            hash_json(&actual).unwrap(),
            s(row, "expectedHash").to_utf8().unwrap(),
            "{:?}: {actual:?}",
            s(row, "name")
        );
        assert!(
            world.replay.steps.lock().unwrap().is_empty(),
            "Unconsumed {:?}",
            s(row, "name")
        );
    }
    assert_eq!((cases, steps, boundaries, rpc), (108, 128, 1573, 438));
}
