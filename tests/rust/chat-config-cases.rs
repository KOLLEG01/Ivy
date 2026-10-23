use ivy_chat_bridge::whatsapp::config::read_whatsapp_config;
use ivy_contracts::{
    IvyJson as Json, JsString,
    json::{parse, stringify},
};

fn json(text: &str) -> Json {
    parse(text, usize::MAX).unwrap()
}
struct Fixture(std::path::PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[tokio::test]
async fn protected_configuration_validates_native_paths_and_exact_sender_policy() {
    let fixture = Fixture(
        std::env::temp_dir().join(format!("ivy-chat-config-{}.json", uuid::Uuid::new_v4())),
    );
    let path: JsString = fixture.0.to_str().unwrap().into();
    let base = json(
        r#"{"accountId":"account","botPhoneNumber":"+15555550200","senders":[{"phoneNumber":"0015555550201","principalId":"principal","channelId":"channel"}],"since":"2024-01-01T00:00:00Z"}"#,
    );
    let definition = json(
        r#"{"channels":[{"channel":{"adapter":"whatsapp","accountId":"account","channelId":"channel"}}]}"#,
    );
    std::fs::write(&fixture.0, stringify(&base, usize::MAX).unwrap()).unwrap();
    assert_eq!(
        read_whatsapp_config(&path, &definition).await.unwrap(),
        base
    );
    for heartbeat in [10_000.0, 120_000.0] {
        let mut value = base.clone();
        value.insert("heartbeatMs", Json::Number(heartbeat));
        std::fs::write(&fixture.0, stringify(&value, usize::MAX).unwrap()).unwrap();
        assert_eq!(
            read_whatsapp_config(&path, &definition).await.unwrap(),
            value
        );
    }
    for (key, invalid) in [
        ("accountId", Json::Bool(true)),
        ("botPhoneNumber", Json::Number(15_555_550_200.0)),
        ("senders", json("[]")),
        ("senders", json("[null]")),
        (
            "senders",
            json(r#"[{"phoneNumber":"+15555550201","principalId":4,"channelId":"channel"}]"#),
        ),
        ("since", "not a date".into()),
        ("heartbeatMs", Json::Number(9_999.0)),
        ("heartbeatMs", Json::Number(10_000.5)),
        ("heartbeatMs", "10000".into()),
    ] {
        let mut value = base.clone();
        value.insert(key, invalid);
        std::fs::write(&fixture.0, stringify(&value, usize::MAX).unwrap()).unwrap();
        assert_eq!(
            read_whatsapp_config(&path, &definition)
                .await
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
    }
    std::fs::write(&fixture.0, stringify(&base, usize::MAX).unwrap()).unwrap();
    assert_eq!(
        read_whatsapp_config(&path, &json(r#"{"channels":[]}"#))
            .await
            .unwrap_err()
            .code,
        "chat_sender_refused"
    );
    assert_eq!(
        read_whatsapp_config(&"relative.json".into(), &definition)
            .await
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    std::fs::write(&fixture.0, b"not json").unwrap();
    assert!(read_whatsapp_config(&path, &definition).await.is_err());
}
