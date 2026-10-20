use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_hive::{browser_push_https::NativeHttps, browser_push_native::NativeSender};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdout, Command},
    time::{Instant, timeout},
};
use tokio_rustls::rustls::{
    ClientConfig, RootCertStore, crypto::aws_lc_rs, pki_types::CertificateDer,
};

struct Oracle {
    child: Child,
    output: Lines<BufReader<ChildStdout>>,
    profile: Value,
}
impl Oracle {
    async fn start() -> Self {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let mut child =
            Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()))
                .arg("tests/fixtures/hive-push-https-oracle.mjs")
                .current_dir(root)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::inherit())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
        let profile = serde_json::from_str(
            &timeout(Duration::from_secs(10), output.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        Self {
            child,
            output,
            profile,
        }
    }
    async fn call(&mut self, input: Value) -> Value {
        self.child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(format!("{input}\n").as_bytes())
            .await
            .unwrap();
        serde_json::from_str(
            &timeout(Duration::from_secs(25), self.output.next_line())
                .await
                .unwrap()
                .unwrap()
                .expect("oracle exited"),
        )
        .unwrap()
    }
    async fn close(mut self) {
        assert_eq!(self.call(json!({"op":"close"})).await["closed"], true);
        assert!(
            timeout(Duration::from_secs(5), self.child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
    }
    fn sender(&self, trusted: bool) -> NativeSender {
        NativeSender::new(
            Arc::new(self.transport(trusted)),
            Arc::new(|| Ok(1700043200)),
        )
    }
    fn transport(&self, trusted: bool) -> NativeHttps {
        let mut roots = RootCertStore::empty();
        if trusted {
            roots
                .add(CertificateDer::from(
                    STANDARD
                        .decode(self.profile["caDer"].as_str().unwrap())
                        .unwrap(),
                ))
                .unwrap();
        }
        let config = ClientConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        NativeHttps::new(tokio::runtime::Handle::current(), Arc::new(config)).unwrap()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_https_sender_uses_native_system_clock_expiration() {
    let mut peer = Oracle::start().await;
    let sender = NativeSender::with_numeric_expiration(
        Arc::new(peer.transport(true)),
        ivy_hive::browser_push_native::system_expiration(),
    );
    let before = peer.call(json!({"op":"expiration"})).await;
    let (status, millis) = send(&sender, &peer.profile, "/status/201", None).await;
    let after = peer.call(json!({"op":"expiration"})).await;
    let observed = peer.call(json!({"op":"stats"})).await;
    save(
        &json!({"systemClock":true,"before":before,"after":after,"native":observed,"status":status,"millis":millis}),
    );
    assert_eq!(status, 201);
    let requests = observed["requests"].as_array().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["signatureValid"], true);
    assert_eq!(requests[0]["plaintext"], "notification");
    let expiration = requests[0]["claims"]["exp"].as_i64().unwrap();
    assert!(expiration >= before["expiration"].as_i64().unwrap());
    assert!(expiration <= after["expiration"].as_i64().unwrap());
    drop(sender);
    peer.close().await;
}
fn options(profile: &Value) -> IvyJson {
    IvyJson::from_serde(&json!({"TTL":3600,"timeout":10000,"vapidDetails":profile["details"]}))
        .unwrap()
}
async fn send(
    sender: &NativeSender,
    profile: &Value,
    path: &str,
    host: Option<&str>,
) -> (u16, f64) {
    let mut sub = profile["subscription"].clone();
    let base = host.map_or_else(
        || profile["base"].as_str().unwrap().to_string(),
        |host| format!("https://{host}:{}", profile["port"]),
    );
    sub["endpoint"] = json!(format!("{base}{path}"));
    let start = Instant::now();
    let outcome = timeout(
        Duration::from_secs(23),
        sender.send(
            IvyJson::from_serde(&sub).unwrap(),
            "notification".into(),
            options(profile),
        ),
    )
    .await
    .unwrap();
    let status = match outcome {
        SendOutcome::Response(Some(IvyJson::Number(n)))
        | SendOutcome::Rejected(Some(IvyJson::Number(n))) => n as u16,
        SendOutcome::Rejected(None) => 0,
        _ => panic!("unexpected outcome"),
    };
    (status, start.elapsed().as_secs_f64() * 1000.0)
}
fn save(value: &Value) {
    if let Some(path) = std::env::var_os("IVY_HIVE_PUSH_HTTPS_CORPUS") {
        use std::fs::OpenOptions;
        writeln!(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap(),
            "{value}"
        )
        .unwrap();
    }
}
fn semantic_headers(request: &Value) -> Value {
    let raw = request["rawHeaders"].as_array().unwrap();
    let mut fields = serde_json::Map::new();
    for pair in raw.chunks_exact(2) {
        let name = pair[0].as_str().unwrap().to_ascii_lowercase();
        assert!(
            fields.insert(name, pair[1].clone()).is_none(),
            "unexpected duplicate field"
        );
    }
    // Crypto randomness is verified in the existing cross-decryption oracle.
    // Here retain the whole raw request in the corpus and compare transport fields.
    assert!(
        fields
            .remove("authorization")
            .unwrap()
            .as_str()
            .unwrap()
            .starts_with("vapid t=")
    );
    Value::Object(fields)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_https_sender_preserves_status_headers_body_end_reuse_and_trust() {
    let mut peer = Oracle::start().await;
    let sender = peer.sender(true);
    let mut original_connection = None;
    let mut native_connection = None;
    for path in [
        "/status/200?q=%2F",
        "/status/201",
        "/status/204",
        "/status/299",
        "/status/300",
        "/status/404",
        "/status/410",
        "/status/429",
        "/status/500",
        "/redirect",
        "/compressed",
        "/delay-body",
    ] {
        let original = peer.call(json!({"op":"original","path":path})).await;
        let before = peer.call(json!({"op":"stats"})).await["requests"]
            .as_array()
            .unwrap()
            .len();
        let (status, millis) = send(&sender, &peer.profile, path, None).await;
        let all = peer.call(json!({"op":"stats"})).await;
        let requests = &all["requests"].as_array().unwrap()[before..];
        let record = json!({"path":path,"original":original,"native":{"status":status,"millis":millis,"requests":requests}});
        save(&record);
        assert_eq!(json!(status), original["status"], "{record}");
        assert_eq!(requests.len(), 1, "no redirect or retry: {record}");
        let old = &original["requests"][0];
        let new = &requests[0];
        for request in [old, new] {
            assert_eq!(request["signatureValid"], true);
            assert_eq!(request["plaintext"], "notification");
        }
        assert_eq!(old["method"], new["method"]);
        assert_eq!(old["path"], new["path"]);
        assert_eq!(semantic_headers(old), semantic_headers(new), "{record}");
        if path == "/status/200?q=%2F" {
            original_connection = Some(old["connection"].clone());
            native_connection = Some(new["connection"].clone());
        }
        if path == "/status/201" {
            assert_eq!(original_connection.as_ref(), Some(&old["connection"]));
            assert_eq!(native_connection.as_ref(), Some(&new["connection"]));
        }
        if path == "/delay-body" {
            assert!(millis >= 500.0 && original["millis"].as_f64().unwrap() >= 500.0);
        }
    }
    for (trusted, host) in [(false, None), (true, Some("127.0.0.1"))] {
        let original=peer.call(json!({"op":"original","path":"/status/201","trusted":trusted,"host":if host.is_some(){"ip"}else{"dns"}})).await;
        let (status, millis) =
            send(&peer.sender(trusted), &peer.profile, "/status/201", host).await;
        save(
            &json!({"trusted":trusted,"host":host,"original":original,"native":{"status":status,"millis":millis}}),
        );
        assert_eq!(status, 0);
        assert_eq!(original["status"], 0);
    }
    drop(sender);
    peer.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_https_timeout_tracks_socket_progress_and_observer_drop_does_not_cancel() {
    let mut peer = Oracle::start().await;
    let sender = peer.sender(true);
    // Each original and native request is checked against the same isolated peer.
    for path in [
        "/drop",
        "/stall-headers",
        "/stall-body",
        "/trickle-headers",
        "/trickle-body",
    ] {
        let original = peer.call(json!({"op":"original","path":path})).await;
        let (status, millis) = send(&sender, &peer.profile, path, None).await;
        save(&json!({"path":path,"original":original,"native":{"status":status,"millis":millis}}));
        assert_eq!(json!(status), original["status"], "path={path}");
        if path.starts_with("/stall") {
            assert!((9000.0..15000.0).contains(&millis));
            assert!((9000.0..15000.0).contains(&original["millis"].as_f64().unwrap()));
        }
        if path.starts_with("/trickle") {
            assert!((11000.0..18000.0).contains(&millis));
            assert!((11000.0..18000.0).contains(&original["millis"].as_f64().unwrap()));
        }
    }
    let before = peer.call(json!({"op":"stats"})).await;
    let mut sub = peer.profile["subscription"].clone();
    sub["endpoint"] = json!(format!(
        "{}/delay-body",
        peer.profile["base"].as_str().unwrap()
    ));
    let observer = sender.send(
        IvyJson::from_serde(&sub).unwrap(),
        "notification".into(),
        options(&peer.profile),
    );
    drop(observer);
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let after = peer.call(json!({"op":"stats"})).await;
    save(&json!({"abandonedObserver":true,"before":before,"after":after}));
    assert_eq!(
        after["requests"].as_array().unwrap().len(),
        before["requests"].as_array().unwrap().len() + 1
    );
    assert_eq!(
        after["completions"].as_array().unwrap().len(),
        before["completions"].as_array().unwrap().len() + 1
    );
    drop(sender);
    peer.close().await;
}
