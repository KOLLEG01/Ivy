use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_chat_bridge::{
    admission::ChatAdmission,
    notice_input::{
        NoticeInstructions, admitted_notice_input, attributed_notice_reply, notice_prefix,
    },
    notices::{ChatNotices, MainFuture, NoticeMain},
    operations::ChatOperations,
    queue::{ChatQueue, input_name},
    store::{ChatStore, Destination, Document, Pin, Reference},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    hash::hash_json,
    json::{canonical, parse},
    operation::parse_operation_id,
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

fn text(v: &Json, k: &str) -> String {
    v.required(k).unwrap().as_str().unwrap()
}
fn js<'a>(v: &'a Json, k: &str) -> &'a JsString {
    v.required(k).unwrap().as_js_string().unwrap()
}
fn array<'a>(v: &'a Json, k: &str) -> &'a [Json] {
    v.required(k).unwrap().as_array().unwrap()
}
fn number(v: &Json, k: &str) -> f64 {
    v.required(k).unwrap().as_f64().unwrap()
}
fn flag(v: &Json, k: &str) -> bool {
    v.get(k) == Some(&Json::Bool(true))
}
fn pin(v: &Json) -> Pin {
    Pin {
        object_id: text(v, "objectId"),
        revision: number(v, "revision") as i64,
    }
}
fn document(d: Document) -> Json {
    Json::object([
        ("value", d.value),
        ("revision", Json::Number(d.pin.revision as f64)),
    ])
}
fn result(r: IvyResult<Json>) -> Json {
    match r {
        Ok(v) => Json::object([("value", v)]),
        Err(e) => Json::object([(
            "error",
            Json::object([
                ("code", Json::String(e.code_js())),
                ("message", Json::String(e.message_js())),
                ("outcome", e.outcome.as_str().into()),
            ]),
        )]),
    }
}
fn equal(a: &Json, b: &Json, label: &str) {
    assert_eq!(
        canonical(a, usize::MAX).unwrap(),
        canonical(b, usize::MAX).unwrap(),
        "{label}"
    );
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
struct Fixture(PathBuf);
impl Fixture {
    fn new(database: Option<&Json>) -> Self {
        let path = std::env::temp_dir().join(format!("ivy-chat-notices-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        if let Some(database) = database {
            std::fs::write(
                path.join("chat-bridge.sqlite"),
                STANDARD.decode(database.as_str().unwrap()).unwrap(),
            )
            .unwrap();
        }
        Self(path)
    }
    fn writes(&self, last: &mut i64) -> Vec<Json> {
        let db = rusqlite::Connection::open(self.0.join("chat-bridge.sqlite")).unwrap();
        let mut q = db
            .prepare(
                "SELECT rowid,key,value_json FROM notice_test_writes WHERE rowid>? ORDER BY rowid",
            )
            .unwrap();
        q.query_map([*last], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .map(|r| {
            let (id, key, value) = r.unwrap();
            *last = id;
            Json::object([
                ("key", key.into()),
                ("value", parse(&value, usize::MAX).unwrap()),
            ])
        })
        .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn oracle() -> &'static Json {
    static ORACLE: OnceLock<Json> = OnceLock::new();
    ORACLE.get_or_init(|| {
        let output = Command::new("node")
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .arg("tests/rust/fixtures/chat-notices-oracle.mjs")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if let Some(path) = std::env::var_os("IVY_CHAT_NOTICES_ORACLE_CAPTURE") {
            std::fs::write(path, &output.stdout).unwrap();
        }
        parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
    })
}
fn instructions() -> NoticeInstructions {
    let raw = STANDARD.decode(text(oracle(), "instructionAsset")).unwrap();
    NoticeInstructions::from_json(&parse(std::str::from_utf8(&raw).unwrap(), usize::MAX).unwrap())
        .unwrap()
}

#[test]
fn notice_attribution_admission_and_installed_prompt_match_original() {
    let corpus = oracle();
    let instructions = instructions();
    for case in array(corpus, "pure") {
        let r = (|| {
            let caller = js(case, "caller");
            Ok(Json::String(match text(case, "kind").as_str() {
                "prefix" => notice_prefix(caller)?,
                "attributed" => attributed_notice_reply(caller, js(case, "text"))?,
                "prompt" => instructions.prompt(caller, js(case, "text"))?,
                _ => panic!("Unknown pure case"),
            }))
        })();
        equal(
            &result(r),
            case.required("expected").unwrap(),
            &format!("pure {}", text(case, "kind")),
        );
    }
    let admission = ChatAdmission::new(corpus.required("definition").unwrap()).unwrap();
    for case in array(corpus, "admittedCases") {
        let r = (|| {
            let a = admitted_notice_input(
                &admission,
                js(case, "caller"),
                case.required("request")?,
                case.required("binding")?,
                &instructions,
            )?;
            Ok(Json::object([
                ("identity", a.identity),
                ("payload", a.payload),
                ("requestHash", a.request_hash.into()),
            ]))
        })();
        equal(
            &result(r),
            case.required("expected").unwrap(),
            &text(case, "label"),
        );
    }
    let fixture = Fixture::new(None);
    let folder = fixture.0.join("instructions");
    std::fs::create_dir(&folder).unwrap();
    assert!(NoticeInstructions::load(&fixture.0).is_err());
    for invalid in [
        "null",
        "[]",
        "[\"a\",\"b\"]",
        "[\"a\",4,\"c\"]",
        "[\"a\",\"b\",\"c\",\"d\"]",
        "not json",
    ] {
        std::fs::write(folder.join("chat-notice-prompt.json"), invalid).unwrap();
        assert!(NoticeInstructions::load(&fixture.0).is_err());
    }
    let bytes = STANDARD.decode(text(corpus, "instructionAsset")).unwrap();
    std::fs::write(folder.join("chat-notice-prompt.json"), bytes).unwrap();
    let loaded = NoticeInstructions::load(&fixture.0).unwrap();
    assert_eq!(
        loaded
            .prompt(&"task-board".into(), &"Original".into())
            .unwrap(),
        instructions
            .prompt(&"task-board".into(), &"Original".into())
            .unwrap()
    );
}

struct State {
    records: Vec<Json>,
    versions: HashMap<String, HashMap<i64, Json>>,
    calls: Vec<Json>,
    fault: Option<String>,
}
struct Client(Mutex<State>);
impl Client {
    fn new(records: Vec<Json>) -> Self {
        let versions = records
            .iter()
            .map(|r| {
                (
                    text(r.required("object").unwrap(), "id"),
                    HashMap::from([(
                        number(r.required("revision").unwrap(), "revision") as i64,
                        r.clone(),
                    )]),
                )
            })
            .collect();
        Self(Mutex::new(State {
            records,
            versions,
            calls: vec![],
            fault: None,
        }))
    }
    fn log(&self, method: &str, params: Json) {
        self.0.lock().unwrap().calls.push(Json::object([
            ("method", method.into()),
            ("params", params),
        ]));
    }
    fn execute(&self, method: &str, params: Json) -> IvyResult<Json> {
        self.log(method, params.clone());
        let mut state = self.0.lock().unwrap();
        match method {
            "system.status" => Ok(Json::object([("runtimeEpoch", "chat".into())])),
            "objects.query" => {
                let filters = array(params.required("where")?, "args");
                let name = filters
                    .iter()
                    .find(|f| f.get("field") == Some(&Json::from("object.name")))
                    .unwrap()
                    .required("value")?;
                let parent = filters[0].get("value").cloned().unwrap_or(Json::Null);
                let items = state
                    .records
                    .iter()
                    .filter(|r| {
                        let o = r.required("object").unwrap();
                        o.get("contractKey") == params.get("contractKey")
                            && o.get("parentId") == Some(&parent)
                            && o.get("name") == Some(name)
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
                Ok(Json::object([
                    ("items", Json::Array(items)),
                    ("nextCursor", Json::Null),
                ]))
            }
            "objects.read" => {
                let id = text(&params, "objectId");
                let current = state
                    .records
                    .iter()
                    .find(|r| text(r.required("object").unwrap(), "id") == id)
                    .ok_or_else(|| IvyError::new("object_missing", "Fixture object is missing."))?;
                if let Some(revision) = params.get("revision") {
                    state
                        .versions
                        .get(&id)
                        .and_then(|v| v.get(&(revision.as_f64().unwrap() as i64)))
                        .cloned()
                        .ok_or_else(|| {
                            IvyError::new("revision_pruned", "Fixture revision is unavailable.")
                        })
                } else {
                    Ok(current.clone())
                }
            }
            "objects.write" => {
                assert_eq!(
                    parse_operation_id(&text(&params, "mutationId"))?.runtime_epoch,
                    "chat"
                );
                let is_input = params.get("create").and_then(|c| c.get("contractKey"))
                    == Some(&Json::from("chat-bridge/input"));
                if is_input && state.fault.as_deref() == Some("contention-input") {
                    return Err(IvyError::new(
                        "revision_conflict",
                        "Fixture competing writer retained the input name.",
                    ));
                }
                if is_input && state.fault.as_deref() == Some("before-input") {
                    state.fault = None;
                    return Err(IvyError::new(
                        "fixture_unavailable",
                        "Fixture input write was not executed.",
                    ));
                }
                let index;
                if let Some(create) = params.get("create") {
                    if state.records.iter().any(|r| {
                        let o = r.required("object").unwrap();
                        o.get("name") == create.get("name")
                            && o.get("parentId") == create.get("parentId")
                            && o.get("contractKey") == create.get("contractKey")
                    }) {
                        return Err(IvyError::new(
                            "revision_conflict",
                            "Fixture name already exists.",
                        ));
                    }
                    index = state.records.len();
                    state.records.push(Json::object([
                        (
                            "object",
                            Json::object([
                                ("id", format!("fixture-record-{}", index + 1).into()),
                                ("parentId", create.required("parentId")?.clone()),
                                ("name", create.required("name")?.clone()),
                                ("contractKey", create.required("contractKey")?.clone()),
                                ("effectivelyArchived", false.into()),
                                ("createdAt", crate_at()),
                            ]),
                        ),
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
                    index = state
                        .records
                        .iter()
                        .position(|r| {
                            r.required("object").unwrap().get("id") == params.get("objectId")
                        })
                        .unwrap();
                    let n = number(state.records[index].required("revision")?, "revision");
                    if Some(&Json::Number(n)) != params.get("expectedRevision") {
                        return Err(IvyError::new(
                            "revision_conflict",
                            "Fixture revision changed.",
                        ));
                    }
                    let mut revision = state.records[index].required("revision")?.clone();
                    revision.insert("revision", Json::Number(n + 1.0));
                    state.records[index].insert("revision", revision);
                    state.records[index].insert("content", params.required("content")?.clone());
                }
                let current = state.records[index].clone();
                let id = text(current.required("object")?, "id");
                let revision = number(current.required("revision")?, "revision") as i64;
                state
                    .versions
                    .entry(id)
                    .or_default()
                    .insert(revision, current.clone());
                if is_input && state.fault.as_deref() == Some("after-input") {
                    state.fault = None;
                    return Err(IvyError::new(
                        "outcome_unknown",
                        "Fixture reply was lost after the input committed.",
                    )
                    .with_outcome(Outcome::Unknown));
                }
                Ok(current)
            }
            _ => panic!("Unexpected RPC {method}"),
        }
    }
    fn patch_input(&self, id: &str, patch: &Json) {
        let mut state = self.0.lock().unwrap();
        let index = state
            .records
            .iter()
            .position(|r| text(r.required("object").unwrap(), "id") == id)
            .unwrap();
        let mut current = state.records[index].clone();
        let mut revision = current.required("revision").unwrap().clone();
        revision.insert(
            "revision",
            Json::Number(number(&revision, "revision") + 1.0),
        );
        current.insert("revision", revision);
        let mut content = current.required("content").unwrap().clone();
        let mut value = content.required("value").unwrap().clone();
        for (k, v) in patch.as_object().unwrap() {
            value.insert(k.clone(), v.clone());
        }
        content.insert("value", value);
        current.insert("content", content);
        state.records[index] = current.clone();
        state.versions.get_mut(id).unwrap().insert(
            number(current.required("revision").unwrap(), "revision") as i64,
            current,
        );
    }
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move { self.execute(method, params) })
    }
}
fn crate_at() -> Json {
    Json::String(ivy_sdk::clock::date_to_iso(number(oracle(), "now")).unwrap())
}

#[derive(Default)]
struct Control {
    owner_refused: bool,
    publish_binding: bool,
    hold_pending: bool,
    claim_contention: bool,
    verify_count: usize,
}
struct Main<'a> {
    ops: &'a ChatOperations<'a>,
    client: &'a Client,
    instructions: &'a NoticeInstructions,
    control: &'a Mutex<Control>,
    binding: &'a Pin,
    main: &'a Pin,
}
async fn update(store: &ChatStore, key: &str, doc: Document, patch: &Json) -> IvyResult<()> {
    let mut value = doc.value;
    for (k, v) in patch.as_object().unwrap() {
        value.insert(k.clone(), v.clone());
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
impl NoticeMain for Main<'_> {
    fn ensure_notice<'a>(&'a self, caller: &'a JsString, id: &'a JsString) -> MainFuture<'a> {
        Box::pin(async move {
            self.client.log(
                "main.ensure",
                Json::object([
                    ("caller", Json::String(caller.clone())),
                    ("operationId", Json::String(id.clone())),
                    ("mode", "notice".into()),
                ]),
            );
            let publish = self.control.lock().unwrap().publish_binding;
            if publish {
                let doc = self
                    .ops
                    .store
                    .read("chat-bridge/main", Reference::Current(&self.main.object_id))
                    .await
                    .map_err(|e| e.into_ivy())?;
                update(
                    self.ops.store,
                    "chat-bridge/main",
                    doc,
                    &Json::object([("binding", self.binding.json())]),
                )
                .await?;
            }
            Ok(())
        })
    }
    fn recover_pending(&self) -> MainFuture<'_> {
        Box::pin(async move {
            self.client.log("main.recoverPending", Json::Object(vec![]));
            let hold = self.control.lock().unwrap().hold_pending;
            if !hold {
                ChatNotices::new(self.ops, self, self.instructions)
                    .recover_pending()
                    .await?;
            }
            Ok(())
        })
    }
    fn verify_owner(&self) -> MainFuture<'_> {
        Box::pin(async move {
            self.client.log("native.verifyOwner", Json::Object(vec![]));
            let conflict = {
                let mut c = self.control.lock().unwrap();
                c.verify_count += 1;
                if c.owner_refused {
                    return Err(IvyError::new(
                        "fixture_owner_refused",
                        "The original native owner is unavailable.",
                    ));
                }
                c.claim_contention && c.verify_count > 1
            };
            if conflict {
                let doc = self
                    .ops
                    .store
                    .read("chat-bridge/main", Reference::Current(&self.main.object_id))
                    .await
                    .map_err(|e| e.into_ivy())?;
                update(
                    self.ops.store,
                    "chat-bridge/main",
                    doc,
                    &Json::Object(vec![]),
                )
                .await?;
            }
            Ok(())
        })
    }
}
fn normalize(value: &Json, ids: &HashMap<String, String>) -> Json {
    match value {
        Json::String(s) => s
            .to_utf8()
            .ok()
            .and_then(|v| ids.get(&v))
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
    fixture: &Fixture,
    other: Option<&Document>,
    output: Json,
    last: &mut i64,
    mutations: &mut HashMap<String, String>,
) -> Json {
    let corpus = oracle();
    let ops = ChatOperations::new(store, admission).unwrap();
    let mut documents = Json::Object(vec![]);
    let mut ids = HashMap::new();
    for caller in ["producer", "other"] {
        for (name, req) in corpus.required("requests").unwrap().as_object().unwrap() {
            if let Some(doc) = ops
                .find(&caller.into(), js(req, "operationId"))
                .await
                .unwrap()
            {
                ids.insert(
                    doc.pin.object_id.clone(),
                    format!("operation:{caller}:{}", text(req, "operationId")),
                );
                documents.insert(
                    format!("{caller}:{}", name.to_utf8().unwrap()),
                    document(doc),
                );
            }
        }
    }
    if let Some(other) = other {
        ids.insert(other.pin.object_id.clone(), "operation:other".into());
    }
    let main = store
        .read(
            "chat-bridge/main",
            Reference::Current(&text(corpus.required("mainPin").unwrap(), "objectId")),
        )
        .await
        .unwrap();
    let state = client.0.lock().unwrap();
    let mut answer = normalize(
        &Json::object([
            ("output", output),
            ("main", document(main)),
            ("operations", documents),
            ("records", Json::Array(state.records.clone())),
            ("writes", Json::Array(fixture.writes(last))),
            ("calls", Json::Array(state.calls.clone())),
        ]),
        &ids,
    );
    drop(state);
    if let Some(calls) = answer.get("calls").unwrap().as_array() {
        let mut normalized = Vec::new();
        for mut call in calls.iter().cloned() {
            if call.get("method") == Some(&Json::from("objects.write")) {
                let mut params = call.required("params").unwrap().clone();
                let id = text(&params, "mutationId");
                let next = format!("<mutation:{}>", mutations.len());
                let alias = mutations.entry(id).or_insert(next).clone();
                params.insert("mutationId", alias.into());
                call.insert("params", params);
            }
            normalized.push(call);
        }
        answer.insert("calls", Json::Array(normalized));
    }
    client.0.lock().unwrap().calls.clear();
    answer
}
async fn operation(ops: &ChatOperations<'_>, name: &str) -> IvyResult<Document> {
    ops.find(
        &"producer".into(),
        js(
            oracle().required("requests")?.required(name)?,
            "operationId",
        ),
    )
    .await?
    .ok_or_else(IvyError::internal)
}
async fn input(store: &ChatStore, name: &str) -> IvyResult<Document> {
    let corpus = oracle();
    let definition = corpus.required("definition")?;
    let identity = Json::object([
        ("channel", corpus.required("channel")?.clone()),
        ("senderPrincipalId", "producer".into()),
        (
            "messageId",
            corpus
                .required("requests")?
                .required(name)?
                .required("operationId")?
                .clone(),
        ),
    ]);
    let name = input_name(definition, &identity, Some(&hash_json(definition)?))?;
    store
        .named("chat-bridge/input", &name)
        .await
        .map_err(|e| e.into_ivy())?
        .ok_or_else(IvyError::internal)
}
async fn publish(
    ops: &ChatOperations<'_>,
    client: &Client,
    name: &str,
    final_notice: bool,
) -> IvyResult<()> {
    let corpus = oracle();
    let op = operation(ops, name).await?;
    let mut immutable;
    let reply_name;
    if final_notice {
        immutable = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("workspaceId", "chat".into()),
            ("definitionHash", ops.admission.definition_hash().into()),
            ("channel", corpus.required("channel")?.clone()),
            (
                "origin",
                Json::object([
                    ("kind", "notice".into()),
                    (
                        "operation",
                        Pin {
                            object_id: "final-producer".into(),
                            revision: 1,
                        }
                        .json(),
                    ),
                    (
                        "claim",
                        Pin {
                            object_id: "final-claim".into(),
                            revision: 1,
                        }
                        .json(),
                    ),
                    ("producerPrincipalId", "producer".into()),
                    ("source", corpus.required("source")?.clone()),
                ]),
            ),
            ("sequence", Json::Number(2.0)),
            ("partIndex", Json::Number(0.0)),
            ("text", "Final notice".into()),
            ("artifacts", Json::Array(vec![])),
            ("createdAt", crate_at()),
        ]);
        reply_name = "Chat notice final-producer".to_string();
    } else {
        let original = input(ops.store, name).await?;
        let binding = original.value.required("binding")?.clone();
        let value = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("inputId", original.pin.object_id.clone().into()),
            ("binding", binding.clone()),
            ("turnId", "fixture-turn".into()),
            ("nativeState", "completed".into()),
            ("evidence", Json::Array(vec![original.pin.json()])),
            ("textParts", Json::Array(vec![])),
            ("createdAt", crate_at()),
        ]);
        let result = ops
            .store
            .write(
                "chat-bridge/result",
                &value,
                "fixture-result",
                Destination::Create {
                    parent_id: Some(&original.pin.object_id),
                    name: &format!("Fixture result {name}"),
                },
            )
            .await
            .map_err(|e| e.into_ivy())?;
        client.patch_input(
            &original.pin.object_id,
            &Json::object([
                ("state", "completed".into()),
                ("result", result.json()),
                ("turnId", "fixture-turn".into()),
                ("finishedAt", crate_at()),
                ("updatedAt", crate_at()),
            ]),
        );
        immutable = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("workspaceId", "chat".into()),
            ("definitionHash", ops.admission.definition_hash().into()),
            ("channel", corpus.required("channel")?.clone()),
            (
                "origin",
                Json::object([
                    ("kind", "native".into()),
                    ("inputId", original.pin.object_id.into()),
                    ("binding", binding),
                    ("result", result.json()),
                ]),
            ),
            (
                "sequence",
                Json::Number(number(&original.value, "sequence") + 1.0),
            ),
            ("partIndex", Json::Number(0.0)),
            ("text", format!("Native answer {name}").into()),
            ("artifacts", Json::Array(vec![])),
            ("createdAt", crate_at()),
        ]);
        reply_name = format!("Fixture reply {name}");
    }
    immutable.insert("immutableHash", hash_json(&immutable)?.into());
    immutable.insert("state", "pending".into());
    immutable.insert("firstOfferedAt", Json::Null);
    immutable.insert("confirmedAt", Json::Null);
    let reply = ops
        .store
        .write(
            "chat-bridge/reply",
            &immutable,
            if final_notice {
                "fixture-final"
            } else {
                "fixture-reply"
            },
            Destination::Create {
                parent_id: None,
                name: &reply_name,
            },
        )
        .await
        .map_err(|e| e.into_ivy())?;
    let doc = ops
        .store
        .read("chat-bridge/reply", Reference::Pinned(&reply))
        .await
        .map_err(|e| e.into_ivy())?;
    ops.finish(
        &op.pin.object_id,
        &Json::object([
            ("action", "notify".into()),
            ("operationId", op.value.required("operationId")?.clone()),
            (
                "reply",
                Json::object([("object", doc.pin.json()), ("data", doc.value)]),
            ),
        ]),
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn durable_notice_claims_recovery_and_delivery_match_original() {
    let corpus = oracle();
    let instructions = instructions();
    let main_pin = pin(corpus.required("mainPin").unwrap());
    let binding = pin(corpus.required("binding").unwrap());
    for case in array(corpus, "cases") {
        let path = Fixture::new(Some(
            case.get("database")
                .unwrap_or(corpus.get("database").unwrap()),
        ));
        let records = case
            .get("records")
            .and_then(Json::as_array)
            .map(|v| v.to_vec())
            .unwrap_or_else(|| {
                vec![Json::object([
                    (
                        "object",
                        Json::object([
                            ("id", "source".into()),
                            ("parentId", Json::Null),
                            ("name", "source".into()),
                            ("contractKey", "domain/source".into()),
                            ("effectivelyArchived", false.into()),
                            ("createdAt", crate_at()),
                        ]),
                    ),
                    (
                        "revision",
                        Json::object([
                            ("revision", Json::Number(1.0)),
                            ("contractVersion", "1.3.0".into()),
                            ("mediaType", "application/json".into()),
                        ]),
                    ),
                    (
                        "content",
                        Json::object([
                            ("encoding", "json".into()),
                            ("value", Json::object([("original", true.into())])),
                        ]),
                    ),
                ])]
            });
        let client = Arc::new(Client::new(records));
        let clock = Arc::new(Fixed(number(corpus, "now") as i64));
        if flag(case, "scopeCacheWarm") {
            ivy_sdk::operation_ids::runtime_epoch(client.clone())
                .await
                .unwrap();
        }
        let mut store =
            ChatStore::with_clock(client.clone(), None, Some(path.0.clone()), clock.clone())
                .await
                .unwrap();
        let mut admission = ChatAdmission::new(corpus.required("definition").unwrap()).unwrap();
        let control = Mutex::new(Control::default());
        let mut other: Option<Document> = None;
        let mut stale: Option<Document> = None;
        let mut last = case
            .get("initialWrite")
            .and_then(Json::as_f64)
            .unwrap_or(0.0) as i64;
        let mut mutations = HashMap::new();
        client.0.lock().unwrap().calls.clear();
        for (index, step) in array(case, "steps").iter().enumerate() {
            let action = text(step, "action");
            let output = if action == "reopen" {
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
            } else if action == "definition-change" {
                let mut definition = corpus.required("definition").unwrap().clone();
                let mut channels = array(&definition, "channels").to_vec();
                channels[0].insert("displayName", "Changed".into());
                definition.insert("channels", Json::Array(channels));
                admission = ChatAdmission::new(&definition).unwrap();
                result(Ok(Json::Null))
            } else {
                let ops = ChatOperations::new(&store, &admission).unwrap();
                let main = Main {
                    ops: &ops,
                    client: &client,
                    instructions: &instructions,
                    control: &control,
                    binding: &binding,
                    main: &main_pin,
                };
                let notices = ChatNotices::new(&ops, &main, &instructions);
                result(
                    async {
                        match action.as_str() {
                            "notify" => {
                                notices
                                    .notify(
                                        js(step, "caller"),
                                        corpus
                                            .required("requests")?
                                            .required(&text(step, "name"))?,
                                    )
                                    .await
                            }
                            "notify-raw" => {
                                notices
                                    .notify(&"producer".into(), step.required("request")?)
                                    .await
                            }
                            "begin" => {
                                let mut request = corpus
                                    .required("requests")?
                                    .required(&text(step, "name"))?
                                    .clone();
                                request.insert(
                                    "expectedBridge",
                                    admission.expected(&"producer".into()),
                                );
                                request.insert("channel", corpus.required("channel")?.clone());
                                Ok(ops.begin(&"producer".into(), &request).await?.value)
                            }
                            "claim" => {
                                let op = operation(&ops, "a").await?;
                                let doc = store
                                    .read(
                                        "chat-bridge/main",
                                        Reference::Current(&main_pin.object_id),
                                    )
                                    .await
                                    .map_err(|e| e.into_ivy())?;
                                update(
                                    &store,
                                    "chat-bridge/main",
                                    doc,
                                    &Json::object([("pendingAction", op.pin.json())]),
                                )
                                .await?;
                                Ok(Json::Null)
                            }
                            "recover" => {
                                notices.recover_pending().await?;
                                Ok(Json::Null)
                            }
                            "owner" => {
                                control.lock().unwrap().owner_refused = flag(step, "refused");
                                Ok(Json::Null)
                            }
                            "ensure" => {
                                control.lock().unwrap().publish_binding =
                                    flag(step, "publishBinding");
                                Ok(Json::Null)
                            }
                            "hold-pending" => {
                                control.lock().unwrap().hold_pending = flag(step, "value");
                                Ok(Json::Null)
                            }
                            "claim-contention" => {
                                control.lock().unwrap().claim_contention = flag(step, "value");
                                Ok(Json::Null)
                            }
                            "fault" => {
                                client.0.lock().unwrap().fault = step
                                    .get("when")
                                    .filter(|v| !v.is_null())
                                    .map(|v| v.as_str().unwrap());
                                Ok(Json::Null)
                            }
                            "patch-main" | "patch-operation" => {
                                let key = if action == "patch-main" {
                                    "chat-bridge/main"
                                } else {
                                    "chat-bridge/operation"
                                };
                                let doc = if action == "patch-main" {
                                    store
                                        .read(key, Reference::Current(&main_pin.object_id))
                                        .await
                                        .map_err(|e| e.into_ivy())?
                                } else {
                                    operation(&ops, "a").await?
                                };
                                update(&store, key, doc, step.required("patch")?).await?;
                                Ok(Json::Null)
                            }
                            "patch-input" => {
                                let doc = input(&store, "a").await?;
                                client.patch_input(&doc.pin.object_id, step.required("patch")?);
                                Ok(Json::Null)
                            }
                            "patch-reply" => {
                                let op = operation(&ops, "a").await?;
                                let selected = pin(op
                                    .value
                                    .required("outcome")?
                                    .required("reply")?
                                    .required("object")?);
                                let reply = store
                                    .read("chat-bridge/reply", Reference::Pinned(&selected))
                                    .await
                                    .map_err(|e| e.into_ivy())?;
                                update(&store, "chat-bridge/reply", reply, step.required("patch")?)
                                    .await?;
                                Ok(Json::Null)
                            }
                            "patch-ticket" => {
                                let doc = store
                                    .read(
                                        "chat-bridge/main",
                                        Reference::Current(&main_pin.object_id),
                                    )
                                    .await
                                    .map_err(|e| e.into_ivy())?;
                                let mut ticket = array(&doc.value, "queue")[0].clone();
                                for (k, v) in step.required("patch")?.as_object().unwrap() {
                                    ticket.insert(k.clone(), v.clone());
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
                            "source" => {
                                let mut state = client.0.lock().unwrap();
                                let index = state
                                    .records
                                    .iter()
                                    .position(|r| {
                                        r.required("object").unwrap().get("id")
                                            == Some(&Json::from("source"))
                                    })
                                    .unwrap();
                                if flag(step, "missing") {
                                    state.records.remove(index);
                                } else {
                                    let mut record = state.records[index].clone();
                                    if flag(step, "archived") {
                                        let mut o = record.required("object")?.clone();
                                        o.insert("effectivelyArchived", true.into());
                                        record.insert("object", o);
                                    }
                                    if flag(step, "pruned") {
                                        let mut r = record.required("revision")?.clone();
                                        r.insert("revision", Json::Number(2.0));
                                        record.insert("revision", r);
                                    }
                                    state.records[index] = record.clone();
                                    state.versions.get_mut("source").unwrap().insert(1, record);
                                }
                                Ok(Json::Null)
                            }
                            "begin-other" => {
                                let request = Json::object([
                                    ("action", "acknowledge".into()),
                                    (
                                        "operationId",
                                        format!(
                                            "chat:{}:other-action",
                                            number(corpus, "now") as i64
                                        )
                                        .into(),
                                    ),
                                    ("expectedBridge", admission.expected(&"producer".into())),
                                    ("offerId", "offer".into()),
                                    ("replyIds", Json::Array(vec!["reply".into()])),
                                ]);
                                other = Some(ops.begin(&"producer".into(), &request).await?);
                                Ok(Json::Null)
                            }
                            "claim-other" => {
                                let doc = store
                                    .read(
                                        "chat-bridge/main",
                                        Reference::Current(&main_pin.object_id),
                                    )
                                    .await
                                    .map_err(|e| e.into_ivy())?;
                                update(
                                    &store,
                                    "chat-bridge/main",
                                    doc,
                                    &Json::object([(
                                        "pendingAction",
                                        other.as_ref().unwrap().pin.json(),
                                    )]),
                                )
                                .await?;
                                Ok(Json::Null)
                            }
                            "retain-input" => {
                                stale = Some(input(&store, "a").await?);
                                Ok(Json::Null)
                            }
                            "finish-undelivered" => {
                                if flag(step, "stale") {
                                    notices.finish_undelivered(stale.as_ref().unwrap()).await?;
                                } else {
                                    notices
                                        .finish_undelivered(&input(&store, "a").await?)
                                        .await?;
                                }
                                Ok(Json::Null)
                            }
                            "finish-other" => {
                                notices
                                    .finish_undelivered(&Document {
                                        key: "chat-bridge/input".into(),
                                        pin: Pin {
                                            object_id: "synthetic-input".into(),
                                            revision: 1,
                                        },
                                        value: Json::object([
                                            (
                                                "identity",
                                                Json::object([(
                                                    "senderPrincipalId",
                                                    "producer".into(),
                                                )]),
                                            ),
                                            (
                                                "operationId",
                                                other
                                                    .as_ref()
                                                    .unwrap()
                                                    .value
                                                    .required("operationId")?
                                                    .clone(),
                                            ),
                                        ]),
                                        metadata: Json::Null,
                                        revision: Json::Null,
                                    })
                                    .await?;
                                Ok(Json::Null)
                            }
                            "notice" => {
                                notices
                                    .notice(
                                        js(step, "caller"),
                                        &Json::object([(
                                            "operationId",
                                            corpus
                                                .required("requests")?
                                                .required(&text(step, "name"))?
                                                .required("operationId")?
                                                .clone(),
                                        )]),
                                    )
                                    .await
                            }
                            "publish" => {
                                publish(&ops, &client, &text(step, "name"), false).await?;
                                Ok(Json::Null)
                            }
                            "publish-final" => {
                                publish(&ops, &client, "a", true).await?;
                                Ok(Json::Null)
                            }
                            "send-user" => {
                                let request = Json::object([
                                    ("action", "send".into()),
                                    (
                                        "operationId",
                                        format!("chat:{}:user", number(corpus, "now") as i64)
                                            .into(),
                                    ),
                                    ("expectedBridge", admission.expected(&"user".into())),
                                    ("channel", corpus.required("channel")?.clone()),
                                    ("messageId", "user-message".into()),
                                    ("expectedBinding", binding.json()),
                                    (
                                        "payload",
                                        Json::object([
                                            ("text", "User input".into()),
                                            ("images", Json::Array(vec![])),
                                        ]),
                                    ),
                                ]);
                                let value = ChatQueue::new(&ops, None)
                                    .send(&"user".into(), &request)
                                    .await?;
                                other = ops
                                    .find(&"user".into(), js(&request, "operationId"))
                                    .await?;
                                Ok(value)
                            }
                            "finish-with-reply" => {
                                let op = operation(&ops, &text(step, "name")).await?;
                                let other = operation(&ops, &text(step, "replyName")).await?;
                                let mut outcome = other.value.required("outcome")?.clone();
                                outcome.insert(
                                    "operationId",
                                    op.value.required("operationId")?.clone(),
                                );
                                ops.finish(&op.pin.object_id, &outcome).await?;
                                Ok(Json::Null)
                            }
                            _ => panic!("Unknown action {action}"),
                        }
                    }
                    .await,
                )
            };
            let actual = snapshot(
                &store,
                &admission,
                &client,
                &path,
                other.as_ref(),
                output,
                &mut last,
                &mut mutations,
            )
            .await;
            let expected = &array(case, "expected")[index];
            for part in ["output", "main", "operations", "records", "writes", "calls"] {
                equal(
                    actual.required(part).unwrap(),
                    expected.required(part).unwrap(),
                    &format!("{} step {index} {part}", text(case, "label")),
                );
            }
        }
        store.close().await.unwrap();
    }
}

#[tokio::test]
async fn competing_notice_owners_preserve_original_claims_and_capacity() {
    let corpus = oracle();
    let admission = ChatAdmission::new(corpus.required("definition").unwrap()).unwrap();
    let instructions = instructions();
    let main_pin = pin(corpus.required("mainPin").unwrap());
    let binding = pin(corpus.required("binding").unwrap());
    for case in array(corpus, "races") {
        let path = Fixture::new(Some(corpus.required("database").unwrap()));
        let record = Json::object([
            (
                "object",
                Json::object([
                    ("id", "source".into()),
                    ("parentId", Json::Null),
                    ("name", "source".into()),
                    ("contractKey", "domain/source".into()),
                    ("effectivelyArchived", false.into()),
                    ("createdAt", crate_at()),
                ]),
            ),
            (
                "revision",
                Json::object([
                    ("revision", Json::Number(1.0)),
                    ("contractVersion", "1.3.0".into()),
                    ("mediaType", "application/json".into()),
                ]),
            ),
            (
                "content",
                Json::object([
                    ("encoding", "json".into()),
                    ("value", Json::object([("original", true.into())])),
                ]),
            ),
        ]);
        let client = Arc::new(Client::new(vec![record]));
        let clock = Arc::new(Fixed(number(corpus, "now") as i64));
        let mut stores = Vec::new();
        for _ in 0..2 {
            stores.push(
                ChatStore::with_clock(client.clone(), None, Some(path.0.clone()), clock.clone())
                    .await
                    .unwrap(),
            );
        }
        let actual = {
            let operations = stores
                .iter()
                .map(|s| ChatOperations::new(s, &admission).unwrap())
                .collect::<Vec<_>>();
            let control = Mutex::new(Control::default());
            let mains = operations
                .iter()
                .map(|ops| Main {
                    ops,
                    client: &client,
                    instructions: &instructions,
                    control: &control,
                    binding: &binding,
                    main: &main_pin,
                })
                .collect::<Vec<_>>();
            let notices = mains
                .iter()
                .enumerate()
                .map(|(i, m)| ChatNotices::new(&operations[i], m, &instructions))
                .collect::<Vec<_>>();
            if flag(case, "filled") {
                let main = stores[0]
                    .read("chat-bridge/main", Reference::Current(&main_pin.object_id))
                    .await
                    .unwrap();
                update(
                    &stores[0],
                    "chat-bridge/main",
                    main,
                    &Json::object([
                        ("queue", Json::Array(array(corpus, "full")[..63].to_vec())),
                        ("nextSequence", Json::Number(64.0)),
                    ]),
                )
                .await
                .unwrap();
            }
            for (i, name) in array(case, "names").iter().enumerate() {
                let mut request = corpus
                    .required("requests")
                    .unwrap()
                    .required(&name.as_str().unwrap())
                    .unwrap()
                    .clone();
                request.insert("expectedBridge", admission.expected(&"producer".into()));
                request.insert("channel", corpus.required("channel").unwrap().clone());
                operations[i]
                    .begin(&"producer".into(), &request)
                    .await
                    .unwrap();
            }
            let caller = JsString::from("producer");
            let a = corpus
                .required("requests")
                .unwrap()
                .required(&array(case, "names")[0].as_str().unwrap())
                .unwrap();
            let b = corpus
                .required("requests")
                .unwrap()
                .required(&array(case, "names")[1].as_str().unwrap())
                .unwrap();
            let (a, b) = tokio::join!(notices[0].notify(&caller, a), notices[1].notify(&caller, b));
            let replies = [a.unwrap(), b.unwrap()];
            notices[0].recover_pending().await.unwrap();
            let main = ChatQueue::new(&operations[0], None).main().await.unwrap();
            let mut phases = replies.iter().map(|r| text(r, "phase")).collect::<Vec<_>>();
            phases.sort();
            let mut errors = replies
                .iter()
                .filter(|r| r.get("phase") == Some(&Json::from("failed")))
                .map(|r| text(r.required("error").unwrap(), "code"))
                .collect::<Vec<_>>();
            errors.sort();
            Json::object([
                (
                    "phases",
                    Json::Array(phases.into_iter().map(Json::from).collect()),
                ),
                (
                    "errors",
                    Json::Array(errors.into_iter().map(Json::from).collect()),
                ),
                (
                    "queueLength",
                    Json::Number(array(&main.value, "queue").len() as f64),
                ),
                (
                    "nextSequence",
                    main.value.required("nextSequence").unwrap().clone(),
                ),
                (
                    "pending",
                    (!main.value.required("pendingAction").unwrap().is_null()).into(),
                ),
                (
                    "inputCount",
                    Json::Number(
                        client
                            .0
                            .lock()
                            .unwrap()
                            .records
                            .iter()
                            .filter(|r| {
                                r.required("object").unwrap().get("contractKey")
                                    == Some(&Json::from("chat-bridge/input"))
                            })
                            .count() as f64,
                    ),
                ),
            ])
        };
        for store in stores {
            store.close().await.unwrap();
        }
        equal(
            &actual,
            case.required("expected").unwrap(),
            &text(case, "label"),
        );
    }
}
