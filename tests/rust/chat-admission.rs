use ivy_chat_bridge::admission::ChatAdmission;
use ivy_contracts::{IvyJson as Json, IvyResult, json::parse};
use std::process::Command;
#[path = "chat-operations-cases.rs"]
mod operations;

#[test]
fn admission_order_limits_and_original_request_hashes_match_typescript() {
    let output = Command::new("node")
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .arg("tests/rust/fixtures/chat-admission-oracle.mjs")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    for (index, row) in rows.as_array().unwrap().iter().enumerate() {
        let run = || -> IvyResult<Json> {
            let definition = row.required("definition")?;
            let admission = ChatAdmission::new(definition)?;
            let caller = row.required("caller")?.as_js_string().unwrap();
            let request = row.required("request")?;
            match row.required("kind")?.as_str().as_deref().unwrap() {
                "send" => {
                    let result = admission.send(caller, request)?;
                    Ok(Json::object([
                        ("identity", result.identity),
                        ("requestHash", result.request_hash.into()),
                    ]))
                }
                "authorize" => {
                    admission.authorize(
                        caller,
                        request.required("expectedBridge")?,
                        request.get("channel"),
                    )?;
                    Ok(Json::Null)
                }
                _ => {
                    let mut copy = admission.channels_for(caller);
                    copy = Json::Array(vec![copy]);
                    assert_ne!(copy, admission.channels_for(caller));
                    assert_eq!(admission.definition(), definition);
                    Ok(Json::object([
                        ("definitionHash", admission.definition_hash().into()),
                        ("expected", admission.expected(caller)),
                        ("channels", admission.channels_for(caller)),
                    ]))
                }
            }
        };
        let actual = match run() {
            Ok(value) => Json::object([("value", value)]),
            Err(e) => Json::object([(
                "error",
                Json::object([
                    ("code", e.code.into()),
                    ("message", e.message.into()),
                    ("outcome", e.outcome.to_json()),
                ]),
            )]),
        };
        let request = row.required("request").unwrap();
        if row.get("kind") == Some(&Json::from("send"))
            && (request.as_object().is_none() || request.get("expectedBridge").is_none())
        {
            let error = actual.required("error").unwrap();
            assert_eq!(
                error.get("code"),
                Some(&"invalid_arguments".into()),
                "row {index}"
            );
            assert_eq!(error.get("outcome"), Some(&"not_executed".into()));
        } else {
            assert_eq!(&actual, row.get("result").unwrap(), "oracle row {index}");
        }
    }
    assert!(rows.as_array().unwrap().len() > 50);
}
