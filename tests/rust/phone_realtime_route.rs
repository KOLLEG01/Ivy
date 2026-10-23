use super::*;
use crate::media::realtime::{
    network::{Datagrams, Sockets},
    route,
};
use crate::media::{
    AudioDesktopIdentity, AudioRoutePermit, AudioSettings, CallAudioError, CallAudioFactory,
    CallAudioPort, CallAudioRoute,
};
use std::{future::Future, net::UdpSocket, pin::Pin, sync::Mutex};
use tokio_util::sync::CancellationToken;

#[cfg(all(windows, target_pointer_width = "64"))]
mod native_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/rust/phone_realtime_native.rs"
    ));
}

struct NoDevices;
impl CallAudioFactory for NoDevices {
    fn open<'a>(
        &'a self,
        _: &'a AudioSettings,
        _: Option<&'a AudioDesktopIdentity>,
        _: AudioRoutePermit,
        _: CancellationToken,
    ) -> Pin<
        Box<
            dyn Future<Output = std::result::Result<Arc<dyn CallAudioRoute>, CallAudioError>>
                + Send
                + 'a,
        >,
    > {
        panic!("digital audio must never open a device route")
    }
}
fn port() -> CallAudioPort {
    CallAudioPort::new(
        "11111111-1111-4111-8111-111111111111".into(),
        Arc::new(NoDevices),
    )
    .unwrap()
    .test_realtime_factory(Arc::new(|queue, prebuffer, permitted, stop| {
        route::bound(
            queue,
            prebuffer,
            permitted,
            stop,
            vec!["127.0.0.1:0".parse().unwrap()],
        )
    }))
}
fn candidate(offer: &str) -> SocketAddr {
    let line = offer
        .lines()
        .find(|v| v.starts_with("a=candidate:"))
        .unwrap();
    let parts: Vec<_> = line.split_whitespace().collect();
    SocketAddr::new(parts[4].parse().unwrap(), parts[5].parse().unwrap())
}
fn permit() -> Arc<dyn Fn() -> bool + Send + Sync> {
    Arc::new(|| true)
}
struct Remote {
    rtc: Rtc,
    socket: UdpSocket,
    timeout: Instant,
    mid: Option<Mid>,
    events: bool,
    packets: Vec<Vec<u8>>,
}
impl Remote {
    fn answer(offer: &str) -> (Self, String) {
        let now = Instant::now();
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let mut remote = Self {
            rtc: Rtc::builder()
                .clear_codecs()
                .enable_opus(true, false)
                .set_rtp_mode(true)
                .set_fingerprint_verification(true)
                .build(now),
            socket,
            timeout: now,
            mid: None,
            events: false,
            packets: Vec::new(),
        };
        remote.drain();
        remote.rtc.add_local_candidate(
            Candidate::host(remote.socket.local_addr().unwrap(), "udp").unwrap(),
        );
        remote.drain();
        let answer = remote
            .rtc
            .sdp_api()
            .accept_offer(SdpOffer::from_sdp_string(offer).unwrap())
            .unwrap()
            .to_sdp_string();
        remote.drain();
        (remote, answer)
    }
    fn drain(&mut self) {
        loop {
            match self.rtc.poll_output().unwrap() {
                Output::Timeout(t) => {
                    self.timeout = t;
                    break;
                }
                Output::Transmit(p) => {
                    assert_eq!(p.source, self.socket.local_addr().unwrap());
                    assert_eq!(
                        self.socket.send_to(&p.contents, p.destination).unwrap(),
                        p.contents.len()
                    );
                }
                Output::Event(Event::MediaAdded(m)) => self.mid = Some(m.mid),
                Output::Event(Event::RtpPacket(p)) => self.packets.push(p.payload.to_vec()),
                Output::Event(Event::ChannelOpen(_, label)) if label == "oai-events" => {
                    self.events = true
                }
                _ => {}
            }
        }
    }
    fn step(&mut self) {
        let mut bytes = [0u8; 65_536];
        for _ in 0..32 {
            match self.socket.recv_from(&mut bytes) {
                Ok((length, source)) => {
                    let input = Receive::new(
                        Protocol::Udp,
                        source,
                        self.socket.local_addr().unwrap(),
                        &bytes[..length],
                    )
                    .unwrap();
                    self.rtc
                        .handle_input(Input::Receive(Instant::now(), input))
                        .unwrap();
                    self.drain();
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => panic!("{e}"),
            }
        }
        if Instant::now() >= self.timeout {
            self.rtc
                .handle_input(Input::Timeout(Instant::now()))
                .unwrap();
            self.drain();
        }
        bytes.fill(0);
    }
    async fn until(&mut self, ready: impl Fn(&Self) -> bool) {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                self.step();
                if ready(self) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("owned loopback peer deadline");
    }
    fn audio(&mut self, sequence: u64) {
        let mid = self.mid.unwrap();
        let pt = self.rtc.media(mid).unwrap().remote_pts()[0];
        self.rtc
            .direct_api()
            .stream_tx_by_mid(mid, None)
            .unwrap()
            .write_rtp(RtpWrite::new(
                pt,
                sequence.into(),
                sequence as u32 * 960,
                Instant::now(),
                Pair::encoded(),
            ));
        self.drain();
        self.rtc
            .handle_input(Input::Timeout(Instant::now()))
            .unwrap();
        self.drain();
    }
}

mod route_contracts {
    use super::*;

    #[test]
    fn sockets_publish_exact_sources_destinations_and_release_partial_bindings() {
        for binding in ["127.0.0.1:0", "[::1]:0"] {
            let binding: SocketAddr = binding.parse().unwrap();
            let mut sockets = Sockets::bind(vec![binding]).unwrap();
            let local = sockets.candidates()[0];
            let remote = UdpSocket::bind(binding).unwrap();
            remote
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            remote.send_to(b"probe", local).unwrap();
            let mut b = [0u8; 32];
            let deadline = Instant::now() + Duration::from_secs(1);
            let (n, source, destination) = loop {
                if let Some(v) = sockets.receive(&mut b).unwrap() {
                    break v;
                }
                assert!(Instant::now() < deadline);
                std::thread::yield_now();
            };
            assert_eq!((source, destination), (remote.local_addr().unwrap(), local));
            assert_eq!(&b[..n], b"probe");
            // Test publication by generating a real ICE transmit from the engine.
            let mut peer =
                RealtimePeer::new(Instant::now(), &[local], 80, None, permit(), &mut |p| {
                    sockets.send(p)
                })
                .unwrap();
            peer.close(&mut |p| sockets.send(p)).unwrap();
            drop(sockets);
            let released = UdpSocket::bind(local).unwrap();
            drop(released);
            let reserved = UdpSocket::bind(binding).unwrap();
            let occupied = reserved.local_addr().unwrap();
            assert!(Sockets::bind(vec![local, occupied]).is_err());
            assert!(
                UdpSocket::bind(local).is_ok(),
                "earlier socket survived partial bind failure"
            );
        }
        assert!(Sockets::bind(vec![]).is_err());
        assert!(matches!(
            Sockets::bind(vec!["0.0.0.0:0".parse().unwrap()]),
            Err(RealtimeError::InvalidRequest)
        ));
        trace(
            "route-sockets",
            serde_json::json!({"families":2,"actualLoopback":true,"partialBindReleased":true}),
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn owned_route_connects_udp_dtls_sctp_and_sends_only_admitted_pcm() {
        let allowed = Arc::new(AtomicBool::new(true));
        let permission = allowed.clone();
        let route = route::bound(
            100,
            Some(0),
            Arc::new(move || permission.load(Ordering::Acquire)),
            CancellationToken::new(),
            vec!["127.0.0.1:0".parse().unwrap()],
        )
        .await
        .unwrap();
        assert!(!route.state().open);
        let address = candidate(route.offer());
        let (mut remote, answer) = Remote::answer(route.offer());
        let accepted = route.answer(answer).unwrap();
        drop(accepted.clone());
        remote.until(|r| r.rtc.is_connected() && r.events).await;
        accepted.wait().await.unwrap();
        assert!(!route.state().open, "connection must not resume PCM");
        route.resume().unwrap();
        let admitted = route.state();
        assert!(admitted.open);
        assert!(route.write_received(&[0.25; 960], admitted.generation));
        remote.until(|r| !r.packets.is_empty()).await;
        for sequence in 0..4 {
            remote.audio(sequence);
            tokio::time::sleep(Duration::from_millis(4)).await;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        let mut pcm = [0.0; 960];
        assert_eq!(route.read_captured(&mut pcm, admitted.generation), 960);
        assert!(pcm.iter().any(|v| v.abs() > 0.001));
        allowed.store(false, Ordering::Release);
        assert!(!route.write_received(&[0.3; 960], admitted.generation));
        route.suspend().unwrap();
        allowed.store(true, Ordering::Release);
        route.resume().unwrap();
        assert_ne!(route.state().generation, admitted.generation);
        assert!(!route.write_received(&[0.3; 960], admitted.generation));
        let observation: serde_json::Value =
            serde_json::from_str(&route.observation().unwrap()).unwrap();
        assert!(observation["sentPackets"].as_i64().unwrap() >= 1);
        assert!(observation["receivedPackets"].as_i64().unwrap() >= 2);
        let close = route.close();
        drop(close);
        route.close().await.unwrap();
        assert!(!route.state().open);
        assert!(
            UdpSocket::bind(address).is_ok(),
            "close returned before socket release/join"
        );
        route.close().await.unwrap();
        trace("route-owned", observation);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn original_call_generations_stop_reset_and_revoke_retire_the_same_route() {
        let port = port();
        assert!(matches!(
            port.prepare_realtime(1, 80, None),
            Err(RealtimeError::NotReady)
        ));
        let preparing = port.prepare_realtime(0, 80, Some(0)).unwrap();
        drop(preparing.clone());
        assert!(matches!(
            port.prepare_realtime(0, 80, None),
            Err(RealtimeError::NotReady)
        ));
        preparing.wait().await.unwrap();
        let offer = port.realtime_offer(0).unwrap();
        let address = candidate(&offer);
        assert_eq!(port.status().state, "suspended");
        assert!(matches!(
            port.accept_realtime(1, "invalid".into()),
            Err(RealtimeError::NotReady)
        ));
        let (mut remote, answer) = Remote::answer(&offer);
        let connected = port.accept_realtime(0, answer).unwrap();
        remote.until(|r| r.rtc.is_connected() && r.events).await;
        connected.wait().await.unwrap();
        assert!(port.state().open);
        let old = port.state();
        let stopped = port.stop_realtime(0).unwrap();
        drop(stopped.clone());
        stopped.wait().await.unwrap();
        assert!(!port.state().open);
        assert!(port.realtime_observation().unwrap().is_none());
        assert!(UdpSocket::bind(address).is_ok());
        port.stop_realtime(0).unwrap().wait().await.unwrap();
        let next = port.prepare_realtime(1, 80, None).unwrap();
        next.wait().await.unwrap();
        let address = candidate(&port.realtime_offer(1).unwrap());
        assert_ne!(port.state(), old);
        assert!(matches!(
            port.stop_realtime(0),
            Err(RealtimeError::NotReady)
        ));
        port.revoke();
        port.close().wait().await.unwrap();
        assert!(UdpSocket::bind(address).is_ok());
        assert!(matches!(
            port.prepare_realtime(2, 80, None),
            Err(RealtimeError::NotReady)
        ));
        trace(
            "route-call-generation",
            serde_json::json!({"sameOriginalCall":true,"stoppedGeneration":0,"replacementGeneration":1,"revoked":true,"joined":true}),
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejected_preparation_consumes_its_generation_without_opening_devices() {
        let port = port();
        assert_eq!(
            port.prepare_realtime(0, 19, None).unwrap().wait().await,
            Err(RealtimeError::InvalidRequest)
        );
        assert!(matches!(
            port.prepare_realtime(1, 80, None),
            Err(RealtimeError::NotReady)
        ));
        port.stop_realtime(0).unwrap().wait().await.unwrap();
        port.prepare_realtime(1, 80, None)
            .unwrap()
            .wait()
            .await
            .unwrap();
        assert!(!port.state().open);
        assert!(matches!(
            port.accept_realtime(1, String::new()),
            Err(RealtimeError::InvalidRequest)
        ));
        port.stop_realtime(1).unwrap().wait().await.unwrap();
        port.close().wait().await.unwrap();
        let stop = CancellationToken::new();
        stop.cancel();
        assert!(matches!(
            route::bound(
                80,
                None,
                permit(),
                stop,
                vec!["127.0.0.1:0".parse().unwrap()]
            )
            .await,
            Err(RealtimeError::Cancelled)
        ));
        trace(
            "route-preparation",
            serde_json::json!({"invalidQueue":true,"generationConsumed":true,"suspended":true,"cancelledBeforeBind":true}),
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_driver_keeps_the_original_cleanup_until_close_and_joins_it_once() {
        struct Broken {
            sockets: Sockets,
            polls: Arc<AtomicUsize>,
            drops: Arc<AtomicUsize>,
        }
        impl Drop for Broken {
            fn drop(&mut self) {
                self.drops.fetch_add(1, Ordering::AcqRel);
            }
        }
        impl Datagrams for Broken {
            fn candidates(&self) -> Vec<SocketAddr> {
                self.sockets.candidates()
            }
            fn receive(&mut self, _: &mut [u8]) -> Result<Option<(usize, SocketAddr, SocketAddr)>> {
                self.polls.fetch_add(1, Ordering::AcqRel);
                Err(RealtimeError::Failed)
            }
            fn send(&mut self, p: Transmit) -> Result<()> {
                self.sockets.send(p)
            }
        }
        let polls = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let p = polls.clone();
        let d = drops.clone();
        let route = route::open_with(80, None, permit(), CancellationToken::new(), move || {
            Ok(Box::new(Broken {
                sockets: Sockets::bind(vec!["127.0.0.1:0".parse().unwrap()])?,
                polls: p,
                drops: d,
            }) as Box<dyn Datagrams>)
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !route.failed() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(polls.load(Ordering::Acquire), 1);
        assert_eq!(drops.load(Ordering::Acquire), 0);
        assert_eq!(route.status().get("state"), Some(&"failed".into()));
        assert!(!route.state().open);
        let address = candidate(route.offer());
        assert!(UdpSocket::bind(address).is_err());
        let closing = route.close();
        drop(closing);
        route.close().await.unwrap();
        route.close().await.unwrap();
        assert_eq!(drops.load(Ordering::Acquire), 1);
        assert!(UdpSocket::bind(address).is_ok());
        trace(
            "route-failure",
            serde_json::json!({"failedPolls":1,"resourcesRetainedUntilClose":true,"joined":true,"disposals":1}),
        );
    }

    #[tokio::test]
    async fn native_realtime_dtos_match_public_vectors_without_admitting_a_call_or_route() {
        use crate::{
            native_pipe::{NativeRuntime, NativeShutdown},
            native_rpc::{NativeHandler, NativeInvocation, NativeRequest},
            native_sip::{
                NativeCallConfiguration, NativeSipMediaFactory, NativeSipMediaFuture,
                NativeSipOperations,
            },
            native_sip_commands::NativeSipCommands,
            native_sip_status::NativeSipViews,
        };
        struct NoMedia;
        impl NativeSipMediaFactory for NoMedia {
            fn create(&self, _: String, _: Arc<NativeCallConfiguration>) -> NativeSipMediaFuture {
                panic!("No call admission authorized")
            }
        }
        struct Remaining;
        impl NativeHandler for Remaining {
            fn invoke(&self, _: NativeRequest) -> NativeInvocation {
                panic!("Realtime commands must be bound")
            }
            fn cleanup_slots(&self) -> usize {
                4
            }
        }
        impl NativeRuntime for Remaining {
            fn shutdown(&self) -> NativeShutdown {
                Box::pin(async { Ok(()) })
            }
        }
        let owner = NativeSipOperations::new(Arc::new(NoMedia));
        let handler = NativeSipCommands::new(
            owner.clone(),
            Arc::new(Remaining),
            |_| Box::pin(async { Ok(()) }),
            NativeSipViews::new(|| Ok("null".into()), |_| Ok("null".into())),
        );
        let id = "11111111-1111-4111-8111-111111111111";
        let mut cases = Vec::new();
        for method in [
            "call.realtime.prepare",
            "call.realtime.answer",
            "call.realtime.stop",
        ] {
            let params = match method {
                "call.realtime.prepare" => {
                    format!("{{\"callId\":\"{id}\",\"generation\":0,\"queueMs\":80}}")
                }
                "call.realtime.answer" => {
                    format!("{{\"callId\":\"{id}\",\"generation\":0,\"sdp\":\"answer\"}}")
                }
                _ => format!("{{\"callId\":\"{id}\",\"generation\":0}}"),
            };
            for (name, raw) in [
                ("valid", params.clone()),
                (
                    "fraction",
                    params.replace("\"generation\":0", "\"generation\":0.0"),
                ),
                (
                    "exponent",
                    params.replace("\"generation\":0", "\"generation\":0e0"),
                ),
                (
                    "overflow",
                    params.replace("\"generation\":0", "\"generation\":2147483648"),
                ),
                ("callId-type", params.replace(&format!("\"{id}\""), "4")),
                ("callId-null", params.replace(&format!("\"{id}\""), "null")),
                ("extra", params.replacen('{', "{\"extra\":null,", 1)),
            ] {
                cases.push(
                serde_json::json!({"name":format!("{method}:{name}"),"method":method,"params":raw}),
            );
            }
            if method == "call.realtime.prepare" {
                for raw in [
                    params.replace("80", "80.0"),
                    params.replace("80", "80e0"),
                    params.replace("80", "null"),
                    params.replace("80", "-1"),
                    params.replacen('{', "{\"playbackPrebufferMs\":null,", 1),
                    params.replacen('{', "{\"playbackPrebufferMs\":1.0,", 1),
                ] {
                    cases.push(serde_json::json!({"name":format!("prepare-extra:{}",cases.len()),"method":method,"params":raw}));
                }
            }
            if method == "call.realtime.answer" {
                for raw in [
                    params.replace("\"answer\"", "null"),
                    params.replace("\"answer\"", "4"),
                ] {
                    cases.push(serde_json::json!({"name":format!("answer-extra:{}",cases.len()),"method":method,"params":raw}));
                }
            }
        }
        assert_eq!(cases.len(), 29);
        let mut expanded = Vec::new();
        for stage in ["unconfigured", "ready", "closed"] {
            for row in &cases {
                expanded.push(serde_json::json!({"name":format!("{stage}:{}",row["name"].as_str().unwrap()),"stage":stage,"method":row["method"],"params":row["params"]}));
            }
        }
        let cases = expanded;
        let configuration = r#"{"binding":{"address":"127.0.0.1","port":0,"transport":"udp","rtpPortRangeStart":57000,"rtpPortRangeEnd":57998},"codecs":{"preferences":["PCMA","PCMU"],"receiveReorderMs":20},"incomingPeers":[],"registration":null,"password":null}"#;
        let mut current = "unconfigured";
        let mut endpoint = None;
        let mut actual = Vec::new();
        for row in &cases {
            if current != row["stage"].as_str().unwrap() {
                current = row["stage"].as_str().unwrap();
                if current == "ready" {
                    let frame = format!(
                        "{{\"version\":1,\"epoch\":\"{id}\",\"requestId\":1,\"operationId\":\"{id}\",\"method\":\"configure\",\"params\":{configuration}}}"
                    );
                    let result = handler
                        .invoke(NativeRequest::parse(frame.as_bytes()).unwrap())
                        .await;
                    assert!(result.error_code().is_none());
                    endpoint = owner.observation().endpoint;
                } else {
                    owner.close().wait().await.unwrap();
                }
            }
            let frame = format!(
                "{{\"version\":1,\"epoch\":\"{id}\",\"requestId\":1,\"operationId\":\"{id}\",\"method\":\"{}\",\"params\":{}}}",
                row["method"].as_str().unwrap(),
                row["params"].as_str().unwrap()
            );
            let outcome = handler
                .start(NativeRequest::parse(frame.as_bytes()).unwrap())
                .await
                .await;
            actual.push(
                serde_json::json!({"name":row["name"],"error":outcome.error_code().unwrap()}),
            );
        }
        assert_eq!(actual.len(), 87);
        owner.close().wait().await.unwrap();
        assert!(UdpSocket::bind(endpoint.unwrap()).is_ok());
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../fixtures/phone-realtime-command-cases.json"
        ))
        .unwrap();
        assert_eq!(serde_json::json!(actual), expected);
        trace(
            "route-command-dto",
            serde_json::json!({"expected":expected,"rust":actual,"callsAdmitted":0,"deviceRoutes":0}),
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn late_preparation_keeps_its_join_fence_after_observer_drop_and_revoke() {
        let entered = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let address = Arc::new(Mutex::new(None));
        let e = entered.clone();
        let r = release.clone();
        let a = address.clone();
        let port = CallAudioPort::new(
            "11111111-1111-4111-8111-111111111111".into(),
            Arc::new(NoDevices),
        )
        .unwrap()
        .test_realtime_factory(Arc::new(move |queue, prebuffer, permitted, _stop| {
            let e = e.clone();
            let r = r.clone();
            let a = a.clone();
            Box::pin(async move {
                // Deliberately delay returning an already opened route past
                // revoke, as an uncooperative device/native preparation can.
                let route = route::bound(
                    queue,
                    prebuffer,
                    permitted,
                    CancellationToken::new(),
                    vec!["127.0.0.1:0".parse().unwrap()],
                )
                .await?;
                *a.lock().unwrap() = Some(candidate(route.offer()));
                e.store(true, Ordering::Release);
                while !r.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                Ok(route)
            })
        }));
        let observer = port.prepare_realtime(0, 80, None).unwrap();
        drop(observer);
        tokio::time::timeout(Duration::from_secs(2), async {
            while !entered.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let address = address.lock().unwrap().unwrap();
        assert!(UdpSocket::bind(address).is_err());
        port.revoke();
        let closed = port.close();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), closed.clone().wait())
                .await
                .is_err()
        );
        assert!(!port.state().open);
        release.store(true, Ordering::Release);
        closed.wait().await.unwrap();
        assert!(UdpSocket::bind(address).is_ok());
        assert!(port.realtime_observation().unwrap().is_none());
        trace(
            "route-late-preparation",
            serde_json::json!({"observerDropped":true,"revokeBeforePublication":true,"closeWaited":true,"lateRouteJoined":true}),
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cleanup_uncertainty_is_sticky_and_never_retries_native_destruction() {
        struct Fault {
            socket: Sockets,
            retired: Arc<AtomicUsize>,
        }
        impl Drop for Fault {
            fn drop(&mut self) {
                self.retired.fetch_add(1, Ordering::AcqRel);
                panic!("scripted cleanup uncertainty");
            }
        }
        impl Datagrams for Fault {
            fn candidates(&self) -> Vec<SocketAddr> {
                self.socket.candidates()
            }
            fn receive(&mut self, _: &mut [u8]) -> Result<Option<(usize, SocketAddr, SocketAddr)>> {
                Err(RealtimeError::Failed)
            }
            fn send(&mut self, p: Transmit) -> Result<()> {
                self.socket.send(p)
            }
        }
        let retired = Arc::new(AtomicUsize::new(0));
        let r = retired.clone();
        let port = CallAudioPort::new(
            "11111111-1111-4111-8111-111111111111".into(),
            Arc::new(NoDevices),
        )
        .unwrap()
        .test_realtime_factory(Arc::new(move |queue, prebuffer, permitted, stop| {
            let r = r.clone();
            route::open_with(queue, prebuffer, permitted, stop, move || {
                Ok(Box::new(Fault {
                    socket: Sockets::bind(vec!["127.0.0.1:0".parse().unwrap()])?,
                    retired: r,
                }) as Box<dyn Datagrams>)
            })
        }));
        port.prepare_realtime(0, 80, None)
            .unwrap()
            .wait()
            .await
            .unwrap();
        let stopped = port.stop_realtime(0).unwrap();
        drop(stopped.clone());
        assert_eq!(
            stopped.wait().await,
            Err(CallAudioError::CleanupUnconfirmed)
        );
        assert_eq!(
            port.close().wait().await,
            Err(CallAudioError::CleanupUnconfirmed)
        );
        assert_eq!(
            port.close().wait().await,
            Err(CallAudioError::CleanupUnconfirmed)
        );
        assert!(matches!(
            port.prepare_realtime(1, 80, None),
            Err(RealtimeError::NotReady)
        ));
        assert_eq!(retired.load(Ordering::Acquire), 1);
        trace(
            "route-cleanup-uncertain",
            serde_json::json!({"destructions":1,"releaseConfirmed":false,"fenceRetained":true,"automaticRetry":false}),
        );
    }
}
