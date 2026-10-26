use super::construction_reference;
use ivy_contracts::{IvyJson, JsString, json::stringify};
use ivy_hive::{
    server_options::{self, Error},
    store::Credential,
};

fn strings(value: &IvyJson) -> Vec<JsString> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_js_string().unwrap().clone())
        .collect()
}
#[test]
fn original_constructor_proxy_publisher_url_policy_and_error_order() {
    let original = construction_reference("serverOptions");
    let rows = original.get("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 72);
    for row in rows {
        let input = row.get("input").unwrap();
        let credentials = input
            .get("credentials")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|v| Credential {
                principal_id: v
                    .get("principalId")
                    .unwrap()
                    .as_js_string()
                    .unwrap()
                    .clone(),
                digest: v.get("digest").unwrap().as_js_string().unwrap().clone(),
            })
            .collect::<Vec<_>>();
        let actual = match server_options::validate(
            input.get("publicBaseUrl").unwrap().as_js_string().unwrap(),
            &strings(input.get("trustedProxyAddresses").unwrap()),
            &strings(input.get("packagePublisherPrincipalIds").unwrap()),
            &credentials,
        ) {
            Ok(base) => IvyJson::object([(
                "value",
                IvyJson::object([
                    ("href", IvyJson::String(base.href)),
                    ("origin", IvyJson::String(base.origin)),
                    ("basePath", IvyJson::String(base.base_path)),
                ]),
            )]),
            Err(Error::Ivy(error)) => IvyJson::object([("error", error.to_wire())]),
            Err(Error::UrlConstructor { input }) => {
                IvyJson::object([("urlConstructorInput", IvyJson::String(input))])
            }
        };
        let expected = row.get("expected").unwrap();
        assert!(
            &actual == expected,
            "Input: {}\nNative: {}\nOriginal: {}",
            stringify(input, usize::MAX).unwrap(),
            stringify(&actual, usize::MAX).unwrap(),
            stringify(expected, usize::MAX).unwrap()
        );
    }
}
