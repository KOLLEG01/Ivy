use super::{ServiceFixture, assert_json_eq};
use ivy_contracts::{IvyJson, JsString};
use ivy_host_runtime::{
    default_settings, layout,
    node_path::{self, Style},
};
use std::{collections::BTreeMap, path::Path};

fn path(value: &Path) -> IvyJson {
    IvyJson::String(node_path::from_native(value))
}
fn empty() -> IvyJson {
    IvyJson::object([] as [(&str, IvyJson); 0])
}
fn instance(config: &IvyJson) -> &IvyJson {
    &config.required("instances").unwrap().as_array().unwrap()[0]
}
fn with_instance(config: &IvyJson, instance: IvyJson) -> IvyJson {
    let mut result = config.clone();
    result.insert("instances", IvyJson::Array(vec![instance]));
    result
}

#[test]
fn account_layout_defaults_and_resolution_preserve_declared_paths() {
    let fixture = ServiceFixture::new();
    let style = Style::native();
    let home_path = fixture.0.join("account");
    let home = node_path::from_native(&home_path);
    let os_home = node_path::from_native(&fixture.0.join("os-home"));
    let environment = BTreeMap::from([
        (JsString::from("HOME"), home.clone()),
        (JsString::from("USERPROFILE"), home.clone()),
    ]);
    assert_eq!(
        layout::account_home(&environment, style, &os_home).unwrap(),
        home
    );
    assert_eq!(
        layout::account_home(&BTreeMap::new(), style, &os_home).unwrap(),
        os_home
    );
    let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let empty_environment = BTreeMap::from([(home_key.into(), "".into())]);
    assert_eq!(
        layout::account_home(&empty_environment, style, &os_home).unwrap(),
        os_home
    );
    for relative in ["relative", ".", " "] {
        let environment = BTreeMap::from([(home_key.into(), relative.into())]);
        assert_eq!(
            layout::account_home(&environment, style, &os_home)
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
    }
    assert_eq!(
        layout::default_host_config_path(&environment, style, &os_home).unwrap(),
        node_path::from_native(&home_path.join(".ivy").join("config.json"))
    );
    #[cfg(windows)]
    {
        let mut opaque = home.clone();
        opaque.0.extend([b'\\' as u16, 0xd800]);
        let environment = BTreeMap::from([("USERPROFILE".into(), opaque.clone())]);
        assert_eq!(
            layout::account_home(&environment, style, &os_home).unwrap(),
            opaque
        );
        for relative in ["/", "\\user", "C:relative"] {
            let environment = BTreeMap::from([("USERPROFILE".into(), relative.into())]);
            assert_eq!(
                layout::account_home(&environment, style, &os_home)
                    .unwrap_err()
                    .code,
                "invalid_arguments"
            );
        }
    }

    let settings = IvyJson::object([("custom", true.into())]);
    let account = layout::agent_manager_account_settings(&settings, &home, style).unwrap();
    assert_eq!(account.get("custom"), Some(&true.into()));
    assert_eq!(
        account.get("projectRoot"),
        Some(&path(&home_path.join("projects")))
    );
    assert_eq!(
        account.get("internalProjectRoot"),
        Some(&path(&home_path.join(".ivy").join("codex-projects")))
    );
    assert_eq!(
        account.get("skillsRoot"),
        Some(&path(&home_path.join(".codex").join("skills")))
    );
    let mut explicit = settings.clone();
    explicit.insert("codexHome", path(&home_path.join("native-home")));
    assert_eq!(
        layout::agent_manager_account_settings(&explicit, &home, style)
            .unwrap()
            .get("skillsRoot"),
        Some(&path(&home_path.join("native-home").join("skills")))
    );
    let mut claude = settings.clone();
    claude.insert(
        "appServer",
        IvyJson::object([("mode", "claude-adapter".into())]),
    );
    assert!(
        layout::agent_manager_account_settings(&claude, &home, style)
            .unwrap()
            .get("skillsRoot")
            .is_none()
    );

    let source = node_path::from_native(&fixture.0.join("host.json"));
    let ivy_root = home_path.join(".ivy");
    assert_json_eq(
        &default_settings::default_service_settings_for_paths(
            "extension",
            &settings,
            &node_path::from_native(&ivy_root),
            &source,
            style,
        )
        .unwrap(),
        &settings,
    );
    let manager = IvyJson::object([
        ("instanceId", "renamed-manager".into()),
        ("componentId", "service-manager".into()),
        ("engine", "process".into()),
        ("settings", settings),
    ]);
    let input = IvyJson::object([("instances", IvyJson::Array(vec![manager.clone()]))]);
    let resolve = |input: &IvyJson| {
        layout::resolve_host_configuration(input, &source, &environment, style, &os_home)
    };
    let baseline = resolve(&input).unwrap();
    for (key, suffix) in [
        ("runtimeRoot", "runtime"),
        ("artifactRoot", "artifacts"),
        ("stagingRoot", "staging"),
        ("servicesRoot", "services"),
    ] {
        assert_eq!(baseline.get(key), Some(&path(&ivy_root.join(suffix))));
    }
    assert_eq!(baseline.get("ivyRoot"), Some(&path(&ivy_root)));
    assert_eq!(
        baseline.get("configPath"),
        Some(&IvyJson::String(source.clone()))
    );
    let default_process = IvyJson::object([("allowWindowsBreakaway", true.into())]);
    assert_eq!(
        instance(&baseline).get("process"),
        if cfg!(windows) {
            Some(&default_process)
        } else {
            None
        }
    );
    for process in [
        IvyJson::Null,
        empty(),
        IvyJson::object([("allowWindowsBreakaway", false.into())]),
        default_process.clone(),
    ] {
        let mut selected = manager.clone();
        selected.insert("process", process.clone());
        let mut expected = instance(&baseline).clone();
        expected.insert(
            "process",
            if cfg!(windows) && process.is_null() {
                default_process.clone()
            } else {
                process
            },
        );
        assert_json_eq(
            &resolve(&with_instance(&input, selected)).unwrap(),
            &with_instance(&baseline, expected),
        );
    }
    #[cfg(windows)]
    for process in [
        false.into(),
        true.into(),
        IvyJson::Number(0.0),
        IvyJson::Number(1.0),
        "".into(),
        "configured".into(),
        IvyJson::Array(vec![]),
        IvyJson::Array(vec![empty()]),
    ] {
        let mut selected = manager.clone();
        selected.insert("process", process);
        assert_eq!(
            resolve(&with_instance(&input, selected)).unwrap_err().code,
            "invalid_arguments"
        );
    }
    let mut docker = manager.clone();
    docker.insert("engine", "docker".into());
    assert!(
        instance(&resolve(&with_instance(&input, docker)).unwrap())
            .get("process")
            .is_none()
    );
    let mut other = manager.clone();
    other.insert("componentId", "hive".into());
    assert!(
        instance(&resolve(&with_instance(&input, other)).unwrap())
            .get("process")
            .is_none()
    );

    let paths =
        layout::service_paths_for_style(&baseline, &"renamed-manager".into(), style).unwrap();
    for key in ["data", "work", "logs"] {
        assert_eq!(
            paths.get(key),
            Some(&path(
                &ivy_root.join("services").join("service-manager").join(key)
            ))
        );
    }
    assert_eq!(
        layout::service_paths_for_style(&baseline, &"missing".into(), style)
            .unwrap_err()
            .code,
        "not_found"
    );
    let mut legacy = input.clone();
    let legacy_root = fixture.0.join("legacy");
    legacy.insert("runtimeRoot", path(&legacy_root));
    let legacy = resolve(&legacy).unwrap();
    assert_eq!(legacy.get("runtimeRoot"), Some(&path(&legacy_root)));
    assert_eq!(
        layout::service_paths_for_style(&legacy, &manager, style)
            .unwrap()
            .get("data"),
        Some(&path(
            &legacy_root
                .join("instances")
                .join("renamed-manager")
                .join("data")
        ))
    );
    assert_eq!(
        layout::host_configuration_path_for_style(&legacy, style).unwrap(),
        path(&legacy_root.join("host.json"))
    );
    assert_eq!(
        layout::host_configuration_path_for_style(&baseline, style).unwrap(),
        IvyJson::String(source)
    );
}
