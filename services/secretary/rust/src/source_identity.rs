//! Catalogue account evidence retains the observed email exactly, without normalization.
use ivy_contracts::{IvyError, IvyJson, IvyResult, hash::hash_json};

pub fn catalogue_account_hash(value: &IvyJson) -> IvyResult<String> {
    let invalid = || {
        IvyError::new(
            "secretary_account_response_invalid",
            "Expected an account response.",
        )
    };
    if value.as_object().is_none() {
        return Err(invalid());
    }
    let account = value
        .get("account")
        .filter(|v| v.as_object().is_some())
        .ok_or_else(invalid)?;
    let email = account.get("email").and_then(IvyJson::as_js_string);
    if account.get("type") != Some(&IvyJson::from("chatgpt"))
        || !email.is_some_and(|s| !s.0.is_empty())
    {
        return Err(IvyError::new(
            "secretary_account_mismatch",
            "The observed account differs from the configured account.",
        ));
    }
    hash_json(&IvyJson::object([
        ("type", "chatgpt".into()),
        ("email", IvyJson::String(email.unwrap().clone())),
    ]))
}
