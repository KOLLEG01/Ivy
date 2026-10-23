use ivy_sdk::{
    IvyJson as J, JsString, Outcome,
    rpc::{ClientOptions, HiveClient, RequestOptions, RpcClient},
};

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
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}
