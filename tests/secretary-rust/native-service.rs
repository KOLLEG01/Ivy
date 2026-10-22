//! Run the native executable against a loopback Hive protocol peer, without Node.
use futures_util::{SinkExt, StreamExt};
use ivy_contracts::{
    IvyJson,
    hash::{hash_json, tool_definition_hash},
    json::{canonical, parse},
};
use ivy_secretary::{
    assignment_schema::default_configuration,
    store::{Record, record_references},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot},
    time::{sleep, timeout},
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

const ROOT: &str = "00000000-0000-4000-8000-000000000001";
const CONFIGURATION: &str = "00000000-0000-4000-8000-000000000099";
const AT: &str = "2026-01-01T00:00:00.000Z";

#[test]
fn public_request_ids_are_strings() {
    use ivy_sdk::service::InvocationContext;
    use ivy_secretary::service_handlers::{EngineMethod, PublicMethod, admit};
    let context = InvocationContext::detached(
        "fixture-user".into(), None, 7, Default::default(),
        ivy_contracts::IvyError::new("cancelled", "Fixture cancelled."),
    );
    let request = |id: Value| {
        parse(
            &json!({"expectedScope":settings()["identity"]["scope"],"operationId":id}).to_string(),
            1024 * 1024,
        )
        .unwrap()
    };
    let admitted = admit(
        PublicMethod::OperationRead,
        &request(json!("native:1792690000000:read")),
        &context,
        Some(7),
        || Ok(()),
    )
    .unwrap();
    assert_eq!(admitted.method, EngineMethod::OperationRead);
    for id in [json!(1), json!(false), json!(["id"]), Value::Null] {
        let error = admit(
            PublicMethod::OperationRead,
            &request(id),
            &context,
            Some(7),
            || Ok(()),
        )
        .err()
        .expect("A non-string ID must fail");
        assert_eq!(error.code, "invalid_arguments");
    }
}

#[test]
fn inbox_notice_text_uses_admitted_strings() {
    use ivy_secretary::inbox_evidence::notice_text;
    let message =
        json!({"accountId":"mail","senderId":"alice","title":"Subject","text":"Original text"});
    let value = |value: Value| parse(&value.to_string(), 1024 * 1024).unwrap();
    let assessment = value(json!({"summary":"Summary"}));
    let source = value(json!({"kind":"email"}));
    assert_eq!(
        notice_text(&assessment, &value(message.clone()), &source)
            .unwrap()
            .to_utf8()
            .unwrap(),
        "[mail] alice – Subject: Summary"
    );
    let mut invalid = message;
    invalid["accountId"] = json!(17);
    assert!(notice_text(&assessment, &value(invalid), &source).is_err());
    let teams = value(
        json!({"accountId":"chat","senderId":"user:alice","title":"Team","text":"Original text"}),
    );
    assert_eq!(
        notice_text(&assessment, &teams, &value(json!({"kind":"chat"})))
            .unwrap()
            .to_utf8()
            .unwrap(),
        "[chat] Team: Original text"
    );
}

#[test]
fn native_results_and_documents_preserve_user_content() {
    use ivy_secretary::{
        assignment_decision::assignment_decision,
        document::{Format, extract},
        triage_results::result_items,
    };
    let decision = json!({"schemaVersion":1,"urgency":"normal","notification":"main","text":"A useful result","reason":"An event occurred"});
    let fenced = format!("  ```json\n{decision}\n```  ");
    assert_eq!(
        assignment_decision(&fenced.into())
            .unwrap()
            .required("text")
            .unwrap(),
        &IvyJson::from("A useful result")
    );
    let assessment = json!({"urgency":"normal","disposition":"record","notification":"none","summary":"Saved observation","question":false,"usefulIntel":true,"task":null});
    let turn = parse(r#"{"status":"completed","error":null}"#, 1024).unwrap();
    let mut item = json!({"id":"answer","type":"agentMessage","phase":"final_answer","text":assessment.to_string()});
    let value = |value: &Value| parse(&value.to_string(), 1_048_576).unwrap();
    assert_eq!(
        result_items(&turn, &[value(&item)])
            .unwrap()
            .required("summary")
            .unwrap(),
        &IvyJson::from("Saved observation")
    );
    item["type"] = json!({"toString":"agentMessage"});
    assert_eq!(
        result_items(&turn, &[value(&item)]).unwrap_err().code,
        "secretary_triage_unexpected_action"
    );
    let html = extract(
        Format::Html,
        b"<html><body><p>Hello</p><p>World</p></body></html>",
    )
    .unwrap();
    assert_eq!(html.schema_version, 1);
    let text = html
        .sections
        .iter()
        .map(|section| section.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Hello") && text.contains("World"));
    let bytes = b"Useful original text";
    let source = ivy_secretary::media_input::Source {
        name: "message.txt".into(),
        media_type: Some(" TEXT/PLAIN ; charset=utf-8 ".into()),
        byte_length: bytes.len(),
        content_hash: ivy_contracts::hash::digest(bytes),
    };
    let prepared = ivy_secretary::media_input::prepare(&source, bytes, 4096.0)
        .unwrap()
        .to_json();
    assert_eq!(
        prepared
            .required("input")
            .unwrap()
            .required("text")
            .unwrap(),
        &IvyJson::from("Useful original text")
    );
}

#[test]
fn object_projection_uses_json_pointer_indices() {
    use ivy_secretary::assignment_object::pointer;
    let value = parse(r#"{"items":[{"a/b":{"~key":42}}]}"#, 1024).unwrap();
    assert_eq!(
        pointer(&value, &"/items/0/a~1b/~0key".into()),
        IvyJson::Number(42.0)
    );
    for path in ["/items/length", "/items/00", "/items/-1", "items/0"] {
        assert_eq!(pointer(&value, &path.into()), IvyJson::Null);
    }
}

#[test]
fn schedule_catch_up_remains_bounded() {
    use ivy_secretary::schedule::{LocalMinute, Schedule, occurrences};
    let now = 16 * 60_000;
    let local = |_: i64, _: &str| -> Result<LocalMinute, ivy_contracts::IvyError> {
        unreachable!("Interval schedules use UTC")
    };
    let page = occurrences(Schedule::Interval { minutes: 5 }, now, 0, 2, local).unwrap();
    assert_eq!(
        page.items
            .iter()
            .map(|item| item.occurred_at)
            .collect::<Vec<_>>(),
        [5 * 60_000, 10 * 60_000]
    );
    let next = occurrences(
        Schedule::Interval { minutes: 5 },
        now,
        page.checked_at,
        2,
        local,
    )
    .unwrap();
    assert_eq!(
        next.items
            .iter()
            .map(|item| item.occurred_at)
            .collect::<Vec<_>>(),
        [15 * 60_000]
    );
}

struct MetadataStub(IvyJson);
impl ivy_sdk::rpc::RpcClient for MetadataStub {
    fn request<'a>(
        &'a self,
        method: &'a str,
        _: IvyJson,
        _: ivy_sdk::rpc::RequestOptions,
    ) -> ivy_sdk::rpc::RequestFuture<'a> {
        Box::pin(async move {
            match method {
                "system.status" => Ok(IvyJson::object([("runtimeEpoch", "fixture-epoch".into())])),
                "serviceNodes.get" => Ok(self.0.clone()),
                _ => panic!("Unexpected metadata request: {method}"),
            }
        })
    }
}
#[tokio::test]
async fn inbox_owner_requires_true_connection_flags() {
    use ivy_secretary::inbox_evidence::verify_owner;
    let settings = parse(&settings().to_string(), 1024 * 1024).unwrap();
    let mut owner = json!({"connected":true,"synced":true,"principalId":"secretary-principal","hostId":"fixture-host","serviceName":"secretary"});
    let client = |owner: &Value| MetadataStub(parse(&owner.to_string(), 1024 * 1024).unwrap());
    verify_owner(
        &client(&owner),
        &"fixture.secretary".into(),
        &settings,
        &Default::default(),
    )
    .await
    .unwrap();
    for flag in ["connected", "synced"] {
        for invalid in [json!(false), json!(1), json!("true"), Value::Null] {
            owner[flag] = invalid;
            assert_eq!(
                verify_owner(
                    &client(&owner),
                    &"fixture.secretary".into(),
                    &settings,
                    &Default::default()
                )
                .await
                .unwrap_err()
                .code,
                "secretary_owner_mismatch"
            );
        }
        owner[flag] = json!(true);
    }
}

#[test]
fn teams_complete_pages_keep_false_flags_and_zero_cursors() {
    use ivy_secretary::teams_projection::teams_page;
    let value = |value: Value| parse(&value.to_string(), 1024 * 1024).unwrap();
    let hash = format!("sha256:{}", "a".repeat(64));
    let configuration = value(
        json!({"sourceId":"teams","target":{"serviceNodeId":"native","hostId":"host","nativeVersion":"fixture","nativeExecutableHash":hash,"catalogHash":hash},"expectedAccountHash":hash,"profile":{"id":"teams-account","email":"me@example.test"},"since":AT,"maximumChats":8,"maximumMessages":8,"excludedChatIds":[],"pollMs":60000,"threadCwd":std::env::current_dir().unwrap()}),
    );
    let source = value(
        json!({"sourceId":"teams","kind":"teams","accountId":"teams-account","producerPrincipalIds":["producer"],"ownSenderIds":[],"allowedSenderIds":null}),
    );
    let selection = value(json!({"since":AT,"until":"2026-01-02T00:00:00.000Z","chat":null}));
    let observed = "2026-01-02T00:00:01.000Z".into();
    let mut page = json!({"chats":[],"has_more":false,"next_from_index":0,"next_link":""});
    let complete = teams_page(
        &value(page.clone()),
        &configuration,
        &source,
        &selection,
        &observed,
    )
    .unwrap();
    assert_eq!(
        complete.required("messagesWithAttachments").unwrap(),
        &IvyJson::Number(0.0)
    );
    for (key, more) in [
        ("has_more", json!(true)),
        ("next_from_index", json!(1)),
        ("next_link", json!("https://example.test/next")),
    ] {
        let original = page[key].clone();
        page[key] = more;
        assert_eq!(
            teams_page(
                &value(page.clone()),
                &configuration,
                &source,
                &selection,
                &observed
            )
            .unwrap_err()
            .code,
            "teams_page_incomplete"
        );
        page[key] = original;
    }
}

#[tokio::test]
async fn outlook_and_teams_preserve_page_continuations_and_gap_backoff() {
    use ivy_sdk::{
        clock::SystemClock, native_evidence::NativeOwnerIdentity, native_owner::NativeOwner,
        tools::ToolClients,
    };
    use ivy_secretary::{
        engine::SecretaryEngine,
        inbox_evidence::InboxEvidence,
        inbox_recovery::RecoveryScan,
        journal::{Journal, Limits, Pin},
        microsoft_store::MicrosoftDocument,
        outlook_native::OutlookNative,
        store::Store,
        teams_native::TeamsNative,
    };
    use std::cell::RefCell;
    let value = |value: Value| parse(&value.to_string(), 1024 * 1024).unwrap();
    let client = Arc::new(MetadataStub(IvyJson::Null));
    let journal = Journal::open(Path::new(":memory:"), Limits::default()).unwrap();
    let store = Store {
        client: client.clone(),
        root: ROOT.into(),
        journal: &journal,
    };
    let settings = value(settings());
    let evidence = InboxEvidence {
        store: &store,
        settings: &settings,
        service_node_id: &"fixture.secretary".into(),
        cancellation: Default::default(),
    };
    let tools = Arc::new(ToolClients::new(client));
    let issues = RefCell::new(RecoveryScan::default());
    let engine = SecretaryEngine::new(
        &evidence,
        &SystemClock,
        &SystemClock,
        || "fixture-id".into(),
        &tools,
        &issues,
    );
    let owner = NativeOwner::new(
        tools.clone(),
        "secretary-principal".into(),
        NativeOwnerIdentity {
            service_node_id: "fixture.agent".into(),
            host_id: Some("fixture-host".into()),
            native_version: "1".into(),
            native_executable_hash: format!("sha256:{}", "a".repeat(64)).into(),
            catalog_hash: format!("sha256:{}", "b".repeat(64)).into(),
        },
        Default::default(),
    );
    let configuration = value(json!({"pollMs":5000}));
    let outlook = OutlookNative {
        engine: &engine,
        configuration: configuration.clone(),
        owner: owner.clone(),
    };
    let teams = TeamsNative {
        engine: &engine,
        configuration,
        owner,
    };
    let document = |name: &str, value| MicrosoftDocument {
        pin: Pin {
            object_id: name.into(),
            revision: 1,
        },
        value,
    };
    let head = value(json!({"pending":{"objectId":"plan","revision":1},"continuation":null}));
    let plan = document(
        "plan",
        value(json!({"selection":{"skip":0,"chat":null},"pageNumber":0})),
    );
    let proof = document(
        "page",
        value(
            json!({"gap":null,"page":{"complete":false,"nextIndex":50,"chats":[{"id":"chat-1"}]},"observedAt":AT,"retryAfterSeconds":null}),
        ),
    );
    let next = outlook.next_head(&head, &plan, &proof).unwrap();
    assert_eq!(next.required("pending").unwrap(), &IvyJson::Null);
    assert_eq!(next.required("nextPollAt").unwrap(), &IvyJson::from(AT));
    assert_eq!(
        next.required("continuation").unwrap(),
        &value(json!({"selection":{"skip":50,"chat":null},"pageNumber":1}))
    );
    let next = teams.next_head(&head, &plan, &proof).unwrap();
    assert_eq!(
        next.required("continuation")
            .unwrap()
            .required("selection")
            .unwrap()
            .required("chat")
            .unwrap(),
        &value(json!({"id":"chat-1"}))
    );
    assert_eq!(next.required("nextPollAt").unwrap(), &IvyJson::from(AT));
    let gap = document(
        "gap",
        value(
            json!({"gap":"provider_unavailable","page":null,"observedAt":AT,"retryAfterSeconds":30}),
        ),
    );
    for next in [
        outlook.next_head(&head, &plan, &gap),
        teams.next_head(&head, &plan, &gap),
    ] {
        let next = next.unwrap();
        assert_eq!(next.required("continuation").unwrap(), &IvyJson::Null);
        assert_eq!(
            next.required("nextPollAt").unwrap(),
            &IvyJson::from("2026-01-01T00:00:30.000Z")
        );
        assert!(next.get("lastCompleteAt").is_none());
    }
    let complete = document(
        "complete",
        value(json!({"gap":null,"page":{"complete":true,"chats":[]},"observedAt":AT})),
    );
    for next in [
        outlook.next_head(&head, &plan, &complete),
        teams.next_head(&head, &plan, &complete),
    ] {
        let next = next.unwrap();
        assert_eq!(next.required("lastCompleteAt").unwrap(), &IvyJson::from(AT));
        assert_eq!(
            next.required("nextPollAt").unwrap(),
            &IvyJson::from("2026-01-01T00:00:05.000Z")
        );
        assert_eq!(next.required("continuation").unwrap(), &IvyJson::Null);
    }
}

struct NoNativeCalls;
impl ivy_secretary::media_spool::Validation for NoNativeCalls {
    fn operation(&self, _: &IvyJson) -> ivy_contracts::IvyResult<()> {
        panic!("Opening a spool must not invoke native work")
    }
    fn progress(&self, _: &IvyJson, _: &IvyJson) -> ivy_contracts::IvyResult<()> {
        panic!("Opening a spool must not advance native work")
    }
}
#[test]
fn native_spool_ownership_and_link_checks_remain_enforced() {
    use ivy_secretary::media_spool::SpoolJournal;
    let directory = Directory(
        std::env::temp_dir().join(format!("ivy-secretary-spool-{}", uuid::Uuid::new_v4())),
    );
    fs::create_dir(&directory.0).unwrap();
    let owner = parse(r#"{"principalId":"fixture-owner"}"#, 1024).unwrap();
    let opened = SpoolJournal::open(&directory.0, &owner, NoNativeCalls).unwrap();
    assert_eq!(
        SpoolJournal::open(&directory.0, &owner, NoNativeCalls)
            .err()
            .unwrap()
            .code,
        "media_spool_owned"
    );
    drop(opened);
    drop(SpoolJournal::open(&directory.0, &owner, NoNativeCalls).unwrap());
    let target = directory.0.join("target");
    let linked = directory.0.join("linked");
    fs::create_dir(&target).unwrap();
    ivy_host_runtime::filesystem::directory_link(&target, &linked).unwrap();
    assert_eq!(
        SpoolJournal::open(&linked, &owner, NoNativeCalls)
            .err()
            .unwrap()
            .code,
        "path_unsafe"
    );
    for name in [
        "owner.sqlite",
        "media.sqlite",
        "media.sqlite-wal",
        "media.sqlite-shm",
    ] {
        let root = directory.0.join(format!("hardlink-{name}"));
        fs::create_dir(&root).unwrap();
        let original = root.join("retained");
        fs::write(&original, b"retained bytes").unwrap();
        fs::hard_link(&original, root.join(name)).unwrap();
        assert_eq!(
            SpoolJournal::open(&root, &owner, NoNativeCalls)
                .err()
                .unwrap()
                .code,
            "media_spool_path_invalid"
        );
        assert_eq!(fs::read(original).unwrap(), b"retained bytes");
    }
}

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn write(path: impl AsRef<Path>, value: &Value) {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}
fn read(path: impl AsRef<Path>) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}
fn settings() -> Value {
    json!({
        "identity": {"scope": {"secretaryId":"secretary", "rootObjectId":ROOT},
            "principalId":"secretary-principal", "hostId":"fixture-host", "serviceNodeId":"fixture.secretary"},
        "sources":[], "policy":{"silentTime":null,"urgentBypass":false,"whatsappQuestionsOnly":false,
            "minimumMainUrgency":"normal","researchMaxMinutes":0,"voiceEscalation":null},
        "pollMs":100,"recordsPerTick":10
    })
}
fn configuration(workspace: &Path) -> Value {
    let settings = parse(&settings().to_string(), 1024 * 1024).unwrap();
    let execution = parse(
        &json!({"serviceNodeId":"fixture.agent","threadCwd":workspace,
        "model":null,"effort":"medium","permissions":":read-only"})
        .to_string(),
        1024 * 1024,
    )
    .unwrap();
    let value = default_configuration(&settings, &AT.into(), Some(&execution)).unwrap();
    let content: Value = serde_json::from_str(&canonical(&value, 1024 * 1024).unwrap()).unwrap();
    let references: Value = serde_json::from_str(
        &canonical(
            &record_references(Record::Configuration, &value).unwrap(),
            1024 * 1024,
        )
        .unwrap(),
    )
    .unwrap();
    json!({"object":{"id":CONFIGURATION,"parentId":ROOT,"effectivelyArchived":false,"contractKey":"secretary/configuration"},
        "revision":{"revision":1,"contractVersion":"1.1.0","mediaType":"application/json",
            "contentHash":hash_json(&value).unwrap(),"byteLength":canonical(&value, 1024 * 1024).unwrap().len(),
            "references":references,"createdAt":AT},"content":{"encoding":"json","value":content}})
}

#[derive(Default)]
struct Observed {
    registry: Option<Value>,
    connected: Option<Value>,
    assignment_queries: usize,
    ready: bool,
    unexpected: Vec<String>,
}
struct Invocation {
    name: &'static str,
    arguments: Value,
    reply: oneshot::Sender<Value>,
}
async fn invoke(sender: &mpsc::Sender<Invocation>, name: &'static str, arguments: Value) -> Value {
    let (reply, result) = oneshot::channel();
    sender
        .send(Invocation {
            name,
            arguments,
            reply,
        })
        .await
        .unwrap();
    let frame = timeout(Duration::from_secs(10), result)
        .await
        .expect("Public call timed out")
        .unwrap();
    assert!(frame.get("error").is_none(), "{frame}");
    frame["result"].clone()
}
async fn peer(
    listener: TcpListener,
    document: Value,
    observed: Arc<Mutex<Observed>>,
    mut calls: mpsc::Receiver<Invocation>,
) {
    let (stream, _) = listener.accept().await.unwrap();
    let mut socket = accept_async(stream).await.unwrap();
    let mut hashes = HashMap::new();
    let mut pending = HashMap::new();
    let mut next_id = 0;
    loop {
        tokio::select! {
            call = calls.recv() => {
                let Some(call) = call else { break; };
                next_id += 1;
                let id = format!("native-{next_id}");
                let hash = hashes.get(call.name).expect("Registered public tool");
                let frame = json!({"jsonrpc":"2.0","id":id,"method":"provider.invoke","params":{
                    "generation":7,"qualifiedName":format!("secretary.{}",call.name),"definitionHash":hash,
                    "callerPrincipalId":"fixture-user","arguments":call.arguments}});
                pending.insert(id, call.reply);
                socket.send(Message::Text(frame.to_string().into())).await.unwrap();
            }
            message = socket.next() => {
                let Some(Ok(Message::Text(text))) = message else { break; };
                let frame: Value = serde_json::from_str(&text).unwrap();
                let Some(method) = frame["method"].as_str() else {
                    if let Some(reply) = frame["id"].as_str().and_then(|id|pending.remove(id)) { let _ = reply.send(frame); }
                    continue;
                };
                let params = &frame["params"];
                let result = match method {
                    "service.connect" => { observed.lock().unwrap().connected = Some(params.clone()); json!({"generation":7}) }
                    "registry.sync" => {
                        for namespace in params["namespaces"].as_array().expect("Namespaces") {
                            for definition in namespace["tools"].as_array().unwrap() {
                                let value = parse(&definition.to_string(), 1024 * 1024).unwrap();
                                hashes.insert(definition["name"].as_str().unwrap().to_owned(), tool_definition_hash(&value).unwrap());
                            }
                        }
                        observed.lock().unwrap().registry = Some(params.clone());
                        json!({})
                    }
                    "service.heartbeat" => { observed.lock().unwrap().ready = params["ready"] == true; json!({"ready":params["ready"]}) }
                    "system.status" => json!({"runtimeEpoch":"native-reference"}),
                    "serviceNodes.get" => json!({"connected":true,"synced":true,"principalId":"secretary-principal","hostId":"fixture-host","serviceName":"secretary"}),
                    "objects.stat" => json!({"effectivelyArchived":false}),
                    "objects.query" => {
                        if params["contractKey"] == "secretary/assignment" { observed.lock().unwrap().assignment_queries += 1; }
                        json!({"items":if params["contractKey"] == "secretary/configuration" {vec![json!({"objectId":CONFIGURATION,"revision":1})]} else {vec![]},"nextCursor":null})
                    }
                    "objects.read" => document.clone(),
                    _ => { observed.lock().unwrap().unexpected.push(method.to_owned()); json!(null) }
                };
                if !frame["id"].is_null() {
                    socket.send(Message::Text(json!({"jsonrpc":"2.0","id":frame["id"],"result":result}).to_string().into())).await.unwrap();
                }
            }
        }
    }
}

fn command(config: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ivy-secretary"));
    command
        .env("IVY_INSTANCE_CONFIG", config)
        .env_remove("IVY_NODE_EXECUTABLE")
        .env("PATH", "")
        .env("IVY_LAUNCH_ID", "native-service-test");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}

#[tokio::test]
async fn native_process_registers_serves_and_restarts_without_node_or_job_helper() {
    let directory = Directory(
        std::env::temp_dir().join(format!("ivy-secretary-native-{}", uuid::Uuid::new_v4())),
    );
    let artifact = directory.0.join("artifact");
    let data = directory.0.join("data");
    let instance = directory.0.join("instance.json");
    let build = json!({"version":"fixture","buildId":format!("sha256:{}","a".repeat(64))});
    write(artifact.join("dist/build-info.json"), &build);
    write(
        artifact.join("dist/instructions/secretary-notification-policy.json"),
        &json!("Send useful notifications."),
    );
    let mut config = json!({"schemaVersion":1,"hostId":"fixture-host","instanceId":"fixture.secretary","componentId":"secretary",
        "serviceNodeId":"fixture.secretary","credential":"fixture","publicBaseUrl":"http://127.0.0.1:1",
        "dataRoot":data,"artifactRoot":artifact,"version":"fixture","buildId":build["buildId"],"settings":settings()});

    // Removing Node must not weaken the existing artifact/config identity check.
    config["buildId"] = json!(format!("sha256:{}", "b".repeat(64)));
    write(&instance, &config);
    let rejected = command(&instance).output().unwrap();
    assert!(!rejected.status.success());
    let error: Value = serde_json::from_slice(&rejected.stderr).unwrap();
    assert_eq!(error["code"], "build_mismatch");
    config["buildId"] = build["buildId"].clone();

    let mut previous_boot = Value::Null;
    for _ in 0..2 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        config["publicBaseUrl"] = json!(format!("http://{}", listener.local_addr().unwrap()));
        write(&instance, &config);
        let observed = Arc::new(Mutex::new(Observed::default()));
        let (sender, calls) = mpsc::channel(4);
        let peer = tokio::spawn(peer(
            listener,
            configuration(&directory.0),
            observed.clone(),
            calls,
        ));
        let stderr = directory.0.join("stderr.log");
        let mut process = Process(
            command(&instance)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(fs::File::create(&stderr).unwrap())
                .spawn()
                .unwrap(),
        );
        let health = timeout(Duration::from_secs(15), async {
            loop {
                assert!(
                    process.0.try_wait().unwrap().is_none(),
                    "Service exited: {}",
                    fs::read_to_string(&stderr).unwrap()
                );
                if let Some(health) = read(data.join("health.json")) {
                    if health["ready"] == true && observed.lock().unwrap().ready {
                        break health;
                    }
                }
                sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("Native service did not become ready");
        assert_ne!(health["bootId"], previous_boot);
        previous_boot = health["bootId"].clone();
        assert_eq!(health["generation"], 7);
        {
            let observed = observed.lock().unwrap();
            assert_eq!(
                observed.connected.as_ref().unwrap()["serviceName"],
                "secretary"
            );
            let tools: usize = observed.registry.as_ref().unwrap()["namespaces"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n["tools"].as_array().unwrap().len())
                .sum();
            assert_eq!(tools, 16);
        }
        let binding = invoke(&sender, "binding", json!({})).await;
        assert_eq!(binding["available"], true);
        assert_eq!(binding["expectedScope"], settings()["identity"]["scope"]);
        let before = observed.lock().unwrap().assignment_queries;
        let assignments = invoke(
            &sender,
            "listAssignments",
            json!({"expectedScope":binding["expectedScope"]}),
        )
        .await;
        assert_eq!(assignments["assignments"], json!([]));
        // A scope mismatch must be rejected at the public service boundary.
        let (reply, result) = oneshot::channel();
        sender
            .send(Invocation {
                name: "listAssignments",
                arguments: json!({"expectedScope":{"secretaryId":"other","rootObjectId":ROOT}}),
                reply,
            })
            .await
            .unwrap();
        let denied = timeout(Duration::from_secs(10), result)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(denied["error"]["data"]["code"], "secretary_scope_conflict");
        timeout(Duration::from_secs(5), async {
            while observed.lock().unwrap().assignment_queries <= before + 1 {
                sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("Scheduler did not keep polling assignments");
        assert!(data.join("secretary.sqlite").is_file());
        write(
            data.join("control.json"),
            &json!({"schemaVersion":1,"instanceId":"fixture.secretary",
            "launchId":health["launchId"],"bootId":health["bootId"],"action":"shutdown","requestedAt":AT}),
        );
        let status = timeout(Duration::from_secs(10), async {
            loop {
                if let Some(status) = process.0.try_wait().unwrap() {
                    break status;
                }
                sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("Host shutdown did not finish");
        assert!(status.success());
        assert_eq!(read(data.join("health.json")).unwrap()["ready"], false);
        drop(sender);
        timeout(Duration::from_secs(5), peer)
            .await
            .unwrap()
            .unwrap();
        let observed = observed.lock().unwrap();
        assert!(
            observed.unexpected.is_empty(),
            "Unexpected RPCs: {:?}",
            observed.unexpected
        );
    }
    // Maintenance of a current journal is also native and needs no Hive session.
    let maintenance = command(&instance)
        .args([
            "import",
            "--operation-id",
            "native-reference:1792690000000:import",
        ])
        .output()
        .unwrap();
    assert!(
        maintenance.status.success(),
        "{}",
        String::from_utf8_lossy(&maintenance.stderr)
    );
    let result: Value = serde_json::from_slice(&maintenance.stdout).unwrap();
    assert_eq!(result, json!({"complete":true,"alreadyCurrent":true}));
}
