use super::*;
use crate::store::{CollectorStore, Limits};
use ivy_contracts::{hash::hash_json, json::parse};
use ivy_sdk::rpc::RequestFuture;
use std::{collections::HashMap, path::PathBuf};
use tokio::sync::Notify;

const TIME: &str = "2026-01-02T03:04:05.006Z";
fn json(source: &str) -> IvyJson {
    parse(source, usize::MAX).unwrap()
}
fn digest(c: char) -> IvyJson {
    format!("sha256:{}", c.to_string().repeat(64)).into()
}
#[derive(Clone, Copy, Default)]
enum Mode {
    #[default]
    Normal,
    HoldStart,
    LostStart,
    UnknownStart,
    HoldTool,
    LostUnsubscribe,
    HoldUnsubscribe,
}
#[derive(Default)]
struct Client {
    store: Mutex<Option<StoreOwner>>,
    operations: Mutex<HashMap<String, IvyJson>>,
    invokes: Mutex<Vec<IvyJson>>,
    mode: Mutex<Mode>,
    epoch: Mutex<Option<String>>,
    entered: Notify,
    release: Notify,
}
impl Client {
    fn status(&self) -> IvyJson {
        IvyJson::object([
            ("serviceNodeId", "native-agent".into()),
            ("hostId", "fixture".into()),
            ("nativeVersion", "0.159.2".into()),
            ("nativeExecutableHash", digest('d')),
            ("catalogHash", digest('c')),
            (
                "epoch",
                self.epoch
                    .lock()
                    .unwrap()
                    .as_deref()
                    .unwrap_or("epoch")
                    .into(),
            ),
            ("pid", 42.0.into()),
            ("state", "ready".into()),
            ("observedAt", TIME.into()),
            ("code", IvyJson::Null),
            ("initialized", object()),
            ("pendingInputs", 0.0.into()),
            (
                "operations",
                json(r#"{"retained":0,"maximum":100,"bytes":0,"maximumBytes":1048576}"#),
            ),
            ("observedMethods", IvyJson::Array(vec![])),
        ])
    }
    fn observation(&self, args: &IvyJson, phase: &str) -> IvyJson {
        let method = args.required("method").unwrap().clone();
        let params = args.required("params").unwrap().clone();
        let request_hash = hash_json(&IvyJson::object([
            ("method", method.clone()),
            ("params", params.clone()),
        ]))
        .unwrap();
        let mut value = IvyJson::object([
            ("schemaVersion", 1.0.into()),
            ("operationId", args.required("operationId").unwrap().clone()),
            ("callerPrincipalId", "collector".into()),
            ("serviceNodeId", "native-agent".into()),
            ("nativeVersion", "0.159.2".into()),
            ("nativeExecutableHash", digest('d')),
            ("method", method),
            ("params", params),
            ("requestHash", request_hash.into()),
            ("createdAt", TIME.into()),
            ("updatedAt", TIME.into()),
            ("phase", phase.into()),
            ("epoch", "epoch".into()),
            ("requestId", 1.0.into()),
            ("reply", IvyJson::Null),
            ("code", IvyJson::Null),
        ]);
        if phase == "succeeded" {
            value.insert("reply", IvyJson::object([("result", Self::reply(args))]));
        }
        if phase == "outcome_unknown" {
            value.insert("code", "native_exit".into());
        }
        value
    }
    fn reply(args: &IvyJson) -> IvyJson {
        match model::text(args, "method").unwrap().as_str() {
            "thread/start" => json(r#"{"thread":{"id":"own-thread","ephemeral":true}}"#),
            "mcpServer/tool/call" => {
                json(r#"{"content":[{"type":"text","text":"{\"ok\":true}"}],"isError":false}"#)
            }
            "thread/unsubscribe" => object(),
            other => panic!("Unexpected native method {other}"),
        }
    }
    fn finish(&self, method: &str) {
        let mut operations = self.operations.lock().unwrap();
        let mut found = false;
        for operation in operations
            .values_mut()
            .filter(|v| v.get("method") == Some(&method.into()))
        {
            operation.insert("phase", "succeeded".into());
            operation.insert("code", IvyJson::Null);
            operation.insert(
                "reply",
                IvyJson::object([("result", Self::reply(operation))]),
            );
            found = true;
        }
        assert!(found, "No original {method}");
    }
    fn count(&self, method: &str) -> usize {
        self.invokes
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v.get("method") == Some(&method.into()))
            .count()
    }
    fn original(&self, method: &str) -> String {
        model::text(
            self.invokes
                .lock()
                .unwrap()
                .iter()
                .find(|v| v.get("method") == Some(&method.into()))
                .unwrap(),
            "operationId",
        )
        .unwrap()
    }
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            if method == "system.status" {
                return Ok(json(
                    r#"{"callerPrincipalId":"collector","runtimeEpoch":"hive-epoch"}"#,
                ));
            }
            if method == "tools.list" {
                let namespace = model::text(&params, "namespace")?;
                let name = model::text(&params, "namePrefix")?;
                return Ok(IvyJson::object([
                    (
                        "provider",
                        json(r#"{"node":{"serviceNodeId":"native-agent"}}"#),
                    ),
                    (
                        "items",
                        IvyJson::Array(vec![IvyJson::object([
                            ("qualifiedName", format!("{namespace}.{name}").into()),
                            (
                                "definition",
                                IvyJson::object([
                                    ("namespace", namespace.clone().into()),
                                    ("name", name.into()),
                                    (
                                        "interfaceVersion",
                                        if namespace == "agent" {
                                            "1.0.0"
                                        } else {
                                            "0.159.2"
                                        }
                                        .into(),
                                    ),
                                    ("inputSchema", object()),
                                    ("outputSchema", object()),
                                ]),
                            ),
                            (
                                "definitionHash",
                                digest(if namespace == "agent" { 'a' } else { 'b' }),
                            ),
                            ("catalogHash", digest('c')),
                        ])]),
                    ),
                    ("nextCursor", IvyJson::Null),
                ]));
            }
            assert_eq!(method, "tools.call");
            let args = params.required("arguments")?;
            match model::text(&params, "qualifiedName")?.as_str() {
                "agent.status" => Ok(self.status()),
                "agent.operation" => {
                    let id = model::text(args, "operationId")?;
                    self.operations
                        .lock()
                        .unwrap()
                        .get(&id)
                        .cloned()
                        .ok_or_else(|| {
                            let mut error = IvyError::new("not_found", "No original operation.");
                            error.details = Some(IvyJson::object([
                                ("kind", "agent_operation_absent".into()),
                                ("operationId", id.into()),
                                ("serviceNodeId", "native-agent".into()),
                                ("epoch", "epoch".into()),
                            ]));
                            error
                        })
                }
                "agent.invoke" => {
                    let store = self.store.lock().unwrap().as_ref().unwrap().clone();
                    let id = model::text(args, "operationId")?;
                    let wanted = id.clone();
                    // Assert the durable write, including exact native parameters, precedes every send.
                    let request = args.clone();
                    store
                        .call(move |store| {
                            let mut rows = store
                                .connection()
                                .prepare("SELECT value FROM metadata WHERE key GLOB 'native-mcp:*'")
                                .unwrap();
                            let mut values = rows.query([]).unwrap();
                            let mut found = false;
                            while let Some(row) = values.next().unwrap() {
                                let value = json(&row.get::<_, String>(0).unwrap());
                                for name in ["start", "pending", "unsubscribe"] {
                                    if let Some(call) = value.get(name).filter(|v| {
                                        v.get("operationId") == Some(&wanted.clone().into())
                                    }) {
                                        assert_eq!(call.get("method"), request.get("method"));
                                        assert_eq!(call.get("params"), request.get("params"));
                                        found = true;
                                    }
                                }
                            }
                            assert!(found, "Native dispatch had no durable original identity");
                            Ok(())
                        })
                        .await?;
                    self.invokes.lock().unwrap().push(args.clone());
                    let native_method = model::text(args, "method")?;
                    let mode = *self.mode.lock().unwrap();
                    let held = matches!(
                        (mode, native_method.as_str()),
                        (Mode::HoldStart, "thread/start")
                            | (Mode::HoldTool, "mcpServer/tool/call")
                            | (Mode::HoldUnsubscribe, "thread/unsubscribe")
                    );
                    let lost = matches!(
                        (mode, native_method.as_str()),
                        (Mode::LostStart, "thread/start")
                            | (Mode::LostUnsubscribe, "thread/unsubscribe")
                    );
                    let unknown = matches!(
                        (mode, native_method.as_str()),
                        (Mode::UnknownStart, "thread/start")
                    );
                    let value = self.observation(
                        args,
                        if unknown {
                            "outcome_unknown"
                        } else if held || lost {
                            "dispatched"
                        } else {
                            "succeeded"
                        },
                    );
                    self.operations.lock().unwrap().insert(id, value.clone());
                    if held {
                        self.entered.notify_one();
                        self.release.notified().await;
                    }
                    if lost {
                        self.entered.notify_one();
                        return Err(IvyError::new("connection_closed", "Reply lost.")
                            .with_outcome(Outcome::Unknown));
                    }
                    Ok(value)
                }
                other => panic!("Unexpected management method {other}"),
            }
        })
    }
}
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("ivy-collector-native-mcp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn store(&self) -> StoreOwner {
        StoreOwner::new(
            CollectorStore::open(&self.0.join("collector.sqlite"), Limits::default(), TIME)
                .unwrap(),
        )
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn fixture(mode: Mode) -> (Root, Arc<Client>, Arc<NativeMcpFactory>, NativeMcpSession) {
    let root = Root::new();
    let store = root.store();
    let client = Arc::new(Client::default());
    *client.store.lock().unwrap() = Some(store.clone());
    *client.mode.lock().unwrap() = mode;
    let current = client.clone();
    let factory = Arc::new(NativeMcpFactory::new(
        Arc::new(move || Ok(current.clone())),
        store,
    ));
    let (rpc, tools) = factory.selected().unwrap();
    let cwd = if cfg!(windows) {
        "C:/fixture"
    } else {
        "/fixture"
    };
    let configuration = IvyJson::object([
        ("serviceNodeId", "native-agent".into()),
        ("cwd", cwd.into()),
        ("server", "fixture".into()),
        ("tools", IvyJson::Array(vec!["echo".into()])),
    ]);
    let session = NativeMcpSession::new(
        rpc,
        tools,
        configuration,
        CancellationToken::new(),
        factory.contexts.clone(),
    )
    .unwrap();
    (root, client, factory, session)
}
async fn retained(factory: &NativeMcpFactory, client: &Client) -> Option<Context> {
    factory
        .contexts
        .read(format!("native-mcp:{}", client.original("thread/start")))
        .await
        .unwrap()
}
async fn call(session: &NativeMcpSession) -> IvyResult<IvyJson> {
    session.call("echo".into(), object()).await
}
async fn entered(client: &Client) {
    tokio::time::timeout(Duration::from_secs(5), client.entered.notified())
        .await
        .unwrap();
}

#[tokio::test]
async fn normal_context_is_durably_owned_before_dispatch_and_retires_once() {
    let (_root, client, factory, session) = fixture(Mode::Normal);
    assert!(call(&session).await.is_ok());
    assert!(
        session
            .call(
                "echo".into(),
                json(r#"{"lone":"\ud800","null":null,"false":false,"nested":["😀",{"zero":0}]}"#)
            )
            .await
            .is_ok()
    );
    assert_eq!(client.count("thread/start"), 1);
    assert_eq!(client.count("mcpServer/tool/call"), 2);
    assert!(retained(&factory, &client).await.unwrap().pending.is_none());
    session.close().await.unwrap();
    session.close().await.unwrap();
    factory.recover().await.unwrap();
    assert_eq!(client.count("thread/unsubscribe"), 1);
    assert!(retained(&factory, &client).await.is_none());
    assert!(call(&session).await.is_err());
    assert_eq!(
        client.operations.lock().unwrap().len(),
        4,
        "Original receipts stay with the native owner"
    );
}
#[tokio::test]
async fn close_during_start_keeps_the_original_and_retires_its_delayed_thread() {
    let (_root, client, factory, session) = fixture(Mode::HoldStart);
    let pending = session.clone();
    let task = tokio::spawn(async move { call(&pending).await });
    entered(&client).await;
    session.close().await.unwrap();
    assert!(task.await.unwrap().is_err());
    let context = retained(&factory, &client).await.unwrap();
    assert!(context.closing && context.thread.is_none());
    assert_eq!(client.count("thread/unsubscribe"), 0);
    client.finish("thread/start");
    factory.recover().await.unwrap();
    assert_eq!(client.count("thread/start"), 1);
    assert_eq!(client.count("thread/unsubscribe"), 1);
    assert!(retained(&factory, &client).await.is_none());
}
#[tokio::test]
async fn task_cancellation_keeps_the_start_identity() {
    let (_root, client, factory, session) = fixture(Mode::HoldStart);
    let pending = session.clone();
    let task = tokio::spawn(async move { call(&pending).await });
    entered(&client).await;
    session.0.signal.cancel();
    task.await.unwrap().unwrap_err();
    session.close().await.unwrap();
    assert!(retained(&factory, &client).await.unwrap().closing);
    client.finish("thread/start");
    factory.recover().await.unwrap();
    assert!(retained(&factory, &client).await.is_none());
    assert_eq!(client.count("thread/start"), 1);
}
#[tokio::test]
async fn lost_start_is_not_replayed_and_survives_a_real_database_reopen() {
    let (root, client, factory, session) = fixture(Mode::LostStart);
    let pending = session.clone();
    let task = tokio::spawn(async move { call(&pending).await });
    entered(&client).await;
    // The SDK polls the original after a lost admission reply. Losing that
    // waiter must not lose the already persisted identity or dispatch again.
    task.abort();
    let _ = task.await;
    call(&session).await.unwrap_err();
    assert_eq!(client.count("thread/start"), 1);
    // Simulate a crash before close: the old session did not mark itself closing.
    let old_key = retained(&factory, &client).await.unwrap().key().unwrap();
    assert!(
        factory
            .contexts
            .next(String::new())
            .await
            .unwrap()
            .is_none()
    );
    let store = factory.contexts.store.clone();
    drop(session);
    drop(factory);
    store.close().await.unwrap();
    let reopened = root.store();
    *client.store.lock().unwrap() = Some(reopened.clone());
    let current = client.clone();
    let factory = NativeMcpFactory::new(Arc::new(move || Ok(current.clone())), reopened);
    factory.recover().await.unwrap_err();
    assert!(
        factory
            .contexts
            .read(old_key.clone())
            .await
            .unwrap()
            .is_some()
    );
    client.finish("thread/start");
    factory.recover().await.unwrap();
    assert!(factory.contexts.read(old_key).await.unwrap().is_none());
    assert_eq!(client.count("thread/start"), 1);
    assert_eq!(client.count("thread/unsubscribe"), 1);
}
#[tokio::test]
async fn outcome_unknown_absence_and_changed_owner_do_not_authorize_unloading() {
    for mismatch in [
        "unknown",
        "absent",
        "caller",
        "params",
        "epoch",
        "persistent",
        "truthy",
    ] {
        let (_root, client, factory, session) = fixture(Mode::UnknownStart);
        tokio::time::timeout(Duration::from_secs(2), call(&session))
            .await
            .unwrap()
            .unwrap_err();
        let id = client.original("thread/start");
        if mismatch != "unknown" {
            client.finish("thread/start");
        }
        {
            let mut operations = client.operations.lock().unwrap();
            match mismatch {
                "absent" => {
                    operations.remove(&id);
                }
                "caller" => {
                    operations
                        .get_mut(&id)
                        .unwrap()
                        .insert("callerPrincipalId", "someone-else".into());
                }
                "params" => {
                    operations.get_mut(&id).unwrap().insert("params", object());
                }
                "epoch" => {
                    *client.epoch.lock().unwrap() = Some("another-epoch".into());
                }
                "truthy" => {
                    operations.get_mut(&id).unwrap().insert(
                        "reply",
                        json(r#"{"result":{"thread":{"id":"foreign-thread","ephemeral":"true"}}}"#),
                    );
                }
                "persistent" => {
                    operations.get_mut(&id).unwrap().insert(
                        "reply",
                        json(r#"{"result":{"thread":{"id":"foreign-thread","ephemeral":false}}}"#),
                    );
                }
                _ => (),
            }
        }
        session.close().await.unwrap();
        factory.recover().await.unwrap_err();
        assert!(retained(&factory, &client).await.is_some(), "{mismatch}");
        assert_eq!(client.count("thread/unsubscribe"), 0, "{mismatch}");
        assert_eq!(client.count("thread/start"), 1, "{mismatch}");
    }
}
#[tokio::test]
async fn a_confirmed_failed_start_releases_only_its_local_slot() {
    let (_root, client, factory, session) = fixture(Mode::UnknownStart);
    call(&session).await.unwrap_err();
    let id = client.original("thread/start");
    {
        let mut operations = client.operations.lock().unwrap();
        let operation = operations.get_mut(&id).unwrap();
        operation.insert("phase", "failed".into());
        operation.insert("code", "native_error".into());
        operation.insert(
            "reply",
            json(r#"{"error":{"code":-32602,"message":"Start rejected."}}"#),
        );
    }
    session.close().await.unwrap();
    assert!(retained(&factory, &client).await.is_none());
    assert_eq!(client.count("thread/unsubscribe"), 0);
    assert_eq!(client.count("thread/start"), 1);
    assert_eq!(
        client
            .operations
            .lock()
            .unwrap()
            .get(&id)
            .unwrap()
            .get("phase"),
        Some(&"failed".into())
    );
}
#[tokio::test]
async fn unresolved_tool_work_keeps_its_context_and_original_call() {
    let (_root, client, factory, session) = fixture(Mode::HoldTool);
    let pending = session.clone();
    let task = tokio::spawn(async move { call(&pending).await });
    entered(&client).await;
    session.close().await.unwrap();
    task.await.unwrap().unwrap_err();
    let before = retained(&factory, &client).await.unwrap();
    assert_eq!(
        before.pending.unwrap().operation_id,
        client.original("mcpServer/tool/call").into()
    );
    factory.recover().await.unwrap_err();
    assert_eq!(client.count("thread/unsubscribe"), 0);
    client.finish("mcpServer/tool/call");
    factory.recover().await.unwrap();
    assert_eq!(client.count("mcpServer/tool/call"), 1);
    assert_eq!(client.count("thread/unsubscribe"), 1);
    assert!(retained(&factory, &client).await.is_none());
}
#[tokio::test]
async fn a_lost_unsubscribe_is_reconciled_without_another_send() {
    let (_root, client, factory, session) = fixture(Mode::LostUnsubscribe);
    call(&session).await.unwrap();
    let mut stale = retained(&factory, &client).await.unwrap();
    session.close().await.unwrap();
    // A canceled store waiter may leave this old write queued on the blocking
    // owner. It must not erase closing or the newly retained unsubscribe.
    factory.contexts.save(&mut stale).await.unwrap_err();
    let original = retained(&factory, &client)
        .await
        .unwrap()
        .unsubscribe
        .unwrap()
        .operation_id;
    factory.recover().await.unwrap_err();
    session.close().await.unwrap();
    assert_eq!(client.count("thread/unsubscribe"), 1);
    assert_eq!(
        retained(&factory, &client)
            .await
            .unwrap()
            .unsubscribe
            .unwrap()
            .operation_id,
        original
    );
    client.finish("thread/unsubscribe");
    factory.recover().await.unwrap();
    assert_eq!(client.count("thread/unsubscribe"), 1);
    assert!(retained(&factory, &client).await.is_none());
}
#[tokio::test]
async fn a_dropped_bridge_close_and_network_timeout_keep_the_cleanup_receipt() {
    let (_root, client, factory, session) = fixture(Mode::HoldUnsubscribe);
    call(&session).await.unwrap();
    let bridge = LocalBridge::open(Arc::new(session), CancellationToken::new())
        .await
        .unwrap();
    let close = tokio::spawn(async move { bridge.close().await });
    entered(&client).await;
    close.abort();
    tokio::time::sleep(Duration::from_millis(5100)).await;
    let context = retained(&factory, &client).await.unwrap();
    assert!(context.closing && context.unsubscribe.is_some());
    client.finish("thread/unsubscribe");
    factory.recover().await.unwrap();
    assert_eq!(client.count("thread/unsubscribe"), 1);
    assert!(retained(&factory, &client).await.is_none());
}
#[tokio::test]
async fn pending_context_limit_prevents_dispatch_and_recovery_rotates_past_unknowns() {
    let (_root, client, factory, session) = fixture(Mode::UnknownStart);
    call(&session).await.unwrap_err();
    session.close().await.unwrap();
    let original = retained(&factory, &client).await.unwrap();
    for index in 1..64 {
        let mut context = Context {
            revision: 0,
            session_id: "previous-process".into(),
            caller: original.caller.clone(),
            owner: original.owner.clone(),
            epoch: original.epoch.clone(),
            start: original.start.clone(),
            thread: None,
            pending: None,
            unsubscribe: None,
            closing: true,
        };
        context.start.operation_id = format!("another-{index:02}").into();
        factory
            .contexts
            .insert(&context, &CancellationToken::new())
            .await
            .unwrap();
    }
    let mut context = original;
    context.start.operation_id = "over-limit".into();
    factory
        .contexts
        .insert(&context, &CancellationToken::new())
        .await
        .unwrap_err();
    let first = factory.contexts.next(String::new()).await.unwrap().unwrap();
    let second = factory.contexts.next(first.clone()).await.unwrap().unwrap();
    assert_ne!(first, second);
    factory.recover().await.unwrap_err();
    assert_eq!(*factory.contexts.cleanup.lock().await, first);
    factory.recover().await.unwrap_err();
    assert_eq!(*factory.contexts.cleanup.lock().await, second);
    assert_eq!(client.count("thread/start"), 1);
    assert_eq!(client.count("thread/unsubscribe"), 0);
    context.start.operation_id = "closed-before-insert".into();
    let closed = CancellationToken::new();
    closed.cancel();
    factory
        .contexts
        .insert(&context, &closed)
        .await
        .unwrap_err();
    assert!(
        factory
            .contexts
            .read(context.key().unwrap())
            .await
            .unwrap()
            .is_none()
    );
}
