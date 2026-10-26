use serde_json::Value;
#[path = "../fixtures/reference-data.rs"]
mod reference_data;

pub fn auth_reference(subject: &str, input: &Value) -> Value {
    static CASES: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    let cases = CASES.get_or_init(|| {
        serde_json::from_slice(&reference_data::read(include_bytes!(
            "../fixtures/hive-auth-reference.json.gz"
        )))
        .unwrap()
    });
    let key = ivy_contracts::hash::digest(format!("{subject}\n{input}"));
    cases["traces"]
        .get(&key)
        .unwrap_or_else(|| panic!("Missing original Hive {subject} trace: {key}"))
        .clone()
}
