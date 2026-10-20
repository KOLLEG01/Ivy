use super::*;
#[path = "phone_native_answer.rs"]
mod answer;
#[path = "phone_native_dial.rs"]
mod dial;
#[path = "phone_native_feedback.rs"]
mod feedback;
#[path = "phone_native_incoming.rs"]
mod incoming;
#[path = "phone_native_notifications.rs"]
mod notifications;
#[path = "phone_native_registration.rs"]
mod registration;
#[path = "phone_native_requests.rs"]
mod requests;
#[path = "phone_native_screening.rs"]
mod screening;
#[path = "phone_native_status.rs"]
mod status;
#[path = "phone_native_upgrade.rs"]
mod upgrade;
use ivy_phone_bridge::{
    native_pipe::{NativeRuntime, NativeShutdown, NativeShutdownError},
    native_rpc::{NativeDispatcher, NativeHandler, NativeInvocation, NativeOutcome},
    native_sip_commands::NativeSipCommands,
    native_sip_status::NativeSipViews,
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
    actual_owner_with(first, last, Arc::new(Clock), Arc::new(Events::default()))
}
fn actual_owner_with(
    first: u16,
    last: u16,
    runtime: Arc<dyn ivy_phone_bridge::media::RtcpRuntime>,
    events: Arc<dyn SipReceiveEvents>,
) -> (
    NativeSipOperations,
    Arc<CountedFactory>,
    Arc<Factory>,
    String,
) {
    let routes = Arc::new(Factory::routes(vec![]));
    let factory = Arc::new(CountedFactory {
        inner: NativeSipSessionFactory::new(routes.clone(), None, runtime, move |_| events.clone()),
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
        if ivy_phone_bridge::ipc::is_read_only(method) { "null".into() } else { format!("\"{id}\"") }).as_bytes()).unwrap()
}
fn closed_platform_views() -> NativeSipViews {
    NativeSipViews::new(
        || Ok("null".into()),
        |call| {
            assert!(!call.media().unwrap().call_audio().status().route.is_some());
            Ok("null".into())
        },
    )
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
async fn native_features_dto_error_order_and_saved_receipts_match_original_outgoing_calls() {
    let (owner, factory, routes, params) = actual_owner(52000, 52998);
    let remaining = Arc::new(Remaining::default());
    remaining.close_gate.release();
    let handler = Arc::new(NativeSipCommands::new(
        owner.clone(),
        remaining.clone(),
        |_| Box::pin(async { Ok(()) }),
        closed_platform_views(),
    ));
    let selected = format!(r#"{{"callId":"{CALL}"}}"#);
    let challenge = |raw: &str| format!(r#"{{"callId":"{CALL}","challenge":{raw}}}"#);
    let mut steps: Vec<(String, String)> = vec![
        (
            "call.features".into(),
            r#"{"callId":null,"challenge":null}"#.into(),
        ),
        ("configure".into(), params),
        (
            "call.features".into(),
            r#"{"callId":null,"challenge":null}"#.into(),
        ),
        (
            "call.features".into(),
            r#"{"callId":12,"challenge":null}"#.into(),
        ),
        (
            "call.features".into(),
            r#"{"callId":"missing","challenge":3}"#.into(),
        ),
        ("call.prepare".into(), selected.clone()),
        ("call.features".into(), selected.clone()),
        (
            "call.features".into(),
            format!(r#"{{"callId":"{CALL}","challenge":null,"extra":1}}"#),
        ),
    ];
    for raw in [
        "3",
        "[]",
        "false",
        r#"{"unknown":true}"#,
        r#"{"code":3}"#,
        r#"{"code":"*1#","timeoutSeconds":1.0}"#,
        r#"{"code":"*1#","timeoutSeconds":1e0}"#,
        r#"{"code":"*1#","maxFailedAttempts":2147483648}"#,
        r#"{"code":"*1#","timeoutSeconds":null}"#,
        r#"{"Code":"*1#"}"#,
        r#"{}"#,
        r#"{"code":null}"#,
        r#"{"code":"*1#"}"#,
        r#"{"code":"*1#","timeoutSeconds":0}"#,
    ] {
        steps.push(("call.features".into(), challenge(raw)));
    }
    steps.extend([
        ("call.features".into(), challenge("null")),
        ("call.features".into(), challenge("null")),
        ("call.hangup".into(), selected.clone()),
        (
            "call.features".into(),
            challenge(r#"{"code":"*1#","timeoutSeconds":0}"#),
        ),
        (
            "call.features".into(),
            challenge(r#"{"code":"*1#","timeoutSeconds":1.0}"#),
        ),
        ("call.release".into(), selected),
    ]);
    let inputs: Vec<_> = steps
        .iter()
        .map(|(method, params)| {
            IvyJson::object([
                ("method", method.clone().into()),
                ("params", parse(params, 65536).unwrap()),
            ])
        })
        .collect();
    // Keep raw params for fractional/exponent spellings: the original handler
    // consumes those exact tokens, independently of the test's projection.
    let raw = format!(
        "[{}]",
        steps
            .iter()
            .map(|(method, params)| format!(r#"{{"method":"{method}","params":{params}}}"#))
            .collect::<Vec<_>>()
            .join(",")
    );
    let original = if std::env::var_os("IVY_PHONE_CONFIG_ORACLE_DIR").is_some() {
        let mut child = Command::new("pwsh")
            .args([
                "-NoProfile",
                "-File",
                "tests/fixtures/phone-native-lifecycle-oracle.ps1",
            ])
            .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.."))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(raw.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Some(
            parse(
                std::str::from_utf8(&output.stdout)
                    .unwrap()
                    .trim_start_matches('\u{feff}'),
                1_000_000,
            )
            .unwrap(),
        )
    } else {
        None
    };
    let dispatcher = NativeDispatcher::new(CALL.into(), 128, handler.clone()).unwrap();
    let mut rows = Vec::new();
    let mut saved = None;
    for (index, (method, params)) in steps.iter().enumerate() {
        let reply = dispatcher
            .execute(command(method, params, index as u64 + 1, index as u64 + 1))
            .await;
        let row = if let Some(error) = reply.outcome().error_code() {
            IvyJson::object([("error", error.into())])
        } else if method == "configure" {
            IvyJson::object([("configured", true.into())])
        } else {
            value(reply.outcome())
        };
        if method == "call.features" && reply.outcome().error_code().is_none() {
            saved = Some((index, reply.outcome().result_json().unwrap().to_owned()));
        }
        if let Some(original) = &original {
            assert_eq!(
                &row,
                &original.as_array().unwrap()[index],
                "feature handler step {index} {method} {params}"
            );
        }
        rows.push(row);
    }
    let (index, snapshot) = saved.unwrap();
    let replay = dispatcher
        .execute(command(
            &steps[index].0,
            &steps[index].1,
            index as u64 + 1,
            1000,
        ))
        .await;
    assert_eq!(replay.outcome().result_json(), Some(snapshot.as_str()));
    assert_eq!(factory.starts.load(Ordering::SeqCst), 1);
    assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
    assert!(remaining.methods.lock().unwrap().is_empty());
    handler.shutdown().await.unwrap();
    trace(
        "native-call-features",
        IvyJson::object([
            ("inputs", IvyJson::Array(inputs)),
            ("rawInputs", raw.into()),
            ("rust", IvyJson::Array(rows)),
            ("original", original.unwrap_or(IvyJson::Null)),
            ("savedSnapshotAfterRelease", true.into()),
            ("originalFactoryInvocations", n(1)),
            ("physicalAudioOpens", n(0)),
        ]),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admitted_media_delivers_actual_rtp_digits_to_the_same_call_policy_and_retires_it() {
    let (owner, factory, routes, params) = actual_owner(53000, 53998);
    owner
        .configure(&configuration(&params), |_| async { Ok(()) })
        .unwrap()
        .wait()
        .await
        .unwrap();
    let call = owner.prepare(CALL.into()).unwrap();
    call.prepared().await.unwrap();
    let features = call.features();
    let media = call.media().unwrap();
    let transport = media.observation().receive.transport;
    let (rtp, rtcp) = pair().await;
    let peer = rtp.local_addr().unwrap();
    let offer=SdpDocument::parse(&format!("v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {} RTP/AVP 8 101\r\na=rtpmap:8 PCMA/8000\r\na=rtpmap:101 telephone-event/8000\r\na=sendrecv\r\n",peer.port())).unwrap();
    media
        .set_remote_description(SdpType::Offer, &offer)
        .unwrap();
    media.create_answer().unwrap();
    assert!(ivy_phone_bridge::sip::DelayedInviteMedia::start(&media).await);
    let codec = ivy_phone_bridge::media::G711Codec::new(&Codec::Pcma.offered_formats()[0]).unwrap();
    crate::sip_receiver::send(
        &rtp,
        transport.local_rtp,
        0,
        8,
        111,
        &codec.encode_20ms(&[0; 160]),
    )
    .await;
    // The next datagram is a causal barrier on this same receive loop. Its
    // event counter proves the preceding unconfigured audio packet finished.
    crate::sip_receiver::send(
        &rtp,
        transport.local_rtp,
        0,
        101,
        111,
        &[0, 0x80 | 10, 0, 160],
    )
    .await;
    until(|| media.observation().receive.event_packets == 1).await;
    assert_eq!(media.observation().receive.inband_analyzed_packets, 0);
    owner.configure_features(&call, None).unwrap();
    features.connected(); // Synthetic call notification; this is not a dial-path qualification.
    for frame in 0..10 {
        let pcm = std::array::from_fn(|i| {
            let t = (usize::from(frame) * 160 + i) as f64 / 8000.0;
            (((770.0 * std::f64::consts::TAU * t).sin()
                + (1336.0 * std::f64::consts::TAU * t).sin())
                * 6000.0) as i16
        });
        crate::sip_receiver::send(
            &rtp,
            transport.local_rtp,
            frame + 1,
            8,
            111,
            &codec.encode_20ms(&pcm),
        )
        .await;
        until(|| media.observation().receive.inband_analyzed_packets == u64::from(frame) + 1).await;
    }
    until(|| media.observation().receive.inband_detected_digits == 1).await;
    assert_eq!(features.observation().command_sequence, 0);
    for (sequence, event) in [10, 0, 11].into_iter().enumerate() {
        crate::sip_receiver::send(
            &rtp,
            transport.local_rtp,
            sequence as u16 + 11,
            101,
            111,
            &[event, 0x80 | 10, 0, 160],
        )
        .await;
    }
    until(|| features.observation().command_sequence == 1).await;
    assert_eq!(features.observation().command, Some("new_voice"));
    // The same original policy is still installed after SDP renegotiation.
    media
        .set_remote_description(SdpType::Offer, &offer)
        .unwrap();
    media.create_answer().unwrap();
    assert_eq!(features.observation().command_sequence, 1);
    // Initialize analysis again if SDP reset it, then prove that acceptance
    // disables analysis on the next audio packet while PCM remains closed.
    let before_analyzed = media.observation().receive.inband_analyzed_packets;
    crate::sip_receiver::send(
        &rtp,
        transport.local_rtp,
        14,
        8,
        111,
        &codec.encode_20ms(&[0; 160]),
    )
    .await;
    until(|| media.observation().receive.inband_analyzed_packets > before_analyzed).await;
    assert!(media.observation().receive.inband.is_some());
    let analyzed = media.observation().receive.inband_analyzed_packets;
    features.start_screening().unwrap();
    crate::sip_receiver::send(
        &rtp,
        transport.local_rtp,
        15,
        101,
        111,
        &[1, 0x80 | 10, 0, 160],
    )
    .await;
    until(|| features.observation().screening == "accepted").await;
    crate::sip_receiver::send(
        &rtp,
        transport.local_rtp,
        16,
        8,
        111,
        &codec.encode_20ms(&[0; 160]),
    )
    .await;
    until(|| media.observation().receive.inband.is_none()).await;
    assert_eq!(
        media.observation().receive.inband_analyzed_packets,
        analyzed
    );
    let before = features.observation();
    owner.hangup(&call).unwrap();
    assert_eq!(features.observation(), before); // Hangup retains feature observations.
    owner.release(&call).unwrap().wait().await.unwrap();
    assert!(!features.needs_inband_dtmf());
    assert_eq!(features.observation().command_sequence, 1);
    let replacement = owner.prepare(CALL.into()).unwrap();
    replacement.prepared().await.unwrap();
    assert_eq!(replacement.features().observation().command_sequence, 0);
    features.digit('*', InputSource::SipInfo).unwrap();
    assert_eq!(replacement.features().observation().command_sequence, 0);
    assert_eq!(factory.starts.load(Ordering::SeqCst), 2);
    assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
    owner.close().wait().await.unwrap();
    drop((media, call, replacement, rtp, rtcp));
    trace(
        "native-call-features-media",
        IvyJson::object([
            ("actualRtpDigits", n(5)),
            ("ignoredBeforeConfiguration", n(1)),
            ("actualInBandDigits", n(1)),
            ("dynamicAnalysisPolicy", true.into()),
            ("samePolicyAfterRenegotiation", true.into()),
            ("retiredAtRelease", true.into()),
            ("replacementStartsFresh", true.into()),
            ("originalFactoryInvocations", n(2)),
            ("physicalAudioOpens", n(0)),
        ]),
    );
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
        closed_platform_views(),
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
        .execute(command("inventory", "{\"caller\":42}", 0, 9))
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
        vec![("inventory".into(), parse("{\"caller\":42}", 65536).unwrap())]
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
            closed_platform_views(),
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
            NativeSipViews::new(
                move || {
                    count.fetch_add(1, Ordering::SeqCst);
                    match mode {
                        0 => Err(SipHostError::NotReady.into()),
                        1 => Ok(format!("{{\"value\":\"{}\"}}", "x".repeat(65536))),
                        2 => panic!("synthetic projection panic"),
                        _ => Ok("{\"invalid\":NaN}".into()),
                    }
                },
                |_| Ok("null".into()),
            ),
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
        assert!(
            dispatcher
                .execute(command("call.prepare", &selected, 2, 2))
                .await
                .outcome()
                .error_code()
                .is_none()
        );
        let code = match mode {
            0 => "runtime_not_ready",
            _ => "operation_outcome_unknown",
        };
        for request in [2, 3] {
            assert_eq!(
                dispatcher
                    .execute(command("status", &selected, 0, request))
                    .await
                    .outcome()
                    .error_code(),
                Some(code)
            );
        }
        assert!(owner.find(CALL).is_some());
        assert_eq!(views.load(Ordering::SeqCst), 2);
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
            ("projectionInvocations", n(2)),
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
        closed_platform_views(),
    ));
    let selected = format!(r#"{{"callId":"{CALL}"}}"#);
    let steps: Vec<(&str, String)> = vec![
        ("call.prepare", "{}".into()),
        ("call.hangup", "{\"callId\":42}".into()),
        ("call.release", "{\"callId\":null}".into()),
        ("registration.reconnect", "{}".into()),
        ("configure", "{}".into()),
        ("configure", params.clone()),
        ("registration.reconnect", "{}".into()),
        ("registration.reconnect", "{\"extra\":null}".into()),
        ("registration.reconnect", "{\"callId\":42}".into()),
        ("registration.reconnect", "{\"callId\":null}".into()),
        ("configure", "{\"callId\":null}".into()),
        ("configure", "{}".into()),
        ("configure", "{\"callId\":42}".into()),
        ("call.prepare", "{}".into()),
        ("call.prepare", "{\"callId\":null}".into()),
        ("call.prepare", "{\"callId\":42}".into()),
        ("call.prepare", "{\"callId\":\"bad\"}".into()),
        ("call.hangup", "{\"callId\":null}".into()),
        ("call.release", "{\"callId\":null}".into()),
        ("call.release", "{\"callId\":\"\"}".into()),
        ("call.hangup", "{\"callId\":\"\"}".into()),
        ("call.release", "{\"callId\":\"bad\"}".into()),
        ("call.prepare", selected.clone()),
        ("call.prepare", selected.clone()),
        ("registration.reconnect", "{}".into()),
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
    if let Some(value) = &original {
        trace("native-lifecycle-original", value.clone());
    }
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
