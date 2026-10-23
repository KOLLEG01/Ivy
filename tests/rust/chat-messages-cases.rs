use ivy_chat_bridge::whatsapp::messages::Translator;
use ivy_contracts::{IvyJson as Json, JsString, json::parse};

#[test]
fn all_catalog_messages_remain_available_in_each_language() {
    let catalog = parse(
        include_str!("../../services/chat-bridge/rust/src/whatsapp/messages.json"),
        usize::MAX,
    )
    .unwrap();
    for language in [
        None,
        Some(JsString::from("en")),
        Some("de".into()),
        Some("fr".into()),
    ] {
        let translator = Translator::new(language.as_ref());
        for (key, _) in catalog.required("english").unwrap().as_object().unwrap() {
            assert!(
                !translator
                    .translate(&key.to_utf8().unwrap(), None)
                    .unwrap()
                    .to_utf8()
                    .unwrap()
                    .is_empty()
            );
        }
    }
    assert_eq!(
        Translator::default()
            .translate("command.invalid", None)
            .unwrap(),
        "Invalid command.".into()
    );
    assert_eq!(
        Translator::new(Some(&"de".into()))
            .translate("command.invalid", None)
            .unwrap(),
        "Ungültiger Befehl.".into()
    );
}

#[test]
fn message_parameters_use_text_and_numbers_without_replacement_expansion() {
    let translator = Translator::default();
    let text = "$& $$ {code} 世界 😀";
    let value = translator
        .translate(
            "listener.failed",
            Some(&Json::object([("reason", text.into())])),
        )
        .unwrap();
    assert!(value.to_utf8().unwrap().contains(text));
    assert!(
        translator
            .translate(
                "listener.failed",
                Some(&Json::object([("reason", Json::Bool(true))]))
            )
            .is_err()
    );
    assert!(translator.translate("missing-key", None).is_err());
}
