use ivy_contracts::IvyJson as J;
use ivy_host_runtime::{
    codex_home::*,
    native_path::{self, Context},
};
use std::collections::BTreeMap;

#[test]
fn raw_home_settings_preserve_presence_null_selection_and_validation() {
    let paths = Context::current().unwrap();
    let environment = BTreeMap::new();
    let context = HomeContext {
        paths: &paths,
        environment: &environment,
        user_home: None,
        cwd: &paths.cwd,
        account_home: &paths.cwd,
    };
    let selected = native_path::from_native(&native_path::to_native(&paths.cwd).join("selected"));
    for (field, source) in [
        ("codexHome", "host-configuration"),
        ("nativeHome", "existing-native-home"),
    ] {
        let settings = J::object([(field, J::String(selected.clone()))]);
        let result = resolve_codex_home_json(&settings, &context).unwrap();
        assert_eq!((result.path, result.source), (selected.clone(), source));
    }
    let explicit_null = J::object([
        ("nativeHome", J::String(selected.clone())),
        ("codexHome", J::Null),
    ]);
    assert_eq!(
        resolve_codex_home_json(&explicit_null, &context)
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    for value in [
        J::Null,
        false.into(),
        J::Number(1.0),
        J::Array(vec![]),
        J::object([] as [(&str, J); 0]),
    ] {
        let settings = J::object([("nativeHome", value)]);
        assert_eq!(
            resolve_codex_home_json(&settings, &context)
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
    }
    for value in ["", "relative", " \t "] {
        assert_eq!(
            resolve_codex_home_json(&J::object([("codexHome", value.into())]), &context)
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
    }
    assert_eq!(
        resolve_codex_home_json(&J::Null, &context)
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
}

#[test]
fn raw_external_reference_is_verbatim_and_ownership_never_comes_from_environment() {
    let paths = Context::current().unwrap();
    let environment = BTreeMap::from([("CODEX_HOME".into(), paths.cwd.clone())]);
    let context = HomeContext {
        paths: &paths,
        environment: &environment,
        user_home: None,
        cwd: &paths.cwd,
        account_home: &paths.cwd,
    };
    let empty = J::object([] as [(&str, J); 0]);
    assert_eq!(
        instance_owned_codex_home_data_json(&empty, None, &context).unwrap(),
        None
    );
    for value in [
        J::Null,
        false.into(),
        J::Number(3.0),
        "unchanged reference".into(),
    ] {
        let settings = J::object([(
            "appServer",
            J::object([
                ("mode", "external-proxy".into()),
                ("expectedCodexHome", value.clone()),
            ]),
        )]);
        assert!(uses_external_app_server_json(&settings).unwrap());
        assert_eq!(
            expected_server_home_json(&settings, &context).unwrap(),
            value
        );
    }
    for mode in ["owned-stdio", "claude-adapter"] {
        let settings = J::object([("appServer", J::object([("mode", mode.into())]))]);
        assert!(!uses_external_app_server_json(&settings).unwrap());
    }
    let settings = J::object([("codexHome", J::String(paths.cwd.clone()))]);
    assert_eq!(
        instance_owned_codex_home_data_json(&settings, None, &context)
            .unwrap_err()
            .code,
        "internal_error"
    );
}
