use super::*;
use ivy_contracts::{builtin_schema::SchemaBundle, chat_schema::validate_chat};
use ivy_store::rusqlite::Connection;
use ivy_task_board::{
    deliveries::DeliveryRuntime,
    journal_store::{Contracts, JournalStore},
    lifecycle_journal::LifecycleJournal,
    local_journal::{self, LocalJournal},
    local_retention::HistoryLimits,
    operations::Operations,
    publication::Publication,
};
use std::collections::HashMap;
fn id(n: usize) -> JsString {
    format!("00000000-0000-4000-8000-{n:012}").into()
}
fn pin(n: usize) -> Json {
    Json::object([
        ("objectId", Json::String(id(n))),
        ("revision", Json::Number(1.0)),
    ])
}
fn validate(name: &str, v: &Json) -> IvyResult<()> {
    static B: OnceLock<SchemaBundle> = OnceLock::new();
    let b = B.get_or_init(|| {
        SchemaBundle::new(&[
            include_str!("../../specs/schemas/task-board.schema.json"),
            include_str!("../../specs/schemas/chat.schema.json"),
            include_str!("../../specs/schemas/agent.schema.json"),
            include_str!("../../specs/schemas/hive-wire.schema.json"),
        ])
        .unwrap()
    });
    if b.matches(
        &format!("https://ivy.invalid/schemas/task-board.schema.json#/$defs/{name}"),
        v,
    )? {
        Ok(())
    } else {
        Err(IvyError::new(
            "invalid_arguments",
            format!("Fixture {name} admission failed."),
        ))
    }
}
struct Policy {
    task: Json,
}
impl Contracts for Policy {
    fn version(&self, _: &str) -> IvyResult<String> {
        Ok("1.0.0".into())
    }
    fn local(&self, _: &str) -> IvyResult<bool> {
        Ok(true)
    }
    fn readable_version(&self, _: &str, v: &JsString) -> IvyResult<bool> {
        Ok(v == &JsString::from("1.0.0"))
    }
    fn validate_definition(&self, n: &str, v: &Json) -> IvyResult<()> {
        validate(n, v)
    }
    fn validate_record(&self, k: &str, _: &JsString, v: &Json) -> IvyResult<()> {
        match k {
            "task-board/operation" => validate("Operation", v),
            "task-board/delivery" => validate("Delivery", v),
            "task-board/task" => {
                let mut a = v.clone();
                let mut b = self.task.clone();
                let comments = a.remove("comments").unwrap();
                b.remove("comments");
                assert_eq!(hash_json(&a)?, hash_json(&b)?);
                let comments = comments.as_array().unwrap();
                assert!(comments.len() <= 1);
                if let Some(c) = comments.first() {
                    let mut a = c.clone();
                    let mut b = f(&self.task, "comments").as_array().unwrap()[0].clone();
                    let delivery = a.remove("delivery").unwrap();
                    b.remove("delivery");
                    assert_eq!(hash_json(&a)?, hash_json(&b)?);
                    validate("ObjectPin", &delivery)?;
                }
                Ok(())
            }
            k => panic!("Unexpected {k}"),
        }
    }
}
struct Policies {
    handoff: JsString,
    voice: JsString,
    phone: Json,
}
impl DeliveryPolicies for Policies {
    fn now(&self) -> JsString {
        clock().into()
    }
    fn phone_target(&self) -> &Json {
        &self.phone
    }
    fn chat_readable_version(&self, v: &JsString) -> bool {
        [JsString::from("1.2.0"), JsString::from("1.3.0")].contains(v)
    }
    fn handoff(&self, t: &Json, c: &Json) -> IvyResult<JsString> {
        assert_eq!(s(t, "taskKey"), &JsString::from("TASK-0123"));
        assert_eq!(s(c, "commentId"), &JsString::from("comment"));
        Ok(self.handoff.clone())
    }
    fn voice_prompt(&self, _: &Json, _: &Json) -> IvyResult<JsString> {
        Ok(self.voice.clone())
    }
}
struct Client {
    path: std::path::PathBuf,
    delivery: Mutex<Option<Json>>,
    requests: Mutex<HashMap<JsString, Json>>,
    owner: std::sync::atomic::AtomicBool,
    lose: std::sync::atomic::AtomicBool,
    confirmed: std::sync::atomic::AtomicBool,
    readiness: Mutex<Option<(&'static str, Json)>>,
    calls: Mutex<Vec<Json>>,
    target: Json,
    reply: Json,
    input: Json,
}
impl Client {
    fn operation(&self, id: &JsString) -> IvyResult<Json> {
        let requests = self.requests.lock().unwrap();
        let request = requests
            .get(id)
            .ok_or_else(|| IvyError::new("not_found", "Original operation unavailable."))?;
        let confirmed = self.confirmed.load(std::sync::atomic::Ordering::Relaxed);
        let reply = self.reply();
        let value = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("workspaceId", "workspace".into()),
            (
                "definitionHash",
                f(&self.target, "expectedBridge")
                    .required("definitionHash")?
                    .clone(),
            ),
            ("callerPrincipalId", "caller".into()),
            ("operationId", Json::String(id.clone())),
            ("request", request.clone()),
            ("requestHash", hash_json(request)?.into()),
            (
                "phase",
                if confirmed { "succeeded" } else { "accepted" }.into(),
            ),
            ("nativeCall", Json::Null),
            (
                "outcome",
                if confirmed {
                    Json::object([
                        ("action", "notify".into()),
                        ("operationId", Json::String(id.clone())),
                        (
                            "reply",
                            Json::object([("object", pin(800)), ("data", reply)]),
                        ),
                    ])
                } else {
                    Json::Null
                },
            ),
            ("error", Json::Null),
            ("createdAt", clock().into()),
            ("updatedAt", clock().into()),
        ]);
        validate_chat("Operation", &value)?;
        Ok(value)
    }
    fn reply(&self) -> Json {
        let mut v = self.reply.clone();
        let confirmed = self.confirmed.load(std::sync::atomic::Ordering::Relaxed);
        v.insert(
            "state",
            if confirmed { "confirmed" } else { "pending" }.into(),
        );
        v.insert(
            "firstOfferedAt",
            if confirmed {
                clock().into()
            } else {
                Json::Null
            },
        );
        v.insert(
            "confirmedAt",
            if confirmed {
                clock().into()
            } else {
                Json::Null
            },
        );
        v
    }
}
impl RpcClient for Client {
    fn request<'a>(&'a self, m: &'a str, a: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push(Json::Array(vec![m.into(), a.clone()]));
            match m {
                "system.status" => Ok(Json::object([(
                    "runtimeEpoch",
                    "11111111-1111-4111-8111-111111111111".into(),
                )])),
                "serviceNodes.get" => {
                    let own = f(&a, "serviceNodeId") == &Json::from("task-board-node");
                    let mut node = Json::object([
                        ("serviceNodeId", a.required("serviceNodeId")?.clone()),
                        (
                            "serviceName",
                            if own { "task-board" } else { "chat-bridge" }.into(),
                        ),
                        (
                            "principalId",
                            if own {
                                if self.owner.load(std::sync::atomic::Ordering::Relaxed) {
                                    "caller"
                                } else {
                                    "replacement"
                                }
                            } else {
                                "chat-owner"
                            }
                            .into(),
                        ),
                        ("connected", true.into()),
                        ("synced", true.into()),
                        ("ready", true.into()),
                    ]);
                    if !own {
                        if let Some((key, value)) = &*self.readiness.lock().unwrap() {
                            node.insert(*key, value.clone());
                        }
                    }
                    Ok(node)
                }
                "tools.list" => {
                    assert_eq!(f(&a, "namespace"), &Json::from("chat"));
                    let name = s(&a, "namePrefix").clone();
                    let qualified = format!("chat.{}", name.to_utf8()?);
                    let hash = format!("sha256:{}", "a".repeat(64));
                    Ok(Json::object([
                        (
                            "provider",
                            Json::object([(
                                "node",
                                Json::object([("serviceNodeId", "chat-node".into())]),
                            )]),
                        ),
                        (
                            "items",
                            Json::Array(vec![Json::object([
                                ("qualifiedName", qualified.into()),
                                (
                                    "definition",
                                    Json::object([
                                        ("namespace", "chat".into()),
                                        ("name", Json::String(name.clone())),
                                        ("interfaceVersion", "1.2.0".into()),
                                        (
                                            "annotations",
                                            Json::object([(
                                                "readOnlyHint",
                                                (name != JsString::from("notify")).into(),
                                            )]),
                                        ),
                                        ("inputSchema", Json::object(Vec::<(&str, Json)>::new())),
                                        ("outputSchema", Json::object(Vec::<(&str, Json)>::new())),
                                    ]),
                                ),
                                ("definitionHash", hash.clone().into()),
                                ("catalogHash", hash.into()),
                            ])]),
                        ),
                        ("nextCursor", Json::Null),
                    ]))
                }
                "tools.call" => {
                    let args = a.required("arguments")?;
                    match s(&a, "qualifiedName").to_utf8()?.as_str() {
                        "chat.workspace" => Ok(Json::object([
                            (
                                "expectedBridge",
                                self.target.required("expectedBridge")?.clone(),
                            ),
                            ("nativeOwnerReady", true.into()),
                            (
                                "channels",
                                Json::Array(vec![Json::object([(
                                    "channel",
                                    self.target.required("channel")?.clone(),
                                )])]),
                            ),
                        ])),
                        "chat.operation" => self.operation(s(args, "operationId")),
                        "chat.notify" => {
                            let delivery = self.delivery.lock().unwrap().clone().unwrap();
                            let db = Connection::open(&self.path).unwrap();
                            let saved = LocalJournal {
                                connection: &db,
                                root_object_id: Some("root".into()),
                            }
                            .read(
                                "task-board/delivery",
                                delivery.required("objectId")?,
                                true,
                            )?;
                            assert_eq!(
                                f(f(&saved.value, "chatRequest"), "operationId"),
                                f(args, "operationId")
                            );
                            assert_eq!(f(&a, "operationId"), f(args, "operationId"));
                            assert_eq!(f(f(&saved.value, "chatRequest"), "text"), f(args, "text"));
                            let mut request = args.clone();
                            request.insert(
                                "expectedBridge",
                                self.target.required("expectedBridge")?.clone(),
                            );
                            request.insert("channel", self.target.required("channel")?.clone());
                            validate_chat("AdmittedNotifyRequest", &request)?;
                            let operation = s(args, "operationId").clone();
                            self.requests
                                .lock()
                                .unwrap()
                                .insert(operation.clone(), request);
                            if self.lose.swap(false, std::sync::atomic::Ordering::Relaxed) {
                                return Err(IvyError::new(
                                    "outcome_unknown",
                                    "Lost original response.",
                                )
                                .with_outcome(Outcome::Unknown));
                            }
                            self.operation(&operation)
                        }
                        "chat.notice" => {
                            Ok(Json::object([("object", pin(800)), ("data", self.reply())]))
                        }
                        k => panic!("Unexpected {k}"),
                    }
                }
                "objects.read" => {
                    let value = if f(&a, "objectId") == f(&pin(800), "objectId") {
                        self.reply()
                    } else {
                        assert_eq!(f(&a, "objectId"), f(&pin(801), "objectId"));
                        let mut v = self.input.clone();
                        let requests = self.requests.lock().unwrap();
                        assert_eq!(requests.len(), 1);
                        let id = requests.keys().next().unwrap();
                        v.insert("operationId", Json::String(id.clone()));
                        v
                    };
                    Ok(Json::object([
                        (
                            "object",
                            Json::object([(
                                "contractKey",
                                if f(&a, "objectId") == f(&pin(800), "objectId") {
                                    "chat-bridge/reply"
                                } else {
                                    "chat-bridge/input"
                                }
                                .into(),
                            )]),
                        ),
                        (
                            "revision",
                            Json::object([("contractVersion", "1.3.0".into())]),
                        ),
                        (
                            "content",
                            Json::object([("encoding", "json".into()), ("value", value)]),
                        ),
                    ]))
                }
                k => panic!("No live/phone/native effect is permitted: {k}"),
            }
        })
    }
}
fn clock() -> String {
    "2026-01-02T03:04:05.006Z".to_owned()
}
fn fresh() -> JsString {
    uuid::Uuid::new_v4().to_string().into()
}
struct NativeFixture {
    task: Json,
    handoff: JsString,
    target: Json,
    reply: Json,
    input: Json,
}
fn native_fixture() -> NativeFixture {
    let json = |text: &str| parse(text, MANAGEMENT_FRAME_BYTES).unwrap();
    let hash = Json::from(format!("sha256:{}", "a".repeat(64)));
    let channel = json(r#"{"adapter":"whatsapp","accountId":"a","channelId":"main"}"#);
    let mut expected = json(
        r#"{"principalId":"chat-owner","callerPrincipalId":"caller","workspaceId":"workspace"}"#,
    );
    expected.insert("definitionHash", hash.clone());
    let target = Json::object([
        ("serviceNodeId", "chat-node".into()),
        ("expectedBridge", expected),
        ("channel", channel.clone()),
    ]);
    let mut handoff = JsString::from("Saved result");
    handoff.0.push(0xd800);
    let mut comment = json(
        r#"{"commentId":"comment","authorKind":"agent","requests":[],"nativeInput":null,"purpose":"handoff"}"#,
    );
    comment.insert("body", Json::String(handoff.clone()));
    comment.insert("delivery", pin(300));
    let task = Json::object([
        ("taskKey", "TASK-0123".into()),
        ("lastHistory", pin(600)),
        ("publication", Json::Null),
        ("comments", Json::Array(vec![comment])),
    ]);
    let mut input = json(
        r#"{"schemaVersion":1,"workspaceId":"workspace","operationId":"original","sequence":1,"payload":{"text":"Main handoff","images":[]},"state":"completed","nativeCalls":{"resume":null,"turn":null,"interrupt":null},"turnId":"turn","epoch":"epoch","error":null,"cancellation":null}"#,
    );
    input.insert("definitionHash", hash.clone());
    input.insert("requestHash", hash.clone());
    input.insert(
        "identity",
        Json::object([
            ("channel", channel.clone()),
            ("senderPrincipalId", "caller".into()),
            ("messageId", "message".into()),
        ]),
    );
    input.insert("binding", pin(802));
    input.insert("result", pin(803));
    for key in ["admittedAt", "updatedAt", "finishedAt"] {
        input.insert(key, clock().into());
    }
    let mut reply = json(
        r#"{"schemaVersion":1,"workspaceId":"workspace","sequence":1,"partIndex":0,"text":"Main response","artifacts":[],"state":"confirmed"}"#,
    );
    reply.insert("definitionHash", hash.clone());
    reply.insert("immutableHash", hash);
    reply.insert("channel", channel);
    reply.insert(
        "origin",
        Json::object([
            ("kind", "native".into()),
            ("inputId", Json::String(id(801))),
            ("binding", pin(802)),
            ("result", pin(803)),
        ]),
    );
    for key in ["createdAt", "firstOfferedAt", "confirmedAt"] {
        reply.insert(key, clock().into());
    }
    validate_chat("Input", &input).unwrap();
    validate_chat("Reply", &reply).unwrap();
    NativeFixture {
        task,
        handoff,
        target,
        reply,
        input,
    }
}
#[tokio::test(flavor = "current_thread")]
async fn real_publication_delivery_identity_survives_lost_reply_reopen_and_owner_replacement() {
    struct Temp(std::path::PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let temp = Temp(std::env::temp_dir().join(format!(
        "task-board-deliveries-{}.sqlite",
        uuid::Uuid::new_v4()
    )));
    let fixture = native_fixture();
    let mut task = fixture.task;
    let mut comment = f(&task, "comments").as_array().unwrap()[0].clone();
    let handoff = fixture.handoff;
    let client = Arc::new(Client {
        path: temp.0.clone(),
        delivery: Mutex::new(None),
        requests: Mutex::default(),
        owner: std::sync::atomic::AtomicBool::new(true),
        lose: std::sync::atomic::AtomicBool::new(true),
        confirmed: std::sync::atomic::AtomicBool::new(false),
        readiness: Mutex::new(None),
        calls: Mutex::default(),
        target: fixture.target,
        reply: fixture.reply,
        input: fixture.input,
    });
    let mut delivery = None;
    let mut retained_request = None;
    let mut confirmed_pin = None;
    for round in 0..4 {
        let db = Connection::open(&temp.0).unwrap();
        local_journal::initialize(&db).unwrap();
        let store = JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            client: client.clone(),
            contracts: Policy { task: task.clone() },
            limits: HistoryLimits::default(),
            now: clock as fn() -> String,
            new_id: fresh as fn() -> JsString,
            cache: None,
        };
        let operations = Operations {
            store: LifecycleJournal {
                journal: &store,
                pages: &(),
            },
            now: clock as fn() -> String,
        };
        let engine = DeliveryRuntime {
            client: client.clone(),
            principal_id: "caller".into(),
            service_node_id: "task-board-node".into(),
            policies: Policies {
                handoff: handoff.clone(),
                voice: "voice".into(),
                phone: Json::Null,
            },
        };
        if round == 0 {
            let phone_id = engine.new_phone_operation().unwrap();
            assert_eq!(
                uuid::Uuid::parse_str(&phone_id.to_utf8().unwrap())
                    .unwrap()
                    .get_version_num(),
                4
            );
            let actor = Json::object([
                ("principalId", "caller".into()),
                ("serviceNodeId", "task-board-node".into()),
                ("generation", Json::Number(1.0)),
                ("source", "user".into()),
            ]);
            let request = Json::object([
                ("action", "archive".into()),
                ("operationId", "fixture-publication".into()),
                ("taskId", Json::String(id(100))),
                ("expectedRevision", Json::Number(1.0)),
                ("archived", true.into()),
            ]);
            let operation = operations.begin(&request, &actor).await.unwrap();
            let mut empty = task.clone();
            empty.insert("comments", Json::Array(vec![]));
            store
                .journal
                .create(
                    "task-board/task",
                    &"Task".into(),
                    Some(&"root".into()),
                    &empty,
                    &clock().into(),
                    &id(100),
                )
                .unwrap();
            let first = initial_delivery(
                &Json::Null,
                &operation.pin,
                &id(100),
                &"comment".into(),
                &"chat".into(),
                &clock().into(),
            );
            let mut publication = Publication {
                operations: &operations,
                operation: &operation,
                task_written: None,
                index: 0.0,
            };
            let created = publication
                .create("task-board/delivery", &first, &"Delivery".into())
                .await
                .unwrap();
            comment.insert("delivery", created.clone());
            task.insert("comments", Json::Array(vec![comment.clone()]));
            let written = publication
                .write(
                    "task-board/task",
                    &task,
                    &Json::object([
                        ("objectId", Json::String(id(100))),
                        ("expectedRevision", Json::Number(1.0)),
                    ]),
                )
                .await
                .unwrap();
            let outcome = Json::object([
                ("operationId", "fixture-publication".into()),
                ("task", written),
                ("plan", Json::Null),
                ("run", Json::Null),
                ("history", Json::Null),
                ("result", Json::Null),
                ("review", Json::Null),
                ("attachment", Json::Null),
            ]);
            operations
                .finish(s(&operation.pin, "objectId"), &outcome)
                .await
                .unwrap();
            *client.delivery.lock().unwrap() = Some(created.clone());
            delivery = Some(created);
        }
        let delivery_id = s(delivery.as_ref().unwrap(), "objectId");
        let subject = Deliveries {
            store: &store,
            engine: &engine,
        };
        if round == 0 {
            subject.step(delivery_id).await.unwrap();
            let prepared = store
                .read(
                    "task-board/delivery",
                    &Json::String(delivery_id.clone()),
                    true,
                )
                .await
                .unwrap();
            let saved = prepared.value.required("chatRequest").unwrap().clone();
            let op = s(&saved, "operationId").to_utf8().unwrap();
            let parts = op.split(':').collect::<Vec<_>>();
            assert_eq!(parts.len(), 3);
            assert!(parts[1].parse::<u64>().unwrap() > 0);
            assert_eq!(
                uuid::Uuid::parse_str(parts[2]).unwrap().get_version_num(),
                4
            );
            retained_request = Some(saved);
            let mut typed_readiness_checks = 0;
            for key in ["connected", "synced", "ready"] {
                for value in [
                    Json::Null,
                    false.into(),
                    0.0.into(),
                    1.0.into(),
                    "".into(),
                    "yes".into(),
                    Json::Array(vec![]),
                    Json::object(Vec::<(&str, Json)>::new()),
                ] {
                    *client.readiness.lock().unwrap() = Some((key, value));
                    let calls_before = client.calls.lock().unwrap().len();
                    let changes_before = db.total_changes();
                    subject.step(delivery_id).await.unwrap();
                    let saved = store
                        .read(
                            "task-board/delivery",
                            &Json::String(delivery_id.clone()),
                            true,
                        )
                        .await
                        .unwrap();
                    assert_eq!(f(&saved.value, "state"), &Json::from("outcome_unknown"));
                    assert_eq!(
                        f(&saved.value, "code"),
                        &Json::from("task_board_chat_unavailable")
                    );
                    assert_eq!(
                        f(&saved.value, "chatRequest"),
                        retained_request.as_ref().unwrap()
                    );
                    assert!(
                        client.calls.lock().unwrap()[calls_before..]
                            .iter()
                            .all(|call| call.as_array().unwrap()[0]
                                == Json::from("serviceNodes.get")),
                        "A non-true readiness value must not discover or call a channel tool"
                    );
                    assert!(client.requests.lock().unwrap().is_empty());
                    if typed_readiness_checks > 0 {
                        assert_eq!(db.total_changes(), changes_before);
                    }
                    typed_readiness_checks += 1;
                }
            }
            assert_eq!(typed_readiness_checks, 24);
            *client.readiness.lock().unwrap() = None;
            subject.step(delivery_id).await.unwrap();
            let uncertain = store
                .read(
                    "task-board/delivery",
                    &Json::String(delivery_id.clone()),
                    true,
                )
                .await
                .unwrap();
            assert_eq!(f(&uncertain.value, "state"), &Json::from("outcome_unknown"));
        } else if round == 1 {
            subject.step(delivery_id).await.unwrap();
            let waiting = store
                .read(
                    "task-board/delivery",
                    &Json::String(delivery_id.clone()),
                    true,
                )
                .await
                .unwrap();
            assert_eq!(
                f(&waiting.value, "chatRequest"),
                retained_request.as_ref().unwrap()
            );
            let before = db.total_changes();
            subject.step(delivery_id).await.unwrap();
            assert_eq!(db.total_changes(), before);
        } else if round == 2 {
            client
                .confirmed
                .store(true, std::sync::atomic::Ordering::Relaxed);
            subject.step(delivery_id).await.unwrap();
            let confirmed = store
                .read(
                    "task-board/delivery",
                    &Json::String(delivery_id.clone()),
                    true,
                )
                .await
                .unwrap();
            assert_eq!(f(&confirmed.value, "state"), &Json::from("confirmed"));
            assert_eq!(
                f(&confirmed.value, "chatRequest"),
                retained_request.as_ref().unwrap()
            );
            confirmed_pin = Some(confirmed.pin);
        } else {
            let before = db.total_changes();
            let count = client.calls.lock().unwrap().len();
            subject.step(delivery_id).await.unwrap();
            assert_eq!(db.total_changes(), before);
            assert!(
                client.calls.lock().unwrap()[count..]
                    .iter()
                    .all(|c| c.as_array().unwrap()[0] == Json::from("serviceNodes.get"))
            );
            let confirmed = store
                .read(
                    "task-board/delivery",
                    &Json::String(delivery_id.clone()),
                    true,
                )
                .await
                .unwrap();
            assert_eq!(confirmed.pin, confirmed_pin.clone().unwrap());
            client
                .owner
                .store(false, std::sync::atomic::Ordering::Relaxed);
            assert_eq!(
                subject.step(delivery_id).await.err().unwrap().code,
                "task_board_principal_mismatch"
            );
            assert_eq!(db.total_changes(), before);
        }
        assert!(db.is_autocommit());
    }
    let calls = client.calls.lock().unwrap();
    assert_eq!(
        calls
            .iter()
            .filter(|c| f(&c.as_array().unwrap()[1], "qualifiedName") == &Json::from("chat.notify"))
            .count(),
        1
    );
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}
