use super::*;
use ivy_cli::{publish_ui::read_ui_bundle, reset_ui::ResetUiPublisher};
use ivy_sdk::clock::Clock;
use std::{
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
        &"fixture-token".into(),
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
        assert_eq!(
            row.get("authorization"),
            Some(&"42656172657220666978747572652d746f6b656e".into())
        );
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
