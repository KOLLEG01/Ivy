use super::{Database, json};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    builtin_schema::json_equal,
    json::{parse, stringify},
};
use ivy_data_collector::{
    engine::{Engine, RegisterJob, Runner},
    ingress::Ingress,
    mcp_bridge::{LocalBridge, TaskMcp},
    model,
    runner::McpBridge,
};
use ivy_sdk::{clock::Clock, service::ServiceFuture};
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tokio_util::sync::CancellationToken;
struct FixedClock;
impl Clock for FixedClock {
    fn utc_millis(&self) -> i64 {
        1000000
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
struct NoRunner;
impl Runner for NoRunner {
    fn run(
        &self,
        _: IvyJson,
        _: IvyJson,
        _: IvyJson,
        _: CancellationToken,
        _: RegisterJob,
    ) -> ServiceFuture<IvyJson> {
        Box::pin(async { panic!("HTTP does not start a worker directly") })
    }
    fn stopped(&self, _: String, _: String) -> ServiceFuture<bool> {
        Box::pin(async { Ok(true) })
    }
    fn clear_authentication(&self, _: &str) -> IvyResult<()> {
        Ok(())
    }
}
struct Session {
    calls: AtomicUsize,
    closes: AtomicUsize,
    fail_close: bool,
}
impl Session {
    fn new(fail_close: bool) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            closes: AtomicUsize::new(0),
            fail_close,
        })
    }
}
impl TaskMcp for Session {
    fn call(&self, tool: JsString, args: IvyJson) -> ServiceFuture<IvyJson> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if tool == "fail".into() {
                return Err(IvyError::new("provider_unavailable", "private diagnostic")
                    .with_outcome(ivy_contracts::Outcome::Unknown));
            }
            if tool == "large".into() {
                return Ok("x".repeat(1048576).into());
            }
            Ok(IvyJson::object([
                ("tool", IvyJson::String(tool)),
                ("args", args),
            ]))
        })
    }
    fn close(&self) -> ServiceFuture<()> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        let fail = self.fail_close;
        Box::pin(async move {
            if fail {
                Err(IvyError::new("fixture_cleanup", "Cleanup failed."))
            } else {
                Ok(())
            }
        })
    }
}
async fn fixture(c: &IvyJson) -> (Database, Engine, Ingress) {
    let database = Database::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut settings = json(
        r#"{"secrets":{"INPUT":"synthetic"},"ingress":{"host":"127.0.0.1","port":1,"routes":{"/webhook":"task"}}}"#,
    );
    let mut config = settings.get("ingress").unwrap().clone();
    config.insert("port", IvyJson::Number(port as f64));
    settings.insert("ingress", config);
    let engine = Engine::new(
        database.open(),
        Arc::new(NoRunner),
        settings,
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    let mut task = json(
        r#"{"id":"task","name":"Task","enabled":true,"intervalSeconds":60,"timeoutSeconds":5,"memoryMb":64,"script":"export default()=>({data:1})","dependencies":{},"config":{},"secretNames":[],"inputSecretName":"INPUT","allowUnauthenticatedInput":false,"retention":{"maximumCount":1000,"maximumAgeDays":7,"maximumBytes":104857600}}"#,
    );
    task.insert("enabled", (!model::truth(c, "disabled")).into());
    task.insert(
        "allowUnauthenticatedInput",
        model::truth(c, "public").into(),
    );
    if model::truth(c, "public") {
        task.insert("inputSecretName", IvyJson::Null);
    }
    engine
        .update(
            IvyJson::object([
                ("action", "save".into()),
                ("operationId", "save".into()),
                ("expectedRevision", IvyJson::Number(0.0)),
                ("task", task),
            ]),
            None,
        )
        .await
        .unwrap();
    if model::truth(c, "busy") {
        engine
            .update(
                json(r#"{"action":"run","operationId":"prior","id":"task"}"#),
                None,
            )
            .await
            .unwrap();
    }
    let ingress = Ingress::open(engine.clone()).await.unwrap().unwrap();
    (database, engine, ingress)
}
fn body(c: &IvyJson) -> Vec<u8> {
    match model::optional_text(c, "bodyKind").as_deref() {
        Some("invalid") => vec![34, 0xed, 0xa0, 0x80, 34],
        Some("invalid-mcp") => [b"{\"tool\":\"".as_slice(), &[0xed, 0xa0, 0x80], b"\"}"].concat(),
        Some("large") => format!(
            "\"{}\"",
            "x".repeat(model::number(c, "size").unwrap() as usize - 2)
        )
        .into_bytes(),
        Some("large-mcp") => {
            let start = "{\"tool\":\"echo\",\"arguments\":\"";
            let end = "\"}";
            format!(
                "{start}{}{end}",
                "x".repeat(model::number(c, "size").unwrap() as usize - start.len() - end.len())
            )
            .into_bytes()
        }
        _ => model::text(c, "body").unwrap().into_bytes(),
    }
}
async fn read_reply(mut stream: TcpStream) -> IvyJson {
    let mut bytes = Vec::new();
    let received = tokio::time::timeout(Duration::from_secs(45), stream.read_to_end(&mut bytes))
        .await
        .unwrap();
    assert!(
        received.is_ok()
            || !bytes.is_empty()
            || received.as_ref().is_err_and(|error| matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::UnexpectedEof
            )),
        "{received:?}"
    );
    if bytes.is_empty() {
        return IvyJson::object([("status", IvyJson::Null), ("body", IvyJson::Null)]);
    }
    let split = bytes.windows(4).position(|b| b == b"\r\n\r\n").unwrap();
    let headers = String::from_utf8(bytes[..split].to_vec()).unwrap();
    let status = headers
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse::<f64>()
        .unwrap();
    let mut bytes = bytes[split + 4..].to_vec();
    if headers
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        let mut decoded = Vec::new();
        let mut at = 0;
        loop {
            let end = at + bytes[at..].windows(2).position(|b| b == b"\r\n").unwrap();
            let count =
                usize::from_str_radix(std::str::from_utf8(&bytes[at..end]).unwrap(), 16).unwrap();
            if count == 0 {
                break;
            }
            at = end + 2;
            decoded.extend_from_slice(&bytes[at..at + count]);
            at += count + 2;
        }
        bytes = decoded;
    }
    let body = if bytes.is_empty() {
        IvyJson::Null
    } else {
        parse(std::str::from_utf8(&bytes).unwrap(), usize::MAX).unwrap()
    };
    IvyJson::object([("status", IvyJson::Number(status)), ("body", body)])
}
async fn send(address: SocketAddr, c: &IvyJson, token: &str) -> IvyJson {
    if let Some(parts) = c.get("raw").and_then(IvyJson::as_array) {
        let mut stream = TcpStream::connect(address).await.unwrap();
        for part in parts {
            let delay = model::number(part, "afterMs").unwrap() as u64;
            if delay > 0 {
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            let data = model::text(part, "data").unwrap().replace("TOKEN", token);
            if stream.write_all(data.as_bytes()).await.is_err() {
                break;
            }
        }
        return read_reply(stream).await;
    }
    let body = body(c);
    let mut head = format!(
        "{} {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n",
        model::text(c, "method").unwrap(),
        model::text(c, "path").unwrap()
    );
    let headers = c.get("headers").unwrap().as_array().unwrap();
    for pair in headers.chunks(2) {
        head.push_str(&format!(
            "{}: {}\r\n",
            pair[0].as_str().unwrap(),
            pair[1].as_str().unwrap().replace("TOKEN", token)
        ));
    }
    let chunked = model::truth(c, "chunked");
    if chunked {
        head.push_str("Transfer-Encoding: chunked\r\n\r\n");
    } else {
        head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    }
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(head.as_bytes()).await.unwrap();
    if chunked {
        for part in body.chunks(3) {
            stream
                .write_all(format!("{:x}\r\n", part.len()).as_bytes())
                .await
                .unwrap();
            stream.write_all(part).await.unwrap();
            stream.write_all(b"\r\n").await.unwrap();
        }
        stream.write_all(b"0\r\n\r\n").await.unwrap();
    } else {
        let _ = stream.write_all(&body).await;
    }
    read_reply(stream).await
}
#[tokio::test]
async fn raw_http_matches_original_ingress_and_mcp_status_auth_headers_and_limits() {
    http_trace("").await;
}
#[tokio::test]
async fn raw_http_header_parser_boundaries_match_original() {
    http_trace("boundaries").await;
}
#[tokio::test]
async fn raw_http_request_deadlines_match_original_scan_windows() {
    http_trace("timeouts").await;
}
fn normalize_generated_ids(response: &mut IvyJson, queued: &mut IvyJson) {
    fn generated(value: &IvyJson) -> bool {
        value
            .as_str()
            .is_ok_and(|s| uuid::Uuid::parse_str(&s).is_ok())
    }
    if let Some(mut body) = response.get("body").cloned() {
        if body.get("runId").is_some_and(generated) {
            body.insert("runId", "{generated}".into());
            response.insert("body", body);
        }
    }
    if let Some(rows) = queued.as_array() {
        *queued = IvyJson::Array(
            rows.iter()
                .cloned()
                .map(|mut row| {
                    if row.get("id").is_some_and(generated) {
                        row.insert("id", "{generated}".into());
                    }
                    row
                })
                .collect(),
        );
    }
}
async fn observe_http(c: &IvyJson) -> (IvyJson, IvyJson, usize, f64) {
    let started = Instant::now();
    let (response, queued, calls) = if model::text(c, "kind").unwrap() == "ingress" {
        let (_directory, engine, ingress) = fixture(c).await;
        let mut response = send(ingress.address(), c, "").await;
        if model::truth(c, "repeat") {
            response = send(ingress.address(), c, "").await;
        }
        let queued = engine
            .store()
            .call(|s| {
                Ok(IvyJson::Array(
                    s.runs("queued")?
                        .iter()
                        .map(|r| {
                            IvyJson::object([
                                ("id", r.get("id").unwrap().clone()),
                                ("input", r.get("input").unwrap().clone()),
                            ])
                        })
                        .collect(),
                ))
            })
            .await
            .unwrap();
        ingress.close().await;
        engine.close().await;
        (response, queued, 0)
    } else {
        let session = Session::new(false);
        let signal = CancellationToken::new();
        if model::truth(c, "cancelled") {
            signal.cancel();
        }
        let bridge = LocalBridge::open(session.clone(), signal).await.unwrap();
        let context = bridge.context();
        let url = model::text(&context, "url").unwrap();
        let address = url
            .strip_prefix("http://")
            .unwrap()
            .strip_suffix("/call")
            .unwrap()
            .parse()
            .unwrap();
        let token = model::text(&context, "token").unwrap();
        assert_eq!(token.len(), 43);
        let response = send(address, c, &token).await;
        bridge.close().await.unwrap();
        assert_eq!(session.closes.load(Ordering::SeqCst), 1);
        (
            response,
            IvyJson::Null,
            session.calls.load(Ordering::SeqCst),
        )
    };
    (
        response,
        queued,
        calls,
        started.elapsed().as_secs_f64() * 1000.0,
    )
}
async fn http_trace(mode: &str) {
    let oracle = Database::new();
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/data-collector-http-oracle.mjs",
            "runtime",
        ])
        .arg(oracle.0.join("node"))
        .arg(mode)
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cases = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    let cases = cases.as_array().unwrap();
    let mut observed = Vec::new();
    if mode == "timeouts" {
        let mut pending = tokio::task::JoinSet::new();
        for (index, expected) in cases.iter().enumerate() {
            let c = expected.required("case").unwrap().clone();
            pending.spawn(async move { (index, observe_http(&c).await) });
        }
        while let Some(result) = pending.join_next().await {
            observed.push(result.unwrap());
        }
        observed.sort_by_key(|r| r.0);
    } else {
        for (index, expected) in cases.iter().enumerate() {
            observed.push((
                index,
                observe_http(expected.required("case").unwrap()).await,
            ));
        }
    }
    let mut gaps = Vec::new();
    for ((_, (mut response, mut queued, calls, elapsed)), expected) in
        observed.into_iter().zip(cases)
    {
        let c = expected.get("case").unwrap();
        let name = model::text(c, "name").unwrap();
        let mut expected_response = expected.required("response").unwrap().clone();
        let mut expected_queued = expected.required("queued").unwrap().clone();
        normalize_generated_ids(&mut response, &mut queued);
        normalize_generated_ids(&mut expected_response, &mut expected_queued);
        let timing_matches = mode != "timeouts"
            || if name.ends_with("-timeout") {
                (25_000.0..=36_000.0).contains(&elapsed)
            } else {
                (model::number(expected, "elapsedMs").unwrap() - elapsed).abs() < 3000.0
            };
        if !json_equal(&response, &expected_response)
            || !json_equal(&queued, &expected_queued)
            || calls as f64 != model::number(expected, "calls").unwrap()
            || !timing_matches
        {
            gaps.push(format!(
                "{name}: actual {} expected {}; queuedEqual={}, calls={calls}/{}, elapsedMs={elapsed:.0}/{:.0}, timingMatches={timing_matches}",
                stringify(&response, usize::MAX).unwrap(),
                stringify(&expected_response, usize::MAX).unwrap(),
                json_equal(&queued, &expected_queued),
                model::number(expected, "calls").unwrap(),
                expected.get("elapsedMs").and_then(IvyJson::as_f64).unwrap_or(0.0)
            ));
        }
    }
    assert!(
        gaps.is_empty(),
        "HTTP differential gaps:\n{}",
        gaps.join("\n")
    );
}
#[tokio::test]
async fn ingress_rechecks_revision_after_body_and_bridge_close_retires_sockets_on_session_failure()
{
    let (_directory, engine, ingress) = fixture(&json("{}")).await;
    let mut stream = TcpStream::connect(ingress.address()).await.unwrap();
    stream.write_all(b"POST /tasks/task/run HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nAuthorization: Bearer synthetic\r\nContent-Length: 2\r\nIdempotency-Key: run\r\n\r\n{").await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    engine
        .store()
        .call(|s| {
            let mut row = s.get("task", false)?.unwrap();
            row.insert("revision", IvyJson::Number(2.0));
            s.save(&row)
        })
        .await
        .unwrap();
    stream.write_all(b"}").await.unwrap();
    let reply = read_reply(stream).await;
    assert_eq!(model::number(&reply, "status").unwrap(), 401.0);
    assert_eq!(
        engine
            .store()
            .call(|s| Ok(s.runs("queued")?.len()))
            .await
            .unwrap(),
        0
    );
    ingress.close().await;
    engine.close().await;
    let session = Session::new(true);
    let bridge = LocalBridge::open(session.clone(), CancellationToken::new())
        .await
        .unwrap();
    let context = bridge.context();
    let url = model::text(&context, "url").unwrap();
    let address: SocketAddr = url
        .strip_prefix("http://")
        .unwrap()
        .strip_suffix("/call")
        .unwrap()
        .parse()
        .unwrap();
    let mut pending = TcpStream::connect(address).await.unwrap();
    pending
        .write_all(b"POST /call HTTP/1.1\r\nHost:")
        .await
        .unwrap();
    let (one, two) = tokio::join!(bridge.close(), bridge.close());
    assert_eq!(one.unwrap_err().code, "fixture_cleanup");
    assert_eq!(two.unwrap_err().code, "fixture_cleanup");
    assert_eq!(session.closes.load(Ordering::SeqCst), 1);
    assert!(TcpStream::connect(address).await.is_err());
    let mut bytes = Vec::new();
    let closed =
        tokio::time::timeout(Duration::from_secs(1), pending.read_to_end(&mut bytes)).await;
    assert!(closed.is_ok());
}
