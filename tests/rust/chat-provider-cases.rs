use super::{
    Client, Fixed, Replay, equal, field, flag, merge, result, snapshot, step as main_step, text,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_chat_bridge::{
    admission::ChatAdmission, bridge::ChatBridge, chat_main::ChatMain, collector::ChatCollector,
    delivery::ChatDelivery, native_driver::ChatNativeDriver, notice_input::NoticeInstructions,
    operations::ChatOperations, store::ChatStore,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    checked_native_contract::CheckedNativeContracts,
    json::{canonical, parse},
};
use ivy_sdk::{
    rpc::{RequestFuture, RequestOptions, RpcClient},
    service::{ConnectionOptions, InvocationContext, ServiceConnection},
    tools::ToolClients,
};
use std::{
    collections::VecDeque,
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command as AsyncCommand},
};

fn oracle() -> &'static Json {
    static ORACLE: OnceLock<Json> = OnceLock::new();
    ORACLE.get_or_init(|| {
        let output = Command::new("node")
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .arg("tests/rust/fixtures/chat-provider-oracle.mjs")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if let Some(path) = std::env::var_os("IVY_CHAT_PROVIDER_ORACLE_CAPTURE") {
            std::fs::write(path, &output.stdout).unwrap();
        }
        parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
    })
}
fn operation(label: &str) -> Json {
    format!(
        "chat:{}:{label}",
        field(oracle(), "now").as_f64().unwrap() as i64
    )
    .into()
}
fn abort_reason() -> IvyError {
    let mut retained = JsString::from("retained-");
    retained.0.push(0xd800);
    IvyError::new(
        "fixture_provider_closed",
        "Original provider invocation lost its connection.",
    )
    .with_outcome(Outcome::Unknown)
    .with_details(Json::object([("text", Json::String(retained))]))
}

/// Obtain real SDK contexts from an isolated provider socket. Only their public
/// fixture identity fields change; cancellation retains the SDK's close reason.
struct ContextPeer {
    child: Child,
    url: String,
    identity: Json,
    registry: Json,
}
impl ContextPeer {
    async fn start() -> Self {
        let mut child = AsyncCommand::new("node")
            .args([
                "--experimental-transform-types",
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/sdk-provider-peer.mjs",
            ])
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let line = tokio::time::timeout(
            Duration::from_secs(15),
            BufReader::new(child.stdout.take().unwrap())
                .lines()
                .next_line(),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        let value = parse(&line, usize::MAX).unwrap();
        Self {
            child,
            url: text(&value, "url"),
            identity: field(&value, "identity").clone(),
            registry: field(&value, "registry").clone(),
        }
    }
    async fn context(&self) -> (ServiceConnection, InvocationContext) {
        let retained = Arc::new(Mutex::new(None));
        let capture = retained.clone();
        let mut options = ConnectionOptions::new("fixture-node".into());
        options.handlers.insert(
            "fixture.echo".into(),
            Arc::new(move |_, context| {
                let capture = capture.clone();
                Box::pin(async move {
                    context.throw_if_aborted()?;
                    *capture.lock().unwrap() = Some(context);
                    Ok(true.into())
                })
            }),
        );
        let connection = ServiceConnection::connect(
            &self.url,
            "fixture-token",
            options,
            RequestOptions::default().cancellation,
        )
        .await
        .unwrap();
        let value = connection
            .request(
                "service.connect",
                self.identity.clone(),
                RequestOptions::default(),
            )
            .await
            .unwrap();
        connection.set_generation(field(&value, "generation").as_f64().unwrap() as u64);
        connection.set_registry(&self.registry).unwrap();
        connection.set_ready(true);
        connection
            .request(
                "test.invoke",
                Json::object([] as [(&str, Json); 0]),
                RequestOptions::default(),
            )
            .await
            .unwrap();
        let context = retained.lock().unwrap().take().unwrap();
        (connection, context)
    }
    async fn finish(mut self) {
        self.child.kill().await.unwrap();
        self.child.wait().await.unwrap();
    }
}
struct ProviderClient {
    base: Arc<Client>,
    connection: Mutex<Option<ServiceConnection>>,
}
impl RpcClient for ProviderClient {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            if matches!(method, "objects.query" | "objects.read") {
                // Original Promise.all and Rust polling may interleave pure
                // public reads differently. Match the complete exact request
                // within that read prefix, never across a write/status barrier.
                let mut state = self.base.state.lock().unwrap();
                let mut aliases = super::aliases(&self.base.path);
                aliases.extend(state.mutations.iter().cloned());
                let logical = super::strings(&params, &|s| super::alias_string(s, &aliases));
                let logical = canonical(&logical, usize::MAX)?;
                let selected = state
                    .pending
                    .iter()
                    .take_while(|r| {
                        matches!(text(r, "method").as_str(), "objects.query" | "objects.read")
                    })
                    .position(|r| {
                        text(r, "method") == method
                            && canonical(field(r, "params"), usize::MAX).unwrap() == logical
                    });
                if let Some(index) = selected {
                    let expected = state.pending.remove(index).unwrap();
                    state.pending.push_front(expected);
                }
            }
            let abort = self
                .base
                .state
                .lock()
                .unwrap()
                .pending
                .front()
                .is_some_and(|r| flag(r, "abortAfterRpc"));
            let value = self.base.request(method, params, options).await;
            if abort {
                self.connection
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .close_with(abort_reason());
            }
            value
        })
    }
}
fn action_label(action: &Json) -> String {
    action
        .get("label")
        .map(|v| v.as_str().unwrap())
        .unwrap_or_else(|| format!("provider-{}", text(action, "name")))
}
async fn request(
    action: &Json,
    bridge: &ChatBridge<'_>,
    client: &Client,
) -> IvyResult<(JsString, Json)> {
    let caller = action
        .get("caller")
        .and_then(Json::as_js_string)
        .cloned()
        .unwrap_or_else(|| "alice".into());
    let name = text(action, "name");
    let label = action_label(action);
    let expected = bridge.main.admission().expected(&caller);
    let channel = field(bridge.main.admission().definition(), "channels")
        .as_array()
        .unwrap()[0]
        .required("channel")?
        .clone();
    let main = bridge.main.find().await?;
    let input = client
        .state
        .lock()
        .unwrap()
        .records
        .as_array()
        .unwrap()
        .iter()
        .find(|r| field(r, "object").get("contractKey") == Some(&"chat-bridge/input".into()))
        .cloned();
    let input_id = input
        .as_ref()
        .map(|r| field(field(r, "object"), "id").clone())
        .unwrap_or_else(|| "missing-input".into());
    let common = Json::object([
        ("action", name.as_str().into()),
        ("operationId", operation(&label)),
        ("expectedBridge", expected.clone()),
    ]);
    let mut args = match name.as_str() {
        "workspace" => Json::object([] as [(&str, Json); 0]),
        "createMain" => {
            let mut args = common;
            args.insert(
                "expectedMainRevision",
                if flag(action, "current") {
                    main.as_ref()
                        .map(|m| Json::Number(m.pin.revision as f64))
                        .unwrap_or(Json::Null)
                } else {
                    Json::Null
                },
            );
            args.insert("reason", "Explicit synthetic Main creation.".into());
            args
        }
        "send" => {
            let mut args = common;
            args.insert("channel", channel.clone());
            args.insert("messageId", label.clone().into());
            args.insert(
                "expectedBinding",
                main.as_ref()
                    .and_then(|m| m.value.get("binding"))
                    .cloned()
                    .unwrap_or(Json::Null),
            );
            args.insert(
                "payload",
                Json::object([
                    ("text", "Original queued input.".into()),
                    ("images", Json::Array(vec![])),
                ]),
            );
            args
        }
        "cancel" => {
            let mut args = common;
            args.insert("inputId", input_id.clone());
            args.insert(
                "expectedRevision",
                input
                    .as_ref()
                    .map(|r| field(field(r, "revision"), "revision").clone())
                    .unwrap_or(Json::Number(1.0)),
            );
            args.insert("reason", "Explicit synthetic cancellation.".into());
            args
        }
        "receive" => {
            let mut args = common;
            args.insert("channel", channel.clone());
            args.insert("afterSequence", Json::Number(0.0));
            args.insert("limit", Json::Number(8.0));
            args
        }
        "acknowledge" => {
            let receive_caller = action
                .get("receiveCaller")
                .and_then(Json::as_js_string)
                .unwrap_or(&caller);
            let receive_label = action
                .get("receiveLabel")
                .map(|v| v.as_str().unwrap())
                .unwrap_or_else(|| "provider-receive".into());
            let saved = bridge
                .main
                .operations
                .find(
                    receive_caller,
                    operation(&receive_label).as_js_string().unwrap(),
                )
                .await?;
            let offer = saved
                .as_ref()
                .and_then(|r| r.value.get("outcome"))
                .and_then(|r| r.get("offer"));
            let mut args = common;
            args.insert(
                "offerId",
                offer
                    .and_then(|o| o.get("object"))
                    .and_then(|o| o.get("objectId"))
                    .cloned()
                    .unwrap_or_else(|| "missing-offer".into()),
            );
            args.insert(
                "replyIds",
                offer
                    .map(|o| {
                        Json::Array(
                            field(field(o, "data"), "replies")
                                .as_array()
                                .unwrap()
                                .iter()
                                .take(1)
                                .map(|p| field(p, "objectId").clone())
                                .collect(),
                        )
                    })
                    .unwrap_or_else(|| Json::Array(vec!["missing-reply".into()])),
            );
            args
        }
        "operation" => Json::object([
            ("expectedBridge", expected.clone()),
            (
                "operationId",
                operation(
                    &action
                        .get("operationLabel")
                        .map(|v| v.as_str().unwrap())
                        .unwrap_or_else(|| "provider-send".into()),
                ),
            ),
        ]),
        "history" => Json::object([
            ("expectedBridge", expected.clone()),
            ("channel", channel),
            ("afterSequence", Json::Number(0.0)),
            ("limit", Json::Number(8.0)),
        ]),
        "input" => Json::object([
            ("expectedBridge", expected.clone()),
            ("inputId", input_id.clone()),
        ]),
        "notify" => Json::object([
            ("action", name.as_str().into()),
            ("operationId", operation(&label)),
            (
                "source",
                Json::object([
                    ("objectId", "synthetic-source".into()),
                    ("revision", Json::Number(1.0)),
                ]),
            ),
            ("text", "Original private context.".into()),
        ]),
        "notice" => Json::object([(
            "operationId",
            operation(
                &action
                    .get("operationLabel")
                    .map(|v| v.as_str().unwrap())
                    .unwrap_or_else(|| "provider-notify".into()),
            ),
        )]),
        "retryResult" => {
            let id = input_id.as_str()?;
            let collection = if input.is_some() {
                bridge.worker.collector.find(&id).await?
            } else {
                None
            };
            let selected = if flag(action, "replay") {
                collection
                    .as_ref()
                    .and_then(|c| c.value.get("retryAuthorization"))
                    .and_then(|r| r.get("failed"))
                    .cloned()
            } else {
                collection.as_ref().map(|c| c.pin.json())
            };
            Json::object([
                ("inputId", input_id),
                (
                    "expectedCollection",
                    selected.unwrap_or_else(|| {
                        Json::object([
                            ("objectId", "missing-collection".into()),
                            ("revision", Json::Number(1.0)),
                        ])
                    }),
                ),
                ("expectedBridge", expected),
                ("operationId", operation(&label)),
                ("reason", "Explicit original collection retry.".into()),
            ])
        }
        _ => panic!("Unknown provider request"),
    };
    if let Some(patch) = action.get("patch") {
        merge(&mut args, patch);
    }
    if let Some(patch) = action.get("scopePatch") {
        let mut expected = field(&args, "expectedBridge").clone();
        merge(&mut expected, patch);
        args.insert("expectedBridge", expected);
    }
    if flag(action, "replay") && name == "cancel" {
        args = bridge
            .main
            .operations
            .find(&caller, operation(&label).as_js_string().unwrap())
            .await?
            .unwrap()
            .value
            .required("request")?
            .clone();
    }
    if let Some(value) = action.get("args") {
        args = value.clone();
    }
    Ok((caller, args))
}

#[tokio::test]
async fn original_provider_operations_use_sdk_context_and_real_domain_owners() {
    let oracle = oracle();
    let now = field(oracle, "now").as_f64().unwrap() as i64;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let catalogs = CheckedNativeContracts::new(root);
    let unpack = |name| {
        parse(
            std::str::from_utf8(&STANDARD.decode(text(oracle, name)).unwrap()).unwrap(),
            usize::MAX,
        )
        .unwrap()
    };
    let instructions = unpack("instructions").as_js_string().unwrap().clone();
    let notice = NoticeInstructions::from_json(&unpack("noticeInstructions")).unwrap();
    let peer = ContextPeer::start().await;
    let mut steps = 0;
    let mut errors = 0;
    let mut rpc_calls = 0;
    for case in field(oracle, "cases").as_array().unwrap() {
        let seed = field(field(oracle, "seeds"), &text(case, "seed"));
        let path = std::env::temp_dir().join(format!("ivy-rust-provider-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(
            path.join("chat-bridge.sqlite"),
            STANDARD.decode(text(seed, "database")).unwrap(),
        )
        .unwrap();
        let base = Arc::new(Client {
            path: path.clone(),
            state: Mutex::new(Replay {
                pending: VecDeque::new(),
                mutations: vec![],
                records: field(seed, "records").clone(),
            }),
        });
        let client = Arc::new(ProviderClient {
            base: base.clone(),
            connection: Mutex::new(None),
        });
        let tools = Arc::new(ToolClients::new(client.clone()));
        let definition = field(seed, "definition").clone();
        let actions = field(case, "steps").as_array().unwrap();
        let mut index = 0;
        while index < actions.len() {
            let (connection, original_context) = peer.context().await;
            *client.connection.lock().unwrap() = Some(connection.clone());
            let store = ChatStore::with_clock(
                client.clone(),
                None,
                Some(path.clone()),
                Arc::new(Fixed(now)),
            )
            .await
            .unwrap();
            let admission = ChatAdmission::new(&definition).unwrap();
            let ops = ChatOperations::new(&store, &admission).unwrap();
            let native = ChatNativeDriver::new(
                &store,
                tools.clone(),
                &admission,
                &catalogs,
                "chat-provider".into(),
                &instructions,
            )
            .unwrap();
            let main = ChatMain::new(&ops, &native, &notice).unwrap();
            let evidence = native.evidence().unwrap();
            let delivery = ChatDelivery::new(&main);
            let collector = ChatCollector::new(&ops, &evidence, &delivery, &catalogs);
            let mut bridge = ChatBridge::new(&delivery, &collector, 7).unwrap();
            loop {
                if index >= actions.len() {
                    break;
                }
                let action = &actions[index];
                let expected = &field(case, "expected").as_array().unwrap()[index];
                let transport = field(expected, "transport");
                rpc_calls += transport.as_array().unwrap().len();
                base.begin(transport);
                let value = match text(action, "action").as_str() {
                    "reopen" => Ok(Json::Null),
                    "provider-worker" => bridge.worker.step().await.map(|_| Json::Null),
                    "provider" => {
                        let (caller, args) = request(action, &bridge, &base).await.unwrap();
                        let mut context = original_context.clone();
                        context.caller_principal_id = caller;
                        context.generation = action
                            .get("generation")
                            .and_then(Json::as_f64)
                            .unwrap_or(7.0) as u64;
                        context.operation_id = action
                            .get("transportId")
                            .unwrap_or_else(|| args.get("operationId").unwrap_or(&Json::Null))
                            .as_js_string()
                            .cloned();
                        if flag(action, "abortBefore") {
                            connection.close_with(abort_reason());
                        }
                        let value = bridge.invoke(&text(action, "name"), &args, &context).await;
                        if let Err(error) = &value {
                            if error.code_js() == JsString::from("fixture_provider_closed") {
                                assert_eq!(error.details, abort_reason().details);
                                assert_eq!(error.outcome, Outcome::Unknown);
                            }
                        }
                        value
                    }
                    _ => main_step(action, &main, &base, &notice).await,
                };
                let actual = snapshot(&path, &base, result(value));
                assert!(
                    base.state.lock().unwrap().pending.is_empty(),
                    "Missing provider RPC {} step {index}",
                    text(case, "label")
                );
                let mut expected = expected.clone();
                expected.remove("transport");
                equal(
                    &actual,
                    &expected,
                    &format!("Provider {} step {index}", text(case, "label")),
                );
                if field(&actual, "output").get("error").is_some() {
                    errors += 1;
                }
                steps += 1;
                index += 1;
                if index >= actions.len() || text(&actions[index], "action") == "reopen" {
                    break;
                }
            }
            connection.close();
            client.connection.lock().unwrap().take();
        }
        std::fs::remove_dir_all(path).unwrap();
    }
    peer.finish().await;
    assert!(steps >= 150);
    assert!(errors >= 50);
    eprintln!(
        "Original provider: {} workflows, {steps} steps, {errors} expected errors, {rpc_calls} exact RPC comparisons; all twelve operations, actual SDK context/close reason and actual domain/worker composition.",
        field(oracle, "cases").as_array().unwrap().len()
    );
}
