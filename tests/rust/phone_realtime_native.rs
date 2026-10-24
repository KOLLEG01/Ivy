use super::*;
use crate::{
    dtmf::InputSource,
    media::{Codec, G711Codec, OpusDecoder, RtpHeader, RtpPacket, SipReceiveEvents},
    native_pipe::{NativeRuntime, NativeShutdown},
    native_rpc::{NativeDispatcher, NativeHandler, NativeInvocation, NativeOutcome, NativeRequest},
    native_sip_commands::NativeSipCommands,
    native_sip_media::NativeSipSessionFactory,
};
use rsipstack::sip::{Method, Request, Response, prelude::*};
use serde_json::{Value, json};
use std::sync::atomic::AtomicU64;
use tokio::net::UdpSocket as SipSocket;

const CALL: &str = "11111111-1111-4111-8111-111111111111";
const CONFIG: &str = r#"{"binding":{"address":"127.0.0.1","port":0,"transport":"udp","rtpPortRangeStart":32000,"rtpPortRangeEnd":32998},"codecs":{"preferences":["PCMA"],"receiveReorderMs":20},"incomingPeers":["127.0.0.1"],"registration":null,"password":null}"#;

#[cfg(target_os = "linux")]
use crate::media::LinuxRtcpRuntime as SystemRtcpRuntime;
#[cfg(windows)]
use crate::media::WindowsRtcpRuntime as SystemRtcpRuntime;

#[derive(Default)]
struct Resources {
    opened: AtomicUsize,
    disposed: AtomicUsize,
    addresses: Mutex<Vec<SocketAddr>>,
}
struct Network(Sockets, Arc<Resources>);
impl Datagrams for Network {
    fn candidates(&self) -> Vec<SocketAddr> {
        self.0.candidates()
    }
    fn receive(&mut self, output: &mut [u8]) -> Result<Option<(usize, SocketAddr, SocketAddr)>> {
        self.0.receive(output)
    }
    fn send(&mut self, packet: Transmit) -> Result<()> {
        self.0.send(packet)
    }
}
impl Drop for Network {
    fn drop(&mut self) {
        self.1.disposed.fetch_add(1, Ordering::SeqCst);
    }
}
struct Events;
impl SipReceiveEvents for Events {
    fn digit(&self, _: char, _: InputSource) {}
    fn failed(&self) {
        panic!("owned SIP media unexpectedly failed")
    }
    fn rtcp(&self, _: SocketAddr, _: &[u8]) {}
}
#[derive(Default)]
struct Remaining(AtomicUsize);
impl NativeHandler for Remaining {
    fn invoke(&self, request: NativeRequest) -> NativeInvocation {
        panic!("command escaped its actual owner: {}", request.method())
    }
    fn cleanup_slots(&self) -> usize {
        4
    }
}
impl NativeRuntime for Remaining {
    fn shutdown(&self) -> NativeShutdown {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}
struct Native {
    rpc: NativeDispatcher,
    commands: Arc<NativeSipCommands>,
    resources: Arc<Resources>,
    remaining: Arc<Remaining>,
    sequence: AtomicU64,
    endpoint: SocketAddr,
}
impl Native {
    async fn new() -> Self {
        let resources = Arc::new(Resources::default());
        let opened = resources.clone();
        // Use the real media factory and the platform's command composition.
        // Only candidate selection/binding is redirected to owned loopback.
        let factory = NativeSipSessionFactory::new(
            Arc::new(NoDevices),
            None,
            Arc::new(SystemRtcpRuntime),
            |_| Arc::new(Events),
        )
        .test_realtime_factory(Arc::new(move |queue, prebuffer, permit, stop| {
            let opened = opened.clone();
            route::open_with(queue, prebuffer, permit, stop, move || {
                let sockets = Sockets::bind(vec!["127.0.0.1:0".parse().unwrap()])?;
                opened
                    .addresses
                    .lock()
                    .unwrap()
                    .extend(sockets.candidates());
                opened.opened.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(Network(sockets, opened)) as Box<dyn Datagrams>)
            })
        }));
        let remaining = Arc::new(Remaining::default());
        #[cfg(windows)]
        let commands =
            NativeSipCommands::windows(Arc::new(factory), remaining.clone(), |_| Ok("null".into()));
        #[cfg(target_os = "linux")]
        let commands = NativeSipCommands::new(
            crate::native_sip::NativeSipOperations::new(Arc::new(factory)),
            remaining.clone(),
            |_| Box::pin(async { Ok(()) }),
            crate::native_sip_status::NativeSipViews::new(
                || Ok("null".into()),
                |call| {
                    call.media()
                        .unwrap()
                        .call_audio()
                        .realtime_observation()
                        .map(|value| value.unwrap_or_else(|| "null".into()))
                        .map_err(|_| crate::sip::SipHostError::CleanupUnconfirmed.into())
                },
            ),
        );
        let commands = Arc::new(commands);
        let mut native = Self {
            rpc: NativeDispatcher::new(CALL.into(), 128, commands.clone()).unwrap(),
            commands,
            resources,
            remaining,
            sequence: AtomicU64::new(1),
            endpoint: "127.0.0.1:0".parse().unwrap(),
        };
        native
            .ok("configure", serde_json::from_str(CONFIG).unwrap())
            .await;
        native.endpoint = native.status(None).await["endpoint"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        native
    }
    fn request(&self, method: &str, params: Value, operation: Option<u64>) -> NativeRequest {
        let sequence = self.sequence.fetch_add(1, Ordering::SeqCst);
        let operation = operation.or_else(|| (method != "status").then_some(sequence));
        NativeRequest::parse(
            json!({"version":1,"epoch":CALL,"requestId":sequence,
            "operationId":operation.map(|n|format!("22222222-2222-4222-8222-{n:012}")),
            "method":method,"params":params})
            .to_string()
            .as_bytes(),
        )
        .unwrap()
    }
    async fn invoke(&self, method: &str, params: Value, operation: Option<u64>) -> NativeOutcome {
        tokio::time::timeout(
            Duration::from_secs(8),
            self.rpc.execute(self.request(method, params, operation)),
        )
        .await
        .expect("native command deadline")
        .outcome()
        .clone()
    }
    async fn ok(&self, method: &str, params: Value) -> Value {
        value(self.invoke(method, params, None).await)
    }
    async fn status(&self, id: Option<&str>) -> Value {
        self.ok(
            "status",
            id.map_or_else(|| json!({}), |id| json!({"callId":id})),
        )
        .await
    }
    async fn wait_status(&self, id: &str, ready: impl Fn(&Value) -> bool) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let status = self.status(Some(id)).await;
                if ready(&status) {
                    return status;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("native status deadline")
    }
    async fn prepare(&self, id: &str, generation: i32) -> Value {
        self.ok(
            "call.realtime.prepare",
            json!({"callId":id,"generation":generation,"queueMs":80,"playbackPrebufferMs":20}),
        )
        .await
    }
    async fn connect(
        &self,
        id: &str,
        generation: i32,
        offer: &str,
        operation: u64,
        abandon_observer: bool,
    ) -> Remote {
        let (mut remote, answer) = Remote::answer(offer);
        let params = json!({"callId":id,"generation":generation,"sdp":answer});
        let observer = self
            .rpc
            .begin(self.request("call.realtime.answer", params.clone(), Some(operation)))
            .await;
        if abandon_observer {
            drop(observer);
        } else {
            let (reply, ()) =
                tokio::join!(observer, remote.until(|r| r.rtc.is_connected() && r.events));
            assert_eq!(reply.outcome().error_code(), None);
        }
        remote.until(|r| r.rtc.is_connected() && r.events).await;
        let first = self
            .invoke("call.realtime.answer", params.clone(), Some(operation))
            .await;
        assert_eq!(value(first.clone()), json!({"callId":id,"connected":true}));
        assert_eq!(
            self.invoke("call.realtime.answer", params, Some(operation))
                .await,
            first
        );
        let status = self.status(Some(id)).await;
        assert_eq!(status["call"]["state"], "connected");
        assert_eq!(
            status["media"]["diagnostics"]["realtime"]["state"],
            "connected"
        );
        remote
    }
    async fn release(&self, id: &str, peer: &SipSocket) -> Value {
        let (released, ()) = tokio::join!(self.ok("call.release", json!({"callId":id})), bye(peer));
        assert_eq!(released, json!({"callId":id,"released":true}));
        assert!(
            self.status(None).await["callIds"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        released
    }
    async fn close(&self) {
        drop(self.commands.shutdown());
        self.commands.shutdown().await.unwrap();
        self.commands.shutdown().await.unwrap();
        assert_eq!(self.remaining.0.load(Ordering::SeqCst), 1);
        assert!(UdpSocket::bind(self.endpoint).is_ok());
        assert_eq!(
            self.resources.opened.load(Ordering::SeqCst),
            self.resources.disposed.load(Ordering::SeqCst)
        );
        for address in self.resources.addresses.lock().unwrap().iter() {
            assert!(
                UdpSocket::bind(address).is_ok(),
                "driver must join before native completion: {address}"
            );
        }
    }
}
fn value(outcome: NativeOutcome) -> Value {
    assert_eq!(outcome.error_code(), None, "{outcome:?}");
    serde_json::from_str(outcome.result_json().unwrap()).unwrap()
}
fn pcma() -> G711Codec {
    G711Codec::new(&Codec::Pcma.offered_formats()[0]).unwrap()
}
async fn pair() -> (SipSocket, SipSocket) {
    for _ in 0..64 {
        let first = SipSocket::bind("127.0.0.1:0").await.unwrap();
        let mut address = first.local_addr().unwrap();
        if address.port() % 2 != 0 || address.port() == 65535 {
            continue;
        }
        address.set_port(address.port() + 1);
        if let Ok(second) = SipSocket::bind(address).await {
            return (first, second);
        }
    }
    panic!("owned RTP pair unavailable")
}
async fn bytes(peer: &SipSocket) -> (String, SocketAddr) {
    let mut buffer = [0u8; 65536];
    let (n, source) = tokio::time::timeout(Duration::from_secs(5), peer.recv_from(&mut buffer))
        .await
        .expect("SIP peer deadline")
        .unwrap();
    (String::from_utf8(buffer[..n].to_vec()).unwrap(), source)
}
fn response(request: &Request, peer: SocketAddr, sdp: &str) -> String {
    let to = request.to_header().unwrap().value();
    let to = if to.contains(";tag=") {
        to.to_owned()
    } else {
        format!("{to};tag=owned-digital")
    };
    format!(
        "SIP/2.0 200 Fixture\r\nVia: {}\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {}\r\nContact: <sip:peer@{}>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{}",
        request.top_via_header().unwrap().value(),
        request.from_header().unwrap().value(),
        to,
        request.call_id_header().unwrap().value(),
        request.cseq_header().unwrap().value(),
        peer,
        sdp.len(),
        sdp
    )
}
fn sdp(port: u16) -> String {
    format!(
        "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {port} RTP/AVP 8 101\r\na=rtpmap:8 PCMA/8000\r\na=rtpmap:101 telephone-event/8000\r\na=sendrecv\r\n"
    )
}
async fn outgoing(native: &Native) -> (SipSocket, (SipSocket, SipSocket), SocketAddr) {
    native.ok("call.prepare", json!({"callId":CALL})).await;
    native
        .ok("call.features", json!({"callId":CALL,"challenge":null}))
        .await;
    let peer = SipSocket::bind("127.0.0.1:0").await.unwrap();
    let media = pair().await;
    let accept = async {
        let (raw, source) = bytes(&peer).await;
        let invite = Request::try_from(raw).unwrap();
        assert_eq!(invite.method, Method::Invite);
        assert_eq!(source, native.endpoint);
        peer.send_to(
            response(
                &invite,
                peer.local_addr().unwrap(),
                &sdp(media.0.local_addr().unwrap().port()),
            )
            .as_bytes(),
            source,
        )
        .await
        .unwrap();
        assert_eq!(
            Request::try_from(bytes(&peer).await.0).unwrap().method,
            Method::Ack
        );
    };
    let (result,())=tokio::join!(native.ok("call.dial",json!({"callId":CALL,"destination":format!("sip:peer@{}",peer.local_addr().unwrap()),"username":"fixture-user","password":null,"ringSeconds":5,"waiting":false})),accept);
    assert_eq!(result["state"], "connected");
    let status = native.status(Some(CALL)).await;
    assert_eq!(status["call"]["features"]["access"], "trusted");
    let local = status["media"]["diagnostics"]["localRtp"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    (peer, media, local)
}
async fn bye(peer: &SipSocket) {
    let (raw, source) = bytes(peer).await;
    let request = Request::try_from(raw).unwrap();
    assert_eq!(request.method, Method::Bye);
    peer.send_to(
        response(&request, peer.local_addr().unwrap(), "").as_bytes(),
        source,
    )
    .await
    .unwrap();
}
async fn packet(
    peer: &SipSocket,
    target: SocketAddr,
    sequence: u16,
    timestamp: u32,
    payload_type: u8,
    payload: &[u8],
) {
    let mut buffer = vec![0; 12 + payload.len()];
    RtpHeader {
        marker: true,
        payload_type,
        sequence,
        timestamp,
        source: 0x13572468,
    }
    .write(payload, &mut buffer)
    .unwrap();
    peer.send_to(&buffer, target).await.unwrap();
}
fn media_released(address: SocketAddr) {
    assert!(UdpSocket::bind(address).is_ok());
    let rtcp = SocketAddr::new(address.ip(), address.port() + 1);
    assert!(UdpSocket::bind(rtcp).is_ok());
}
async fn duplex(
    native: &Native,
    id: &str,
    peer: &SipSocket,
    local: SocketAddr,
    remote: &mut Remote,
) -> Value {
    let pcm = std::array::from_fn(|i| ((i as f32 * 0.27).sin() * 14000.0) as i16);
    let encoded = pcma().encode_20ms(&pcm);
    let mut opus =
        OpusDecoder::new(&AudioFormat::new(111, "opus", 48000, 48000, 2, None).unwrap()).unwrap();
    let mut sip_energy = 0i16;
    let mut digital_energy = 0i16;
    let mut seen = remote.packets.len();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut sequence = 100;
    while sip_energy < 1000 || digital_energy < 1000 {
        assert!(
            Instant::now() < deadline,
            "both decoded directions must carry nonsilent audio: SIP={sip_energy}, digital={digital_energy}"
        );
        packet(peer, local, sequence, sequence as u32 * 160, 8, &encoded).await;
        remote.audio(sequence as u64);
        for _ in 0..10 {
            remote.step();
            let mut buffer = [0u8; 2048];
            for _ in 0..32 {
                match peer.try_recv_from(&mut buffer) {
                    Ok((length, source)) => {
                        assert_eq!(source, local);
                        let packet = RtpPacket::parse(&buffer[..length]).unwrap();
                        assert_eq!(packet.header.payload_type, 8);
                        sip_energy = sip_energy.max(
                            pcma()
                                .decode(packet.payload)
                                .unwrap()
                                .into_iter()
                                .map(|n| n.saturating_abs())
                                .max()
                                .unwrap(),
                        );
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => panic!("{e}"),
                }
            }
            for packet in &remote.packets[seen..] {
                digital_energy = digital_energy.max(
                    opus.decode(packet, 960, false)
                        .unwrap()
                        .into_iter()
                        .map(|n| n.saturating_abs())
                        .max()
                        .unwrap(),
                );
            }
            seen = remote.packets.len();
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        sequence += 1;
    }
    let status = native.status(Some(id)).await;
    let realtime = &status["media"]["diagnostics"]["realtime"];
    assert!(realtime["sentPackets"].as_u64().unwrap() > 0);
    assert!(realtime["receivedPackets"].as_u64().unwrap() > 0);
    assert!(status["media"]["sentPackets"].as_u64().unwrap() > 0);
    assert!(status["media"]["receivedPackets"].as_u64().unwrap() > 0);
    json!({"sipDecodedPeak":sip_energy,"digitalDecodedPeak":digital_energy,"sentPackets":status["media"]["sentPackets"],"receivedPackets":status["media"]["receivedPackets"],"realtime":realtime})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_native_outgoing_duplex_generation_replay_and_release() {
    let native = Native::new().await;
    let (peer, media, local) = outgoing(&native).await;
    let params = json!({"callId":CALL,"generation":0,"queueMs":80,"playbackPrebufferMs":20});
    drop(
        native
            .rpc
            .begin(native.request("call.realtime.prepare", params.clone(), Some(1000)))
            .await,
    );
    let prepared = value(
        native
            .invoke("call.realtime.prepare", params.clone(), Some(1000))
            .await,
    );
    assert_eq!(
        prepared,
        value(
            native
                .invoke("call.realtime.prepare", params, Some(1000))
                .await
        )
    );
    assert_eq!(native.resources.opened.load(Ordering::SeqCst), 1);
    let offer = prepared["sdp"].as_str().unwrap();
    let first = candidate(offer);
    let mut remote = native.connect(CALL, 0, offer, 1001, true).await;
    let audio = duplex(&native, CALL, &media.0, local, &mut remote).await;
    assert_eq!(
        native
            .ok("call.realtime.stop", json!({"callId":CALL,"generation":0}))
            .await,
        json!({"callId":CALL,"stopped":true})
    );
    assert!(UdpSocket::bind(first).is_ok());
    assert_eq!(native.resources.disposed.load(Ordering::SeqCst), 1);
    assert!(native.status(Some(CALL)).await["media"]["diagnostics"]["realtime"].is_null());
    let next = native.prepare(CALL, 1).await;
    let second = candidate(next["sdp"].as_str().unwrap());
    assert!(UdpSocket::bind(second).is_err());
    let stale = native
        .invoke(
            "call.realtime.stop",
            json!({"callId":CALL,"generation":0}),
            None,
        )
        .await;
    assert_eq!(stale.error_code(), Some("runtime_not_ready"));
    assert!(UdpSocket::bind(second).is_err());
    // Replaying old success cannot revive a retired driver or affect generation1.
    assert_eq!(
        prepared,
        value(
            native
                .invoke(
                    "call.realtime.prepare",
                    json!({"callId":CALL,"generation":0,"queueMs":80,"playbackPrebufferMs":20}),
                    Some(1000)
                )
                .await
        )
    );
    native.release(CALL, &peer).await;
    media_released(local);
    assert!(UdpSocket::bind(second).is_ok());
    native.close().await;
    trace(
        "native-outgoing-duplex",
        json!({"audio":audio,"defaultComposition":true,"abandonedPrepareAndAnswerObservers":true,"retiredGenerations":2,"replayPreserved":true,"allSocketsReleasedAfterJoins":true}),
    );
}

fn incoming(method: &str, tag: &str, source: SocketAddr, target: SocketAddr, body: &str) -> String {
    format!(
        "{method} sip:fixture@{target} SIP/2.0\r\nVia: SIP/2.0/UDP {source};rport;branch=z9hG4bK-owned-digital-{method}\r\nFrom: <sip:caller@{source}>;tag=caller-tag\r\nTo: <sip:fixture@{target}>{}\r\nCall-ID: owned-native-digital\r\nCSeq: 1 {method}\r\nContact: <sip:caller@{source}>\r\nMax-Forwards: 70\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}",
        if tag.is_empty() {
            String::new()
        } else {
            format!(";tag={tag}")
        },
        body.len()
    )
}
async fn final_response(peer: &SipSocket) -> Response {
    loop {
        let response = Response::try_from(bytes(peer).await.0).unwrap();
        if response.status_code.code() != 100 {
            return response;
        }
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_native_incoming_requires_actual_rfc4733_access_authentication() {
    let native = Native::new().await;
    let peer = SipSocket::bind("127.0.0.1:0").await.unwrap();
    let media = pair().await;
    peer.send_to(
        incoming(
            "INVITE",
            "",
            peer.local_addr().unwrap(),
            native.endpoint,
            &sdp(media.0.local_addr().unwrap().port()),
        )
        .as_bytes(),
        native.endpoint,
    )
    .await
    .unwrap();
    assert_eq!(final_response(&peer).await.status_code.code(), 180);
    let status = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let v = native.status(None).await;
            if !v["callIds"].as_array().unwrap().is_empty() {
                break v;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let id = status["callIds"][0].as_str().unwrap();
    native.ok("call.claim", json!({"callId":id})).await;
    native
        .ok(
            "call.features",
            json!({"callId":id,"challenge":{"code":"*123#","failureResponseDelayMilliseconds":0}}),
        )
        .await;
    let (answer, final_sip) = tokio::join!(
        native.ok("call.answer", json!({"callId":id,"waiting":false})),
        final_response(&peer)
    );
    assert_eq!(answer["state"], "connected");
    assert_eq!(final_sip.status_code.code(), 200);
    let tag = final_sip
        .to_header()
        .unwrap()
        .tag()
        .unwrap()
        .unwrap()
        .value()
        .to_owned();
    peer.send_to(
        incoming("ACK", &tag, peer.local_addr().unwrap(), native.endpoint, "").as_bytes(),
        native.endpoint,
    )
    .await
    .unwrap();
    let status = native.status(Some(id)).await;
    assert_eq!(status["call"]["features"]["access"], "awaiting_code");
    let local: SocketAddr = status["media"]["diagnostics"]["localRtp"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let prepared = native.prepare(id, 0).await;
    let (mut remote, answer) = Remote::answer(prepared["sdp"].as_str().unwrap());
    let params = json!({"callId":id,"generation":0,"sdp":answer});
    assert_eq!(
        native
            .invoke("call.realtime.answer", params.clone(), Some(2000))
            .await
            .error_code(),
        Some("runtime_not_ready")
    );
    assert_eq!(
        native.status(Some(id)).await["media"]["diagnostics"]["realtime"]["state"],
        "new"
    );
    // Access digits traverse actual UDP/RTP negotiation and the original call's
    // installed feature callback. No test directly grants audio authority.
    for (i, digit) in [10u8, 1, 2, 3, 11].into_iter().enumerate() {
        for repeat in 0..3 {
            packet(
                &media.0,
                local,
                (i * 3 + repeat) as u16,
                (i as u32 + 1) * 800,
                101,
                &[digit, 0x8a, 1, 64],
            )
            .await;
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    let authenticated = native
        .wait_status(id, |s| s["call"]["features"]["access"] == "authenticated")
        .await;
    assert_eq!(
        authenticated["media"]["diagnostics"]["dtmf"]["rfc4733DetectedDigits"],
        5
    );
    // The old denial is a saved receipt even after authentication.
    assert_eq!(
        native
            .invoke("call.realtime.answer", params.clone(), Some(2000))
            .await
            .error_code(),
        Some("runtime_not_ready")
    );
    let work = native.invoke("call.realtime.answer", params, Some(2001));
    let (accepted, ()) = tokio::join!(work, remote.until(|r| r.rtc.is_connected() && r.events));
    assert_eq!(value(accepted), json!({"callId":id,"connected":true}));
    let audio = duplex(&native, id, &media.0, local, &mut remote).await;
    native.release(id, &peer).await;
    media_released(local);
    native.close().await;
    trace(
        "native-incoming-authenticated",
        json!({"access":"authenticated","actualRfc4733Digits":5,"oldDenialReplayed":true,"audio":audio,"defaultComposition":true,"allSocketsReleasedAfterJoins":true}),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_native_pending_answer_release_retains_receipt_across_id_reuse() {
    let native = Native::new().await;
    let (peer, _media, local) = outgoing(&native).await;
    let prepared = native.prepare(CALL, 0).await;
    let offer = prepared["sdp"].as_str().unwrap();
    let old = candidate(offer);
    let (_remote, answer) = Remote::answer(offer);
    let params = json!({"callId":CALL,"generation":0,"sdp":answer});
    // No peer pumping: the original driver is admitted but still connecting.
    drop(
        native
            .rpc
            .begin(native.request("call.realtime.answer", params.clone(), Some(3000)))
            .await,
    );
    native
        .wait_status(CALL, |s| {
            s["media"]["diagnostics"]["realtime"]["state"] == "connecting"
        })
        .await;
    native.release(CALL, &peer).await;
    assert!(UdpSocket::bind(old).is_ok());
    media_released(local);
    let retired = native
        .invoke("call.realtime.answer", params.clone(), Some(3000))
        .await;
    assert!(retired.error_code().is_some());
    native.ok("call.prepare", json!({"callId":CALL})).await;
    let replacement = native.prepare(CALL, 0).await;
    let new = candidate(replacement["sdp"].as_str().unwrap());
    assert_eq!(
        retired,
        native
            .invoke("call.realtime.answer", params, Some(3000))
            .await
    );
    assert_eq!(
        native.status(Some(CALL)).await["media"]["diagnostics"]["realtime"]["state"],
        "new"
    );
    assert!(UdpSocket::bind(new).is_err());
    // The replacement never dialed; shutdown still disposes it and its driver.
    native.close().await;
    assert!(UdpSocket::bind(new).is_ok());
    trace(
        "native-pending-answer-release",
        json!({"retiredError":retired.error_code(),"sameIdReplacementUnaffected":true,"retainedCloseAndShutdown":true,"driversJoined":2,"defaultComposition":true}),
    );
}
