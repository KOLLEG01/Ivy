use super::*;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
};

struct Peer {
    child: Child,
    reader: Option<JoinHandle<()>>,
    events: Arc<Mutex<Vec<Value>>>,
    changed: Arc<Notify>,
    base: String,
    name: &'static str,
}
impl Peer {
    async fn start(name: &'static str, tls: bool, host: &str) -> Self {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../tests/fixtures/native-http-peer.mjs");
        let mut command = Command::new("node");
        command
            .arg(fixture)
            .arg(if tls { "tls" } else { "plain" })
            .arg(host)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            command.creation_flags(0x0800_0000);
        }
        let mut child = command.spawn().expect("loopback Node peer");
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let events = Arc::new(Mutex::new(Vec::new()));
        let changed = Arc::new(Notify::new());
        let saved = events.clone();
        let signal = changed.clone();
        let reader = tokio::spawn(async move {
            while let Some(line) = lines.next_line().await.unwrap() {
                lock(&saved).push(serde_json::from_str(&line).expect("peer JSON"));
                signal.notify_one();
            }
        });
        let mut peer = Self {
            child,
            reader: Some(reader),
            events,
            changed,
            base: String::new(),
            name,
        };
        let listening = peer.wait(|event| event["event"] == "listening").await;
        peer.base = format!(
            "{}://{}:{}",
            if tls { "https" } else { "http" },
            if host == "::1" { "[::1]" } else { host },
            listening["port"].as_u64().unwrap()
        );
        peer
    }
    async fn wait(&self, predicate: impl Fn(&Value) -> bool) -> Value {
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                let changed = self.changed.notified();
                if let Some(event) = lock(&self.events)
                    .iter()
                    .find(|event| predicate(event))
                    .cloned()
                {
                    return event;
                }
                changed.await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{}: missing peer event; observed {:?}",
                self.name,
                lock(&self.events)
            )
        })
    }
    async fn request(&self, mode: &str) -> Value {
        self.wait(|event| event["event"] == "request" && event["mode"] == mode)
            .await
    }
    async fn closed(&self, id: u64) {
        self.wait(|event| event["event"] == "close" && event["id"] == id)
            .await;
    }
    fn observe(&self, case: &str, fields: Value) {
        lock(&self.events).push(json!({"event":"observation", "case":case, "details":fields}));
    }
    async fn finish(mut self) {
        drop(self.child.stdin.take());
        tokio::time::timeout(Duration::from_secs(4), self.child.wait())
            .await
            .unwrap()
            .unwrap();
        self.reader.take().unwrap().await.unwrap();
        self.save();
    }
    fn save(&self) {
        if let Some(directory) = std::env::var_os("IVY_TEST_RAW_EVIDENCE") {
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                std::path::Path::new(&directory).join(format!("{}.json", self.name)),
                serde_json::to_vec_pretty(&*lock(&self.events)).unwrap(),
            )
            .unwrap();
        }
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.save();
        let _ = self.child.start_kill();
        if let Some(reader) = self.reader.take() {
            reader.abort();
        }
    }
}

struct ConfigSource {
    calls: AtomicUsize,
    config: Mutex<Arc<rustls::ClientConfig>>,
}
impl TlsConfigSource for ConfigSource {
    fn for_origin<'a>(&'a self, _: &'a Uri) -> ConfigFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(ready(Ok(lock(&self.config).clone())))
    }
}
fn config(trusted: bool) -> Arc<rustls::ClientConfig> {
    config_with_versions(trusted, &[&rustls::version::TLS13, &rustls::version::TLS12])
}
fn config_with_versions(
    trusted: bool,
    versions: &[&'static rustls::SupportedProtocolVersion],
) -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    if trusted {
        roots
            .add(rustls::pki_types::CertificateDer::from(
                include_bytes!("../fixtures/native-http-ca.der").to_vec(),
            ))
            .unwrap();
    }
    let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_protocol_versions(versions)
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    config.resumption = rustls::client::Resumption::in_memory_sessions(100);
    config.enable_early_data = false;
    Arc::new(config)
}
fn source() -> Arc<ConfigSource> {
    Arc::new(ConfigSource {
        calls: AtomicUsize::new(0),
        config: Mutex::new(config(true)),
    })
}
fn settings(tls: Arc<dyn TlsConfigSource>) -> ConnectionSettings {
    ConnectionSettings {
        tls,
        connect_timeout: Duration::from_secs(2),
        headers_timeout: Duration::from_secs(2),
        body_idle_timeout: Duration::from_secs(2),
        idle_timeout: Duration::from_secs(2),
        maximum_idle_timeout: Duration::from_secs(2),
        idle_timeout_threshold: Duration::ZERO,
        tcp_keepalive_delay: Duration::from_secs(60),
        happy_eyeballs_delay: Duration::from_millis(50),
        maximum_head_bytes: 16384,
        maximum_head_fields: 128,
    }
}
fn admission(status: StatusCode) -> HeadAdmission {
    if matches!(status.as_u16(), 100 | 101) {
        HeadAdmission::RejectAndRetire
    } else {
        HeadAdmission::Continue
    }
}
fn policy() -> HeadPolicy {
    HeadPolicy {
        on_informational: admission,
        on_final_head: admission,
    }
}
fn request(base: &str, mode: &str) -> Request<RequestBody> {
    Request::get(format!("{base}/{mode}"))
        .body(Full::new(Bytes::new()))
        .unwrap()
}
async fn get(client: &Http1Client, peer: &Peer, mode: &str) -> Response {
    client
        .send(
            request(&peer.base, mode),
            policy(),
            CancellationToken::new(),
        )
        .await
        .unwrap()
}
fn id(response: &Response) -> u64 {
    response.headers()["x-connection-id"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap()
}
async fn drain(response: &mut Response) -> Result<Vec<u8>, TransportError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.next_chunk().await? {
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
fn error<T>(result: Result<T, TransportError>) -> TransportError {
    match result {
        Ok(_) => panic!("unexpected admitted response"),
        Err(error) => error,
    }
}
async fn release_tasks() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

#[test]
fn construction_is_inert_without_a_runtime() {
    let source = source();
    let client = Http1Client::new(settings(source.clone()));
    let clone = client.clone();
    drop(client);
    drop(clone);
    assert_eq!(source.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn http_heads_streaming_and_retirement() {
    let peer = Peer::start("http-heads", false, "127.0.0.1").await;
    let source = source();
    let client = Http1Client::new(settings(source.clone()));
    let mut first = get(&client, &peer, "ok").await;
    let initial = id(&first);
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        drain(&mut first).await.unwrap(),
        format!("reply-{initial}-1").as_bytes()
    );
    for mode in ["chunked", "info", "segmented"] {
        let mut response = get(&client, &peer, mode).await;
        assert_eq!(id(&response), initial);
        let bytes = drain(&mut response).await.unwrap();
        if mode == "chunked" {
            assert_eq!(bytes, b"abc");
        }
        if mode == "segmented" {
            assert_eq!(bytes, b"abcdef");
        }
        peer.observe(
            mode,
            json!({"id":initial,"admitted":true,"bytes":bytes,"reused":true}),
        );
    }
    let first_request = peer.request("ok").await;
    assert!(
        first_request["head"]
            .as_str()
            .unwrap()
            .starts_with("GET /ok HTTP/1.1\r\n")
    );
    assert!(
        first_request["head"]
            .as_str()
            .unwrap()
            .to_ascii_lowercase()
            .contains("\r\nhost: 127.0.0.1:")
    );
    for mode in [
        "100-coalesced",
        "100-split",
        "100-only",
        "100-after-103",
        "100-partial-final",
        "upgrade",
    ] {
        let failed = client
            .send(
                request(&peer.base, mode),
                policy(),
                CancellationToken::new(),
            )
            .await;
        let expected = if mode == "upgrade" { 101 } else { 100 };
        assert_eq!(
            error(failed).kind,
            TransportErrorKind::RejectedHead(StatusCode::from_u16(expected).unwrap())
        );
        let rejected = peer.request(mode).await;
        let retired = rejected["id"].as_u64().unwrap();
        peer.closed(retired).await;
        let mut next = get(&client, &peer, &format!("after-{mode}")).await;
        assert_ne!(id(&next), retired);
        let next_id = id(&next);
        drain(&mut next).await.unwrap();
        assert_eq!(
            lock(&peer.events)
                .iter()
                .filter(|event| event["event"] == "request" && event["mode"] == mode)
                .count(),
            1
        );
        peer.observe(mode, json!({"rejectedId":retired,"replacementId":next_id,"responseExposed":false,"bodyExposed":false,"requests":1,"closed":true}));
    }
    let mut close = get(&client, &peer, "peer-close").await;
    let close_id = id(&close);
    assert!(!drain(&mut close).await.unwrap().is_empty());
    peer.closed(close_id).await;
    let mut truncated = get(&client, &peer, "truncated").await;
    assert_eq!(
        error(drain(&mut truncated).await).kind,
        TransportErrorKind::Body
    );
    peer.closed(id(&truncated)).await;
    assert_eq!(
        source.calls.load(Ordering::SeqCst),
        0,
        "plain HTTP must not invoke TLS policy"
    );
    drop(client);
    peer.finish().await;
}

#[tokio::test]
async fn cancellation_drop_generations_and_logical_owners() {
    let peer = Peer::start("http-lifetime", false, "127.0.0.1").await;
    let client = Http1Client::new(settings(source()));
    let token = CancellationToken::new();
    let mut first = client
        .send(request(&peer.base, "ok"), policy(), token.clone())
        .await
        .unwrap();
    let first_id = id(&first);
    let old_socket = first.lease.as_ref().unwrap().socket.clone();
    let old_generation = first.lease.as_ref().unwrap().generation;
    drain(&mut first).await.unwrap();
    let clone = client.clone();
    drop(client);
    let current_token = CancellationToken::new();
    let mut active = clone
        .send(
            request(&peer.base, "stall-body"),
            policy(),
            current_token.clone(),
        )
        .await
        .unwrap();
    assert_eq!(
        id(&active),
        first_id,
        "dropping one logical client must preserve reuse"
    );
    assert!(active.lease.as_ref().unwrap().generation > old_generation);
    token.cancel();
    release_tasks().await;
    assert!(!old_socket.retire(old_generation, TransportErrorKind::Cancelled));
    assert!(lock(&old_socket.wire).io.is_some());
    assert_eq!(active.next_chunk().await.unwrap().unwrap(), "start");
    current_token.cancel();
    peer.closed(first_id).await; // The watcher closes even without polling the body again.
    assert_eq!(
        error(active.next_chunk().await).kind,
        TransportErrorKind::Cancelled
    );
    peer.observe("stale-generation", json!({"id":first_id,"oldGeneration":old_generation,"newGeneration":active.lease.as_ref().unwrap().generation,"staleRetire":false,"newTokenClosed":true}));
    drop(active);
    drop(old_socket);

    let token = CancellationToken::new();
    let mut sending =
        Box::pin(clone.send(request(&peer.base, "stall-head"), policy(), token.clone()));
    let observed = tokio::select! {
        event = peer.request("stall-head") => event,
        _ = &mut sending => panic!("head unexpectedly completed"),
    };
    token.cancel();
    assert_eq!(error(sending.await).kind, TransportErrorKind::Cancelled);
    peer.closed(observed["id"].as_u64().unwrap()).await;

    let mut sending = Box::pin(clone.send(
        request(&peer.base, "stall-head?drop"),
        policy(),
        CancellationToken::new(),
    ));
    let observed = tokio::select! {
        event = peer.wait(|e| e["event"] == "request" && e["path"] == "/stall-head?drop") => event,
        _ = &mut sending => panic!("head unexpectedly completed"),
    };
    drop(sending);
    peer.closed(observed["id"].as_u64().unwrap()).await;

    let unread = get(&clone, &peer, "stall-body?unread").await;
    let unread_id = id(&unread);
    drop(unread);
    peer.closed(unread_id).await;
    let mut held = get(&clone, &peer, "stall-body?owner").await;
    let held_id = id(&held);
    let weak_owner = Arc::downgrade(&clone.owner);
    let weak_socket = Arc::downgrade(&held.lease.as_ref().unwrap().socket);
    let mut idle = get(&clone, &peer, "idle-owned").await;
    let idle_id = id(&idle);
    assert_ne!(held_id, idle_id);
    drain(&mut idle).await.unwrap();
    drop(clone);
    assert!(
        weak_owner.upgrade().is_none(),
        "responses and workers are not logical owners"
    );
    peer.closed(held_id).await;
    peer.closed(idle_id).await;
    release_tasks().await;
    assert!(
        held.lease
            .as_ref()
            .unwrap()
            .watcher
            .as_ref()
            .unwrap()
            .is_finished(),
        "shutdown reclaims the cancellation watcher while the response is held"
    );
    assert_eq!(
        error(held.next_chunk().await).kind,
        TransportErrorKind::Cancelled
    );
    drop(held);
    release_tasks().await;
    assert!(
        weak_socket.upgrade().is_none(),
        "no physical connection retention cycle"
    );
    peer.observe("final-owner", json!({"activeId":held_id,"idleId":idle_id,"ownerReleased":true,"physicalReleased":true,"bothClosed":true}));
    peer.finish().await;
}

#[tokio::test]
async fn isolated_limits_and_timeouts_retire_connections() {
    let peer = Peer::start("http-limits", false, "127.0.0.1").await;
    for (mode, head_bytes, fields) in [("large-head", 256, 128), ("many-head", 16384, 8)] {
        let mut settings = settings(source());
        settings.maximum_head_bytes = head_bytes;
        settings.maximum_head_fields = fields;
        let client = Http1Client::new(settings);
        assert_eq!(
            error(
                client
                    .send(
                        request(&peer.base, mode),
                        policy(),
                        CancellationToken::new()
                    )
                    .await
            )
            .kind,
            TransportErrorKind::Protocol
        );
        peer.closed(peer.request(mode).await["id"].as_u64().unwrap())
            .await;
    }
    let mut short = settings(source());
    short.maximum_head_bytes = 256;
    let client = Http1Client::new(short);
    let mut response = get(&client, &peer, "large-trailer").await;
    assert_eq!(
        error(drain(&mut response).await).kind,
        TransportErrorKind::Body
    );
    peer.closed(id(&response)).await;
    drop(client);
    let mut short = settings(source());
    short.headers_timeout = Duration::from_millis(100);
    short.body_idle_timeout = Duration::from_millis(100);
    let client = Http1Client::new(short);
    assert_eq!(
        error(
            client
                .send(
                    request(&peer.base, "stall-head"),
                    policy(),
                    CancellationToken::new()
                )
                .await
        )
        .kind,
        TransportErrorKind::Timeout
    );
    peer.closed(peer.request("stall-head").await["id"].as_u64().unwrap())
        .await;
    let mut response = get(&client, &peer, "stall-body").await;
    assert_eq!(response.next_chunk().await.unwrap().unwrap(), "start");
    assert_eq!(
        error(response.next_chunk().await).kind,
        TransportErrorKind::Timeout
    );
    peer.closed(id(&response)).await;
    drop(client);
    let mut short = settings(source());
    short.idle_timeout = Duration::from_millis(100);
    let client = Http1Client::new(short);
    let mut idle = get(&client, &peer, "idle-expire").await;
    let expired = id(&idle);
    drain(&mut idle).await.unwrap();
    peer.closed(expired).await;
    let mut next = get(&client, &peer, "keepalive-zero").await;
    assert_ne!(id(&next), expired);
    let immediate = id(&next);
    drain(&mut next).await.unwrap();
    peer.closed(immediate).await;
    peer.observe("timeouts-and-limits", json!({"headTimeoutMs":100,"bodyFrameTimeoutMs":100,"idleTimeoutMs":100,"wholeHeadBytes":256,"headFields":8,"limitsAreUndiciEquivalent":false}));
    drop(client);
    peer.finish().await;
}

#[tokio::test]
async fn tls_validates_roots_names_alpn_and_pool_identity() {
    let peer = Peer::start("https-policy", true, "127.0.0.1").await;
    let tls = source();
    let client = Http1Client::new(settings(tls.clone()));
    let dns_base = peer.base.replace("127.0.0.1", "localhost");
    let mut first = client
        .send(
            request(&dns_base, "dns"),
            policy(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let first_id = id(&first);
    drain(&mut first).await.unwrap();
    let secure = peer
        .wait(|e| e["event"] == "secure" && e["id"] == first_id)
        .await;
    assert_eq!(secure["alpn"], "http/1.1");
    assert_eq!(secure["servername"], "localhost");
    assert_eq!(secure["protocol"], "TLSv1.3");
    let mut same = client
        .send(
            request(&dns_base, "same"),
            policy(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(id(&same), first_id);
    drain(&mut same).await.unwrap();
    *lock(&tls.config) = config(true);
    let mut rotated = client
        .send(
            request(&dns_base, "rotated"),
            policy(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_ne!(id(&rotated), first_id);
    drain(&mut rotated).await.unwrap();
    *lock(&tls.config) = config_with_versions(true, &[&rustls::version::TLS12]);
    let mut tls12 = client
        .send(
            request(&dns_base, "tls12"),
            policy(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let tls12_id = id(&tls12);
    drain(&mut tls12).await.unwrap();
    let negotiated = peer
        .wait(|e| e["event"] == "secure" && e["id"] == tls12_id)
        .await;
    assert_eq!(negotiated["protocol"], "TLSv1.2");
    assert_eq!(negotiated["alpn"], "http/1.1");
    let mut ip = get(&client, &peer, "ip").await;
    let ip_id = id(&ip);
    drain(&mut ip).await.unwrap();
    let secure = peer
        .wait(|e| e["event"] == "secure" && e["id"] == ip_id)
        .await;
    assert!(secure["servername"] == false || secure["servername"] == "");
    let untrusted = Arc::new(ConfigSource {
        calls: AtomicUsize::new(0),
        config: Mutex::new(config(false)),
    });
    let bad = Http1Client::new(settings(untrusted));
    let failed = error(
        bad.send(
            request(&peer.base, "untrusted"),
            policy(),
            CancellationToken::new(),
        )
        .await,
    );
    assert_eq!(failed.kind, TransportErrorKind::Tls);
    assert!(failed.source().is_some());
    let mut alpn = (*config(true)).clone();
    alpn.alpn_protocols = vec![b"h2".to_vec()];
    let bad = Http1Client::new(settings(Arc::new(ConfigSource {
        calls: AtomicUsize::new(0),
        config: Mutex::new(Arc::new(alpn)),
    })));
    let before = lock(&peer.events)
        .iter()
        .filter(|e| e["event"] == "connection")
        .count();
    assert_eq!(
        error(
            bad.send(
                request(&peer.base, "h2"),
                policy(),
                CancellationToken::new()
            )
            .await
        )
        .kind,
        TransportErrorKind::Tls
    );
    assert_eq!(
        lock(&peer.events)
            .iter()
            .filter(|e| e["event"] == "connection")
            .count(),
        before
    );
    peer.observe("validated-https", json!({"dnsId":first_id,"rotatedPolicyId":id(&rotated),"tls12Id":tls12_id,"ipId":ip_id,"root":"fixture-only CA","provider":"aws-lc-rs","versions":["TLS1.3","TLS1.2"],"bothVersionsNegotiated":true,"alpn":"http/1.1","earlyData":false,"resumptionEntries":100,"untrustedRejected":true,"h2RejectedBeforeConnect":true}));
    drop(client);
    drop(bad);
    peer.finish().await;
    let wrong_name = Peer::start("https-name", true, "127.0.0.2").await;
    let client = Http1Client::new(settings(source()));
    assert_eq!(
        error(
            client
                .send(
                    request(&wrong_name.base, "name"),
                    policy(),
                    CancellationToken::new()
                )
                .await
        )
        .kind,
        TransportErrorKind::Tls
    );
    drop(client);
    wrong_name.finish().await;
}

#[tokio::test]
async fn tls_rejection_cancellation_and_stream_reuse() {
    let peer = Peer::start("https-retirement", true, "127.0.0.1").await;
    let client = Http1Client::new(settings(source()));
    let mut first = get(&client, &peer, "ok").await;
    let first_id = id(&first);
    drain(&mut first).await.unwrap();
    let mut second = get(&client, &peer, "segmented").await;
    assert_eq!(id(&second), first_id);
    assert_eq!(drain(&mut second).await.unwrap(), b"abcdef");
    for mode in [
        "100-coalesced",
        "100-split",
        "100-only",
        "100-after-103",
        "100-partial-final",
        "upgrade",
    ] {
        let rejected = error(
            client
                .send(
                    request(&peer.base, mode),
                    policy(),
                    CancellationToken::new(),
                )
                .await,
        );
        assert_eq!(
            rejected.kind,
            TransportErrorKind::RejectedHead(
                StatusCode::from_u16(if mode == "upgrade" { 101 } else { 100 }).unwrap()
            )
        );
        let event = peer.request(mode).await;
        let retired = event["id"].as_u64().unwrap();
        peer.closed(retired).await;
        let mut next = get(&client, &peer, &format!("after-{mode}")).await;
        let next_id = id(&next);
        assert_ne!(next_id, retired);
        drain(&mut next).await.unwrap();
        peer.observe(mode, json!({"rejectedId":retired,"replacementId":next_id,"responseExposed":false,"bodyExposed":false,"closed":true,"validatedHttps":true}));
    }
    let token = CancellationToken::new();
    let mut body = client
        .send(request(&peer.base, "stall-body"), policy(), token.clone())
        .await
        .unwrap();
    let retired = id(&body);
    assert_eq!(body.next_chunk().await.unwrap().unwrap(), "start");
    let mut reading = Box::pin(body.next_chunk());
    tokio::select! { _ = &mut reading => panic!("stalled body completed"), _ = release_tasks() => {} }
    token.cancel();
    assert_eq!(error(reading.await).kind, TransportErrorKind::Cancelled);
    peer.closed(retired).await;
    let mut next = get(&client, &peer, "after-body-cancel").await;
    assert_ne!(id(&next), retired);
    drain(&mut next).await.unwrap();
    let token = CancellationToken::new();
    let mut sending =
        Box::pin(client.send(request(&peer.base, "stall-head"), policy(), token.clone()));
    let event = tokio::select! { e = peer.request("stall-head") => e, _ = &mut sending => panic!("stalled head completed") };
    token.cancel();
    assert_eq!(error(sending.await).kind, TransportErrorKind::Cancelled);
    peer.closed(event["id"].as_u64().unwrap()).await;
    let unread = get(&client, &peer, "stall-body?drop").await;
    let unread_id = id(&unread);
    drop(unread);
    peer.closed(unread_id).await;
    peer.observe("validated-https-cancel-drop", json!({"pendingBodyCancelled":true,"bodyId":retired,"headId":event["id"],"unreadId":unread_id,"allClosed":true}));
    drop(client);
    peer.finish().await;
}

struct PendingSource {
    entered: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
}
struct PolicyGuard(Arc<AtomicUsize>);
impl Drop for PolicyGuard {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
impl TlsConfigSource for PendingSource {
    fn for_origin<'a>(&'a self, _: &'a Uri) -> ConfigFuture<'a> {
        Box::pin(async move {
            self.entered.fetch_add(1, Ordering::SeqCst);
            let _guard = PolicyGuard(self.dropped.clone());
            std::future::pending().await
        })
    }
}
#[tokio::test]
async fn setup_cancellation_and_dropped_futures_own_pending_resources() {
    let peer = Peer::start("http-setup", false, "127.0.0.1").await;
    let entered = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    let client = Http1Client::new(settings(Arc::new(PendingSource {
        entered: entered.clone(),
        dropped: dropped.clone(),
    })));
    for cancel in [true, false] {
        let token = CancellationToken::new();
        let tls_base = peer.base.replace("http:", "https:");
        let mut future =
            Box::pin(client.send(request(&tls_base, "policy"), policy(), token.clone()));
        tokio::select! { _ = &mut future => panic!("pending policy completed"), _ = release_tasks() => {} }
        if cancel {
            token.cancel();
            assert_eq!(error(future.await).kind, TransportErrorKind::Cancelled);
        } else {
            drop(future);
        }
    }
    assert_eq!(entered.load(Ordering::SeqCst), 2);
    assert_eq!(dropped.load(Ordering::SeqCst), 2);
    assert!(
        !lock(&peer.events)
            .iter()
            .any(|e| e["event"] == "connection")
    );
    drop(client);
    // A plain peer leaves TLS handshakes pending; cancel/drop must close the actual TCP stream.
    let client = Http1Client::new(settings(source()));
    for (index, cancel) in [true, false].into_iter().enumerate() {
        let token = CancellationToken::new();
        let tls_base = peer.base.replace("http:", "https:");
        let mut future =
            Box::pin(client.send(request(&tls_base, "tls-pending"), policy(), token.clone()));
        let observed = tokio::select! {
            event = peer.wait(|e| e["event"] == "connection" && e["id"] == (index + 1) as u64) => event,
            _ = &mut future => panic!("pending TLS completed"),
        };
        if cancel {
            token.cancel();
            assert_eq!(error(future.await).kind, TransportErrorKind::Cancelled);
        } else {
            drop(future);
        }
        peer.closed(observed["id"].as_u64().unwrap()).await;
    }
    let invalid = Request::get("/relative")
        .body(Full::new(Bytes::new()))
        .unwrap();
    assert_eq!(
        error(
            client
                .send(invalid, policy(), CancellationToken::new())
                .await
        )
        .kind,
        TransportErrorKind::InvalidRequest
    );
    let mut invalid = request(&peer.base, "bad-version");
    *invalid.version_mut() = hyper::Version::HTTP_2;
    assert_eq!(
        error(
            client
                .send(invalid, policy(), CancellationToken::new())
                .await
        )
        .kind,
        TransportErrorKind::InvalidRequest
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    assert_eq!(
        error(
            client
                .send(
                    request(&format!("http://{address}"), "refused"),
                    policy(),
                    CancellationToken::new()
                )
                .await
        )
        .kind,
        TransportErrorKind::Connect
    );
    peer.observe("owned-setup", json!({"policyEntered":2,"policyDropped":2,"tlsPendingCancelClosed":true,"tlsPendingDropClosed":true,"refusedConnect":"Connect","invalidRequest":"InvalidRequest","dnsDelayInjected":false}));
    drop(client);
    peer.finish().await;
}

#[test]
fn retirement_drops_concrete_io_synchronously_and_preserves_first_error() {
    struct StreamDrop(Arc<AtomicUsize>);
    impl Drop for StreamDrop {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let dropped = Arc::new(AtomicUsize::new(0));
    let socket = Physical::new(StreamDrop(dropped.clone()), None);
    assert!(socket.retire(1, TransportErrorKind::RejectedHead(StatusCode::CONTINUE)));
    assert_eq!(
        dropped.load(Ordering::SeqCst),
        1,
        "stream Drop must finish before retire returns"
    );
    assert!(!socket.retire(1, TransportErrorKind::Cancelled));
    assert_eq!(
        socket.terminal(1),
        Some(TransportErrorKind::RejectedHead(StatusCode::CONTINUE))
    );
    drop(socket);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}
