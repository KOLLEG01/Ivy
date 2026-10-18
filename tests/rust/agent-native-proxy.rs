use ivy_host_runtime::codex_rpc;
#[path = "../../packages/host-runtime/rust/src/codex_proxy_transport.rs"]
mod transport;

use futures_util::{SinkExt, StreamExt};
use std::{io, time::Duration};
use tokio::io::{
    AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, DuplexStream,
};
use tokio_tungstenite::{
    WebSocketStream, accept_hdr_async,
    tungstenite::{
        Message,
        handshake::server::{Request, Response},
        protocol::frame::{
            Frame,
            coding::{Data, OpCode},
        },
    },
};
use transport::ProxyTransport;

async fn fixture() -> (ProxyTransport, WebSocketStream<DuplexStream>) {
    let (client, server) = tokio::io::duplex(65536);
    let (read, write) = tokio::io::split(client);
    let (transport, socket) = tokio::join!(
        ProxyTransport::start(write, read),
        accept_hdr_async(server, |request: &Request, response: Response| {
            assert_eq!(request.uri().path(), "/");
            assert_eq!(request.headers()["host"], "localhost");
            assert!(request.headers().get("sec-websocket-extensions").is_none());
            Ok(response)
        })
    );
    (transport.unwrap(), socket.unwrap())
}
async fn line(output: &mut BufReader<transport::ProxyOutput>) -> String {
    let mut text = String::new();
    tokio::time::timeout(Duration::from_secs(3), output.read_line(&mut text))
        .await
        .unwrap()
        .unwrap();
    text
}
async fn end_error(output: &mut transport::ProxyOutput) -> io::ErrorKind {
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), output.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap_err()
        .kind()
}

#[tokio::test]
async fn proxy_text_fragmentation_ping_and_clean_close_match_original_transport() {
    let (mut transport, mut socket) = fixture().await;
    let mut trace = Vec::<String>::new();
    transport
        .input
        .write_all("{\"hello\":\"Grüße😀\"}\n".as_bytes())
        .await
        .unwrap();
    let sent = socket.next().await.unwrap().unwrap();
    trace.push(sent.to_text().unwrap().to_owned());
    assert!(matches!(sent, Message::Text(_))); // server rejects unmasked client frames
    socket
        .send(Message::Frame(Frame::message(
            b"{\"result\":\"Gr\xc3".to_vec(),
            OpCode::Data(Data::Text),
            false,
        )))
        .await
        .unwrap();
    socket
        .send(Message::Ping(b"probe".to_vec().into()))
        .await
        .unwrap();
    assert_eq!(
        socket.next().await.unwrap().unwrap(),
        Message::Pong(b"probe".to_vec().into())
    );
    socket
        .send(Message::Frame(Frame::message(
            b"\xbc\xc3\x9fe\"}".to_vec(),
            OpCode::Data(Data::Continue),
            true,
        )))
        .await
        .unwrap();
    let mut output = BufReader::new(transport.output);
    trace.push(line(&mut output).await);
    socket.send(Message::Text("".into())).await.unwrap();
    trace.push(line(&mut output).await);
    let peer = tokio::spawn(async move {
        let Some(Ok(Message::Close(Some(frame)))) = socket.next().await else {
            panic!("missing close frame")
        };
        assert_eq!(u16::from(frame.code), 1000);
        let _ = socket.flush().await;
        format!("{}:{}", u16::from(frame.code), frame.reason)
    });
    transport.control.close().await;
    transport.control.close().await;
    trace.push(peer.await.unwrap());
    assert_eq!(line(&mut output).await, "");
    assert!(transport.input.write_all(b"{}\n").await.is_err());
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let oracle = std::process::Command::new("node")
        .arg(root.join("tests/fixtures/native-proxy-oracle.mjs"))
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        oracle.status.success(),
        "{}",
        String::from_utf8_lossy(&oracle.stderr)
    );
    let actual = ivy_contracts::IvyJson::Array(trace.into_iter().map(Into::into).collect());
    let expected =
        ivy_contracts::json::parse(std::str::from_utf8(&oracle.stdout).unwrap(), 65536).unwrap();
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn proxy_binary_invalid_utf8_and_oversize_frames_fail_the_reader() {
    for message in [
        Message::Binary(vec![1, 2].into()),
        Message::Frame(Frame::message(vec![0xff], OpCode::Data(Data::Text), true)),
        Message::Text("x".repeat(codex_rpc::NATIVE_FRAME_BYTES + 1).into()),
    ] {
        let (mut transport, mut socket) = fixture().await;
        let peer = tokio::spawn(async move {
            let _ = socket.send(message).await;
        });
        assert_eq!(
            end_error(&mut transport.output).await,
            io::ErrorKind::InvalidData
        );
        transport.control.close().await;
        peer.await.unwrap();
    }
}

#[tokio::test]
async fn proxy_rejects_incomplete_or_oversize_writes_but_preserves_exact_frame_limit() {
    let (mut transport, mut socket) = fixture().await;
    for bytes in [
        vec![],
        b"{}".to_vec(),
        vec![b'\n'; codex_rpc::NATIVE_REQUEST_FRAME_BYTES + 2],
    ] {
        // AsyncWriteExt::write_all intentionally short-circuits the empty buffer.
        let result = futures_util::future::poll_fn(|cx| {
            std::pin::Pin::new(&mut transport.input).poll_write(cx, &bytes)
        })
        .await;
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
    let peer = tokio::spawn(async move {
        let text = socket.next().await.unwrap().unwrap().into_text().unwrap();
        assert_eq!(text.len(), codex_rpc::NATIVE_REQUEST_FRAME_BYTES);
        socket
    });
    let mut maximum = vec![b'x'; codex_rpc::NATIVE_REQUEST_FRAME_BYTES];
    maximum.push(b'\n');
    transport.input.write_all(&maximum).await.unwrap();
    let mut socket = peer.await.unwrap();
    socket.close(None).await.unwrap();
    let mut rest = Vec::new();
    transport.output.read_to_end(&mut rest).await.unwrap();
    assert!(rest.is_empty());
}

#[tokio::test]
async fn proxy_close_is_bounded_with_blocked_writer_and_unresponsive_peer() {
    let (mut transport, socket) = fixture().await;
    let control = transport.control.clone();
    let writer = tokio::spawn(async move {
        transport
            .input
            .write_all(&vec![b'\n'; codex_rpc::NATIVE_REQUEST_FRAME_BYTES + 1])
            .await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!writer.is_finished());
    tokio::time::timeout(Duration::from_secs(2), control.close())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), writer)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    drop(socket);
}

#[tokio::test]
async fn proxy_backpressure_is_released_by_close_without_a_reader() {
    let (transport, mut socket) = fixture().await;
    let (first, sent) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        socket.send(Message::Text("first".into())).await.unwrap();
        let _ = first.send(());
        for _ in 0..100 {
            if socket
                .send(Message::Text("x".repeat(65536).into()))
                .await
                .is_err()
            {
                return;
            }
        }
        // Closing may drain already queued frames while completing the handshake.
    });
    sent.await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!peer.is_finished());
    tokio::time::timeout(Duration::from_secs(2), transport.control.close())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), peer)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn proxy_failed_upgrade_and_dropped_owners_release_the_pipes() {
    let (client, mut server) = tokio::io::duplex(65536);
    let (read, write) = tokio::io::split(client);
    let peer = tokio::spawn(async move {
        let mut request = [0; 4096];
        server.read(&mut request).await.unwrap();
        server
            .write_all(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
        let mut remaining = Vec::new();
        server.read_to_end(&mut remaining).await.unwrap();
    });
    let result = ProxyTransport::start(write, read).await;
    let error = result.err().unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let oracle = std::process::Command::new("node")
        .arg(root.join("tests/fixtures/native-proxy-failure-oracle.mjs"))
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        oracle.status.success(),
        "{}",
        String::from_utf8_lossy(&oracle.stderr)
    );
    assert_eq!(
        ivy_contracts::IvyJson::object([
            ("code", error.code.into()),
            ("message", error.message.into()),
            ("outcome", error.outcome.as_str().into())
        ]),
        ivy_contracts::json::parse(std::str::from_utf8(&oracle.stdout).unwrap(), 65536).unwrap()
    );
    tokio::time::timeout(Duration::from_secs(1), peer)
        .await
        .unwrap()
        .unwrap();
    let (transport, mut socket) = fixture().await;
    drop(transport);
    let result = tokio::time::timeout(Duration::from_secs(1), socket.next())
        .await
        .unwrap();
    assert!(result.is_none() || matches!(result, Some(Err(_))));
}

#[tokio::test]
async fn native_rpc_uses_proxy_text_and_retains_read_failure_on_binary_protocol() {
    let (transport, mut socket) = fixture().await;
    let catalog = ivy_contracts::json::parse(
        include_str!("../../specs/native/codex-0.154.0/catalog.json"),
        2 * 1024 * 1024,
    )
    .unwrap();
    let rpc = codex_rpc::NativeRpc::start(
        &catalog,
        transport.input,
        transport.output,
        Default::default(),
    )
    .unwrap();
    let peer = tokio::spawn(async move {
        let request = socket.next().await.unwrap().unwrap().into_text().unwrap();
        let request = ivy_contracts::json::parse(&request, 65536).unwrap();
        assert_eq!(request.get("method"), Some(&"account/logout".into()));
        assert_eq!(request.get("id"), Some(&"original-request".into()));
        socket
            .send(Message::Text(
                r#"{"id":"original-request","result":{"confirmed":true}}"#.into(),
            ))
            .await
            .unwrap();
        socket
    });
    let reply = rpc
        .request(
            "account/logout".into(),
            ivy_contracts::IvyJson::Null,
            codex_rpc::RequestOptions {
                request_id: Some("original-request".into()),
                timeout_ms: 3000,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        reply.get("result").unwrap().get("confirmed"),
        Some(&true.into())
    );
    let mut socket = peer.await.unwrap();
    socket.send(Message::Binary(vec![1].into())).await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), rpc.closed())
            .await
            .unwrap(),
        "native_read_failed"
    );
    transport.control.close().await;
}

#[tokio::test]
async fn keepalive_upgrade_rejection_fails_startup_without_crashing_or_leaking_pipes() {
    // Intentional crash fix: the Node Duplex tunnel can throw an uncaught
    // setTimeout TypeError on this response. Preserve the public error boundary,
    // not that accidental process crash. Normal rejection parity is tested above.
    let (client, mut server) = tokio::io::duplex(65536);
    let (read, write) = tokio::io::split(client);
    let peer = tokio::spawn(async move {
        let mut request = [0; 4096];
        server.read(&mut request).await.unwrap();
        server
            .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
        let mut remaining = Vec::new();
        server.read_to_end(&mut remaining).await.unwrap();
    });
    let error = tokio::time::timeout(Duration::from_secs(2), ProxyTransport::start(write, read))
        .await
        .unwrap()
        .err()
        .unwrap();
    assert_eq!(error, ivy_contracts::IvyError::internal());
    tokio::time::timeout(Duration::from_secs(1), peer)
        .await
        .unwrap()
        .unwrap();
}
