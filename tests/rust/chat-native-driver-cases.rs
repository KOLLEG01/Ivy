use base64::{Engine, engine::general_purpose::STANDARD};
#[path = "chat-main-cases.rs"]
mod main;
use ivy_chat_bridge::{
    admission::ChatAdmission,
    native_driver::{ChatNativeDriver, NativeOrigin, native_path},
    store::{ChatStore, Destination, Document, Pin, Reference, mutation},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    checked_native_contract::CheckedNativeContracts,
    hash::digest,
    json::{canonical, parse},
};
use ivy_sdk::{
    clock::Clock,
    native_owner::{GuardFuture, NativeDispatchGuard},
    rpc::{RequestFuture, RequestOptions, RpcClient},
    tools::ToolClients,
};
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    process::Command,
    sync::{Arc, Mutex, OnceLock},
    time::Instant,
};

fn field<'a>(value: &'a Json, key: &str) -> &'a Json {
    value.get(key).unwrap()
}
fn text(value: &Json, key: &str) -> String {
    field(value, key).as_str().unwrap()
}
fn equal(actual: &Json, expected: &Json, label: &str) {
    assert_eq!(
        canonical(actual, usize::MAX).unwrap(),
        canonical(expected, usize::MAX).unwrap(),
        "{label}"
    );
}
fn result(value: IvyResult<Json>) -> Json {
    match value {
        Ok(value) => Json::object([("value", value)]),
        Err(e) => Json::object([(
            "error",
            Json::object([
                ("code", Json::String(e.code_js())),
                ("message", Json::String(e.message_js())),
                ("outcome", e.outcome.to_json()),
            ]),
        )]),
    }
}
fn decode(value: &Json) -> IvyResult<Json> {
    if let Some(error) = value.get("error") {
        let mut e = IvyError::new_js(
            field(error, "code").as_js_string().unwrap().clone(),
            field(error, "message").as_js_string().unwrap().clone(),
        )
        .with_outcome(match text(error, "outcome").as_str() {
            "unknown" => Outcome::Unknown,
            "completed" => Outcome::Completed,
            _ => Outcome::NotExecuted,
        });
        e.details = error.get("details").cloned();
        Err(e)
    } else {
        Ok(field(value, "value").clone())
    }
}
fn pin(value: &Json) -> Pin {
    Pin {
        object_id: text(value, "objectId"),
        revision: field(value, "revision").as_f64().unwrap() as i64,
    }
}
fn merge(value: &mut Json, patch: &Json) {
    for (key, v) in patch.as_object().unwrap() {
        value.insert(key.clone(), v.clone());
    }
}
fn member<'a>(value: &'a mut Json, key: &str) -> &'a mut Json {
    let Json::Object(fields) = value else {
        panic!("Expected fixture object")
    };
    &mut fields
        .iter_mut()
        .find(|(k, _)| k == &JsString::from(key))
        .unwrap()
        .1
}
fn flag(value: &Json, key: &str) -> bool {
    value.get(key) == Some(&Json::Bool(true))
}
fn document(value: Document) -> Json {
    Json::object([("pin", value.pin.json()), ("value", value.value)])
}
fn oracle() -> &'static Json {
    static ORACLE: OnceLock<Json> = OnceLock::new();
    ORACLE.get_or_init(|| {
        let output = Command::new("node")
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .arg("tests/rust/fixtures/chat-native-driver-workflows.mjs")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if let Some(path) = std::env::var_os("IVY_CHAT_NATIVE_DRIVER_ORACLE_CAPTURE") {
            std::fs::write(path, &output.stdout).unwrap();
        }
        parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
    })
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

// Only the explicit synthetic native/Hive transport and caller-claim adapters
// replay. Driver, SQLite, evidence bytes and production validators execute Rust.
struct Transport(Mutex<VecDeque<Json>>);
impl Transport {
    fn begin(&self, value: &Json) {
        let mut pending = self.0.lock().unwrap();
        assert!(pending.is_empty());
        pending.extend(value.as_array().unwrap().iter().cloned());
    }
    fn take(&self, kind: &str) -> Json {
        let value = self
            .0
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected adapter call");
        assert_eq!(text(&value, "kind"), kind, "Adapter order");
        value
    }
}
impl RpcClient for Transport {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let event = self.take("rpc");
            assert_eq!(text(&event, "method"), method, "RPC method");
            equal(&params, field(&event, "params"), "RPC arguments");
            decode(field(&event, "result"))
        })
    }
}
impl NativeDispatchGuard for Transport {
    fn check(&self) -> GuardFuture<'_> {
        Box::pin(async move {
            let event = self.take("guard");
            decode(field(&event, "result")).map(|_| ())
        })
    }
}
fn normalize(value: &Json, aliases: &HashMap<String, String>) -> Json {
    match value {
        Json::String(s) => s
            .to_utf8()
            .ok()
            .and_then(|s| aliases.get(&s))
            .map(|s| Json::from(s.clone()))
            .unwrap_or_else(|| value.clone()),
        Json::Array(values) => Json::Array(values.iter().map(|v| normalize(v, aliases)).collect()),
        Json::Object(fields) => Json::Object(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), normalize(v, aliases)))
                .collect(),
        ),
        _ => value.clone(),
    }
}
fn snapshot(
    path: &Path,
    output: Json,
    last: &mut i64,
    aliases: &mut HashMap<String, String>,
) -> Json {
    let db = rusqlite::Connection::open(path.join("chat-bridge.sqlite")).unwrap();
    let documents:Vec<_>=db.prepare("SELECT d.id,d.name,d.parent_id,d.current_revision,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key IN ('chat-bridge/evidence','chat-bridge/native-call') ORDER BY d.name").unwrap()
        .query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,i64>(3)?,r.get::<_,String>(4)?))).unwrap().map(|r|{let(id,name,parent,revision,value)=r.unwrap();aliases.insert(id.clone(),format!("<local:{name}>"));Json::object([
            ("id",id.into()),("name",name.into()),("parentId",parent.map(Json::from).unwrap_or(Json::Null)),("revision",Json::Number(revision as f64)),("value",parse(&value,usize::MAX).unwrap())])}).collect();
    let blobs: Vec<_> = db
        .prepare("SELECT id,key,name,parent_id,bytes FROM blobs ORDER BY name,parent_id")
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
            aliases.insert(id.clone(), format!("<local:{name}>"));
            Json::object([
                ("id", id.into()),
                ("key", key.into()),
                ("name", name.into()),
                ("parentId", parent.map(Json::from).unwrap_or(Json::Null)),
                ("byteLength", Json::Number(bytes.len() as f64)),
                ("contentHash", digest(bytes).into()),
            ])
        })
        .collect();
    let writes: Vec<_> = db
        .prepare(
            "SELECT rowid,value_json FROM native_evidence_test_writes WHERE rowid>? ORDER BY rowid",
        )
        .unwrap()
        .query_map([*last], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })
        .unwrap()
        .map(|r| {
            let (id, value) = r.unwrap();
            *last = id;
            parse(&value, usize::MAX).unwrap()
        })
        .collect();
    normalize(
        &Json::object([
            ("output", output),
            ("documents", Json::Array(documents)),
            ("blobs", Json::Array(blobs)),
            ("writes", Json::Array(writes)),
        ]),
        aliases,
    )
}
fn selected_pin(selected: &Option<Pin>) -> IvyResult<Pin> {
    selected.clone().ok_or_else(|| {
        IvyError::new(
            "fixture_pin_missing",
            "Original native call pin is unavailable.",
        )
    })
}
async fn step(
    action: &Json,
    store: &ChatStore,
    driver: &ChatNativeDriver<'_>,
    body: &mut Json,
    origin: &mut Json,
    selected: &mut Option<Pin>,
    path: &Path,
    parent: &str,
) -> IvyResult<Json> {
    match text(action, "action").as_str() {
        "prepare" => {
            let call = driver
                .prepare(
                    parent,
                    &"driver-original-call".into(),
                    field(body, "request"),
                    NativeOrigin {
                        predecessor: origin.get("predecessor").filter(|v| !v.is_null()).map(pin),
                        prepared_epoch: origin
                            .get("preparedEpoch")
                            .and_then(Json::as_js_string)
                            .cloned(),
                    },
                    flag(action, "projection"),
                )
                .await?;
            *selected = Some(call.pin.clone());
            return Ok(document(call));
        }
        "get" => {
            let pin = selected_pin(selected)?;
            return Ok(document(
                driver
                    .get(
                        parent,
                        if flag(action, "current") {
                            Reference::Current(&pin.object_id)
                        } else {
                            Reference::Pinned(&pin)
                        },
                    )
                    .await?,
            ));
        }
        "verify-project" => driver.verify_project().await?,
        "patch" => match text(action, "target").as_str() {
            "origin" => merge(origin, field(action, "value")),
            "requestParams" => merge(
                member(member(body, "request"), "params"),
                field(action, "value"),
            ),
            "node" => {}
            target => merge(member(body, target), field(action, "value")),
        },
        "state" => {}
        "rewrite" => {
            let p = selected_pin(selected)?;
            let old = store
                .read_in(
                    "chat-bridge/native-call",
                    Reference::Current(&p.object_id),
                    Some(parent),
                )
                .await
                .map_err(|e| e.into_ivy())?;
            let mut next = old.value;
            merge(&mut next, field(action, "value"));
            store
                .write(
                    "chat-bridge/native-call",
                    &next,
                    &mutation(&old.pin.object_id, "fixture-rewrite")?,
                    Destination::Update {
                        object_id: &old.pin.object_id,
                        expected_revision: old.pin.revision,
                    },
                )
                .await
                .map_err(|e| e.into_ivy())?;
        }
        "rename" => {
            let db = rusqlite::Connection::open(path.join("chat-bridge.sqlite")).unwrap();
            db.execute(
                "UPDATE documents SET name=? WHERE id=?",
                rusqlite::params![text(action, "name"), selected_pin(selected)?.object_id],
            )
            .unwrap();
        }
        "corrupt-receipt" => {
            let p = selected_pin(selected)?;
            let old = store
                .read_in(
                    "chat-bridge/native-call",
                    Reference::Current(&p.object_id),
                    Some(parent),
                )
                .await
                .map_err(|e| e.into_ivy())?;
            let p = pin(field(&old.value, "evidence"));
            let db = rusqlite::Connection::open(path.join("chat-bridge.sqlite")).unwrap();
            let original: String = db
                .query_row(
                    "SELECT value_json FROM revisions WHERE id=? AND revision=?",
                    rusqlite::params![p.object_id, p.revision],
                    |r| r.get(0),
                )
                .unwrap();
            let mut next = parse(&original, usize::MAX)?;
            next.insert("contentHash", digest("tampered").into());
            db.execute(
                "UPDATE revisions SET value_json=? WHERE id=? AND revision=?",
                rusqlite::params![canonical(&next, usize::MAX)?, p.object_id, p.revision],
            )
            .unwrap();
        }
        name => panic!("Unexpected step {name}"),
    }
    Ok(Json::Null)
}

#[tokio::test]
async fn durable_driver_and_project_policy_match_original() {
    let oracle = oracle();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let catalogs = CheckedNativeContracts::new(&root);
    let bytes = STANDARD.decode(text(oracle, "instructions")).unwrap();
    let instructions = parse(std::str::from_utf8(&bytes).unwrap(), usize::MAX)
        .unwrap()
        .as_js_string()
        .unwrap()
        .clone();
    let clock = Arc::new(Fixed(field(oracle, "now").as_f64().unwrap() as i64));
    let mut steps = 0;
    let mut errors = 0;
    for case in field(oracle, "cases").as_array().unwrap() {
        let path = std::env::temp_dir().join(format!("ivy-rust-driver-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let seed = field(field(oracle, "seeds"), &text(case, "version"));
        std::fs::write(
            path.join("chat-bridge.sqlite"),
            STANDARD.decode(text(seed, "database")).unwrap(),
        )
        .unwrap();
        let client = Arc::new(Transport(Mutex::new(VecDeque::new())));
        let tools = Arc::new(ToolClients::new(client.clone()));
        let admission = ChatAdmission::new(field(case, "definition")).unwrap();
        let mut store = Some(
            ChatStore::with_clock(client.clone(), None, Some(path.clone()), clock.clone())
                .await
                .unwrap(),
        );
        let mut body = field(case, "body").clone();
        let mut origin = field(case, "origin").clone();
        let mut selected = None;
        let mut last = 0;
        let mut aliases = HashMap::new();
        let parent = if case.get("parent") == Some(&"missing".into()) {
            "missing"
        } else {
            "original-input"
        };
        for (index, action) in field(case, "steps").as_array().unwrap().iter().enumerate() {
            let expected = &field(case, "expected").as_array().unwrap()[index];
            client.begin(field(expected, "transport"));
            let output = if text(action, "action") == "reopen" {
                drop(store.take());
                store = Some(
                    ChatStore::with_clock(client.clone(), None, Some(path.clone()), clock.clone())
                        .await
                        .unwrap(),
                );
                result(Ok(Json::Null))
            } else {
                let store = store.as_ref().unwrap();
                let driver = ChatNativeDriver::new(
                    store,
                    tools.clone(),
                    &admission,
                    &catalogs,
                    "chat-provider".into(),
                    &instructions,
                )
                .unwrap();
                let value = match text(action, "action").as_str() {
                    "advance" => match selected_pin(&selected) {
                        Ok(pin) => driver
                            .advance(parent, &pin, client.as_ref())
                            .await
                            .map(document),
                        Err(e) => Err(e),
                    },
                    "prevent" => match selected_pin(&selected) {
                        Ok(pin) => driver
                            .prevent(parent, &pin, client.as_ref())
                            .await
                            .map(document),
                        Err(e) => Err(e),
                    },
                    _ => {
                        step(
                            action,
                            store,
                            &driver,
                            &mut body,
                            &mut origin,
                            &mut selected,
                            &path,
                            parent,
                        )
                        .await
                    }
                };
                result(value)
            };
            assert!(
                client.0.lock().unwrap().is_empty(),
                "Missing required adapter call {} step {index}",
                text(case, "label")
            );
            let actual = snapshot(&path, output, &mut last, &mut aliases);
            let mut expected = expected.clone();
            expected.remove("transport");
            equal(
                &actual,
                &expected,
                &format!("{} step {index}", text(case, "label")),
            );
            steps += 1;
            if field(&actual, "output").get("error").is_some() {
                errors += 1;
            }
        }
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }
    assert!(field(oracle, "cases").as_array().unwrap().len() >= 67);
    assert!(steps >= 205);
    assert!(errors >= 37);
    eprintln!(
        "Original durable driver: {} workflows, {steps} steps, {errors} expected errors; production validation, exact RPC/guard order and SQLite snapshots.",
        field(oracle, "cases").as_array().unwrap().len()
    );
}

#[test]
fn native_owner_paths_keep_absolute_roots_and_compare_lexically() {
    for (value, expected) in [
        ("/one/./two/../three/", "/one/three"),
        ("/../", "/"),
        ("/Case/é", "/Case/é"),
        (r"C:\One\.\Two\..\Three\", "c:/one/three"),
        ("c:/../", "c:"),
        (r"\\Server\Share\..\Folder", "//server/share/folder"),
        (r"\\?\C:\Folder", "//?/c:/folder"),
        (
            r"\\?\UNC\Server\Share\..\Folder",
            "//?/unc/server/share/folder",
        ),
    ] {
        assert_eq!(native_path(&value.into()).unwrap(), expected.into());
    }
    for value in [
        "relative",
        "C:relative",
        "\\relative",
        "\\\\server",
        "/nul\0path",
    ] {
        assert_eq!(
            native_path(&value.into()).unwrap_err().code,
            "chat_project_mismatch"
        );
    }
    assert!(native_path(&JsString(vec![47, 0xd800])).is_err());
}
