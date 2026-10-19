use ivy_phone_bridge::{
    ipc::{FRAME_BYTES, FrameError},
    native_rpc::{Admission, NativeOutcome, NativeReceipts, NativeRequest},
};
use std::sync::Arc;
#[path = "phone_media_format.rs"]
mod media_format;
#[path = "phone_pcm_codec.rs"]
mod pcm_codec;
#[path = "phone_rtp_transport.rs"]
mod rtp_transport;
#[path = "phone_sdp.rs"]
mod sdp;
#[path = "phone_native_config.rs"]
mod native_config;
#[path = "phone_native_dispatch.rs"]
mod native_dispatch;
#[path = "phone_native_pipe.rs"]
mod native_pipe;
#[path = "phone_register.rs"]
mod register;
#[path = "phone_registration.rs"]
mod registration;
#[path = "phone_sip.rs"]
mod sip;
#[path = "phone_sip_host.rs"]
mod sip_host;
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

#[test]
fn exact_envelope_and_integer_lexemes() {
    let valid = envelope("status", None, "{}", 1);
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

/// Execute the production C# parser as an independent Windows oracle. No SIP,
/// audio or process-owner paths are invoked by this fixture.
#[cfg(windows)]
#[test]
fn differential_against_original_native_parser() {
    use ivy_contracts::{
        IvyJson,
        json::{parse, stringify},
    };
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let valid = envelope("status", None, "{}", 1);
    let mut frames = vec![valid.clone(), envelope("call.dial", Some(OP), "{}", 1)];
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
            frames.push(valid.replace(&format!("\"{field}\":1"), &format!("\"{field}\":{number}")));
        }
    }
    for guid in [
        EPOCH.into(),
        EPOCH.to_uppercase(),
        format!(" {EPOCH} "),
        format!("\\t{EPOCH}\\n"),
        EPOCH.replace('-', ""),
        format!("{{{EPOCH}}}"),
        "00000000-0000-0000-0000-000000000000".into(),
    ] {
        frames.push(valid.replace(EPOCH, &guid));
    }
    for params in [
        r#"{"a":1,"a":2}"#,
        r#"{"a":[{"a":1,"\u0061":2}]}"#,
        r#"{"\ud800":null}"#,
        r#"{"\udfff":null}"#,
        r#"{"v":"\ud800"}"#,
        r#"{"n":1e999}"#,
        r#"{ "x" : ["}", "\"[", {"n":1e0}], "a":"\u0062" }"#,
        "[]",
        "null",
        "1",
    ] {
        frames.push(envelope("status", None, params, 1));
    }
    for depth in [21, 22, 23, 24] {
        frames.push(envelope(
            "status",
            None,
            &format!("{{\"v\":{}0{}}}", "[".repeat(depth), "]".repeat(depth)),
            1,
        ));
    }
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
        "call.screening.synthesize",
        "STATUS",
        "",
    ] {
        frames.push(envelope(method, None, "{}", 1));
        frames.push(envelope(method, Some(OP), "{}", 1));
    }
    frames.extend([
        valid.replace("\"version\"", "\"vers\\u0069on\""),
        valid.replace("\"version\":1", "\"version\":1,\"version\":1"),
        valid.replace("\"version\":1", "\"version\":1,\"extra\":0"),
        valid.replace("\"version\":1,", ""),
        format!("\u{feff}{valid}"),
        format!("\r\n{valid}\r\n"),
        format!("[{valid}]"),
        format!("{valid}{{}}"),
    ]);
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let executable = std::env::var_os("IVY_POWERSHELL").unwrap_or("pwsh".into());
    let mut child = Command::new(executable)
        .current_dir(root)
        .args([
            "-NoProfile",
            "-File",
            "tests/fixtures/phone-native-oracle.ps1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = stringify(
        &IvyJson::Array(frames.iter().cloned().map(IvyJson::from).collect()),
        1_000_000,
    )
    .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let results = parse(std::str::from_utf8(&output.stdout).unwrap(), 1_000_000).unwrap();
    let results = results.as_array().unwrap();
    assert_eq!(frames.len(), results.len());
    for (frame, expected) in frames.iter().zip(results) {
        let actual = NativeRequest::parse(frame.as_bytes());
        let ok = expected.get("ok").unwrap().as_bool().unwrap();
        assert_eq!(actual.is_ok(), ok, "Parser disagreement: {frame}");
        if let Ok(actual) = actual {
            assert_eq!(
                actual.epoch(),
                expected.get("epoch").unwrap().as_str().unwrap()
            );
            assert_eq!(
                actual.request_id() as f64,
                expected.get("requestId").unwrap().as_f64().unwrap()
            );
            assert_eq!(
                actual.method(),
                expected.get("method").unwrap().as_str().unwrap()
            );
            assert_eq!(
                actual.raw_params(),
                expected.get("params").unwrap().as_str().unwrap()
            );
            let operation = expected.get("operationId").unwrap();
            assert_eq!(
                actual.operation_id().map(str::to_owned),
                if operation.is_null() {
                    None
                } else {
                    Some(operation.as_str().unwrap())
                }
            );
        }
    }
}
