#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_chat_bridge::{
    admission::ChatAdmission,
    chat_main::{ChatMain, ConfiguredMainResume},
    native_driver::ChatNativeDriver,
    notice_input::NoticeInstructions,
    operations::ChatOperations,
    queue::main_name,
    store::{ChatStore, Destination, Document, Reference},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, JsString, Outcome,
    checked_native_contract::CheckedNativeContracts,
    hash::{hash_json, tool_definition_hash},
    json::parse,
    native_backend::{BackendIdentity, NativeProvider},
    native_contract::NativeContract,
};
use ivy_sdk::{
    clock::Clock,
    rpc::{RequestFuture, RequestOptions, RpcClient},
    tools::ToolClients,
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Instant,
};

struct Fixed;
impl Clock for Fixed {
    fn utc_millis(&self) -> i64 {
        1790000000123
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
async fn open(client: Arc<Client>, disk: &Disk) -> ChatStore {
    ChatStore::with_clock(client, None, Some(disk.0.clone()), Arc::new(Fixed))
        .await
        .unwrap()
}

fn field<'a>(v: &'a Json, key: &str) -> &'a Json {
    v.required(key).unwrap()
}
fn text(v: &Json, key: &str) -> String {
    field(v, key).as_str().unwrap()
}
fn fixture() -> Json {
    let mut corpus = parse(
        std::str::from_utf8(&reference_data::read(include_bytes!(
            "fixtures/chat-service-active-cases.json.gz"
        )))
        .unwrap(),
        usize::MAX,
    )
    .unwrap();
    corpus.insert(
        "createdMain",
        parse(
            std::str::from_utf8(&reference_data::read(include_bytes!(
                "fixtures/chat-created-main-seeds.json.gz"
            )))
            .unwrap(),
            usize::MAX,
        )
        .unwrap(),
    );
    corpus
}
fn seed(corpus: &Json) -> &Json {
    field(field(corpus, "createdMain"), "0.159.2")
}
fn selected(
    definition: &Json,
    catalogs: &CheckedNativeContracts,
    account: &str,
) -> (Json, Arc<NativeContract>) {
    let mut result = definition.clone();
    let plan = result.get_mut("nativePlan").unwrap();
    let backend = BackendIdentity::new(
        "codex".to_owned(),
        NativeProvider::Codex,
        account.to_owned(),
    )
    .unwrap();
    let catalog = Arc::new(
        catalogs
            .checked_native_contract(
                field(plan, "nativeVersion").as_js_string().unwrap(),
                Some(&Json::object([(
                    "sourceHash",
                    field(plan, "catalogSourceHash").clone(),
                )])),
            )
            .unwrap()
            .with_backend(Some(backend.clone())),
    );
    plan.insert("backend", backend.json());
    for (key, method) in [
        ("threadStart", "thread/start"),
        ("threadResume", "thread/resume"),
        ("turnStart", "turn/start"),
        ("turnInterrupt", "turn/interrupt"),
    ] {
        plan.get_mut("definitions")
            .unwrap()
            .insert(key, catalog.definition_hash(&method.into()).unwrap().into());
    }
    (result, catalog)
}
struct Disk(PathBuf);
impl Disk {
    fn new(seed: &Json) -> Self {
        let path = std::env::temp_dir().join(format!("ivy-chat-resume-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(
            path.join("chat-bridge.sqlite"),
            STANDARD.decode(text(seed, "database")).unwrap(),
        )
        .unwrap();
        Self(path)
    }
}
impl Drop for Disk {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct State {
    calls: Vec<Json>,
    receipts: HashMap<String, Json>,
    records: HashMap<String, Json>,
    fault: String,
    lose_reply: bool,
    receipts_unavailable: bool,
    invokes: usize,
}
struct Client {
    contract: Arc<NativeContract>,
    original: Json,
    state: Mutex<State>,
}
impl Client {
    fn new(contract: Arc<NativeContract>, seed: &Json) -> Self {
        Self {
            contract,
            original: field(seed, "journal").as_array().unwrap()[0]
                .as_array()
                .unwrap()[1]
                .clone(),
            state: Mutex::new(State {
                calls: vec![],
                receipts: HashMap::new(),
                records: HashMap::new(),
                fault: String::new(),
                lose_reply: false,
                receipts_unavailable: false,
                invokes: 0,
            }),
        }
    }
    fn backend(&self) -> Json {
        self.contract.backend().unwrap().json()
    }
    fn observation(&self, args: &Json, read: bool) -> Json {
        let method = field(args, "method").clone();
        let params = field(args, "params").clone();
        let mut result = self.original.clone();
        result.insert("backend", self.backend());
        result.insert("method", method.clone());
        result.insert("params", params.clone());
        result.insert(
            "requestHash",
            hash_json(&Json::object([("method", method), ("params", params)]))
                .unwrap()
                .into(),
        );
        if read {
            for key in ["operationId", "phase", "createdAt", "updatedAt", "code"] {
                result.remove(key);
            }
            result.insert("observationId", "fresh-thread-read".into());
            result.insert("observedAt", "2026-09-21T14:13:20.123Z".into());
            result.insert("catalogHash", self.contract.catalog_hash().into());
            result.insert(
                "reply",
                Json::object([(
                    "result",
                    Json::object([(
                        "thread",
                        field(field(field(&self.original, "reply"), "result"), "thread").clone(),
                    )]),
                )]),
            );
        } else {
            result.insert("operationId", field(args, "operationId").clone());
        }
        result
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
            state.calls.push(Json::object([
                ("method", method.into()),
                ("params", params.clone()),
            ]));
            match method {
                "serviceNodes.get" => Ok(Json::object([
                    ("principalId", "chat-service".into()),
                    ("serviceName", "chat-bridge".into()),
                    ("connected", true.into()),
                    ("synced", true.into()),
                ])),
                "system.status" => Ok(Json::object([("runtimeEpoch", "chat".into())])),
                "objects.query" => Ok(Json::object([
                    ("items", Json::Array(vec![])),
                    ("nextCursor", Json::Null),
                ])),
                "objects.write" => {
                    let id = format!("public-{}", state.records.len());
                    let record = Json::object([
                        ("object", Json::object([("id", id.clone().into())])),
                        ("revision", Json::object([("revision", 1.0.into())])),
                    ]);
                    state.records.insert(id, record.clone());
                    Ok(record)
                }
                "tools.list" => {
                    let namespace = text(&params, "namespace");
                    let name = text(&params, "namePrefix");
                    let definition = if namespace == "codex" {
                        self.contract.definition(&name.clone().into())?
                    } else {
                        Json::object([
                            ("namespace", namespace.clone().into()),
                            ("name", name.clone().into()),
                            ("interfaceVersion", "1.0.0".into()),
                            ("inputSchema", Json::object::<&str>([])),
                            ("outputSchema", Json::object::<&str>([])),
                        ])
                    };
                    Ok(Json::object([
                        (
                            "provider",
                            Json::object([(
                                "node",
                                Json::object([("serviceNodeId", "owner".into())]),
                            )]),
                        ),
                        (
                            "items",
                            Json::Array(vec![Json::object([
                                ("qualifiedName", format!("{namespace}.{name}").into()),
                                ("definitionHash", tool_definition_hash(&definition)?.into()),
                                ("definition", definition),
                                ("catalogHash", self.contract.catalog_hash().into()),
                            ])]),
                        ),
                        ("nextCursor", Json::Null),
                    ]))
                }
                "tools.call" => {
                    let args = field(&params, "arguments");
                    assert_eq!(args.get("backend"), Some(&self.backend()));
                    match text(&params, "qualifiedName").as_str() {
                        "agent.storageIdentity" => {
                            let mut origin = Json::object([
                                ("serviceNodeId", "owner".into()),
                                ("originalServiceNodeId", "owner".into()),
                                ("provider", "codex".into()),
                                ("backend", self.backend()),
                            ]);
                            match state.fault.as_str() {
                                "account" => origin
                                    .get_mut("backend")
                                    .unwrap()
                                    .insert("accountId", "foreign".into()),
                                "missing-account" => {
                                    origin.remove("backend");
                                }
                                "provider" => origin.insert("provider", "claude".into()),
                                "origin" => {
                                    origin.insert("originalServiceNodeId", "different-owner".into())
                                }
                                _ => {}
                            }
                            Ok(origin)
                        }
                        "agent.read" => {
                            let mut read = self.observation(args, true);
                            let thread = read
                                .get_mut("reply")
                                .unwrap()
                                .get_mut("result")
                                .unwrap()
                                .get_mut("thread")
                                .unwrap();
                            match state.fault.as_str() {
                                "thread" => thread.insert("id", "foreign-thread".into()),
                                "ephemeral" => thread.insert("ephemeral", true.into()),
                                "cwd" => thread.insert("cwd", "/foreign".into()),
                                "project" => thread.insert("projectId", "foreign-project".into()),
                                "active" => thread.insert(
                                    "status",
                                    Json::object([
                                        ("type", "active".into()),
                                        ("activeFlags", Json::Array(vec![])),
                                    ]),
                                ),
                                _ => {}
                            }
                            Ok(read)
                        }
                        "agent.status" => Ok(Json::object([
                            ("backend", self.backend()),
                            ("serviceNodeId", "owner".into()),
                            ("hostId", "native-host".into()),
                            (
                                "nativeVersion",
                                field(self.contract.catalog(), "version").clone(),
                            ),
                            (
                                "nativeExecutableHash",
                                field(self.contract.catalog(), "nativeExecutableHash").clone(),
                            ),
                            ("catalogHash", self.contract.catalog_hash().into()),
                            ("epoch", "original-epoch".into()),
                            ("pid", 42.0.into()),
                            ("state", "ready".into()),
                            ("observedAt", "2026-09-21T14:13:20.123Z".into()),
                            ("code", Json::Null),
                            ("initialized", Json::object::<&str>([])),
                            ("pendingInputs", 0.0.into()),
                            (
                                "operations",
                                Json::object([
                                    ("retained", 0.0.into()),
                                    ("maximum", 100.0.into()),
                                    ("bytes", 0.0.into()),
                                    ("maximumBytes", 1048576.0.into()),
                                ]),
                            ),
                            ("observedMethods", Json::Array(vec![])),
                        ])),
                        "agent.projects" => Ok(Json::object([
                            ("source", "native".into()),
                            ("observedAt", "2026-09-21T14:13:20.123Z".into()),
                            ("backend", self.backend()),
                            (
                                "projects",
                                Json::Array(vec![Json::object([
                                    ("nativeId", "/synthetic/project".into()),
                                    ("source", "native".into()),
                                    ("name", "Project".into()),
                                    ("paths", Json::Array(vec!["/synthetic/project".into()])),
                                ])]),
                            ),
                        ])),
                        "agent.operation" => {
                            if state.receipts_unavailable {
                                return Err(IvyError::new(
                                    "service_unavailable",
                                    "Original receipt temporarily unavailable.",
                                )
                                .with_outcome(Outcome::Unknown));
                            }
                            state
                                .receipts
                                .get(&text(args, "operationId"))
                                .cloned()
                                .ok_or_else(|| {
                                    let mut error =
                                        IvyError::new("not_found", "No original operation.");
                                    error.details = Some(Json::object([
                                        ("kind", "agent_operation_absent".into()),
                                        ("serviceNodeId", "owner".into()),
                                        ("backend", self.backend()),
                                        ("operationId", field(args, "operationId").clone()),
                                        ("epoch", "original-epoch".into()),
                                    ]));
                                    error
                                })
                        }
                        "agent.invoke" => {
                            assert_eq!(args.get("method"), Some(&"thread/resume".into()));
                            assert_eq!(
                                field(args, "params").get("threadId"),
                                Some(&"native-main-1".into())
                            );
                            let receipt = self.observation(args, false);
                            let id = text(args, "operationId");
                            assert!(
                                !state.receipts.contains_key(&id),
                                "Never redispatch the original resume"
                            );
                            state.receipts.insert(id, receipt.clone());
                            state.invokes += 1;
                            if state.lose_reply {
                                state.receipts_unavailable = true;
                                return Err(IvyError::new(
                                    "connection_lost",
                                    "Original resume reply lost.",
                                )
                                .with_outcome(Outcome::Unknown));
                            }
                            Ok(receipt)
                        }
                        name => panic!("Unexpected native tool {name}"),
                    }
                }
                name => panic!("Unexpected RPC {name}"),
            }
        })
    }
}
async fn original(store: &ChatStore, seed: &Json) -> Document {
    store
        .named(
            "chat-bridge/main",
            &main_name(field(seed, "definition")).unwrap(),
        )
        .await
        .unwrap()
        .unwrap()
}
fn directive(main: &Document) -> Json {
    Json::object([
        ("operationId", "chat:1790000000123:configured-main".into()),
        ("main", main.pin.json()),
        ("binding", field(&main.value, "binding").clone()),
    ])
}
fn instructions(corpus: &Json) -> (JsString, NoticeInstructions) {
    let decoded = |key| {
        parse(
            std::str::from_utf8(&STANDARD.decode(text(corpus, key)).unwrap()).unwrap(),
            usize::MAX,
        )
        .unwrap()
    };
    (
        decoded("instructions").into_js_string().unwrap(),
        NoticeInstructions::from_json(&decoded("noticeInstructions")).unwrap(),
    )
}
async fn resume(
    store: &ChatStore,
    client: Arc<Client>,
    admission: &ChatAdmission,
    catalogs: &CheckedNativeContracts,
    corpus: &Json,
    directive: &Json,
) -> ivy_contracts::IvyResult<()> {
    let (instructions, notices) = instructions(corpus);
    let driver = ChatNativeDriver::new(
        store,
        Arc::new(ToolClients::new(client)),
        admission,
        catalogs,
        "chat-provider".into(),
        &instructions,
    )?;
    let operations = ChatOperations::new(store, admission)?;
    ChatMain::new(&operations, &driver, &notices)?
        .resume_configured(&ConfiguredMainResume::from_json(directive)?)
        .await
}
fn catalogs() -> CheckedNativeContracts {
    CheckedNativeContracts::new(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
}

#[tokio::test]
async fn legacy_main_resumes_once_and_reconnect_retains_binding_without_new_native_work() {
    let corpus = fixture();
    let seed = seed(&corpus);
    let disk = Disk::new(seed);
    let catalogs = catalogs();
    let (definition, contract) = selected(field(seed, "definition"), &catalogs, "work");
    let admission = ChatAdmission::new(&definition).unwrap();
    let client = Arc::new(Client::new(contract, seed));
    let store = open(client.clone(), &disk).await;
    let source = original(&store, seed).await;
    let directive = directive(&source);
    resume(
        &store,
        client.clone(),
        &admission,
        &catalogs,
        &corpus,
        &directive,
    )
    .await
    .unwrap();
    let main = store
        .named("chat-bridge/main", &main_name(&definition).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(main.value.get("pendingAction"), Some(&Json::Null));
    let binding = field(&main.value, "binding").clone();
    let source_after = store
        .read(
            "chat-bridge/main",
            Reference::Current(&source.pin.object_id),
        )
        .await
        .unwrap();
    assert_eq!(source_after.pin, source.pin);
    assert_eq!(source_after.value, source.value);
    store.close().await.unwrap();
    let before = client.state.lock().unwrap().calls.len();
    client.state.lock().unwrap().fault = "active".to_owned();
    let store = open(client.clone(), &disk).await;
    resume(
        &store,
        client.clone(),
        &admission,
        &catalogs,
        &corpus,
        &directive,
    )
    .await
    .unwrap();
    assert_eq!(
        client.state.lock().unwrap().calls.len(),
        before,
        "A completed transition needs no native reads or redispatch on reconnect"
    );
    assert_eq!(client.state.lock().unwrap().invokes, 1);
    assert_eq!(
        store
            .named("chat-bridge/main", &main_name(&definition).unwrap())
            .await
            .unwrap()
            .unwrap()
            .value
            .get("binding"),
        Some(&binding)
    );
    store.close().await.unwrap();
}

#[tokio::test]
async fn lost_resume_reply_recovers_exact_original_operation_after_reopen() {
    let corpus = fixture();
    let seed = seed(&corpus);
    let disk = Disk::new(seed);
    let catalogs = catalogs();
    let (definition, contract) = selected(field(seed, "definition"), &catalogs, "work");
    let admission = ChatAdmission::new(&definition).unwrap();
    let client = Arc::new(Client::new(contract, seed));
    client.state.lock().unwrap().lose_reply = true;
    let store = open(client.clone(), &disk).await;
    let directive = directive(&original(&store, seed).await);
    assert!(
        resume(
            &store,
            client.clone(),
            &admission,
            &catalogs,
            &corpus,
            &directive
        )
        .await
        .is_err()
    );
    assert_eq!(client.state.lock().unwrap().invokes, 1);
    let pending = store
        .named("chat-bridge/main", &main_name(&definition).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(!field(&pending.value, "pendingAction").is_null());
    store.close().await.unwrap();
    client.state.lock().unwrap().receipts_unavailable = false;
    let store = open(client.clone(), &disk).await;
    resume(
        &store,
        client.clone(),
        &admission,
        &catalogs,
        &corpus,
        &directive,
    )
    .await
    .unwrap();
    assert_eq!(client.state.lock().unwrap().invokes, 1);
    let state = client.state.lock().unwrap();
    let ids: Vec<_> = state
        .calls
        .iter()
        .filter(|c| c.get("method") == Some(&"tools.call".into()))
        .map(|c| field(c, "params"))
        .filter(|c| c.get("qualifiedName") == Some(&"agent.operation".into()))
        .map(|c| text(field(c, "arguments"), "operationId"))
        .collect();
    assert!(!ids.is_empty() && ids.iter().all(|id| id == &ids[0]));
    drop(state);
    store.close().await.unwrap();
}

#[tokio::test]
async fn legacy_storage_proof_refuses_foreign_account_origin_thread_and_live_work_before_resume() {
    let corpus = fixture();
    let seed = seed(&corpus);
    let catalogs = catalogs();
    for fault in [
        "account",
        "missing-account",
        "provider",
        "origin",
        "thread",
        "ephemeral",
        "cwd",
        "project",
        "active",
        "plan",
        "engine",
        "busy",
    ] {
        let input_seed = if fault == "busy" {
            field(
                field(&corpus, "cases")
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|v| v.get("label") == Some(&"terminal-0.159.2".into()))
                    .unwrap(),
                "seed",
            )
        } else {
            seed
        };
        let disk = Disk::new(input_seed);
        let base = if fault == "engine" {
            field(
                field(
                    field(&corpus, "cases")
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|v| v.get("label") == Some(&"terminal-0.163.0".into()))
                        .unwrap(),
                    "seed",
                ),
                "definition",
            )
        } else {
            field(seed, "definition")
        };
        let (mut definition, contract) = selected(base, &catalogs, "work");
        if fault == "plan" {
            definition
                .get_mut("nativePlan")
                .unwrap()
                .get_mut("turnStart")
                .unwrap()
                .get_mut("params")
                .unwrap()
                .insert("model", "changed".into());
        }
        let admission = ChatAdmission::new(&definition).unwrap();
        let client = Arc::new(Client::new(contract, seed));
        client.state.lock().unwrap().fault = fault.to_owned();
        let store = open(client.clone(), &disk).await;
        let source = original(&store, seed).await;
        let result = resume(
            &store,
            client.clone(),
            &admission,
            &catalogs,
            &corpus,
            &directive(&source),
        )
        .await;
        assert_eq!(
            result.unwrap_err().code,
            if fault == "busy" {
                "chat_main_busy"
            } else {
                "chat_binding_mismatch"
            },
            "fault: {fault}"
        );
        assert_eq!(client.state.lock().unwrap().invokes, 0, "fault: {fault}");
        assert!(
            store
                .named("chat-bridge/main", &main_name(&definition).unwrap())
                .await
                .unwrap()
                .is_none()
        );
        store.close().await.unwrap();
    }
}

#[test]
fn configured_resume_requires_complete_exact_pins_and_a_stable_operation_identity() {
    let value = Json::object([
        ("operationId", "chat:1790000000123:transition".into()),
        (
            "main",
            Json::object([("objectId", "main".into()), ("revision", 1.0.into())]),
        ),
        (
            "binding",
            Json::object([("objectId", "binding".into()), ("revision", 1.0.into())]),
        ),
    ]);
    assert!(ConfiguredMainResume::from_json(&value).is_ok());
    for fault in ["extra", "main", "binding", "operationId"] {
        let mut invalid = value.clone();
        match fault {
            "extra" => invalid.insert("unexpected", true.into()),
            "operationId" => invalid.insert("operationId", "not-an-operation".into()),
            field => invalid
                .get_mut(field)
                .unwrap()
                .insert("revision", 0.0.into()),
        }
        assert!(
            ConfiguredMainResume::from_json(&invalid).is_err(),
            "{fault}"
        );
    }
}

#[tokio::test]
async fn exact_source_pins_and_explicit_account_boundaries_precede_native_io() {
    let corpus = fixture();
    let seed = seed(&corpus);
    let catalogs = catalogs();
    for fault in ["main-pin", "binding-pin", "explicit-account", "destination"] {
        let disk = Disk::new(seed);
        let (definition, contract) = selected(field(seed, "definition"), &catalogs, "work");
        let admission = ChatAdmission::new(&definition).unwrap();
        let client = Arc::new(Client::new(contract, seed));
        let store = open(client.clone(), &disk).await;
        let mut source = original(&store, seed).await;
        if fault == "destination" {
            let mut value = source.value.clone();
            value.insert("definition", definition.clone());
            value.insert("definitionHash", hash_json(&definition).unwrap().into());
            store
                .write(
                    "chat-bridge/main",
                    &value,
                    "unrelated-destination",
                    Destination::Create {
                        parent_id: None,
                        name: &main_name(&definition).unwrap(),
                    },
                )
                .await
                .unwrap();
        }
        if fault == "explicit-account" {
            let (old_definition, _) = selected(field(seed, "definition"), &catalogs, "other");
            let mut value = source.value.clone();
            value.insert("definition", old_definition.clone());
            value.insert("definitionHash", hash_json(&old_definition).unwrap().into());
            let pin = store
                .write(
                    "chat-bridge/main",
                    &value,
                    "explicit-source",
                    Destination::Create {
                        parent_id: None,
                        name: &main_name(&old_definition).unwrap(),
                    },
                )
                .await
                .unwrap();
            source = store
                .read("chat-bridge/main", Reference::Pinned(&pin))
                .await
                .unwrap();
        }
        let mut selected = directive(&source);
        if fault == "main-pin" {
            selected
                .get_mut("main")
                .unwrap()
                .insert("revision", ((source.pin.revision + 1) as f64).into());
        }
        if fault == "binding-pin" {
            selected
                .get_mut("binding")
                .unwrap()
                .insert("objectId", "wrong-binding".into());
        }
        assert_eq!(
            resume(
                &store,
                client.clone(),
                &admission,
                &catalogs,
                &corpus,
                &selected
            )
            .await
            .unwrap_err()
            .code,
            "chat_binding_mismatch",
            "{fault}"
        );
        assert_eq!(client.state.lock().unwrap().invokes, 0);
        assert!(
            client
                .state
                .lock()
                .unwrap()
                .calls
                .iter()
                .all(|c| c.get("method") != Some(&"tools.call".into()))
        );
        store.close().await.unwrap();
    }
}
