use super::*;

#[tokio::test]
async fn configured_main_resume_failure_prevents_transport_attachment_and_readiness() {
    use ivy_chat_bridge::chat_main::ConfiguredMainResume;
    let seed = read_oracle("startup-seed");
    let root = std::env::temp_dir().join(format!(
        "ivy-chat-resume-admission-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(
        root.join("chat-bridge.sqlite"),
        STANDARD.decode(text(&seed, "database")).unwrap(),
    )
    .unwrap();
    let resume = Json::object([
        ("operationId", "chat:1790000000123:selected-resume".into()),
        (
            "main",
            Json::object([
                ("objectId", "missing-main".into()),
                ("revision", 1.0.into()),
            ]),
        ),
        (
            "binding",
            Json::object([
                ("objectId", "missing-binding".into()),
                ("revision", 1.0.into()),
            ]),
        ),
    ]);
    let generation = Arc::new(ChatGenerationConfig {
        admission: ChatAdmission::new(field(&seed, "definition")).unwrap(),
        catalogs: CheckedNativeContracts::new(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
        ),
        instructions: parse(
            std::str::from_utf8(&STANDARD.decode(text(&seed, "instructions")).unwrap()).unwrap(),
            usize::MAX,
        )
        .unwrap()
        .into_js_string()
        .unwrap(),
        notice_instructions: NoticeInstructions::from_json(
            &parse(
                std::str::from_utf8(&STANDARD.decode(text(&seed, "noticeInstructions")).unwrap())
                    .unwrap(),
                usize::MAX,
            )
            .unwrap(),
        )
        .unwrap(),
        service_node_id: "chat-provider".into(),
        data_root: Some(root.clone()),
        language: "en".into(),
        diagnostic_notifications: false,
        resume_main: Some(ConfiguredMainResume::from_json(&resume).unwrap()),
    });
    let mut peer = Peer::start().await;
    peer.command(Json::object([
        ("action", "begin".into()),
        ("spec", Json::object([("label", "resume-refused".into())])),
    ]))
    .await;
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
                ("pollMs", 60000.0.into()),
                ("definition", field(&seed, "definition").clone()),
                ("resumeMain", resume),
                ("whatsappConfigPath", "synthetic-account".into()),
            ]),
        ),
    ]);
    let probe = Probe::new("resume-refused".to_owned());
    let app = ChatService::with_clock(
        &config,
        generation,
        probe.health(),
        Some(probe.clone()),
        Arc::new(Fixed(1790000000123, Instant::now())),
    )
    .await
    .unwrap();
    until(
        || {
            probe
                .events
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e.get("attach") == Some(&Json::Null))
                .count()
                >= 2
        },
        "Failed Main reconciliation closes its generation",
    )
    .await;
    assert!(!app.service().ready());
    assert!(app.runtime().is_none());
    assert!(
        probe
            .events
            .lock()
            .unwrap()
            .iter()
            .all(|e| e.get("attach").is_none_or(Json::is_null)),
        "Ingress must not attach before the configured Main is settled"
    );
    assert!(
        probe
            .health
            .lock()
            .unwrap()
            .iter()
            .all(|h| !flag(h, "ready"))
    );
    let report = peer
        .command(Json::object([("action", "summary".into())]))
        .await;
    assert!(
        field(&report, "calls")
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c.get("method") == Some(&"system.status".into())),
        "The test must reach actual generation reconciliation"
    );
    app.close().await.unwrap();
    peer.finish().await;
    std::fs::remove_dir_all(root).unwrap();
}
