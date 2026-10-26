#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use super::{Fixture, Time, array, difference, field, js, matches, number, pointer, result, text};
use ivy_chat_bridge::{
    admission::ChatAdmission,
    operations::ChatOperations,
    outbox::ChatOutbox,
    publisher::{ChatPublisher, PublicationFuture, PublicationSource},
    queue::ChatQueue,
    store::{ChatStore, Destination, Document, Reference},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    hash::digest,
    json::{canonical, parse},
    operation::parse_operation_id,
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{
    collections::HashMap,
    path::Path,
    process::Command,
    sync::{Arc, Mutex, OnceLock},
};

fn flag(v: &Json, k: &str) -> bool {
    v.get(k) == Some(&Json::Bool(true))
}
fn empty() -> Json {
    Json::Object(vec![])
}
fn member_mut<'a>(v: &'a mut Json, key: &str) -> &'a mut Json {
    let Json::Object(pairs) = v else {
        panic!("Expected object");
    };
    &mut pairs
        .iter_mut()
        .find(|(k, _)| k == &JsString::from(key))
        .unwrap()
        .1
}
fn merge(value: &mut Json, patch: &Json) {
    for (key, v) in patch.as_object().unwrap() {
        value.insert(key.clone(), v.clone());
    }
}
fn oracle() -> &'static Json {
    static ORACLE: OnceLock<Json> = OnceLock::new();
    ORACLE.get_or_init(|| {
        let output = reference_data::read(include_bytes!("fixtures/chat-publication-cases.json.gz"));
        if let Some(path) = std::env::var_os("IVY_CHAT_PUBLICATION_ORACLE_CAPTURE") {
            std::fs::write(path, &output).unwrap();
        }
        parse(std::str::from_utf8(&output).unwrap(), usize::MAX).unwrap()
    })
}
struct State {
    records: Vec<Json>,
    versions: HashMap<String, HashMap<i64, Json>>,
    calls: Vec<Json>,
    fault: Option<Json>,
}
struct Client(Mutex<State>);
impl Client {
    fn new(records: &[Json]) -> Self {
        Self(Mutex::new(State {
            records: records.to_vec(),
            versions: records
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
                .collect(),
            calls: vec![],
            fault: None,
        }))
    }
    fn trace(&self, method: &str, params: Json) {
        self.0.lock().unwrap().calls.push(Json::object([
            ("method", method.into()),
            ("params", params),
        ]));
    }
    fn patch(&self, id: &str, patch: &Json) {
        let mut s = self.0.lock().unwrap();
        let record = s
            .records
            .iter_mut()
            .find(|r| field(r, "object").get("id") == Some(&id.into()))
            .unwrap();
        let revision = number(field(record, "revision"), "revision") + 1.0;
        member_mut(record, "revision").insert("revision", Json::Number(revision));
        merge(member_mut(member_mut(record, "content"), "value"), patch);
        let saved = record.clone();
        s.versions
            .get_mut(id)
            .unwrap()
            .insert(revision as i64, saved);
    }
    fn execute(&self, method: &str, params: Json) -> IvyResult<Json> {
        if method == "objects.write" {
            assert_eq!(
                parse_operation_id(&text(&params, "mutationId"))?.runtime_epoch,
                "chat"
            );
        }
        self.trace(method, params.clone());
        let mut s = self.0.lock().unwrap();
        if method == "system.status" {
            return Ok(Json::object([("runtimeEpoch", "chat".into())]));
        }
        if method == "objects.query" {
            let mut selected: Vec<_> = s
                .records
                .iter()
                .filter(|r| {
                    field(r, "object").get("contractKey") == params.get("contractKey")
                        && (flag(&params, "includeArchived")
                            || !flag(field(r, "object"), "effectivelyArchived"))
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
            return Ok(Json::object([
                (
                    "items",
                    Json::Array(
                        selected
                            .iter()
                            .skip(offset)
                            .take(limit)
                            .map(|r| {
                                Json::object([
                                    ("objectId", field(field(r, "object"), "id").clone()),
                                    ("revision", field(field(r, "revision"), "revision").clone()),
                                ])
                            })
                            .collect(),
                    ),
                ),
                (
                    "nextCursor",
                    if offset + limit < selected.len() {
                        (offset + limit).to_string().into()
                    } else {
                        Json::Null
                    },
                ),
            ]));
        }
        if method == "objects.read" {
            let id = text(&params, "objectId");
            let record = s
                .records
                .iter()
                .find(|r| field(r, "object").get("id") == params.get("objectId"))
                .ok_or_else(|| IvyError::new("object_missing", "Fixture object is missing."))?;
            return match params.get("revision") {
                None => Ok(record.clone()),
                Some(revision) => s
                    .versions
                    .get(&id)
                    .and_then(|m| m.get(&(revision.as_f64().unwrap() as i64)))
                    .cloned()
                    .ok_or_else(|| {
                        IvyError::new("revision_pruned", "Fixture revision was pruned.")
                    }),
            };
        }
        assert_eq!(method, "objects.write");
        let applies = s.fault.as_ref().is_some_and(|f| {
            f.get("key")
                .is_some_and(|k| Some(k) == params.get("create").and_then(|c| c.get("contractKey")))
                || f.get("objectId")
                    .is_some_and(|id| Some(id) == params.get("objectId"))
        });
        if applies {
            let fault = s.fault.as_mut().unwrap();
            if text(fault, "kind") == "before" {
                s.fault = None;
                return Err(IvyError::new(
                    "fixture_unavailable",
                    "Fixture write was not executed.",
                ));
            }
            if text(fault, "kind") == "contention" {
                let n = number(fault, "remaining");
                fault.insert("remaining", Json::Number(n - 1.0));
                if n > 0.0 {
                    return Err(IvyError::new(
                        "revision_conflict",
                        "Fixture revision changed.",
                    ));
                }
            }
        }
        let saved = if let Some(create) = params.get("create") {
            if s.records.iter().any(|r| {
                ["contractKey", "parentId", "name"]
                    .iter()
                    .all(|k| field(r, "object").get(k) == create.get(k))
            }) {
                return Err(IvyError::new(
                    "revision_conflict",
                    "Fixture name already exists.",
                ));
            }
            let id = format!("publication-record-{}", s.records.len() + 1);
            let saved = Json::object([
                (
                    "object",
                    Json::object([
                        ("id", id.clone().into()),
                        ("parentId", field(create, "parentId").clone()),
                        ("name", field(create, "name").clone()),
                        ("contractKey", field(create, "contractKey").clone()),
                        ("effectivelyArchived", Json::Bool(false)),
                        ("createdAt", "2026-09-21T14:13:20.123Z".into()),
                    ]),
                ),
                (
                    "revision",
                    Json::object([
                        ("revision", Json::Number(1.0)),
                        ("contractVersion", field(&params, "contractVersion").clone()),
                        ("mediaType", "application/json".into()),
                    ]),
                ),
                ("content", field(&params, "content").clone()),
            ]);
            s.records.push(saved.clone());
            s.versions.insert(id, HashMap::new());
            saved
        } else {
            let record = s
                .records
                .iter_mut()
                .find(|r| field(r, "object").get("id") == params.get("objectId"))
                .ok_or_else(|| IvyError::new("object_missing", "Fixture object is missing."))?;
            let revision = number(field(record, "revision"), "revision");
            if Some(&Json::Number(revision)) != params.get("expectedRevision") {
                return Err(IvyError::new(
                    "revision_conflict",
                    "Fixture revision changed.",
                ));
            }
            member_mut(record, "revision").insert("revision", Json::Number(revision + 1.0));
            record.insert("content", field(&params, "content").clone());
            record.clone()
        };
        s.versions
            .get_mut(&text(field(&saved, "object"), "id"))
            .unwrap()
            .insert(
                number(field(&saved, "revision"), "revision") as i64,
                saved.clone(),
            );
        if applies && s.fault.as_ref().is_some_and(|f| text(f, "kind") == "after") {
            s.fault = None;
            return Err(IvyError::new(
                "outcome_unknown",
                "Fixture reply was lost after the write committed.",
            )
            .with_outcome(Outcome::Unknown));
        }
        Ok(saved)
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
struct Source<'a> {
    operations: &'a ChatOperations<'a>,
    client: Arc<Client>,
    control: Arc<Mutex<Json>>,
}
impl Source<'_> {
    fn fail(&self, method: &str) -> IvyResult<()> {
        if let Some(Json::String(code)) =
            self.control.lock().unwrap().get(&format!("{method}Error"))
        {
            return Err(IvyError::new(
                code.to_utf8()?,
                format!("Fixture {method} failed."),
            ));
        }
        Ok(())
    }
    async fn collection(&self, input_id: &str, collect: bool) -> IvyResult<Option<Document>> {
        let method = if collect { "collect" } else { "find" };
        self.client.trace(
            if collect {
                "collector.collect"
            } else {
                "collector.find"
            },
            Json::object([("inputId", input_id.into())]),
        );
        self.fail(method)?;
        let control = self.control.lock().unwrap().clone();
        if flag(&control, &format!("{method}Missing")) {
            return Ok(None);
        }
        let mut doc = self
            .operations
            .store
            .named_in(
                "chat-bridge/collection",
                "Chat original turn collection",
                Some(input_id),
            )
            .await
            .map_err(|e| e.into_ivy())?;
        if let Some(d) = &mut doc {
            if flag(&control, &format!("{method}Incomplete")) {
                d.value.insert("state", "reading".into());
            }
            if collect && flag(&control, "collectChanged") {
                d.pin.revision += 1;
            }
        }
        Ok(doc)
    }
}
impl PublicationSource for Source<'_> {
    fn recover_pending(&self) -> PublicationFuture<'_, ()> {
        Box::pin(async move {
            self.client.trace("main.recoverPending", empty());
            self.fail("recover")
        })
    }
    fn verify_owner(&self) -> PublicationFuture<'_, ()> {
        Box::pin(async move {
            self.client.trace("native.verifyOwner", empty());
            let mut c = self.control.lock().unwrap();
            let n = c.get("verifyCount").and_then(Json::as_f64).unwrap_or(0.0) + 1.0;
            c.insert("verifyCount", Json::Number(n));
            if c.get("refuseAt") == Some(&Json::Number(n)) {
                return Err(IvyError::new(
                    "fixture_owner_refused",
                    "The original native owner is unavailable.",
                ));
            }
            Ok(())
        })
    }
    fn head(&self) -> PublicationFuture<'_, Option<Document>> {
        Box::pin(async move {
            self.client.trace("delivery.head", empty());
            if flag(&self.control.lock().unwrap(), "noHead") {
                return Ok(None);
            }
            let main = ChatQueue::new(self.operations, None).main().await?;
            let Some(ticket) = array(&main.value, "queue").first() else {
                return Ok(None);
            };
            Ok(Some(
                self.operations
                    .store
                    .read(
                        "chat-bridge/input",
                        Reference::Current(&text(ticket, "inputId")),
                    )
                    .await
                    .map_err(|e| e.into_ivy())?,
            ))
        })
    }
    fn find<'a>(&'a self, id: &'a str) -> PublicationFuture<'a, Option<Document>> {
        Box::pin(self.collection(id, false))
    }
    fn collect<'a>(&'a self, id: &'a str) -> PublicationFuture<'a, Option<Document>> {
        Box::pin(self.collection(id, true))
    }
}
fn compact(v: &Json) -> Json {
    match v {
        Json::String(s) if s.0.len() > 512 => {
            let bytes = s.0.iter().flat_map(|c| c.to_le_bytes()).collect::<Vec<_>>();
            let utf8 = String::from_utf16_lossy(&s.0);
            Json::object([
                ("utf16Length", Json::Number(s.0.len() as f64)),
                ("utf16Hash", digest(bytes).into()),
                ("utf8Length", Json::Number(utf8.len() as f64)),
                ("utf8Hash", digest(utf8).into()),
            ])
        }
        Json::Array(a) => Json::Array(a.iter().map(compact).collect()),
        Json::Object(o) => Json::object(o.iter().map(|(k, v)| (k.clone(), compact(v)))),
        _ => v.clone(),
    }
}
fn snapshot(
    path: &Path,
    client: &Client,
    output: Json,
    last: &mut i64,
    mutations: &mut HashMap<String, String>,
) -> Json {
    let db = rusqlite::Connection::open(path.join("chat-bridge.sqlite")).unwrap();
    let local=db.prepare("SELECT d.id,d.key,d.current_revision,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision ORDER BY d.id").unwrap()
        .query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?))).unwrap().map(|r|{let(id,key,revision,value)=r.unwrap();Json::object([
            ("id",id.into()),("key",key.into()),("revision",Json::Number(revision as f64)),("value",parse(&value,usize::MAX).unwrap())])}).collect();
    let writes = db
        .prepare(
            "SELECT rowid,key,value_json FROM publication_test_writes WHERE rowid>? ORDER BY rowid",
        )
        .unwrap()
        .query_map([*last], |r| {
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
        .collect();
    let mut state = client.0.lock().unwrap();
    let mut calls = vec![];
    let mut batch = vec![];
    let flush = |batch: &mut Vec<Json>, calls: &mut Vec<Json>| {
        batch.sort_by_cached_key(|c| JsString::from(canonical(c, usize::MAX).unwrap()));
        calls.append(batch);
    };
    for mut call in state.calls.drain(..) {
        if ["objects.read", "objects.query"].contains(&text(&call, "method").as_str()) {
            batch.push(call);
            continue;
        }
        flush(&mut batch, &mut calls);
        if text(&call, "method") == "objects.write" {
            let id = text(field(&call, "params"), "mutationId");
            let next = format!("<mutation:{}>", mutations.len());
            let alias = mutations.entry(id).or_insert(next).clone();
            member_mut(&mut call, "params").insert("mutationId", alias.into());
        }
        calls.push(call);
    }
    flush(&mut batch, &mut calls);
    compact(&Json::object([
        ("output", output),
        ("local", Json::Array(local)),
        ("records", Json::Array(state.records.clone())),
        ("writes", Json::Array(writes)),
        ("calls", Json::Array(calls)),
    ]))
}
async fn apply_step(
    step: &Json,
    seed: &Json,
    store: &ChatStore,
    ops: &ChatOperations<'_>,
    publisher: &mut ChatPublisher<'_>,
    client: &Client,
    control: &Mutex<Json>,
    corpus: &Json,
) -> IvyResult<Json> {
    match text(step, "action").as_str() {
        "step" => publisher.step().await?,
        "history" => {
            return ChatOutbox::new(ops)
                .history(
                    &"viewer".into(),
                    &Json::object([
                        ("expectedBridge", ops.admission.expected(&"viewer".into())),
                        ("channel", field(corpus, "channel").clone()),
                        ("afterSequence", Json::Number(0.0)),
                        ("limit", Json::Number(8.0)),
                    ]),
                )
                .await;
        }
        "control" => merge(&mut control.lock().unwrap(), field(step, "value")),
        "fault" => client.0.lock().unwrap().fault = Some(field(step, "value").clone()),
        "remove-text" => client
            .0
            .lock()
            .unwrap()
            .records
            .retain(|r| field(r, "object").get("id") != Some(&"text".into())),
        "patch" => {
            let target = text(step, "target");
            if ["input", "reply"].contains(&target.as_str()) {
                let id = if target == "input" {
                    text(seed, "inputId")
                } else {
                    text(
                        field(
                            client
                                .0
                                .lock()
                                .unwrap()
                                .records
                                .iter()
                                .find(|r| {
                                    field(r, "object").get("contractKey")
                                        == Some(&"chat-bridge/reply".into())
                                })
                                .unwrap(),
                            "object",
                        ),
                        "id",
                    )
                };
                client.patch(&id, field(step, "value"));
            } else {
                let id = text(field(seed, &format!("{target}Pin")), "objectId");
                let key = format!("chat-bridge/{target}");
                let parent = text(seed, "inputId");
                let doc = store
                    .read_in(
                        &key,
                        Reference::Current(&id),
                        if target == "main" {
                            None
                        } else {
                            Some(&parent)
                        },
                    )
                    .await
                    .map_err(|e| e.into_ivy())?;
                let mut value = doc.value;
                merge(&mut value, field(step, "value"));
                let written = store
                    .write(
                        &key,
                        &value,
                        "fixture-update",
                        Destination::Update {
                            object_id: &id,
                            expected_revision: doc.pin.revision,
                        },
                    )
                    .await
                    .map_err(|e| e.into_ivy())?;
                if target == "result" {
                    let collection_id = text(field(seed, "collectionPin"), "objectId");
                    let collection = store
                        .read_in(
                            "chat-bridge/collection",
                            Reference::Current(&collection_id),
                            Some(&parent),
                        )
                        .await
                        .map_err(|e| e.into_ivy())?;
                    let mut value = collection.value;
                    value.insert("result", written.json());
                    store
                        .write(
                            "chat-bridge/collection",
                            &value,
                            "fixture-update",
                            Destination::Update {
                                object_id: &collection_id,
                                expected_revision: collection.pin.revision,
                            },
                        )
                        .await
                        .map_err(|e| e.into_ivy())?;
                }
            }
        }
        "patch-claim" => {
            let id = text(field(seed, "mainPin"), "objectId");
            let doc = store
                .read("chat-bridge/main", Reference::Current(&id))
                .await
                .map_err(|e| e.into_ivy())?;
            let mut value = doc.value;
            let key = text(step, "field");
            let changed = if key == "partCount" {
                Json::Number(2.0)
            } else if key == "createdAt" {
                "2026-01-01T00:00:00.000Z".into()
            } else {
                Json::object([
                    ("objectId", "other".into()),
                    ("revision", Json::Number(1.0)),
                ])
            };
            member_mut(&mut value, "publication").insert(key, changed);
            value.insert("nextSequence", Json::Number(100.0));
            store
                .write(
                    "chat-bridge/main",
                    &value,
                    "fixture-update",
                    Destination::Update {
                        object_id: &id,
                        expected_revision: doc.pin.revision,
                    },
                )
                .await
                .map_err(|e| e.into_ivy())?;
        }
        "patch-reply-origin" => {
            let (id, mut origin) = {
                let s = client.0.lock().unwrap();
                let r = s
                    .records
                    .iter()
                    .find(|r| {
                        field(r, "object").get("contractKey") == Some(&"chat-bridge/reply".into())
                    })
                    .unwrap();
                (
                    text(field(r, "object"), "id"),
                    field(field(r, "content"), "value")
                        .get("origin")
                        .unwrap()
                        .clone(),
                )
            };
            origin.insert("inputId", "other".into());
            client.patch(&id, &Json::object([("origin", origin)]));
        }
        "reopen" => {}
        _ => panic!("Unknown step"),
    }
    Ok(Json::Null)
}
#[tokio::test]
async fn durable_publication_and_original_crash_databases_match_typescript() {
    let corpus = oracle();
    let admission = ChatAdmission::new(field(corpus, "definition")).unwrap();
    for case in array(corpus, "cases") {
        let seed = field(field(corpus, "seeds"), &text(case, "seed"));
        let path = Fixture::new(
            case.get("database")
                .unwrap_or_else(|| field(seed, "database")),
        );
        let client = Arc::new(Client::new(
            case.get("records")
                .map(|v| v.as_array().unwrap())
                .unwrap_or_else(|| array(seed, "records")),
        ));
        if flag(case, "scopeCacheWarm") {
            ivy_sdk::operation_ids::runtime_epoch(client.clone())
                .await
                .unwrap();
            client.0.lock().unwrap().calls.clear();
        }
        let control = Arc::new(Mutex::new(empty()));
        let mut last = number(case, "initialWrite") as i64;
        let mut mutations = HashMap::new();
        let mut index = 0;
        while index < array(case, "steps").len() {
            let store = ChatStore::with_clock(
                client.clone(),
                None,
                Some(path.0.clone()),
                Arc::new(Time(number(corpus, "now") as i64)),
            )
            .await
            .unwrap();
            let ops = ChatOperations::new(&store, &admission).unwrap();
            let source = Source {
                operations: &ops,
                client: client.clone(),
                control: control.clone(),
            };
            let mut publisher = ChatPublisher::new(&ops, &source);
            loop {
                let step = &array(case, "steps")[index];
                let output = result(
                    apply_step(
                        step,
                        seed,
                        &store,
                        &ops,
                        &mut publisher,
                        &client,
                        &control,
                        corpus,
                    )
                    .await,
                );
                let actual = snapshot(&path.0, &client, output, &mut last, &mut mutations);
                let expected = &array(case, "expected")[index];
                if let Some(diff) = difference(&actual, expected, "publication") {
                    panic!(
                        "{} step {index} {}: {diff}",
                        text(case, "label"),
                        text(step, "action")
                    );
                }
                index += 1;
                if index == array(case, "steps").len() || text(step, "action") == "reopen" {
                    break;
                }
            }
        }
    }
}
#[tokio::test]
async fn two_sqlite_publishers_converge_without_duplicate_replies() {
    let corpus = oracle();
    let admission = ChatAdmission::new(field(corpus, "definition")).unwrap();
    for case in array(corpus, "races") {
        let seed = field(field(corpus, "seeds"), &text(case, "seed"));
        let path = Fixture::new(field(seed, "database"));
        let client = Arc::new(Client::new(array(seed, "records")));
        let a = ChatStore::with_clock(
            client.clone(),
            None,
            Some(path.0.clone()),
            Arc::new(Time(number(corpus, "now") as i64)),
        )
        .await
        .unwrap();
        let b = ChatStore::with_clock(
            client.clone(),
            None,
            Some(path.0.clone()),
            Arc::new(Time(number(corpus, "now") as i64)),
        )
        .await
        .unwrap();
        let oa = ChatOperations::new(&a, &admission).unwrap();
        let ob = ChatOperations::new(&b, &admission).unwrap();
        let sa = Source {
            operations: &oa,
            client: client.clone(),
            control: Arc::new(Mutex::new(empty())),
        };
        let sb = Source {
            operations: &ob,
            client: client.clone(),
            control: Arc::new(Mutex::new(empty())),
        };
        let mut pa = ChatPublisher::new(&oa, &sa);
        let mut pb = ChatPublisher::new(&ob, &sb);
        for _ in 0..10 {
            let (x, y) = tokio::join!(pa.step(), pb.step());
            x.unwrap();
            y.unwrap();
        }
        let main = ChatQueue::new(&oa, None).main().await.unwrap().value;
        let operation = oa
            .find(
                js(seed, "caller"),
                &format!("chat:{}:original", number(corpus, "now") as i64).into(),
            )
            .await
            .unwrap()
            .unwrap()
            .value;
        let s = client.0.lock().unwrap();
        let input = field(
            field(
                s.records
                    .iter()
                    .find(|r| field(r, "object").get("id") == seed.get("inputId"))
                    .unwrap(),
                "content",
            ),
            "value",
        )
        .clone();
        let mut replies: Vec<_> = s
            .records
            .iter()
            .filter(|r| field(r, "object").get("contractKey") == Some(&"chat-bridge/reply".into()))
            .map(|r| field(field(r, "content"), "value").clone())
            .collect();
        replies.sort_by(|a, b| number(a, "sequence").total_cmp(&number(b, "sequence")));
        let actual = compact(&Json::object([
            ("main", main),
            ("input", input),
            ("replies", Json::Array(replies)),
            ("operation", operation),
        ]));
        if let Some(diff) = difference(&actual, field(case, "expected"), "race") {
            panic!("{}: {diff}", text(case, "seed"));
        }
    }
}
