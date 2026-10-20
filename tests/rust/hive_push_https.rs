use super::peer::*;
use ivy_contracts::IvyJson;
use ivy_hive::browser_push::{SendOutcome, Sender};
use ivy_hive::browser_push_native::NativeSender;
use serde_json::{Value, json};
use std::{io::Write, sync::Arc, time::Duration};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_https_sender_uses_native_system_clock_expiration() {
    let mut peer = Oracle::start().await;
    let sender = NativeSender::with_numeric_expiration(
        Arc::new(peer.transport(true)),
        ivy_hive::browser_push_native::system_expiration(),
    );
    let before = peer.call(json!({"op":"expiration"})).await;
    let (status, millis) = send(&sender, &peer.profile, "/status/201", None).await;
    let after = peer.call(json!({"op":"expiration"})).await;
    let observed = peer.call(json!({"op":"stats"})).await;
    save(
        &json!({"systemClock":true,"before":before,"after":after,"native":observed,"status":status,"millis":millis}),
    );
    assert_eq!(status, 201);
    let requests = observed["requests"].as_array().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["signatureValid"], true);
    assert_eq!(requests[0]["plaintext"], "notification");
    let expiration = requests[0]["claims"]["exp"].as_i64().unwrap();
    assert!(expiration >= before["expiration"].as_i64().unwrap());
    assert!(expiration <= after["expiration"].as_i64().unwrap());
    drop(sender);
    peer.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_https_sender_preserves_status_headers_body_end_reuse_and_trust() {
    let mut peer = Oracle::start().await;
    let sender = peer.sender(true);
    let mut original_connection = None;
    let mut native_connection = None;
    for path in [
        "/status/200?q=%2F",
        "/status/201",
        "/status/204",
        "/status/299",
        "/status/300",
        "/status/404",
        "/status/410",
        "/status/429",
        "/status/500",
        "/redirect",
        "/compressed",
        "/delay-body",
    ] {
        let original = peer.call(json!({"op":"original","path":path})).await;
        let before = peer.call(json!({"op":"stats"})).await["requests"]
            .as_array()
            .unwrap()
            .len();
        let (status, millis) = send(&sender, &peer.profile, path, None).await;
        let all = peer.call(json!({"op":"stats"})).await;
        let requests = &all["requests"].as_array().unwrap()[before..];
        let record = json!({"path":path,"original":original,"native":{"status":status,"millis":millis,"requests":requests}});
        save(&record);
        assert_eq!(json!(status), original["status"], "{record}");
        assert_eq!(requests.len(), 1, "no redirect or retry: {record}");
        let old = &original["requests"][0];
        let new = &requests[0];
        for request in [old, new] {
            assert_eq!(request["signatureValid"], true);
            assert_eq!(request["plaintext"], "notification");
        }
        assert_eq!(old["method"], new["method"]);
        assert_eq!(old["path"], new["path"]);
        assert_eq!(semantic_headers(old), semantic_headers(new), "{record}");
        if path == "/status/200?q=%2F" {
            original_connection = Some(old["connection"].clone());
            native_connection = Some(new["connection"].clone());
        }
        if path == "/status/201" {
            assert_eq!(original_connection.as_ref(), Some(&old["connection"]));
            assert_eq!(native_connection.as_ref(), Some(&new["connection"]));
        }
        if path == "/delay-body" {
            assert!(millis >= 500.0 && original["millis"].as_f64().unwrap() >= 500.0);
        }
    }
    for (trusted, host) in [(false, None), (true, Some("127.0.0.1"))] {
        let original=peer.call(json!({"op":"original","path":"/status/201","trusted":trusted,"host":if host.is_some(){"ip"}else{"dns"}})).await;
        let (status, millis) =
            send(&peer.sender(trusted), &peer.profile, "/status/201", host).await;
        save(
            &json!({"trusted":trusted,"host":host,"original":original,"native":{"status":status,"millis":millis}}),
        );
        assert_eq!(status, 0);
        assert_eq!(original["status"], 0);
    }
    drop(sender);
    peer.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_https_timeout_tracks_socket_progress_and_observer_drop_does_not_cancel() {
    let mut peer = Oracle::start().await;
    let sender = peer.sender(true);
    // Each original and native request is checked against the same isolated peer.
    for path in [
        "/drop",
        "/stall-headers",
        "/stall-body",
        "/trickle-headers",
        "/trickle-body",
    ] {
        let original = peer.call(json!({"op":"original","path":path})).await;
        let (status, millis) = send(&sender, &peer.profile, path, None).await;
        save(&json!({"path":path,"original":original,"native":{"status":status,"millis":millis}}));
        assert_eq!(json!(status), original["status"], "path={path}");
        if path.starts_with("/stall") {
            assert!((9000.0..15000.0).contains(&millis));
            assert!((9000.0..15000.0).contains(&original["millis"].as_f64().unwrap()));
        }
        if path.starts_with("/trickle") {
            assert!((11000.0..18000.0).contains(&millis));
            assert!((11000.0..18000.0).contains(&original["millis"].as_f64().unwrap()));
        }
    }
    let before = peer.call(json!({"op":"stats"})).await;
    let mut sub = peer.profile["subscription"].clone();
    sub["endpoint"] = json!(format!(
        "{}/delay-body",
        peer.profile["base"].as_str().unwrap()
    ));
    let observer = sender.send(
        IvyJson::from_serde(&sub).unwrap(),
        "notification".into(),
        options(&peer.profile),
    );
    drop(observer);
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let after = peer.call(json!({"op":"stats"})).await;
    save(&json!({"abandonedObserver":true,"before":before,"after":after}));
    assert_eq!(
        after["requests"].as_array().unwrap().len(),
        before["requests"].as_array().unwrap().len() + 1
    );
    assert_eq!(
        after["completions"].as_array().unwrap().len(),
        before["completions"].as_array().unwrap().len() + 1
    );
    drop(sender);
    peer.close().await;
}
