//! Shared envelope for a service-owned browser notification event.
use crate::{IvyJson, JsString, json::parse};
use std::sync::OnceLock;

pub fn browser_notice_schema() -> &'static IvyJson {
    static SCHEMA: OnceLock<IvyJson> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        parse(
            include_str!("../../../../specs/schemas/browser-notice.schema.json"),
            65_536,
        )
        .expect("checked-in browser notice schema")
    })
}
pub fn browser_notice_topic(namespace: &JsString) -> IvyJson {
    let mut topic = namespace.clone();
    topic.0.extend(".browser-notification".encode_utf16());
    IvyJson::object([
        ("topic", IvyJson::String(topic)),
        ("version", "1.0.0".into()),
        ("title", "Browser notification".into()),
        (
            "description",
            "A user-visible notice for opted-in Ivy browsers.".into(),
        ),
        ("payloadSchema", browser_notice_schema().clone()),
        ("eventKinds", IvyJson::Array(vec![])),
    ])
}
