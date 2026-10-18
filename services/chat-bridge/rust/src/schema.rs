use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, chat_bundle::bundled_schema, chat_schema::validate_chat,
    json::parse,
};
use std::sync::OnceLock;

pub const VERSION: &str = "1.3.0";
pub const READABLE_VERSIONS: [&str; 2] = ["1.2.0", VERSION];
pub const CONTRACT_NAMES: [(&str, &str); 12] = [
    ("chat-bridge/conversation", "Conversation"),
    ("chat-bridge/main", "Main"),
    ("chat-bridge/binding", "Binding"),
    ("chat-bridge/native-call", "NativeCall"),
    ("chat-bridge/input", "Input"),
    ("chat-bridge/evidence", "Evidence"),
    ("chat-bridge/collection", "Collection"),
    ("chat-bridge/result", "Result"),
    ("chat-bridge/reply", "Reply"),
    ("chat-bridge/offer", "Offer"),
    ("chat-bridge/acknowledgement", "Acknowledgement"),
    ("chat-bridge/operation", "Operation"),
];
pub fn local_contract(key: &str) -> bool {
    matches!(
        key,
        "chat-bridge/main"
            | "chat-bridge/binding"
            | "chat-bridge/native-call"
            | "chat-bridge/evidence"
            | "chat-bridge/collection"
            | "chat-bridge/offer"
            | "chat-bridge/acknowledgement"
            | "chat-bridge/operation"
    )
}
pub(crate) fn final_contract(key: &str) -> bool {
    matches!(
        key,
        "chat-bridge/conversation"
            | "chat-bridge/input"
            | "chat-bridge/result"
            | "chat-bridge/reply"
    )
}
pub fn validate_domain(key: &str, value: &Json) -> IvyResult<()> {
    let name = CONTRACT_NAMES
        .iter()
        .find(|(k, _)| *k == key)
        .ok_or_else(IvyError::internal)?
        .1;
    validate_chat(name, value)
}
pub(crate) fn catalog() -> &'static Json {
    static CATALOG: OnceLock<Json> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let text = include_str!("../../../../specs/schemas/chat.operations.json");
        parse(text, text.len()).expect("checked-in Chat operations catalog")
    })
}
pub fn chat_schema(name: &str) -> IvyResult<Json> {
    bundled_schema(&format!("#/$defs/{name}").into(), None)
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
        if !final_contract(&key) {
            continue;
        }
        result.push(Json::object([
            ("key", key.as_str().into()), ("version", VERSION.into()),
            ("owner", Json::object([("kind", "service".into()), ("serviceName", "chat-bridge".into())])),
            ("mediaType", "application/json".into()),
            ("retention", retention(if key == "chat-bridge/conversation" { "retain" } else { "expire" })),
            ("jsonSchema", bundled_schema(item.required("definition")?.as_js_string().unwrap(), catalog().get("schema").and_then(Json::as_js_string))?),
            ("specMarkdown", format!("# {key} {VERSION}\n\nChatBridge owns this final user-visible conversation record. Conversation identity is retained; completed turn records expire after 90 days. Original caller, input, binding and result identities remain mandatory across configuration changes.").into()),
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
