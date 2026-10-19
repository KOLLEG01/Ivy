use ivy_phone_bridge::{
    dtmf::InputSource,
    media::{
        AudioNegotiator, Codec, PcmAudioPort, PcmAudioState, RtpHeader, RtpPeer, SdpDocument,
        SdpType, SipReceiveEvents, SipRtpReceiver,
    },
};
use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::UdpSocket,
    time::{sleep, timeout},
};

struct Audio {
    state: Mutex<(PcmAudioState, Vec<Vec<f32>>)>,
    revoke: AtomicBool,
}
impl Audio {
    fn new(open: bool) -> Self {
        Self {
            state: Mutex::new((
                PcmAudioState {
                    open,
                    generation: 1,
                },
                Vec::new(),
            )),
            revoke: AtomicBool::new(false),
        }
    }
    fn set(&self, open: bool, generation: u64) {
        self.state.lock().unwrap().0 = PcmAudioState { open, generation };
    }
    fn frames(&self) -> usize {
        self.state.lock().unwrap().1.len()
    }
}
impl PcmAudioPort for Audio {
    fn state(&self) -> PcmAudioState {
        self.state.lock().unwrap().0
    }
    fn read_captured(&self, output: &mut [f32], generation: u64) -> usize {
        let state = self.state.lock().unwrap().0;
        output.fill(if state.open && state.generation == generation {
            0.25
        } else {
            0.0
        });
        if state.open && state.generation == generation {
            output.len()
        } else {
            0
        }
    }
    fn write_received(&self, input: &[f32], generation: u64) -> bool {
        let mut state = self.state.lock().unwrap();
        if self.revoke.swap(false, Ordering::SeqCst) {
            state.0.generation += 1;
        }
        if !state.0.open || state.0.generation != generation {
            return false;
        }
        assert!(input.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
        state.1.push(input.to_vec());
        true
    }
}
#[derive(Default)]
struct Events {
    digits: Mutex<Vec<(char, InputSource)>>,
    controls: Mutex<Vec<Vec<u8>>>,
    failures: AtomicUsize,
    panic_report: AtomicBool,
    panic_failure: AtomicBool,
}
impl SipReceiveEvents for Events {
    fn digit(&self, digit: char, source: InputSource) {
        self.digits.lock().unwrap().push((digit, source));
    }
    fn failed(&self) {
        self.failures.fetch_add(1, Ordering::SeqCst);
        assert!(
            !self.panic_failure.load(Ordering::SeqCst),
            "Synthetic failure callback"
        );
    }
    fn rtcp(&self, _: SocketAddr, bytes: &[u8]) {
        assert!(
            !self.panic_report.load(Ordering::SeqCst),
            "Synthetic report callback"
        );
        self.controls.lock().unwrap().push(bytes.to_vec());
    }
}
async fn wait(mut condition: impl FnMut() -> bool) {
    timeout(Duration::from_secs(4), async {
        while !condition() {
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}
fn description(port: u16, opus: bool) -> SdpDocument {
    SdpDocument::parse(&format!("v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {port} RTP/AVP {} 101 110\r\n{}a=rtpmap:101 telephone-event/8000\r\na=rtpmap:110 telephone-event/48000\r\na=sendrecv\r\n",
        if opus {"111"} else {"0 8"}, if opus {"a=rtpmap:111 opus/48000/2\r\n"} else {"a=rtpmap:0 PCMU/8000\r\na=rtpmap:8 PCMA/8000\r\n"})).unwrap()
}
fn configure(receiver: &SipRtpReceiver, rtp: SocketAddr, rtcp: SocketAddr, opus: bool) {
    let mut negotiator = AudioNegotiator::new(if opus {
        &[Codec::Opus]
    } else {
        &[Codec::Pcmu, Codec::Pcma]
    })
    .unwrap();
    negotiator
        .set_remote_description(SdpType::Offer, &description(rtp.port(), opus))
        .unwrap();
    receiver
        .configure(
            negotiator.state().unwrap(),
            RtpPeer::new(rtp, rtcp).unwrap(),
        )
        .unwrap();
}
async fn send(
    socket: &UdpSocket,
    destination: SocketAddr,
    sequence: u16,
    payload_type: u8,
    source: u32,
    data: &[u8],
) {
    let mut packet = vec![0; 12 + data.len()];
    RtpHeader {
        marker: false,
        payload_type,
        sequence,
        timestamp: u32::from(sequence).wrapping_mul(160),
        source,
    }
    .write(data, &mut packet)
    .unwrap();
    socket.send_to(&packet, destination).await.unwrap();
    packet.fill(0);
}
async fn setup(
    first: u16,
    open: bool,
    opus: bool,
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
        first,
        first + 98,
        60,
        audio.clone(),
        None,
        events.clone(),
    )
    .unwrap();
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let control = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    configure(
        &receiver,
        peer.local_addr().unwrap(),
        control.local_addr().unwrap(),
        opus,
    );
    (receiver, audio, events, peer, control)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn actual_receiver_orders_wrap_wakes_gaps_and_erases_stale_route_and_format_state() {
    let (receiver, audio, _, peer, _) = setup(40000, true, false).await;
    let local = receiver.observation().transport.local_rtp;
    send(&peer, local, 65534, 0, 7, &[0x80; 160]).await;
    wait(|| receiver.observation().received_packets == 1).await;
    send(&peer, local, 0, 0, 7, &[0x80; 160]).await;
    wait(|| receiver.observation().receive.queued_packets == 1).await;
    send(&peer, local, 65535, 0, 7, &[0x00; 160]).await;
    wait(|| receiver.observation().received_packets == 3).await;
    assert_eq!(receiver.observation().receive.missing_packets, 0);
    send(&peer, local, 0, 0, 7, &[0x80; 160]).await;
    wait(|| receiver.observation().receive.late_packets > 0).await;
    send(&peer, local, 2, 0, 7, &[0x80; 160]).await;
    wait(|| receiver.observation().received_packets == 4).await; // No later datagram to wake this gap.
    assert_eq!(receiver.observation().receive.missing_packets, 1);
    let before = audio.frames();
    audio.set(false, 1);
    send(&peer, local, 3, 0, 7, &[0x80; 160]).await;
    wait(|| receiver.observation().receive.gated_packets > 0).await;
    assert_eq!(audio.frames(), before);
    audio.set(true, 2);
    send(&peer, local, 4, 8, 7, &[0xd5; 160]).await;
    wait(|| receiver.observation().received_packets == 5).await;
    assert_eq!(audio.frames(), before + 1);
    send(&peer, local, 6, 0, 7, &[0x80; 160]).await;
    wait(|| receiver.observation().receive.queued_packets == 1).await;
    audio.set(true, 3);
    send(&peer, local, 7, 0, 7, &[0x80; 160]).await;
    wait(|| receiver.observation().received_packets == 6).await;
    assert_eq!(receiver.observation().receive.queued_packets, 0);
    assert_eq!(audio.frames(), before + 2); // Old generation packet6 was cleared.
    audio.revoke.store(true, Ordering::SeqCst);
    send(&peer, local, 8, 0, 7, &[0x80; 160]).await;
    wait(|| receiver.observation().received_packets == 7).await;
    assert_eq!(audio.frames(), before + 2);
    send(&peer, local, 9, 0, 7, &[0x80; 160]).await;
    wait(|| receiver.observation().received_packets == 8).await;
    send(&peer, local, 10, 0, 8, &[0x80; 160]).await;
    wait(|| receiver.observation().receive.foreign_packets > 0).await;
    send(&peer, local, 10, 127, 7, &[1]).await;
    wait(|| receiver.observation().unsupported_payload_packets == 1).await;
    let foreign = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    send(&foreign, local, 10, 0, 7, &[0x80; 160]).await;
    wait(|| receiver.observation().transport.counters.foreign_rtp > 0).await;
    timeout(Duration::from_secs(3), receiver.close())
        .await
        .unwrap();
    let closed = receiver.observation();
    assert!(closed.cleanup_confirmed);
    assert!(!closed.failed);
    assert_eq!(closed.receive.queued_packets, 0);
    let _rtp = std::net::UdpSocket::bind(closed.transport.local_rtp).unwrap();
    let _rtcp = std::net::UdpSocket::bind(closed.transport.local_rtcp).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn wire_events_and_private_inband_analysis_work_while_pcm_is_closed_and_rtcp_is_independent()
{
    let (receiver, audio, events, peer, control) = setup(40200, false, false).await;
    let local = receiver.observation().transport;
    send(&peer, local.local_rtp, 0, 110, 9, &[5, 0, 0, 160]).await;
    send(&peer, local.local_rtp, 0, 101, 9, &[5, 0x80, 1, 64]).await; // Same event across payloads.
    send(&peer, local.local_rtp, 0, 110, 9, &[16, 0, 0, 1]).await;
    send(&peer, local.local_rtp, 0, 110, 9, &[5]).await;
    wait(|| receiver.observation().event_packets == 4).await;
    assert_eq!(
        *events.digits.lock().unwrap(),
        vec![('5', InputSource::Rfc4733)]
    );
    let report = [0x80, 201, 0, 1, 0, 0, 0, 9];
    control.send_to(&report, local.local_rtcp).await.unwrap();
    wait(|| events.controls.lock().unwrap().len() == 1).await;
    assert_eq!(events.controls.lock().unwrap()[0], report);
    let foreign = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    foreign.send_to(&report, local.local_rtcp).await.unwrap();
    wait(|| receiver.observation().transport.counters.foreign_rtcp == 1).await;
    receiver.set_inband_needed(true);
    let codec = ivy_phone_bridge::media::G711Codec::new(&Codec::Pcmu.offered_formats()[0]).unwrap();
    for frame in 0..10 {
        let pcm = std::array::from_fn(|i| {
            let t = (usize::from(frame) * 160 + i) as f64 / 8000.0;
            let sample = ((770.0 * std::f64::consts::TAU * t).sin()
                + (1336.0 * std::f64::consts::TAU * t).sin())
                * 6000.0;
            sample as i16
        });
        let data = codec.encode_20ms(&pcm);
        send(&peer, local.local_rtp, frame, 0, 9, &data).await;
        wait(|| receiver.observation().inband_analyzed_packets == u64::from(frame) + 1).await;
    }
    wait(|| receiver.observation().inband_detected_digits == 1).await;
    assert_eq!(
        *events.digits.lock().unwrap(),
        vec![('5', InputSource::Rfc4733), ('5', InputSource::InBandAudio)]
    );
    assert_eq!(audio.frames(), 0);
    assert_eq!(receiver.observation().received_packets, 0);
    assert!(receiver.observation().inband.unwrap().analyzed_windows > 0);
    receiver.set_inband_needed(false);
    assert!(receiver.observation().inband.is_none());
    receiver.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn codec_and_notification_failures_close_original_workers_even_when_the_failure_callback_panics()
 {
    for panic in [false, true] {
        let (receiver, _, events, peer, control) =
            setup(if panic { 40600 } else { 40400 }, true, !panic).await;
        let local = receiver.observation().transport;
        if panic {
            events.panic_report.store(true, Ordering::SeqCst);
            events.panic_failure.store(true, Ordering::SeqCst);
            control
                .send_to(&[0x80, 201, 0, 1, 0, 0, 0, 9], local.local_rtcp)
                .await
                .unwrap();
        } else {
            send(&peer, local.local_rtp, 0, 111, 9, &[0xff]).await;
        }
        wait(|| receiver.observation().cleanup_confirmed).await;
        assert!(receiver.observation().failed);
        assert_eq!(events.failures.load(Ordering::SeqCst), 1);
        let _rtp = std::net::UdpSocket::bind(local.local_rtp).unwrap();
        let _rtcp = std::net::UdpSocket::bind(local.local_rtcp).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn last_handle_drop_stops_idle_receive_and_deadline_owners_and_releases_the_exact_pair() {
    let (receiver, audio, events, peer, _) = setup(40800, true, false).await;
    let local = receiver.observation().transport;
    send(&peer, local.local_rtp, 0, 0, 9, &[0x80; 160]).await;
    wait(|| receiver.observation().received_packets == 1).await;
    send(&peer, local.local_rtp, 2, 0, 9, &[0x80; 160]).await;
    wait(|| receiver.observation().receive.queued_packets == 1).await;
    let clone = receiver.clone();
    drop(receiver);
    assert!(!clone.observation().transport.closed);
    drop(clone);
    wait(|| Arc::strong_count(&audio) == 1 && Arc::strong_count(&events) == 1).await;
    let _rtp = std::net::UdpSocket::bind(local.local_rtp).unwrap();
    let _rtcp = std::net::UdpSocket::bind(local.local_rtcp).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn all_real_codec_receive_paths_use_the_same_bound_owner_and_accepted_negotiation() {
    use ivy_phone_bridge::media::{EvsLibrary, MediaCodec, SdpIdentity};
    let evs = std::env::var_os("IVY_PHONE_EVS_DIR").map(|path| {
        // SAFETY: the qualification runner verifies this exact reference build,
        // ABI2 source/compiler/notice and DLL byte hashes before execution.
        unsafe { EvsLibrary::load_packaged(&std::path::PathBuf::from(path)) }.unwrap()
    });
    let mut paths = Vec::new();
    for codec in [
        Codec::Pcma,
        Codec::Pcmu,
        Codec::G722,
        Codec::Opus,
        Codec::Evs,
    ] {
        if codec == Codec::Evs && evs.is_none() {
            eprintln!("Real EVS receive path unavailable; not executed");
            continue;
        }
        let audio = Arc::new(Audio::new(true));
        let events = Arc::new(Events::default());
        let receiver = SipRtpReceiver::bind(
            "127.0.0.1:0".parse().unwrap(),
            41000,
            41098,
            20,
            audio.clone(),
            evs.clone(),
            events,
        )
        .unwrap();
        let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let control = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut negotiator = AudioNegotiator::new(&[codec]).unwrap();
        let identity = SdpIdentity::new(peer.local_addr().unwrap(), 1, 1, 9, "fixture").unwrap();
        let offer = negotiator.create_offer(&identity);
        negotiator
            .set_remote_description(SdpType::Offer, &offer)
            .unwrap();
        let plan = negotiator.state().unwrap();
        receiver
            .configure(
                plan,
                RtpPeer::new(peer.local_addr().unwrap(), control.local_addr().unwrap()).unwrap(),
            )
            .unwrap();
        let mut encoder =
            MediaCodec::new(plan.sender().clone(), audio.clone(), evs.clone()).unwrap();
        for frame in 0..25 {
            let payload = encoder.encode().unwrap();
            send(
                &peer,
                receiver.observation().transport.local_rtp,
                frame,
                plan.sender().payload(),
                9,
                &payload,
            )
            .await;
            wait(|| receiver.observation().received_packets == u64::from(frame) + 1).await;
        }
        assert_eq!(audio.frames(), 25);
        let samples = audio
            .state
            .lock()
            .unwrap()
            .1
            .iter()
            .map(Vec::len)
            .sum::<usize>();
        assert!(
            samples > 23000 && samples <= 24000,
            "{} samples={samples}",
            codec.name()
        );
        paths.push(ivy_contracts::IvyJson::object([
            ("codec", codec.name().into()),
            ("packets", ivy_contracts::IvyJson::Number(25.0)),
            ("samples", ivy_contracts::IvyJson::Number(samples as f64)),
        ]));
        receiver.close().await;
        assert!(!receiver.observation().failed);
    }
    if let Some(path) = std::env::var_os("IVY_PHONE_SIP_RECEIVER_TRACE") {
        std::fs::write(
            path,
            ivy_contracts::json::stringify(&ivy_contracts::IvyJson::Array(paths), 100000).unwrap(),
        )
        .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_close_waiter_does_not_abandon_an_active_callback_or_original_cleanup() {
    struct Closing {
        receiver: Mutex<Option<SipRtpReceiver>>,
        was_pending: AtomicBool,
        returned: AtomicBool,
    }
    impl SipReceiveEvents for Closing {
        fn digit(&self, _: char, _: InputSource) {}
        fn failed(&self) {
            panic!("Unexpected media failure");
        }
        fn rtcp(&self, _: SocketAddr, _: &[u8]) {
            use std::{
                future::Future,
                task::{Context, Waker},
            };
            let receiver = self.receiver.lock().unwrap().take().unwrap();
            let mut close = Box::pin(receiver.close());
            // This callback is still active. Close must request cancellation
            // without waiting synchronously for this very callback to return.
            self.was_pending.store(
                close
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending(),
                Ordering::SeqCst,
            );
            drop(close); // Cancel only this observer, retaining original cleanup.
            self.returned.store(true, Ordering::SeqCst);
        }
    }
    let events = Arc::new(Closing {
        receiver: Mutex::new(None),
        was_pending: AtomicBool::new(false),
        returned: AtomicBool::new(false),
    });
    let receiver = SipRtpReceiver::bind(
        "127.0.0.1:0".parse().unwrap(),
        41200,
        41298,
        20,
        Arc::new(Audio::new(true)),
        None,
        events.clone(),
    )
    .unwrap();
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let control = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    configure(
        &receiver,
        peer.local_addr().unwrap(),
        control.local_addr().unwrap(),
        false,
    );
    *events.receiver.lock().unwrap() = Some(receiver.clone());
    let local = receiver.observation().transport;
    control
        .send_to(&[0x80, 201, 0, 1, 0, 0, 0, 9], local.local_rtcp)
        .await
        .unwrap();
    wait(|| events.returned.load(Ordering::SeqCst) && receiver.observation().cleanup_confirmed)
        .await;
    assert!(
        events.was_pending.load(Ordering::SeqCst),
        "Close returned before its active callback completed"
    );
    assert!(receiver.observation().cleanup_confirmed);
    let _rtp = std::net::UdpSocket::bind(local.local_rtp).unwrap();
    let _rtcp = std::net::UdpSocket::bind(local.local_rtcp).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn original_managed_sender_drives_the_real_owned_pcm_receiver() {
    use ivy_contracts::{IvyJson, json::parse};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    if std::env::var_os("IVY_PHONE_CONFIG_ORACLE_DIR").is_none() {
        eprintln!("Original managed sender unavailable; receive interoperability not executed");
        return;
    }
    let audio = Arc::new(Audio::new(true));
    let receiver = SipRtpReceiver::bind(
        "127.0.0.1:0".parse().unwrap(),
        41400,
        41498,
        60,
        audio.clone(),
        None,
        Arc::new(Events::default()),
    )
    .unwrap();
    let mut child = tokio::process::Command::new("pwsh")
        .args(["-NoProfile", "-File", "tests/fixtures/phone-rtp-peer.ps1"])
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.."))
        .kill_on_drop(true)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    async fn read(output: &mut BufReader<tokio::process::ChildStdout>) -> IvyJson {
        let mut text = String::new();
        assert!(
            timeout(Duration::from_secs(15), output.read_line(&mut text))
                .await
                .unwrap()
                .unwrap()
                > 0
        );
        parse(&text, 65536).unwrap()
    }
    async fn command(
        input: &mut tokio::process::ChildStdin,
        output: &mut BufReader<tokio::process::ChildStdout>,
        command: &str,
    ) -> IvyJson {
        input
            .write_all(format!("{command}\n").as_bytes())
            .await
            .unwrap();
        input.flush().await.unwrap();
        read(output).await
    }
    let hello = read(&mut output).await;
    let rtp = format!(
        "127.0.0.1:{}",
        hello.get("rtp").unwrap().as_f64().unwrap() as u16
    )
    .parse()
    .unwrap();
    let rtcp = format!(
        "127.0.0.1:{}",
        hello.get("rtcp").unwrap().as_f64().unwrap() as u16
    )
    .parse()
    .unwrap();
    configure(&receiver, rtp, rtcp, false);
    let local = receiver.observation().transport;
    command(
        &mut input,
        &mut output,
        &format!(
            r#"{{"op":"peer","rtp":{},"rtcp":{}}}"#,
            local.local_rtp.port(),
            local.local_rtcp.port()
        ),
    )
    .await;
    for frame in 0..12_u16 {
        command(
            &mut input,
            &mut output,
            &format!(
                r#"{{"op":"send","sequence":{frame},"timestamp":{},"positive":{}}}"#,
                u32::from(frame) * 160,
                frame % 2 == 0
            ),
        )
        .await;
        wait(|| receiver.observation().received_packets == u64::from(frame) + 1).await;
    }
    let frames = audio.state.lock().unwrap().1.clone();
    assert_eq!(frames.len(), 12);
    for (index, frame) in frames.iter().enumerate() {
        let mean =
            frame[frame.len() / 2..].iter().sum::<f32>() / (frame.len() - frame.len() / 2) as f32;
        assert!(
            if index % 2 == 0 {
                mean > 0.2
            } else {
                mean < -0.2
            },
            "Original PCM order/level at{index}: {mean}"
        );
    }
    command(&mut input, &mut output, r#"{"op":"stop"}"#).await;
    assert!(
        timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    receiver.close().await;
}
