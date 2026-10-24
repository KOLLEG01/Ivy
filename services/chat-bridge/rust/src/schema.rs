use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, chat_bundle::bundled_schema, chat_schema::validate_chat,
    json::parse,
};
use std::sync::OnceLock;

pub use ivy_contracts::published_contracts::CHAT_VERSION as VERSION;
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
pub(crate) use ivy_contracts::published_contracts::chat_final_contract as final_contract;
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
pub use ivy_contracts::published_contracts::chat_contracts;
