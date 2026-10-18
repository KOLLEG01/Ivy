use ivy_agent_manager::environment_text::{
    adopt_matching_permissions, has_app_section, has_legacy_sandbox_table, root_configuration_keys,
    server_sections,
};
use ivy_contracts::{IvyJson, json::parse};
use std::{path::PathBuf, process::Command};
fn string(value: &IvyJson) -> String {
    value.as_js_string().unwrap().to_utf8().unwrap()
}
fn array(values: Vec<String>) -> IvyJson {
    IvyJson::Array(values.into_iter().map(IvyJson::from).collect())
}
#[test]
fn unmanaged_sections_keys_and_permission_adoption_match_original() {
    let output = Command::new("node")
        .arg("tests/fixtures/agent-environment-conflicts-oracle.mjs")
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        2 * 1024 * 1024,
    )
    .unwrap();
    let rows = data.get("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 120);
    for (i, row) in rows.iter().enumerate() {
        let source = string(row.get("source").unwrap());
        assert_eq!(
            &array(server_sections(&source)),
            row.get("servers").unwrap(),
            "Servers {i}"
        );
        assert_eq!(
            &array(root_configuration_keys(&source)),
            row.get("keys").unwrap(),
            "Keys {i}"
        );
        assert_eq!(
            IvyJson::Bool(has_legacy_sandbox_table(&source)),
            *row.get("legacy").unwrap(),
            "Legacy {i}"
        );
        let actual = match adopt_matching_permissions(&source) {
            Ok(value) => IvyJson::object([("result", value.into())]),
            Err(error) => IvyJson::object([("error", error.to_wire())]),
        };
        assert_eq!(&actual, row.get("adopted").unwrap(), "Adoption {i}");
    }
    let apps = data.get("apps").unwrap().as_array().unwrap();
    assert_eq!(apps.len(), 68);
    for (i, row) in apps.iter().enumerate() {
        assert_eq!(
            IvyJson::Bool(has_app_section(
                &string(row.get("source").unwrap()),
                &string(row.get("appId").unwrap())
            )),
            *row.get("expected").unwrap(),
            "App {i}"
        );
    }
}
