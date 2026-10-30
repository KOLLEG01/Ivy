use super::*;

struct MemorySocket(Arc<Mutex<Vec<IvyJson>>>);
impl Socket for MemorySocket {
    fn is_open(&self) -> bool { true }
    fn buffered_amount(&self) -> f64 { 0.0 }
    fn send_text(&mut self, text: String) -> IvyResult<()> {
        self.0.lock().unwrap().push(ivy_contracts::json::parse(&text, usize::MAX)?);
        Ok(())
    }
    fn terminate(&mut self) -> IvyResult<()> { Ok(()) }
}
impl PeerSocket for MemorySocket {
    fn ping(&mut self) -> IvyResult<()> { Ok(()) }
}

#[test]
fn db_completion_and_timeout_deliver_once_and_release_the_request_once() {
    for timeout_first in [false, true] {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let peer = Peer(Arc::new(Connection {
            socket: Mutex::new(Box::new(MemorySocket(sent.clone()))),
            context: Mutex::new(IvyJson::Null),
            session: Mutex::new(Session {
                active: 1, alive: true, diagnosed_at: 0.0,
                filters: Vec::new(), changes: Vec::new(), expiry: None,
            }),
        }));
        let requests = RequestBudget::new(crate::admission::RequestBudgetOptions {
            maximum: 1, per_consumer: 1, ..Default::default()
        });
        let mut delivery = PeerDelivery {
            peer: peer.clone(), id: "call".into(),
            lease: requests.acquire(&"credential".into(), Some(4096)).unwrap(),
            finished: false, error: None,
        };
        let timeout = || IvyError::new("deadline_exceeded", "Original call timed out.")
            .with_outcome(Outcome::Unknown);
        if timeout_first {
            delivery.reject(timeout());
            delivery.finish(Ok("committed".into()));
        } else {
            delivery.finish(Ok("committed".into()));
            delivery.reject(timeout());
        }
        assert_eq!(peer.0.session.lock().unwrap().active, 0);
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].get("id"), Some(&"call".into()));
        assert_eq!(sent[0].get("error").is_some(), timeout_first);
        assert!(requests.acquire(&"credential".into(), Some(4096)).is_ok());
    }
}

fn error_factory() -> ErrorConstructor {
    Arc::new(|parts| {
        let mut error = IvyError::new_js(parts.code, parts.message)
            .with_provider_outcome(parts.outcome);
        error.details = parts.details;
        error
    })
}
fn copied_provider_response(frame: &IvyJson, factory: &ErrorConstructor) -> IvyResult<IvyResult<IvyJson>> {
    validate_response_frame(frame)?;
    if let Some(error) = frame.get("error") {
        let data = error.required("data")?;
        Ok(Err(factory(ProviderError {
            code: text(data, "code")?, message: text(error, "message")?,
            outcome: data.required("outcome")?.clone(), details: data.get("details").cloned(),
        })))
    } else {
        Ok(Ok(frame.required("result")?.clone()))
    }
}
fn response_trace(result: IvyResult<IvyResult<IvyJson>>) -> String {
    let (kind, value) = match result {
        Ok(Ok(value)) => ("result", value),
        Ok(Err(error)) => ("provider-error", error.to_wire()),
        Err(error) => ("envelope-error", error.to_wire()),
    };
    ivy_contracts::json::stringify(&IvyJson::object([(kind, value)]), usize::MAX).unwrap()
}
fn response_frame(payload: IvyJson, failure: bool) -> IvyJson {
    let mut frame = IvyJson::object([("jsonrpc", "2.0".into()), ("id", "call".into())]);
    if failure {
        frame.insert("error", IvyJson::object([
            ("code", (-32000.0).into()),
            ("message", IvyJson::String(JsString(vec![0xd800]))),
            ("data", IvyJson::object([
                ("code", IvyJson::String(JsString(vec![0xdc00]))),
                ("outcome", IvyJson::Array(vec![IvyJson::Array(vec!["not_executed".into()])])),
                ("details", payload),
            ])),
        ]));
    } else { frame.insert("result", payload); }
    frame
}

#[test]
fn provider_response_ownership_preserves_envelopes_outcomes_and_opaque_payloads() {
    let factory = error_factory();
    let opaque = IvyJson::object([
        (JsString(vec![0xd800]), IvyJson::String(JsString(vec![0xdc00]))),
        ("unknown".into(), IvyJson::Array(vec![true.into(), IvyJson::Number(f64::NAN), 9007199254740992.0.into()])),
    ]);
    let mut frames = vec![IvyJson::Null, IvyJson::Array(vec![]), response_frame(IvyJson::Null, false),
        response_frame(opaque.clone(), false), response_frame(opaque, true)];
    for outcome in [IvyJson::Null, "unknown".into(), "completed".into(), 1.0.into(),
        IvyJson::Array(vec!["not_executed".into()]), IvyJson::Array(vec!["unknown".into(), "completed".into()])]
    {
        let mut frame = response_frame(IvyJson::Null, true);
        let mut error = frame.remove("error").unwrap();
        let mut data = error.remove("data").unwrap();
        data.insert("outcome", outcome);
        error.insert("data", data);frame.insert("error", error);frames.push(frame);
    }
    for text in [
        r#"{"jsonrpc":"2.0","id":"call"}"#,
        r#"{"jsonrpc":"2.0","id":"call","result":null,"error":null}"#,
        r#"{"jsonrpc":"2.0","id":"call","result":null,"extra":null}"#,
        r#"{"jsonrpc":"2.0","id":"call","error":{"code":-32000,"message":"failed"}}"#,
        r#"{"jsonrpc":"2.0","id":"call","error":{"code":-32000,"message":"failed","data":{"code":"remote","outcome":"unknown"}}}"#,
    ] { frames.push(ivy_contracts::json::parse(text, usize::MAX).unwrap()); }
    for frame in frames {
        let expected = response_trace(copied_provider_response(&frame, &factory));
        assert_eq!(response_trace(provider_response(frame, &factory)), expected);
    }
}

#[test]
fn provider_response_moves_result_and_error_detail_buffers() {
    use crate::retention::graph_tests::{LIVE, PEAK};
    use std::sync::atomic::Ordering;
    const CHILD: &str = "IVY_PROVIDER_RESPONSE_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "server_peers::delivery_tests::provider_response_moves_result_and_error_detail_buffers", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));
        return;
    }
    let pointer = |value: &IvyJson| value.get("opaque").unwrap().as_js_string().unwrap().0.as_ptr() as usize;
    let factory = error_factory();
    for failure in [false, true] {
        let payload = IvyJson::object([("opaque", "p".repeat(1024 * 1024).into())]);
        let address = pointer(&payload);
        let frame = response_frame(payload, failure);
        let before = LIVE.load(Ordering::Relaxed);
        PEAK.store(before, Ordering::Relaxed);
        let copied = copied_provider_response(&frame, &factory).unwrap();
        let copied_peak = PEAK.load(Ordering::Relaxed) - before;
        drop(copied);
        let before = LIVE.load(Ordering::Relaxed);
        PEAK.store(before, Ordering::Relaxed);
        let result = provider_response(frame, &factory).unwrap();
        let moved_peak = PEAK.load(Ordering::Relaxed) - before;
        match &result {
            Ok(value) => { assert!(!failure);assert_eq!(pointer(value), address); }
            Err(error) => {
                assert!(failure);assert_eq!(pointer(error.details.as_ref().unwrap()), address);
                assert_eq!(error.outcome_json(), IvyJson::Array(vec![IvyJson::Array(vec!["not_executed".into()])]));
                assert_ne!(error.outcome, Outcome::NotExecuted);
            }
        }
        eprintln!("Hive provider-response Rust allocation peak: failure={failure}, copied {copied_peak} bytes, moved {moved_peak} bytes for 1 MiB payload text");
        assert!(copied_peak > 2 * 1024 * 1024);
        assert!(moved_peak < 32 * 1024, "Provider response copied its payload");
    }
}
