use ivy_phone_bridge::{
    ipc::{FRAME_BYTES, FrameError},
    native_rpc::{Admission, NativeOutcome, NativeReceipts, NativeRequest},
};
use std::sync::Arc;
#[path = "phone_audio_mute.rs"]
mod audio_mute;
#[path = "phone_audio_sender.rs"]
mod audio_sender;
#[path = "phone_call_audio.rs"]
mod call_audio;
#[path = "phone_call_features.rs"]
mod call_features;
#[cfg(windows)]
#[path = "phone_loopback_probe.rs"]
mod loopback_probe;
#[path = "phone_media_codec.rs"]
mod media_codec;
#[path = "phone_media_format.rs"]
mod media_format;
#[cfg(windows)]
#[path = "phone_micro_transport.rs"]
mod micro_transport;
#[path = "phone_native_config.rs"]
mod native_config;
#[path = "phone_native_dispatch.rs"]
mod native_dispatch;
#[path = "phone_native_pipe.rs"]
mod native_pipe;
#[path = "phone_pcm_codec.rs"]
mod pcm_codec;
#[path = "phone_register.rs"]
mod register;
#[path = "phone_registration.rs"]
mod registration_contracts;
#[path = "phone_resampler.rs"]
mod resampler;
#[path = "phone_rtcp.rs"]
mod rtcp;
#[path = "phone_rtcp_owner.rs"]
mod rtcp_owner;
#[path = "phone_rtcp_session.rs"]
mod rtcp_session;
#[path = "phone_rtcp_state.rs"]
mod rtcp_state;
#[path = "phone_rtp_transport.rs"]
mod rtp_transport;
#[path = "phone_sdp.rs"]
mod sdp;
#[path = "phone_sip.rs"]
mod sip;
#[path = "phone_sip_host.rs"]
mod sip_host;
#[path = "phone_sip_receiver.rs"]
mod sip_receiver;
#[path = "phone_sip_stack.rs"]
mod sip_stack;
#[path = "phone_voice_rpc.rs"]
mod voice_rpc;

const EPOCH: &str = "01234567-89ab-cdef-0123-456789abcdef";
const OP: &str = "11234567-89ab-cdef-0123-456789abcdef";
fn envelope(method: &str, operation: Option<&str>, params: &str, request_id: u64) -> String {
    format!(
        r#"{{"version":1,"epoch":"{EPOCH}","requestId":{request_id},"operationId":{},"method":"{method}","params":{params}}}"#,
        operation
            .map(|s| format!("\"{s}\""))
            .unwrap_or("null".into())
    )
}
fn request(method: &str, operation: Option<&str>, params: &str, request_id: u64) -> NativeRequest {
    NativeRequest::parse(envelope(method, operation, params, request_id).as_bytes()).unwrap()
}
fn rejected(admission: Admission, code: &str) {
    match admission {
        Admission::Reject(actual) => assert_eq!(actual, code),
        _ => panic!("Expected rejection {code}"),
    }
}

mod rpc_contracts {
    use super::*;

    #[test]
    fn exact_envelope_and_integer_lexemes() {
        let valid = envelope("status", None, "{}", 1);
        for field in ["version", "requestId"] {
            for number in [
                "0",
                "-1",
                "1.0",
                "1e0",
                "1E+0",
                "1",
                "9007199254740991",
                "9007199254740992",
                "1e999",
                "null",
                "\"1\"",
            ] {
                let frame =
                    valid.replace(&format!("\"{field}\":1"), &format!("\"{field}\":{number}"));
                assert_eq!(
                    NativeRequest::parse(frame.as_bytes()).is_ok(),
                    number == "1" || field == "requestId" && number == "9007199254740991",
                    "{field}: {number}"
                );
            }
        }
        for invalid in [
            valid.replace("\"version\":1", "\"version\":1.0"),
            valid.replace("\"version\":1", "\"version\":1e0"),
            valid.replace("\"requestId\":1", "\"requestId\":1.0"),
            valid.replace("\"requestId\":1", "\"requestId\":1e0"),
            valid.replace("\"requestId\":1", "\"requestId\":0"),
            valid.replace("\"requestId\":1", "\"requestId\":9007199254740992"),
            valid.replace("\"version\":1", "\"version\":1,\"version\":1"),
            valid.replace("\"version\":1", "\"version\":1,\"other\":null"),
            valid.replace("\"version\":1,", ""),
            envelope("status", Some(OP), "{}", 1),
            envelope("call.dial", None, "{}", 1),
            envelope("call.screening.synthesize", Some(OP), "{}", 1),
            envelope("status", None, "[]", 1),
            envelope("status", None, "null", 1),
            envelope("status", None, "1", 1),
            format!("\u{feff}{valid}"),
            format!("[{valid}]"),
            format!("{valid}{{}}"),
        ] {
            assert!(
                NativeRequest::parse(invalid.as_bytes()).is_err(),
                "Accepted {invalid}"
            );
        }
        let escaped = valid.replace("\"version\"", "\"vers\\u0069on\"");
        assert!(NativeRequest::parse(escaped.as_bytes()).is_ok());
        assert!(
            NativeRequest::parse(envelope("status", None, "{}", 9_007_199_254_740_991).as_bytes())
                .is_ok()
        );
    }

    #[test]
    fn nested_duplicates_depth_utf8_and_size() {
        for params in [
            r#"{"a":1,"a":2}"#,
            r#"{"a":[{"a":1,"\u0061":2}]}"#,
            r#"{"\ud800":null}"#,
            r#"{"\udfff":null}"#,
        ] {
            assert!(NativeRequest::parse(envelope("status", None, params, 1).as_bytes()).is_err());
        }
        // Root envelope + params object + 22 arrays is the native depth ceiling.
        let nested = |count| format!("{{\"value\":{}0{}}}", "[".repeat(count), "]".repeat(count));
        assert!(NativeRequest::parse(envelope("status", None, &nested(22), 1).as_bytes()).is_ok());
        assert!(NativeRequest::parse(envelope("status", None, &nested(23), 1).as_bytes()).is_err());
        assert!(
            NativeRequest::parse(
                envelope("status", None, r#"{"value":"\ud800","huge":1e999}"#, 1).as_bytes()
            )
            .is_ok()
        );
        assert!(matches!(
            NativeRequest::parse(&[0xff]),
            Err(FrameError::InvalidUtf8)
        ));
        assert!(matches!(NativeRequest::parse(&[]), Err(FrameError::Empty)));
        let mut padded = envelope("status", None, "{}", 1);
        padded.extend(std::iter::repeat_n(' ', FRAME_BYTES - padded.len()));
        assert!(NativeRequest::parse(padded.as_bytes()).is_ok());
        padded.push(' ');
        assert!(matches!(
            NativeRequest::parse(padded.as_bytes()),
            Err(FrameError::Oversized)
        ));
    }

    #[test]
    fn raw_spans_are_independent_of_envelope_order_and_escapes() {
        let params = r#"{ "x" : ["}", "\"[", {"n":1e0}], "a": "\u0062" }"#;
        let frame = format!(
            r#" {{ "params" : {params}, "method":"call.dial", "operationId":"{OP}", "requestId":7, "epoch":"{EPOCH}", "version":1 }} "#
        );
        let parsed = NativeRequest::parse(frame.as_bytes()).unwrap();
        assert_eq!(parsed.raw_params(), params);
        assert_eq!(parsed.request_id(), 7);
        let mut book = NativeReceipts::new(EPOCH.into(), 10).unwrap();
        assert!(matches!(
            book.admit(&parsed, 4),
            Admission::Dispatch(Some(_))
        ));
        let replay = request("call.dial", Some(OP), params, 8);
        assert!(matches!(book.admit(&replay, 4), Admission::Replay(_)));
        rejected(
            book.admit(
                &request("call.dial", Some(OP), &params.replace("1e0", "1"), 9),
                4,
            ),
            "operation_conflict",
        );
        rejected(
            book.admit(
                &request("call.dial", Some(OP), &params.replace("\\u0062", "b"), 9),
                4,
            ),
            "operation_conflict",
        );
        rejected(
            book.admit(&request("call.answer", Some(OP), params, 9), 4),
            "operation_conflict",
        );
    }

    #[test]
    fn pending_replays_share_completion_without_reinvocation_or_eviction() {
        let mut book = NativeReceipts::new(EPOCH.into(), 1).unwrap();
        let original = request(
            "call.dial",
            Some(OP),
            r#"{"secret":"never retained in receipt"}"#,
            1,
        );
        let Admission::Dispatch(Some(first)) = book.admit(&original, 4) else {
            panic!()
        };
        let Admission::Replay(second) = book.admit(&original, 4) else {
            panic!()
        };
        assert!(Arc::ptr_eq(&first, &second));
        assert!(first.outcome().is_none());
        first
            .complete(NativeOutcome::result(r#"{"callId":"safe"}"#.into()))
            .unwrap();
        assert_eq!(
            second.outcome().unwrap().result_json(),
            Some(r#"{"callId":"safe"}"#)
        );
        assert!(
            second
                .complete(NativeOutcome::error("operation_outcome_unknown"))
                .is_err()
        );
        assert_eq!(book.len(), 1);
        rejected(
            book.admit(&request("call.dial", Some(EPOCH), "{}", 2), 0),
            "capacity_exceeded",
        );
        assert!(matches!(
            book.admit(&request("status", None, "{}", 3), 0),
            Admission::Dispatch(None)
        ));
    }

    #[test]
    fn cleanup_reserve_and_stop_precede_replay() {
        let mut book = NativeReceipts::new(EPOCH.into(), 5).unwrap();
        let original = request("call.dial", Some(OP), "{}", 1);
        assert!(matches!(book.admit(&original, 4), Admission::Dispatch(_)));
        rejected(
            book.admit(&request("call.dial", Some(EPOCH), "{}", 2), 4),
            "capacity_exceeded",
        );
        assert!(matches!(
            book.admit(&request("call.realtime.stop", Some(EPOCH), "{}", 2), 4),
            Admission::Dispatch(_)
        ));
        book.stop();
        assert!(!book.may_invoke());
        rejected(book.admit(&original, 4), "runtime_stopping");
        rejected(
            book.admit(&request("status", None, "{}", 3), 4),
            "runtime_stopping",
        );
        let wrong = envelope("status", None, "{}", 3).replace(EPOCH, OP);
        rejected(
            book.admit(&NativeRequest::parse(wrong.as_bytes()).unwrap(), 4),
            "epoch_mismatch",
        );
    }

    #[test]
    fn guid_spelling_is_validated_but_never_normalized() {
        for guid in [
            EPOCH.to_owned(),
            EPOCH.to_uppercase(),
            format!(" {EPOCH} "),
            format!("\\t{EPOCH}\\n"),
            "00000000-0000-0000-0000-000000000000".into(),
        ] {
            let frame = envelope("status", None, "{}", 1).replace(EPOCH, &guid);
            assert!(NativeRequest::parse(frame.as_bytes()).is_ok(), "{guid}");
        }
        for guid in [EPOCH.replace('-', ""), format!("{{{EPOCH}}}")] {
            let frame = envelope("status", None, "{}", 1).replace(EPOCH, &guid);
            assert!(NativeRequest::parse(frame.as_bytes()).is_err(), "{guid}");
        }
        let mut book = NativeReceipts::new(EPOCH.into(), 10).unwrap();
        let padded = envelope("status", None, "{}", 1).replace(EPOCH, &format!(" {EPOCH} "));
        let parsed = NativeRequest::parse(padded.as_bytes()).unwrap();
        rejected(book.admit(&parsed, 4), "epoch_mismatch");
        assert!(matches!(
            book.admit(&request("call.dial", Some(OP), "{}", 1), 4),
            Admission::Dispatch(_)
        ));
        assert!(matches!(
            book.admit(&request("call.dial", Some(&OP.to_uppercase()), "{}", 2), 4),
            Admission::Dispatch(_)
        ));
        assert_eq!(book.len(), 2);
    }

    #[test]
    fn serialized_results_retain_bytes_and_bound_unknown_outcomes() {
        let raw = r#"{ "x" : 1 }"#;
        assert_eq!(NativeOutcome::result(raw.into()).result_json(), Some(raw));
        assert_eq!(
            NativeOutcome::result("{bad".into()).error_code(),
            Some("invalid_request")
        );
        let exact = format!("\"{}\"", "a".repeat(FRAME_BYTES - 514));
        assert!(NativeOutcome::result(exact.clone()).result_json().is_some());
        assert_eq!(
            NativeOutcome::result(format!("{exact} ")).error_code(),
            Some("operation_outcome_unknown")
        );
    }

    /// Native protocol admission is independent of any transport or device.
    #[test]
    fn native_methods_require_the_correct_read_or_effect_identity() {
        for method in [
            "status",
            "audio.probe",
            "codec.test",
            "inventory",
            "heartbeat",
            "desktop.observe",
            "desktop.capture",
            "desktop.captureOwner",
            "desktop.process",
            "desktop.controls",
        ] {
            let frame = envelope(method, None, "{}", 1);
            let parsed = NativeRequest::parse(frame.as_bytes()).unwrap();
            assert_eq!(parsed.method(), method);
            assert_eq!(parsed.epoch(), EPOCH);
            assert_eq!(parsed.raw_params(), "{}");
            assert!(NativeRequest::parse(envelope(method, Some(OP), "{}", 1).as_bytes()).is_err());
        }
        for method in [
            "registration.reconnect",
            "configure",
            "desktop.launch",
            "call.desktop.launch",
            "call.desktop.startVoice",
            "call.desktop.pauseVoice",
            "call.desktop.resumeVoice",
            "call.desktop.stopVoice",
            "call.screening.prepare",
            "call.screening.bridge",
            "call.features",
            "call.windows.connect",
            "call.codec.upgrade",
            "call.realtime.prepare",
            "call.realtime.answer",
            "call.realtime.stop",
            "call.prepare",
            "call.audio.prepare",
            "call.audio.rebind",
            "call.dial",
            "call.claim",
            "call.answer",
            "call.waiting.end",
            "call.command.feedback",
            "call.hangup",
            "call.release",
            "shutdown",
        ] {
            let frame = envelope(method, Some(OP), "{}", 1);
            let parsed = NativeRequest::parse(frame.as_bytes()).unwrap();
            assert_eq!(parsed.operation_id(), Some(OP));
            assert!(NativeRequest::parse(envelope(method, None, "{}", 1).as_bytes()).is_err());
        }
        for method in ["call.screening.synthesize", "STATUS", ""] {
            for operation in [None, Some(OP)] {
                assert!(
                    NativeRequest::parse(envelope(method, operation, "{}", 1).as_bytes()).is_err()
                );
            }
        }
    }
}
