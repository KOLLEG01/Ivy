use super::{Fixed, decode, document, equal, field, flag, merge, result, text};
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_chat_bridge::{
    admission::ChatAdmission,
    cancellation::ChatCancellation,
    chat_main::ChatMain,
    native_driver::ChatNativeDriver,
    notice_input::NoticeInstructions,
    notices::ChatNotices,
    operations::ChatOperations,
    store::{ChatStore, Destination, Pin, Reference, local::LocalStore},
};
use ivy_contracts::{
    IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts,
    hash::{digest, hash_json},
    json::{canonical, parse},
    operation::parse_operation_id,
};
use ivy_sdk::{
    rpc::{RequestFuture, RequestOptions, RpcClient},
    tools::ToolClients,
};
use std::{
    collections::VecDeque,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex, OnceLock},
};

fn progress(value: &str) {
    if let Some(path) = std::env::var_os("IVY_CHAT_MAIN_TRACE_CAPTURE") {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(file, "{value}").unwrap();
    }
}

fn oracle() -> &'static Json {
    static ORACLE: OnceLock<Json> = OnceLock::new();
    ORACLE.get_or_init(|| {
        let output = Command::new("node")
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .arg("tests/rust/fixtures/chat-main-oracle.mjs")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if let Some(path) = std::env::var_os("IVY_CHAT_MAIN_ORACLE_CAPTURE") {
            std::fs::write(path, &output.stdout).unwrap();
        }
        parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
    })
}
fn alias_string(value: &str, aliases: &[(String, String)]) -> String {
    let mut value = value.to_owned();
    for _ in 0..8 {
        let before = value.clone();
        for (id, alias) in aliases {
            value = value.replace(id, alias);
        }
        if before == value {
            break;
        }
    }
    value
}
fn strings(value: &Json, convert: &impl Fn(&str) -> String) -> Json {
    match value {
        Json::String(s) => s
            .to_utf8()
            .ok()
            .map(|s| Json::from(convert(&s)))
            .unwrap_or_else(|| value.clone()),
        Json::Array(values) => Json::Array(values.iter().map(|v| strings(v, convert)).collect()),
        Json::Object(fields) => Json::Object(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), strings(v, convert)))
                .collect(),
        ),
        _ => value.clone(),
    }
}
fn aliases(path: &Path) -> Vec<(String, String)> {
    let db = rusqlite::Connection::open(path.join("chat-bridge.sqlite")).unwrap();
    let mut result: Vec<_> = db
        .prepare("SELECT id,name FROM documents")
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                format!("<local:{}>", r.get::<_, String>(1)?),
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    result.extend(
        db.prepare("SELECT id,name FROM blobs")
            .unwrap()
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    format!("<blob:{}>", r.get::<_, String>(1)?),
                ))
            })
            .unwrap()
            .map(|r| r.unwrap()),
    );
    result
}
struct Replay {
    pending: VecDeque<Json>,
    mutations: Vec<(String, String)>,
    records: Json,
}
struct Client {
    path: PathBuf,
    state: Mutex<Replay>,
}
impl Client {
    fn begin(&self, value: &Json) {
        let mut state = self.state.lock().unwrap();
        assert!(state.pending.is_empty());
        state
            .pending
            .extend(value.as_array().unwrap().iter().cloned());
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
            let mut state = self.state.lock().unwrap();
            let expected = state.pending.pop_front().expect("Unexpected Main RPC");
            assert_eq!(text(&expected, "method"), method, "Main RPC order");
            let mut all = aliases(&self.path);
            if let Some(id) = params.get("mutationId") {
                let id = id.as_str()?;
                assert_eq!(parse_operation_id(&id)?.runtime_epoch, "chat");
                let logical = text(field(&expected, "params"), "mutationId");
                if let Some((_, old)) = state.mutations.iter().find(|(raw, _)| raw == &id) {
                    assert_eq!(old, &logical, "Mutation replay identity");
                } else {
                    assert!(
                        !state.mutations.iter().any(|(_, alias)| alias == &logical),
                        "Changed original mutation identity"
                    );
                    state.mutations.push((id, logical));
                }
            }
            all.extend(state.mutations.iter().cloned());
            equal(
                &strings(&params, &|s| alias_string(s, &all)),
                field(&expected, "params"),
                "Exact Main RPC arguments",
            );
            if flag(&expected, "competingMainRevision") {
                // An independent real SQLite writer commits between selection
                // and the original Queue CAS. Do not supply a saved outcome.
                let db = rusqlite::Connection::open(self.path.join("chat-bridge.sqlite")).unwrap();
                let name: String = db
                    .query_row(
                        "SELECT name FROM documents WHERE key='chat-bridge/main'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                drop(db);
                let mut other = LocalStore::open(Some(&self.path)).unwrap();
                let current = other
                    .named("chat-bridge/main", &name, None)
                    .unwrap()
                    .unwrap();
                other
                    .write(
                        "chat-bridge/main",
                        &current.value,
                        Destination::Update {
                            object_id: &current.pin.object_id,
                            expected_revision: current.pin.revision,
                        },
                        field(oracle(), "now").as_f64().unwrap() as i64,
                        &text(&current.value, "updatedAt"),
                    )
                    .unwrap();
                other.close().unwrap();
            }
            if flag(&expected, "assertClaim") {
                let db = rusqlite::Connection::open(self.path.join("chat-bridge.sqlite")).unwrap();
                let id = text(field(&params, "arguments"), "operationId");
                let value:String=db.query_row("SELECT r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='chat-bridge/native-call' AND json_extract(r.value_json,'$.operationId')=?",[id],|r|r.get(0)).unwrap();
                let call = parse(&value, usize::MAX)?;
                // The parent is the original local operation; query it directly.
                let parent:String=db.query_row("SELECT parent_id FROM documents WHERE key='chat-bridge/native-call' AND json_extract((SELECT value_json FROM revisions WHERE id=documents.id AND revision=documents.current_revision),'$.operationId')=?",[text(&call,"operationId")],|r|r.get(0)).unwrap();
                let original:String=db.query_row("SELECT r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.id=?",[&parent],|r|r.get(0)).unwrap();
                let original = parse(&original, usize::MAX)?;
                assert_eq!(field(&original, "phase"), &"applying".into());
                let call_id = text(field(&original, "nativeCall"), "objectId");
                let attached:String=db.query_row("SELECT id FROM documents WHERE key='chat-bridge/native-call' AND parent_id=?",[&parent],|r|r.get(0)).unwrap();
                assert_eq!(call_id, attached);
                let claimed:i64=db.query_row("SELECT COUNT(*) FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='chat-bridge/main' AND json_extract(r.value_json,'$.pendingAction.objectId')=? AND json_array_length(json_extract(r.value_json,'$.queue'))=0",[parent],|r|r.get(0)).unwrap();
                assert_eq!(
                    claimed, 1,
                    "Attachment and empty current claim precede native I/O"
                );
            }
            let mut reverse: Vec<_> = all
                .iter()
                .map(|(raw, alias)| (alias_string(alias, &all), raw.clone()))
                .collect();
            reverse.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
            let decode_alias = |s: &str| {
                let mut result = s.to_owned();
                for (alias, raw) in &reverse {
                    result = result.replace(alias, raw);
                }
                result
            };
            if let Some(records) = expected.get("records") {
                state.records = strings(records, &decode_alias);
            }
            decode(&strings(field(&expected, "result"), &decode_alias))
        })
    }
}
fn snapshot(path: &Path, client: &Client, output: Json) -> Json {
    let db = rusqlite::Connection::open(path.join("chat-bridge.sqlite")).unwrap();
    let documents: Vec<_> = db
        .prepare("SELECT id,key,name,parent_id,current_revision FROM documents")
        .unwrap()
        .query_map([], |r| {
            Ok(Json::object([
                ("id", r.get::<_, String>(0)?.into()),
                ("key", r.get::<_, String>(1)?.into()),
                ("name", r.get::<_, String>(2)?.into()),
                (
                    "parentId",
                    r.get::<_, Option<String>>(3)?
                        .map(Json::from)
                        .unwrap_or(Json::Null),
                ),
                ("revision", Json::Number(r.get::<_, i64>(4)? as f64)),
            ]))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let revisions:Vec<_>=db.prepare("SELECT d.name,d.key,r.id,r.revision,r.value_json FROM documents d JOIN revisions r ON r.id=d.id").unwrap().query_map([],|r|Ok(Json::object([
        ("name",r.get::<_,String>(0)?.into()),("key",r.get::<_,String>(1)?.into()),("id",r.get::<_,String>(2)?.into()),("revision",Json::Number(r.get::<_,i64>(3)? as f64)),("value",parse(&r.get::<_,String>(4)?,usize::MAX).unwrap())]))).unwrap().map(|r|r.unwrap()).collect();
    let blobs: Vec<_> = db
        .prepare("SELECT id,key,name,parent_id,bytes FROM blobs")
        .unwrap()
        .query_map([], |r| {
            let bytes = r.get::<_, Vec<u8>>(4)?;
            Ok(Json::object([
                ("id", r.get::<_, String>(0)?.into()),
                ("key", r.get::<_, String>(1)?.into()),
                ("name", r.get::<_, String>(2)?.into()),
                (
                    "parentId",
                    r.get::<_, Option<String>>(3)?
                        .map(Json::from)
                        .unwrap_or(Json::Null),
                ),
                ("byteLength", Json::Number(bytes.len() as f64)),
                ("contentHash", digest(bytes).into()),
            ]))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let state = client.state.lock().unwrap();
    let mut all = aliases(path);
    all.extend(state.mutations.clone());
    let normalize = |v: &Json| strings(v, &|s| alias_string(s, &all));
    let sort = |values: Vec<Json>| {
        let mut values: Vec<_> = values.iter().map(&normalize).collect();
        values.sort_by_cached_key(|v| canonical(v, usize::MAX).unwrap());
        Json::Array(values)
    };
    Json::object([
        ("output", normalize(&output)),
        ("documents", sort(documents)),
        ("revisions", sort(revisions)),
        ("blobs", sort(blobs)),
        ("records", normalize(&state.records)),
    ])
}
fn operation(label: &str) -> Json {
    format!("chat:1790000000123:{label}").into()
}
fn creation(main: &ChatMain<'_>, caller: &JsString, label: &str, revision: Json) -> Json {
    Json::object([
        ("action", "createMain".into()),
        ("operationId", operation(label)),
        ("expectedBridge", main.admission().expected(caller)),
        ("expectedMainRevision", revision),
        ("reason", "Explicit synthetic Main creation.".into()),
    ])
}
fn object_pin(value: &Json) -> Pin {
    Pin {
        object_id: text(value, "objectId"),
        revision: field(value, "revision").as_f64().unwrap() as i64,
    }
}
async fn step(action: &Json, main: &ChatMain<'_>, client: &Client) -> IvyResult<Json> {
    let caller = action
        .get("caller")
        .and_then(Json::as_js_string)
        .cloned()
        .unwrap_or_else(|| "alice".into());
    let label = |default: &str| {
        action
            .get("label")
            .and_then(|v| v.as_str().ok())
            .unwrap_or_else(|| default.into())
    };
    match text(action, "action").as_str() {
        "create" => {
            let revision = if flag(action, "current") {
                main.find()
                    .await?
                    .map(|m| Json::Number(m.pin.revision as f64))
                    .unwrap_or(Json::Null)
            } else {
                action.get("revision").cloned().unwrap_or(Json::Null)
            };
            let mut request = creation(main, &caller, &label("create"), revision);
            if let Some(patch) = action.get("patch") {
                merge(&mut request, patch);
            }
            main.create(&caller, &request).await
        }
        "ensure" => {
            main.ensure(
                &caller,
                operation(&label("incoming")).as_js_string().unwrap(),
            )
            .await
        }
        "find" => Ok(main.find().await?.map(document).unwrap_or(Json::Null)),
        "recover" => {
            main.recover_pending().await?;
            Ok(Json::Null)
        }
        "send" | "claim-send" => {
            let current = main.find().await?;
            let binding = current
                .as_ref()
                .and_then(|m| m.value.get("binding"))
                .cloned()
                .unwrap_or(Json::Null);
            let label = label("main-send");
            let request = Json::object([
                ("action", "send".into()),
                ("operationId", operation(&label)),
                ("expectedBridge", main.admission().expected(&caller)),
                (
                    "channel",
                    field(main.admission().definition(), "channels")
                        .as_array()
                        .unwrap()[0]
                        .required("channel")?
                        .clone(),
                ),
                ("messageId", label.into()),
                (
                    "expectedBinding",
                    if flag(action, "auto") {
                        Json::Null
                    } else {
                        binding
                    },
                ),
                (
                    "payload",
                    Json::object([
                        ("text", "Original queued input.".into()),
                        ("images", Json::Array(vec![])),
                    ]),
                ),
            ]);
            if text(action, "action") == "send" {
                main.queue().send(&caller, &request).await
            } else {
                // Construct a claimed receipt through the actual original
                // store transitions; useful for hostile/stale retained state.
                let accepted = main.operations.begin(&caller, &request).await?;
                let current = current.unwrap();
                let mut value = current.value.clone();
                value.insert("pendingAction", accepted.pin.json());
                value.insert("updatedAt", field(&accepted.value, "createdAt").clone());
                main.store()
                    .write(
                        "chat-bridge/main",
                        &value,
                        "fixture-claim-send",
                        Destination::Update {
                            object_id: &current.pin.object_id,
                            expected_revision: current.pin.revision,
                        },
                    )
                    .await
                    .map_err(|e| e.into_ivy())?;
                Ok(Json::Null)
            }
        }
        "cancel" => {
            let request = if flag(action, "replay") {
                field(
                    &main
                        .operations
                        .find(&caller, operation("main-cancel").as_js_string().unwrap())
                        .await?
                        .unwrap()
                        .value,
                    "request",
                )
                .clone()
            } else {
                let input = client
                    .state
                    .lock()
                    .unwrap()
                    .records
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|r| {
                        field(field(r, "object"), "contractKey") == &"chat-bridge/input".into()
                    })
                    .unwrap()
                    .clone();
                Json::object([
                    ("action", "cancel".into()),
                    ("operationId", operation("main-cancel")),
                    ("expectedBridge", main.admission().expected(&caller)),
                    ("inputId", field(field(&input, "object"), "id").clone()),
                    (
                        "expectedRevision",
                        field(field(&input, "revision"), "revision").clone(),
                    ),
                    ("reason", "Explicit synthetic cancellation.".into()),
                ])
            };
            ChatCancellation::new(main.operations, main, &notice_asset())
                .cancel(&caller, &request)
                .await
        }
        "patch-pending" => {
            let current = main.find().await?.unwrap();
            let saved = main
                .store()
                .read(
                    "chat-bridge/operation",
                    Reference::Pinned(&object_pin(field(&current.value, "pendingAction"))),
                )
                .await
                .map_err(|e| e.into_ivy())?;
            let mut value = saved.value.clone();
            if let Some(patch) = action.get("value") {
                merge(&mut value, patch);
            }
            if let Some(patch) = action.get("requestPatch") {
                let mut request = field(&value, "request").clone();
                merge(&mut request, patch);
                value.insert("request", request);
            }
            if flag(action, "useBindingAsNativeCall") {
                value.insert("nativeCall", field(&current.value, "binding").clone());
            }
            if flag(action, "rehash") {
                value.insert("requestHash", hash_json(field(&value, "request"))?.into());
            }
            let saved = main
                .store()
                .write(
                    "chat-bridge/operation",
                    &value,
                    "fixture-pending-patch",
                    Destination::Update {
                        object_id: &saved.pin.object_id,
                        expected_revision: saved.pin.revision,
                    },
                )
                .await
                .map_err(|e| e.into_ivy())?;
            let mut value = current.value.clone();
            value.insert("pendingAction", saved.json());
            main.store()
                .write(
                    "chat-bridge/main",
                    &value,
                    "fixture-pending-pin",
                    Destination::Update {
                        object_id: &current.pin.object_id,
                        expected_revision: current.pin.revision,
                    },
                )
                .await
                .map_err(|e| e.into_ivy())?;
            Ok(Json::Null)
        }
        "notify" => {
            ChatNotices::new(main.operations, main, &notice_asset())
                .notify(
                    &caller,
                    &Json::object([
                        ("action", "notify".into()),
                        ("operationId", operation(&label("main-notice"))),
                        (
                            "source",
                            Json::object([
                                ("objectId", "synthetic-source".into()),
                                ("revision", Json::Number(1.0)),
                            ]),
                        ),
                        ("text", "Original private context.".into()),
                    ]),
                )
                .await
        }
        "patch-main" => {
            let current = main.find().await?.unwrap();
            let mut value = current.value.clone();
            merge(&mut value, field(action, "value"));
            main.store()
                .write(
                    "chat-bridge/main",
                    &value,
                    "fixture-main-patch",
                    Destination::Update {
                        object_id: &current.pin.object_id,
                        expected_revision: current.pin.revision,
                    },
                )
                .await
                .map_err(|e| e.into_ivy())?;
            Ok(Json::Null)
        }
        "state" | "phase" => Ok(Json::Null),
        "constructor-store" => {
            let other = ChatStore::open(main.store().client.clone(), None, None)
                .await
                .map_err(|e| e.into_ivy())?;
            let ops = ChatOperations::new(&other, main.admission())?;
            ChatMain::new(&ops, main.native, &notice_asset())?;
            Ok(Json::Null)
        }
        "constructor-definition" => {
            let mut value = main.admission().definition().clone();
            value.insert("workspaceId", "foreign".into());
            let admission = ChatAdmission::new(&value)?;
            let ops = ChatOperations::new(main.store(), &admission)?;
            ChatMain::new(&ops, main.native, &notice_asset())?;
            Ok(Json::Null)
        }
        other => panic!("Unexpected Main action {other}"),
    }
}
fn notice_asset() -> NoticeInstructions {
    let bytes = STANDARD
        .decode(text(oracle(), "noticeInstructions"))
        .unwrap();
    NoticeInstructions::from_json(&parse(std::str::from_utf8(&bytes).unwrap(), usize::MAX).unwrap())
        .unwrap()
}

#[tokio::test]
async fn original_main_creation_recovery_and_live_claims() {
    progress("start");
    let oracle = oracle();
    progress("original corpus parsed");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let catalogs = CheckedNativeContracts::new(root);
    let bytes = STANDARD.decode(text(oracle, "instructions")).unwrap();
    let instructions = parse(std::str::from_utf8(&bytes).unwrap(), usize::MAX)
        .unwrap()
        .as_js_string()
        .unwrap()
        .clone();
    let notice = notice_asset();
    let clock = Arc::new(Fixed(field(oracle, "now").as_f64().unwrap() as i64));
    let mut steps = 0;
    let mut errors = 0;
    for case in field(oracle, "cases").as_array().unwrap() {
        if flag(case, "referenceOnly") {
            continue;
        }
        progress(&format!("case {}", text(case, "label")));
        let seed = field(field(oracle, "seeds"), &text(case, "seed"));
        let path = std::env::temp_dir().join(format!("ivy-rust-main-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(
            path.join("chat-bridge.sqlite"),
            STANDARD.decode(text(seed, "database")).unwrap(),
        )
        .unwrap();
        let client = Arc::new(Client {
            path: path.clone(),
            state: Mutex::new(Replay {
                pending: VecDeque::new(),
                mutations: vec![],
                records: field(seed, "records").clone(),
            }),
        });
        let tools = Arc::new(ToolClients::new(client.clone()));
        let mut definition = field(seed, "definition").clone();
        let mut store = Some(
            ChatStore::with_clock(client.clone(), None, Some(path.clone()), clock.clone())
                .await
                .unwrap(),
        );
        for (index, action) in field(case, "steps").as_array().unwrap().iter().enumerate() {
            progress(&format!(
                "{} step {index} {}",
                text(case, "label"),
                text(action, "action")
            ));
            let expected = &field(case, "expected").as_array().unwrap()[index];
            client.begin(field(expected, "transport"));
            let name = text(action, "action");
            let value = if name == "reopen" || name == "reconfigure" {
                if name == "reconfigure" {
                    let mut channels = field(&definition, "channels").clone();
                    let Json::Array(values) = &mut channels else {
                        unreachable!()
                    };
                    values[0].insert("displayName", "Changed configuration".into());
                    definition.insert("channels", channels);
                    if let Some(patch) = action.get("value") {
                        merge(&mut definition, patch);
                    }
                }
                drop(store.take());
                store = Some(
                    ChatStore::with_clock(client.clone(), None, Some(path.clone()), clock.clone())
                        .await
                        .unwrap(),
                );
                Ok(Json::Null)
            } else {
                let store = store.as_ref().unwrap();
                let admission = ChatAdmission::new(&definition).unwrap();
                let ops = ChatOperations::new(store, &admission).unwrap();
                let native = ChatNativeDriver::new(
                    store,
                    tools.clone(),
                    &admission,
                    &catalogs,
                    "chat-provider".into(),
                    &instructions,
                )
                .unwrap();
                let main = ChatMain::new(&ops, &native, &notice).unwrap();
                if name == "resume" {
                    let db = rusqlite::Connection::open(path.join("chat-bridge.sqlite")).unwrap();
                    let selected:(String,i64)=db.query_row("SELECT d.id,d.current_revision FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='chat-bridge/main' AND json_extract(r.value_json,'$.definitionHash')!=?",[admission.definition_hash()],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
                    drop(db);
                    let caller = action
                        .get("caller")
                        .and_then(Json::as_js_string)
                        .cloned()
                        .unwrap_or_else(|| "alice".into());
                    let label = action
                        .get("label")
                        .and_then(|v| v.as_str().ok())
                        .unwrap_or_else(|| "resume".into());
                    let mut request = creation(&main, &caller, &label, Json::Null);
                    request.insert(
                        "resumeMain",
                        Json::object([
                            ("objectId", selected.0.into()),
                            (
                                "revision",
                                Json::Number(
                                    (selected.1 - i64::from(flag(action, "stale"))) as f64,
                                ),
                            ),
                        ]),
                    );
                    main.create(&caller, &request).await
                } else {
                    step(action, &main, &client).await
                }
            };
            let actual = snapshot(&path, &client, result(value));
            assert!(
                client.state.lock().unwrap().pending.is_empty(),
                "Missing Main RPC {} step {index}",
                text(case, "label")
            );
            let mut expected = expected.clone();
            expected.remove("transport");
            equal(
                &actual,
                &expected,
                &format!("Main {} step {index}", text(case, "label")),
            );
            steps += 1;
            if field(&actual, "output").get("error").is_some() {
                errors += 1;
            }
        }
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }
    assert!(field(oracle, "cases").as_array().unwrap().len() >= 40);
    assert!(steps >= 150);
    assert!(errors >= 15);
    progress(&format!("complete steps={steps} expectedErrors={errors}"));
    eprintln!(
        "Original Main: {} workflows, {steps} steps, {errors} expected errors, twelve exact original checkpoint copies.",
        field(oracle, "cases").as_array().unwrap().len()
    );
}
