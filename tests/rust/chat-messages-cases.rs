use ivy_chat_bridge::whatsapp::messages::Translator;
use ivy_contracts::{IvyJson as Json, JsString, json::parse};

#[test]
fn all_original_messages_and_interpolation_match_with_isolated_languages() {
    let output = std::process::Command::new("node")
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .arg("tests/rust/fixtures/chat-whatsapp-messages-oracle.mjs")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let corpus = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    let keys = corpus.required("keys").unwrap().as_f64().unwrap() as usize;
    assert!(keys > 0);
    assert_eq!(rows.len(), keys * 28);
    for row in rows {
        let language = row.get("language").and_then(Json::as_js_string);
        let translator = Translator::new(language);
        let key = row.required("key").unwrap().as_str().unwrap();
        assert_eq!(
            Json::String(translator.translate(&key, row.get("parameters")).unwrap()),
            *row.required("expected").unwrap(),
            "{} {:?}",
            key,
            language
        );
    }
    let english = Translator::default();
    let german = Translator::new(Some(&JsString::from("de")));
    assert_eq!(
        german.translate("command.invalid", None).unwrap(),
        JsString::from("Ungültiger Befehl.")
    );
    assert_eq!(
        english.translate("command.invalid", None).unwrap(),
        JsString::from("Invalid command.")
    );
    assert_eq!(
        german.translate("command.invalid", None).unwrap(),
        JsString::from("Ungültiger Befehl.")
    );
}
