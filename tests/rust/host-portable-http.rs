use flate2::{Compression, write::GzEncoder};
use ivy_host_runtime::{
    portable_dependencies::MAXIMUM_PORTABLE_PACKAGE_BYTES, portable_http::PortableHttp,
};
use std::io::Write;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

async fn serve(responses: Vec<Vec<u8>>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = vec![];
        for response in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![];
            while !request.ends_with(b"\r\n\r\n") {
                request.push(socket.read_u8().await.unwrap());
                assert!(request.len() < 16384);
            }
            requests.push(String::from_utf8(request).unwrap());
            socket.write_all(&response).await.unwrap();
            socket.shutdown().await.unwrap();
        }
        requests
    });
    (base, task)
}

#[tokio::test]
async fn redirects_and_standard_decoding_use_an_ivy_identity() {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(b"pinned package bytes").unwrap();
    let gzip = encoder.finish().unwrap();
    let mut response = format!("HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", gzip.len()).into_bytes();
    response.extend(gzip);
    let (base, task) = serve(vec![b"HTTP/1.1 302 Found\r\nLocation: /package\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(), response]).await;
    let http = PortableHttp::new(reqwest::Client::builder().no_proxy()).unwrap();
    assert_eq!(
        http.fetch(&format!("{base}/start").into()).await.unwrap(),
        b"pinned package bytes"
    );
    let requests = task.await.unwrap();
    assert!(requests[1].starts_with("GET /package "));
    for request in requests {
        let headers = request.to_ascii_lowercase();
        assert!(headers.contains("user-agent: ivy-host\r\n"));
        assert!(!headers.contains("sec-fetch-mode:") && !headers.contains("accept-language:"));
    }
}

#[tokio::test]
async fn excessive_lengths_corrupt_streams_and_unsafe_redirects_are_refused() {
    let excessive = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        MAXIMUM_PORTABLE_PACKAGE_BYTES + 1
    )
    .into_bytes();
    for response in [excessive,
        b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 3\r\nConnection: close\r\n\r\nbad".to_vec(),
        b"HTTP/1.1 302 Found\r\nLocation: http://user:password@localhost/package\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()] {
        let (base, task) = serve(vec![response]).await;
        let http = PortableHttp::new(reqwest::Client::builder().no_proxy()).unwrap();
        assert!(http.fetch(&base.into()).await.is_err()); task.await.unwrap();
    }
}
