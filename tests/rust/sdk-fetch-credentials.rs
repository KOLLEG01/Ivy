use ivy_contracts::json::{parse, stringify};
use ivy_sdk::{IvyJson as J, JsString};
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

#[path = "../../packages/sdk/rust/src/fetch_header.rs"]
mod fetch_header;
#[path = "../fixtures/rpc-http-peer.rs"]
mod http_peer;

fn oracle(mode: &str, input: Option<&J>, label: &str) -> J {
    let mut child = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/sdk-fetch-credentials-oracle.mjs",
            mode,
        ])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stringify(input, usize::MAX).unwrap().as_bytes())
            .unwrap();
    } else {
        drop(child.stdin.take());
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    if let Some(directory) = std::env::var_os("IVY_TEST_RAW_EVIDENCE") {
        std::fs::write(
            Path::new(&directory).join(format!("sdk-fetch-credentials-original-{label}.json")),
            &output.stdout,
        )
        .unwrap();
    }
    value
}
fn row(name: &str, value: Option<JsString>) -> J {
    let mut fields = vec![("name", name.into())];
    if let Some(value) = value {
        fields.push(("credential", J::String(value)));
    }
    J::object(fields)
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn node_fetch_raw_credential_conversion_dispatch_admission_and_bytes() {
    let mut cases = vec![row("absent", None), row("empty", Some("".into()))];
    cases.extend((0..=255u16).map(|unit| row(&format!("unit-{unit}"), Some(JsString(vec![unit])))));
    for (name, value) in [
        ("trailing-crlf", "fixture\r\n".into()),
        ("internal-crlf", "a\r\nb".into()),
        ("surrogate", JsString(vec![0xd800])),
        ("paired", JsString(vec![0xd83d, 0xde00])),
        ("tabs-spaces", "\tfixture \t".into()),
        ("latin1-sequence", JsString(vec![233, 255, 128, 159])),
        ("above-latin1", JsString(vec![0x100])),
    ] {
        cases.push(row(name, Some(value)));
    }
    let original = oracle("run", Some(&J::Array(cases)), "unit-dispatch");
    let rows = original.get("rows").unwrap().as_array().unwrap();
    let mut accepted = Vec::new();
    for row in rows {
        assert_eq!(row.get("constructed"), Some(&J::Bool(true)));
        let credential = row
            .get("credential")
            .and_then(J::as_js_string)
            .cloned()
            .unwrap_or_default();
        let actual = fetch_header::authorization(&credential);
        let expected = row.get("result").unwrap().get("ok").unwrap() == &J::Bool(true);
        assert_eq!(
            actual.is_ok(),
            expected,
            "{}",
            row.get("name").unwrap().as_str().unwrap()
        );
        let requests = row.get("requests").unwrap().as_array().unwrap();
        match actual {
            Ok(value) => {
                assert_eq!(requests.len(), 1);
                let encoded = value
                    .as_ref()
                    .map(|value| J::from(hex(value.as_bytes())))
                    .unwrap_or(J::Null);
                assert_eq!(requests[0].get("authorization"), Some(&encoded));
                assert_eq!(
                    requests[0].get("params").unwrap().get("raw"),
                    Some(&J::String(JsString(vec![0xd800])))
                );
                if credential.0.len() == 1 {
                    accepted.push(credential.0[0]);
                }
            }
            Err(()) => {
                assert!(
                    requests.is_empty(),
                    "Rejected credentials must not dispatch or replay."
                );
                assert_eq!(
                    row.get("result")
                        .unwrap()
                        .get("error")
                        .unwrap()
                        .get("data")
                        .unwrap()
                        .get("code"),
                    Some(&"outcome_unknown".into())
                );
            }
        }
    }
    let headers = oracle("headers", None, "unit-conversion");
    let converted = headers.get("rows").unwrap().as_array().unwrap();
    assert!(
        converted
            .iter()
            .all(|row| row.get("unit").unwrap().as_f64().unwrap() <= 255.0)
    );
    accepted.sort_unstable();
    accepted.dedup();
    let actual: Vec<_> = (0..=65535u16)
        .filter(|unit| fetch_header::authorization(&JsString(vec![*unit])).is_ok())
        .collect();
    assert_eq!(
        actual, accepted,
        "All raw UTF16 units must retain Node conversion plus actual dispatch admission."
    );
}

#[tokio::test]
async fn raw_client_defers_credential_admission_and_preserves_wire_bytes() {
    use ivy_sdk::rpc::{ClientOptions, HiveClient, RequestOptions, RpcClient};
    use std::sync::{Arc, Mutex};
    let cases = [
        "",
        "fixture",
        "éÿ\u{80}\u{9f}",
        "fixture\r\n",
        "\t",
        "\n",
        "\r",
        "\u{7f}",
        "a\nb",
        "\0",
        "\u{100}",
    ]
    .into_iter()
    .enumerate()
    .map(|(i, value)| row(&format!("transport-{i}"), Some(value.into())))
    .chain([
        row("surrogate", Some(JsString(vec![0xd800]))),
        row("paired", Some(JsString(vec![0xd83d, 0xde00]))),
    ])
    .collect();
    let original = oracle("run", Some(&J::Array(cases)), "transport");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let traces = Arc::new(Mutex::new(Vec::<J>::new()));
    let observed = traces.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (frame, trace) = http_peer::receive(&mut socket).await;
            observed.lock().unwrap().push(trace);
            http_peer::reply(&mut socket, &frame, Ok(J::object([("ok", true.into())]))).await;
        }
    });
    let mut evidence = Vec::new();
    for (index, row) in original
        .get("rows")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let before = traces.lock().unwrap().len();
        let client = HiveClient::new_js(
            &format!("http://{address}/case/{index}").as_str().into(),
            ClientOptions {
                credential: row.get("credential").and_then(J::as_js_string).cloned(),
                ..ClientOptions::default()
            },
        )
        .expect("Raw credentials are not rejected during construction.");
        let result = match client
            .request(
                "fixture.echo",
                J::object([("raw", J::String(JsString(vec![0xd800])))]),
                RequestOptions::default(),
            )
            .await
        {
            Ok(value) => J::object([("ok", true.into()), ("value", value)]),
            Err(error) => J::object([("ok", false.into()), ("error", error.to_wire())]),
        };
        assert_eq!(row.get("result"), Some(&result));
        let requests = traces.lock().unwrap()[before..].to_vec();
        assert_eq!(
            row.get("requests").unwrap().as_array().unwrap().len(),
            requests.len()
        );
        for (actual, expected) in requests
            .iter()
            .zip(row.get("requests").unwrap().as_array().unwrap())
        {
            for key in ["authorization", "path", "method", "params"] {
                assert_eq!(actual.get(key), expected.get(key), "{key}");
            }
        }
        evidence.push(J::object([
            ("name", row.get("name").unwrap().clone()),
            ("result", result),
            ("requests", J::Array(requests)),
        ]));
    }
    let legacy = HiveClient::new(
        &format!("http://{address}/legacy"),
        ClientOptions {
            credential: Some("é".to_owned()),
            ..ClientOptions::default()
        },
    )
    .unwrap();
    legacy
        .request("fixture.echo", J::Null, RequestOptions::default())
        .await
        .unwrap();
    assert_eq!(
        traces.lock().unwrap().last().unwrap().get("authorization"),
        Some(&"42656172657220c3a9".into())
    );
    server.abort();
    let _ = server.await;
    if let Some(directory) = std::env::var_os("IVY_TEST_RAW_EVIDENCE") {
        std::fs::write(
            Path::new(&directory).join("sdk-fetch-credentials-native-transport.json"),
            stringify(&J::Array(evidence), usize::MAX).unwrap(),
        )
        .unwrap();
    }
}
