use ivy_chat_bridge::{
    main_context::{
        load_main_instructions, main_input_origin, main_thread_options, main_turn_context,
    },
    native_plan::chat_turn_options,
};
use ivy_contracts::{IvyJson as Json, json::parse};

fn json(text: &str) -> Json {
    parse(text, usize::MAX).unwrap()
}

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ivy-chat-context-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(path.join("instructions")).unwrap();
        Self(path)
    }
    fn asset(&self) -> std::path::PathBuf {
        self.0.join("instructions/chat-main-instructions.json")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn main_thread_restrictions_preserve_settings_and_caller_templates() {
    let template = json(
        r#"{"model":"selected","developerInstructions":"Keep this","config":{"retained":true,"mcp_servers.ivy.disabled_tools":["existing","existing","another"]}}"#,
    );
    let before = template.clone();
    let value = main_thread_options(&template, &"Main instructions 世界".into()).unwrap();
    assert_eq!(
        value,
        json(
            r#"{"model":"selected","developerInstructions":"Keep this\n\nMain instructions 世界","config":{"retained":true,"mcp_servers.ivy.disabled_tools":["existing","another","chat_bridge_send"]}}"#
        )
    );
    assert_eq!(template, before);
    let default = main_thread_options(&json("{}"), &"Main".into()).unwrap();
    assert_eq!(default.get("developerInstructions"), Some(&"Main".into()));
    assert_eq!(
        default
            .required("config")
            .unwrap()
            .get("mcp_servers.ivy.disabled_tools"),
        Some(&json(r#"["chat_bridge_send"]"#))
    );
    for invalid in [
        json("null"),
        json("[]"),
        json(r#"{"developerInstructions":4}"#),
        json(r#"{"config":{"mcp_servers.ivy.disabled_tools":[{}]}}"#),
    ] {
        assert!(main_thread_options(&invalid, &"Main".into()).is_err());
    }
}

#[test]
fn main_context_retains_other_contexts_and_formats_notice_origins() {
    let request = json(r#"{"action":"notify","source":{"id":"source","revision":1}}"#);
    let origin = main_input_origin(&"caller".into(), &request).unwrap();
    assert_eq!(
        origin,
        "service notification from caller; source {\"id\":\"source\",\"revision\":1}".into()
    );
    assert_eq!(
        main_input_origin(&"caller".into(), &json(r#"{"action":"input"}"#)).unwrap(),
        "conversation input".into()
    );
    assert!(main_input_origin(&"caller".into(), &json("null")).is_err());
    let template = json(r#"{"additionalContext":{"other":{"kind":"text","value":"retain"}}}"#);
    let before = template.clone();
    let context = main_turn_context(&template, &origin, &"Main 😀".into()).unwrap();
    assert_eq!(
        context.required("additionalContext").unwrap().get("other"),
        template.required("additionalContext").unwrap().get("other")
    );
    assert_eq!(
        context
            .required("additionalContext")
            .unwrap()
            .required("ivy-main")
            .unwrap()
            .get("value"),
        Some(&format!("Main 😀\n\nInput origin: {}", origin.to_utf8().unwrap()).into())
    );
    assert_eq!(template, before);
}

#[test]
fn collaboration_preferences_preserve_precedence_and_templates() {
    let template = json(
        r#"{"model":"top","collaborationMode":{"mode":"plan","settings":{"model":"retained","reasoning_effort":"high","other":true}}}"#,
    );
    let before = template.clone();
    let value = chat_turn_options(
        &template,
        Some(&json(r#"{"model":"chosen","effort":"medium"}"#)),
    )
    .unwrap();
    assert_eq!(value.get("model"), Some(&"chosen".into()));
    assert_eq!(
        value
            .required("collaborationMode")
            .unwrap()
            .required("settings")
            .unwrap(),
        &json(r#"{"model":"chosen","reasoning_effort":"medium","other":true}"#)
    );
    assert_eq!(chat_turn_options(&template, None).unwrap(), template);
    let null = chat_turn_options(&template, Some(&json(r#"{"model":null}"#))).unwrap();
    assert_eq!(
        null.required("collaborationMode").unwrap(),
        template.required("collaborationMode").unwrap()
    );
    assert_eq!(template, before);
}

#[test]
fn instruction_asset_reads_unicode_and_refuses_missing_or_invalid_text() {
    let path = Fixture::new();
    assert!(load_main_instructions(&path.0).is_err());
    for bytes in [
        b"not json".as_slice(),
        b"{}",
        b"null",
        b"[1]",
        b"true",
        &[0xff],
        b"\xef\xbb\xbf\"BOM\"",
        b"\"\\ud800\"",
    ] {
        std::fs::write(path.asset(), bytes).unwrap();
        assert!(load_main_instructions(&path.0).is_err());
    }
    std::fs::write(path.asset(), "\"line\\n世界 😀\"\n").unwrap();
    assert_eq!(
        load_main_instructions(&path.0).unwrap(),
        "line\n世界 😀".into()
    );
}
