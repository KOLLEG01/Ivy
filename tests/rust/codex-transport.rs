use ivy_host_runtime::{codex_admission as admission, codex_framing as framing};

use admission::{
    AdmissionError, NativeAdmission, PendingRequest, Priority, ResponseOwner, Timeout,
};
use framing::{FrameError, NativeLines};

// Expectations mirror tests/native-rpc.test.ts. These tests exercise framing
// and admission without starting a native provider or executing model work.
#[test]
fn fragmented_utf8_and_multiple_frames_preserve_exact_text() {
    let wire = "\u{feff}{\"result\":\"😀é\"}\r\n{\"params\":null}\n".as_bytes();
    for width in 1..=wire.len() {
        let mut lines = NativeLines::new(100);
        let mut received = Vec::new();
        for chunk in wire.chunks(width) {
            lines
                .receive(chunk, |text| {
                    received.push(text.to_owned());
                    Ok::<_, ()>(())
                })
                .unwrap();
        }
        assert_eq!(received, ["{\"result\":\"😀é\"}\r", "{\"params\":null}"]);
        assert_eq!(lines.finish(), "native_connection_closed");
    }
}

#[test]
fn frame_bound_excludes_lf_but_counts_cr_and_multibyte_utf8() {
    let mut lines = NativeLines::new(4);
    let mut received = Vec::new();
    lines
        .receive("😀\n".as_bytes(), |text| {
            received.push(text.to_owned());
            Ok::<_, ()>(())
        })
        .unwrap();
    assert_eq!(received, ["😀"]);
    assert_eq!(
        lines.receive(b"1234\r\n", |_| Ok::<_, ()>(())),
        Err(FrameError::TooLarge)
    );
    assert_eq!(lines.buffered_bytes(), 0);
    lines
        .receive(b"ignored\n", |_| panic!("closed parser delivered a frame"))
        .unwrap_or_else(|_: FrameError<()>| unreachable!());
}

#[test]
fn incomplete_and_invalid_utf8_close_without_delivering_a_frame() {
    for bytes in [
        vec![0xff, b'\n'],
        vec![0xc0, 0x80, b'\n'],
        vec![0xed, 0xa0, 0x80, b'\n'],
        vec![0xf0, 0x9f, b'\n'],
    ] {
        let mut lines = NativeLines::new(100);
        assert_eq!(
            lines.receive(&bytes, |_| panic!("invalid UTF-8 delivered")),
            Err::<(), FrameError<()>>(FrameError::InvalidUtf8)
        );
    }
    let mut lines = NativeLines::new(100);
    lines.receive(&[0xf0, 0x9f], |_| Ok::<_, ()>(())).unwrap();
    assert_eq!(lines.finish(), "native_truncated_frame");
}

#[test]
fn consumer_failure_stops_later_frames_and_drops_buffer() {
    let mut lines = NativeLines::new(100);
    let mut received = Vec::new();
    assert_eq!(
        lines.receive(b"first\n\nthird\n", |text| {
            received.push(text.to_owned());
            if text.is_empty() {
                Err("native_invalid_frame")
            } else {
                Ok(())
            }
        }),
        Err(FrameError::Consumer("native_invalid_frame"))
    );
    assert_eq!(received, ["first", ""]);
    assert_eq!(lines.buffered_bytes(), 0);
}

#[test]
fn many_small_fragments_are_joined_once_and_enforce_limit_before_newline() {
    let mut lines = NativeLines::new(131_073);
    let expected = "x".repeat(131_073);
    for chunk in expected.as_bytes().chunks(13) {
        lines.receive(chunk, |_| Ok::<_, ()>(())).unwrap();
    }
    assert_eq!(lines.buffered_bytes(), expected.len());
    lines
        .receive(b"\n", |text| {
            assert_eq!(text, expected);
            Ok::<_, ()>(())
        })
        .unwrap();
    assert_eq!(lines.buffered_bytes(), 0);
    lines
        .receive(expected.as_bytes(), |_| Ok::<_, ()>(()))
        .unwrap();
    assert_eq!(
        lines.receive(b"x", |_| Ok::<_, ()>(())),
        Err(FrameError::TooLarge)
    );
}

fn request(detach_on_timeout: bool) -> PendingRequest {
    PendingRequest {
        method: "thread/read".into(),
        timeout_ms: 120_000,
        detach_on_timeout,
    }
}

#[test]
fn background_and_foreground_capacity_reserves_eight_interaction_slots() {
    let mut ledger = NativeAdmission::default();
    for n in 0..56 {
        ledger
            .admit(n.to_string(), request(false), Priority::Background)
            .unwrap();
    }
    assert_eq!(
        ledger.admit("56".into(), request(false), Priority::Background),
        Err(AdmissionError::Capacity)
    );
    for n in 56..64 {
        ledger
            .admit(n.to_string(), request(false), Priority::Interaction)
            .unwrap();
    }
    assert_eq!(
        ledger.admit("65".into(), request(false), Priority::Interaction),
        Err(AdmissionError::Capacity)
    );
    assert_eq!(ledger.pending_count(), 64);
    assert_eq!(
        ledger
            .close()
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>(),
        (0..64).map(|n| n.to_string()).collect::<Vec<_>>()
    );
    assert_eq!(ledger.pending_count(), 0);
}

#[test]
fn expired_observations_keep_ids_reserved_until_validated_late_response() {
    let mut ledger = NativeAdmission::default();
    for n in 0..16 {
        ledger
            .admit(n.to_string(), request(true), Priority::Background)
            .unwrap();
        assert!(matches!(
            ledger.timeout(&n.to_string()),
            Timeout::Detached(_)
        ));
    }
    assert_eq!(ledger.pending_count(), 0);
    assert_eq!(ledger.expired_count(), 16);
    ledger.admit("next".into(), request(true), Priority::Background).unwrap();
    assert_eq!(
        ledger.admit("0".into(), request(false), Priority::Interaction),
        Err(AdmissionError::DuplicateId)
    );
    assert_eq!(ledger.response_owner("0"), ResponseOwner::Expired);
    assert_eq!(ledger.complete("0"), None);
    ledger
        .admit("after-reply".into(), request(true), Priority::Background)
        .unwrap();
    assert_eq!(ledger.response_owner("0"), ResponseOwner::Unknown);
    ledger.close();
    assert_eq!(ledger.expired_count(), 0);
}

#[test]
fn interaction_detached_requests_use_their_existing_larger_budget() {
    let mut ledger = NativeAdmission::default();
    for n in 0..64 {
        ledger
            .admit(n.to_string(), request(true), Priority::Interaction)
            .unwrap();
    }
    assert_eq!(
        ledger.admit("full".into(), request(true), Priority::Interaction),
        Err(AdmissionError::Capacity)
    );
    for n in 0..64 {
        assert!(matches!(
            ledger.timeout(&n.to_string()),
            Timeout::Detached(_)
        ));
    }
    ledger.admit("next".into(), request(true), Priority::Interaction).unwrap();
    ledger
        .admit("cleanup".into(), request(false), Priority::Interaction)
        .unwrap();
}

#[test]
fn hook_failure_and_response_commit_have_distinct_reservation_lifetimes() {
    let mut ledger = NativeAdmission::default();
    ledger
        .admit("id".into(), request(false), Priority::Background)
        .unwrap();
    assert_eq!(ledger.cancel_before_send("id"), Some(request(false)));
    assert_eq!(ledger.expired_count(), 0);
    ledger
        .admit("id".into(), request(false), Priority::Background)
        .unwrap();
    assert_eq!(
        ledger.response_owner("id"),
        ResponseOwner::Pending(&request(false))
    );
    assert_eq!(ledger.timeout("id"), Timeout::CloseConnection);
    assert_eq!(ledger.pending_count(), 1); // owner still owes unknown outcomes
    assert_eq!(ledger.complete("id"), Some(request(false)));
    assert_eq!(ledger.timeout("id"), Timeout::Inactive);
    assert_eq!(ledger.response_owner("id"), ResponseOwner::Unknown);
}

#[test]
fn deadlines_and_duplicate_ids_are_validated_before_admission() {
    let mut ledger = NativeAdmission::default();
    for timeout_ms in [0, 600_001, u64::MAX] {
        assert_eq!(
            ledger.admit(
                "id".into(),
                PendingRequest {
                    timeout_ms,
                    ..request(false)
                },
                Priority::Background
            ),
            Err(AdmissionError::InvalidDeadline)
        );
    }
    assert_eq!(ledger.pending_count(), 0);
    ledger
        .admit(
            "id".into(),
            PendingRequest {
                timeout_ms: 600_000,
                ..request(false)
            },
            Priority::Background,
        )
        .unwrap();
    assert_eq!(
        ledger.admit("id".into(), request(false), Priority::Background),
        Err(AdmissionError::DuplicateId)
    );
    assert_eq!(AdmissionError::DuplicateId.code(), "native_state_conflict");
}
