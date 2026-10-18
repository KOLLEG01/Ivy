use base64::{Engine, engine::general_purpose::STANDARD};
#[path = "chat-publication-cases.rs"]
mod publication;
use ivy_chat_bridge::{
    admission::ChatAdmission,
    operations::ChatOperations,
    outbox::ChatOutbox,
    store::{ChatStore, Document, Reference},
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
    sync::{Arc, Mutex},
    time::Instant,
};

fn field<'a>(v: &'a Json, k: &str) -> &'a Json {
    v.get(k).unwrap()
}
fn text(v: &Json, k: &str) -> String {
    field(v, k).as_str().unwrap()
}
fn array<'a>(v: &'a Json, k: &str) -> &'a [Json] {
    field(v, k).as_array().unwrap()
}
fn number(v: &Json, k: &str) -> f64 {
    field(v, k).as_f64().unwrap()
}
fn js<'a>(v: &'a Json, k: &str) -> &'a JsString {
    field(v, k).as_js_string().unwrap()
}
fn view(d: &Document) -> Json {
    Json::object([("object", d.pin.json()), ("data", d.value.clone())])
}
fn result(v: IvyResult<Json>) -> Json {
    match v {
        Ok(v) => Json::object([("value", v)]),
        Err(e) => Json::object([(
            "error",
            Json::object([
                ("code", e.code.into()),
                ("message", e.message.into()),
                ("outcome", e.outcome.as_str().into()),
            ]),
        )]),
    }
}
struct Time(i64);
impl Clock for Time {
    fn utc_millis(&self) -> i64 {
        self.0
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
struct Fixture(PathBuf);
impl Fixture {
    fn new(database: &Json) -> Self {
        let path = std::env::temp_dir().join(format!("ivy-chat-outbox-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(
            path.join("chat-bridge.sqlite"),
            STANDARD.decode(database.as_str().unwrap()).unwrap(),
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
struct State {
    records: Vec<Json>,
    versions: HashMap<String, HashMap<i64, Json>>,
    calls: Vec<Json>,
    fault: Option<Json>,
    cursor: Option<Json>,
}
struct Client(Mutex<State>);
fn pointer<'a>(record: &'a Json, path: &str) -> Option<&'a Json> {
    let (mut value, path, separator) = if let Some(path) = path.strip_prefix("data:/") {
        (field(field(record, "content"), "value"), path, '/')
    } else {
        (record, path, '.')
    };
    for key in path.split(separator) {
        value = value.get(key)?;
    }
    Some(value)
}
fn matches(record: &Json, p: &Json) -> bool {
    let value = p
        .get("field")
        .map(|v| pointer(record, &v.as_str().unwrap()))
        .flatten();
    match text(p, "op").as_str() {
        "and" => array(p, "args").iter().all(|p| matches(record, p)),
        "or" => array(p, "args").iter().any(|p| matches(record, p)),
        "isNull" => value == Some(&Json::Null),
        "eq" => value == p.get("value"),
        "ne" => value != p.get("value"),
        "gt" => value.and_then(Json::as_f64) > p.get("value").and_then(Json::as_f64),
        "lte" => value.and_then(Json::as_f64) <= p.get("value").and_then(Json::as_f64),
        _ => panic!("Unsupported predicate"),
    }
}
impl Client {
    fn new(records: Vec<Json>) -> Self {
        let versions = records
            .iter()
            .map(|r| {
                (
                    text(field(r, "object"), "id"),
                    HashMap::from([(number(field(r, "revision"), "revision") as i64, r.clone())]),
                )
            })
            .collect();
        Self(Mutex::new(State {
            records,
            versions,
            calls: vec![],
            fault: None,
            cursor: None,
        }))
    }
    fn execute(&self, method: &str, params: Json) -> IvyResult<Json> {
        let mut state = self.0.lock().unwrap();
        let mut logged = params.clone();
        if method == "objects.write" {
            assert_eq!(
                parse_operation_id(&text(&params, "mutationId"))?.runtime_epoch,
                "chat"
            );
            logged.insert("mutationId", "<scoped>".into());
        }
        state.calls.push(Json::object([
            ("method", method.into()),
            ("params", logged),
        ]));
        if method == "system.status" {
            return Ok(Json::object([("runtimeEpoch", "chat".into())]));
        }
        if method == "objects.query" {
            let mut selected: Vec<_> = state
                .records
                .iter()
                .filter(|r| {
                    field(r, "object").get("contractKey") == params.get("contractKey")
                        && (params.get("includeArchived") == Some(&Json::Bool(true))
                            || field(r, "object").get("effectivelyArchived")
                                != Some(&Json::Bool(true)))
                        && matches(r, field(&params, "where"))
                })
                .collect();
            for sort in params
                .get("orderBy")
                .and_then(Json::as_array)
                .unwrap_or(&[])
                .iter()
                .rev()
            {
                let path = text(sort, "field");
                selected.sort_by(|a, b| match (pointer(a, &path), pointer(b, &path)) {
                    (Some(Json::Number(a)), Some(Json::Number(b))) => a.total_cmp(b),
                    (Some(Json::String(a)), Some(Json::String(b))) => a.cmp(b),
                    _ => std::cmp::Ordering::Equal,
                });
            }
            let offset = params
                .get("cursor")
                .map(|v| v.as_str().unwrap().parse::<usize>().unwrap())
                .unwrap_or(0);
            let limit = number(&params, "limit") as usize;
            let items = selected
                .iter()
                .skip(offset)
                .take(limit)
                .map(|r| {
                    Json::object([
                        ("objectId", field(field(r, "object"), "id").clone()),
                        ("revision", field(field(r, "revision"), "revision").clone()),
                    ])
                })
                .collect();
            let cursor = state.cursor.clone().unwrap_or_else(|| {
                if offset + limit < selected.len() {
                    (offset + limit).to_string().into()
                } else {
                    Json::Null
                }
            });
            return Ok(Json::object([
                ("items", Json::Array(items)),
                ("nextCursor", cursor),
            ]));
        }
        let id = text(&params, "objectId");
        let Some(index) = state
            .records
            .iter()
            .position(|r| field(r, "object").get("id") == params.get("objectId"))
        else {
            return Err(IvyError::new(
                "object_missing",
                "Fixture object is missing.",
            ));
        };
        if method == "objects.read" {
            return match params.get("revision") {
                None => Ok(state.records[index].clone()),
                Some(revision) => state
                    .versions
                    .get(&id)
                    .and_then(|v| v.get(&(revision.as_f64().unwrap() as i64)))
                    .cloned()
                    .ok_or_else(|| {
                        IvyError::new("revision_pruned", "Fixture revision was pruned.")
                    }),
            };
        }
        assert_eq!(method, "objects.write");
        let applies = state.fault.as_ref().is_some_and(|f| {
            f.get("objectId") == params.get("objectId")
                && f.get("state")
                    .map(|s| Some(s) == field(field(&params, "content"), "value").get("state"))
                    .unwrap_or(true)
        });
        if applies {
            let fault = state.fault.as_mut().unwrap();
            match text(fault, "kind").as_str() {
                "contention" => {
                    let n = number(fault, "remaining");
                    fault.insert("remaining", Json::Number(n - 1.0));
                    if n > 0.0 {
                        return Err(IvyError::new(
                            "revision_conflict",
                            "Fixture revision changed.",
                        ));
                    }
                }
                "before" => {
                    state.fault = None;
                    return Err(IvyError::new(
                        "fixture_unavailable",
                        "Fixture write was not executed.",
                    ));
                }
                _ => {}
            }
        }
        let mut record = state.records[index].clone();
        let mut revision = field(&record, "revision").clone();
        if revision.get("revision") != params.get("expectedRevision") {
            return Err(IvyError::new(
                "revision_conflict",
                "Fixture revision changed.",
            ));
        }
        let next = number(&revision, "revision") + 1.0;
        revision.insert("revision", Json::Number(next));
        record.insert("revision", revision);
        record.insert("content", field(&params, "content").clone());
        state.records[index] = record.clone();
        state
            .versions
            .get_mut(&id)
            .unwrap()
            .insert(next as i64, record.clone());
        if applies
            && state
                .fault
                .as_ref()
                .is_some_and(|f| f.get("kind") == Some(&Json::from("after")))
        {
            state.fault = None;
            return Err(IvyError::new(
                "outcome_unknown",
                "Fixture reply was lost after the write committed.",
            )
            .with_outcome(Outcome::Unknown));
        }
        Ok(record)
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
            let result = self.execute(method, params);
            tokio::task::yield_now().await;
            result
        })
    }
}
type Requests = Vec<(JsString, Json)>;
fn remember(requests: &mut Requests, name: JsString, request: Json) {
    if let Some(entry) = requests.iter_mut().find(|(k, _)| k == &name) {
        entry.1 = request;
    } else {
        requests.push((name, request));
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
        Json::Object(fields) => {
            let mut value = Json::Object(
                fields
                    .iter()
                    .map(|(k, v)| (k.clone(), normalize(v, ids)))
                    .collect(),
            );
            if value.get("requestHash").is_some() && value.get("request").is_some() {
                value.insert(
                    "requestHash",
                    hash_json(field(&value, "request")).unwrap().into(),
                );
            }
            value
        }
        _ => value.clone(),
    }
}
async fn snapshot(
    store: &ChatStore,
    admission: &ChatAdmission,
    client: &Client,
    requests: &Requests,
    output: Json,
    path: &Path,
) -> Json {
    let ops = ChatOperations::new(store, admission).unwrap();
    let mut documents = Vec::new();
    let mut ids = HashMap::new();
    for (name, request) in requests {
        let op = ops
            .find(
                js(field(request, "expectedBridge"), "callerPrincipalId"),
                js(request, "operationId"),
            )
            .await
            .unwrap();
        let Some(op) = op else {
            continue;
        };
        let label = name.to_utf8().unwrap();
        ids.insert(op.pin.object_id.clone(), format!("operation:{label}"));
        documents.push(Json::object([
            ("name", Json::String(name.clone())),
            ("kind", "operation".into()),
            ("object", op.pin.json()),
            ("data", op.value),
        ]));
        for (kind, prefix) in [
            ("offer", "Chat offer "),
            ("acknowledgement", "Chat acknowledgement "),
        ] {
            if let Some(doc) = store
                .named(
                    &format!("chat-bridge/{kind}"),
                    &format!("{prefix}{}", op.pin.object_id),
                )
                .await
                .unwrap()
            {
                ids.insert(doc.pin.object_id.clone(), format!("{kind}:{label}"));
                documents.push(Json::object([
                    ("name", Json::String(name.clone())),
                    ("kind", kind.into()),
                    ("object", doc.pin.json()),
                    ("data", doc.value),
                ]));
            }
        }
    }
    let state = client.0.lock().unwrap();
    let mut calls = Vec::new();
    let mut batch = Vec::new();
    let flush = |batch: &mut Vec<Json>, calls: &mut Vec<Json>| {
        batch.sort_by_cached_key(|call| JsString::from(canonical(call, usize::MAX).unwrap()));
        calls.append(batch);
    };
    for call in &state.calls {
        if ["objects.query", "objects.read"].contains(&text(call, "method").as_str()) {
            batch.push(call.clone());
        } else {
            flush(&mut batch, &mut calls);
            calls.push(call.clone());
        }
    }
    flush(&mut batch, &mut calls);
    let db = rusqlite::Connection::open_with_flags(
        path.join("chat-bridge.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let markers = db
        .prepare("SELECT key,value_json FROM markers ORDER BY key")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .map(|row| {
            let (key, value) = row.unwrap();
            Json::object([
                ("key", key.into()),
                ("value", parse(&value, usize::MAX).unwrap()),
            ])
        })
        .collect();
    let acknowledgements = db
        .prepare("SELECT offer_id,reply_id FROM offer_acknowledgements")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .map(|row| {
            let (offer, reply) = row.unwrap();
            Json::object([("offer_id", offer.into()), ("reply_id", reply.into())])
        })
        .collect();
    let mut data = normalize(
        &Json::object([
            ("output", output),
            ("documents", Json::Array(documents)),
            ("records", Json::Array(state.records.clone())),
            ("calls", Json::Array(calls)),
            ("markers", Json::Array(markers)),
            ("acknowledgements", Json::Array(acknowledgements)),
        ]),
        &ids,
    );
    let mut acknowledgements = array(&data, "acknowledgements").to_vec();
    acknowledgements.sort_by_cached_key(|row| JsString::from(canonical(row, usize::MAX).unwrap()));
    data.insert("acknowledgements", Json::Array(acknowledgements));
    data
}
async fn offer(
    store: &ChatStore,
    admission: &ChatAdmission,
    now: i64,
    name: &str,
) -> IvyResult<Document> {
    let ops = ChatOperations::new(store, admission)?;
    let op = ops
        .find(&"alice".into(), &format!("chat:{now}:{name}").into())
        .await?
        .ok_or_else(IvyError::internal)?;
    store
        .named(
            "chat-bridge/offer",
            &format!("Chat offer {}", op.pin.object_id),
        )
        .await
        .map_err(|e| e.into_ivy())?
        .ok_or_else(IvyError::internal)
}
fn difference(actual: &Json, expected: &Json, path: &str) -> Option<String> {
    match (actual, expected) {
        (Json::Object(a), Json::Object(b)) => {
            if a.len() != b.len() {
                return Some(format!(
                    "{path}: object field count {} != {}",
                    a.len(),
                    b.len()
                ));
            }
            for (k, a) in a {
                let Some(b) = b.iter().find(|(name, _)| name == k).map(|(_, v)| v) else {
                    return Some(format!("{path}: missing key {k:?}"));
                };
                if let Some(diff) =
                    difference(a, b, &format!("{path}.{}", String::from_utf16_lossy(&k.0)))
                {
                    return Some(diff);
                }
            }
            None
        }
        (Json::Array(a), Json::Array(b)) => {
            if a.len() != b.len() {
                return Some(format!("{path}: array count {} != {}", a.len(), b.len()));
            }
            a.iter()
                .zip(b)
                .enumerate()
                .find_map(|(i, (a, b))| difference(a, b, &format!("{path}[{i}]")))
        }
        _ if actual == expected => None,
        _ => Some(format!(
            "{path}: actual={} expected={}",
            canonical(actual, usize::MAX)
                .unwrap()
                .chars()
                .take(600)
                .collect::<String>(),
            canonical(expected, usize::MAX)
                .unwrap()
                .chars()
                .take(600)
                .collect::<String>()
        )),
    }
}
#[tokio::test]
async fn immutable_offers_history_and_authenticated_confirmation_match_original() {
    let output = Command::new("node")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .arg("tests/rust/fixtures/chat-outbox-oracle.mjs")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Qualification can retain the exact synthetic SQLite/oracle bytes used by
    // this run without creating a persistent test output directory by default.
    if let Some(path) = std::env::var_os("IVY_CHAT_OUTBOX_ORACLE_CAPTURE") {
        std::fs::write(path, &output.stdout).unwrap();
    }
    let fixture = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    let now = number(&fixture, "now") as i64;
    for case in array(&fixture, "cases") {
        let path = Fixture::new(
            case.get("database")
                .unwrap_or_else(|| field(&fixture, "database")),
        );
        let client = Arc::new(Client::new(array(case, "records").to_vec()));
        // Original setup can retain the authenticated client's epoch cache even
        // when its SQLite store is reopened. Recreate that same fixture state.
        if case.get("scopeCacheWarm") == Some(&Json::Bool(true)) {
            ivy_sdk::operation_ids::scoped_operation_id(
                client.clone(),
                &Json::Array(vec!["chat".into(), "fixture-cache".into()]),
            )
            .await
            .unwrap();
            client.0.lock().unwrap().calls.clear();
        }
        if let Some(versions) = case.get("versions") {
            client.0.lock().unwrap().versions = versions
                .as_object()
                .unwrap()
                .iter()
                .map(|(id, versions)| {
                    (
                        id.to_utf8().unwrap(),
                        versions
                            .as_object()
                            .unwrap()
                            .iter()
                            .map(|(revision, record)| {
                                (revision.to_utf8().unwrap().parse().unwrap(), record.clone())
                            })
                            .collect(),
                    )
                })
                .collect();
        }
        let definition = field(case, "definition");
        let admission = ChatAdmission::new(definition).unwrap();
        let root = definition
            .get("rootObjectId")
            .filter(|v| !v.is_null())
            .map(|v| v.as_str().unwrap());
        let time = Arc::new(Time(now));
        let mut store = ChatStore::with_clock(
            client.clone(),
            root.clone(),
            Some(path.0.clone()),
            time.clone(),
        )
        .await
        .unwrap();
        let mut requests = case
            .get("requests")
            .and_then(Json::as_object)
            .unwrap_or(&[])
            .to_vec();
        let mut parts = vec![];
        for (index, step) in array(case, "steps").iter().enumerate() {
            let action = text(step, "action");
            let output = if action == "reopen" {
                store.close().await.unwrap();
                store = ChatStore::with_clock(
                    client.clone(),
                    root.clone(),
                    Some(path.0.clone()),
                    time.clone(),
                )
                .await
                .unwrap();
                result(Ok(Json::Null))
            } else {
                result(
                    async {
                        let ops = ChatOperations::new(&store, &admission)?;
                        let outbox = ChatOutbox::new(&ops);
                        let caller = step
                            .get("caller")
                            .and_then(Json::as_js_string)
                            .cloned()
                            .unwrap_or_else(|| "alice".into());
                        match action.as_str() {
                            "fault" => {
                                client.0.lock().unwrap().fault = Some(field(step, "fault").clone());
                                Ok(Json::Null)
                            }
                            "cursor" => {
                                client.0.lock().unwrap().cursor =
                                    Some(field(step, "value").clone());
                                Ok(Json::Null)
                            }
                            "prune" => {
                                let mut state = client.0.lock().unwrap();
                                state.versions = state
                                    .records
                                    .iter()
                                    .map(|r| {
                                        (
                                            text(field(r, "object"), "id"),
                                            HashMap::from([(
                                                number(field(r, "revision"), "revision") as i64,
                                                r.clone(),
                                            )]),
                                        )
                                    })
                                    .collect();
                                Ok(Json::Null)
                            }
                            "alter-reply" => {
                                let mut state = client.0.lock().unwrap();
                                let selected = state
                                    .records
                                    .iter_mut()
                                    .find(|r| {
                                        field(r, "object").get("id") == Some(&Json::from("reply-a"))
                                    })
                                    .unwrap();
                                let mut value = field(field(selected, "content"), "value").clone();
                                value.insert("text", "Different immutable reply".into());
                                let mut immutable = value.clone();
                                for key in
                                    ["state", "firstOfferedAt", "confirmedAt", "immutableHash"]
                                {
                                    immutable.remove(key);
                                }
                                value.insert("immutableHash", hash_json(&immutable)?.into());
                                selected.insert(
                                    "content",
                                    Json::object([("encoding", "json".into()), ("value", value)]),
                                );
                                let selected = selected.clone();
                                state.versions.get_mut("reply-a").unwrap().insert(
                                    number(field(&selected, "revision"), "revision") as i64,
                                    selected,
                                );
                                Ok(Json::Null)
                            }
                            "reply" => Ok(view(
                                &outbox
                                    .reply(
                                        Reference::Current(&text(step, "id")),
                                        field(&fixture, "channel"),
                                    )
                                    .await?,
                            )),
                            "parts" => {
                                let first = outbox
                                    .reply(
                                        Reference::Current("reply-a"),
                                        field(&fixture, "channel"),
                                    )
                                    .await?;
                                parts = outbox.notice_parts(&first).await?;
                                Ok(Json::Array(parts.iter().map(view).collect()))
                            }
                            "confirm" => {
                                outbox
                                    .confirm_notice(
                                        if step.get("empty") == Some(&Json::Bool(true)) {
                                            &[]
                                        } else {
                                            &parts
                                        },
                                    )
                                    .await?;
                                Ok(Json::Null)
                            }
                            "raw" => match text(step, "method").as_str() {
                                "history" => outbox.history(&caller, field(step, "request")).await,
                                "receive" => outbox.receive(&caller, field(step, "request")).await,
                                "acknowledge" => {
                                    outbox.acknowledge(&caller, field(step, "request")).await
                                }
                                _ => unreachable!(),
                            },
                            "pending" => {
                                let pending = outbox.pending_notices(None).await?;
                                let replies = pending
                                    .replies
                                    .into_iter()
                                    .map(|doc| {
                                        Json::object([
                                            ("key", doc.key.into()),
                                            ("pin", doc.pin.json()),
                                            ("value", doc.value),
                                            ("metadata", doc.metadata),
                                            ("revision", doc.revision),
                                        ])
                                    })
                                    .collect();
                                let mut output = Json::object([("replies", Json::Array(replies))]);
                                if let Some(cursor) = pending.next_cursor {
                                    output.insert("nextCursor", cursor);
                                }
                                Ok(output)
                            }
                            "history" | "receive" => {
                                let after = if let Some(name) = step.get("afterOffer") {
                                    field(
                                        &offer(&store, &admission, now, &name.as_str()?)
                                            .await?
                                            .value,
                                        "throughSequence",
                                    )
                                    .clone()
                                } else {
                                    step.get("after").cloned().unwrap_or(Json::Number(0.0))
                                };
                                let mut request = Json::object([
                                    ("expectedBridge", admission.expected(&caller)),
                                    ("channel", field(&fixture, "channel").clone()),
                                    ("afterSequence", after),
                                    (
                                        "limit",
                                        step.get("limit").cloned().unwrap_or(Json::Number(8.0)),
                                    ),
                                ]);
                                if step.get("bad") == Some(&Json::from("channel")) {
                                    let mut channel = field(&fixture, "channel").clone();
                                    channel.insert("channelId", "foreign".into());
                                    request.insert("channel", channel);
                                }
                                if step.get("bad") == Some(&Json::from("expectedBridge")) {
                                    let mut bridge = admission.expected(&caller);
                                    bridge.insert("callerPrincipalId", "foreign".into());
                                    request.insert("expectedBridge", bridge);
                                }
                                if action == "history" {
                                    outbox.history(&caller, &request).await
                                } else {
                                    let name = js(step, "name").clone();
                                    request.insert("action", "receive".into());
                                    request.insert(
                                        "operationId",
                                        format!("chat:{now}:{}", name.to_utf8()?).into(),
                                    );
                                    remember(&mut requests, name, request.clone());
                                    outbox.receive(&caller, &request).await
                                }
                            }
                            "acknowledge" => {
                                let offered =
                                    offer(&store, &admission, now, &text(step, "offer")).await?;
                                let name = js(step, "name").clone();
                                let request = Json::object([
                                    ("action", "acknowledge".into()),
                                    (
                                        "operationId",
                                        format!("chat:{now}:{}", name.to_utf8()?).into(),
                                    ),
                                    ("expectedBridge", admission.expected(&caller)),
                                    ("offerId", offered.pin.object_id.into()),
                                    ("replyIds", field(step, "ids").clone()),
                                ]);
                                remember(&mut requests, name, request.clone());
                                outbox.acknowledge(&caller, &request).await
                            }
                            _ => panic!("Bad step {action}"),
                        }
                    }
                    .await,
                )
            };
            let actual = snapshot(&store, &admission, &client, &requests, output, &path.0).await;
            let expected = &array(case, "expected")[index];
            if hash_json(&actual).unwrap() != text(expected, "hash") {
                if let Some(expected) = expected.get("snapshot") {
                    panic!(
                        "{} step{index} {action}: {}",
                        text(case, "label"),
                        difference(&actual, expected, "$")
                            .unwrap_or_else(|| "hash mismatch".into())
                    );
                }
                let hashes = Json::Object(
                    actual
                        .as_object()
                        .unwrap()
                        .iter()
                        .map(|(key, value)| (key.clone(), hash_json(value).unwrap().into()))
                        .collect(),
                );
                panic!(
                    "{} step{index} {action}: compact snapshot mismatch: {}",
                    text(case, "label"),
                    difference(&hashes, field(expected, "hashes"), "$").unwrap()
                );
            }
        }
        store.close().await.unwrap();
    }
    for race in array(&fixture, "races") {
        let path = Fixture::new(field(&fixture, "database"));
        let client = Arc::new(Client::new(array(race, "records").to_vec()));
        let admission = ChatAdmission::new(field(&fixture, "definition")).unwrap();
        let time = Arc::new(Time(now));
        let first = ChatStore::with_clock(client.clone(), None, Some(path.0.clone()), time.clone())
            .await
            .unwrap();
        let second = ChatStore::with_clock(client.clone(), None, Some(path.0.clone()), time)
            .await
            .unwrap();
        {
            let first_ops = ChatOperations::new(&first, &admission).unwrap();
            let second_ops = ChatOperations::new(&second, &admission).unwrap();
            let first_box = ChatOutbox::new(&first_ops);
            let second_box = ChatOutbox::new(&second_ops);
            let caller = JsString::from("alice");
            let kind = text(race, "kind");
            let mut requests = array(race, "requests").to_vec();
            if kind.starts_with("ack") {
                let original = Json::object([
                    ("action", "receive".into()),
                    ("operationId", format!("chat:{now}:original").into()),
                    ("expectedBridge", admission.expected(&caller)),
                    ("channel", field(&fixture, "channel").clone()),
                    ("afterSequence", Json::Number(0.0)),
                    ("limit", Json::Number(8.0)),
                ]);
                let received = first_box.receive(&caller, &original).await.unwrap();
                let id = field(field(field(&received, "outcome"), "offer"), "object")
                    .required("objectId")
                    .unwrap()
                    .clone();
                for request in &mut requests {
                    request.insert("offerId", id.clone());
                }
            }
            let (a, b) = if kind.starts_with("receive") {
                tokio::join!(
                    first_box.receive(&caller, &requests[0]),
                    second_box.receive(&caller, &requests[1])
                )
            } else {
                tokio::join!(
                    first_box.acknowledge(&caller, &requests[0]),
                    second_box.acknowledge(&caller, &requests[1])
                )
            };
            let successes = [&a, &b].iter().filter(|r| r.is_ok()).count();
            let mut errors = [&a, &b]
                .iter()
                .filter_map(|r| r.as_ref().err().map(|e| e.code.clone()))
                .collect::<Vec<_>>();
            errors.sort();
            let one = first_ops
                .find(&caller, js(&requests[0], "operationId"))
                .await
                .unwrap()
                .unwrap();
            let two = second_ops
                .find(&caller, js(&requests[1], "operationId"))
                .await
                .unwrap()
                .unwrap();
            let mut phases = vec![
                field(&one.value, "phase").clone(),
                field(&two.value, "phase").clone(),
            ];
            phases.sort_by_cached_key(|v| v.as_str().unwrap());
            let states = client
                .0
                .lock()
                .unwrap()
                .records
                .iter()
                .filter(|r| {
                    field(r, "object").get("contractKey") == Some(&Json::from("chat-bridge/reply"))
                })
                .map(|r| {
                    Json::object([
                        ("id", field(field(r, "object"), "id").clone()),
                        (
                            "state",
                            field(field(field(r, "content"), "value"), "state").clone(),
                        ),
                    ])
                })
                .collect();
            let actual = Json::object([
                ("successes", Json::Number(successes as f64)),
                (
                    "errors",
                    Json::Array(errors.into_iter().map(Json::from).collect()),
                ),
                ("phases", Json::Array(phases)),
                (
                    "sameOperation",
                    (one.pin.object_id == two.pin.object_id).into(),
                ),
                ("states", Json::Array(states)),
            ]);
            if let Some(diff) = difference(&actual, field(race, "expected"), "$") {
                panic!("{kind}: {diff}");
            }
        }
        first.close().await.unwrap();
        second.close().await.unwrap();
    }
}
