use super::*;
use crate::{
    dtmf::InputSource,
    media::{
        AudioNegotiator, Codec, MediaError, RtcpRuntime, RtcpWallTime, RtpPacket, RtpPeer,
        SdpDocument, SdpType, SipReceiveEvents, SipRtpReceiver,
    },
};
use ivy_contracts::{IvyJson, json::stringify};
use std::{
    net::SocketAddr,
    sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
};
use tokio::{
    net::UdpSocket,
    time::{sleep, timeout},
};

#[derive(Default)]
struct Gate {
    reached: AtomicBool,
    released: Mutex<bool>,
    changed: Condvar,
}
impl Gate {
    fn hit(&self) {
        self.reached.store(true, Ordering::SeqCst);
        let guard = self.released.lock().unwrap();
        let (guard, _) = self
            .changed
            .wait_timeout_while(guard, Duration::from_secs(5), |r| !*r)
            .unwrap();
        assert!(*guard, "Synthetic race gate was not released");
    }
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
    async fn wait(&self) {
        wait(|| self.reached.load(Ordering::SeqCst)).await;
    }
}
struct Audio {
    state: Mutex<PcmAudioState>,
    reads: AtomicUsize,
    health: AtomicU8,
    health_reads: AtomicUsize,
    health_gate: Mutex<Option<Arc<Gate>>>,
    read_gate: Mutex<Option<Arc<Gate>>>,
}
impl Audio {
    fn new(open: bool) -> Self {
        Self {
            state: Mutex::new(PcmAudioState {
                open,
                generation: 1,
            }),
            reads: AtomicUsize::new(0),
            health: AtomicU8::new(0),
            health_reads: AtomicUsize::new(0),
            health_gate: Mutex::new(None),
            read_gate: Mutex::new(None),
        }
    }
    fn change(&self, open: bool) {
        let mut state = self.state.lock().unwrap();
        state.generation += 1;
        state.open = open;
    }
}
impl PcmAudioPort for Audio {
    fn state(&self) -> PcmAudioState {
        *self.state.lock().unwrap()
    }
    fn read_captured(&self, out: &mut [f32], generation: u64) -> usize {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let state = self.state();
        let current = state.open && state.generation == generation;
        out.fill(if current { 0.5 } else { 0.0 });
        let gate = self.read_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.hit();
        }
        if current { out.len() } else { 0 }
    }
    fn write_received(&self, _: &[f32], _: u64) -> bool {
        false
    }
}
impl PcmSendPort for Audio {
    fn with_send_state(&self, admitted: PcmAudioState, action: &mut dyn FnMut()) -> bool {
        let state = self.state.lock().unwrap();
        if *state != admitted {
            return false;
        }
        action();
        drop(state);
        true
    }
    fn send_health(&self) -> AudioSendHealth {
        self.health_reads.fetch_add(1, Ordering::SeqCst);
        let gate = self.health_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.hit();
        }
        match self.health.load(Ordering::SeqCst) {
            0 => AudioSendHealth::Ready,
            1 => AudioSendHealth::RepairPending,
            _ => AudioSendHealth::Failed,
        }
    }
}
#[derive(Default)]
struct Events(AtomicUsize);
impl SipReceiveEvents for Events {
    fn digit(&self, _: char, _: InputSource) {}
    fn failed(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
    fn rtcp(&self, _: SocketAddr, _: &[u8]) {}
}
struct Clock;
impl RtcpRuntime for Clock {
    fn now(&self) -> Result<RtcpWallTime, MediaError> {
        #[cfg(windows)]
        {
            crate::media::WindowsRtcpRuntime.now()
        }
        #[cfg(not(windows))]
        {
            RtcpWallTime::new(RtcpWallTime::UNIX_TICKS + 100_000_000, 0)
        }
    }
    fn interval_ms(&self) -> Result<u16, MediaError> {
        Ok(2500)
    }
}
async fn wait(mut ready: impl FnMut() -> bool) {
    timeout(Duration::from_secs(5), async {
        while !ready() {
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}
fn trace(name: &str, value: IvyJson) {
    if let Some(root) = std::env::var_os("IVY_PHONE_SENDER_TRACE_DIR") {
        std::fs::write(
            std::path::PathBuf::from(root).join(format!("{name}.json")),
            stringify(&value, 4_000_000).unwrap(),
        )
        .unwrap();
    }
}
fn count(value: u64) -> IvyJson {
    IvyJson::Number(value as f64)
}
async fn setup(
    open: bool,
) -> (
    SipRtpReceiver,
    Arc<Audio>,
    Arc<Events>,
    UdpSocket,
    UdpSocket,
) {
    let audio = Arc::new(Audio::new(open));
    let events = Arc::new(Events::default());
    let receiver = SipRtpReceiver::bind(
        "127.0.0.1:0".parse().unwrap(),
        52000,
        53998,
        20,
        audio.clone(),
        None,
        events.clone(),
    )
    .unwrap();
    receiver
        .enable_reporting(17, "fixture", Arc::new(Clock))
        .unwrap();
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let control = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    (receiver, audio, events, peer, control)
}
fn configure(receiver: &SipRtpReceiver, peer: &UdpSocket, control: &UdpSocket, codec: Codec) {
    let (payload, rtpmap) = match codec {
        Codec::Pcmu => (0, "PCMU/8000"),
        Codec::Pcma => (8, "PCMA/8000"),
        Codec::G722 => (9, "G722/8000"),
        Codec::Opus => (111, "opus/48000/2"),
        _ => unreachable!(),
    };
    let offer = SdpDocument::parse(&format!("v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {} RTP/AVP {payload}\r\na=rtpmap:{payload} {rtpmap}\r\na=sendrecv\r\n", peer.local_addr().unwrap().port())).unwrap();
    let mut negotiation = AudioNegotiator::new(&[codec]).unwrap();
    negotiation
        .set_remote_description(SdpType::Offer, &offer)
        .unwrap();
    receiver
        .configure(
            negotiation.state().unwrap(),
            RtpPeer::new(peer.local_addr().unwrap(), control.local_addr().unwrap()).unwrap(),
        )
        .unwrap();
}
fn hook(receiver: &SipRtpReceiver, hook: SendHook) {
    receiver.sender_state().state.lock().unwrap().hook = Some(hook);
}
async fn packet(peer: &UdpSocket) -> (Vec<u8>, SocketAddr) {
    let mut wire = vec![0; 9000];
    let (size, source) = timeout(Duration::from_secs(2), peer.recv_from(&mut wire))
        .await
        .unwrap()
        .unwrap();
    wire.truncate(size);
    (wire, source)
}
fn counters(receiver: &SipRtpReceiver) -> (u64, u16, u32, u32, u32, u64) {
    let state = receiver.sender_state().state.lock().unwrap();
    let frames = state.frames.as_ref().unwrap();
    let observation = receiver.observation();
    let rtcp = observation.rtcp_session.unwrap().sent;
    (
        state.sent,
        frames.sequence(),
        frames.timestamp(),
        rtcp.packets,
        rtcp.octets,
        observation.transport.counters.rtp_sent,
    )
}
async fn close(receiver: &SipRtpReceiver) {
    timeout(Duration::from_secs(3), receiver.close())
        .await
        .unwrap();
    assert!(receiver.observation().cleanup_confirmed);
    let state = receiver.sender_state().state.lock().unwrap();
    assert!(!state.started || state.completed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn obsolete_original_tokens_at_every_encode_and_dispatch_boundary_have_zero_effects() {
    let mut rows = Vec::new();
    for point in [
        SendPoint::Encode(EncodePoint::Admitted),
        SendPoint::Encode(EncodePoint::Captured),
        SendPoint::Encode(EncodePoint::Resampled),
        SendPoint::Encode(EncodePoint::Encoded),
        SendPoint::BeforeDispatch,
    ] {
        let (receiver, audio, _, peer, control) = setup(true).await;
        configure(&receiver, &peer, &control, Codec::G722);
        let first = Arc::new(Gate::default());
        let next = Arc::new(Gate::default());
        let seen = Arc::new(Mutex::new(Vec::new()));
        hook(&receiver, {
            let first = first.clone();
            let next = next.clone();
            let seen = seen.clone();
            Arc::new(move |at, token, _| {
                if first.reached.load(Ordering::SeqCst)
                    && at == SendPoint::Encode(EncodePoint::Admitted)
                {
                    next.hit();
                }
                if at == point && !first.reached.load(Ordering::SeqCst) {
                    seen.lock().unwrap().push(token);
                    first.hit();
                }
            })
        });
        receiver
            .start_sending(audio.clone(), 17, 65535, u32::MAX - 80)
            .unwrap();
        first.wait().await;
        audio.change(false);
        first.release();
        next.wait().await;
        assert_eq!(
            *seen.lock().unwrap(),
            vec![PcmAudioState {
                open: true,
                generation: 1
            }]
        );
        assert_eq!(
            counters(&receiver),
            (0, 65535, u32::MAX - 80, 0, 0, 0),
            "{point:?}"
        );
        assert!(
            timeout(Duration::from_millis(10), peer.recv(&mut [0; 9000]))
                .await
                .is_err()
        );
        // Closing must not wait on a captured/encoding frame or authorize it.
        receiver.request_close();
        next.release();
        close(&receiver).await;
        rows.push(IvyJson::object([
            ("boundary", format!("{point:?}").into()),
            ("admissionGeneration", count(1)),
            ("currentGeneration", count(audio.state().generation)),
            ("packets", count(0)),
            ("sequence", count(65535)),
            ("timestamp", count(u64::from(u32::MAX - 80))),
            ("closed", true.into()),
        ]));
    }
    trace("obsolete-tokens", IvyJson::Array(rows));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoke_during_actual_capture_drops_pcm_and_cancelled_close_observer_does_not_abandon_thread()
 {
    let (receiver, audio, _, peer, control) = setup(true).await;
    configure(&receiver, &peer, &control, Codec::Pcmu);
    let read = Arc::new(Gate::default());
    *audio.read_gate.lock().unwrap() = Some(read.clone());
    receiver.start_sending(audio.clone(), 17, 7, 77).unwrap();
    read.wait().await;
    audio.change(false);
    let waiter = tokio::spawn({
        let receiver = receiver.clone();
        async move { receiver.close().await }
    });
    wait(|| receiver.observation().transport.closed).await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert!(!receiver.observation().cleanup_confirmed);
    read.release();
    close(&receiver).await;
    assert_eq!(counters(&receiver), (0, 7, 77, 0, 0, 0));
    assert!(
        timeout(Duration::from_millis(10), peer.recv(&mut [0; 9000]))
            .await
            .is_err()
    );
    trace(
        "capture-close",
        IvyJson::object([
            ("captureReads", count(1)),
            ("obsoletePackets", count(0)),
            ("observerCancelled", true.into()),
            ("cleanupConfirmed", true.into()),
        ]),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn all_actual_udp_chunks_and_bookkeeping_finish_before_revoke_and_call_close_return() {
    let (receiver, audio, _, peer, control) = setup(true).await;
    configure(&receiver, &peer, &control, Codec::Pcmu);
    let chunk = Arc::new(Gate::default());
    let next = Arc::new(Gate::default());
    hook(&receiver, {
        let chunk = chunk.clone();
        let next = next.clone();
        Arc::new(move |point, _, packet| {
            if point == SendPoint::Encode(EncodePoint::Admitted)
                && chunk.reached.load(Ordering::SeqCst)
            {
                next.hit();
            }
            if point == SendPoint::BeforeDispatch {
                *packet = (0..8192).map(|i| (i % 251) as u8).collect();
            }
            if point == SendPoint::Chunk && !chunk.reached.load(Ordering::SeqCst) {
                chunk.hit();
            }
        })
    });
    receiver
        .start_sending(audio.clone(), 17, 65535, u32::MAX - 80)
        .unwrap();
    chunk.wait().await;
    let revoked = Arc::new(AtomicBool::new(false));
    let closed = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicUsize::new(0));
    let revoke = tokio::task::spawn_blocking({
        let audio = audio.clone();
        let revoked = revoked.clone();
        let started = started.clone();
        move || {
            started.fetch_add(1, Ordering::SeqCst);
            audio.change(false);
            revoked.store(true, Ordering::SeqCst);
        }
    });
    let stop = tokio::task::spawn_blocking({
        let receiver = receiver.clone();
        let closed = closed.clone();
        let started = started.clone();
        move || {
            started.fetch_add(1, Ordering::SeqCst);
            receiver.request_close();
            closed.store(true, Ordering::SeqCst);
        }
    });
    wait(|| started.load(Ordering::SeqCst) == 2).await;
    sleep(Duration::from_millis(20)).await;
    assert!(!revoked.load(Ordering::SeqCst));
    assert!(!closed.load(Ordering::SeqCst));
    chunk.release();
    timeout(Duration::from_secs(2), revoke)
        .await
        .unwrap()
        .unwrap();
    timeout(Duration::from_secs(2), stop)
        .await
        .unwrap()
        .unwrap();
    next.release();
    close(&receiver).await;
    let mut frames = RtpAudioFrames::new(17, 65535, u32::MAX - 80);
    let mut expected = Vec::new();
    frames
        .send(
            160,
            0,
            &(0..8192).map(|i| (i % 251) as u8).collect::<Vec<_>>(),
            |header, part| {
                let mut wire = vec![0; 12 + part.len()];
                header.write(part, &mut wire).unwrap();
                expected.push(wire);
                Ok(())
            },
        )
        .unwrap();
    for wire in &expected {
        let (actual, source) = packet(&peer).await;
        assert_eq!(&actual, wire);
        assert_eq!(
            source.port(),
            receiver.observation().transport.local_rtp.port()
        );
    }
    assert_eq!(counters(&receiver), (1, 6, 79, 7, 8192, 7));
    assert!(
        timeout(Duration::from_millis(20), peer.recv(&mut [0; 9000]))
            .await
            .is_err()
    );
    trace(
        "serialized-chunks",
        IvyJson::object([
            ("actualDatagrams", count(7)),
            ("octets", count(8192)),
            ("revokeWaitedForFrame", true.into()),
            ("closeWaitedForFrame", true.into()),
            ("postClosePackets", count(0)),
        ]),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_thread_waits_on_progressing_deadlines_then_sends_on_the_original_pair() {
    let (receiver, audio, _, peer, control) = setup(true).await;
    let deadlines = Arc::new(Mutex::new(Vec::new()));
    hook(&receiver, {
        let deadlines = deadlines.clone();
        Arc::new(move |point, _, _| {
            if let SendPoint::Due(next) = point {
                #[cfg(windows)]
                {
                    use windows_sys::Win32::System::Threading::{
                        GetCurrentThread, GetThreadPriority, THREAD_PRIORITY_HIGHEST,
                    };
                    assert_eq!(
                        unsafe { GetThreadPriority(GetCurrentThread()) },
                        THREAD_PRIORITY_HIGHEST
                    );
                }
                deadlines.lock().unwrap().push(next);
            }
        })
    });
    receiver.start_sending(audio.clone(), 17, 123, 456).unwrap();
    wait(|| deadlines.lock().unwrap().len() >= 5).await;
    assert_eq!(counters(&receiver), (0, 123, 456, 0, 0, 0));
    assert_eq!(audio.reads.load(Ordering::SeqCst), 0);
    {
        let values = deadlines.lock().unwrap();
        assert_eq!(values[0], Duration::from_millis(20));
        assert!(
            values
                .windows(2)
                .all(|p| p[1] >= p[0] + Duration::from_millis(20))
        );
    }
    configure(&receiver, &peer, &control, Codec::Pcmu);
    let (wire, source) = packet(&peer).await;
    let rtp = RtpPacket::parse(&wire).unwrap();
    assert_eq!((rtp.header.sequence, rtp.header.timestamp), (123, 456));
    assert_eq!(
        source.port(),
        receiver.observation().transport.local_rtp.port()
    );
    close(&receiver).await;
    trace(
        "idle-thread",
        IvyJson::object([
            (
                "deadlinesMicros",
                IvyJson::Array(
                    deadlines
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|d| count(d.as_micros() as u64))
                        .collect(),
                ),
            ),
            ("idlePackets", count(0)),
            ("idleCodecReads", count(0)),
            ("firstNegotiatedSequence", count(123)),
            ("cleanupConfirmed", true.into()),
        ]),
    );
}

#[test]
fn sender_clock_keeps_original_after_work_catchup_and_strict_suspension_boundary() {
    let ms = Duration::from_millis;
    for (done, next) in [(20, 40), (40, 40), (80, 40), (81, 101)] {
        let mut clock = RtpSendClock::new(ms(0)).unwrap();
        assert!(!clock.due(ms(19)).unwrap());
        assert!(clock.due(ms(20)).unwrap());
        clock.advance(ms(done)).unwrap();
        assert_eq!(clock.next(), ms(next));
    }
    let mut clock = RtpSendClock::new(ms(0)).unwrap();
    assert!(clock.due(ms(80)).unwrap());
    clock.advance(ms(80)).unwrap();
    assert!(clock.due(ms(80)).unwrap());
    clock.advance(ms(80)).unwrap();
    assert_eq!(clock.next(), ms(60));
    assert!(clock.due(ms(80)).unwrap());
    clock.advance(ms(80)).unwrap();
    assert_eq!(clock.next(), ms(80));
    assert!(clock.due(ms(80)).unwrap());
    clock.advance(ms(80)).unwrap();
    assert_eq!(clock.next(), ms(100));
    assert!(!clock.due(ms(80)).unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stable_closed_silence_and_independent_progress_authority_keep_original_frame_semantics() {
    let mut rows = Vec::new();
    for mode in ["open", "closed", "progress", "empty", "socket-error"] {
        let (receiver, route, _, peer, control) = setup(mode == "open").await;
        configure(&receiver, &peer, &control, Codec::Pcmu);
        let audio = if mode == "progress" {
            Arc::new(Audio::new(true))
        } else {
            route.clone()
        };
        let next = Arc::new(Gate::default());
        let frames = Arc::new(AtomicUsize::new(0));
        hook(&receiver, {
            let next = next.clone();
            let frames = frames.clone();
            Arc::new(move |point, _, packet| {
                if point == SendPoint::Encode(EncodePoint::Admitted)
                    && frames.fetch_add(1, Ordering::SeqCst) > 0
                {
                    next.hit();
                }
                if mode == "empty" && point == SendPoint::BeforeDispatch {
                    packet.clear();
                }
            })
        });
        if mode == "socket-error" {
            // An IPv4 socket cannot send to this synthetic IPv6 destination.
            // The original ignores socket I/O errors but counts the attempt.
            let mut negotiation = AudioNegotiator::new(&[Codec::Pcmu]).unwrap();
            let offer=SdpDocument::parse("v=0\r\no=- 1 1 IN IP6 ::1\r\ns=fixture\r\nc=IN IP6 ::1\r\nt=0 0\r\nm=audio 12345 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\na=sendrecv\r\n").unwrap();
            negotiation
                .set_remote_description(SdpType::Offer, &offer)
                .unwrap();
            receiver
                .configure(
                    negotiation.state().unwrap(),
                    RtpPeer::new(
                        "[::1]:12345".parse().unwrap(),
                        "[::1]:12346".parse().unwrap(),
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        receiver
            .start_sending(audio.clone(), 17, 65535, u32::MAX - 80)
            .unwrap();
        next.wait().await;
        let empty = mode == "empty";
        let io = mode == "socket-error";
        assert_eq!(
            counters(&receiver),
            (
                1,
                if empty { 65535 } else { 0 },
                79,
                if empty { 0 } else { 1 },
                if empty { 0 } else { 160 },
                if empty || io { 0 } else { 1 }
            )
        );
        if !empty && !io {
            let (wire, source) = packet(&peer).await;
            let parsed = RtpPacket::parse(&wire).unwrap();
            assert_eq!(
                (parsed.header.sequence, parsed.header.timestamp),
                (65535, u32::MAX - 80)
            );
            assert!(!parsed.header.marker);
            assert_eq!(
                source.port(),
                receiver.observation().transport.local_rtp.port()
            );
            assert_eq!(parsed.payload.iter().all(|b| *b == 255), mode == "closed");
        }
        let other = Arc::new(Audio::new(true));
        assert!(!receiver.start_sending(other.clone(), 17, 1, 1).unwrap());
        assert_eq!(other.reads.load(Ordering::SeqCst), 0);
        receiver.request_close();
        next.release();
        close(&receiver).await;
        rows.push(IvyJson::object([
            ("mode", mode.into()),
            ("frames", count(1)),
            ("rtcpPackets", count(if empty { 0 } else { 1 })),
            ("actualDatagrams", count(if empty || io { 0 } else { 1 })),
            ("timestamp", count(79)),
        ]));
    }
    trace("normal-progress-empty-error", IvyJson::Array(rows));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn predictive_history_resets_on_obsolete_frame_and_reopen_without_fresh_token_recapture() {
    let (receiver, audio, _, peer, control) = setup(true).await;
    configure(&receiver, &peer, &control, Codec::Opus);
    let next = Arc::new(Gate::default());
    let frame = Arc::new(AtomicUsize::new(0));
    let packets = Arc::new(Mutex::new(Vec::new()));
    hook(&receiver, {
        let audio = audio.clone();
        let next = next.clone();
        let frame = frame.clone();
        let packets = packets.clone();
        Arc::new(move |point, token, packet| {
            if point == SendPoint::Encode(EncodePoint::Admitted) {
                if frame.fetch_add(1, Ordering::SeqCst) >= 3 {
                    next.hit();
                }
            }
            if point == SendPoint::Encode(EncodePoint::Encoded) && frame.load(Ordering::SeqCst) == 2
            {
                assert_eq!(
                    token,
                    PcmAudioState {
                        open: true,
                        generation: 1
                    }
                );
                audio.change(false);
            }
            if point == SendPoint::BeforeDispatch {
                packets.lock().unwrap().push(packet.clone());
                if frame.load(Ordering::SeqCst) == 2 {
                    audio.change(true);
                }
            }
        })
    });
    receiver.start_sending(audio.clone(), 17, 7, 77).unwrap();
    next.wait().await;
    let packets = packets.lock().unwrap().clone();
    assert_eq!(packets.len(), 3);
    assert!(!packets[0].is_empty());
    assert!(packets[1].is_empty());
    assert_eq!(packets[0], packets[2]);
    assert_eq!(receiver.transmission().1, 2);
    assert_eq!(receiver.observation().rtcp_session.unwrap().sent.packets, 2);
    for payload in [&packets[0], &packets[2]] {
        let (wire, _) = packet(&peer).await;
        assert_eq!(RtpPacket::parse(&wire).unwrap().payload, payload);
    }
    receiver.request_close();
    next.release();
    close(&receiver).await;
    assert_eq!(counters(&receiver).1, 9);
    assert_eq!(counters(&receiver).2, 77 + 1920);
    trace(
        "predictive-reopen",
        IvyJson::object([
            ("encodedFrames", count(3)),
            ("admittedFrames", count(2)),
            ("obsoleteFrames", count(1)),
            ("freshPredictiveStateEqual", true.into()),
            ("reopenedGeneration", count(3)),
        ]),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepted_format_replacement_invalidates_inflight_frame_without_replacing_clock_or_socket()
{
    let (receiver, audio, _, peer, control) = setup(true).await;
    configure(&receiver, &peer, &control, Codec::Pcmu);
    let first = Arc::new(Gate::default());
    let next = Arc::new(Gate::default());
    hook(&receiver, {
        let first = first.clone();
        let next = next.clone();
        Arc::new(move |point, _, _| {
            if point == SendPoint::Encode(EncodePoint::Admitted)
                && first.reached.load(Ordering::SeqCst)
            {
                next.hit();
            }
            if point == SendPoint::BeforeDispatch && !first.reached.load(Ordering::SeqCst) {
                first.hit();
            }
        })
    });
    receiver.start_sending(audio.clone(), 17, 7, 77).unwrap();
    first.wait().await;
    let local = receiver.observation().transport.local_rtp;
    configure(&receiver, &peer, &control, Codec::Pcma);
    first.release();
    next.wait().await;
    assert_eq!(counters(&receiver), (0, 7, 77, 0, 0, 0));
    next.release();
    let (wire, source) = packet(&peer).await;
    let packet = RtpPacket::parse(&wire).unwrap();
    assert_eq!(
        (
            packet.header.payload_type,
            packet.header.sequence,
            packet.header.timestamp
        ),
        (8, 7, 77)
    );
    assert_eq!(source, local);
    close(&receiver).await;
    trace(
        "format-replacement",
        IvyJson::object([
            ("obsoleteOldFormatFrames", count(0)),
            ("firstPayload", count(8)),
            ("firstSequence", count(7)),
            ("firstTimestamp", count(77)),
            ("retainedSocket", true.into()),
        ]),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repair_pending_keeps_the_original_fifteen_second_limit_even_without_negotiation() {
    let (receiver, audio, events, _, _) = setup(false).await;
    audio.health.store(1, Ordering::SeqCst);
    let health = Arc::new(Gate::default());
    *audio.health_gate.lock().unwrap() = Some(health.clone());
    receiver.start_sending(audio.clone(), 17, 7, 77).unwrap();
    health.wait().await;
    sleep(Duration::from_millis(150)).await;
    let began = Instant::now();
    health.release();
    timeout(Duration::from_secs(18), async {
        while events.0.load(Ordering::SeqCst) == 0 {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let elapsed = began.elapsed();
    assert!(elapsed >= Duration::from_secs(15));
    close(&receiver).await;
    assert_eq!(counters(&receiver), (0, 7, 77, 0, 0, 0));
    assert_eq!(audio.reads.load(Ordering::SeqCst), 0);
    assert_eq!(events.0.load(Ordering::SeqCst), 1);
    trace(
        "repair-limit",
        IvyJson::object([
            ("elapsedMicros", count(elapsed.as_micros() as u64)),
            ("failures", count(1)),
            ("idleCodecReads", count(0)),
            (
                "healthChecks",
                count(audio.health_reads.load(Ordering::SeqCst) as u64),
            ),
        ]),
    );
}
