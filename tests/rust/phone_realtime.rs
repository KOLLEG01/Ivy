use super::*;
use crate::media::{AudioFormat, OpusEncoder, PcmAudioPort, PcmSendPort};
use std::{
    collections::VecDeque,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use str0m::{
    Candidate, Event, Input, Output, Rtc,
    change::SdpOffer,
    media::Mid,
    net::{Protocol, Receive, Transmit},
    rtp::RtpWrite,
};
mod route_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/rust/phone_realtime_route.rs"
    ));
}

struct Packet {
    source: SocketAddr,
    destination: SocketAddr,
    bytes: Vec<u8>,
}
impl From<Transmit> for Packet {
    fn from(v: Transmit) -> Self {
        Self {
            source: v.source,
            destination: v.destination,
            bytes: v.contents.to_vec(),
        }
    }
}
fn audio_packet(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && bytes[0] >> 6 == 2 && !(192..=223).contains(&bytes[1])
}
fn trace(name: &str, value: serde_json::Value) {
    if let Ok(root) = std::env::var("IVY_PHONE_REALTIME_TRACE") {
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            std::path::Path::new(&root).join(format!("{name}.json")),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
    }
}
// Actual ICE/DTLS/SRTP/SCTP engines and certificates; only the UDP delivery and
// monotonic clock are synthetic. No sockets, accounts or audio devices exist.
struct Pair {
    peer: RealtimePeer,
    remote: Rtc,
    now: Instant,
    remote_timeout: Instant,
    to_peer: VecDeque<Packet>,
    to_remote: VecDeque<Packet>,
    mid: Option<Mid>,
    packets: Vec<Vec<u8>>,
    remote_events: bool,
    permit: Arc<AtomicBool>,
}
impl Pair {
    fn new() -> Self {
        let now = Instant::now();
        let permit = Arc::new(AtomicBool::new(true));
        let permission = permit.clone();
        let mut to_remote = VecDeque::new();
        let peer = RealtimePeer::new(
            now,
            &["127.0.0.1:42000".parse().unwrap()],
            100,
            Some(0),
            Arc::new(move || permission.load(Ordering::Acquire)),
            &mut |p| {
                to_remote.push_back(p.into());
                Ok(())
            },
        )
        .unwrap();
        let remote = Rtc::builder()
            .clear_codecs()
            .enable_opus(true, false)
            .set_rtp_mode(true)
            .set_fingerprint_verification(true)
            .build(now);
        let mut pair = Self {
            peer,
            remote,
            now,
            remote_timeout: now,
            to_peer: VecDeque::new(),
            to_remote,
            mid: None,
            packets: Vec::new(),
            remote_events: false,
            permit,
        };
        pair.drain_remote();
        pair.remote.add_local_candidate(
            Candidate::host("127.0.0.1:42001".parse().unwrap(), "udp").unwrap(),
        );
        pair.drain_remote();
        pair
    }
    fn negotiate(&mut self) {
        let offer = SdpOffer::from_sdp_string(self.peer.offer()).unwrap();
        let answer = self.remote.sdp_api().accept_offer(offer).unwrap();
        self.drain_remote();
        self.peer
            .answer(self.now, &answer.to_sdp_string(), &mut |p| {
                self.to_remote.push_back(p.into());
                Ok(())
            })
            .unwrap();
    }
    fn drain_remote(&mut self) {
        loop {
            match self.remote.poll_output().unwrap() {
                Output::Timeout(t) => {
                    self.remote_timeout = t;
                    break;
                }
                Output::Transmit(p) => self.to_peer.push_back(p.into()),
                Output::Event(Event::MediaAdded(m)) => self.mid = Some(m.mid),
                Output::Event(Event::RtpPacket(p)) => self.packets.push(p.payload.to_vec()),
                Output::Event(Event::ChannelOpen(_, label)) if label == "oai-events" => {
                    self.remote_events = true
                }
                _ => {}
            }
        }
    }
    fn deliver(&mut self) {
        for _ in 0..1000 {
            if let Some(p) = self.to_peer.pop_front() {
                self.peer
                    .receive(self.now, p.source, p.destination, &p.bytes, &mut |v| {
                        self.to_remote.push_back(v.into());
                        Ok(())
                    })
                    .unwrap();
            } else if let Some(p) = self.to_remote.pop_front() {
                let r = Receive::new(Protocol::Udp, p.source, p.destination, &p.bytes).unwrap();
                self.remote
                    .handle_input(Input::Receive(self.now, r))
                    .unwrap();
                self.drain_remote();
            } else {
                return;
            }
        }
        panic!("unbounded synthetic packet exchange");
    }
    fn connect(&mut self) {
        self.negotiate();
        for _ in 0..2000 {
            self.deliver();
            if self.peer.connection_result(self.now) == Ok(true)
                && self.remote.is_connected()
                && self.peer.events_open()
                && self.remote_events
            {
                return;
            }
            self.now += Duration::from_millis(5);
            self.peer
                .timeout(self.now, &mut |p| {
                    self.to_remote.push_back(p.into());
                    Ok(())
                })
                .unwrap();
            self.remote.handle_input(Input::Timeout(self.now)).unwrap();
            self.drain_remote();
        }
        panic!("real crypto/datachannel connection did not complete");
    }
    fn encoded() -> Vec<u8> {
        let format =
            AudioFormat::new(111, "opus", 48000, 48000, 2, Some("useinbandfec=1")).unwrap();
        let input = std::array::from_fn(|i| ((i as f32 * 0.08).sin() * 12000.0) as i16);
        OpusEncoder::new(&format)
            .unwrap()
            .encode_20ms(&input)
            .unwrap()
    }
    fn remote_audio(&mut self, sequence: u64, timestamp: u32, payload: &[u8]) -> Packet {
        let pt = self.remote.media(self.mid.unwrap()).unwrap().remote_pts()[0];
        self.remote
            .direct_api()
            .stream_tx_by_mid(self.mid.unwrap(), None)
            .unwrap()
            .write_rtp(RtpWrite::new(
                pt,
                sequence.into(),
                timestamp,
                self.now,
                payload,
            ));
        self.drain_remote();
        self.remote.handle_input(Input::Timeout(self.now)).unwrap();
        self.drain_remote();
        let index = self
            .to_peer
            .iter()
            .position(|p| audio_packet(&p.bytes))
            .expect("encrypted RTP output");
        self.to_peer.remove(index).unwrap()
    }
    fn receive(&mut self, packet: &Packet) -> bool {
        self.peer
            .receive(
                self.now,
                packet.source,
                packet.destination,
                &packet.bytes,
                &mut |p| {
                    self.to_remote.push_back(p.into());
                    Ok(())
                },
            )
            .unwrap()
    }
}

mod protocol_tests {
    use super::*;

    #[test]
    fn admission_sdp_deadline_and_retained_audio_close() {
        let now = Instant::now();
        let permit: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(|| true);
        for (queue, prebuffer) in [(0, None), (19, None), (201, None), (100, Some(101))] {
            assert!(matches!(
                RealtimePeer::new(now, &[], queue, prebuffer, permit.clone(), &mut |_| Ok(())),
                Err(RealtimeError::InvalidRequest)
            ));
        }
        let mut pair = Pair::new();
        let now = pair.now;
        let offer = pair.peer.offer();
        assert!(offer.contains("opus/48000/2") && offer.contains("oai-events") == false);
        assert!(
            !offer.contains("VP8") && !offer.contains("PCMU") && offer.contains("m=application")
        );
        for sdp in [
            "".to_owned(),
            " ".to_owned(),
            "x".repeat(48001),
            "😀".repeat(24001),
        ] {
            assert_eq!(
                pair.peer.answer(now, &sdp, &mut |_| Ok(())),
                Err(RealtimeError::InvalidRequest)
            );
        }
        assert_eq!(
            pair.peer.answer(now, "invalid", &mut |_| Ok(())),
            Err(RealtimeError::InvalidAnswer)
        );
        assert_eq!(
            pair.peer.connection_result(now),
            Err(RealtimeError::NotReady)
        );
        let mut probe = vec![0; 28];
        probe[0] = 0x80;
        probe[1] = 111;
        assert!(
            !pair
                .peer
                .receive(
                    now,
                    "127.0.0.1:42001".parse().unwrap(),
                    "127.0.0.1:42000".parse().unwrap(),
                    &probe,
                    &mut |_| panic!("pre-DTLS media emitted output")
                )
                .unwrap()
        );
        pair.negotiate();
        assert_eq!(
            pair.peer.connection_result(now + Duration::from_secs(15)),
            Err(RealtimeError::Timeout)
        );
        assert_eq!(pair.peer.connection_result(now), Ok(false));
        let audio = pair.peer.audio();
        audio.resume().unwrap();
        let before = audio.state();
        assert!(!before.open);
        pair.peer.close(&mut |_| Ok(())).unwrap();
        pair.peer
            .close(&mut |_| panic!("second close emitted"))
            .unwrap();
        assert_eq!(pair.peer.connection_result(now), Err(RealtimeError::Closed));
        assert!(!audio.state().open);
        assert_eq!(audio.resume(), Err(RealtimeError::Failed));
        drop(pair);
        let mut output = [1.0; 960];
        assert_eq!(audio.read_captured(&mut output, before.generation), 0);
        assert_eq!(output, [0.0; 960]);
        trace(
            "admission",
            serde_json::json!({"badSettings":4,"badSdpBounds":4,"invalidSdp":1,"preDtlsDrop":true,"deadline":15,"retainedClosed":true}),
        );
    }

    #[test]
    fn authenticated_startup_wrap_tamper_replay_and_pcm() {
        let payload = Pair::encoded();
        // A new SRTP receiver starts at ROC zero. Keep the former fixture's
        // unseeded rollover as a negative case, not a demand to guess ROC one.
        let mut unseeded = Pair::new();
        unseeded.connect();
        let unseeded_audio = unseeded.peer.audio();
        unseeded_audio.resume().unwrap();
        let first = unseeded.remote_audio(65534, 0, &payload);
        assert!(!unseeded.receive(&first));
        let valid = unseeded.remote_audio(65535, 960, &payload);
        let mut corrupt = Packet {
            source: valid.source,
            destination: valid.destination,
            bytes: valid.bytes.clone(),
        };
        *corrupt.bytes.last_mut().unwrap() ^= 0x80;
        assert!(unseeded.receive(&corrupt));
        assert!(!unseeded.receive(&valid));
        let wrapped = unseeded.remote_audio(65536, 1920, &payload);
        assert!(unseeded.receive(&wrapped));
        assert_eq!(unseeded_audio.0.lock().unwrap().received_packets, 0);
        let mut silent = [0.0; 960];
        assert_eq!(
            unseeded_audio.read_captured(&mut silent, unseeded_audio.state().generation),
            0
        );
        unseeded.peer.close(&mut |_| Ok(())).unwrap();

        let mut pair = Pair::new();
        pair.connect();
        let audio = pair.peer.audio();
        audio.resume().unwrap();
        let first = pair.remote_audio(65533, 0, &payload);
        assert!(!pair.receive(&first));
        assert_eq!(audio.0.lock().unwrap().received_packets, 0);
        let valid = pair.remote_audio(65534, 960, &payload);
        let mut corrupt = Packet {
            source: valid.source,
            destination: valid.destination,
            bytes: valid.bytes.clone(),
        };
        *corrupt.bytes.last_mut().unwrap() ^= 0x80;
        assert!(pair.receive(&corrupt));
        assert_eq!(audio.0.lock().unwrap().received_packets, 0);
        // Authentication failure must not seed replay protection or finish probation.
        assert!(!pair.receive(&valid));
        let before_wrap = pair.remote_audio(65535, 1920, &payload);
        assert!(pair.receive(&before_wrap));
        assert_eq!(audio.0.lock().unwrap().received_packets, 1);
        let wrapped = pair.remote_audio(65536, 2880, &payload);
        assert!(pair.receive(&wrapped));
        assert_eq!(audio.0.lock().unwrap().received_packets, 2);
        assert!(pair.receive(&wrapped));
        assert_eq!(audio.0.lock().unwrap().received_packets, 2);
        let following = pair.remote_audio(65537, 3840, &payload);
        assert!(pair.receive(&following));
        assert_eq!(audio.0.lock().unwrap().received_packets, 3);
        let later = pair.remote_audio(65538, 4800, &payload);
        let mut damaged = Packet {
            source: later.source,
            destination: later.destination,
            bytes: later.bytes.clone(),
        };
        *damaged.bytes.last_mut().unwrap() ^= 1;
        assert!(pair.receive(&damaged));
        assert_eq!(audio.0.lock().unwrap().received_packets, 3);
        assert!(pair.receive(&later));
        assert_eq!(audio.0.lock().unwrap().received_packets, 4);
        let mut output = [0.0; 960];
        let generation = audio.state().generation;
        assert_eq!(audio.read_captured(&mut output, generation), 960);
        assert!(output.iter().any(|v| v.abs() > 0.01));
        pair.peer.close(&mut |_| Ok(())).unwrap();
        trace(
            "authenticated",
            serde_json::json!({"connected":true,"eventsBothDirections":true,"firstDropped":true,"tamperedRejected":true,"unseededRocRejected":true,"authenticatedBeforeWrap":true,"wrapAccepted":true,"replayRejected":true,"decodedPackets":4,"pcmSamples":960}),
        );
    }

    #[test]
    fn outbound_pcm_generation_permit_and_drop_retirement() {
        let mut pair = Pair::new();
        pair.connect();
        let audio = pair.peer.audio();
        audio.resume().unwrap();
        let generation = audio.state().generation;
        assert!(audio.write_received(&[0.25; 960], generation));
        pair.peer
            .send_frame(pair.now, &mut |p| {
                pair.to_remote.push_back(p.into());
                Ok(())
            })
            .unwrap();
        pair.deliver();
        assert_eq!(pair.packets.len(), 1);
        assert!(!pair.packets[0].is_empty());
        let admitted = audio.state();
        let mut actions = 0;
        assert!(audio.with_send_state(admitted, &mut || actions += 1));
        audio.suspend();
        assert!(!audio.with_send_state(admitted, &mut || actions += 1));
        assert_eq!(actions, 1);
        assert!(!audio.write_received(&[1.0; 960], generation));
        audio.resume().unwrap();
        let resumed = audio.state();
        assert_ne!(resumed.generation, generation);
        assert!(audio.write_received(&[0.4; 960], resumed.generation));
        pair.permit.store(false, Ordering::Release);
        pair.peer
            .send_frame(pair.now, &mut |_| panic!("revoked PCM escaped"))
            .unwrap();
        let denied = audio.state();
        assert!(!denied.open);
        assert_ne!(denied.generation, resumed.generation);
        let status = audio.status();
        assert_eq!(
            status.get("captureQueuedSamples").unwrap().as_f64(),
            Some(0.0)
        );
        assert_eq!(
            status.get("renderQueuedSamples").unwrap().as_f64(),
            Some(0.0)
        );
        drop(pair);
        assert!(!audio.state().open);
        assert_eq!(audio.resume(), Err(RealtimeError::Failed));
        trace(
            "authority",
            serde_json::json!({"encryptedOutbound":1,"sendActions":actions,"suspendGeneration":true,"permitGeneration":true,"queuesCleared":true,"dropClosesSurvivor":true}),
        );
    }

    #[test]
    fn transport_failure_is_sticky_and_clock_cannot_reverse() {
        let mut pair = Pair::new();
        pair.connect();
        let audio = pair.peer.audio();
        audio.resume().unwrap();
        assert_eq!(
            pair.peer
                .timeout(pair.now - Duration::from_secs(1), &mut |_| panic!()),
            Err(RealtimeError::InvalidRequest)
        );
        let generation = audio.state().generation;
        assert!(audio.write_received(&[0.2; 960], generation));
        let mut calls = 0;
        assert_eq!(
            pair.peer.send_frame(pair.now, &mut |_| {
                calls += 1;
                Err(RealtimeError::Failed)
            }),
            Err(RealtimeError::Failed)
        );
        assert_eq!(calls, 1);
        assert!(!audio.state().open);
        assert_eq!(
            pair.peer.connection_result(pair.now),
            Err(RealtimeError::Failed)
        );
        assert_eq!(
            pair.peer
                .timeout(pair.now, &mut |_| panic!("failed engine reused")),
            Err(RealtimeError::Failed)
        );
        pair.peer.close(&mut |_| Ok(())).unwrap();
        assert_eq!(pair.peer.connection_state(), "closed");
        let mut invalid = Pair::new();
        invalid.connect();
        let bad_audio = invalid.peer.audio();
        bad_audio.resume().unwrap();
        let generation = bad_audio.state().generation;
        assert!(bad_audio.write_received(&[f32::NAN; 960], generation));
        assert_eq!(
            invalid
                .peer
                .send_frame(invalid.now, &mut |_| panic!("invalid PCM escaped")),
            Err(RealtimeError::Failed)
        );
        assert_eq!(
            invalid.peer.connection_result(invalid.now),
            Err(RealtimeError::Failed)
        );
        invalid.peer.close(&mut |_| Ok(())).unwrap();
        let mut closing = Pair::new();
        closing.connect();
        let mut closes = 0;
        let result = closing.peer.close(&mut |_| {
            closes += 1;
            Err(RealtimeError::Failed)
        });
        assert_eq!(result, Err(RealtimeError::Failed));
        assert!(closes > 0);
        assert_eq!(
            closing
                .peer
                .close(&mut |_| panic!("close result was not retained")),
            result
        );
        trace(
            "failure",
            serde_json::json!({"clockRejected":true,"transportCalls":calls,"failureSticky":true,"closeAfterFailure":true,"invalidPcmFailed":true,"closeFailureRetained":true}),
        );
    }

    #[test]
    fn permit_revoked_between_admission_and_encoding_emits_nothing() {
        let checks = Arc::new(AtomicUsize::new(0));
        let observed = checks.clone();
        let mut core = crate::media::realtime::pcm::Core::new(
            100,
            Some(0),
            Arc::new(move || observed.fetch_add(1, Ordering::AcqRel) == 0),
        )
        .unwrap();
        core.connected = true;
        core.negotiate(111).unwrap();
        core.resume().unwrap();
        assert!(core.encode().unwrap().is_none());
        assert_eq!(core.sent, 0);
        let state = core.state();
        assert!(!state.open);
        assert_eq!(state.generation, 1);
        trace(
            "permit-race",
            serde_json::json!({"encodedFrames":0,"generation":state.generation,"revokedBeforeEncoding":true}),
        );
    }

    #[test]
    fn dtls_fingerprint_mismatch_never_grants_pcm() {
        let mut pair = Pair::new();
        let offer = SdpOffer::from_sdp_string(pair.peer.offer()).unwrap();
        let answer = pair.remote.sdp_api().accept_offer(offer).unwrap();
        pair.drain_remote();
        let mut answer = answer.to_sdp_string();
        let index = answer.find("a=fingerprint:sha-256 ").unwrap() + "a=fingerprint:sha-256 ".len();
        let replacement = if &answer[index..index + 1] == "0" {
            "1"
        } else {
            "0"
        };
        answer.replace_range(index..index + 1, replacement);
        pair.peer
            .answer(pair.now, &answer, &mut |p| {
                pair.to_remote.push_back(p.into());
                Ok(())
            })
            .unwrap();
        let audio = pair.peer.audio();
        audio.resume().unwrap();
        let mut dtls = 0;
        let mut rejected = false;
        'handshake: for _ in 0..2000 {
            while let Some(p) = pair.to_peer.pop_front() {
                if p.bytes.first().is_some_and(|b| (20..=63).contains(b)) {
                    dtls += 1;
                }
                match pair
                    .peer
                    .receive(pair.now, p.source, p.destination, &p.bytes, &mut |p| {
                        pair.to_remote.push_back(p.into());
                        Ok(())
                    }) {
                    Err(RealtimeError::Failed) => {
                        rejected = true;
                        break 'handshake;
                    }
                    value => {
                        value.unwrap();
                    }
                }
                assert_ne!(pair.peer.connection_result(pair.now), Ok(true));
                assert!(!audio.state().open);
                if pair.peer.connection_result(pair.now) == Err(RealtimeError::Failed) {
                    rejected = true;
                    break 'handshake;
                }
            }
            while let Some(p) = pair.to_remote.pop_front() {
                let receive =
                    Receive::new(Protocol::Udp, p.source, p.destination, &p.bytes).unwrap();
                pair.remote
                    .handle_input(Input::Receive(pair.now, receive))
                    .unwrap();
                pair.drain_remote();
            }
            pair.now += Duration::from_millis(5);
            match pair.peer.timeout(pair.now, &mut |p| {
                pair.to_remote.push_back(p.into());
                Ok(())
            }) {
                Err(RealtimeError::Failed) => {
                    rejected = true;
                    break;
                }
                value => {
                    value.unwrap();
                }
            }
            pair.remote.handle_input(Input::Timeout(pair.now)).unwrap();
            pair.drain_remote();
        }
        assert!(rejected && dtls > 0);
        assert!(!audio.state().open);
        assert_eq!(audio.0.lock().unwrap().received_packets, 0);
        pair.peer.close(&mut |_| Ok(())).unwrap();
        trace(
            "fingerprint",
            serde_json::json!({"dtlsDatagrams":dtls,"mismatchRejected":true,"pcmGranted":false}),
        );
    }
}
