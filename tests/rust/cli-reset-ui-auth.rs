use super::*;
use ivy_cli::{
    publish_ui::read_ui_bundle,
    reset_ui::{ResetUiPublisher, reset_ui_credential},
};
use ivy_sdk::clock::Clock;
use std::{
    io::Write,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};
#[path = "../fixtures/rpc-http-peer.rs"]
mod http_peer;

struct FixedClock {
    calls: AtomicUsize,
    time: i64,
}
impl Clock for FixedClock {
    fn utc_millis(&self) -> i64 {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.time
    }
    fn monotonic(&self) -> Instant {
        panic!("The host owns the retry deadline.")
    }
}
fn clock(time: i64) -> FixedClock {
    FixedClock {
        calls: AtomicUsize::new(0),
        time,
    }
}
fn original(input: &J) -> J {
    let mut child = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/sdk-fetch-credentials-oracle.mjs",
            "reset",
        ])
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stringify(input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    if let Some(directory) = std::env::var_os("IVY_TEST_RAW_EVIDENCE") {
        std::fs::write(
            std::path::Path::new(&directory).join("reset-ui-auth-original-order.json"),
            &output.stdout,
        )
        .unwrap();
    }
    json(std::str::from_utf8(&output.stdout).unwrap())
}

#[test]
fn configured_credential_and_constructor_clock_error_order_match_original() {
    let selections = json(
        r#"[
      {"instances":[]},{"instances":[{}]},
      {"instances":[{"credential":""},{"credential":"later"}]},
      {"instances":[{"credential":null},{"credential":"first"},{"credential":"later"}]},
      {"instances":[{"credential":" \t"},{"credential":"later"}]},
      {"instances":[{"credential":"\ud800"}]},
      {"instances":[null]},
      {"instances":[{"credential":false},{"credential":0},{"credential":"later"}]}
    ]"#,
    );
    let constructors = [
        (
            "normal",
            "http://127.0.0.1/path",
            "fixture",
            "epoch",
            "reset",
            100_i64,
        ),
        (
            "raw-credential",
            "http://127.0.0.1/",
            "\u{100}",
            "epoch",
            "reset",
            100,
        ),
        (
            "base-before-time",
            "http://remote.invalid/",
            "fixture",
            "bad:epoch",
            "reset",
            100,
        ),
        (
            "malformed-url",
            "invalid-url",
            "fixture",
            "epoch",
            "reset",
            100,
        ),
        (
            "bad-epoch",
            "http://127.0.0.1/",
            "fixture",
            "bad:epoch",
            "reset",
            100,
        ),
        (
            "negative-time",
            "http://127.0.0.1/",
            "fixture",
            "epoch",
            "reset",
            -1,
        ),
        (
            "unsafe-time",
            "http://127.0.0.1/",
            "fixture",
            "epoch",
            "reset",
            9_007_199_254_740_992,
        ),
        (
            "bad-nonce",
            "http://127.0.0.1/",
            "fixture",
            "epoch",
            "bad:reset",
            100,
        ),
    ]
    .map(|(name, base, credential, epoch, reset_id, time)| {
        J::object([
            ("name", name.into()),
            ("base", base.into()),
            ("credential", credential.into()),
            ("epoch", epoch.into()),
            ("resetId", reset_id.into()),
            (
                "times",
                J::Array(vec![J::Number(time as f64), J::Number(200.0)]),
            ),
        ])
    })
    .to_vec();
    let mut constructors = constructors;
    for (name, epoch, reset) in [
        (
            "raw-identity",
            JsString(vec![101, 0xd800]),
            JsString(vec![114, 0xdc00]),
        ),
        ("long-nonce", "epoch".into(), JsString(vec![114; 128])),
    ] {
        constructors.push(J::object([
            ("name", name.into()),
            ("base", "http://127.0.0.1/".into()),
            ("credential", "fixture".into()),
            ("epoch", J::String(epoch)),
            ("resetId", J::String(reset)),
            ("times", J::Array(vec![J::Number(100.0), J::Number(200.0)])),
        ]));
    }
    let expected = original(&J::object([
        ("selections", selections),
        ("constructors", J::Array(constructors)),
    ]));
    for row in expected.get("selections").unwrap().as_array().unwrap() {
        let selected =
            reset_ui_credential(row.get("instances").unwrap().as_array().unwrap()).map(|value| {
                J::object([
                    ("found", value.is_some().into()),
                    ("value", value.map(J::String).unwrap_or(J::Null)),
                ])
            });
        assert_eq!(captured(selected), *row.get("result").unwrap());
    }
    let mut evidence = Vec::new();
    for row in expected.get("constructors").unwrap().as_array().unwrap() {
        let time = row.get("times").unwrap().as_array().unwrap()[0]
            .as_f64()
            .unwrap() as i64;
        let clock = clock(time);
        let value = ResetUiPublisher::new(
            row.get("base").unwrap().as_js_string().unwrap(),
            row.get("credential").unwrap().as_js_string().unwrap(),
            row.get("epoch").unwrap().as_js_string().unwrap(),
            row.get("resetId").unwrap().as_js_string().unwrap(),
            &clock,
        );
        let observed = captured(value.map(|publisher| J::String(publisher.mutation_id().clone())));
        let result = row.get("result").unwrap();
        let want = if result.get("ok") == Some(&true.into()) {
            J::object([
                ("ok", true.into()),
                (
                    "value",
                    result
                        .get("value")
                        .unwrap()
                        .get("mutationId")
                        .unwrap()
                        .clone(),
                ),
            ])
        } else {
            result.clone()
        };
        assert_eq!(
            observed,
            want,
            "{}",
            row.get("name").unwrap().as_str().unwrap()
        );
        let expected_calls = row
            .get("trace")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| **v == J::from("now"))
            .count()
            .min(1);
        assert_eq!(clock.calls.load(Ordering::SeqCst), expected_calls);
        evidence.push(J::object([
            ("name", row.get("name").unwrap().clone()),
            ("result", observed),
            ("clockCalls", J::Number(expected_calls as f64)),
        ]));
    }
    if let Some(directory) = std::env::var_os("IVY_TEST_RAW_EVIDENCE") {
        std::fs::write(
            std::path::Path::new(&directory).join("reset-ui-auth-native-order.json"),
            stringify(&J::Array(evidence), usize::MAX).unwrap(),
        )
        .unwrap();
    }
}

#[tokio::test]
async fn publisher_reuses_actual_authenticated_client_and_identity_across_attempts() {
    let parent = std::env::var_os("IVY_TEST_TEMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let directory = parent.join(format!(
        "ivy-reset-ui-auth-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    let fixture = Fixture(directory.clone());
    let definition = json(
        r#"{"metadata":{"uiId":"fixture","displayName":"Fixture","description":"Fixture","iconKey":"ui"},"entryPath":"index.html","requirements":{"hiveProtocol":1,"contracts":[],"services":[]},"dataContracts":[]}"#,
    );
    std::fs::write(
        directory.join("index.html"),
        "<!doctype html><h1>Fixture</h1>",
    )
    .unwrap();
    let checked_bundle = read_ui_bundle(&directory, &definition).unwrap();
    let release = checked_bundle.release_id.clone();
    let bundles = vec![ResetUiBundle {
        directory: directory.clone(),
        definition,
        checked_bundle,
    }];
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let trace = Arc::new(Mutex::new(Vec::<J>::new()));
    let collected = trace.clone();
    let server = tokio::spawn(async move {
        for index in 0..6 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (frame, request) = http_peer::receive(&mut socket).await;
            collected.lock().unwrap().push(request);
            if index == 0 {
                continue;
            }
            let result = match index {
                1 => Err(IvyError::new("not_found", "Fixture missing.")
                    .with_outcome(Outcome::NotExecuted)),
                2 | 3 => Ok(J::Null),
                _ => Ok(J::object([
                    ("status", "ready".into()),
                    ("currentReleaseId", release.clone().into()),
                ])),
            };
            http_peer::reply(&mut socket, &frame, result).await;
        }
    });
    let clock = clock(100);
    let publisher = ResetUiPublisher::new(
        &format!("http://{address}").as_str().into(),
        &"é\r\n".into(),
        &JsString(vec![101, 0xd800]),
        &JsString(vec![114, 0xdc00]),
        &clock,
    )
    .unwrap();
    let retained = publisher.mutation_id().clone();
    assert_eq!(
        publisher.republish(&bundles).await.unwrap_err().code,
        "outcome_unknown"
    );
    assert_eq!(
        trace.lock().unwrap().len(),
        1,
        "The wrapper must not retry a lost reply."
    );
    assert_eq!(
        publisher.republish(&bundles).await.unwrap(),
        vec![JsString::from("fixture")]
    );
    std::fs::remove_file(directory.join("index.html")).unwrap();
    assert_eq!(
        publisher.republish(&bundles).await.unwrap(),
        vec![JsString::from("fixture")]
    );
    server.await.unwrap();
    assert_eq!(clock.calls.load(Ordering::SeqCst), 1);
    assert_eq!(publisher.mutation_id(), &retained);
    let rows = trace.lock().unwrap().clone();
    assert_eq!(
        rows.iter()
            .map(|v| v.get("method").unwrap().as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "uis.inspect",
            "uis.inspect",
            "uis.stageAsset",
            "uis.deploy",
            "uis.inspect",
            "uis.inspect"
        ]
    );
    for row in &rows {
        assert_eq!(row.get("authorization"), Some(&"42656172657220e9".into()));
    }
    let mutation = ivy_contracts::operation::derive_operation_id_js(
        &retained,
        &J::Array(vec!["ui".into(), "fixture".into()]),
    )
    .unwrap();
    assert_eq!(
        rows[3].get("params").unwrap().get("mutationId"),
        Some(&J::String(mutation))
    );
    if let Some(directory) = std::env::var_os("IVY_TEST_RAW_EVIDENCE") {
        std::fs::write(
            std::path::Path::new(&directory).join("reset-ui-auth-native-attempts.json"),
            stringify(&J::Array(rows), usize::MAX).unwrap(),
        )
        .unwrap();
    }
    drop(fixture);
}
