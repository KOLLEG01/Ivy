use super::super::Fixture;
use super::{Gate, array, equal, field, member, merge, result, text};
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_chat_bridge::{
    admission::ChatAdmission,
    collector::{ChatCollector, CollectionHead, CollectionRuntime},
    native_evidence::ChatNativeEvidence,
    operations::ChatOperations,
    publisher::{ChatPublisher, PublicationFuture},
    queue::ChatQueue,
    store::{ChatStore, Pin, Reference},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, Outcome,
    checked_native_contract::CheckedNativeContracts,
    hash::digest,
    json::{canonical, parse},
};
use ivy_sdk::{
    clock::Clock,
    rpc::{RequestFuture, RequestOptions, RpcClient},
};
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    process::Command,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicI64, Ordering},
    },
    time::Instant,
};

fn oracle() -> &'static Json {
    static CORPUS: OnceLock<Json> = OnceLock::new();
    CORPUS.get_or_init(|| {
        let output = Command::new("node")
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .arg("tests/rust/fixtures/chat-collector-oracle.mjs")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if let Some(path) = std::env::var_os("IVY_CHAT_COLLECTOR_ORACLE_CAPTURE") {
            std::fs::write(path, &output.stdout).unwrap();
        }
        parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
    })
}
fn number(v: &Json, key: &str) -> usize {
    field(v, key).as_f64().unwrap() as usize
}
fn reference(v: &Json) -> Pin {
    Pin {
        object_id: text(v, "objectId"),
        revision: number(v, "revision") as i64,
    }
}
fn optional(v: Option<&Json>) -> Json {
    v.cloned().unwrap_or(Json::Null)
}
fn flag(v: &Json, key: &str) -> bool {
    v.get(key) == Some(&true.into())
}
struct Time(AtomicI64);
impl Clock for Time {
    fn utc_millis(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
struct Remote {
    records: Vec<Json>,
    versions: HashMap<String, HashMap<usize, Json>>,
    calls: Vec<Json>,
    receipts: HashMap<String, Json>,
    fault: Option<Json>,
}
struct Client(Mutex<Remote>);
impl Client {
    fn new(records: Vec<Json>) -> Self {
        let versions = records
            .iter()
            .map(|r| {
                (
                    text(field(r, "object"), "id"),
                    HashMap::from([(number(field(r, "revision"), "revision"), r.clone())]),
                )
            })
            .collect();
        Self(Mutex::new(Remote {
            records,
            versions,
            calls: vec![],
            receipts: HashMap::new(),
            fault: None,
        }))
    }
}
impl RpcClient for Client {
    fn request<'a>(&'a self, method: &'a str, p: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            state.calls.push(Json::object([
                ("method", method.into()),
                ("params", p.clone()),
            ]));
            match method {
                "system.status" => return Ok(Json::object([("runtimeEpoch", "chat".into())])),
                "objects.read" | "objects.stat" => {
                    let current = state
                        .records
                        .iter()
                        .find(|r| field(r, "object").get("id") == p.get("objectId"))
                        .ok_or_else(|| {
                            IvyError::new("object_missing", "Original object is unavailable.")
                        })?;
                    let selected = if p.get("revision").is_none() {
                        current
                    } else {
                        state
                            .versions
                            .get(&text(&p, "objectId"))
                            .and_then(|v| v.get(&number(&p, "revision")))
                            .ok_or_else(|| {
                                IvyError::new(
                                    "revision_pruned",
                                    "Original revision is unavailable.",
                                )
                            })?
                    };
                    return Ok(if method == "objects.stat" {
                        field(selected, "object")
                    } else {
                        selected
                    }
                    .clone());
                }
                "objects.query" => {
                    let args = array(field(&p, "where"), "args");
                    let parent = optional(args[0].get("value"));
                    let name = field(&args[1], "value");
                    let rows = state
                        .records
                        .iter()
                        .filter(|r| {
                            field(r, "object").get("contractKey") == p.get("contractKey")
                                && field(r, "object").get("parentId") == Some(&parent)
                                && field(r, "object").get("name") == Some(name)
                        })
                        .map(|r| {
                            Json::object([
                                ("objectId", field(field(r, "object"), "id").clone()),
                                ("revision", field(field(r, "revision"), "revision").clone()),
                            ])
                        })
                        .collect();
                    return Ok(Json::object([
                        ("items", Json::Array(rows)),
                        ("nextCursor", Json::Null),
                    ]));
                }
                "objects.write" => {}
                _ => panic!("Unexpected RPC {method}"),
            }
            let mutation = text(&p, "mutationId");
            if let Some(saved) = state.receipts.get(&mutation) {
                return Ok(saved.clone());
            }
            let applies = state.fault.as_ref().is_some_and(|f| {
                f.get("key").is_some()
                    && f.get("key") == p.get("create").and_then(|c| c.get("contractKey"))
                    || f.get("objectId").is_some() && f.get("objectId") == p.get("objectId")
            });
            let fault = if applies { state.fault.take() } else { None };
            if let Some(fault) = &fault {
                match text(fault, "kind").as_str() {
                    "before" => {
                        return Err(IvyError::new(
                            "fixture_unavailable",
                            "Original write was not executed.",
                        ));
                    }
                    "contention" => {
                        return Err(IvyError::new(
                            "revision_conflict",
                            "Original revision changed.",
                        ));
                    }
                    _ => {}
                }
            }
            let saved = if let Some(create) = p.get("create") {
                if state.records.iter().any(|r| {
                    ["contractKey", "parentId", "name"]
                        .iter()
                        .all(|k| field(r, "object").get(k) == create.get(k))
                }) {
                    return Err(IvyError::new(
                        "revision_conflict",
                        "Original name already exists.",
                    ));
                }
                let content = field(&p, "content");
                let text_content = content.get("encoding") == Some(&"text".into());
                let mut metadata = Json::object([
                    (
                        "id",
                        format!("collector-public-{}", state.records.len() + 1).into(),
                    ),
                    ("parentId", field(create, "parentId").clone()),
                    ("name", field(create, "name").clone()),
                    ("contractKey", field(create, "contractKey").clone()),
                    ("effectivelyArchived", false.into()),
                ]);
                if let Some(owner) = create.get("ownerObjectId") {
                    metadata.insert("ownerObjectId", owner.clone());
                }
                let mut revision = Json::object([
                    ("revision", 1.into()),
                    ("contractVersion", field(&p, "contractVersion").clone()),
                    (
                        "mediaType",
                        if text_content {
                            "text/plain"
                        } else {
                            "application/json"
                        }
                        .into(),
                    ),
                ]);
                if text_content {
                    let bytes = text(content, "value").into_bytes();
                    revision.insert("byteLength", Json::Number(bytes.len() as f64));
                    revision.insert("contentHash", digest(&bytes).into());
                }
                let saved = Json::object([
                    ("object", metadata),
                    ("revision", revision),
                    ("content", content.clone()),
                ]);
                state.records.push(saved.clone());
                saved
            } else {
                let current = state
                    .records
                    .iter_mut()
                    .find(|r| field(r, "object").get("id") == p.get("objectId"))
                    .ok_or_else(|| {
                        IvyError::new("object_missing", "Original object is unavailable.")
                    })?;
                if field(current, "revision").get("revision") != p.get("expectedRevision") {
                    return Err(IvyError::new(
                        "revision_conflict",
                        "Original revision changed.",
                    ));
                }
                let n = number(field(current, "revision"), "revision");
                member(current, "revision").insert("revision", Json::Number((n + 1) as f64));
                current.insert("content", field(&p, "content").clone());
                current.clone()
            };
            state
                .versions
                .entry(text(field(&saved, "object"), "id"))
                .or_default()
                .insert(number(field(&saved, "revision"), "revision"), saved.clone());
            state.receipts.insert(mutation, saved.clone());
            if fault
                .as_ref()
                .is_some_and(|f| f.get("kind") == Some(&"after".into()))
            {
                return Err(IvyError::new(
                    "outcome_unknown",
                    "Original reply was lost after commit.",
                )
                .with_outcome(Outcome::Unknown));
            }
            Ok(saved)
        })
    }
}
struct Live {
    controls: Json,
    events: Vec<Json>,
    observations: VecDeque<Json>,
}
struct Runtime<'a> {
    store: &'a ChatStore,
    operations: &'a ChatOperations<'a>,
    binding: Pin,
    live: &'a Mutex<Live>,
    clock: &'a Time,
}
impl Runtime<'_> {
    fn event(&self, event: Json) {
        self.live.lock().unwrap().events.push(event);
    }
}
impl CollectionRuntime for Runtime<'_> {
    fn head<'a>(
        &'a self,
        input_id: Option<&'a str>,
    ) -> PublicationFuture<'a, Option<CollectionHead>> {
        Box::pin(async move {
            self.event(Json::object([
                ("method", "head".into()),
                ("inputId", input_id.map(Json::from).unwrap_or(Json::Null)),
            ]));
            if flag(&self.live.lock().unwrap().controls, "noHead") {
                return Ok(None);
            }
            if array(
                &ChatQueue::new(self.operations, None).main().await?.value,
                "queue",
            )
            .is_empty()
            {
                return Ok(None);
            }
            let input = self
                .store
                .read(
                    "chat-bridge/input",
                    Reference::Current(input_id.unwrap_or("original-input")),
                )
                .await
                .map_err(|e| e.into_ivy())?;
            let binding = self
                .store
                .read("chat-bridge/binding", Reference::Pinned(&self.binding))
                .await
                .map_err(|e| e.into_ivy())?;
            Ok(Some(CollectionHead { input, binding }))
        })
    }
    fn call<'a>(
        &'a self,
        input_id: &'a str,
        pin: &'a Pin,
    ) -> PublicationFuture<'a, ivy_chat_bridge::store::Document> {
        Box::pin(async move {
            self.event(Json::object([
                ("method", "call".into()),
                ("inputId", input_id.into()),
                ("pin", pin.json()),
            ]));
            self.store
                .read_in(
                    "chat-bridge/native-call",
                    Reference::Pinned(pin),
                    Some(input_id),
                )
                .await
                .map_err(|e| e.into_ivy())
        })
    }
    fn ready(&self) -> PublicationFuture<'_, Json> {
        Box::pin(async move {
            self.event(Json::object([("method", "ready".into())]));
            Ok(Json::object([
                (
                    "epoch",
                    field(&self.live.lock().unwrap().controls, "epoch").clone(),
                ),
                (
                    "observedAt",
                    Json::String(ivy_sdk::clock::date_to_iso(self.clock.utc_millis() as f64)?),
                ),
            ]))
        })
    }
    fn read(&self, request: Json) -> PublicationFuture<'_, Json> {
        Box::pin(async move {
            self.event(Json::object([
                ("method", "read".into()),
                ("request", request.clone()),
            ]));
            let mut live = self.live.lock().unwrap();
            if let Some(code) = live.controls.get("readError").filter(|v| !v.is_null()) {
                return Err(IvyError::new(
                    code.as_str()?,
                    "Original read transport is unavailable.",
                ));
            }
            let observation = live
                .observations
                .pop_front()
                .expect("Unexpected original read");
            for key in ["method", "params", "nativeVersion"] {
                equal(
                    field(&request, key),
                    field(&observation, key),
                    "native read transport request",
                );
            }
            Ok(observation)
        })
    }
    fn verify_owner(&self) -> PublicationFuture<'_, ()> {
        Box::pin(async move {
            self.event(Json::object([("method", "verify".into())]));
            Ok(())
        })
    }
    fn recover_pending(&self) -> PublicationFuture<'_, ()> {
        Box::pin(async move {
            self.event(Json::object([("method", "recover".into())]));
            Ok(())
        })
    }
}
fn compact(value: &Json) -> Json {
    match value {
        Json::String(s) if s.utf16_len() > 512 => {
            let utf16: Vec<u8> = s.0.iter().flat_map(|u| u.to_le_bytes()).collect();
            let utf8 = String::from_utf16_lossy(&s.0).into_bytes();
            Json::object([
                ("utf16Length", Json::Number(s.utf16_len() as f64)),
                ("utf16Hash", digest(utf16).into()),
                ("utf8Length", Json::Number(utf8.len() as f64)),
                ("utf8Hash", digest(utf8).into()),
            ])
        }
        Json::Array(a) => Json::Array(a.iter().map(compact).collect()),
        Json::Object(o) => Json::Object(o.iter().map(|(k, v)| (k.clone(), compact(v))).collect()),
        _ => value.clone(),
    }
}
fn normalize(value: &Json, aliases: &[(String, String)]) -> Json {
    match value {
        Json::String(s) => {
            let Ok(mut text) = s.to_utf8() else {
                return value.clone();
            };
            if let Some((_, alias)) = aliases.iter().find(|(id, _)| id == &text) {
                return alias.clone().into();
            }
            for (id, alias) in aliases {
                if text.contains(id) {
                    text = text.replace(id, alias);
                }
            }
            text.into()
        }
        Json::Array(a) => Json::Array(a.iter().map(|v| normalize(v, aliases)).collect()),
        Json::Object(o) => Json::Object(
            o.iter()
                .map(|(k, v)| (k.clone(), normalize(v, aliases)))
                .collect(),
        ),
        _ => value.clone(),
    }
}
fn alias(aliases: &mut Vec<(String, String)>, id: String, name: String) {
    if let Some((_, old)) = aliases.iter_mut().find(|(key, _)| key == &id) {
        *old = name;
    } else {
        aliases.push((id, name));
    }
}
fn snapshot(
    path: &Fixture,
    client: &Client,
    live: &Mutex<Live>,
    gate: &Gate,
    output: Json,
    last: &mut i64,
    aliases: &mut Vec<(String, String)>,
    mutations: &mut HashMap<String, String>,
) -> Json {
    let db = path.db();
    let documents:Vec<Json>=db.prepare("SELECT d.id,d.key,d.name,d.parent_id,d.current_revision,r.value_json FROM documents d JOIN revisions r ON d.id=r.id AND d.current_revision=r.revision ORDER BY d.name,d.id").unwrap().query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,i64>(4)?,r.get::<_,String>(5)?))).unwrap().map(|r|{
        let (id,key,name,parent,revision,value)=r.unwrap();
        let logical=match key.as_str(){"chat-bridge/collection"=>"<collection>".into(),"chat-bridge/result"=>"<result>".into(),"chat-bridge/reply"=>format!("<reply:{}>",name.split(" part ").last().unwrap()),_=>format!("<local:{name}>")};
        alias(aliases,id.clone(),logical);
        Json::object([("id",id.into()),("key",key.into()),("name",name.into()),("parent",parent.map(Json::from).unwrap_or(Json::Null)),("revision",Json::Number(revision as f64)),("value",parse(&value,usize::MAX).unwrap())])
    }).collect();
    let blobs: Vec<Json> = db
        .prepare("SELECT id,key,name,parent_id,bytes FROM blobs ORDER BY name,id")
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Vec<u8>>(4)?,
            ))
        })
        .unwrap()
        .map(|r| {
            let (id, key, name, parent, bytes) = r.unwrap();
            alias(aliases, id.clone(), format!("<blob:{name}>"));
            Json::object([
                ("id", id.into()),
                ("key", key.into()),
                ("name", name.into()),
                ("parent", parent.map(Json::from).unwrap_or(Json::Null)),
                ("byteLength", Json::Number(bytes.len() as f64)),
                ("contentHash", digest(bytes).into()),
            ])
        })
        .collect();
    let writes:Vec<Json>=db.prepare("SELECT rowid,key,id,revision,value_json FROM collector_test_writes WHERE rowid>? ORDER BY rowid").unwrap().query_map([*last],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?,r.get::<_,String>(4)?))).unwrap().map(|r|{
        let (row,key,id,revision,value)=r.unwrap();*last=row;
        Json::object([("key",key.into()),("id",id.into()),("revision",Json::Number(revision as f64)),("value",parse(&value,usize::MAX).unwrap())])
    }).collect();
    assert!(
        gate.expected.lock().unwrap().is_empty(),
        "Missing original shared gate"
    );
    let mut remote = client.0.lock().unwrap();
    for call in &remote.calls {
        if let Some(id) = field(call, "params").get("mutationId") {
            let id = id.as_str().unwrap();
            let next = format!("<mutation:{}>", mutations.len());
            let logical = mutations.entry(id.clone()).or_insert(next).clone();
            alias(aliases, id, logical);
        }
    }
    compact(&normalize(
        &Json::object([
            ("output", output),
            ("documents", Json::Array(documents)),
            ("blobs", Json::Array(blobs)),
            ("writes", Json::Array(writes)),
            ("records", Json::Array(remote.records.clone())),
            ("calls", Json::Array(std::mem::take(&mut remote.calls))),
            (
                "events",
                Json::Array(std::mem::take(&mut live.lock().unwrap().events)),
            ),
            (
                "gates",
                Json::Array(std::mem::take(&mut *gate.calls.lock().unwrap())),
            ),
        ]),
        aliases,
    ))
}
async fn action(
    s: &Json,
    collector: &ChatCollector<'_>,
    publisher: &mut ChatPublisher<'_>,
    store: &ChatStore,
    client: &Client,
    live: &Mutex<Live>,
    clock: &Time,
    path: &Fixture,
    retry: &mut Option<Json>,
    admission: &ChatAdmission,
) -> IvyResult<Json> {
    match text(s, "action").as_str() {
        "step" => {
            return Ok(collector
                .step("original-input")
                .await?
                .map(|d| Json::object([("pin", d.pin.json()), ("value", d.value)]))
                .unwrap_or(Json::Null));
        }
        "terminal" => return Ok(collector.observed_terminal("original-input").await?.into()),
        "publish" => publisher.step().await?,
        "reopen" | "new-client" => {}
        "control" => merge(&mut live.lock().unwrap().controls, field(s, "value")),
        "advance" => {
            clock
                .0
                .fetch_add(number(s, "millis") as i64, Ordering::Relaxed);
        }
        "fault" => client.0.lock().unwrap().fault = Some(field(s, "value").clone()),
        "patch" => {
            if text(s, "target") == "input" {
                let mut state = client.0.lock().unwrap();
                let input = &mut state.records[0];
                merge(member(member(input, "content"), "value"), field(s, "value"));
                let input = input.clone();
                state
                    .versions
                    .get_mut("original-input")
                    .unwrap()
                    .insert(number(field(&input, "revision"), "revision"), input);
            } else {
                let current = collector.find("original-input").await?.unwrap();
                let mut value = current.value.clone();
                merge(&mut value, field(s, "value"));
                if flag(field(s, "value"), "duplicateReads") {
                    value.remove("duplicateReads");
                    let mut reads = array(&value, "reads").to_vec();
                    reads.push(reads[0].clone());
                    value.insert("reads", Json::Array(reads));
                }
                path.patch(&current.pin, &value);
            }
        }
        "retry" => {
            let current = collector.find("original-input").await?;
            if retry.is_none() || s.get("remember") != Some(&false.into()) {
                let selected = if let Some(revision) = s.get("revision") {
                    Json::object([
                        (
                            "objectId",
                            current.as_ref().unwrap().pin.object_id.clone().into(),
                        ),
                        ("revision", revision.clone()),
                    ])
                } else {
                    current.map(|c| c.pin.json()).unwrap_or(Json::object([
                        ("objectId", "missing".into()),
                        ("revision", 1.into()),
                    ]))
                };
                *retry = Some(Json::object([
                    ("inputId", "original-input".into()),
                    ("expectedCollection", selected),
                    ("expectedBridge", admission.expected(&"alice".into())),
                    (
                        "operationId",
                        format!("chat:{}:retry", clock.0.load(Ordering::Relaxed)).into(),
                    ),
                    ("reason", "Original operator retry".into()),
                ]));
            }
            return collector
                .retry(
                    s.get("caller")
                        .and_then(Json::as_js_string)
                        .unwrap_or(&"alice".into()),
                    retry.as_ref().unwrap(),
                )
                .await;
        }
        "crash-collection" | "crash-complete" => {
            let condition = if text(s, "action") == "crash-complete" {
                "json_extract(NEW.value_json,'$.state')='complete'"
            } else {
                "json_array_length(NEW.value_json,'$.reads')>1"
            };
            path.db().execute_batch(&format!("CREATE TABLE collector_crash_arm(x INTEGER);INSERT INTO collector_crash_arm VALUES(1);CREATE TRIGGER collector_commit_crash BEFORE INSERT ON revisions WHEN EXISTS(SELECT 1 FROM documents WHERE id=NEW.id AND key='chat-bridge/collection') AND {condition} AND EXISTS(SELECT 1 FROM collector_crash_arm) BEGIN SELECT RAISE(FAIL,'Original collector checkpoint unavailable'); END;")).unwrap();
        }
        other => panic!("Unexpected action {other}"),
    }
    let _ = store;
    Ok(Json::Null)
}
#[tokio::test]
async fn original_collector_replay_retry_results_and_publication() {
    let corpus = oracle();
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let instructions = String::from_utf8(STANDARD.decode(text(corpus, "instructions")).unwrap())
        .unwrap()
        .into();
    let mut workflows = 0;
    let mut steps = 0;
    let mut gates = 0;
    for case in array(corpus, "cases") {
        let seed_key = case
            .get("seed")
            .unwrap_or_else(|| field(case, "version"))
            .as_str()
            .unwrap();
        let seed = field(field(corpus, "seeds"), &seed_key);
        if let Some(snapshot) = seed.get("snapshot") {
            assert_eq!(
                digest(STANDARD.decode(text(seed, "database")).unwrap()),
                text(snapshot, "contentHash"),
                "Original SQLite snapshot bytes differ"
            );
        }
        let path = Fixture::new(field(seed, "database"));
        let mut client = Arc::new(Client::new(array(seed, "records").to_vec()));
        let clock = Arc::new(Time(AtomicI64::new(
            seed.get("initialNow")
                .and_then(Json::as_f64)
                .unwrap_or(number(corpus, "now") as f64) as i64,
        )));
        let live = Mutex::new(Live {
            controls: seed
                .get("initialControls")
                .cloned()
                .unwrap_or_else(|| Json::object([("epoch", "original-epoch".into())])),
            events: vec![],
            observations: array(case, "observations").iter().cloned().collect(),
        });
        let gate = Gate::new();
        let mut aliases = vec![];
        let mut mutations = HashMap::new();
        let mut last = 0;
        let mut index = 0;
        let mut retry = None;
        while index < array(case, "steps").len() {
            let store =
                ChatStore::with_clock(client.clone(), None, Some(path.0.clone()), clock.clone())
                    .await
                    .unwrap();
            let admission = ChatAdmission::new(field(seed, "definition")).unwrap();
            let ops = ChatOperations::new(&store, &admission).unwrap();
            let evidence = ChatNativeEvidence::new(
                &store,
                field(seed, "definition"),
                &catalogs,
                &gate,
                &instructions,
            )
            .unwrap();
            let runtime = Runtime {
                store: &store,
                operations: &ops,
                binding: reference(field(seed, "binding")),
                live: &live,
                clock: &clock,
            };
            let collector = ChatCollector::new(&ops, &evidence, &runtime, &catalogs);
            let mut publisher = ChatPublisher::new(&ops, &collector);
            loop {
                let s = &array(case, "steps")[index];
                let expected = &array(case, "expected")[index];
                gate.begin(array(expected, "gates"));
                gates += array(expected, "gates").len();
                let output = result(
                    action(
                        s,
                        &collector,
                        &mut publisher,
                        &store,
                        &client,
                        &live,
                        &clock,
                        &path,
                        &mut retry,
                        &admission,
                    )
                    .await,
                );
                let actual = snapshot(
                    &path,
                    &client,
                    &live,
                    &gate,
                    output,
                    &mut last,
                    &mut aliases,
                    &mut mutations,
                );
                // Report a compact first difference instead of multi-megabyte assertions.
                let label = format!("{} step {index} {}", text(case, "label"), text(s, "action"));
                compare(&actual, expected, &label, "");
                index += 1;
                steps += 1;
                if ["reopen", "new-client"]
                    .iter()
                    .any(|v| text(s, "action") == *v)
                {
                    path.db().execute_batch("DROP TRIGGER IF EXISTS collector_commit_crash;DROP TABLE IF EXISTS collector_crash_arm;").unwrap();
                    break;
                }
                if index == array(case, "steps").len() {
                    break;
                }
            }
            let previous = &array(case, "steps")[index - 1];
            if text(previous, "action") == "new-client" {
                let records = client.0.lock().unwrap().records.clone();
                client = Arc::new(Client::new(records));
            }
        }
        assert!(
            live.lock().unwrap().observations.is_empty(),
            "Unconsumed original observations"
        );
        workflows += 1;
    }
    println!(
        "Original collector differential: {workflows} workflows, {steps} steps, {gates} required shared boundary comparisons; retained SQLite, read identity, Result text and publisher execute in Rust."
    );
}
fn compare(a: &Json, b: &Json, label: &str, path: &str) {
    match (a, b) {
        (Json::Object(x), Json::Object(y)) => {
            let mut ak: Vec<_> = x.iter().map(|(k, _)| k).collect();
            let mut bk: Vec<_> = y.iter().map(|(k, _)| k).collect();
            ak.sort();
            bk.sort();
            assert_eq!(ak, bk, "{label} {path} keys");
            for (k, v) in x {
                compare(
                    v,
                    b.get_key(k).unwrap(),
                    label,
                    &format!("{path}/{}", k.to_utf8().unwrap()),
                );
            }
        }
        (Json::Array(x), Json::Array(y)) => {
            assert_eq!(x.len(), y.len(), "{label} {path} array length");
            for (i, (x, y)) in x.iter().zip(y).enumerate() {
                compare(x, y, label, &format!("{path}/{i}"));
            }
        }
        _ => assert_eq!(
            canonical(a, usize::MAX).unwrap(),
            canonical(b, usize::MAX).unwrap(),
            "{label} {path}"
        ),
    }
}
