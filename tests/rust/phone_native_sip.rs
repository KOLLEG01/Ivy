use super::*;
use ivy_phone_bridge::{
    native_rpc::NativeRequest,
    native_sip::{
        NativeCallConfiguration, NativeSipMediaFactory, NativeSipMediaFuture, NativeSipOperations,
    },
};
const BASE: &str = r#"{"binding":{"address":"127.0.0.1","port":0,"transport":"udp","rtpPortRangeStart":57000,"rtpPortRangeEnd":57998},"codecs":{"preferences":["PCMA","PCMU"],"receiveReorderMs":20},"incomingPeers":["registered","127.0.0.1"],"registration":null,"password":null}"#;
fn configuration(params: &str) -> NativeRequest {
    NativeRequest::parse(format!(r#"{{"version":1,"epoch":"{CALL}","requestId":1,"operationId":"{CALL}","method":"configure","params":{params}}}"#).as_bytes()).unwrap()
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
async fn failed_native_setup_and_uncertain_construction_retain_original_capacity_and_binding() {
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
        assert_eq!(
            owner.prepare(CALL.into()).err(),
            Some(SipHostError::NotReady)
        );
        assert_eq!(factory.starts.load(Ordering::SeqCst), 0);
        assert!(std::net::UdpSocket::bind(endpoint).is_err());
        factory.owner.lock().unwrap().take();
        let closed = owner.close().wait().await;
        if uncertain {
            assert_eq!(closed, Err(SipHostError::CleanupUnconfirmed));
            assert!(std::net::UdpSocket::bind(endpoint).is_err());
        } else {
            closed.unwrap();
            drop(std::net::UdpSocket::bind(endpoint).unwrap());
        }
        rows.push(IvyJson::object([
            ("setupUncertain", uncertain.into()),
            ("noFactoryDispatch", true.into()),
            ("closeConfirmed", (!uncertain).into()),
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
