use ivy_contracts::{
    IvyJson, JsString,
    json::{parse, stringify},
};
use ivy_hive::{
    server_options::{self, Error},
    store::Credential,
};
use std::{fs, path::Path, process::Command};

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
    let output = Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()))
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .arg("tests/fixtures/hive-server-options-oracle.mjs")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let original = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    let rows = original.get("rows").unwrap().as_array().unwrap();
    assert!(rows.len() > 60);
    let mut observed = vec![];
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
        observed.push(IvyJson::object([
            ("original", row.clone()),
            ("native", actual.clone()),
        ]));
        if let Some(path) = std::env::var_os("IVY_HIVE_CONSTRUCTOR_CORPUS") {
            fs::write(
                path,
                stringify(&IvyJson::Array(observed.clone()), usize::MAX).unwrap(),
            )
            .unwrap();
        }
        assert_eq!(
            &actual,
            row.get("expected").unwrap(),
            "{}",
            stringify(input, usize::MAX).unwrap()
        );
    }
}
