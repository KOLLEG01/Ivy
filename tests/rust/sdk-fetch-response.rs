use ivy_contracts::{
    IvyJson as J, JsString, Outcome, json::stringify, limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::{ClientOptions, HiveClient, RequestOptions, RpcClient};
use tokio::{io::AsyncWriteExt, net::TcpListener};

#[path = "../fixtures/rpc-http-peer.rs"]
#[allow(dead_code)]
mod http_peer;

async fn exchange(
    utf16: bool,
    status: u16,
    headers: &str,
    body: impl FnOnce(&J) -> Vec<u8> + Send + 'static,
) -> ivy_sdk::IvyResult<J> {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/prefix", listener.local_addr().unwrap());
    let headers = headers.to_owned();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let (request, trace) = http_peer::receive(&mut socket).await;
        assert_eq!(trace.get("path"), Some(&"/prefix/api/v1/rpc".into()));
        assert_eq!(trace.get("method"), Some(&"objects.write".into()));
        let body = body(&request);
        let head = format!(
            "HTTP/1.1 {status} Fixture\r\nConnection: close\r\n{headers}Content-Length: {}\r\n\r\n",
            body.len()
        );
        // A refused or oversized response may close the connection early.
        let _ = socket.write_all(head.as_bytes()).await;
        let _ = socket.write_all(&body).await;
        let _ = socket.shutdown().await;
        drop(socket);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "Uncertain writes must not redirect or replay."
        );
    });
    let client = if utf16 {
        HiveClient::new_js(&base.as_str().into(), ClientOptions::default())
    } else {
        HiveClient::new(&base, ClientOptions::default())
    }
    .unwrap();
    let result = client
        .request("objects.write", J::Null, RequestOptions::default())
        .await;
    server.await.unwrap();
    result
}
fn result(request: &J) -> Vec<u8> {
    stringify(
        &J::object([
            ("jsonrpc", "2.0".into()),
            ("id", request.get("id").unwrap().clone()),
            ("result", J::String(JsString(vec![0xe9, 0xd800]))),
        ]),
        usize::MAX,
    )
    .unwrap()
    .into_bytes()
}

#[tokio::test]
async fn native_response_validation_is_shared_by_both_constructors() {
    for utf16 in [false, true] {
        for status in [200, 201, 206, 400, 401, 404, 409, 500, 503] {
            assert_eq!(
                exchange(utf16, status, "Content-Type: text/plain\r\n", result)
                    .await
                    .unwrap(),
                J::String(JsString(vec![0xe9, 0xd800]))
            );
        }
        for status in [300, 301, 302, 303, 304, 305, 306, 307, 308, 399] {
            let error = exchange(utf16, status, "Location: /must-not-follow\r\n", result)
                .await
                .unwrap_err();
            assert_eq!(error.code, "outcome_unknown");
            assert_eq!(error.outcome, Outcome::Unknown);
        }
        for status in [101, 204, 205] {
            let error = exchange(utf16, status, "", |_| Vec::new())
                .await
                .unwrap_err();
            assert_eq!(error.code, "invalid_frame");
            assert_eq!(error.outcome, Outcome::Unknown);
        }
    }
}

#[tokio::test]
async fn native_response_rejects_malformed_envelopes_and_excess_bytes() {
    for utf16 in [false, true] {
        for (body, code) in [
            (Vec::new(), "invalid_frame"),
            (vec![0xff], "invalid_frame"),
            (
                b"{\"jsonrpc\":\"2.0\",\"id\":\"wrong\",\"result\":true}".to_vec(),
                "invalid_frame",
            ),
            (b"null".to_vec(), "invalid_frame"),
            (vec![b' '; MANAGEMENT_FRAME_BYTES + 1], "result_too_large"),
        ] {
            let error = exchange(utf16, 200, "", move |_| body).await.unwrap_err();
            assert_eq!(error.code, code);
            assert_eq!(error.outcome, Outcome::Unknown);
        }
        assert_eq!(
            exchange(utf16, 200, "", |request| {
                [b"\xef\xbb\xbf".as_slice(), &result(request)].concat()
            })
            .await
            .unwrap(),
            J::String(JsString(vec![0xe9, 0xd800]))
        );
    }
}

#[tokio::test]
async fn native_response_preserves_provider_errors_without_retry() {
    for utf16 in [false, true] {
        let error = exchange(utf16, 409, "", |request| {
            stringify(
                &J::object([
                    ("jsonrpc", "2.0".into()),
                    ("id", request.get("id").unwrap().clone()),
                    (
                        "error",
                        ivy_sdk::IvyError::new("revision_conflict", "Use the current revision.")
                            .with_outcome(Outcome::NotExecuted)
                            .to_wire(),
                    ),
                ]),
                usize::MAX,
            )
            .unwrap()
            .into_bytes()
        })
        .await
        .unwrap_err();
        assert_eq!(error.code, "revision_conflict");
        assert_eq!(error.outcome, Outcome::NotExecuted);
    }
}
