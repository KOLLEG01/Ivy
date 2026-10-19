use ivy_contracts::{IvyJson, json::stringify};
use ivy_phone_bridge::media::{
    AudioFormat, Codec, CodecError, EvsDecoder, EvsEncoder, EvsLibrary, MediaCodec, PcmAudioPort,
    PcmAudioState, RtpAudioPacket,
};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex},
};

fn library() -> Option<Arc<EvsLibrary>> {
    let path = std::env::var_os("IVY_PHONE_EVS_DIR")?;
    // SAFETY: Qualification verifies the pinned authored/reference build and
    // DLL hash before execution. This is the exact ABI2 native codec fixture.
    Some(unsafe { EvsLibrary::load_packaged(&PathBuf::from(path)) }.unwrap())
}
fn evs_format(channels: u8, full: bool, rate: u32, bandwidth: &str) -> AudioFormat {
    AudioFormat::new(
        112,
        "EVS",
        16000,
        16000,
        channels,
        Some(&format!(
            "br={}.{};bw={bandwidth};hf-only={};channels={channels}",
            rate / 1000,
            rate % 1000 / 100,
            usize::from(full)
        )),
    )
    .unwrap()
}
fn signal(position: usize) -> i16 {
    ((position as f64 * 997.0 * std::f64::consts::TAU / 48000.0).sin() * 8000.0) as i16
}
fn trace(variable: &str, value: IvyJson) {
    if let Some(path) = std::env::var_os(variable) {
        std::fs::write(path, stringify(&value, 8_000_000).unwrap()).unwrap();
    }
}

#[tokio::test]
async fn evs_real_reference_abi_all_profiles_and_complete_receive_blocks() {
    let Some(library) = library() else {
        eprintln!("Real EVS reference library unavailable; EVS ABI/profile checks not executed.");
        return;
    };
    let executable = PathBuf::from(
        std::env::var_os("IVY_PHONE_EVS_TEST").expect("Pinned original C test required"),
    );
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(90),
        tokio::process::Command::new(executable)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("Original synthetic C codec test exceeded its owned deadline")
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut profiles = 0;
    let mut packets = 0;
    let mut blocks = 0;
    for rate in [
        7200, 8000, 9600, 13200, 16400, 24400, 32000, 48000, 64000, 96000, 128000,
    ] {
        for (bw, name) in [(0, "nb"), (1, "wb"), (2, "swb"), (3, "fb")] {
            if bw == 0 && rate > 24400 || bw == 2 && rate < 9600 || bw == 3 && rate < 16400 {
                continue;
            }
            for (channels, full) in [(1, false), (1, true), (2, true)] {
                let format = evs_format(channels, full, rate, name);
                let mut encoder = EvsEncoder::new(library.clone(), &format).unwrap();
                let mut independent = EvsEncoder::new(library.clone(), &format).unwrap();
                let mut decoder = EvsDecoder::new(library.clone(), &format).unwrap();
                let mut recent = Vec::new();
                for frame in 0..8 {
                    let pcm = std::array::from_fn(|i| signal(frame * 960 + i));
                    let packet = encoder.encode_20ms(&pcm).unwrap();
                    assert_eq!(packet, independent.encode_20ms(&pcm).unwrap());
                    assert_eq!(
                        packet.len(),
                        channels as usize * (rate as usize / 400 + usize::from(full))
                    );
                    let saved = packet.clone();
                    let decoded = decoder.decode(&packet).unwrap();
                    assert_eq!(decoded.len(), 960);
                    assert_eq!(packet, saved);
                    if channels == 2 {
                        let bytes = rate as usize / 400;
                        assert_eq!(&packet[2..2 + bytes], &packet[2 + bytes..]);
                    }
                    recent.push(packet);
                    packets += 1;
                }
                assert_eq!(decoder.decode(&[]), Err(CodecError::InvalidFrame));
                assert_eq!(
                    decoder.decode(&vec![0; 4097]),
                    Err(CodecError::InvalidFrame)
                );
                assert_eq!(decoder.decode(&[13]), Err(CodecError::InvalidFrame));
                assert_eq!(decoder.decode(&recent[7]).unwrap().len(), 960);
                let bytes = rate as usize / 400;
                let header = if full { channels as usize } else { 0 };
                let type_id = [
                    7200, 8000, 9600, 13200, 16400, 24400, 32000, 48000, 64000, 96000, 128000,
                ]
                .iter()
                .position(|r| *r == rate)
                .unwrap()
                    + 1;
                let full_format = evs_format(channels, true, rate, name);
                let mut grouped = EvsDecoder::new(library.clone(), &full_format).unwrap();
                for length in 1..=6 {
                    let frames = length * channels as usize;
                    let mut packet: Vec<u8> = (0..frames)
                        .map(|i| type_id as u8 | if i + 1 < frames { 0x40 } else { 0 })
                        .collect();
                    for recent in recent.iter().take(length) {
                        packet
                            .extend_from_slice(&recent[header..header + channels as usize * bytes]);
                    }
                    assert_eq!(grouped.decode(&packet).unwrap().len(), length * 960);
                    blocks += 1;
                }
                profiles += 1;
            }
        }
    }
    assert_eq!(profiles, 99);
    trace(
        "IVY_PHONE_EVS_TRACE",
        IvyJson::object([
            ("profiles", IvyJson::Number(profiles as f64)),
            ("packets", IvyJson::Number(packets as f64)),
            ("groupedReceiveChecks", IvyJson::Number(blocks as f64)),
            (
                "referenceCTestOutput",
                String::from_utf8_lossy(&output.stdout).to_string().into(),
            ),
        ]),
    );
}

#[cfg(windows)]
#[test]
fn evs_loader_rejects_wrong_abi_missing_symbol_and_retains_module_until_last_handle() {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    let Some(root) = std::env::var_os("IVY_PHONE_EVS_LOADER_FIXTURES") else {
        eprintln!("EVS loader fixtures unavailable; loader lifetime/rejection check not executed.");
        return;
    };
    let root = PathBuf::from(root);
    // SAFETY: All rejection fixtures have the declared bounded C signatures;
    // they are separately built/pinned synthetic DLLs with no backend effects.
    assert!(unsafe { EvsLibrary::load_packaged(std::path::Path::new("relative")) }.is_err());
    for name in ["wrong-abi", "missing-export", "absent"] {
        assert!(unsafe { EvsLibrary::load_packaged(&root.join(name)) }.is_err());
    }
    let directory = root.join("lifetime");
    let path = directory.join(EvsLibrary::FILE_NAME);
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: Valid terminated full module name; query does not acquire a reference.
    assert!(unsafe { GetModuleHandleW(wide.as_ptr()) }.is_null());
    let library = unsafe { EvsLibrary::load_packaged(&directory) }.unwrap();
    let weak = Arc::downgrade(&library);
    let mut encoder = EvsEncoder::new(library.clone(), &Codec::Evs.offered_formats()[0]).unwrap();
    drop(library);
    assert!(weak.upgrade().is_some());
    assert!(!unsafe { GetModuleHandleW(wide.as_ptr()) }.is_null());
    assert_eq!(encoder.encode_20ms(&[0; 960]).unwrap().len(), 320);
    drop(encoder);
    assert!(weak.upgrade().is_none());
    assert!(unsafe { GetModuleHandleW(wide.as_ptr()) }.is_null());
}

struct State {
    authority: PcmAudioState,
    position: usize,
    amplitude: f32,
    source: bool,
    relay: bool,
    queue: VecDeque<f32>,
    samples: Vec<f32>,
    revoke_read: bool,
    churn_write: bool,
    writes: usize,
    churn_write_at: Option<usize>,
    state_calls: usize,
    churn_state_at: Option<usize>,
}
struct Port(Mutex<State>);
impl Port {
    fn new(source: bool, relay: bool) -> Arc<Self> {
        Arc::new(Self(Mutex::new(State {
            authority: PcmAudioState {
                open: true,
                generation: 1,
            },
            position: 0,
            amplitude: 0.2,
            source,
            relay,
            queue: VecDeque::new(),
            samples: Vec::new(),
            revoke_read: false,
            churn_write: false,
            writes: 0,
            churn_write_at: None,
            state_calls: 0,
            churn_state_at: None,
        })))
    }
    fn received(&self) -> usize {
        self.0.lock().unwrap().samples.len()
    }
    fn change(&self, open: bool) {
        let mut s = self.0.lock().unwrap();
        s.authority.open = open;
        s.authority.generation += 1;
        s.queue.clear();
    }
}
impl PcmAudioPort for Port {
    fn state(&self) -> PcmAudioState {
        let mut state = self.0.lock().unwrap();
        state.state_calls += 1;
        let observed = state.authority;
        if state.churn_state_at == Some(state.state_calls) {
            state.churn_state_at = None;
            state.authority.generation += 1;
        }
        observed
    }
    fn read_captured(&self, output: &mut [f32], generation: u64) -> usize {
        let mut s = self.0.lock().unwrap();
        output.fill(0.0);
        if s.authority
            != (PcmAudioState {
                open: true,
                generation,
            })
        {
            return 0;
        }
        for value in output.iter_mut() {
            *value = if s.source {
                (s.position as f64 * 997.0 * std::f64::consts::TAU / 48000.0).sin() as f32
                    * s.amplitude
            } else {
                s.queue.pop_front().unwrap_or(0.0)
            };
            s.position += 1;
        }
        if s.revoke_read {
            s.revoke_read = false;
            s.authority.open = false;
            s.authority.generation += 1;
        }
        output.len()
    }
    fn write_received(&self, input: &[f32], generation: u64) -> bool {
        let mut s = self.0.lock().unwrap();
        if s.authority
            != (PcmAudioState {
                open: true,
                generation,
            })
        {
            return false;
        }
        assert!(input.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
        s.samples.extend_from_slice(input);
        if s.relay {
            s.queue.extend(input);
        }
        s.writes += 1;
        if s.churn_write || s.churn_write_at == Some(s.writes) {
            s.churn_write = false;
            s.churn_write_at = None;
            s.authority.generation += 1;
        }
        true
    }
}
fn formats(evs: bool) -> Vec<AudioFormat> {
    let mut result = vec![
        Codec::Pcma.offered_formats()[0].clone(),
        Codec::Pcmu.offered_formats()[0].clone(),
        Codec::G722.offered_formats()[0].clone(),
        Codec::Opus.offered_formats()[0].clone(),
    ];
    if evs {
        for (ch, full) in [(1, false), (1, true), (2, true)] {
            result.push(evs_format(ch, full, 128000, "fb"));
        }
    }
    result
}

#[test]
fn media_codec_generation_reset_bounds_clipping_and_continuous_opus_relay() {
    let library = library();
    let mut reports = Vec::new();
    assert!(
        MediaCodec::new(
            Codec::Evs.offered_formats()[0].clone(),
            Port::new(true, false),
            None
        )
        .is_err()
    );
    for format in formats(library.is_some()) {
        let source = Port::new(true, false);
        let relay = Port::new(false, true);
        let sink = Port::new(false, false);
        let opus = Codec::Opus.offered_formats()[0].clone();
        let mut voice_tx = MediaCodec::new(opus.clone(), source.clone(), None).unwrap();
        let mut voice_rx = MediaCodec::new(opus, relay.clone(), None).unwrap();
        let mut tx = MediaCodec::new(format.clone(), relay.clone(), library.clone()).unwrap();
        let mut rx = MediaCodec::new(format.clone(), sink.clone(), library.clone()).unwrap();
        let expected = if format.codec() == Codec::Opus {
            960
        } else if format.codec() == Codec::Evs {
            320
        } else {
            160
        };
        assert_eq!(tx.rtp_duration(), expected);
        for _ in 0..200 {
            voice_rx.decode(&voice_tx.encode().unwrap(), None).unwrap();
            rx.decode(&tx.encode().unwrap(), None).unwrap();
        }
        let samples = sink.0.lock().unwrap().samples[9600..].to_vec();
        let jump = samples
            .windows(2)
            .map(|s| (s[1] - s[0]).abs())
            .fold(0.0_f32, f32::max);
        let rms = (samples.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / samples.len() as f64)
            .sqrt();
        assert!(
            samples.len() > 170000 && rms > 0.08 && rms < 0.3 && jump < 0.15,
            "{} rms={rms} jump={jump}",
            format.name()
        );
        // Capture changes authority after filling a frame: the returned packet
        // must be newly encoded silence, with no prior prediction/filter state.
        relay.0.lock().unwrap().revoke_read = true;
        let revoked = tx.encode().unwrap();
        let clean = Port::new(false, false);
        let mut fresh = MediaCodec::new(format.clone(), clean.clone(), library.clone()).unwrap();
        fresh.decode(&revoked, None).unwrap();
        assert!(
            clean
                .0
                .lock()
                .unwrap()
                .samples
                .iter()
                .all(|s| s.abs() < 0.01)
        );
        let closed = tx.encode().unwrap();
        fresh.reset();
        fresh.decode(&closed, None).unwrap();
        assert!(
            clean
                .0
                .lock()
                .unwrap()
                .samples
                .iter()
                .all(|s| s.abs() < 0.01)
        );
        let before = sink.received();
        sink.change(false);
        rx.decode(&vec![0; 8193], None).unwrap();
        assert_eq!(sink.received(), before);
        sink.change(true);
        rx.decode(&vec![0; 8193], Some(1)).unwrap();
        assert_eq!(sink.received(), before);
        assert!(rx.decode(&vec![0; 8193], None).is_err());
        relay.change(true);
        relay.0.lock().unwrap().queue.clear();
        let silent = tx.encode().unwrap();
        fresh.reset();
        fresh.decode(&silent, None).unwrap();
        assert!(
            clean
                .0
                .lock()
                .unwrap()
                .samples
                .iter()
                .all(|s| s.abs() < 0.01)
        );
        reports.push(IvyJson::object([
            ("codec", format.name().into()),
            ("channels", IvyJson::Number(format.channels() as f64)),
            ("rtpDuration", IvyJson::Number(expected as f64)),
            ("samples", IvyJson::Number(samples.len() as f64)),
            ("rms", IvyJson::Number(rms)),
            ("maximumJump", IvyJson::Number(jump as f64)),
        ]));
    }
    // Clipping and ties-to-even conversion on the identity path have no filter.
    let port = Port::new(true, false);
    port.0.lock().unwrap().amplitude = 4.0;
    let format = Codec::Opus.offered_formats()[0].clone();
    let mut owner = MediaCodec::new(format, port.clone(), None).unwrap();
    let packet = owner.encode().unwrap();
    owner.decode(&packet, None).unwrap();
    port.0.lock().unwrap().amplitude = f32::NAN;
    assert!(owner.encode().is_err());
    trace("IVY_PHONE_MEDIA_OWNER_TRACE", IvyJson::Array(reports));
}

fn rtp(packet: Vec<u8>, timestamp: u32, generation: u64) -> RtpAudioPacket {
    RtpAudioPacket {
        sequence: 0,
        timestamp,
        source: 1,
        payload_type: 111,
        payload: packet,
        generation,
    }
}
#[test]
fn media_codec_opus_timeline_gap_reset_and_timestamp_wrap() {
    let format = Codec::Opus.offered_formats()[0].clone();
    let source = Port::new(true, false);
    let sink = Port::new(false, false);
    let mut tx = MediaCodec::new(format.clone(), source, None).unwrap();
    let mut rx = MediaCodec::new(format, sink.clone(), None).unwrap();
    let mut position = 0_u32;
    let mut losses = 0;
    for frame in 0..200 {
        let packet = tx.encode().unwrap();
        if frame > 10 && frame % 17 == 0 {
            position += 960;
            losses += 1;
            continue;
        }
        rx.decode_rtp(&rtp(packet, position, 1)).unwrap();
        position += 960;
    }
    assert_eq!(sink.received(), 200 * 960);
    assert_eq!(rx.concealed_samples(), losses * 960);
    {
        let state = sink.0.lock().unwrap();
        assert!(
            state.samples[9600..]
                .windows(2)
                .map(|s| (s[1] - s[0]).abs())
                .fold(0.0_f32, f32::max)
                < 0.15
        );
    }
    let before = sink.received();
    sink.change(true);
    rx.decode_rtp(&rtp(vec![], position, 1)).unwrap();
    assert_eq!(sink.received(), before);
    let packet = tx.encode().unwrap();
    rx.decode_rtp(&rtp(packet.clone(), u32::MAX - 959, 2))
        .unwrap();
    let before = sink.received();
    let concealed = rx.concealed_samples();
    rx.decode_rtp(&rtp(packet.clone(), 960, 2)).unwrap();
    assert_eq!(sink.received() - before, 1920);
    assert_eq!(rx.concealed_samples() - concealed, 960);
    let before = sink.received();
    let concealed = rx.concealed_samples();
    rx.decode_rtp(&rtp(packet, 100000, 2)).unwrap();
    assert_eq!(sink.received() - before, 960);
    assert_eq!(rx.concealed_samples(), concealed);
    for timestamp in [100000, 99040, 100001] {
        let before = sink.received();
        let concealed = rx.concealed_samples();
        rx.decode_rtp(&rtp(tx.encode().unwrap(), timestamp, 2))
            .unwrap();
        assert_eq!(sink.received() - before, 960);
        assert_eq!(rx.concealed_samples(), concealed);
    }
    sink.change(false);
    let before = sink.received();
    rx.decode_rtp(&rtp(vec![], 0, 3)).unwrap();
    assert_eq!(sink.received(), before);
    sink.change(true);
    rx.decode_rtp(&rtp(tx.encode().unwrap(), 0, 4)).unwrap();
    let before = sink.received();
    let concealed = rx.concealed_samples();
    rx.decode_rtp(&rtp(tx.encode().unwrap(), 6720, 4)).unwrap();
    assert_eq!(sink.received() - before, 6720);
    assert_eq!(rx.concealed_samples() - concealed, 5760);
}

fn opus_payload(duration: usize) -> Vec<u8> {
    let frame = duration.min(960);
    let mut encoder =
        opus::Encoder::new(48000, opus::Channels::Mono, opus::Application::Voip).unwrap();
    encoder.set_bitrate(opus::Bitrate::Bits(24000)).unwrap();
    encoder.set_vbr(false).unwrap();
    encoder.set_inband_fec(true).unwrap();
    encoder.set_packet_loss_perc(30).unwrap();
    let mut packet = vec![0; 8192];
    for index in 0..80 {
        let samples: Vec<_> = (index * frame..(index + 1) * frame).map(signal).collect();
        let length = encoder.encode(&samples, &mut packet).unwrap();
        if index == 79 {
            packet.truncate(length);
        }
    }
    if duration <= 960 {
        return packet;
    }
    let parts = vec![packet.as_slice(); duration / 960];
    let mut combined = vec![0; 8192];
    let count = opus::Repacketizer::new()
        .unwrap()
        .combine(&parts, &mut combined)
        .unwrap();
    combined.truncate(count);
    combined
}

#[test]
fn media_codec_approved_packet_generation_fences_every_recovery_boundary() {
    let mut reports = Vec::new();
    for (duration, gap, recovery_writes) in [
        (120, 480, 1),
        (240, 480, 1),
        (480, 960, 2),
        (960, 960, 1),
        (960, 2880, 3),
        (1920, 5760, 5),
        (5760, 5760, 1),
    ] {
        let packet = opus_payload(duration);
        for (kind, limit) in [
            ("write", recovery_writes + 1),
            ("state", 2 * recovery_writes + 5),
        ] {
            for stage in 1..=limit {
                let sink = Port::new(false, false);
                let mut rx =
                    MediaCodec::new(Codec::Opus.offered_formats()[0].clone(), sink.clone(), None)
                        .unwrap();
                rx.decode_rtp(&rtp(packet.clone(), 0, 1)).unwrap();
                {
                    let mut state = sink.0.lock().unwrap();
                    state.writes = 0;
                    state.state_calls = 0;
                    if kind == "write" {
                        state.churn_write_at = Some(stage);
                    } else {
                        state.churn_state_at = Some(stage);
                    }
                }
                let old_timestamp = (duration + gap) as u32;
                rx.decode_rtp(&rtp(packet.clone(), old_timestamp, 1))
                    .unwrap();
                let generation = sink.0.lock().unwrap().authority.generation;
                assert_eq!(
                    generation, 2,
                    "unreached callback {duration}/{gap}/{kind}/{stage}"
                );
                let before = sink.received();
                let concealed = rx.concealed_samples();
                let next_timestamp = old_timestamp + duration as u32 * 2;
                rx.decode_rtp(&rtp(packet.clone(), next_timestamp, generation))
                    .unwrap();
                assert_eq!(
                    sink.received() - before,
                    duration,
                    "old timestamp seed at {duration}/{gap}/{kind}/{stage}"
                );
                assert_eq!(
                    rx.concealed_samples(),
                    concealed,
                    "new generation inherited concealment"
                );
                rx.decode_rtp(&rtp(
                    packet.clone(),
                    next_timestamp + duration as u32,
                    generation,
                ))
                .unwrap();
                assert_eq!(sink.received() - before, 2 * duration);
                assert_eq!(rx.concealed_samples(), concealed);
                // A second revocation proves history does not survive a later
                // epoch either. Counters retain the old work; no rollback.
                sink.0.lock().unwrap().churn_write = true;
                let old_again = next_timestamp + 2 * duration as u32 + gap as u32;
                rx.decode_rtp(&rtp(packet.clone(), old_again, generation))
                    .unwrap();
                let next_generation = sink.0.lock().unwrap().authority.generation;
                assert_eq!(next_generation, 3);
                let before = sink.received();
                let second_concealed = rx.concealed_samples();
                assert!(second_concealed >= concealed);
                rx.decode_rtp(&rtp(
                    packet.clone(),
                    old_again + 2 * duration as u32,
                    next_generation,
                ))
                .unwrap();
                assert_eq!(sink.received() - before, duration);
                assert_eq!(rx.concealed_samples(), second_concealed);
                reports.push(IvyJson::object([
                    ("packetSamples", IvyJson::Number(duration as f64)),
                    ("gapSamples", IvyJson::Number(gap as f64)),
                    ("callbackKind", kind.into()),
                    ("callbackStage", IvyJson::Number(stage as f64)),
                    (
                        "firstNewGenerationSamples",
                        IvyJson::Number(duration as f64),
                    ),
                    ("inheritedConcealmentSamples", IvyJson::Number(0.0)),
                    (
                        "retainedOldConcealedSamples",
                        IvyJson::Number(concealed as f64),
                    ),
                    (
                        "repeatedGenerationSamples",
                        IvyJson::Number(duration as f64),
                    ),
                ]));
            }
        }
    }
    trace("IVY_PHONE_MEDIA_GENERATION_TRACE", IvyJson::Array(reports));
}
