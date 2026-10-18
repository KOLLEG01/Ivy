use ivy_contracts::json::{parse, stringify};
use ivy_sdk::{IvyError, IvyJson, JsString, Outcome, rpc::*, service::*};
use std::{
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::Semaphore,
};
use tokio_util::sync::CancellationToken;

fn object() -> IvyJson {
    IvyJson::object(Vec::<(String, IvyJson)>::new())
}
fn json(text: &str) -> IvyJson {
    parse(text, 32 * 1024 * 1024).unwrap()
}
fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}
fn peer_command() -> Command {
    let mut command = Command::new("node");
    command
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/sdk-provider-peer.mjs",
        ])
        .current_dir(root())
        .kill_on_drop(true);
    command
}
struct Peer {
    child: Child,
    url: String,
    identity: IvyJson,
    registry: IvyJson,
}
impl Peer {
    async fn start() -> Self {
        let mut child = peer_command()
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let first = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let value = json(&first);
        Self {
            child,
            url: value.get("url").unwrap().as_str().unwrap(),
            identity: value.get("identity").unwrap().clone(),
            registry: value.get("registry").unwrap().clone(),
        }
    }
    async fn open(&self, mut options: ConnectionOptions) -> ServiceConnection {
        options.service_node_id = "fixture-node".into();
        let connection = ServiceConnection::connect(
            &self.url,
            "fixture-token",
            options,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let connected = call(&connection, "service.connect", self.identity.clone())
            .await
            .unwrap();
        connection.set_generation(connected.get("generation").unwrap().as_f64().unwrap() as u64);
        connection.set_registry(&self.registry).unwrap();
        connection.set_ready(true);
        connection
    }
    async fn finish(mut self) {
        self.child.kill().await.unwrap();
        self.child.wait().await.unwrap();
    }
}
async fn call(
    connection: &ServiceConnection,
    method: &str,
    params: IvyJson,
) -> ivy_sdk::IvyResult<IvyJson> {
    connection
        .request(
            method,
            params,
            RequestOptions {
                timeout: Duration::from_secs(3),
                ..Default::default()
            },
        )
        .await
}
fn echo() -> ToolHandler {
    Arc::new(|args, context| {
        Box::pin(async move {
            Ok(IvyJson::object([
                ("args", args),
                (
                    "callerPrincipalId",
                    IvyJson::String(context.caller_principal_id),
                ),
                (
                    "operationId",
                    IvyJson::String(context.operation_id.unwrap()),
                ),
                ("generation", IvyJson::Number(context.generation as f64)),
            ]))
        })
    })
}
fn options() -> ConnectionOptions {
    let mut options = ConnectionOptions::new("fixture-node".into());
    options.handlers.insert("fixture.echo".into(), echo());
    options
}
fn capture(result: ivy_sdk::IvyResult<IvyJson>) -> IvyJson {
    match result {
        Ok(value) => IvyJson::object([("ok", true.into()), ("value", value)]),
        Err(error) => IvyJson::object([
            ("ok", false.into()),
            (
                "error",
                IvyJson::object([
                    ("code", IvyJson::String(error.code_js())),
                    ("message", IvyJson::String(error.message_js())),
                    ("outcome", error.outcome.as_str().into()),
                ]),
            ),
        ]),
    }
}
fn without_id(value: IvyJson) -> IvyJson {
    IvyJson::object(
        value
            .into_object()
            .unwrap()
            .into_iter()
            .filter(|(key, _)| key != &JsString::from("id")),
    )
}
fn abort_reason(result: ivy_sdk::IvyResult<()>) -> IvyJson {
    match result {
        Ok(()) => IvyJson::Null,
        Err(error) => IvyJson::object([
            ("code", IvyJson::String(error.code_js())),
            ("message", IvyJson::String(error.message_js())),
            ("outcome", error.outcome.as_str().into()),
            ("details", error.details.unwrap_or(IvyJson::Null)),
        ]),
    }
}
#[tokio::test]
async fn invocation_and_connection_abort_checks_retain_original_error_after_close() {
    let expected = tokio::time::timeout(
        Duration::from_secs(15),
        peer_command().arg("abort-oracle").output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        expected.status.success(),
        "{}",
        String::from_utf8_lossy(&expected.stderr)
    );
    let expected = json(std::str::from_utf8(&expected.stdout).unwrap());
    let peer = Peer::start().await;
    let retained = Arc::new(Mutex::new(None));
    let mut config = options();
    let captured = retained.clone();
    config.handlers.insert(
        "fixture.echo".into(),
        Arc::new(move |_, context| {
            let captured = captured.clone();
            Box::pin(async move {
                context.throw_if_aborted()?;
                *captured.lock().unwrap() = Some(context);
                Ok(true.into())
            })
        }),
    );
    let mut out = vec![];
    for outcome in [
        Some(Outcome::NotExecuted),
        Some(Outcome::Unknown),
        Some(Outcome::Completed),
        None,
    ] {
        let connection = peer.open(config.clone()).await;
        call(&connection, "test.invoke", object()).await.unwrap();
        let context = retained.lock().unwrap().take().unwrap();
        let cloned = context.clone();
        let before = IvyJson::Array(vec![
            abort_reason(connection.throw_if_aborted()),
            abort_reason(context.throw_if_aborted()),
        ]);
        match outcome {
            Some(outcome) => {
                let code = JsString(vec![102, 105, 120, 116, 117, 114, 101, 95, 0xd800]);
                let mut message = JsString::from("Retained ");
                message.0.push(0xdfff);
                message
                    .0
                    .extend(JsString::from(format!(" {}", "x".repeat(2050))).0);
                connection.close_with(
                    IvyError::new_js(code, message)
                        .with_outcome(outcome)
                        .with_details(json(r#"{"text":"\ud800","nested":[1,null,false]}"#)),
                );
            }
            None => connection.close(),
        }
        connection.close_with(IvyError::new("later", "Must not replace the first close."));
        let reason = abort_reason(connection.throw_if_aborted());
        assert_eq!(reason, abort_reason(connection.check_open()));
        drop(connection);
        out.push(IvyJson::object([
            ("before", before),
            ("connection", reason),
            ("context", abort_reason(context.throw_if_aborted())),
            ("clone", abort_reason(cloned.throw_if_aborted())),
            ("aborted", context.signal.is_cancelled().into()),
        ]));
    }
    assert_eq!(IvyJson::Array(out), expected);
    peer.finish().await;
}
#[tokio::test]
async fn requests_handlers_and_notifications_match_typescript() {
    let expected = tokio::time::timeout(
        Duration::from_secs(15),
        peer_command().arg("oracle").output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        expected.status.success(),
        "{}",
        String::from_utf8_lossy(&expected.stderr)
    );
    let expected = json(std::str::from_utf8(&expected.stdout).unwrap());
    let peer = Peer::start().await;
    let notices = Arc::new(Mutex::new(vec![]));
    let observer = notices.clone();
    let mut config = options();
    config.on_notification = Some(Arc::new(move |frame| {
        observer
            .lock()
            .unwrap()
            .push(frame.get("params").unwrap().clone())
    }));
    let connection = peer.open(config).await;
    let mut out = vec![];
    out.push(capture(
        call(
            &connection,
            "test.echo",
            json(r#"{"text":"\ud800","array":[1,null,false]}"#),
        )
        .await,
    ));
    out.push(capture(call(&connection, "test.ping", object()).await));
    out.push(capture(
        connection
            .request(
                "test.delay",
                json(r#"{"ms":75}"#),
                RequestOptions {
                    timeout: Duration::from_millis(10),
                    ..Default::default()
                },
            )
            .await,
    ));
    tokio::time::sleep(Duration::from_millis(90)).await;
    out.push(capture(
        call(&connection, "test.echo", json(r#"{"afterTimeout":true}"#)).await,
    ));
    out.push(capture(call(&connection, "test.badResult", object()).await));
    let pre = CancellationToken::new();
    pre.cancel();
    out.push(capture(
        connection
            .request(
                "test.echo",
                object(),
                RequestOptions {
                    cancellation: pre,
                    cancel_error: IvyError::new("fixture_cancel", "Before sending."),
                    ..Default::default()
                },
            )
            .await,
    ));
    let cancel = CancellationToken::new();
    let signal = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        signal.cancel();
    });
    out.push(capture(
        connection
            .request(
                "test.delay",
                json(r#"{"ms":75}"#),
                RequestOptions {
                    cancellation: cancel,
                    ..Default::default()
                },
            )
            .await,
    ));
    tokio::time::sleep(Duration::from_millis(90)).await;
    for params in [
        object(),
        json(r#"{"generation":2}"#),
        IvyJson::object([(
            "definitionHash",
            format!("sha256:{}", "b".repeat(64)).into(),
        )]),
    ] {
        out.push(without_id(
            call(&connection, "test.invoke", params).await.unwrap(),
        ));
    }
    call(
        &connection,
        "test.hints",
        json(r#"{"sequences":[4,2],"notification":true}"#),
    )
    .await
    .unwrap();
    connection
        .wait_for_events(3, Duration::from_secs(1))
        .await
        .unwrap();
    out.push(IvyJson::Array(notices.lock().unwrap().clone()));
    connection
        .notification(
            "fixture".into(),
            "notice".into(),
            "1.0.0".into(),
            json(r#"{"text":"\udfff"}"#),
        )
        .unwrap();
    let info = call(&connection, "test.info", object()).await.unwrap();
    let header = info.get("header").unwrap();
    out.push(IvyJson::object([
        ("notifications", info.get("notifications").unwrap().clone()),
        (
            "origin",
            (header.get("origin").unwrap().as_str().unwrap()
                == reqwest::Url::parse(&peer.url)
                    .unwrap()
                    .origin()
                    .ascii_serialization())
            .into(),
        ),
        (
            "authorization",
            header.get("authorization").unwrap().clone(),
        ),
        ("path", header.get("path").unwrap().clone()),
        ("extensions", header.get("extensions").unwrap().clone()),
    ]));
    out.push(capture(call(&connection, "test.binary", object()).await));
    assert_eq!(
        stringify(&IvyJson::Array(out), 1_000_000).unwrap(),
        stringify(&expected, 1_000_000).unwrap()
    );
    assert!(connection.signal().is_cancelled());
    peer.finish().await;
}

#[tokio::test]
async fn malformed_frames_close_but_retired_requests_do_not_poison_other_observers() {
    let peer = Peer::start().await;
    for method in ["test.malformed", "test.badHint"] {
        let connection = peer.open(options()).await;
        let error = call(&connection, method, object()).await.unwrap_err();
        assert_eq!(error.code, "invalid_frame");
        assert_eq!(error.outcome, Outcome::Unknown);
        assert_eq!(error.message, "Hive sent malformed protocol data.");
        assert!(connection.signal().is_cancelled());
    }
    let connection = peer.open(options()).await;
    let mut requests = vec![];
    for _ in 0..64 {
        let c = connection.clone();
        requests.push(tokio::spawn(async move {
            call(&c, "test.delay", json(r#"{"ms":400}"#)).await
        }));
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        call(&connection, "test.echo", object())
            .await
            .unwrap_err()
            .code,
        "limit_exceeded"
    );
    for request in requests {
        request.await.unwrap().unwrap();
    }
    assert_eq!(
        call(&connection, "test.echo", true.into()).await.unwrap(),
        true.into()
    );
    // Dropping a Rust observer retires only its correlation slot; it does not replay or close.
    let c = connection.clone();
    let dropped = tokio::spawn(async move { call(&c, "test.delay", json(r#"{"ms":100}"#)).await });
    tokio::time::sleep(Duration::from_millis(10)).await;
    dropped.abort();
    let _ = dropped.await;
    tokio::time::sleep(Duration::from_millis(120)).await;
    call(&connection, "test.echo", object()).await.unwrap();
    connection.close();
    peer.finish().await;
}

#[tokio::test]
async fn accepted_handlers_retain_capacity_across_connection_generations() {
    let peer = Peer::start().await;
    let capacity = InvocationCapacity::default();
    let release = Arc::new(Semaphore::new(0));
    let entered = Arc::new(AtomicUsize::new(0));
    let finished = Arc::new(AtomicUsize::new(0));
    let mut config = options();
    config.capacity = capacity.clone();
    let gate = release.clone();
    let count = entered.clone();
    let completed = finished.clone();
    config.handlers.insert(
        "fixture.echo".into(),
        Arc::new(move |_, context| {
            let gate = gate.clone();
            let count = count.clone();
            let completed = completed.clone();
            Box::pin(async move {
                context.throw_if_aborted()?;
                count.fetch_add(1, Ordering::SeqCst);
                gate.acquire().await.unwrap().forget();
                assert!(context.signal.is_cancelled());
                let error = context.throw_if_aborted().unwrap_err();
                assert_eq!(error.code, "service_unavailable");
                assert_eq!(error.outcome, Outcome::Unknown);
                assert_eq!(error.message, "Service connection is stopping.");
                completed.fetch_add(1, Ordering::SeqCst);
                Ok(true.into())
            })
        }),
    );
    let first = peer.open(config).await;
    call(&first, "test.startInvocations", json(r#"{"count":64}"#))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while entered.load(Ordering::SeqCst) != 64 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    first.close();
    let mut config = options();
    config.capacity = capacity;
    let second = peer.open(config).await;
    let denied = call(&second, "test.invoke", object()).await.unwrap();
    assert_eq!(
        denied
            .get("error")
            .unwrap()
            .get("data")
            .unwrap()
            .get("code"),
        Some(&"limit_exceeded".into())
    );
    release.add_permits(64);
    tokio::time::timeout(Duration::from_secs(2), async {
        while finished.load(Ordering::SeqCst) != 64 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(
        call(&second, "test.invoke", object())
            .await
            .unwrap()
            .get("result")
            .is_some()
    );
    let info = call(&second, "test.info", object()).await.unwrap();
    assert!(
        info.get("previous").unwrap().as_array().unwrap()[0]
            .get("results")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    second.close();
    peer.finish().await;
}

#[tokio::test]
async fn event_persistence_failure_withholds_ack_and_cancellation_preserves_gap_order() {
    let peer = Peer::start().await;
    let connection = peer.open(options()).await;
    let persisted = Arc::new(Mutex::new(vec![]));
    let trace = persisted.clone();
    let observed = Arc::new(AtomicUsize::new(0));
    let failures = observed.clone();
    let error = consume_events(
        &connection,
        json(r#"{"name":"fixture"}"#),
        Arc::new(move |batch, _| {
            let trace = trace.clone();
            Box::pin(async move {
                let sequence = batch.get("throughSequence").unwrap().as_f64().unwrap();
                if sequence == 2.0 {
                    return Err(IvyError::new(
                        "storage_unavailable",
                        "Fixture persistence failed.",
                    ));
                }
                trace.lock().unwrap().push(sequence);
                Ok(())
            })
        }),
        ConsumeEventsOptions {
            poll: Duration::from_millis(25),
            on_failure: Some(Arc::new(move |error, batch| {
                assert_eq!(error.code, "storage_unavailable");
                assert_eq!(batch.get("throughSequence").unwrap().as_f64(), Some(2.0));
                failures.fetch_add(1, Ordering::SeqCst);
            })),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "storage_unavailable");
    assert_eq!(observed.load(Ordering::SeqCst), 1);
    assert_eq!(*persisted.lock().unwrap(), vec![1.0]);
    let info = call(&connection, "test.info", object()).await.unwrap();
    assert_eq!(
        info.get("acks"),
        Some(&json(
            r#"[{"name":"fixture","throughSequence":1,"gapThroughSequence":1}]"#
        ))
    );
    let c = connection.clone();
    let error = consume_events(
        &connection,
        json(r#"{"name":"fixture"}"#),
        Arc::new(move |_, _| {
            let c = c.clone();
            Box::pin(async move {
                c.close();
                Ok(())
            })
        }),
        ConsumeEventsOptions::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "service_unavailable");
    peer.finish().await;
}

#[tokio::test]
async fn lifecycle_reconciles_before_readiness_and_reconnects_without_replaying_requests() {
    let peer = Peer::start().await;
    let states = Arc::new(Mutex::new(vec![]));
    let observed = states.clone();
    let reconciled = Arc::new(AtomicUsize::new(0));
    let count = reconciled.clone();
    let registry = peer.registry.clone();
    let mut options = ServiceOptions::new(
        peer.url.replace("/ivy/", "/slow/"),
        Arc::new(|| Box::pin(async { Ok("fixture-token".into()) })),
        peer.identity.clone(),
        Arc::new(move || {
            let registry = registry.clone();
            Box::pin(async move { Ok(registry) })
        }),
        Arc::new(move |connection| {
            let count = count.clone();
            Box::pin(async move {
                assert!(!connection.ready());
                call(&connection, "test.echo", json(r#"{"reconcile":true}"#)).await?;
                count.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }),
    );
    options.handlers.insert("fixture.echo".into(), echo());
    options.heartbeat = Duration::from_millis(50);
    options.reconnect_min = Duration::from_millis(25);
    options.reconnect_max = Duration::from_millis(25);
    options.on_state = Some(Arc::new(move |state| observed.lock().unwrap().push(state)));
    options.on_notification = Some(Arc::new(|_| {}));
    let client = ServiceClient::new(options).unwrap();
    assert_eq!(client.connection().err().unwrap().code, "service_not_ready");
    client.start();
    client.start();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!client.ready());
    let connection = client
        .wait_ready(Some(RequestOptions {
            timeout: Duration::from_secs(5),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert_eq!(reconciled.load(Ordering::SeqCst), 1);
    let info = call(&connection, "test.info", object()).await.unwrap();
    assert_eq!(
        &info.get("trace").unwrap().as_array().unwrap()[..5],
        &[
            "service.connect".into(),
            "registry.sync".into(),
            "notifications.subscribe".into(),
            "test.echo".into(),
            "service.heartbeat".into()
        ]
    );
    assert_eq!(
        info.get("results").unwrap().as_array().unwrap().len(),
        1,
        "handler is available during the heartbeat acknowledgement gap"
    );
    let failed = call(&connection, "test.close", object()).await.unwrap_err();
    assert_eq!(failed.outcome, Outcome::Unknown);
    let next = client
        .wait_ready(Some(RequestOptions {
            timeout: Duration::from_secs(5),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert!(next.generation() > connection.generation());
    assert_eq!(
        call(&connection, "test.echo", object())
            .await
            .unwrap_err()
            .outcome,
        Outcome::NotExecuted
    );
    let info = call(&next, "test.info", object()).await.unwrap();
    assert!(
        !info
            .get("trace")
            .unwrap()
            .as_array()
            .unwrap()
            .contains(&"test.close".into())
    );
    client.stop().await;
    assert!(!client.ready());
    client.start();
    assert!(!client.ready());
    assert_eq!(states.lock().unwrap().last().unwrap().status, "stopped");
    peer.finish().await;
}

#[tokio::test]
async fn owner_cache_is_generation_bound_retries_unsynced_and_returns_independent_values() {
    use ivy_sdk::connection_owner::connection_owner;
    let peer = Peer::start().await;
    let connection = peer.open(options()).await;
    call(
        &connection,
        "test.ownerState",
        json(r#"{"connected":false}"#),
    )
    .await
    .unwrap();
    let id = JsString::from("fixture-node");
    let first = connection_owner(&connection, &id).await.unwrap();
    assert_eq!(first.get("reads").unwrap().as_f64(), Some(1.0));
    call(
        &connection,
        "test.ownerState",
        json(r#"{"connected":true}"#),
    )
    .await
    .unwrap();
    let (a, b) = tokio::join!(
        connection_owner(&connection, &id),
        connection_owner(&connection, &id)
    );
    let mut a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a, b);
    assert_eq!(a.get("reads").unwrap().as_f64(), Some(2.0));
    a.insert("changed", true.into());
    assert!(
        connection_owner(&connection, &id)
            .await
            .unwrap()
            .get("changed")
            .is_none()
    );
    call(
        &connection,
        "test.ownerState",
        json(r#"{"connected":false}"#),
    )
    .await
    .unwrap();
    assert_eq!(
        connection_owner(&connection, &id)
            .await
            .unwrap()
            .get("connected"),
        Some(&true.into()),
        "bound ownership is not a live readiness cache"
    );
    connection_owner(&connection, &"other-node".into())
        .await
        .unwrap();
    connection_owner(&connection, &"other-node".into())
        .await
        .unwrap();
    let info = call(&connection, "test.info", object()).await.unwrap();
    assert_eq!(
        info.get("trace")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| *v == &IvyJson::from("serviceNodes.get"))
            .count(),
        4
    );
    connection.close();
    assert!(connection_owner(&connection, &id).await.is_err());
    let next = peer.open(options()).await;
    assert_eq!(
        connection_owner(&next, &id)
            .await
            .unwrap()
            .get("reads")
            .unwrap()
            .as_f64(),
        Some(1.0)
    );
    next.close();
    peer.finish().await;
}

#[tokio::test]
async fn stop_during_reconciliation_does_not_cancel_accepted_dependency_work() {
    let peer = Peer::start().await;
    let registry = peer.registry.clone();
    let gate = Arc::new(Semaphore::new(0));
    let release = gate.clone();
    let entered = Arc::new(AtomicUsize::new(0));
    let started = entered.clone();
    let finished = Arc::new(AtomicUsize::new(0));
    let completed = finished.clone();
    let mut config = ServiceOptions::new(
        peer.url.clone(),
        Arc::new(|| Box::pin(async { Ok("fixture-token".into()) })),
        peer.identity.clone(),
        Arc::new(move || {
            let registry = registry.clone();
            Box::pin(async move { Ok(registry) })
        }),
        Arc::new(move |connection| {
            let gate = gate.clone();
            let started = started.clone();
            let completed = completed.clone();
            Box::pin(async move {
                started.fetch_add(1, Ordering::SeqCst);
                gate.acquire().await.unwrap().forget();
                assert!(connection.signal().is_cancelled());
                completed.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }),
    );
    config.handlers.insert("fixture.echo".into(), echo());
    let client = ServiceClient::new(config).unwrap();
    client.start();
    tokio::time::timeout(Duration::from_secs(3), async {
        while entered.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(1), client.stop())
        .await
        .unwrap();
    assert!(!client.ready());
    assert_eq!(finished.load(Ordering::SeqCst), 0);
    release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(1), async {
        while finished.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let next = peer.open(options()).await;
    let info = call(&next, "test.info", object()).await.unwrap();
    assert!(
        !info.get("previous").unwrap().as_array().unwrap()[0]
            .get("trace")
            .unwrap()
            .as_array()
            .unwrap()
            .contains(&"service.heartbeat".into())
    );
    next.close();
    peer.finish().await;
}

#[tokio::test]
async fn outbound_limits_reject_without_closing_and_bindings_require_exactly_one_handler() {
    let peer = Peer::start().await;
    let mut config = options();
    config.json_handlers = config.handlers.clone();
    let invalid =
        ServiceConnection::connect(&peer.url, "fixture-token", config, CancellationToken::new())
            .await
            .unwrap();
    assert_eq!(
        invalid.set_registry(&peer.registry).unwrap_err().code,
        "registry_invalid"
    );
    invalid.close();
    let connection = peer.open(options()).await;
    let large = IvyJson::String(JsString(vec![97; 33 * 1024 * 1024]));
    let error = call(&connection, "test.echo", large).await.unwrap_err();
    assert_eq!(error.code, "content_too_large");
    assert_eq!(error.outcome, Outcome::NotExecuted);
    assert!(!connection.signal().is_cancelled());
    let payload = IvyJson::String(JsString(vec![97; 18 * 1024 * 1024]));
    // Current-thread execution keeps the socket writer from draining between these sends.
    for _ in 0..3 {
        connection
            .notification(
                "fixture".into(),
                "large".into(),
                "1.0.0".into(),
                payload.clone(),
            )
            .unwrap();
    }
    let error = connection
        .notification("fixture".into(), "large".into(), "1.0.0".into(), payload)
        .unwrap_err();
    assert_eq!(error.code, "limit_exceeded");
    assert_eq!(error.message, "Service socket buffer is full.");
    assert!(!connection.signal().is_cancelled());
    connection.close();
    peer.finish().await;
}
