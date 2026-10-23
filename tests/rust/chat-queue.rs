use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_chat_bridge::{
    admission::ChatAdmission,
    operations::ChatOperations,
    queue::{BindingFuture, ChatQueue, EnsureBinding, conversation_name, input_name, main_name},
    store::{ChatStore, Destination, Document, Pin, Reference},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    json::{canonical, parse},
};
use ivy_sdk::{
    clock::Clock,
    rpc::{RequestFuture, RequestOptions, RpcClient},
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex, OnceLock},
    time::Instant,
};

struct Fixture(PathBuf);
impl Fixture {
    fn new(database: &str) -> Self {
        let path = std::env::temp_dir().join(format!("ivy-chat-queue-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(
            path.join("chat-bridge.sqlite"),
            STANDARD.decode(database).unwrap(),
        )
        .unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Fixed(i64);
impl Clock for Fixed {
    fn utc_millis(&self) -> i64 {
        self.0
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
fn text(value: &Json, key: &str) -> String {
    value.required(key).unwrap().as_str().unwrap()
}
fn js<'a>(value: &'a Json, key: &str) -> &'a JsString {
    value.required(key).unwrap().as_js_string().unwrap()
}
fn items<'a>(value: &'a Json, key: &str) -> &'a [Json] {
    value.required(key).unwrap().as_array().unwrap()
}
fn pin(value: &Json) -> Pin {
    Pin {
        object_id: text(value, "objectId"),
        revision: value.required("revision").unwrap().as_f64().unwrap() as i64,
    }
}
fn document(doc: Document) -> Json {
    Json::object([
        ("value", doc.value),
        ("revision", Json::Number(doc.pin.revision as f64)),
    ])
}
fn result(value: IvyResult<Json>) -> Json {
    match value {
        Ok(value) => Json::object([("value", value)]),
        Err(error) => Json::object([(
            "error",
            Json::object([
                ("code", error.code.into()),
                ("message", error.message.into()),
                ("outcome", error.outcome.to_json()),
            ]),
        )]),
    }
}
fn diff(actual: &Json, expected: &Json, path: &str) -> Option<String> {
    match (actual, expected) {
        (Json::Object(a), Json::Object(b)) if a.len() == b.len() => {
            for (key, value) in a {
                let name = key.to_utf8().unwrap();
                let Some(expected) = expected.get(&name) else {
                    return Some(format!("{path}.{name}: unexpected field"));
                };
                if let Some(error) = diff(value, expected, &format!("{path}.{name}")) {
                    return Some(error);
                }
            }
            None
        }
        (Json::Array(a), Json::Array(b)) if a.len() == b.len() => a
            .iter()
            .zip(b)
            .enumerate()
            .find_map(|(i, (a, b))| diff(a, b, &format!("{path}[{i}]"))),
        _ if actual == expected => None,
        _ => Some(format!(
            "{path}: actual={} expected={}",
            canonical(actual, usize::MAX)
                .unwrap()
                .chars()
                .take(1000)
                .collect::<String>(),
            canonical(expected, usize::MAX)
                .unwrap()
                .chars()
                .take(1000)
                .collect::<String>()
        )),
    }
}
fn equal(actual: &Json, expected: &Json, label: &str) {
    if let Some(error) = diff(actual, expected, label) {
        panic!("{error}");
    }
}

#[derive(Default)]
struct Client {
    records: Mutex<Vec<Json>>,
    fault: Mutex<Option<String>>,
}
impl Client {
    fn new(records: Vec<Json>) -> Self {
        Self {
            records: Mutex::new(records),
            fault: Mutex::new(None),
        }
    }
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            if method == "system.status" {
                return Ok(Json::object([("runtimeEpoch", "chat".into())]));
            }
            let mut records = self.records.lock().unwrap();
            if method == "objects.query" {
                let filters = items(params.required("where")?, "args");
                let name = filters
                    .iter()
                    .find(|f| f.get("field") == Some(&Json::from("object.name")))
                    .unwrap()
                    .required("value")?;
                let parent = filters[0].get("value").cloned().unwrap_or(Json::Null);
                let records = records
                    .iter()
                    .filter(|r| {
                        let object = r.required("object").unwrap();
                        object.get("contractKey") == params.get("contractKey")
                            && object.get("parentId") == Some(&parent)
                            && object.get("name") == Some(name)
                    })
                    .map(|r| {
                        Json::object([(
                            "objectId",
                            r.required("object")
                                .unwrap()
                                .required("id")
                                .unwrap()
                                .clone(),
                        )])
                    })
                    .collect();
                return Ok(Json::object([
                    ("items", Json::Array(records)),
                    ("nextCursor", Json::Null),
                ]));
            }
            if method == "objects.read" {
                return records
                    .iter()
                    .find(|r| r.required("object").unwrap().get("id") == params.get("objectId"))
                    .cloned()
                    .ok_or_else(|| IvyError::new("object_missing", "Fixture object is missing."));
            }
            assert_eq!(method, "objects.write");
            let mut fault_state = self.fault.lock().unwrap();
            if fault_state.as_deref() == Some("contention-input") {
                return Err(IvyError::new(
                    "revision_conflict",
                    "Fixture writer retained the competing name.",
                ));
            }
            let fault = fault_state.take();
            drop(fault_state);
            if fault.as_deref() == Some("before-input") {
                return Err(IvyError::new(
                    "fixture_unavailable",
                    "Fixture write was not executed.",
                ));
            }
            let index;
            if let Some(create) = params.get("create") {
                if records.iter().any(|r| {
                    let object = r.required("object").unwrap();
                    object.get("name") == create.get("name")
                        && object.get("contractKey") == create.get("contractKey")
                        && object.get("parentId") == create.get("parentId")
                }) {
                    return Err(IvyError::new(
                        "revision_conflict",
                        "Fixture name already exists.",
                    ));
                }
                index = records.len();
                let object = Json::object([
                    ("id", format!("fixture-input-{}", index + 1).into()),
                    ("name", create.required("name")?.clone()),
                    ("parentId", create.required("parentId")?.clone()),
                    ("contractKey", create.required("contractKey")?.clone()),
                    ("effectivelyArchived", false.into()),
                ]);
                records.push(Json::object([
                    ("object", object),
                    (
                        "revision",
                        Json::object([
                            ("revision", Json::Number(1.0)),
                            (
                                "contractVersion",
                                params.required("contractVersion")?.clone(),
                            ),
                            ("mediaType", "application/json".into()),
                        ]),
                    ),
                    ("content", params.required("content")?.clone()),
                ]));
            } else {
                index = records
                    .iter()
                    .position(|r| r.required("object").unwrap().get("id") == params.get("objectId"))
                    .unwrap();
                let n = records[index]
                    .required("revision")?
                    .required("revision")?
                    .as_f64()
                    .unwrap();
                if Some(&Json::Number(n)) != params.get("expectedRevision") {
                    return Err(IvyError::new(
                        "revision_conflict",
                        "Fixture revision changed.",
                    ));
                }
                let mut revision = records[index].required("revision")?.clone();
                revision.insert("revision", Json::Number(n + 1.0));
                records[index].insert("revision", revision);
                records[index].insert("content", params.required("content")?.clone());
            }
            if fault.as_deref() == Some("after-input") {
                return Err(IvyError::new(
                    "outcome_unknown",
                    "Fixture reply was lost after the input committed.",
                )
                .with_outcome(ivy_contracts::Outcome::Unknown));
            }
            Ok(records[index].clone())
        })
    }
}
struct Creator(Pin);
impl EnsureBinding for Creator {
    fn ensure_binding<'a>(&'a self, _: &'a JsString, _: &'a JsString) -> BindingFuture<'a> {
        Box::pin(async { Ok(self.0.clone()) })
    }
}
fn normalize(value: &Json, ids: &HashMap<String, String>) -> Json {
    match value {
        Json::String(s) => s
            .to_utf8()
            .ok()
            .and_then(|s| ids.get(&s))
            .map(|s| Json::from(s.clone()))
            .unwrap_or_else(|| value.clone()),
        Json::Array(a) => Json::Array(a.iter().map(|v| normalize(v, ids)).collect()),
        Json::Object(o) => Json::Object(
            o.iter()
                .map(|(k, v)| (k.clone(), normalize(v, ids)))
                .collect(),
        ),
        _ => value.clone(),
    }
}
async fn snapshot(
    store: &ChatStore,
    admission: &ChatAdmission,
    client: &Client,
    requests: &Json,
    other: Option<&Document>,
    output: Json,
) -> Json {
    let ops = ChatOperations::new(store, admission).unwrap();
    let mut documents = Json::Object(vec![]);
    let mut ids = HashMap::new();
    for (name, request) in requests.as_object().unwrap() {
        let caller = js(
            request.required("expectedBridge").unwrap(),
            "callerPrincipalId",
        );
        if let Some(doc) = store
            .named(
                "chat-bridge/operation",
                &ops.name(caller, js(request, "operationId")).unwrap(),
            )
            .await
            .unwrap()
        {
            ids.insert(
                doc.pin.object_id.clone(),
                format!("operation:{}", text(request, "operationId")),
            );
            documents.insert(name.clone(), document(doc));
        }
    }
    if let Some(other) = other {
        ids.insert(other.pin.object_id.clone(), "operation:other".into());
    }
    let main = store
        .named(
            "chat-bridge/main",
            &main_name(admission.definition()).unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    normalize(
        &Json::object([
            ("output", output),
            ("main", document(main)),
            ("operations", documents),
            (
                "inputs",
                Json::Array(client.records.lock().unwrap().clone()),
            ),
        ]),
        &ids,
    )
}
async fn update(store: &ChatStore, key: &str, doc: Document, patch: &Json) -> IvyResult<()> {
    let mut value = doc.value;
    for (key, v) in patch.as_object().unwrap() {
        value.insert(key.clone(), v.clone());
    }
    store
        .write(
            key,
            &value,
            "fixture-update",
            Destination::Update {
                object_id: &doc.pin.object_id,
                expected_revision: doc.pin.revision,
            },
        )
        .await
        .map_err(|e| e.into_ivy())?;
    Ok(())
}
fn oracle() -> &'static Json {
    static ORACLE: OnceLock<Json> = OnceLock::new();
    ORACLE.get_or_init(|| {
        let output = Command::new("node")
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .arg("tests/rust/fixtures/chat-queue-oracle.mjs")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
    })
}

#[tokio::test]
async fn queue_identity_admission_and_interrupted_sqlite_recovery_match_original() {
    let fixture = oracle();
    let admission = ChatAdmission::new(fixture.required("definition").unwrap()).unwrap();
    let binding = pin(fixture.required("binding").unwrap());
    let now = fixture.required("now").unwrap().as_f64().unwrap() as i64;
    let requests = fixture.required("requests").unwrap();
    for case in items(&fixture, "pure") {
        let path = Fixture::new(&text(&fixture, "database"));
        let store = ChatStore::with_clock(
            Arc::new(Client::default()),
            None,
            Some(path.0.clone()),
            Arc::new(Fixed(now)),
        )
        .await
        .unwrap();
        let ops = ChatOperations::new(&store, &admission).unwrap();
        let queue = ChatQueue::new(&ops, None);
        let actual = result((|| {
            if text(case, "kind") == "names" {
                let def = case.required("definition")?;
                let identity = case.required("identity")?;
                Ok(Json::object([
                    ("main", main_name(def)?.into()),
                    ("conversation", conversation_name(def)?.into()),
                    ("input", input_name(def, identity, None)?.into()),
                    (
                        "retained",
                        input_name(def, identity, Some(&text(case, "retainedHash")))?.into(),
                    ),
                ]))
            } else {
                Ok(queue
                    .check_main(Document {
                        key: "chat-bridge/main".into(),
                        pin: pin(fixture.required("main")?),
                        value: case.required("value")?.clone(),
                        metadata: Json::Null,
                        revision: Json::Null,
                    })?
                    .value)
            }
        })());
        equal(
            &actual,
            case.required("expected").unwrap(),
            &text(case, "label"),
        );
        store.close().await.unwrap();
    }
    for case in items(&fixture, "cases") {
        let database = case
            .get("database")
            .unwrap_or(fixture.get("database").unwrap())
            .as_str()
            .unwrap();
        let path = Fixture::new(&database);
        let client = Arc::new(Client::new(
            case.get("records")
                .and_then(Json::as_array)
                .unwrap_or(&[])
                .to_vec(),
        ));
        let clock = Arc::new(Fixed(now));
        let mut store =
            ChatStore::with_clock(client.clone(), None, Some(path.0.clone()), clock.clone())
                .await
                .unwrap();
        let creator = Creator(binding.clone());
        let mut other = None;
        for (index, step) in items(case, "steps").iter().enumerate() {
            let output = if text(step, "action") == "reopen" {
                store.close().await.unwrap();
                store = ChatStore::with_clock(
                    client.clone(),
                    None,
                    Some(path.0.clone()),
                    clock.clone(),
                )
                .await
                .unwrap();
                result(Ok(Json::Null))
            } else {
                let ops = ChatOperations::new(&store, &admission).unwrap();
                let queue = ChatQueue::new(
                    &ops,
                    case.get("ensure")
                        .and_then(Json::as_bool)
                        .filter(|v| *v)
                        .map(|_| &creator as &dyn EnsureBinding),
                );
                result(
                    async {
                        let main = || async {
                            store
                                .named("chat-bridge/main", &main_name(admission.definition())?)
                                .await
                                .map_err(|e| e.into_ivy())?
                                .ok_or_else(IvyError::internal)
                        };
                        let ops_ref = &ops;
                        let operation = |name: String| async move {
                            let request = requests.required(&name)?;
                            ops_ref
                                .find(
                                    js(request.required("expectedBridge")?, "callerPrincipalId"),
                                    js(request, "operationId"),
                                )
                                .await?
                                .ok_or_else(IvyError::internal)
                        };
                        match text(step, "action").as_str() {
                            "send" => {
                                let request = requests.required(&text(step, "name"))?;
                                let caller =
                                    if request.required("expectedBridge")?.get("callerPrincipalId")
                                        == Some(&Json::from("bob"))
                                    {
                                        "bob"
                                    } else {
                                        "alice"
                                    };
                                queue.send(&JsString::from(caller), request).await
                            }
                            "recover" => {
                                queue.recover_pending().await?;
                                Ok(Json::Null)
                            }
                            "begin" => Ok(ops
                                .begin(&"alice".into(), requests.required(&text(step, "name"))?)
                                .await?
                                .value),
                            "claim" => {
                                let doc = operation(text(step, "name")).await?;
                                update(
                                    &store,
                                    "chat-bridge/main",
                                    main().await?,
                                    &Json::object([("pendingAction", doc.pin.json())]),
                                )
                                .await?;
                                Ok(Json::Null)
                            }
                            "patch-main" => {
                                update(
                                    &store,
                                    "chat-bridge/main",
                                    main().await?,
                                    step.required("patch")?,
                                )
                                .await?;
                                Ok(Json::Null)
                            }
                            "patch-ticket" => {
                                let doc = main().await?;
                                let mut ticket = items(&doc.value, "queue")[0].clone();
                                for (key, value) in step.required("patch")?.as_object().unwrap() {
                                    ticket.insert(key.clone(), value.clone());
                                }
                                update(
                                    &store,
                                    "chat-bridge/main",
                                    doc,
                                    &Json::object([("queue", Json::Array(vec![ticket]))]),
                                )
                                .await?;
                                Ok(Json::Null)
                            }
                            "patch-input" => {
                                let mut records = client.records.lock().unwrap();
                                let mut content = records[0].required("content")?.clone();
                                let mut value = content.required("value")?.clone();
                                for (key, v) in step.required("patch")?.as_object().unwrap() {
                                    value.insert(key.clone(), v.clone());
                                }
                                content.insert("value", value);
                                records[0].insert("content", content);
                                Ok(Json::Null)
                            }
                            "patch-binding" => {
                                let doc = store
                                    .read("chat-bridge/binding", Reference::Pinned(&binding))
                                    .await
                                    .map_err(|e| e.into_ivy())?;
                                update(&store, "chat-bridge/binding", doc, step.required("patch")?)
                                    .await?;
                                Ok(Json::Null)
                            }
                            "patch-operation" => {
                                update(
                                    &store,
                                    "chat-bridge/operation",
                                    operation(text(step, "name")).await?,
                                    step.required("patch")?,
                                )
                                .await?;
                                Ok(Json::Null)
                            }
                            "refuse" => {
                                let doc = operation(text(step, "name")).await?;
                                Ok(ops
                                    .reject_unapplied(
                                        &doc.pin.object_id,
                                        "synthetic_refusal",
                                        "Proven unapplied.",
                                    )
                                    .await?
                                    .value)
                            }
                            "begin-other" => {
                                let request = Json::object([
                                    ("action", "acknowledge".into()),
                                    ("operationId", format!("chat:{now}:other").into()),
                                    ("expectedBridge", admission.expected(&"alice".into())),
                                    ("offerId", "offer".into()),
                                    ("replyIds", Json::Array(vec!["reply".into()])),
                                ]);
                                other = Some(ops.begin(&"alice".into(), &request).await?);
                                Ok(Json::Null)
                            }
                            "claim-other" => {
                                update(
                                    &store,
                                    "chat-bridge/main",
                                    main().await?,
                                    &Json::object([(
                                        "pendingAction",
                                        other.as_ref().unwrap().pin.json(),
                                    )]),
                                )
                                .await?;
                                Ok(Json::Null)
                            }
                            "fault" => {
                                *client.fault.lock().unwrap() = Some(text(step, "when"));
                                Ok(Json::Null)
                            }
                            "terminal-input" => {
                                let mut records = client.records.lock().unwrap();
                                let record = &mut records[0];
                                let mut content = record.required("content")?.clone();
                                let mut value = content.required("value")?.clone();
                                value.insert("state", "cancelled".into());
                                value.insert(
                                    "finishedAt",
                                    fixture
                                        .required("mainValue")?
                                        .required("updatedAt")?
                                        .clone(),
                                );
                                value.insert(
                                    "updatedAt",
                                    fixture
                                        .required("mainValue")?
                                        .required("updatedAt")?
                                        .clone(),
                                );
                                content.insert("value", value);
                                record.insert("content", content);
                                let mut rev = record.required("revision")?.clone();
                                rev.insert(
                                    "revision",
                                    Json::Number(rev.required("revision")?.as_f64().unwrap() + 1.0),
                                );
                                record.insert("revision", rev);
                                Ok(Json::Null)
                            }
                            "archive-input" => {
                                let mut records = client.records.lock().unwrap();
                                let mut object = records[0].required("object")?.clone();
                                object.insert("effectivelyArchived", true.into());
                                records[0].insert("object", object);
                                Ok(Json::Null)
                            }
                            _ => panic!("Unknown step"),
                        }
                    }
                    .await,
                )
            };
            let actual = snapshot(
                &store,
                &admission,
                &client,
                requests,
                other.as_ref(),
                output,
            )
            .await;
            equal(
                &actual,
                &items(case, "expected")[index],
                &format!("{} step {index}", text(case, "label")),
            );
        }
        store.close().await.unwrap();
    }
}

#[tokio::test]
async fn competing_instances_preserve_original_alias_conflict_and_final_slot_outcomes() {
    let fixture = oracle();
    let admission = ChatAdmission::new(fixture.required("definition").unwrap()).unwrap();
    let now = fixture.required("now").unwrap().as_f64().unwrap() as i64;
    let requests = fixture.required("requests").unwrap();
    for case in items(fixture, "races") {
        let path = Fixture::new(&text(fixture, "database"));
        let client = Arc::new(Client::default());
        let first = ChatStore::with_clock(
            client.clone(),
            None,
            Some(path.0.clone()),
            Arc::new(Fixed(now)),
        )
        .await
        .unwrap();
        let second = ChatStore::with_clock(
            client.clone(),
            None,
            Some(path.0.clone()),
            Arc::new(Fixed(now)),
        )
        .await
        .unwrap();
        let a = ChatOperations::new(&first, &admission).unwrap();
        let b = ChatOperations::new(&second, &admission).unwrap();
        let qa = ChatQueue::new(&a, None);
        let qb = ChatQueue::new(&b, None);
        let initial = items(case, "queue");
        if !initial.is_empty() {
            update(
                &first,
                "chat-bridge/main",
                qa.main().await.unwrap(),
                &Json::object([
                    ("queue", Json::Array(initial.to_vec())),
                    ("nextSequence", Json::Number(64.0)),
                ]),
            )
            .await
            .unwrap();
        }
        let names = items(case, "names");
        let left = requests.required(&names[0].as_str().unwrap()).unwrap();
        let right = requests.required(&names[1].as_str().unwrap()).unwrap();
        a.begin(&"alice".into(), left).await.unwrap();
        b.begin(&"alice".into(), right).await.unwrap();
        let caller = JsString::from("alice");
        let (left, right) = tokio::join!(qa.send(&caller, left), qb.send(&caller, right));
        let values = [left.unwrap(), right.unwrap()];
        qa.recover_pending().await.unwrap();
        let main = qa.main().await.unwrap();
        let mut phases = values.iter().map(|v| text(v, "phase")).collect::<Vec<_>>();
        phases.sort();
        let errors = values
            .iter()
            .filter(|v| text(v, "phase") == "failed")
            .map(|v| v.required("error").unwrap().clone())
            .collect::<Vec<_>>();
        let successes = values
            .iter()
            .filter(|v| text(v, "phase") == "succeeded")
            .collect::<Vec<_>>();
        let input_id = |v: &Json| {
            v.required("outcome")
                .unwrap()
                .required("input")
                .unwrap()
                .required("object")
                .unwrap()
                .required("objectId")
                .unwrap()
                .clone()
        };
        let records = client.records.lock().unwrap();
        let actual = Json::object([
            (
                "phases",
                Json::Array(phases.into_iter().map(Json::from).collect()),
            ),
            ("errors", Json::Array(errors)),
            (
                "queueLength",
                Json::Number(items(&main.value, "queue").len() as f64),
            ),
            (
                "nextSequence",
                main.value.required("nextSequence").unwrap().clone(),
            ),
            (
                "pending",
                (!main.value.required("pendingAction").unwrap().is_null()).into(),
            ),
            ("inputCount", Json::Number(records.len() as f64)),
            (
                "sameInput",
                (successes.len() == 2 && input_id(successes[0]) == input_id(successes[1])).into(),
            ),
        ]);
        equal(
            &actual,
            case.required("expected").unwrap(),
            &text(case, "label"),
        );
        // Every new ticket still names its exact public input, identity, binding,
        // original content hash and admission sequence after competing writers.
        for ticket in items(&main.value, "queue").iter().skip(initial.len()) {
            let record = records
                .iter()
                .find(|r| r.required("object").unwrap().get("id") == ticket.get("inputId"))
                .unwrap();
            let input = record
                .required("content")
                .unwrap()
                .required("value")
                .unwrap();
            for key in [
                "identity",
                "binding",
                "requestHash",
                "sequence",
                "admittedAt",
            ] {
                equal(
                    ticket.required(key).unwrap(),
                    input.required(key).unwrap(),
                    &format!("{} ticket.{key}", text(case, "label")),
                );
            }
        }
        drop(records);
        first.close().await.unwrap();
        second.close().await.unwrap();
    }
}
