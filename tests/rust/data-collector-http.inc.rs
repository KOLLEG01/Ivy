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
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
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
    let oracle = Database::new();
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/data-collector-http-oracle.mjs",
            "http",
        ])
        .arg(oracle.0.join("node"))
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cases = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    for expected in cases.as_array().unwrap() {
        let c = expected.get("case").unwrap();
        let name = model::text(c, "name").unwrap();
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
        assert!(
            json_equal(&response, expected.get("response").unwrap()),
            "{name}: actual {} expected {}",
            stringify(&response).unwrap(),
            stringify(expected.get("response").unwrap()).unwrap()
        );
        assert!(
            json_equal(&queued, expected.get("queued").unwrap()),
            "{name}: queued input mismatch"
        );
        assert_eq!(
            calls as f64,
            model::number(expected, "calls").unwrap(),
            "{name}"
        );
    }
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
