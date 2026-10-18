use super::{
    Client as Records, Fixture, Gate, Live, Time, array, compare, field, flag, member, merge,
    number, snapshot, text,
};
#[path = "chat-cancel-worker-cases.rs"]
mod worker;
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_chat_bridge::{
    admission::ChatAdmission,
    cancellation::ChatCancellation,
    notice_input::NoticeInstructions,
    notices::{ChatNotices, MainFuture, NoticeMain},
    operations::ChatOperations,
    queue::ChatQueue,
    store::{ChatStore, Destination, Reference},
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, hash::digest, json::parse};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{
    collections::HashMap,
    path::Path,
    process::Command,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicI64, AtomicUsize, Ordering},
    },
};

fn oracle() -> &'static Json {
    static CORPUS: OnceLock<Json> = OnceLock::new();
    CORPUS.get_or_init(|| {
        let output = Command::new("node")
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .arg("tests/rust/fixtures/chat-cancellation-oracle.mjs")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if let Some(path) = std::env::var_os("IVY_CHAT_CANCELLATION_ORACLE_CAPTURE") {
            std::fs::write(path, &output.stdout).unwrap();
        }
        parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
    })
}
fn lookup<'a>(record: &'a Json, path: &str) -> Option<&'a Json> {
    let (base, path, separator) = if let Some(path) = path.strip_prefix("data:/") {
        (record.get("content")?.get("value")?, path, '/')
    } else {
        (record, path, '.')
    };
    path.split(separator).try_fold(base, |v, k| v.get(k))
}
fn matches(record: &Json, p: &Json) -> bool {
    match text(p, "op").as_str() {
        "and" => array(p, "args").iter().all(|p| matches(record, p)),
        "or" => array(p, "args").iter().any(|p| matches(record, p)),
        "isNull" => lookup(record, &text(p, "field")) == Some(&Json::Null),
        "eq" => lookup(record, &text(p, "field")) == p.get("value"),
        other => panic!("Unsupported cancellation predicate {other}"),
    }
}
struct Client(Records);
impl Client {
    fn new(records: Vec<Json>, history: Option<&Json>) -> Self {
        let client = Records::new(records);
        if let Some(history) = history {
            let rows = history
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| {
                    let entry = entry.as_array().unwrap();
                    let id = entry[0].as_str().unwrap();
                    let versions = entry[1]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|pair| {
                            let pair = pair.as_array().unwrap();
                            (pair[0].as_f64().unwrap() as usize, pair[1].clone())
                        })
                        .collect();
                    (id, versions)
                })
                .collect();
            client.0.lock().unwrap().versions = rows;
        }
        Self(client)
    }
    fn patch(&self, id: &str, patch: &Json) {
        let mut state = self.0.0.lock().unwrap();
        let record = state
            .records
            .iter_mut()
            .find(|r| field(r, "object").get("id") == Some(&id.into()))
            .unwrap();
        merge(member(member(record, "content"), "value"), patch);
        let revision = number(field(record, "revision"), "revision") + 1;
        member(record, "revision").insert("revision", Json::Number(revision as f64));
        let record = record.clone();
        state.versions.get_mut(id).unwrap().insert(revision, record);
    }
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        p: Json,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        if method != "objects.query" {
            return self.0.request(method, p, options);
        }
        Box::pin(async move {
            let mut state = self.0.0.lock().unwrap();
            state.calls.push(Json::object([
                ("method", method.into()),
                ("params", p.clone()),
            ]));
            let rows: Vec<_> = state
                .records
                .iter()
                .filter(|r| {
                    field(r, "object").get("contractKey") == p.get("contractKey")
                        && matches(r, field(&p, "where"))
                })
                .collect();
            let limit = number(&p, "limit");
            let items = rows
                .iter()
                .take(limit)
                .map(|r| {
                    Json::object([
                        ("objectId", field(field(r, "object"), "id").clone()),
                        ("revision", field(field(r, "revision"), "revision").clone()),
                    ])
                })
                .collect();
            Ok(Json::object([
                ("items", Json::Array(items)),
                (
                    "nextCursor",
                    if rows.len() > limit {
                        "more".into()
                    } else {
                        Json::Null
                    },
                ),
            ]))
        })
    }
}
struct Main<'a> {
    operations: &'a ChatOperations<'a>,
    instructions: &'a NoticeInstructions,
    live: &'a Mutex<Live>,
    owner_checks: &'a AtomicUsize,
}
impl NoticeMain for Main<'_> {
    fn ensure_notice<'a>(&'a self, _: &'a JsString, _: &'a JsString) -> MainFuture<'a> {
        Box::pin(async { panic!("Unexpected binding creation") })
    }
    fn verify_owner(&self) -> MainFuture<'_> {
        Box::pin(async move {
            let mut live = self.live.lock().unwrap();
            live.events
                .push(Json::object([("method", "verify".into())]));
            let count = self.owner_checks.fetch_add(1, Ordering::Relaxed) + 1;
            if live.controls.get("refuseAt").and_then(Json::as_f64) == Some(count as f64) {
                Err(IvyError::new(
                    "fixture_owner_unavailable",
                    "Original owner is unavailable.",
                ))
            } else {
                Ok(())
            }
        })
    }
    fn recover_pending(&self) -> MainFuture<'_> {
        Box::pin(async move {
            self.live
                .lock()
                .unwrap()
                .events
                .push(Json::object([("method", "recover".into())]));
            let queue = ChatQueue::new(self.operations, None);
            let current = queue.main().await?;
            if current.value.get("pendingAction").is_none_or(Json::is_null) {
                return Ok(());
            }
            let pin = super::reference(field(&current.value, "pendingAction"));
            let operation = self
                .operations
                .store
                .read("chat-bridge/operation", Reference::Pinned(&pin))
                .await
                .map_err(|e| e.into_ivy())?;
            match text(field(&operation.value, "request"), "action").as_str() {
                "cancel" => {
                    ChatCancellation::new(self.operations, self, self.instructions)
                        .recover_pending()
                        .await
                }
                "notify" => {
                    ChatNotices::new(self.operations, self, self.instructions)
                        .recover_pending()
                        .await
                }
                "send" => queue.recover_pending().await,
                _ => Err(IvyError::new(
                    "chat_pending_action",
                    "Unknown original Main claim.",
                )),
            }
        })
    }
}
fn option_text(value: &Json, key: &str, fallback: &str) -> String {
    value
        .get(key)
        .map(|v| v.as_str().unwrap())
        .unwrap_or_else(|| fallback.into())
}
fn request(
    action: &Json,
    seed: &Json,
    client: &Client,
    admission: &ChatAdmission,
    requests: &mut HashMap<String, Json>,
) -> Json {
    let name = option_text(action, "name", "c");
    let target = option_text(action, "target", "tail");
    let caller = option_text(action, "caller", "alice");
    if !flag(action, "replay") || !requests.contains_key(&name) {
        let id = action
            .get("inputId")
            .filter(|v| *v != &Json::from("head"))
            .cloned()
            .unwrap_or_else(|| field(field(seed, "inputs"), &target).clone());
        let revision = action.get("expectedRevision").cloned().unwrap_or_else(|| {
            client
                .0
                .0
                .lock()
                .unwrap()
                .records
                .iter()
                .find(|r| field(r, "object").get("id") == Some(&id))
                .map(|r| field(field(r, "revision"), "revision").clone())
                .unwrap_or(Json::Number(1.0))
        });
        let bridge = admission.expected(&if flag(action, "badBridge") {
            JsString::from("other")
        } else {
            caller.into()
        });
        requests.insert(
            name.clone(),
            Json::object([
                ("action", "cancel".into()),
                (
                    "operationId",
                    format!("chat:{}:cancel-{name}", number(oracle(), "now")).into(),
                ),
                ("inputId", id),
                ("expectedRevision", revision),
                ("expectedBridge", bridge),
                ("reason", "Original cancellation".into()),
            ]),
        );
    }
    let mut value = requests.get(&name).unwrap().clone();
    if let Some(reason) = action.get("reason") {
        value.insert("reason", reason.clone());
    }
    if action.get("inputId") == Some(&"head".into()) {
        value.insert("inputId", field(field(seed, "inputs"), "head").clone());
    }
    if flag(action, "omitBridge") {
        value.remove("expectedBridge");
    }
    value
}
async fn update(
    operations: &ChatOperations<'_>,
    key: &str,
    current: ivy_chat_bridge::store::Document,
    patch: &Json,
) -> IvyResult<()> {
    let mut next = current.value;
    merge(&mut next, patch);
    operations
        .store
        .write(
            key,
            &next,
            &format!("fixture:{}:{}", current.pin.object_id, current.pin.revision),
            Destination::Update {
                object_id: &current.pin.object_id,
                expected_revision: current.pin.revision,
            },
        )
        .await
        .map_err(|e| e.into_ivy())?;
    Ok(())
}
async fn action(
    s: &Json,
    seed: &Json,
    cancellation: &ChatCancellation<'_>,
    operations: &ChatOperations<'_>,
    client: &Client,
    live: &Mutex<Live>,
    requests: &mut HashMap<String, Json>,
) -> IvyResult<Json> {
    let name = option_text(s, "name", "c");
    let target = option_text(s, "target", "tail");
    let input_id = || text(field(seed, "inputs"), &target);
    let store = operations.store;
    let queue = ChatQueue::new(operations, None);
    match text(s, "action").as_str() {
        "cancel" => {
            let r = request(s, seed, client, operations.admission, requests);
            return cancellation
                .cancel(&option_text(s, "caller", "alice").into(), &r)
                .await;
        }
        "begin" => {
            let r = request(s, seed, client, operations.admission, requests);
            return Ok(operations
                .begin(&option_text(s, "caller", "alice").into(), &r)
                .await?
                .value);
        }
        "claim" => {
            let r = requests.get(&name).unwrap();
            let operation = operations
                .find(
                    field(field(r, "expectedBridge"), "callerPrincipalId")
                        .as_js_string()
                        .unwrap(),
                    field(r, "operationId").as_js_string().unwrap(),
                )
                .await?
                .unwrap();
            update(
                operations,
                "chat-bridge/main",
                queue.main().await?,
                &Json::object([("pendingAction", operation.pin.json())]),
            )
            .await?;
        }
        "claim-head-send" => {
            let input = store
                .read(
                    "chat-bridge/input",
                    Reference::Current(&text(field(seed, "inputs"), "head")),
                )
                .await
                .map_err(|e| e.into_ivy())?;
            let operation = operations
                .find(
                    field(field(&input.value, "identity"), "senderPrincipalId")
                        .as_js_string()
                        .unwrap(),
                    field(&input.value, "operationId").as_js_string().unwrap(),
                )
                .await?
                .unwrap();
            update(
                operations,
                "chat-bridge/main",
                queue.main().await?,
                &Json::object([(
                    "pendingAction",
                    Json::object([
                        ("objectId", operation.pin.object_id.into()),
                        ("revision", Json::Number(1.0)),
                    ]),
                )]),
            )
            .await?;
        }
        "recover" => cancellation.recover_pending().await?,
        "recover-intent" => {
            let input = store
                .read("chat-bridge/input", Reference::Current(&input_id()))
                .await
                .map_err(|e| e.into_ivy())?;
            return cancellation
                .recover(&input.value, &input.pin.object_id)
                .await;
        }
        "patch-input" => client.patch(&input_id(), field(s, "value")),
        "patch-cancellation" => {
            let mut cancellation = client
                .0
                .0
                .lock()
                .unwrap()
                .records
                .iter()
                .find(|r| field(r, "object").get("id") == Some(&input_id().into()))
                .map(|r| field(field(field(r, "content"), "value"), "cancellation").clone())
                .unwrap();
            merge(&mut cancellation, field(s, "value"));
            client.patch(&input_id(), &Json::object([("cancellation", cancellation)]));
        }
        "patch-operation" => {
            let r = requests.get(&name).unwrap();
            let operation = operations
                .find(
                    field(field(r, "expectedBridge"), "callerPrincipalId")
                        .as_js_string()
                        .unwrap(),
                    field(r, "operationId").as_js_string().unwrap(),
                )
                .await?
                .unwrap();
            update(
                operations,
                "chat-bridge/operation",
                operation,
                field(s, "value"),
            )
            .await?;
        }
        "remove-tail" => {
            let current = queue.main().await?;
            let tickets = array(&current.value, "queue")
                .iter()
                .filter(|t| t.get("inputId") != field(seed, "inputs").get("tail"))
                .cloned()
                .collect();
            update(
                operations,
                "chat-bridge/main",
                current,
                &Json::object([("queue", Json::Array(tickets))]),
            )
            .await?;
        }
        "fault" => {
            client.0.0.lock().unwrap().fault = Some(Json::object([
                ("kind", field(s, "kind").clone()),
                ("objectId", field(field(seed, "inputs"), "tail").clone()),
            ]));
        }
        "control" => merge(&mut live.lock().unwrap().controls, field(s, "value")),
        "reopen" => {}
        other => panic!("Unknown cancellation action {other}"),
    }
    Ok(Json::Null)
}
#[tokio::test]
async fn original_cancellation_admission_receipts_and_recovery() {
    let corpus = oracle();
    let instructions = NoticeInstructions::from_json(
        &parse(
            std::str::from_utf8(&STANDARD.decode(text(corpus, "instructionAsset")).unwrap())
                .unwrap(),
            usize::MAX,
        )
        .unwrap(),
    )
    .unwrap();
    let mut workflows = 0;
    let mut steps = 0;
    let mut original_crashes = 0;
    for case in array(corpus, "cases") {
        let seed = field(field(corpus, "seeds"), &text(case, "seed"));
        let bytes = case
            .get("database")
            .unwrap_or_else(|| field(seed, "database"));
        if let Some(hash) = case.get("databaseHash") {
            assert_eq!(
                digest(STANDARD.decode(bytes.as_str().unwrap()).unwrap()),
                hash.as_str().unwrap()
            );
            original_crashes += 1;
        }
        let path = Fixture::new(bytes);
        let client = Arc::new(Client::new(
            case.get("records")
                .unwrap_or_else(|| field(seed, "records"))
                .as_array()
                .unwrap()
                .to_vec(),
            case.get("history").or_else(|| seed.get("history")),
        ));
        let live = Mutex::new(Live {
            controls: Json::Object(vec![]),
            events: vec![],
            observations: Default::default(),
        });
        let clock = Arc::new(Time(AtomicI64::new(number(corpus, "now") as i64)));
        let gate = Gate::new();
        let owner_checks = AtomicUsize::new(0);
        let mut requests: HashMap<String, Json> = case
            .get("requests")
            .map(|v| {
                v.as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| (k.to_utf8().unwrap(), v.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let mut last = 0;
        let mut aliases = vec![];
        let mut mutations = HashMap::new();
        let mut index = 0;
        while index < array(case, "steps").len() {
            let store =
                ChatStore::with_clock(client.clone(), None, Some(path.0.clone()), clock.clone())
                    .await
                    .unwrap();
            let admission = ChatAdmission::new(field(corpus, "definition")).unwrap();
            let operations = ChatOperations::new(&store, &admission).unwrap();
            let main = Main {
                operations: &operations,
                instructions: &instructions,
                live: &live,
                owner_checks: &owner_checks,
            };
            let cancellation = ChatCancellation::new(&operations, &main, &instructions);
            loop {
                let s = &array(case, "steps")[index];
                let expected = &array(case, "expected")[index];
                gate.begin(&[]);
                let output = super::super::result(
                    action(
                        s,
                        seed,
                        &cancellation,
                        &operations,
                        &client,
                        &live,
                        &mut requests,
                    )
                    .await,
                );
                let actual = snapshot(
                    &path,
                    &client.0,
                    &live,
                    &gate,
                    output,
                    &mut last,
                    &mut aliases,
                    &mut mutations,
                );
                compare(
                    &actual,
                    expected,
                    &format!("{} step {index} {}", text(case, "label"), text(s, "action")),
                    "",
                );
                index += 1;
                steps += 1;
                if index == array(case, "steps").len() || text(s, "action") == "reopen" {
                    break;
                }
            }
        }
        workflows += 1;
    }
    println!(
        "Original cancellation admission differential: {workflows} workflows, {steps} steps, {original_crashes} original SQLite crash states; native settlement remains outside admission."
    );
}
