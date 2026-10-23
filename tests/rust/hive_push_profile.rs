use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_hive::{
    browser_push_https::NativeHttps,
    browser_push_native::{Https, Request},
};
use serde_json::Value;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    task::{JoinHandle, JoinSet},
    time::Instant,
};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        self, ServerConfig,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    },
};

#[derive(Debug)]
struct Received {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}
struct Peer {
    address: SocketAddr,
    records: Arc<Mutex<Vec<Received>>>,
    task: JoinHandle<()>,
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn pem(value: &str) -> Vec<u8> {
    STANDARD
        .decode(
            value
                .lines()
                .filter(|line| !line.starts_with('-'))
                .collect::<String>(),
        )
        .unwrap()
}
impl Peer {
    async fn start() -> Self {
        let fixture: Value =
            serde_json::from_str(include_str!("../fixtures/hive-push-tls.json")).unwrap();
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(pem(fixture["cert"].as_str().unwrap()))],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pem(fixture["key"]
                    .as_str()
                    .unwrap()))),
            )
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let records = Arc::new(Mutex::new(Vec::new()));
        let received = records.clone();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    incoming = listener.accept() => {
                        let (stream, _) = incoming.unwrap();
                        let acceptor = acceptor.clone(); let records = received.clone();
                        connections.spawn(async move {
                            let Ok(stream) = acceptor.accept(stream).await else { return; };
                            let mut stream = BufReader::new(stream);
                            let mut first = String::new();
                            stream.read_line(&mut first).await.unwrap();
                            let mut parts = first.split_whitespace();
                            let method = parts.next().unwrap().to_owned();
                            let path = parts.next().unwrap().to_owned();
                            let mut headers = HashMap::new();
                            loop {
                                let mut line = String::new(); stream.read_line(&mut line).await.unwrap();
                                if line == "\r\n" { break; }
                                let (name, value) = line.split_once(':').unwrap();
                                headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
                            }
                            let length = headers.get("content-length").unwrap().parse().unwrap();
                            let mut body = vec![0; length]; stream.read_exact(&mut body).await.unwrap();
                            records.lock().unwrap().push(Received { method, path: path.clone(), headers, body });
                            let response = match path.as_str() {
                                "/redirect" => "HTTP/1.1 307 Temporary Redirect\r\nLocation: /status/201\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                                "/status/410" => "HTTP/1.1 410 Gone\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                                "/partial" => "HTTP/1.1 201 Created\r\nContent-Length: 4\r\nConnection: close\r\n\r\nab",
                                "/compressed" => "HTTP/1.1 201 Created\r\nContent-Length: 2\r\nContent-Encoding: gzip\r\nConnection: close\r\n\r\na",
                                _ => "HTTP/1.1 201 Created\r\nContent-Length: 2\r\nConnection: close\r\n\r\na",
                            };
                            stream.get_mut().write_all(response.as_bytes()).await.unwrap();
                            stream.get_mut().flush().await.unwrap();
                            if path == "/delay" {
                                tokio::time::sleep(Duration::from_millis(150)).await;
                                stream.get_mut().write_all(b"b").await.unwrap();
                            } else if path == "/stall" {
                                tokio::time::sleep(Duration::from_secs(20)).await;
                            } else if path != "/partial" && path != "/redirect" && path != "/status/410" {
                                stream.get_mut().write_all(b"b").await.unwrap();
                            }
                            let _ = stream.get_mut().shutdown().await;
                        });
                    },
                    _ = connections.join_next(), if !connections.is_empty() => {},
                }
            }
        });
        Self {
            address,
            records,
            task,
        }
    }
    fn client(&self) -> NativeHttps {
        let fixture: Value =
            serde_json::from_str(include_str!("../fixtures/hive-push-tls.json")).unwrap();
        NativeHttps::from_builder(
            reqwest::Client::builder()
                .proxy(reqwest::Proxy::all("http://127.0.0.1:9").unwrap())
                .add_root_certificate(
                    reqwest::Certificate::from_pem(fixture["ca"].as_str().unwrap().as_bytes())
                        .unwrap(),
                )
                .resolve("localhost", self.address),
        )
        .unwrap()
    }
    fn request(&self, path: &str) -> Request {
        Request {
            endpoint: format!("https://localhost:{}{path}", self.address.port()),
            headers: vec![
                ("TTL".into(), "3600".into()),
                ("Content-Encoding".into(), "aes128gcm".into()),
                ("Authorization".into(), "vapid test-token".into()),
            ],
            body: b"encrypted-notification".to_vec(),
        }
    }
}

#[tokio::test]
async fn native_https_checks_trust_preserves_delivery_and_waits_for_body_end() {
    let peer = Peer::start().await;
    let client = peer.client();
    let start = Instant::now();
    assert_eq!(client.post(peer.request("/delay")).await.unwrap(), 201);
    assert!(start.elapsed() >= Duration::from_millis(150));
    assert_eq!(client.post(peer.request("/status/410")).await.unwrap(), 410);
    assert_eq!(client.post(peer.request("/redirect")).await.unwrap(), 307);
    assert_eq!(client.post(peer.request("/compressed")).await.unwrap(), 201);
    assert!(client.post(peer.request("/partial")).await.is_err());
    let untrusted =
        NativeHttps::from_builder(reqwest::Client::builder().resolve("localhost", peer.address))
            .unwrap();
    assert!(untrusted.post(peer.request("/status/201")).await.is_err());
    let mut insecure = peer.request("/status/201");
    insecure.endpoint = insecure.endpoint.replacen("https:", "http:", 1);
    assert!(client.post(insecure).await.is_err());
    let records = peer.records.lock().unwrap();
    assert_eq!(
        records.len(),
        5,
        "No redirect, retry, untrusted or plaintext delivery."
    );
    assert_eq!(
        records.iter().map(|v| v.path.as_str()).collect::<Vec<_>>(),
        [
            "/delay",
            "/status/410",
            "/redirect",
            "/compressed",
            "/partial"
        ]
    );
    for record in records.iter() {
        assert_eq!(record.method, "POST");
        assert_eq!(record.body, b"encrypted-notification");
        assert_eq!(record.headers["ttl"], "3600");
        assert_eq!(record.headers["content-encoding"], "aes128gcm");
        assert_eq!(record.headers["authorization"], "vapid test-token");
    }
}

#[tokio::test]
async fn native_https_deadline_bounds_an_unfinished_response() {
    let peer = Peer::start().await;
    let result = tokio::time::timeout(
        Duration::from_secs(12),
        peer.client().post(peer.request("/stall")),
    )
    .await;
    assert!(
        result
            .expect("The configured ten-second deadline must end the request.")
            .is_err()
    );
    assert_eq!(peer.records.lock().unwrap().len(), 1);
}
