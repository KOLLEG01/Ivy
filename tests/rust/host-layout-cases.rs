use ivy_contracts::{IvyJson, JsString, json};
use ivy_host_runtime::{layout, default_settings, node_path::Style};
use std::{collections::BTreeMap, path::PathBuf, process::Command};

#[test]
fn account_layout_defaults_and_resolution_match_original_typescript() {
    let output = Command::new("node")
        .args(["--experimental-transform-types", "--disable-warning=ExperimentalWarning", "tests/fixtures/host-layout-oracle.mjs"])
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let corpus = json::parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    let cases = corpus.as_array().unwrap();
    assert!(cases.len() > 600);
    for case in cases {
        let style = if case.required("style").unwrap().as_str().unwrap() == "win32" { Style::Windows } else { Style::Posix };
        let args = case.required("args").unwrap().as_array().unwrap();
        let string = |value: &IvyJson| value.as_js_string().unwrap().clone();
        let environment = |value: &IvyJson| -> BTreeMap<JsString, JsString> { value.as_object().unwrap().iter().map(|(key, value)| (key.clone(), string(value))).collect() };
        let os_home = string(case.required("osHome").unwrap());
        let actual = match case.required("operation").unwrap().as_str().unwrap().as_str() {
            "accountHome" => layout::account_home(&environment(&args[0]), style, &os_home).map(IvyJson::String),
            "defaultHostConfigPath" => layout::default_host_config_path(&environment(&args[0]), style, &os_home).map(IvyJson::String),
            "agentManagerAccountSettings" => layout::agent_manager_account_settings(&args[0], &string(&args[1]), style),
            "resolveHostConfiguration" => layout::resolve_host_configuration(&args[0], &string(&args[1]), &environment(&args[2]), style, &os_home),
            "servicePaths" => layout::service_paths_for_style(&args[0], &args[1], style),
            "hostConfigurationPath" => layout::host_configuration_path_for_style(&args[0], style),
            "defaultServiceSettings" => default_settings::default_service_settings_for_paths(&args[0].required("componentId").unwrap().as_str().unwrap(), args[0].required("settings").unwrap(), &string(args[1].required("ivyRoot").unwrap()), &string(args[1].required("configPath").unwrap()), style),
            _ => panic!("Unknown layout operation"),
        };
        let actual = match actual {
            Ok(value) => IvyJson::object([("ok", true.into()), ("value", value)]),
            Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
        };
        super::assert_json_eq_with_context(&actual, case.required("expected").unwrap(), case);
    }
}
