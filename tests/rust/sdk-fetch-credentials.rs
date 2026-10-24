use ivy_sdk::{
    IvyJson as J, JsString, Outcome,
    rpc::{ClientOptions, HiveClient, RequestOptions, RpcClient},
    service::{ConnectionOptions, ServiceClient, ServiceConnection, ServiceOptions},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio_util::sync::CancellationToken;

#[path = "../fixtures/rpc-http-peer.rs"]
mod http_peer;

fn client(base: &str, credential: Option<&str>, utf16: bool) -> HiveClient {
    if utf16 {
        HiveClient::new_js(
            &base.into(),
            ClientOptions {
                credential: credential.map(JsString::from),
                ..Default::default()
            },
        )
    } else {
        HiveClient::new(
            base,
            ClientOptions {
                credential: credential.map(String::from),
                ..Default::default()
            },
        )
    }
    .unwrap()
}

async fn provider(
    base: &str,
    credential: &str,
    utf16: bool,
) -> ivy_sdk::IvyResult<ServiceConnection> {
    let options = ConnectionOptions::new("fixture-node".into());
    let cancellation = CancellationToken::new();
    if utf16 {
        ServiceConnection::connect_js(base, &credential.into(), options, cancellation).await
    } else {
        ServiceConnection::connect(base, credential, options, cancellation).await
    }
}

#[tokio::test]
async fn native_credentials_share_authentication_and_lossless_rpc_payloads() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/prefix", listener.local_addr().unwrap());
    let credentials = [None, Some(""), Some("fixture"), Some("é空😀")];
    let server = tokio::spawn(async move {
        for _ in 0..credentials.len() * 2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (frame, trace) = http_peer::receive(&mut socket).await;
            http_peer::reply(&mut socket, &frame, Ok(trace)).await;
        }
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "Requests must not redirect or replay."
        );
    });
    let params = J::object([("raw", J::String(JsString(vec![0xd800])))]);
    for credential in credentials {
        let authorization = credential
            .filter(|value| !value.is_empty())
            .map(|value| {
                J::from(
                    format!("Bearer {value}")
                        .bytes()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>(),
                )
            })
            .unwrap_or(J::Null);
        for utf16 in [false, true] {
            let trace = client(&base, credential, utf16)
                .request("fixture.echo", params.clone(), RequestOptions::default())
                .await
                .unwrap();
            assert_eq!(trace.get("authorization"), Some(&authorization));
            assert_eq!(trace.get("path"), Some(&"/prefix/api/v1/rpc".into()));
            assert_eq!(trace.get("method"), Some(&"fixture.echo".into()));
            assert_eq!(trace.get("params"), Some(&params));
        }
    }
    server.await.unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let base = format!("{origin}/prefix");
    let server = tokio::spawn(async move {
        for credential in ["fixture", "é空😀"] {
            for _ in 0..2 {
                let (socket, _) = listener.accept().await.unwrap();
                let expected_origin = origin.clone();
                let socket = tokio_tungstenite::accept_hdr_async(
                    socket,
                    move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                          response| {
                        assert_eq!(request.uri().path(), "/prefix/ws");
                        assert_eq!(
                            request.headers()["origin"].as_bytes(),
                            expected_origin.as_bytes()
                        );
                        assert_eq!(
                            request.headers()["authorization"].as_bytes(),
                            format!("Bearer {credential}").as_bytes()
                        );
                        Ok(response)
                    },
                )
                .await
                .unwrap();
                drop(socket);
            }
        }
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "Provider handshakes must not replay."
        );
    });
    for credential in ["fixture", "é空😀"] {
        for utf16 in [false, true] {
            let connection = provider(&base, credential, utf16).await.unwrap();
            connection.close();
            connection.closed().await;
        }
    }
    server.await.unwrap();
}

#[tokio::test]
async fn invalid_header_credentials_never_dispatch_or_replay() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    for credential in ["fixture\r\n", "a\nb", "\0", "\u{7f}"] {
        for utf16 in [false, true] {
            let error = client(&base, Some(credential), utf16)
                .request("objects.write", J::Null, RequestOptions::default())
                .await
                .unwrap_err();
            assert_eq!(error.code, "outcome_unknown");
            assert_eq!(error.outcome, Outcome::Unknown);
            let error = provider(&base, credential, utf16)
                .await
                .err()
                .expect("Invalid provider header");
            assert_eq!(error.code, "internal_error");
            assert_eq!(error.outcome, Outcome::Unknown);
        }
    }
    let error = HiveClient::new_js(
        &base.as_str().into(),
        ClientOptions {
            credential: Some(JsString(vec![0xd800])),
            ..Default::default()
        },
    )
    .err()
    .expect("A native credential requires Unicode text.");
    assert_eq!(error.code, "invalid_arguments");
    let error = ServiceConnection::connect_js(
        &base,
        &JsString(vec![0xd800]),
        ConnectionOptions::new("fixture-node".into()),
        CancellationToken::new(),
    )
    .await
    .err()
    .expect("A provider credential requires Unicode text.");
    assert_eq!(error.code, "invalid_arguments");

    for (credential, code) in [
        (JsString::from("fixture\r\n"), "internal_error"),
        (JsString(vec![0xd800]), "invalid_arguments"),
    ] {
        let credential_calls = Arc::new(AtomicUsize::new(0));
        let observed_calls = credential_calls.clone();
        let registry_calls = Arc::new(AtomicUsize::new(0));
        let observed_registry = registry_calls.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let sender = Arc::new(Mutex::new(Some(sender)));
        let mut options = ServiceOptions::new_js(
            base.clone(),
            Arc::new(move || {
                let credential = credential.clone();
                let calls = observed_calls.clone();
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(credential)
                })
            }),
            J::object([
                ("serviceNodeId", "fixture-node".into()),
                ("serviceName", "fixture".into()),
                ("hostId", "fixture-host".into()),
                ("version", "1.0.0".into()),
                ("buildId", format!("sha256:{}", "a".repeat(64)).into()),
                ("hiveProtocol", J::Number(1.0)),
            ]),
            Arc::new(move || {
                let calls = observed_registry.clone();
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(J::Null)
                })
            }),
            Arc::new(|_| {
                Box::pin(async { panic!("Reconciliation must not run for invalid credentials") })
            }),
        );
        options.reconnect_min = std::time::Duration::from_secs(60);
        options.reconnect_max = options.reconnect_min;
        options.on_state = Some(Arc::new(move |state| {
            if state.status == "offline" {
                if let Some(sender) = sender.lock().unwrap().take() {
                    let _ = sender.send(state);
                }
            }
        }));
        let client = ServiceClient::new(options).unwrap();
        client.start();
        let state = tokio::time::timeout(std::time::Duration::from_secs(3), receiver)
            .await
            .unwrap()
            .unwrap();
        client.stop().await;
        assert_eq!(state.code, Some(code.into()));
        assert_eq!(state.phase, Some("credential"));
        assert!(!client.ready());
        assert_eq!(credential_calls.load(Ordering::SeqCst), 1);
        assert_eq!(registry_calls.load(Ordering::SeqCst), 0);
    }
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}
