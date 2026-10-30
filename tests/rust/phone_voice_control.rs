use super::*;
use ivy_contracts::json::{parse, stringify};
use ivy_host_runtime::{codex_rpc::NativeRpc, node_path::Style};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream},
    sync::{Semaphore, mpsc},
};

fn value(text: &str) -> IvyJson {
    parse(text, 4 * 1024 * 1024).unwrap()
}
fn empty() -> IvyJson {
    IvyJson::object([] as [(&str, IvyJson); 0])
}
// Observe the REQUIRED compiler boundary; these connection-policy tests do not
// qualify an installed compiler or permit skipping its serialized-value bounds.
#[derive(Default)]
pub(crate) struct Schemas(Mutex<Vec<usize>>);
impl VoiceSchemas for Schemas {
    fn validate(&self, schema: &IvyJson, input: &IvyJson, max: usize) -> IvyResult<()> {
        if input.get("version").is_some() {return ivy_contracts::schema::SchemaValidators::new().validate(schema,input,max);}
        self.0.lock().unwrap().push(max);
        stringify(input, max)?;
        Ok(())
    }
}
#[derive(Default)]
struct CloseProbe {
    calls: AtomicUsize,
    work: AtomicUsize,
    result: Mutex<Option<VoiceControlOperation<()>>>,
}
pub(crate) struct Peer {
    input: BufReader<DuplexStream>,
    output: DuplexStream,
    probe: Arc<CloseProbe>,
}
impl Peer {
    pub(crate) fn into_io(self) -> (BufReader<DuplexStream>, DuplexStream) {
        (self.input, self.output)
    }
    pub(crate) async fn request(&mut self) -> IvyJson {
        let mut line = String::new();
        assert_ne!(timed(self.input.read_line(&mut line)).await.unwrap(), 0);
        value(&line)
    }
    pub(crate) async fn frame(&mut self, frame: IvyJson) {
        self.output
            .write_all(format!("{}\n", stringify(&frame, 4 * 1024 * 1024).unwrap()).as_bytes())
            .await
            .unwrap();
    }
    pub(crate) async fn reply(&mut self, request: &IvyJson, result: IvyJson) {
        self.frame(IvyJson::object([
            ("id", request.get("id").unwrap().clone()),
            ("result", result),
        ]))
        .await;
    }
}
async fn timed<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(8), future)
        .await
        .unwrap()
}
async fn until(mut ready: impl FnMut() -> bool) {
    timed(async {
        while !ready() {
            tokio::task::yield_now().await;
        }
    })
    .await
}
fn code<T>(result: IvyResult<T>) -> String {
    match result {
        Err(error) => error.code,
        Ok(_) => panic!("Expected refusal"),
    }
}
fn done() -> VoiceClose {
    Box::pin(async { Ok(()) })
}
#[derive(Default)]
pub(crate) struct Plan {
    gate: Option<Arc<Semaphore>>,
    fail: bool,
    close_error: bool,
    home: Option<JsString>,
    catalog: Option<IvyJson>,
}
impl Plan {
    pub(crate) fn with_catalog(catalog: IvyJson) -> Self {
        Self {
            catalog: Some(catalog),
            ..Self::default()
        }
    }
}
pub(crate) struct Connector {
    plans: Mutex<VecDeque<Plan>>,
    options: Mutex<Vec<VoiceConnectOptions>>,
    peers: mpsc::UnboundedSender<Peer>,
}
impl VoiceConnector for Connector {
    fn connect(&self, options: VoiceConnectOptions, events: Events) -> VoiceConnect {
        self.options.lock().unwrap().push(options);
        let plan = self
            .plans
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected connection attempt");
        let peers = self.peers.clone();
        Box::pin(async move {
            if let Some(gate) = plan.gate {
                gate.acquire_owned().await.unwrap().forget();
            }
            if plan.fail {
                return Err(IvyError::new(
                    "fixture_connect_failed",
                    "Scripted connect failure.",
                ));
            }
            let (input, sent) = tokio::io::duplex(65536);
            let (emit, output) = tokio::io::duplex(65536);
            let catalog = plan.catalog.unwrap_or_else(|| {
                value(include_str!(
                    "../../specs/native/codex-0.154.0/catalog.json"
                ))
            });
            let rpc = NativeRpc::start(Arc::new(catalog), input, output, events)?;
            let probe = Arc::new(CloseProbe::default());
            peers
                .send(Peer {
                    input: BufReader::new(sent),
                    output: emit,
                    probe: probe.clone(),
                })
                .unwrap_or_else(|_| panic!("Peer owner dropped"));
            let native = rpc.clone();
            Ok(VoiceConnection {
                rpc,
                home: plan.home.unwrap_or_else(|| "C:\\codex".into()),
                close: Arc::new(move || {
                    probe.calls.fetch_add(1, Ordering::SeqCst);
                    let mut saved = probe.result.lock().unwrap();
                    let operation = saved
                        .get_or_insert_with(|| {
                            probe.work.fetch_add(1, Ordering::SeqCst);
                            let (send, receive) = watch::channel(None);
                            let native = native.clone();
                            tokio::spawn(async move {
                                native.close("fixture_closed").await;
                                send.send_replace(Some(if plan.close_error {
                                    Err(IvyError::new(
                                        "fixture_cleanup_failed",
                                        "Scripted close failure.",
                                    ))
                                } else {
                                    Ok(())
                                }));
                            });
                            VoiceControlOperation(receive)
                        })
                        .clone();
                    Box::pin(async move { operation.wait().await })
                }),
            })
        })
    }
}
fn settings(native: IvyJson, style: Style, cwd: JsString) -> VoiceControlSettings {
    VoiceControlSettings {
        native,
        artifact_root: "artifacts".into(),
        data_root: "data/./owned".into(),
        credential: None,
        paths: Context { style, cwd },
    }
}
pub(crate) fn hooks() -> VoiceControlHooks {
    VoiceControlHooks {
        events: Events::default(),
        on_open: Arc::new(|| {}),
        on_error: Arc::new(|_| {}),
    }
}
pub(crate) fn fixture(
    plans: Vec<Plan>,
    native: IvyJson,
    hooks: VoiceControlHooks,
) -> (
    VoiceControl,
    Arc<Connector>,
    mpsc::UnboundedReceiver<Peer>,
    Arc<Schemas>,
) {
    let (connector, receive, schemas) = connector_fixture(plans);
    let control = VoiceControl::new(
        settings(native, Style::Windows, "C:\\owner".into()),
        connector.clone(),
        schemas.clone(),
        hooks,
    )
    .unwrap();
    (control, connector, receive, schemas)
}
pub(crate) fn connector_fixture(
    plans: Vec<Plan>,
) -> (Arc<Connector>, mpsc::UnboundedReceiver<Peer>, Arc<Schemas>) {
    let (send, receive) = mpsc::unbounded_channel();
    let connector = Arc::new(Connector {
        plans: Mutex::new(plans.into()),
        options: Mutex::new(vec![]),
        peers: send,
    });
    let schemas = Arc::new(Schemas::default());
    (connector, receive, schemas)
}
fn options_json(options: &VoiceConnectOptions) -> IvyJson {
    let mut fields = vec![
        ("settings", options.settings.clone()),
        (
            "artifactRoot",
            IvyJson::String(options.artifact_root.clone()),
        ),
        ("dataRoot", IvyJson::String(options.data_root.clone())),
        ("clientName", options.client_name.into()),
        ("clientVersion", options.client_version.into()),
    ];
    if let Some(credential) = &options.credential {
        fields.push(("hiveCredential", IvyJson::String(credential.clone())));
    }
    IvyJson::object(fields)
}

#[tokio::test]
async fn configuration_open_project_and_fingerprint_are_retained() {
    let cases = value(
        r#"[
      {"style":"windows","cwd":"C:\\owner","settings":{"cwd":"C:\\work\\a\\..\\b"},"home":"C:\\codex","runtime":{"artifactRoot":"artifacts","dataRoot":"data/./owned"}},
      {"style":"windows","cwd":"D:\\owner","settings":{"cwd":"\\work","projectId":"p","config":{"z":2,"a":{"text":"\ud800"}}},"home":"D:\\home","runtime":{"artifactRoot":"artifacts","dataRoot":"data/./owned","credential":"test-token"},"projectReply":{"project":{"id":"p"}}},
      {"style":"posix","cwd":"/owner","settings":{"cwd":"/work//a/../b","config":null,"projectId":null},"home":"/home/codex","runtime":{"artifactRoot":"artifacts","dataRoot":"data/./owned","credential":""}},
      {"style":"windows","cwd":"C:\\owner","settings":{"cwd":"\\\\server\\share\\x\\..\\","projectId":""},"home":"C:\\home","runtime":{"artifactRoot":"artifacts","dataRoot":"data/./owned"}},
      {"style":"windows","cwd":"C:\\owner","settings":{"cwd":"C:\\work\\\ud800","projectId":"p"},"home":"C:\\home","runtime":{"artifactRoot":"artifacts","dataRoot":"data/./owned"},"projectReply":{"project":{"id":"wrong"}}},
      {"style":"windows","cwd":"C:\\owner","settings":{"cwd":"relative"},"home":"C:\\home","runtime":{"artifactRoot":"artifacts","dataRoot":"data/./owned"}},
      {"style":"posix","cwd":"/owner","settings":{"cwd":"C:\\work"},"home":"/home","runtime":{"artifactRoot":"artifacts","dataRoot":"data/./owned"}},
      {"style":"windows","cwd":"C:\\owner","settings":{"cwd":"C:work"},"home":"C:\\home","runtime":{"artifactRoot":"artifacts","dataRoot":"data/./owned"}},
      {"style":"windows","cwd":"C:\\owner","settings":{"cwd":"C:\\work"},"home":"C:\\home","runtime":{"artifactRoot":"artifacts","dataRoot":"data/./owned"},"fail":true}
    ]"#,
    );
    let expected = value(include_str!("../fixtures/phone-voice-control-results.json"));
    let mut actual = vec![];
    let mut selected_expected = vec![];
    for (item, expected) in cases
        .as_array()
        .unwrap()
        .iter()
        .zip(expected.as_array().unwrap())
    {
        let windows = item.get("style") == Some(&"windows".into());
        if windows != cfg!(windows) {
            continue;
        }
        selected_expected.push(expected.clone());
        let (send, mut peers) = mpsc::unbounded_channel();
        let connector = Arc::new(Connector {
            plans: Mutex::new(
                vec![Plan {
                    fail: item.get("fail") == Some(&true.into()),
                    home: item.get("home").and_then(IvyJson::as_js_string).cloned(),
                    ..Plan::default()
                }]
                .into(),
            ),
            options: Mutex::new(vec![]),
            peers: send,
        });
        let mut config = settings(
            item.get("settings").unwrap().clone(),
            if item.get("style") == Some(&"windows".into()) {
                Style::Windows
            } else {
                Style::Posix
            },
            item.get("cwd")
                .and_then(IvyJson::as_js_string)
                .unwrap()
                .clone(),
        );
        config.credential = item
            .get("runtime")
            .unwrap()
            .get("credential")
            .and_then(IvyJson::as_js_string)
            .cloned();
        let control = VoiceControl::new(
            config,
            connector.clone(),
            Arc::new(Schemas::default()),
            hooks(),
        );
        let mut requests = vec![];
        let (result, fingerprint) = match control {
            Err(error) => (Err(error), IvyJson::Null),
            Ok(control) => {
                let open = control.open();
                if !expected
                    .get("requests")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .is_empty()
                {
                    let mut peer = timed(peers.recv()).await.unwrap();
                    let request = peer.request().await;
                    requests.push(IvyJson::object([
                        ("method", request.get("method").unwrap().clone()),
                        ("params", request.get("params").unwrap().clone()),
                    ]));
                    peer.reply(&request, item.get("projectReply").unwrap().clone())
                        .await;
                    // Retain the peer until the opening result/close have drained.
                    let result = timed(open.wait()).await.map(|_| ());
                    let fingerprint = control
                        .fingerprint()
                        .map(IvyJson::from)
                        .unwrap_or(IvyJson::Null);
                    timed(control.close(done()).wait()).await.unwrap();
                    (result, fingerprint)
                } else {
                    let result = timed(open.wait()).await.map(|_| ());
                    let fingerprint = control
                        .fingerprint()
                        .map(IvyJson::from)
                        .unwrap_or(IvyJson::Null);
                    timed(control.close(done()).wait()).await.unwrap();
                    (result, fingerprint)
                }
            }
        };
        let mut fields = vec![
            ("ok", result.is_ok().into()),
            ("fingerprint", fingerprint),
            (
                "options",
                IvyJson::Array(
                    connector
                        .options
                        .lock()
                        .unwrap()
                        .iter()
                        .map(options_json)
                        .collect(),
                ),
            ),
            ("requests", IvyJson::Array(requests)),
        ];
        if let Err(error) = result {
            fields.push(("code", IvyJson::String(error.code_js())));
            fields.push(("message", IvyJson::String(error.message_js())));
        }
        actual.push(IvyJson::object(fields));
    }
    let actual = IvyJson::Array(actual);
    let expected = IvyJson::Array(selected_expected);
    // These are observation records, not wire-order comparisons. Error records
    // insert diagnostic keys at different positions in the two test harnesses.
    assert_eq!(
        ivy_contracts::json::canonical(&actual, 4 * 1024 * 1024).unwrap(),
        ivy_contracts::json::canonical(&expected, 4 * 1024 * 1024).unwrap()
    );
}

#[tokio::test]
async fn abandoned_open_and_catalogs_share_the_configured_connection() {
    let gate = Arc::new(Semaphore::new(0));
    let (control, connector, mut peers, schemas) = fixture(
        vec![Plan {
            gate: Some(gate.clone()),
            ..Plan::default()
        }],
        value(r#"{"cwd":"C:\\work","voice":"ember"}"#),
        hooks(),
    );
    drop(control.open());
    let observer = control.open();
    until(|| connector.options.lock().unwrap().len() == 1).await;
    gate.add_permits(1);
    let mut peer = timed(peers.recv()).await.unwrap();
    let opened = timed(observer.wait()).await.unwrap();
    assert!(opened.same_connection(&control.current().unwrap()));
    let models = {
        let control = control.clone();
        tokio::spawn(async move { control.models().await })
    };
    let speakers = {
        let control = control.clone();
        tokio::spawn(async move { control.speaker_voices().await })
    };
    let mut methods = vec![];
    for _ in 0..3 {
        let request = peer.request().await;
        let method = request.get("method").unwrap().as_str().unwrap();
        methods.push(method.to_owned());
        peer.reply(
            &request,
            match method.as_str() {
                "model/list" => value(r#"{"data":[],"nextCursor":null}"#),
                "thread/realtime/listVoices" => {
                    value(r#"{"voices":{"v1":["cove","ember"],"defaultV1":"cove"}}"#)
                }
                "config/read" => value(r#"{"config":{}}"#),
                other => panic!("Unexpected task mutation {other}"),
            },
        )
        .await;
    }
    assert_eq!(
        timed(models).await.unwrap().unwrap(),
        IvyJson::Array(vec![])
    );
    assert_eq!(
        timed(speakers)
            .await
            .unwrap()
            .unwrap()
            .get("defaultVoiceId"),
        Some(&"ember".into())
    );
    assert_eq!(control.models().await.unwrap(), IvyJson::Array(vec![]));
    assert_eq!(
        control
            .speaker_voices()
            .await
            .unwrap()
            .get("defaultVoiceId"),
        Some(&"ember".into())
    );
    assert_eq!(connector.options.lock().unwrap().len(), 1);
    assert_eq!(
        schemas
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|n| **n == 6 * 1024 * 1024)
            .count(),
        3
    );
    assert!(methods.contains(&"config/read".to_owned()));
    timed(control.close(done()).wait()).await.unwrap();
    assert_eq!(peer.probe.work.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn project_read_publishes_before_validation_and_retains_cleanup_authority() {
    let (control, connector, mut peers, _) = fixture(
        vec![Plan::default()],
        value(r#"{"cwd":"C:\\work","projectId":"p"}"#),
        hooks(),
    );
    let opening = control.open();
    let mut peer = timed(peers.recv()).await.unwrap();
    let request = peer.request().await;
    assert_eq!(request.get("method"), Some(&"project/read".into()));
    let current = timed(control.open().wait()).await.unwrap();
    assert!(current.same_connection(&control.current().unwrap()));
    peer.reply(&request, value(r#"{"project":{"id":"wrong"}}"#))
        .await;
    assert_eq!(
        code(timed(opening.wait()).await),
        "phone_voice_project_missing"
    );
    // Preserve the original connected fast path even after failed project/read.
    assert!(
        timed(control.open().wait())
            .await
            .unwrap()
            .same_connection(&current)
    );
    assert_eq!(connector.options.lock().unwrap().len(), 1);
    timed(control.close(done()).wait()).await.unwrap();
    assert_eq!(peer.probe.work.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn close_retains_initialization_and_drain_after_observers_are_dropped() {
    let gate = Arc::new(Semaphore::new(0));
    let (control, connector, mut peers, _) = fixture(
        vec![Plan {
            gate: Some(gate.clone()),
            ..Plan::default()
        }],
        value(r#"{"cwd":"C:\\work"}"#),
        hooks(),
    );
    let opening = control.open();
    until(|| connector.options.lock().unwrap().len() == 1).await;
    let drain = Arc::new(Semaphore::new(0));
    let entered = Arc::new(AtomicUsize::new(0));
    let wait = drain.clone();
    let count = entered.clone();
    drop(control.close(Box::pin(async move {
        count.fetch_add(1, Ordering::SeqCst);
        wait.acquire_owned().await.unwrap().forget();
        Ok(())
    })));
    assert!(control.is_stopping());
    assert_eq!(entered.load(Ordering::SeqCst), 0);
    assert_eq!(code(control.open().wait().await), "service_not_ready");
    gate.add_permits(1);
    let peer = timed(peers.recv()).await.unwrap();
    assert_eq!(code(timed(opening.wait()).await), "service_not_ready");
    until(|| entered.load(Ordering::SeqCst) == 1).await;
    assert_eq!(peer.probe.work.load(Ordering::SeqCst), 1);
    drain.add_permits(1);
    // A later observer joins the original drain; it cannot replace it.
    timed(
        control
            .close(Box::pin(async { panic!("Duplicate drain") }))
            .wait(),
    )
    .await
    .unwrap();
    assert_eq!(peer.probe.calls.load(Ordering::SeqCst), 2);
    assert_eq!(peer.probe.work.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cleanup_reconnect_during_drain_is_the_final_connection_closed() {
    let (control, connector, mut peers, _) = fixture(
        vec![Plan::default(), Plan::default()],
        value(r#"{"cwd":"C:\\work"}"#),
        hooks(),
    );
    let first = timed(control.open().wait()).await.unwrap();
    let first_peer = timed(peers.recv()).await.unwrap();
    first.transport().close("fixture_lost").await;
    let active = control.clone();
    let final_close = control.close(Box::pin(async move {
        let cleanup = active.open_cleanup().wait().await?;
        assert!(cleanup.transport().connected());
        // Ordinary selection remains closed while session cleanup reconnects.
        assert_eq!(code(active.models().await), "service_not_ready");
        Ok(())
    }));
    let second_peer = timed(peers.recv()).await.unwrap();
    timed(final_close.wait()).await.unwrap();
    assert_eq!(first_peer.probe.calls.load(Ordering::SeqCst), 0);
    assert_eq!(second_peer.probe.work.load(Ordering::SeqCst), 1);
    assert_eq!(connector.options.lock().unwrap().len(), 2);
    assert!(!first.same_connection(&control.current().unwrap()));
}

#[tokio::test]
async fn reconnect_replaces_identity_and_catalog_cache_without_preparing_a_task() {
    let (control, connector, mut peers, _) = fixture(
        vec![Plan::default(), Plan::default()],
        value(r#"{"cwd":"C:\\work"}"#),
        hooks(),
    );
    let first = timed(control.open().wait()).await.unwrap();
    let mut peer = timed(peers.recv()).await.unwrap();
    for pass in 0..2 {
        let request = {
            let control = control.clone();
            tokio::spawn(async move { control.models().await })
        };
        let frame = peer.request().await;
        assert_eq!(frame.get("method"), Some(&"model/list".into()));
        peer.reply(&frame, value(r#"{"data":[],"nextCursor":null}"#))
            .await;
        timed(request).await.unwrap().unwrap();
        if pass == 0 {
            first.transport().close("fixture_lost").await;
            let second = timed(control.open().wait()).await.unwrap();
            assert!(!first.same_connection(&second));
            peer = timed(peers.recv()).await.unwrap();
        }
    }
    assert_eq!(connector.options.lock().unwrap().len(), 2);
    timed(control.close(done()).wait()).await.unwrap();
}

#[tokio::test]
async fn original_native_callbacks_and_unexpected_loss_are_forwarded_once() {
    let observed = Arc::new(Mutex::new(vec![]));
    let mut callbacks = hooks();
    let seen = observed.clone();
    callbacks.events.on_request = Arc::new(move |request| {
        let seen = seen.clone();
        Box::pin(async move {
            seen.lock()
                .unwrap()
                .push(format!("request:{}", request.method.to_utf8().unwrap()));
            Ok(())
        })
    });
    let seen = observed.clone();
    callbacks.events.on_notification = Arc::new(move |event| {
        let seen = seen.clone();
        Box::pin(async move {
            seen.lock()
                .unwrap()
                .push(format!("notification:{}", event.method.to_utf8().unwrap()));
            Ok(())
        })
    });
    let seen = observed.clone();
    callbacks.events.on_close = Arc::new(move |code| {
        let seen = seen.clone();
        Box::pin(async move {
            seen.lock().unwrap().push(format!("close:{code}"));
            Ok(())
        })
    });
    let seen = observed.clone();
    callbacks.on_error = Arc::new(move |code| {
        seen.lock().unwrap().push(format!("error:{code}"));
    });
    let seen = observed.clone();
    callbacks.on_open = Arc::new(move || {
        seen.lock().unwrap().push("open".to_owned());
    });
    let (control, _, mut peers, _) = fixture(
        vec![Plan::default(), Plan::default()],
        value(r#"{"cwd":"C:\\work"}"#),
        callbacks,
    );
    let first = timed(control.open().wait()).await.unwrap();
    let mut peer = timed(peers.recv()).await.unwrap();
    let catalog = first.transport().catalog();
    let method = catalog.get("serverRequests").unwrap().as_array().unwrap()[0]
        .get("method")
        .unwrap()
        .clone();
    let notification = catalog
        .get("serverNotifications")
        .unwrap()
        .as_array()
        .unwrap()[0]
        .get("method")
        .unwrap()
        .clone();
    peer.frame(IvyJson::object([
        ("id", 71.0.into()),
        ("method", method),
        ("params", empty()),
    ]))
    .await;
    peer.frame(IvyJson::object([
        ("method", notification),
        ("params", empty()),
    ]))
    .await;
    until(|| observed.lock().unwrap().len() >= 3).await;
    first.transport().close("fixture_lost").await;
    until(|| {
        observed
            .lock()
            .unwrap()
            .iter()
            .any(|s| s == "error:fixture_lost")
    })
    .await;
    timed(control.open().wait()).await.unwrap();
    let _second = timed(peers.recv()).await.unwrap();
    timed(control.close(done()).wait()).await.unwrap();
    let observed = observed.lock().unwrap();
    assert_eq!(
        observed
            .iter()
            .filter(|s| s.starts_with("request:"))
            .count(),
        1
    );
    assert_eq!(
        observed
            .iter()
            .filter(|s| s.starts_with("notification:"))
            .count(),
        1
    );
    assert_eq!(
        observed.iter().filter(|s| s.starts_with("error:")).count(),
        1
    );
    assert!(observed.iter().any(|s| s == "close:fixture_closed"));
}

#[tokio::test]
async fn failed_open_can_retry_and_cleanup_error_remains_the_sticky_close_result() {
    let (control, connector, mut peers, _) = fixture(
        vec![
            Plan {
                fail: true,
                ..Plan::default()
            },
            Plan {
                close_error: true,
                ..Plan::default()
            },
        ],
        value(r#"{"cwd":"C:\\work"}"#),
        hooks(),
    );
    assert_eq!(
        code(timed(control.open().wait()).await),
        "fixture_connect_failed"
    );
    timed(control.open().wait()).await.unwrap();
    let peer = timed(peers.recv()).await.unwrap();
    let first = control.close(Box::pin(async {
        Err(IvyError::new(
            "fixture_drain_failed",
            "Scripted drain failure.",
        ))
    }));
    assert_eq!(code(timed(first.wait()).await), "fixture_cleanup_failed");
    assert_eq!(
        code(
            control
                .close(Box::pin(async { panic!("Duplicate drain") }))
                .wait()
                .await
        ),
        "fixture_cleanup_failed"
    );
    assert_eq!(peer.probe.calls.load(Ordering::SeqCst), 1);
    assert_eq!(connector.options.lock().unwrap().len(), 2);
}
