//! Preserve the frozen JSON traces at the fixture boundary, outside storage.
use ivy_contracts::{IvyJson, JsString};
use ivy_hive::{storage::Payload, store::{AuthContext, Credential}};
use std::sync::Arc;

pub fn json(payload: Payload) -> IvyJson {
    match payload {
        Payload::Json(value) => value,
        Payload::Execute { context, request } => IvyJson::object([
            ("action", "execute".into()), ("context", context), ("request", request),
        ]),
        Payload::ConfigureCredentials(values) => IvyJson::object([
            ("action", "credentials.configure".into()),
            ("credentials", IvyJson::Array(values.iter().map(|value| IvyJson::object([
                ("principalId", value.principal_id.clone().into()), ("digest", value.digest.clone().into()),
            ])).collect())),
        ]),
        Payload::CreateSession(value) => IvyJson::object([
            ("action", "session.create".into()), ("credentialDigest", value.into()),
        ]),
        Payload::ResolveSession(value) => IvyJson::object([
            ("action", "session.resolve".into()), ("sessionDigest", value.into()),
        ]),
        Payload::Authenticate(context) => auth("authenticate", context),
        Payload::EndSession(context) => auth("session.end", context),
    }
}

fn auth(action: &str, context: AuthContext) -> IvyJson {
    let mut value = IvyJson::object([("credentialDigest", context.credential_digest.into())]);
    if let Some(session) = context.session_digest { value.insert("sessionDigest", session.into()); }
    IvyJson::object([("action", action.into()), ("context", value)])
}

pub fn native(mut value: IvyJson) -> Payload {
    let action = value.get("action").and_then(IvyJson::as_js_string);
    if action == Some(&JsString::from("authenticate")) || action == Some(&JsString::from("session.end")) {
        if let Some(context) = value.get("context") {
            if let Some(credential) = context.get("credentialDigest").and_then(IvyJson::as_js_string) {
                let context = AuthContext {
                    credential_digest: credential.clone(),
                    session_digest: context.get("sessionDigest").and_then(IvyJson::as_js_string).cloned(),
                };
                return if action == Some(&JsString::from("authenticate")) {
                    Payload::Authenticate(context)
                } else { Payload::EndSession(context) };
            }
        }
    }
    if action == Some(&JsString::from("session.create")) {
        if let Some(credential) = value.get("credentialDigest").and_then(IvyJson::as_js_string) {
            return Payload::CreateSession(credential.clone());
        }
    }
    if action == Some(&JsString::from("session.resolve")) {
        if let Some(session) = value.get("sessionDigest").and_then(IvyJson::as_js_string) {
            return Payload::ResolveSession(session.clone());
        }
    }
    if action == Some(&JsString::from("execute"))
        && value.get("context").is_some() && value.get("request").is_some() {
        return Payload::Execute {
            context: value.remove("context").unwrap(), request: value.remove("request").unwrap(),
        };
    }
    if action == Some(&JsString::from("credentials.configure")) {
        let credentials = value.get("credentials").and_then(IvyJson::as_array).and_then(|values|
            values.iter().map(|value| Some(Credential {
                principal_id: value.get("principalId")?.as_js_string()?.clone(),
                digest: value.get("digest")?.as_js_string()?.clone(),
            })).collect::<Option<Vec<_>>>());
        if let Some(credentials) = credentials {
            return Payload::ConfigureCredentials(Arc::new(credentials));
        }
    }
    Payload::Json(value)
}

/// Compare the command's actual fields, excluding context the worker never used.
/// Frozen results, response order and side effects remain unchanged.
pub fn project_auth_requests(trace: &mut serde_json::Value) {
    use ivy_contracts::json::{parse, stringify};
    use serde_json::Value;
    match trace {
        Value::Array(values) => values.iter_mut().for_each(project_auth_requests),
        Value::Object(value) => {
            if value.get("kind").and_then(Value::as_str) == Some("worker") {
                if let Some(Value::String(encoded)) = value.get_mut("encoded") {
                    let payload = parse(encoded, usize::MAX).unwrap();
                    *encoded = stringify(&json(native(payload)), usize::MAX).unwrap();
                }
                if let Some(payload) = value.get_mut("payload") {
                    *payload = json(native(IvyJson::from_serde(payload).unwrap())).to_serde().unwrap();
                }
            }
            value.values_mut().for_each(project_auth_requests);
        }
        _ => {}
    }
}
