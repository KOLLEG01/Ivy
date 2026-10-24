//! Published service definitions shared by service registration and offline reset.
use crate::{IvyError, IvyJson as Json, IvyResult, chat_bundle::bundled_schema, json::parse};
use std::sync::OnceLock;
pub const CHAT_VERSION: &str = "1.3.0";
fn catalog() -> &'static Json {
    static CATALOG: OnceLock<Json> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let text = include_str!("../../../../specs/schemas/chat.operations.json");
        parse(text, text.len()).expect("checked-in Chat operations catalog")
    })
}
pub fn chat_final_contract(key: &str) -> bool {
    matches!(
        key,
        "chat-bridge/conversation"
            | "chat-bridge/input"
            | "chat-bridge/result"
            | "chat-bridge/reply"
    )
}
pub fn task_board_contracts() -> IvyResult<Vec<Json>> {
    let text = include_str!("../../../../services/task-board/rust/src/contracts.json");
    parse(text, text.len())?
        .into_array()
        .ok_or_else(IvyError::internal)
}
fn retention(mode: &str) -> Json {
    let mut objects = Json::object([("mode", mode.into())]);
    if mode == "expire" {
        objects.insert("maximumAgeDays", Json::Number(90.0));
    }
    Json::object([
        ("objects", objects),
        ("revisions", Json::object([("mode", "current".into())])),
    ])
}
pub fn chat_contracts() -> IvyResult<Vec<Json>> {
    let mut result = Vec::new();
    for item in catalog().required("contracts")?.as_array().unwrap() {
        let key = item.required("key")?.as_str()?;
        if !chat_final_contract(&key) {
            continue;
        }
        result.push(Json::object([
            ("key", key.as_str().into()), ("version", CHAT_VERSION.into()),
            ("owner", Json::object([("kind", "service".into()), ("serviceName", "chat-bridge".into())])),
            ("mediaType", "application/json".into()),
            ("retention", retention(if key == "chat-bridge/conversation" { "retain" } else { "expire" })),
            ("jsonSchema", bundled_schema(item.required("definition")?.as_js_string().unwrap(), catalog().get("schema").and_then(Json::as_js_string))?),
            ("specMarkdown", format!("# {key} {CHAT_VERSION}\n\nChatBridge owns this final user-visible conversation record. Conversation identity is retained; completed turn records expire after 90 days. Original caller, input, binding and result identities remain mandatory across configuration changes.").into()),
        ]));
    }
    for item in catalog().required("mediaContracts")?.as_array().unwrap() {
        if item.get("key") == Some(&"chat-bridge/evidence-chunk".into()) {
            continue;
        }
        let mut contract = item.clone();
        contract.insert("retention", retention("owned"));
        result.push(contract);
    }
    Ok(result)
}
