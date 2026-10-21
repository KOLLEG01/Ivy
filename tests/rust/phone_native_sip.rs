use super::*;
#[path = "phone_native_sip_commands.rs"]
mod commands;
use ivy_phone_bridge::{
    media::{SipMediaEntropy, SystemSipSdpRoute},
    native_rpc::NativeRequest,
    native_sip::{
        NativeCallConfiguration, NativeSipMediaFactory, NativeSipMediaFuture, NativeSipOperations,
    },
    native_sip_media::NativeSipSessionFactory,
};
const BASE: &str = r#"{"binding":{"address":"127.0.0.1","port":0,"transport":"udp","rtpPortRangeStart":57000,"rtpPortRangeEnd":57998},"codecs":{"preferences":["PCMA","PCMU"],"receiveReorderMs":20},"incomingPeers":["registered","127.0.0.1"],"registration":null,"password":null}"#;
fn configuration(params: &str) -> NativeRequest {
    NativeRequest::parse(format!(r#"{{"version":1,"epoch":"{CALL}","requestId":1,"operationId":"{CALL}","method":"configure","params":{params}}}"#).as_bytes()).unwrap()
}
fn sdp_address(description: &SdpDocument) -> IvyJson {
    IvyJson::object([
        ("originType", description.origin().address_type().into()),
        (
            "originAddress",
            description.origin().address().unwrap().into(),
        ),
        (
            "connectionType",
            description.connection().unwrap().address_type().into(),
        ),
        (
            "connectionAddress",
            description.connection().unwrap().address().into(),
        ),
    ])
}
fn address_fields(value: &IvyJson) -> IvyJson {
    IvyJson::object(
        [
            "originType",
            "originAddress",
            "connectionType",
            "connectionAddress",
        ]
        .map(|key| (key, value.get(key).unwrap().clone())),
    )
}
fn check_identity(description: &SdpDocument, media: &SipMediaSession) {
    assert!(
        (1_000_000_000..=i32::MAX as u32)
            .contains(&description.origin().session_id().parse().unwrap())
    );
    let raw = description.to_sdp();
    let line = raw
        .lines()
        .find(|line| line.starts_with("a=ssrc:"))
        .unwrap();
    let (source, cname) = line
        .strip_prefix("a=ssrc:")
        .unwrap()
        .split_once(" cname:")
        .unwrap();
    let source: u32 = source.parse().unwrap();
    assert!(source <= i32::MAX as u32);
    assert_eq!(media.observation().receive.rtcp.local_source, Some(source));
    assert_eq!(cname.len(), 36);
    assert!(cname.bytes().enumerate().all(|(index, byte)| {
        if matches!(index, 8 | 13 | 18 | 23) {
            byte == b'-'
        } else {
            byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
        }
    }));
    assert_eq!(cname.as_bytes()[14], b'4');
    assert!(b"89ab".contains(&cname.as_bytes()[19]));
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admitted_factory_uses_sdk_identity_and_offer_answer_addresses() {
    let inputs: Vec<_> = [
        ("offer-any", "0.0.0.0", None),
        ("offer-specific", "127.0.0.1", None),
        ("offer-v6-any", "::", None),
        ("offer-v6-specific", "::1", None),
        ("answer-global", "0.0.0.0", Some(Some("127.0.0.1"))),
        ("answer-media-only", "0.0.0.0", Some(None)),
        ("answer-global-any", "0.0.0.0", Some(Some("0.0.0.0"))),
        ("answer-global-v6", "0.0.0.0", Some(Some("::1"))),
        (
            "answer-global-mapped",
            "0.0.0.0",
            Some(Some("::ffff:127.0.0.1")),
        ),
        ("answer-v6-bind", "::", Some(Some("127.0.0.1"))),
        ("answer-specific", "127.0.0.1", Some(Some("::1"))),
    ]
    .into_iter()
    .map(|(label, binding, global)| {
        let mut fields = vec![("label", label.into()), ("binding", binding.into())];
        if let Some(global) = global {
            fields.push((
                "remote",
                IvyJson::object([
                    ("global", global.map(Into::into).unwrap_or(IvyJson::Null)),
                    ("media", "127.0.0.1".into()),
                ]),
            ));
        }
        IvyJson::object(fields)
    })
    .collect();
    trace(
        "native-media-identity-inputs",
        IvyJson::Array(inputs.clone()),
    );
    let original =
        original_session_fixture("tests/fixtures/phone-media-identity-oracle.ps1", &inputs);
    let mut rows = Vec::new();
    for (index, input) in inputs.iter().enumerate() {
        let binding = input.get("binding").unwrap().as_str().unwrap();
        let routes = Arc::new(Factory::routes(vec![]));
        let owner = NativeSipOperations::new(Arc::new(NativeSipSessionFactory::new(
            routes.clone(),
            None,
            Arc::new(Clock),
            |_| Arc::new(Events::default()),
        )));
        let params = BASE
            .replace(
                "\"address\":\"127.0.0.1\"",
                &format!("\"address\":\"{binding}\""),
            )
            .replace("57000", "43000")
            .replace("57998", "43998");
        owner
            .configure(&configuration(&params), |_| async { Ok(()) })
            .unwrap()
            .wait()
            .await
            .unwrap();
        let call = owner.prepare(CALL.into()).unwrap();
        call.prepared()
            .await
            .unwrap_or_else(|error| panic!("{input:?}: {error:?}"));
        let media = call.media().unwrap();
        let offer = media.create_offer().unwrap();
        check_identity(&offer, &media);
        assert_eq!(offer.connection().unwrap().address(), binding);
        let mut answer = IvyJson::Null;
        let (_peer, _control) = pair().await;
        if let Some(remote) = input.get("remote") {
            let port = _peer.local_addr().unwrap().port();
            let global = remote.get("global").unwrap().as_str().ok();
            let global_line = global
                .map(|ip| {
                    format!(
                        "c=IN {} {ip}\r\n",
                        if ip.contains(':') { "IP6" } else { "IP4" }
                    )
                })
                .unwrap_or_default();
            let description = SdpDocument::parse(&format!("v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\n{global_line}t=0 0\r\nm=audio {port} RTP/AVP 8\r\nc=IN IP4 127.0.0.1\r\n")).unwrap();
            media
                .set_remote_description(SdpType::Offer, &description)
                .unwrap();
            let created = media.create_answer().unwrap();
            assert_eq!(created.origin().session_id(), offer.origin().session_id());
            answer = sdp_address(&created);
        }
        let row = IvyJson::object([
            ("label", input.get("label").unwrap().clone()),
            ("offer", sdp_address(&offer)),
            ("answer", answer),
        ]);
        if let Some(original) = &original {
            let expected = &original.as_array().unwrap()[index];
            assert_eq!(expected.get("ok").unwrap(), &IvyJson::Bool(true));
            assert_eq!(
                row.get("offer").unwrap(),
                &address_fields(expected.get("offer").unwrap())
            );
            if input.get("remote").is_some() {
                assert_eq!(
                    expected.get("applied").unwrap(),
                    &IvyJson::String("OK".into())
                );
                assert_eq!(
                    row.get("answer").unwrap(),
                    &address_fields(expected.get("answer").unwrap())
                );
            }
            let identity = expected.get("identity").unwrap();
            assert!(
                (1_000_000_000.0..=f64::from(i32::MAX)).contains(
                    &identity
                        .get("sessionId")
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .parse::<f64>()
                        .unwrap()
                )
            );
            assert!(identity.get("source").unwrap().as_f64().unwrap() <= f64::from(i32::MAX));
            assert!(identity.get("sequence").unwrap().as_f64().unwrap() <= f64::from(u16::MAX));
            assert_eq!(identity.get("timestamp").unwrap().as_f64().unwrap(), 0.0);
        }
        assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
        let transport = media.observation().receive.transport;
        owner.release(&call).unwrap().wait().await.unwrap();
        drop(std::net::UdpSocket::bind(transport.local_rtp).unwrap());
        drop(std::net::UdpSocket::bind(transport.local_rtcp).unwrap());
        owner.close().wait().await.unwrap();
        rows.push(row);
    }
    trace(
        "native-media-identity",
        IvyJson::object([
            ("cases", IvyJson::Array(rows)),
            ("original", original.unwrap_or(IvyJson::Null)),
            ("physicalAudioOpens", n(0)),
        ]),
    );
}

#[derive(Default)]
struct Entropy {
    fills: AtomicUsize,
    fail: AtomicBool,
    gate: Option<Arc<CaptureGate>>,
}
impl SipMediaEntropy for Entropy {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), MediaError> {
        if self.fills.fetch_add(1, Ordering::SeqCst) == 0 {
            if let Some(gate) = &self.gate {
                gate.block();
            }
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err(MediaError::EntropyUnavailable);
        }
        bytes.fill(0);
        Ok(())
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_factory_retains_one_identity_attempt_and_clean_entropy_failure_can_retire() {
    let gate = Arc::new(CaptureGate::default());
    let entropy = Arc::new(Entropy {
        gate: Some(gate.clone()),
        ..Entropy::default()
    });
    let routes = Arc::new(Factory::routes(vec![]));
    let owner = NativeSipOperations::new(Arc::new(NativeSipSessionFactory::with_providers(
        routes.clone(),
        None,
        Arc::new(Clock),
        |_| Arc::new(Events::default()),
        entropy.clone(),
        Arc::new(SystemSipSdpRoute),
    )));
    assert_eq!(
        owner.prepare(CALL.into()).err(),
        Some(SipHostError::NotReady)
    );
    assert_eq!(entropy.fills.load(Ordering::SeqCst), 0);
    let params = BASE.replace("57000", "44000").replace("57998", "44998");
    owner
        .configure(&configuration(&params), |_| async { Ok(()) })
        .unwrap()
        .wait()
        .await
        .unwrap();
    let call = owner.prepare(CALL.into()).unwrap();
    until(|| gate.entered.load(Ordering::SeqCst)).await;
    assert_eq!(entropy.fills.load(Ordering::SeqCst), 1);
    assert_eq!(
        owner.prepare(CALL.into()).err(),
        Some(SipHostError::OperationConflict)
    );
    drop(owner.release(&call).unwrap());
    assert!(owner.find(CALL).is_some());
    assert!(call.media().is_none());
    gate.release();
    until(|| owner.find(CALL).is_none()).await;
    assert_eq!(entropy.fills.load(Ordering::SeqCst), 4);
    assert!(call.media().unwrap().observation().cleanup_confirmed);
    entropy.fail.store(true, Ordering::SeqCst);
    let failed = owner.prepare(CALL.into()).unwrap();
    assert_eq!(
        failed.prepared().await,
        Err(SipHostError::InitializationFailed)
    );
    assert!(failed.media().is_none());
    owner.release(&failed).unwrap().wait().await.unwrap();
    assert!(owner.find(CALL).is_none());
    entropy.fail.store(false, Ordering::SeqCst);
    let next = owner.prepare(CALL.into()).unwrap();
    next.prepared().await.unwrap();
    assert_eq!(entropy.fills.load(Ordering::SeqCst), 9);
    let offer = next.media().unwrap().create_offer().unwrap();
    assert_eq!(offer.origin().session_id(), "1000000000");
    assert_eq!(
        next.media()
            .unwrap()
            .observation()
            .receive
            .rtcp
            .local_source,
        Some(0)
    );
    let (peer, _control) = pair().await;
    let media = next.media().unwrap();
    let remote = SdpDocument::parse(&format!("v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {} RTP/AVP 8\r\n", peer.local_addr().unwrap().port())).unwrap();
    media
        .set_remote_description(SdpType::Offer, &remote)
        .unwrap();
    media.begin_waiting().unwrap();
    assert!(media.start().await);
    let mut bytes = [0; 2048];
    let count = timeout(Duration::from_secs(2), peer.recv(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    let packet = RtpPacket::parse(&bytes[..count]).unwrap();
    assert_eq!(packet.header.source, 0);
    assert_eq!(packet.header.sequence, 0);
    assert_eq!(packet.header.timestamp, 0);
    assert_eq!(packet.header.payload_type, 8);
    assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
    owner.release(&next).unwrap().wait().await.unwrap();
    owner.close().wait().await.unwrap();
    trace(
        "native-media-admission",
        IvyJson::object([
            ("fills", n(9)),
            ("oneConstructionPerSlot", true.into()),
            ("lateFactoryDrained", true.into()),
            ("entropyFailureClean", true.into()),
            ("zeroSsrcAccepted", true.into()),
            ("initialRtpSeed", true.into()),
            ("physicalAudioOpens", n(0)),
        ]),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_media_construction_distinguishes_confirmed_and_uncertain_audio_cleanup() {
    let mut rows = Vec::new();
    for uncertain in [false, true] {
        let route = Arc::new(Route::new());
        route.state.lock().unwrap().bad_close = uncertain;
        let audio = CallAudioPort::new(CALL.into(), Arc::new(Factory::routes(vec![route.clone()])))
            .unwrap();
        audio
            .prepare(settings(), None, CancellationToken::new())
            .unwrap()
            .wait()
            .await
            .unwrap();
        let result = SipMediaSession::bind(
            SipMediaSettings {
                call_id: "22222222-2222-4222-8222-222222222222".into(), // Fail before RTP binding.
                interface: "127.0.0.1:0".parse().unwrap(),
                connection_address: IpAddr::from([127, 0, 0, 1]),
                first_port: 57000,
                last_port: 57998,
                reorder_ms: 20,
                preferences: vec![Codec::Pcma],
                identity: SipMediaIdentity {
                    session_id: 1,
                    source: 17,
                    sequence: 0,
                    timestamp: 0,
                    cname: "fixture".into(),
                },
            },
            audio.clone(),
            None,
            Arc::new(Clock),
            Arc::new(Events::default()),
        )
        .await;
        assert_eq!(
            result.err(),
            Some(if uncertain {
                SipMediaError::CleanupUnconfirmed
            } else {
                SipMediaError::InvalidRequest
            })
        );
        assert_eq!(route.state.lock().unwrap().disposals, 1);
        rows.push(IvyJson::object([
            ("uncertain", uncertain.into()),
            ("disposals", n(1)),
        ]));
    }
    trace("native-media-construction-cleanup", IvyJson::Array(rows));
}
async fn until(ready: impl Fn() -> bool) {
    timeout(Duration::from_secs(3), async {
        while !ready() {
            sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}
struct MediaFactory {
    gate: Arc<Gate>,
    owner: Arc<Mutex<Option<NativeSipOperations>>>,
    starts: AtomicUsize,
    mode: AtomicUsize,
}
impl NativeSipMediaFactory for MediaFactory {
    fn create(
        &self,
        id: String,
        configuration: Arc<NativeCallConfiguration>,
    ) -> NativeSipMediaFuture {
        self.starts.fetch_add(1, Ordering::SeqCst);
        let owner = self.owner.lock().unwrap().as_ref().unwrap().clone();
        assert!(
            owner.find(&id).is_some(),
            "Native call metadata must precede factory invocation"
        );
        assert_eq!(owner.observation().calls[0].id, id);
        assert_eq!(configuration.binding().rtp_port_range(), (57000, 57998));
        assert_eq!(
            configuration.codecs().preferences(),
            &[Codec::Pcma, Codec::Pcmu]
        );
        assert!(configuration.incoming_peers().uses_registered_peer());
        let settings = configuration.media_settings(
            id.clone(),
            SipMediaIdentity {
                session_id: 321,
                source: 17,
                sequence: 100,
                timestamp: 200,
                cname: "fixture".into(),
            },
            IpAddr::from([127, 0, 0, 1]),
        );
        assert_eq!(settings.interface, "127.0.0.1:0".parse().unwrap());
        let gate = self.gate.clone();
        let mode = self.mode.load(Ordering::SeqCst);
        Box::pin(async move {
            gate.block().await;
            match mode {
                1 => return Err(SipHostError::InvalidRequest), // Before creating resources.
                2 => return Err(SipHostError::CleanupUnconfirmed),
                3 => panic!("synthetic original factory panic"),
                _ => {}
            }
            let audio = CallAudioPort::new(id, Arc::new(Factory::routes(vec![]))).unwrap();
            SipMediaSession::bind(
                settings,
                audio,
                None,
                Arc::new(Clock),
                Arc::new(Events::default()),
            )
            .await
            .map_err(|_| SipHostError::CleanupUnconfirmed)
        })
    }
}
fn operations(mode: usize) -> (NativeSipOperations, Arc<MediaFactory>) {
    let factory = Arc::new(MediaFactory {
        gate: Arc::new(Gate::default()),
        owner: Arc::new(Mutex::new(None)),
        starts: AtomicUsize::new(0),
        mode: AtomicUsize::new(mode),
    });
    let owner = NativeSipOperations::new(factory.clone());
    *factory.owner.lock().unwrap() = Some(owner.clone());
    (owner, factory)
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_configuration_and_actual_media_admission_survive_lost_observers_and_late_close() {
    let (owner, factory) = operations(0);
    assert_eq!(
        owner
            .configure(&configuration("{}"), |_| async { Ok(()) })
            .err()
            .unwrap()
            .code(),
        "invalid_request"
    );
    assert!(!owner.configured());
    let setup = Arc::new(Gate::default());
    let original = setup.clone();
    let operation = owner
        .configure(&configuration(BASE), move |micro| async move {
            assert!(micro.is_none());
            original.block().await;
            Ok(())
        })
        .unwrap();
    drop(operation); // No external configure waiter owns the setup.
    until(|| setup.entered.load(Ordering::SeqCst)).await;
    assert!(owner.configured());
    assert_eq!(
        owner.prepare(CALL.into()).err(),
        Some(SipHostError::NotReady)
    );
    assert_eq!(factory.starts.load(Ordering::SeqCst), 0);
    assert_eq!(
        owner
            .configure(&configuration("{}"), |_| async {
                panic!("configure must not repeat")
            })
            .err()
            .unwrap()
            .code(),
        "operation_conflict"
    );
    setup.release();
    until(|| owner.configuration_result().is_some()).await;
    let endpoint = owner.configuration_result().unwrap().unwrap();
    assert!(std::net::UdpSocket::bind(endpoint).is_err());
    let call = owner.prepare(CALL.into()).unwrap();
    until(|| factory.gate.entered.load(Ordering::SeqCst)).await;
    assert!(call.media().is_none());
    assert!(
        timeout(Duration::from_millis(10), call.prepared())
            .await
            .is_err()
    );
    assert_eq!(
        owner.prepare(CALL.into()).err(),
        Some(SipHostError::OperationConflict)
    );
    assert_eq!(
        owner
            .prepare("22222222-2222-4222-8222-222222222222".into())
            .err(),
        Some(SipHostError::OperationConflict)
    );
    drop(owner.release(&call).unwrap());
    assert!(call.observation().ending);
    assert_eq!(owner.observation().calls.len(), 1);
    assert_eq!(factory.starts.load(Ordering::SeqCst), 1);
    factory.gate.release();
    until(|| owner.observation().calls.is_empty()).await;
    let media = call.media().unwrap();
    assert!(media.observation().closed && media.observation().cleanup_confirmed);
    let rtp = media.observation().receive.transport.local_rtp;
    let rtcp = media.observation().receive.transport.local_rtcp;
    drop(std::net::UdpSocket::bind(rtp).unwrap());
    drop(std::net::UdpSocket::bind(rtcp).unwrap());
    assert!(owner.find(CALL).is_none());
    let next = owner.prepare(CALL.into()).unwrap();
    next.prepared().await.unwrap();
    assert_eq!(owner.hangup(&call).err(), Some(SipHostError::NotReady));
    assert_eq!(owner.release(&call).err().map(|_| true), Some(true));
    assert!(!next.media().unwrap().observation().closed);
    owner.release(&next).unwrap().wait().await.unwrap();
    assert_eq!(factory.starts.load(Ordering::SeqCst), 2);
    factory.owner.lock().unwrap().take();
    owner.close().wait().await.unwrap();
    drop(std::net::UdpSocket::bind(endpoint).unwrap());
    assert_eq!(
        owner.prepare(CALL.into()).err(),
        Some(SipHostError::Stopping)
    );
    trace(
        "native-sip-admission",
        IvyJson::object([
            ("actualMedia", true.into()),
            ("slotBeforeFactory", true.into()),
            ("configurationWaiterDetached", true.into()),
            ("lateMediaDrained", true.into()),
            ("bothRtpPortsReleased", true.into()),
            ("oldHandleCannotCloseReplacement", true.into()),
            ("bindingReleased", true.into()),
        ]),
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_native_setup_retires_binding_but_uncertain_construction_retains_it() {
    let mut rows = Vec::new();
    for uncertain in [false, true] {
        let (owner, factory) = operations(0);
        let error = if uncertain {
            SipHostError::CleanupUnconfirmed
        } else {
            SipHostError::InvalidRequest
        };
        let operation = owner
            .configure(&configuration(BASE), move |_| async move { Err(error) })
            .unwrap();
        assert_eq!(operation.wait().await, Err(error));
        let endpoint = owner.observation().endpoint.unwrap();
        assert!(owner.configured());
        assert_eq!(factory.starts.load(Ordering::SeqCst), 0);
        assert!(std::net::UdpSocket::bind(endpoint).is_err());
        factory.owner.lock().unwrap().take();
        owner.close().wait().await.unwrap();
        drop(std::net::UdpSocket::bind(endpoint).unwrap());
        rows.push(IvyJson::object([
            ("setupUncertain", uncertain.into()),
            ("noFactoryDispatch", true.into()),
            ("closeConfirmed", true.into()),
        ]));
    }
    for mode in 1..=3 {
        let (owner, factory) = operations(mode);
        owner
            .configure(&configuration(BASE), |_| async { Ok(()) })
            .unwrap()
            .wait()
            .await
            .unwrap();
        let endpoint = owner.observation().endpoint.unwrap();
        let call = owner.prepare(CALL.into()).unwrap();
        until(|| factory.gate.entered.load(Ordering::SeqCst)).await;
        factory.gate.release();
        let prepared = call.prepared().await;
        let expected = if mode == 1 {
            SipHostError::InvalidRequest
        } else {
            SipHostError::CleanupUnconfirmed
        };
        assert_eq!(prepared, Err(expected));
        assert_eq!(
            owner.prepare(CALL.into()).err(),
            Some(SipHostError::OperationConflict)
        );
        let released = owner.release(&call).unwrap().wait().await;
        if mode == 1 {
            released.unwrap();
            assert!(owner.observation().calls.is_empty());
            owner.close().wait().await.unwrap();
            drop(std::net::UdpSocket::bind(endpoint).unwrap());
        } else {
            assert_eq!(released, Err(SipHostError::CleanupUnconfirmed));
            assert_eq!(owner.observation().calls.len(), 1);
            assert_eq!(
                owner.close().wait().await,
                Err(SipHostError::CleanupUnconfirmed)
            );
            assert!(std::net::UdpSocket::bind(endpoint).is_err());
        }
        factory.owner.lock().unwrap().take();
        rows.push(IvyJson::object([
            ("constructorMode", n(mode)),
            (
                "factoryInvocations",
                n(factory.starts.load(Ordering::SeqCst)),
            ),
            ("cleanupConfirmed", (mode == 1).into()),
        ]));
    }
    trace("native-sip-failures", IvyJson::Array(rows));
}
