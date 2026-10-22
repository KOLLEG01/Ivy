use ivy_contracts::{
    IvyJson as J, JsString,
    hash::digest,
    json::{parse, stringify},
};
use ivy_sdk::{
    IvyError,
    rpc::{ClientOptions, HiveClient, RequestOptions, RpcClient},
};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::{io::AsyncWriteExt, net::TcpListener};
#[path = "../fixtures/rpc-http-peer.rs"]
#[allow(dead_code)]
mod http_peer;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}
fn original() -> J {
    original_mode("response")
}
fn original_mode(mode: &str) -> J {
    let oracle = if mode == "response" {
        "IVY_TEST_FETCH_RESPONSE_ORACLE"
    } else {
        "IVY_TEST_FETCH_INFORMATIONAL_ORACLE"
    };
    let bytes = if let Some(path) = std::env::var_os(oracle) {
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(
            digest(&bytes),
            std::env::var(format!("{oracle}_HASH")).unwrap()
        );
        bytes
    } else {
        let output = std::process::Command::new("node")
            .args([
                "--experimental-transform-types",
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/sdk-fetch-response-oracle.mjs",
                mode,
            ])
            .current_dir(root())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    let result = parse(std::str::from_utf8(&bytes).unwrap(), usize::MAX).unwrap();
    if mode != "response" {
        assert_eq!(result.get("mode"), Some(&mode.into()));
    }
    assert_eq!(
        result.get("sourceSha256"),
        Some(
            &digest(std::fs::read(root().join("packages/sdk/src/client.ts")).unwrap())
                .trim_start_matches("sha256:")
                .into()
        )
    );
    if let Some(dir) = std::env::var_os("IVY_TEST_RAW_EVIDENCE") {
        std::fs::write(
            Path::new(&dir).join(format!("sdk-fetch-{mode}-original.json")),
            bytes,
        )
        .unwrap();
    }
    result
}

#[tokio::test]
async fn global_fetch_informational_and_upgrade_semantics_match_original() {
    let original = original_mode("informational");
    let peer = Peer::new(rows(&original).to_vec()).await;
    let mut evidence = Vec::new();
    let mut mismatches = Vec::new();
    for (index, row) in rows(&original).iter().enumerate() {
        let before = peer.traces.lock().unwrap().len();
        let client = HiveClient::new_js(
            &format!("{}/case/{index}", peer.base).as_str().into(),
            ClientOptions {
                credential: Some(JsString::from("fixture")),
                ..ClientOptions::default()
            },
        )
        .unwrap();
        let actual = peer.call(client, row).await;
        let requests = peer.traces.lock().unwrap()[before..].to_vec();
        assert_eq!(requests.len(), 1, "No redirect or replay: {}", name(row));
        assert_eq!(
            requests[0].get("path"),
            row.get("requests").unwrap().as_array().unwrap()[0].get("path")
        );
        let qualified = row.get("spec").unwrap().get("diagnostic") != Some(&true.into());
        if qualified && Some(&actual) != row.get("result") {
            mismatches.push(name(row));
        }
        evidence.push(J::object([
            ("name", name(row).into()),
            ("qualified", qualified.into()),
            ("expected", row.get("result").unwrap().clone()),
            ("actual", actual),
            ("requests", J::Array(requests)),
        ]));
    }
    // Upgrade rejection also applies to this entry point without a credential.
    let row = rows(&original)
        .iter()
        .find(|row| name(row) == "upgrade-headers")
        .unwrap();
    let client = HiveClient::new_js(&peer.base.as_str().into(), ClientOptions::default()).unwrap();
    let actual = peer.call(client, row).await;
    if Some(&actual) != row.get("result") {
        mismatches.push("without-credential:upgrade-headers".into());
    }
    evidence.push(J::object([
        ("name", "without-credential:upgrade-headers".into()),
        ("qualified", true.into()),
        ("expected", row.get("result").unwrap().clone()),
        ("actual", actual),
    ]));
    raw_evidence("sdk-fetch-informational-native.json", J::Array(evidence));
    assert!(
        mismatches.is_empty(),
        "Informational-profile mismatches: {mismatches:?}"
    );
}
fn rows(original: &J) -> &[J] {
    original.get("rows").unwrap().as_array().unwrap()
}
fn name(row: &J) -> String {
    row.get("spec")
        .unwrap()
        .get("name")
        .unwrap()
        .as_str()
        .unwrap()
}
fn hex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}
struct Peer {
    base: String,
    traces: Arc<Mutex<Vec<J>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Peer {
    async fn new(cases: Vec<J>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let traces = Arc::new(Mutex::new(Vec::new()));
        let observed = traces.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let (frame, trace) = http_peer::receive(&mut socket).await;
                observed.lock().unwrap().push(trace);
                let wanted = frame.get("params").unwrap().get("name").unwrap();
                let row = cases
                    .iter()
                    .find(|row| row.get("spec").unwrap().get("name") == Some(wanted))
                    .unwrap();
                let mut wire = hex(&row.get("wireTemplateHex").unwrap().as_str().unwrap());
                let offset = row.get("idOffset").unwrap().as_f64().unwrap() as i64;
                if offset >= 0 {
                    let id = frame.get("id").unwrap().as_str().unwrap();
                    assert_eq!(id.len(), 36);
                    wire[offset as usize..offset as usize + 36].copy_from_slice(id.as_bytes());
                }
                // A refused/null-body response may already have closed its socket.
                let _ = socket.write_all(&wire).await;
                let _ = socket.shutdown().await;
            }
        });
        Self { base, traces, task }
    }
    async fn call(&self, client: HiveClient, row: &J) -> J {
        match client
            .request(
                "fixture.echo",
                J::object([("name", name(row).into())]),
                RequestOptions::default(),
            )
            .await
        {
            Ok(value) => J::object([("ok", true.into()), ("value", value)]),
            Err(error) => J::object([("ok", false.into()), ("error", error.to_wire())]),
        }
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn raw_evidence(filename: &str, value: J) {
    if let Some(directory) = std::env::var_os("IVY_TEST_RAW_EVIDENCE") {
        std::fs::write(
            Path::new(&directory).join(filename),
            stringify(&value, usize::MAX).unwrap(),
        )
        .unwrap();
    }
}

#[tokio::test]
async fn global_fetch_response_status_and_body_semantics_match_original() {
    let original = original();
    let peer = Peer::new(rows(&original).to_vec()).await;
    let mut evidence = Vec::new();
    let mut mismatches = Vec::new();
    for (index, row) in rows(&original).iter().enumerate() {
        let before = peer.traces.lock().unwrap().len();
        let client = HiveClient::new_js(
            &format!("{}/case/{index}", peer.base).as_str().into(),
            ClientOptions {
                credential: Some(JsString::from("fixture")),
                ..ClientOptions::default()
            },
        )
        .unwrap();
        let actual = peer.call(client, row).await;
        let requests = peer.traces.lock().unwrap()[before..].to_vec();
        assert_eq!(requests.len(), 1, "No redirect or replay: {}", name(row));
        assert_eq!(
            requests[0].get("path"),
            row.get("requests").unwrap().as_array().unwrap()[0].get("path")
        );
        // Unsolicited100 admission belongs to the pending connection profile.
        // Retain that observation without treating it as qualified parity.
        let qualified = name(row) != "interim-100";
        if qualified && Some(&actual) != row.get("result") {
            mismatches.push(name(row));
        }
        evidence.push(J::object([
            ("name", name(row).into()),
            ("qualified", qualified.into()),
            ("expected", row.get("result").unwrap().clone()),
            ("actual", actual),
            ("requests", J::Array(requests)),
        ]));
    }
    // The profile belongs to the entry point, including when no credential exists.
    for wanted in ["status-300", "status-204", "status-205", "status-304"] {
        let row = rows(&original)
            .iter()
            .find(|row| name(row) == wanted)
            .unwrap();
        let client =
            HiveClient::new_js(&peer.base.as_str().into(), ClientOptions::default()).unwrap();
        let actual = peer.call(client, row).await;
        if Some(&actual) != row.get("result") {
            mismatches.push(format!("without-credential:{wanted}"));
        }
        evidence.push(J::object([
            (
                "name",
                format!("without-credential:{wanted}").as_str().into(),
            ),
            ("qualified", true.into()),
            ("expected", row.get("result").unwrap().clone()),
            ("actual", actual),
        ]));
    }
    raw_evidence("sdk-fetch-response-native.json", J::Array(evidence));
    assert!(
        mismatches.is_empty(),
        "Response-profile mismatches: {mismatches:?}"
    );
}

#[tokio::test]
async fn existing_string_client_response_profile_is_preserved() {
    let original = original();
    let peer = Peer::new(rows(&original).to_vec()).await;
    let mut evidence = Vec::new();
    for (wanted,error) in [
        ("status-300",IvyError::new("outcome_unknown","The request connection failed or timed out. Reconcile its original operation identity.").with_outcome(ivy_contracts::Outcome::Unknown)),
        ("status-304",IvyError::new("outcome_unknown","The request connection failed or timed out. Reconcile its original operation identity.").with_outcome(ivy_contracts::Outcome::Unknown)),
        ("status-204",IvyError::new("invalid_frame","Hive returned malformed JSON.").with_outcome(ivy_contracts::Outcome::Unknown)),
        ("upgrade-101",IvyError::new("invalid_frame","Hive returned malformed JSON.").with_outcome(ivy_contracts::Outcome::Unknown)),
    ] {
        let row=rows(&original).iter().find(|row|name(row)==wanted).unwrap();
        let client=HiveClient::new(&peer.base,ClientOptions::default()).unwrap();
        let actual=peer.call(client,row).await;
        assert_eq!(actual,J::object([("ok",false.into()),("error",error.to_wire())]));
        evidence.push(J::object([("name",wanted.into()),("actual",actual)]));
    }
    raw_evidence("sdk-fetch-response-legacy.json", J::Array(evidence));
}
