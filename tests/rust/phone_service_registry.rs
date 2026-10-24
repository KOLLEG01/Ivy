use super::*;
use ivy_contracts::json;
#[test]
fn public_descriptors_reachable_schemas_and_validation_bounds() {
    let mut expected = json::parse(include_str!("../fixtures/phone-protocol.json"), 65536).unwrap();
    let mut namespace = expected.get("namespaces").unwrap().as_array().unwrap()[0].clone();
    let tools = namespace
        .get("tools")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| {
            let mut tool = tool.clone();
            for key in ["inputSchema", "outputSchema"] {
                let name = tool.get(key).unwrap().as_str().unwrap().to_owned();
                tool.insert(key, phone_service_schema(&name).unwrap());
            }
            tool
        })
        .collect::<Vec<_>>();
    assert_eq!(tools.len(), 25);
    namespace.insert("tools", IvyJson::Array(tools));
    expected.insert("namespaces", IvyJson::Array(vec![namespace]));
    let contracts=expected.required("contracts").unwrap().as_array().unwrap().iter().map(|contract| {let mut value=contract.clone();value.insert("jsonSchema",phone_service_schema(&contract.required("jsonSchema").unwrap().as_str().unwrap()).unwrap());value}).collect();
    expected.insert("contracts",IvyJson::Array(contracts));
    assert_eq!(
        json::canonical(&phone_registry().unwrap(), 8 * 1024 * 1024).unwrap(),
        json::canonical(&expected, 8 * 1024 * 1024).unwrap()
    );
    assert_eq!(
        phone_service_schema("missing").unwrap_err().code,
        "internal_error"
    );
    assert_eq!(
        validate_phone_service(
            "PhoneCredentials",
            &IvyJson::object([
                ("username", "x".repeat(256 * 1024).into()),
                ("password", IvyJson::Null),
            ]),
        )
        .unwrap_err()
        .code,
        "content_too_large"
    );
    let valid = IvyJson::object([
        ("operationId", "11234567-89ab-cdef-0123-456789abcdef".into()),
        ("recipientId", "recipient".into()),
        ("initialPrompt", "Please explain why I am calling.".into()),
    ]);
    validate_phone_service("PhoneVoiceCall", &valid).unwrap();
    for change in [None, Some(" \t\n ")] {
        let mut invalid = valid.clone();
        invalid.remove("initialPrompt");
        if let Some(prompt) = change {
            invalid.insert("initialPrompt", prompt.into());
        }
        assert_eq!(
            validate_phone_service("PhoneVoiceCall", &invalid)
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
    }
    let mut direct = valid;
    direct.remove("recipientId");
    direct.insert("destination", "+49123456789".into());
    assert!(validate_phone_service("PhoneVoiceCall", &direct).is_err());
}
