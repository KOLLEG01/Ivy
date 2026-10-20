use super::*;
use ivy_phone_bridge::{
    native_pipe::{NativeRuntime, NativeShutdown, NativeShutdownError},
    native_rpc::{NativeDispatcher, NativeHandler, NativeInvocation, NativeOutcome},
    native_sip::{NativeSipCall, NativeSipError},
    native_sip_commands::NativeSipCommands,
    sip::SipHostCallPhase,
};
use std::{future::Future, task::Poll};

// A finite caller-owned runtime; it never opens a platform device or supplies
// a production status/feature implementation. Tests exercise only composition.
#[derive(Default)]
struct Remaining {
    methods: Mutex<Vec<(String, IvyJson)>>,
    closes: AtomicUsize,
    close_gate: Arc<Gate>,
    close_mode: AtomicUsize,
}
impl NativeHandler for Remaining {
    fn cleanup_slots(&self) -> usize {
        0
    }
    fn invoke(&self, request: NativeRequest) -> NativeInvocation {
        self.methods
            .lock()
            .unwrap()
            .push((request.method().into(), request.params().clone()));
        Box::pin(async { NativeOutcome::result("{\"callerOwned\":true}".into()) })
    }
}
impl NativeRuntime for Remaining {
    fn shutdown(&self) -> NativeShutdown {
        self.closes.fetch_add(1, Ordering::SeqCst);
        let gate = self.close_gate.clone();
        let mode = self.close_mode.load(Ordering::SeqCst);
        if mode == 2 {
            panic!("synthetic caller shutdown constructor panic");
        }
        Box::pin(async move {
            gate.block().await;
            match mode {
                0 => Ok(()),
                3 => panic!("synthetic caller shutdown future panic"),
                _ => Err(NativeShutdownError::OutcomeUnknown),
            }
        })
    }
}
struct CountedFactory {
    inner: NativeSipSessionFactory,
    starts: AtomicUsize,
}
impl NativeSipMediaFactory for CountedFactory {
    fn create(
        &self,
        id: String,
        configuration: Arc<NativeCallConfiguration>,
    ) -> NativeSipMediaFuture {
        self.starts.fetch_add(1, Ordering::SeqCst);
        self.inner.create(id, configuration)
    }
}
fn actual_owner(
    first: u16,
    last: u16,
) -> (
    NativeSipOperations,
    Arc<CountedFactory>,
    Arc<Factory>,
    String,
) {
    let routes = Arc::new(Factory::routes(vec![]));
    let factory = Arc::new(CountedFactory {
        inner: NativeSipSessionFactory::new(routes.clone(), None, Arc::new(Clock), |_| {
            Arc::new(Events::default())
        }),
        starts: AtomicUsize::new(0),
    });
    let params = BASE
        .replace("57000", &first.to_string())
        .replace("57998", &last.to_string());
    (
        NativeSipOperations::new(factory.clone()),
        factory,
        routes,
        params,
    )
}
fn command(method: &str, params: &str, operation: u64, request: u64) -> NativeRequest {
    let id = format!("00000000-0000-4000-8000-{operation:012x}");
    NativeRequest::parse(format!(r#"{{"version":1,"epoch":"{CALL}","requestId":{request},"operationId":{},"method":"{method}","params":{params}}}"#,
        if method == "status" { "null".into() } else { format!("\"{id}\"") }).as_bytes()).unwrap()
}
fn subset(call: &NativeSipCall) -> Result<IvyJson, NativeSipError> {
    let state = call.observation();
    let phase = match state.phase {
        SipHostCallPhase::Prepared => "prepared",
        SipHostCallPhase::Dialing => "dialing",
        SipHostCallPhase::Connected => "connected",
        SipHostCallPhase::LocalEnded => "local_ended",
        SipHostCallPhase::OutcomeUnknown => "outcome_unknown",
    };
    Ok(IvyJson::object([
        ("id", state.id.into()),
        ("state", phase.into()),
    ]))
}
async fn cancel<F: Future>(work: F) {
    let mut work = Box::pin(work);
    std::future::poll_fn(|cx| {
        assert!(work.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(work);
}
fn value(outcome: &NativeOutcome) -> IvyJson {
    parse(
        outcome
            .result_json()
            .unwrap_or_else(|| panic!("{}", outcome.error_code().unwrap())),
        65536,
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concrete_native_lifecycle_receipts_survive_observer_loss_and_id_reuse() {
    let (owner, factory, routes, params) = actual_owner(45000, 45998);
    let setup = Arc::new(Gate::default());
    let original_setup = setup.clone();
    let setups = Arc::new(AtomicUsize::new(0));
    let count = setups.clone();
    let remaining = Arc::new(Remaining::default());
    let handler = Arc::new(NativeSipCommands::new(
        owner.clone(),
        remaining.clone(),
        move |micro| {
            assert!(micro.is_none());
            count.fetch_add(1, Ordering::SeqCst);
            let gate = original_setup.clone();
            Box::pin(async move {
                gate.block().await;
                Ok(())
            })
        },
        subset,
    ));
    let dispatcher = NativeDispatcher::new(CALL.into(), 32, handler.clone()).unwrap();
    cancel(dispatcher.execute(command("configure", &params, 1, 1))).await;
    until(|| setup.entered.load(Ordering::SeqCst)).await;
    let selected = format!(r#"{{"callId":"{CALL}"}}"#);
    let early = dispatcher
        .execute(command("call.prepare", &selected, 2, 2))
        .await;
    assert_eq!(early.outcome().error_code(), Some("runtime_not_ready"));
    assert_eq!(factory.starts.load(Ordering::SeqCst), 0);
    assert!(
        timeout(
            Duration::from_millis(10),
            dispatcher.execute(command("configure", &params, 1, 3))
        )
        .await
        .is_err()
    );
    setup.release();
    let configured = dispatcher
        .execute(command("configure", &params, 1, 4))
        .await;
    let endpoint = value(configured.outcome())
        .get("endpoint")
        .unwrap()
        .as_str()
        .unwrap()
        .parse::<SocketAddr>()
        .unwrap();
    assert_eq!(setups.load(Ordering::SeqCst), 1);
    assert_eq!(
        dispatcher
            .execute(command("call.prepare", &selected, 2, 5))
            .await
            .outcome()
            .error_code(),
        Some("runtime_not_ready")
    );
    cancel(dispatcher.execute(command("call.prepare", &selected, 3, 6))).await;
    let prepared = dispatcher
        .execute(command("call.prepare", &selected, 3, 7))
        .await;
    assert_eq!(prepared.request_id(), 7);
    assert_eq!(
        value(prepared.outcome())
            .get("state")
            .unwrap()
            .as_str()
            .unwrap(),
        "prepared"
    );
    let original = owner.find(CALL).unwrap();
    let media = original.media().unwrap();
    let transport = media.observation().receive.transport;
    let offer = media.create_offer().unwrap().to_sdp();
    assert_eq!(factory.starts.load(Ordering::SeqCst), 1);
    assert_eq!(handler.cleanup_slots(), 4);
    let conflict = dispatcher
        .execute(command("call.prepare", "{\"callId\":null}", 3, 8))
        .await;
    assert_eq!(conflict.outcome().error_code(), Some("operation_conflict"));
    assert_eq!(media.create_offer().unwrap().to_sdp(), offer);
    let forwarded = dispatcher
        .execute(command("status", "{\"caller\":42}", 0, 9))
        .await;
    assert_eq!(
        value(forwarded.outcome())
            .get("callerOwned")
            .unwrap()
            .as_bool(),
        Some(true)
    );
    assert_eq!(
        *remaining.methods.lock().unwrap(),
        vec![("status".into(), parse("{\"caller\":42}", 65536).unwrap())]
    );
    let hung_up = dispatcher
        .execute(command("call.hangup", &selected, 4, 10))
        .await;
    assert_eq!(
        value(hung_up.outcome())
            .get("state")
            .unwrap()
            .as_str()
            .unwrap(),
        "local_ended"
    );
    cancel(dispatcher.execute(command("call.release", &selected, 5, 11))).await;
    let released = dispatcher
        .execute(command("call.release", &selected, 5, 12))
        .await;
    assert_eq!(
        value(released.outcome()),
        IvyJson::object([("callId", CALL.into()), ("released", true.into())])
    );
    assert!(owner.find(CALL).is_none());
    drop(std::net::UdpSocket::bind(transport.local_rtp).unwrap());
    drop(std::net::UdpSocket::bind(transport.local_rtcp).unwrap());
    dispatcher
        .execute(command("call.prepare", &selected, 6, 13))
        .await;
    let replacement = owner.find(CALL).unwrap();
    assert!(!replacement.media().unwrap().observation().closed);
    dispatcher
        .execute(command("call.release", &selected, 5, 14))
        .await;
    assert!(!replacement.media().unwrap().observation().closed);
    assert_eq!(factory.starts.load(Ordering::SeqCst), 2);
    let shutdown = handler.shutdown();
    assert!(owner.observation().stopping);
    drop(shutdown);
    until(|| remaining.close_gate.entered.load(Ordering::SeqCst)).await;
    assert!(
        timeout(Duration::from_millis(10), handler.shutdown())
            .await
            .is_err()
    );
    remaining.close_gate.release();
    handler.shutdown().await.unwrap();
    handler.shutdown().await.unwrap();
    assert_eq!(remaining.closes.load(Ordering::SeqCst), 1);
    assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
    drop(std::net::UdpSocket::bind(endpoint).unwrap());
    trace(
        "native-lifecycle-receipts",
        IvyJson::object([
            ("setupInvocations", n(1)),
            ("factoryInvocations", n(2)),
            ("retainedNegativeReceipt", true.into()),
            ("originalIdentityOnReplay", true.into()),
            ("oldReleaseCannotCloseReplacement", true.into()),
            ("callerShutdownInvocations", n(1)),
            ("physicalAudioOpens", n(0)),
        ]),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_command_shutdown_finally_drains_caller_after_sip_failure_and_retains_uncertainty() {
    let mut rows = Vec::new();
    for mode in 0..=3 {
        let (owner, _, _, params) = actual_owner(46000, 46998);
        let remaining = Arc::new(Remaining::default());
        remaining.close_mode.store(mode, Ordering::SeqCst);
        remaining.close_gate.release();
        let handler = NativeSipCommands::new(
            owner.clone(),
            remaining.clone(),
            |_| Box::pin(async { Err(SipHostError::CleanupUnconfirmed) }),
            subset,
        );
        let result = handler.invoke(command("configure", &params, 1, 1)).await;
        assert_eq!(result.error_code(), Some("operation_outcome_unknown"));
        let endpoint = owner.observation().endpoint.unwrap();
        let stop = handler.shutdown();
        drop(stop);
        assert_eq!(
            handler.shutdown().await,
            Err(NativeShutdownError::OutcomeUnknown)
        );
        assert_eq!(
            handler.shutdown().await,
            Err(NativeShutdownError::OutcomeUnknown)
        );
        assert_eq!(remaining.closes.load(Ordering::SeqCst), 1);
        assert!(
            std::net::UdpSocket::bind(endpoint).is_err(),
            "unconfirmed SIP setup retains its fence"
        );
        assert_eq!(
            handler
                .invoke(command("call.prepare", "{}", 2, 2))
                .await
                .error_code(),
            Some("runtime_stopping")
        );
        rows.push(IvyJson::object([
            ("callerMode", n(mode)),
            ("oneCallerCleanup", true.into()),
            ("sipFailureRetained", true.into()),
        ]));
    }
    trace("native-lifecycle-shutdown", IvyJson::Array(rows));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_projection_failures_keep_original_effects_and_safe_receipts() {
    let mut rows = Vec::new();
    for mode in 0..=3 {
        let (owner, factory, _, params) = actual_owner(47000, 47998);
        let remaining = Arc::new(Remaining::default());
        remaining.close_gate.release();
        let views = Arc::new(AtomicUsize::new(0));
        let count = views.clone();
        let handler = Arc::new(NativeSipCommands::new(
            owner.clone(),
            remaining,
            |_| Box::pin(async { Ok(()) }),
            move |_| {
                count.fetch_add(1, Ordering::SeqCst);
                match mode {
                    0 => Err(SipHostError::NotReady.into()),
                    1 => Ok("x".repeat(65536).into()),
                    2 => panic!("synthetic projection panic"),
                    _ => {
                        let mut value = IvyJson::Null;
                        for _ in 0..25 {
                            value = IvyJson::Array(vec![value]);
                        }
                        Ok(value)
                    }
                }
            },
        ));
        let dispatcher = NativeDispatcher::new(CALL.into(), 16, handler.clone()).unwrap();
        assert!(
            dispatcher
                .execute(command("configure", &params, 1, 1))
                .await
                .outcome()
                .error_code()
                .is_none()
        );
        let selected = format!(r#"{{"callId":"{CALL}"}}"#);
        let code = match mode {
            0 => "runtime_not_ready",
            3 => "invalid_request",
            _ => "operation_outcome_unknown",
        };
        for request in [2, 3] {
            assert_eq!(
                dispatcher
                    .execute(command("call.prepare", &selected, 2, request))
                    .await
                    .outcome()
                    .error_code(),
                Some(code)
            );
        }
        assert!(owner.find(CALL).is_some());
        assert_eq!(views.load(Ordering::SeqCst), 1);
        assert_eq!(factory.starts.load(Ordering::SeqCst), 1);
        dispatcher
            .execute(command("call.release", &selected, 3, 4))
            .await;
        assert!(owner.find(CALL).is_none());
        handler.shutdown().await.unwrap();
        rows.push(IvyJson::object([
            ("mode", n(mode)),
            ("error", code.into()),
            ("effectRetained", true.into()),
            ("projectionInvocations", n(1)),
        ]));
    }
    trace("native-lifecycle-projection", IvyJson::Array(rows));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_lifecycle_dto_and_error_order_match_original_handler_subset() {
    let (owner, factory, routes, params) = actual_owner(48000, 48998);
    let remaining = Arc::new(Remaining::default());
    remaining.close_gate.release();
    let handler = Arc::new(NativeSipCommands::new(
        owner.clone(),
        remaining,
        |_| Box::pin(async { Ok(()) }),
        subset,
    ));
    let selected = format!(r#"{{"callId":"{CALL}"}}"#);
    let steps: Vec<(&str, String)> = vec![
        ("call.prepare", "{}".into()),
        ("call.hangup", "{\"callId\":42}".into()),
        ("configure", "{}".into()),
        ("configure", params.clone()),
        ("configure", "{}".into()),
        ("configure", "{\"callId\":42}".into()),
        ("call.prepare", "{}".into()),
        ("call.prepare", "{\"callId\":null}".into()),
        ("call.prepare", "{\"callId\":42}".into()),
        ("call.prepare", "{\"callId\":\"bad\"}".into()),
        ("call.hangup", "{\"callId\":null}".into()),
        ("call.release", "{\"callId\":\"bad\"}".into()),
        ("call.prepare", selected.clone()),
        ("call.prepare", selected.clone()),
        ("call.hangup", selected.replace('}', ",\"extra\":null}")),
        ("call.hangup", selected.clone()),
        ("call.release", selected.clone()),
        ("call.release", selected.clone()),
    ];
    let inputs: Vec<_> = steps
        .iter()
        .map(|(method, params)| {
            IvyJson::object([
                ("method", (*method).into()),
                ("params", parse(params, 65536).unwrap()),
            ])
        })
        .collect();
    trace(
        "native-lifecycle-oracle-inputs",
        IvyJson::Array(inputs.clone()),
    );
    let original =
        original_session_fixture("tests/fixtures/phone-native-lifecycle-oracle.ps1", &inputs);
    let dispatcher = NativeDispatcher::new(CALL.into(), 64, handler.clone()).unwrap();
    let mut rows = Vec::new();
    for (index, (method, params)) in steps.iter().enumerate() {
        let reply = dispatcher
            .execute(command(method, params, index as u64 + 1, index as u64 + 1))
            .await;
        let row = if let Some(code) = reply.outcome().error_code() {
            IvyJson::object([("error", code.into())])
        } else if *method == "configure" {
            let v = value(reply.outcome());
            assert_eq!(v.get("configured").unwrap().as_bool(), Some(true));
            assert!(
                v.get("endpoint")
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .parse::<SocketAddr>()
                    .unwrap()
                    .ip()
                    .is_loopback()
            );
            IvyJson::object([("configured", true.into())])
        } else {
            value(reply.outcome())
        };
        if let Some(original) = &original {
            assert_eq!(
                &row,
                &original.as_array().unwrap()[index],
                "step {index}: {method} {params}"
            );
        }
        rows.push(row);
    }
    assert_eq!(factory.starts.load(Ordering::SeqCst), 1);
    assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
    handler.shutdown().await.unwrap();
    trace(
        "native-lifecycle-oracle",
        IvyJson::object([
            ("rust", IvyJson::Array(rows)),
            ("original", original.unwrap_or(IvyJson::Null)),
            ("physicalAudioOpens", n(0)),
        ]),
    );
}
