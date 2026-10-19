use super::{Fixed, Peer, Probe, equal, field, flag, text};
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_chat_bridge::{
    admission::ChatAdmission,
    generation::ChatGenerationConfig,
    notice_input::NoticeInstructions,
    runtime::{ChatService, ChatTransport},
};
use ivy_contracts::{
    IvyJson as Json,
    checked_native_contract::CheckedNativeContracts,
    json::{canonical, parse},
};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};
fn command(action: &str) -> Json {
    Json::object([("action", action.into())])
}
fn array(value: Vec<Json>) -> Json {
    Json::Array(value)
}
fn oracle() -> Json {
    let out = Command::new("node")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .args(["tests/rust/fixtures/chat-service-active-peer.mjs", "oracle"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    if let Some(p) = std::env::var_os("IVY_CHAT_ACTIVE_RUNTIME_ORACLE_CAPTURE") {
        std::fs::write(p, &out.stdout).unwrap();
    }
    parse(std::str::from_utf8(&out.stdout).unwrap(), usize::MAX).unwrap()
}
fn generation(row: &Json, corpus: &Json, root: PathBuf) -> Arc<ChatGenerationConfig> {
    Arc::new(ChatGenerationConfig {
        admission: ChatAdmission::new(field(field(row, "seed"), "definition")).unwrap(),
        catalogs: CheckedNativeContracts::new(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
        ),
        instructions: parse(
            &String::from_utf8(STANDARD.decode(text(corpus, "instructions")).unwrap()).unwrap(),
            usize::MAX,
        )
        .unwrap()
        .into_js_string()
        .unwrap(),
        notice_instructions: NoticeInstructions::from_json(
            &parse(
                &String::from_utf8(STANDARD.decode(text(corpus, "noticeInstructions")).unwrap())
                    .unwrap(),
                usize::MAX,
            )
            .unwrap(),
        )
        .unwrap(),
        service_node_id: "chat-provider".into(),
        data_root: Some(root),
        language: "en".into(),
    })
}
async fn invoke(peer: &mut Peer, name: &str, args: &Json) -> Json {
    peer.command(Json::object([
        ("action", "invoke".into()),
        ("name", name.into()),
        ("arguments", args.clone()),
    ]))
    .await
}
async fn state_until(peer: &mut Peer, predicate: impl Fn(&Json) -> bool, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let value = peer.command(command("state")).await;
        if predicate(&value) {
            return;
        }
        if Instant::now() >= deadline {
            let failure = peer.command(command("failure")).await;
            if let Some(path) = std::env::var_os("IVY_CHAT_ACTIVE_RUNTIME_FAILURE_CAPTURE") {
                std::fs::write(path, canonical(&failure, usize::MAX).unwrap()).unwrap();
            }
            panic!(
                "{label}; readiness observations: {}",
                canonical(field(field(&failure, "report"), "heartbeats"), usize::MAX).unwrap()
            );
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
fn compress_health(values: &Json) -> Json {
    let mut out = vec![];
    for v in values.as_array().unwrap() {
        if out.last() != Some(v) {
            out.push(v.clone());
        }
    }
    array(out)
}
fn significant(events: &Json) -> Json {
    array(
        events
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| {
                e.as_js_string().is_some_and(|s| s != &"control".into())
                    || e.get("created").is_some()
            })
            .cloned()
            .collect(),
    )
}
fn domain(snapshot: &Json) -> Json {
    let mut v = snapshot.clone();
    v.remove("transport");
    v
}
fn mutations(snapshot: &Json) -> Json {
    array(
        field(snapshot, "transport")
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| {
                v.get("method") == Some(&Json::from("objects.write"))
                    || v.get("method") == Some(&Json::from("tools.call"))
                        && v.get("params").and_then(|p| p.get("qualifiedName"))
                            == Some(&Json::from("agent.invoke"))
            })
            .cloned()
            .collect(),
    )
}
#[tokio::test]
async fn real_service_resumes_original_queue_lanes_and_wakes_only_the_current_turn() {
    let corpus = oracle();
    let mut peer = Peer::fixture("tests/rust/fixtures/chat-service-active-peer.mjs").await;
    let mut captures = vec![];
    for row in field(&corpus, "cases").as_array().unwrap() {
        let label = text(row, "label");
        let kind = text(row, "kind");
        eprintln!("Active service case: {label}");
        let begun = peer
            .command(Json::object([
                ("action", "begin".into()),
                ("spec", row.clone()),
            ]))
            .await;
        let root = PathBuf::from(text(&begun, "root"));
        assert!(
            root.is_absolute()
                && root
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("ivy-main-original-")
        );
        eprintln!("Active service SQLite fixture: {}", root.display());
        let config = Json::object([
            ("componentId", "chat-bridge".into()),
            ("credential", "fixture-token".into()),
            ("serviceNodeId", "chat-provider".into()),
            ("hostId", "fixture-host".into()),
            ("version", "1.0.0".into()),
            ("buildId", format!("sha256:{}", "a".repeat(64)).into()),
            ("publicBaseUrl", peer.url.clone().into()),
            (
                "settings",
                Json::object([
                    (
                        "definition",
                        field(field(row, "seed"), "definition").clone(),
                    ),
                    ("pollMs", Json::Number(60000.0)),
                    ("whatsappConfigPath", "synthetic-account-lifecycle".into()),
                ]),
            ),
        ]);
        let probe = Probe::new(label.clone());
        let transport: Arc<dyn ChatTransport> = probe.clone();
        let app = ChatService::with_clock(
            &config,
            generation(row, &corpus, root),
            probe.health(),
            Some(transport),
            Arc::new(Fixed(
                field(&corpus, "now").as_f64().unwrap() as i64,
                Instant::now(),
            )),
        )
        .await
        .unwrap();
        app.service().wait_ready(None).await.unwrap();
        if kind == "deliver-prepared" || kind.starts_with("wake-") {
            state_until(
                &mut peer,
                |s| {
                    field(s, "reads").as_f64().unwrap() > 0.0
                        && !field(s, "collections").as_array().unwrap().is_empty()
                },
                "autonomous delivered input never entered its collection checkpoint",
            )
            .await;
        }
        peer.command(command("quiet")).await;
        let mut outputs = vec![];
        if kind.starts_with("wake-") {
            peer.command(command("arm")).await;
            outputs.push(invoke(&mut peer, "send", field(row, "sendRequest")).await);
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let report = peer.command(command("summary")).await;
                if flag(&report, "held") {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "active collector read never began"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            if kind != "wake-none" {
                peer.command(Json::object([
                    ("action", "notify".into()),
                    (
                        "method",
                        if kind == "wake-other-method" {
                            "turn/started"
                        } else {
                            "turn/completed"
                        }
                        .into(),
                    ),
                    (
                        "turnId",
                        if kind == "wake-foreign" {
                            "foreign-turn"
                        } else {
                            "delivery-turn-1"
                        }
                        .into(),
                    ),
                ]))
                .await;
            }
            // Socket order makes this read-only invocation a notification barrier.
            let send = field(row, "sendRequest");
            outputs.push(
                invoke(
                    &mut peer,
                    "operation",
                    &Json::object([
                        ("operationId", field(send, "operationId").clone()),
                        ("expectedBridge", field(send, "expectedBridge").clone()),
                    ]),
                )
                .await,
            );
            peer.command(command("release")).await;
            let report = peer.command(command("quiet")).await;
            equal(
                field(&report, "readsAfterArm"),
                field(row, "wakeReads"),
                "wakePending must observe the selected turn during an active drain",
            );
        } else if kind == "create" {
            let request = field(row, "request");
            outputs.push(invoke(&mut peer, "createMain", request).await);
            outputs.push(invoke(&mut peer, "createMain", request).await);
            let mut changed = request.clone();
            changed.insert(
                "reason",
                "A different request cannot reuse this operation.".into(),
            );
            outputs.push(invoke(&mut peer, "createMain", &changed).await);
            peer.command(command("quiet")).await;
        }
        if kind == "terminal" || kind.starts_with("cancel-") {
            state_until(
                &mut peer,
                |s| {
                    field(field(s, "main"), "queue")
                        .as_array()
                        .unwrap()
                        .is_empty()
                },
                "original queue ticket never settled",
            )
            .await;
        }
        if kind == "deliver-prepared" {
            state_until(
                &mut peer,
                |s| {
                    field(s, "inputs")
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|i| i.get("turnId").is_some_and(|t| t.as_js_string().is_some()))
                },
                "native prepared delivery never started",
            )
            .await;
        }
        app.close().await.unwrap();
        assert!(!app.service().ready());
        assert_eq!(probe.closes.load(Ordering::SeqCst), 1);
        let actual = peer
            .command(Json::object([
                ("action", "end".into()),
                ("output", array(outputs)),
            ]))
            .await;
        let health = array(probe.health.lock().unwrap().clone());
        let events = array(probe.events.lock().unwrap().clone());
        captures.push(Json::object([
            ("label", label.clone().into()),
            ("actual", actual.clone()),
            ("health", health.clone()),
            ("events", events.clone()),
        ]));
        if let Some(p) = std::env::var_os("IVY_CHAT_ACTIVE_RUNTIME_ACTUAL_CAPTURE") {
            std::fs::write(p, canonical(&array(captures.clone()), usize::MAX).unwrap()).unwrap();
        }
        let expected = field(row, "final");
        equal(
            field(field(&actual, "report"), "bad"),
            &array(vec![]),
            "caught domain errors must not hide a fixture failure",
        );
        equal(
            &domain(field(&actual, "snapshot")),
            &domain(field(expected, "snapshot")),
            &format!("{label}: named local UUIDs only; exact domain snapshot"),
        );
        equal(
            &mutations(field(&actual, "snapshot")),
            &mutations(field(expected, "snapshot")),
            &format!("{label}: original durable claims and exact ordered mutation frames"),
        );
        equal(
            field(field(&actual, "report"), "handshakes"),
            field(field(expected, "report"), "handshakes"),
            "exact registry/identity/filter",
        );
        assert!(field(field(&actual,"report"),"heartbeats").as_array().unwrap().iter().all(|h|flag(h,"ready")&&field(h,"diagnostics").as_array().unwrap().is_empty()),"unexpected native worker diagnostic");
        equal(
            &compress_health(&health),
            &compress_health(field(row, "health")),
            "health order ignoring consecutive identical heartbeat writes",
        );
        equal(
            &significant(&events),
            &significant(field(row, "events")),
            "transport posthooks and notification forwarding",
        );
    }
    peer.finish().await;
    eprintln!(
        "Active service: {} retained-checkpoint cases, actual autonomous worker/provider/notification sockets; normalized named UUIDs and ordered mutation/claim comparisons. No Host/account/installed/platform acceptance.",
        captures.len()
    );
}
