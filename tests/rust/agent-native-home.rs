use ivy_contracts::JsString;
use ivy_host_runtime::{
    codex_home::*,
    native_path::{self, Context},
};
use std::collections::BTreeMap;

#[test]
fn native_home_selection_preserves_precedence_and_strict_instance_ownership() {
    let paths = Context::current().unwrap();
    let data = std::env::temp_dir().join(format!("ivy-agent-home-{}", uuid::Uuid::new_v4()));
    let root = native_path::from_native(&data);
    let explicit = native_path::from_native(&data.join("native"));
    let inherited = native_path::from_native(&data.join("external"));
    let environment = BTreeMap::from([("CODEX_HOME".into(), inherited.clone())]);
    let context = HomeContext {
        paths: &paths,
        environment: &environment,
        user_home: None,
        cwd: &paths.cwd,
        account_home: &paths.cwd,
    };
    for codex in [false, true] {
        let settings = HomeSettings {
            codex_home: codex.then(|| explicit.clone()),
            native_home: Some(explicit.clone()),
        };
        let selected = resolve_codex_home(&settings, &context).unwrap();
        assert_eq!(selected.path, explicit);
        assert_eq!(
            selected.source,
            if codex {
                "host-configuration"
            } else {
                "existing-native-home"
            }
        );
        assert_eq!(
            instance_owned_codex_home(&settings, &root, &context).unwrap(),
            Some(explicit.clone())
        );
        assert_eq!(
            instance_owned_codex_home(&settings, &explicit, &context).unwrap(),
            None
        );
        assert_eq!(
            instance_owned_codex_home(&settings, &inherited, &context).unwrap(),
            None
        );
    }
    let conflicting = HomeSettings {
        codex_home: Some(explicit),
        native_home: Some(inherited.clone()),
    };
    assert_eq!(
        resolve_codex_home(&conflicting, &context).unwrap_err().code,
        "invalid_arguments"
    );
    let settings = HomeSettings::default();
    let selected = resolve_codex_home(&settings, &context).unwrap();
    assert_eq!((selected.path, selected.source), (inherited, "environment"));
    assert_eq!(
        instance_owned_codex_home(&settings, &root, &context).unwrap(),
        None
    );
    assert!(
        !data.exists(),
        "Resolving a home never creates it or grants environmental ownership."
    );
}

#[test]
fn native_home_external_references_and_absolute_account_guards_are_preserved() {
    let paths = Context::current().unwrap();
    let environment = BTreeMap::new();
    let relative = JsString::from("relative-account");
    let context = HomeContext {
        paths: &paths,
        environment: &environment,
        user_home: None,
        cwd: &paths.cwd,
        account_home: &relative,
    };
    let settings = HomeSettings::default();
    assert_eq!(
        resolve_codex_home(&settings, &context).unwrap_err().code,
        "invalid_arguments"
    );
    let reference = JsString(vec![65, 0xd800, 66]);
    let mode = AppServerMode::ExternalProxy {
        expected_codex_home: Some(reference.clone()),
    };
    assert!(uses_external_app_server(&mode));
    assert_eq!(
        expected_server_home(&settings, &mode, &context).unwrap(),
        reference
    );
    for codex_home in ["", "relative", " \t "] {
        let settings = HomeSettings {
            codex_home: Some(codex_home.into()),
            native_home: None,
        };
        assert_eq!(
            resolve_codex_home(&settings, &context).unwrap_err().code,
            "invalid_arguments"
        );
    }
}
