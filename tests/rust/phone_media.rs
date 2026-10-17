// Native regression cases from tests/native-phone/{Program,RtpTests}.cs.
use ivy_phone_bridge::media::{
    AudioPermit, AudioQueue, MediaError, RtpAudioPacket, RtpReceiveQueue,
};
use std::{collections::VecDeque, time::Duration};

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}
fn packet(sequence: u16) -> RtpAudioPacket {
    RtpAudioPacket {
        sequence,
        timestamp: u32::from(sequence) * 160,
        source: 10,
        payload_type: 0,
        payload: vec![1, 2, 3],
        generation: 1,
    }
}

#[test]
fn permits_expire_fail_closed_and_keep_observation_order_after_expiry() {
    let mut permit = AudioPermit::default();
    assert!(!permit.is_current(ms(100)));
    permit.renew(ms(100), ms(100), ms(100)).unwrap();
    assert!(permit.is_current(ms(199)));
    assert!(!permit.is_current(ms(200)));
    assert_eq!(
        permit.renew(ms(100), ms(100), ms(200)),
        Err(MediaError::StaleObservation)
    );
    assert_eq!(
        permit.renew(ms(201), ms(100), ms(200)),
        Err(MediaError::StaleObservation)
    );
    permit.renew(ms(200), ms(500), ms(200)).unwrap();
    permit.renew(ms(210), ms(500), ms(210)).unwrap();
    assert!(permit.renew(ms(205), ms(500), ms(210)).is_err());
    assert!(!permit.is_current(ms(209)));
    assert!(!permit.is_current(ms(211)));
    permit.renew(ms(211), ms(50), ms(211)).unwrap();
    permit.revoke();
    assert!(!permit.is_current(ms(211)));
    assert_eq!(
        permit.renew(ms(211), ms(50), ms(211)),
        Err(MediaError::Revoked)
    );
    assert_eq!(permit.grant(), Err(MediaError::Revoked));
    for age in [0, 501] {
        assert!(AudioPermit::default().renew(ms(0), ms(age), ms(0)).is_err());
    }
}

#[test]
fn session_grants_and_observation_generations_have_distinct_lifetimes() {
    let mut permit = AudioPermit::default();
    permit.renew(ms(0), ms(100), ms(0)).unwrap();
    let original = permit.generation();
    permit.renew(ms(50), ms(100), ms(50)).unwrap();
    assert_eq!(permit.generation(), original);
    permit.renew(ms(150), ms(100), ms(150)).unwrap();
    assert_eq!(permit.generation(), original + 1);
    assert!(!permit.is_current(ms(250)));
    assert!(permit.renew(ms(140), ms(500), ms(250)).is_err());
    permit.grant().unwrap();
    let session = permit.generation();
    assert!(permit.is_current(ms(86_400_000)));
    permit.grant().unwrap();
    assert_eq!(permit.generation(), session);
    permit.suspend();
    assert!(!permit.is_current(ms(86_400_001)));
    permit.grant().unwrap();
    assert_eq!(permit.generation(), session + 1);
    permit.revoke();
    assert!(permit.grant().is_err());
}

#[test]
fn pcm_ring_matches_fifo_under_wrap_overflow_and_zero_length_reads() {
    let mut queue = AudioQueue::new(23, 0, false, false).unwrap();
    let mut model = VecDeque::new();
    let mut dropped = 0;
    let mut seed = 0x91e0_u32;
    let mut random = || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        seed
    };
    for iteration in 0..10_000 {
        if random() % 3 != 0 {
            let input: Vec<f32> = (0..random() as usize % 80)
                .map(|index| (iteration + index) as f32)
                .collect();
            queue.write(&input);
            model.extend(&input);
            while model.len() > 23 {
                model.pop_front();
                dropped += 1;
            }
        } else {
            let mut output = vec![-1.0; random() as usize % 50];
            let length = model.len().min(output.len());
            assert_eq!(queue.read(&mut output), length);
            for sample in &output[..length] {
                assert_eq!(*sample, model.pop_front().unwrap());
            }
            assert!(output[length..].iter().all(|sample| *sample == 0.0));
        }
        assert_eq!(queue.count(), model.len());
        assert_eq!(queue.dropped(), dropped);
    }
    queue.clear();
    let mut output = [1.0; 23];
    assert_eq!(queue.read(&mut output), 0);
    assert_eq!(output, [0.0; 23]);
    assert!(AudioQueue::new(0, 0, false, false).is_err());
    assert!(AudioQueue::new(9601, 0, false, false).is_err());
    assert!(AudioQueue::new(8, 9, false, false).is_err());
}

#[test]
fn pcm_prebuffer_preserves_speech_and_rearms_only_when_requested() {
    for rebuffer in [false, true] {
        let mut queue = AudioQueue::new(8, 4, rebuffer, false).unwrap();
        let mut block = [1.0; 3];
        queue.write(&[1.0, 2.0, 3.0]);
        assert_eq!(queue.read(&mut block), 0);
        assert_eq!(block, [0.0; 3]);
        assert_eq!(queue.count(), 3);
        queue.write(&[4.0]);
        assert_eq!(queue.read(&mut block), 3);
        assert_eq!(block, [1.0, 2.0, 3.0]);
        assert_eq!(queue.read(&mut block), 1);
        assert_eq!(block, [4.0, 0.0, 0.0]);
        queue.write(&[5.0]);
        assert_eq!(queue.read(&mut block), if rebuffer { 0 } else { 1 });
        queue.clear();
        queue.write(&[9.0]);
        assert_eq!(queue.read(&mut block), 0);
        assert_eq!(queue.count(), 1);
    }
}

#[test]
fn jitter_reserve_plays_every_sample_and_silence_rearms_it() {
    const FRAME: usize = 960;
    const FRAMES: usize = 64;
    let mut queue = AudioQueue::new(FRAME * 4, FRAME * 2, true, false).unwrap();
    let speech = [0.25; FRAME];
    let mut playback = [0.0; FRAME];
    let mut arriving = 0;
    let mut played = 0;
    for time in (0..=(FRAMES - 1) * 20 + 40).step_by(20) {
        while arriving < FRAMES && arriving * 20 + if arriving % 3 == 1 { 12 } else { 0 } <= time {
            queue.write(&speech);
            arriving += 1;
        }
        let length = queue.read(&mut playback);
        if time < 40 {
            assert_eq!(length, 0);
            assert_eq!(playback, [0.0; FRAME]);
        } else {
            assert_eq!(length, FRAME);
            assert_eq!(playback, speech);
            played += 1;
        }
    }
    assert_eq!((played, queue.count(), queue.dropped()), (FRAMES, 0, 0));
    assert_eq!(queue.read(&mut playback), 0);
    queue.write(&speech);
    assert_eq!(queue.read(&mut playback), 0);
    assert_eq!(queue.count(), FRAME);
    queue.write(&speech);
    assert_eq!(queue.read(&mut playback), FRAME);
    assert_eq!(playback, speech);
}

#[test]
fn discontinuity_blending_does_not_delay_or_modify_continuous_audio() {
    const FRAME: usize = 960;
    let mut queue = AudioQueue::new(FRAME * 4, FRAME * 2, true, true).unwrap();
    let mut playback = [0.0; FRAME];
    let mut previous = 0.0;
    let mut continuous = |samples: &[f32]| {
        for sample in samples {
            assert!((*sample - previous).abs() < 0.01);
            previous = *sample;
        }
    };
    queue.write(&[0.25; FRAME * 2]);
    assert_eq!(queue.read(&mut playback), FRAME);
    continuous(&playback);
    assert!(playback[240..].iter().all(|value| *value == 0.25));
    queue.write(&[-0.25; FRAME * 4]);
    assert_eq!(queue.dropped(), FRAME as u64);
    assert_eq!(queue.read(&mut playback), FRAME);
    continuous(&playback);
    for _ in 0..3 {
        queue.read(&mut playback);
        continuous(&playback);
        assert_eq!(playback, [-0.25; FRAME]);
    }
    assert_eq!(queue.read(&mut playback), 0);
    continuous(&playback);
    assert_eq!(playback[FRAME - 1], 0.0);
    let mut partial = AudioQueue::new(FRAME, 0, false, true).unwrap();
    partial.write(&[0.25; 37]);
    assert_eq!(partial.read(&mut playback), 37);
    assert_eq!(playback[36], 0.0);
    continuous(&playback);
    queue.clear();
    queue.write(&[0.25; FRAME]);
    assert_eq!(queue.read(&mut playback), 0);
    assert_eq!(playback, [0.0; FRAME]);
}

#[test]
fn rtp_orders_wrap_duplicates_gap_filler_and_loss_deadline() {
    let mut queue = RtpReceiveQueue::new(20).unwrap();
    assert!(queue.add(&packet(65534), ms(0)).unwrap());
    assert_eq!(queue.take(ms(0), false).unwrap().unwrap().0.sequence, 65534);
    assert!(queue.add(&packet(0), ms(0)).unwrap());
    assert!(queue.take(ms(0), false).unwrap().is_none());
    assert!(!queue.add(&packet(0), ms(0)).unwrap());
    assert_eq!(queue.status().duplicate_packets, 1);
    assert!(queue.add(&packet(65535), ms(0)).unwrap());
    let (p, gap) = queue.take(ms(0), false).unwrap().unwrap();
    assert_eq!(p.sequence, 65535);
    assert!(!gap);
    let (p, gap) = queue.take(ms(0), false).unwrap().unwrap();
    assert_eq!(p.sequence, 0);
    assert!(!gap);
    assert!(!queue.add(&packet(65535), ms(0)).unwrap());
    assert_eq!(queue.status().late_packets, 1);
    queue.add(&packet(2), ms(10)).unwrap();
    assert!(queue.take(ms(29), false).unwrap().is_none());
    let (p, gap) = queue.take(ms(30), false).unwrap().unwrap();
    assert_eq!(p.sequence, 2);
    assert!(gap);
    assert_eq!(queue.status().missing_packets, 1);
    assert_eq!(queue.status().maximum_residence_ms, 20.0);
    assert_eq!(
        queue.take(ms(29), false),
        Err(MediaError::ClockWentBackwards)
    );
}

#[test]
fn rtp_expiry_and_playout_deadline_never_replay_late_gap_fillers() {
    for deadline in [false, true] {
        let mut queue = RtpReceiveQueue::new(20).unwrap();
        queue.add(&packet(0), ms(0)).unwrap();
        queue.take(ms(0), false).unwrap();
        queue.add(&packet(2), ms(0)).unwrap();
        assert!(queue.take(ms(0), false).unwrap().is_none());
        let now = ms(if deadline { 0 } else { 20 });
        if !deadline {
            assert!(!queue.add(&packet(1), now).unwrap());
        }
        let (p, gap) = queue.take(now, deadline).unwrap().unwrap();
        assert_eq!(p.sequence, 2);
        assert!(gap);
        assert_eq!(queue.status().missing_packets, 1);
        assert!(!queue.add(&packet(1), now).unwrap());
        assert!(queue.take(now, false).unwrap().is_none());
    }
}

#[test]
fn rtp_capacity_prefers_near_packets_and_revocation_advances_watermark() {
    let mut queue = RtpReceiveQueue::new(60).unwrap();
    queue.add(&packet(0), ms(0)).unwrap();
    queue.take(ms(0), false).unwrap();
    for sequence in 2..=9 {
        queue.add(&packet(sequence), ms(0)).unwrap();
    }
    assert_eq!(queue.status().queued_packets, 8);
    assert!(!queue.add(&packet(10), ms(0)).unwrap());
    assert!(queue.add(&packet(1), ms(0)).unwrap());
    for sequence in 1..=8 {
        assert_eq!(
            queue.take(ms(0), false).unwrap().unwrap().0.sequence,
            sequence
        );
    }
    assert_eq!(queue.status().overflow_packets, 2);
    assert_eq!(queue.status().queued_packets, 0);
    queue.add(&packet(10), ms(0)).unwrap();
    queue.add(&packet(11), ms(59)).unwrap();
    let (p, gap) = queue.take(ms(60), false).unwrap().unwrap();
    assert_eq!(p.sequence, 10);
    assert!(gap);
    queue.clear();
    assert!(!queue.add(&packet(11), ms(60)).unwrap());
    queue.skip(10, 15, ms(60)).unwrap();
    assert!(!queue.add(&packet(14), ms(60)).unwrap());
    let mut foreign = packet(16);
    foreign.source = 99;
    assert!(!queue.add(&foreign, ms(60)).unwrap());
    assert_eq!(queue.status().foreign_packets, 1);
    let mut original = packet(16);
    original.generation = 2;
    assert!(queue.add(&original, ms(60)).unwrap());
    original.payload.fill(9);
    let (p, gap) = queue.take(ms(60), false).unwrap().unwrap();
    assert!(gap);
    assert_eq!(p.generation, 2);
    assert_eq!(p.payload, [1, 2, 3]);
    queue.reject_peer();
    assert_eq!(queue.status().foreign_packets, 2);
}

#[test]
fn rtp_bounds_invalid_payloads_and_large_loss_without_per_missing_allocation() {
    assert!(RtpReceiveQueue::new(61).is_err());
    let mut queue = RtpReceiveQueue::new(0).unwrap();
    for length in [0, 8193] {
        let mut p = packet(0);
        p.payload = vec![0; length];
        assert_eq!(queue.add(&p, ms(0)), Err(MediaError::InvalidPacket));
    }
    let mut invalid = packet(0);
    invalid.payload_type = 128;
    assert!(queue.add(&invalid, ms(0)).is_err());
    queue.add(&packet(0), ms(0)).unwrap();
    queue.take(ms(0), false).unwrap();
    queue.add(&packet(30000), ms(0)).unwrap();
    let (p, gap) = queue.take(ms(0), false).unwrap().unwrap();
    assert_eq!(p.sequence, 30000);
    assert!(gap);
    assert_eq!(queue.status().missing_packets, 29999);
}

#[test]
fn realtime_stop_retains_reserved_pipe_slots_without_bypassing_receipt_limits() {
    use ivy_phone_bridge::ipc::*;
    for method in [
        "heartbeat",
        "status",
        "call.hangup",
        "call.desktop.stopVoice",
        "call.release",
        "shutdown",
        "call.realtime.stop",
    ] {
        for active in 32..48 {
            assert!(has_pipe_capacity(method, active), "{method}: {active}");
        }
        assert!(!has_pipe_capacity(method, 48));
    }
    assert!(!has_pipe_capacity("call.realtime.prepare", 32));
    assert!(has_pipe_capacity("call.realtime.prepare", 31));
    assert!(!has_receipt_capacity("call.dial", 4092, 4096, 4));
    assert!(has_receipt_capacity("call.realtime.stop", 4092, 4096, 4));
    assert!(!has_receipt_capacity("call.realtime.stop", 4096, 4096, 4));
    assert!(!has_receipt_capacity("call.realtime.stop", 3972, 4096, 128));
    assert!(has_receipt_capacity("call.realtime.stop", 3971, 4096, 128));
    assert!(!is_read_only("call.realtime.stop"));
    assert!(!is_cleanup("heartbeat"));
}

#[test]
fn native_framing_accepts_fragmented_utf8_and_exact_maximum_without_partial_dispatch() {
    use ivy_phone_bridge::ipc::*;
    let mut reader = FrameReader::default();
    let wire = "{\"text\":\"🎤\"}\r\n{}\n".as_bytes();
    let mut frames = Vec::new();
    for byte in wire {
        reader
            .feed(&[*byte], |frame| {
                frames.push(frame.to_owned());
                Ok(())
            })
            .unwrap();
    }
    assert_eq!(frames, ["{\"text\":\"🎤\"}\r", "{}"]);
    reader
        .feed(&vec![b' '; FRAME_BYTES], |_| {
            panic!("partial frame dispatched")
        })
        .unwrap();
    reader
        .feed(b"\n", |frame| {
            assert_eq!(frame.len(), FRAME_BYTES);
            Ok(())
        })
        .unwrap();
    assert_eq!(reader.finish(), Ok(()));
    assert_eq!(reader.feed(b"{}\n", |_| Ok(())), Err(FrameError::Closed));
}

#[test]
fn malformed_native_framing_closes_epoch_before_following_frames() {
    use ivy_phone_bridge::ipc::*;
    for (wire, expected) in [
        (vec![b' '; FRAME_BYTES + 1], FrameError::Oversized),
        (
            vec![0xff, b'\n', b'{', b'}', b'\n'],
            FrameError::InvalidUtf8,
        ),
        (b"\n{}\n".to_vec(), FrameError::Empty),
    ] {
        let mut reader = FrameReader::default();
        assert_eq!(
            reader.feed(&wire, |_| panic!("invalid frame admitted")),
            Err(expected)
        );
        assert_eq!(reader.feed(b"{}\n", |_| Ok(())), Err(FrameError::Closed));
    }
    let mut reader = FrameReader::default();
    reader
        .feed(b"{", |_| panic!("partial frame admitted"))
        .unwrap();
    assert_eq!(reader.finish(), Err(FrameError::Truncated));
    let mut reader = FrameReader::default();
    let mut calls = 0;
    assert_eq!(
        reader.feed(b"[]\n{}\n", |_| {
            calls += 1;
            Err(FrameError::InvalidRequest)
        }),
        Err(FrameError::InvalidRequest)
    );
    assert_eq!(calls, 1);
}
