use super::{ServiceFixture, assert_json_eq};
use ivy_contracts::{IvyJson, JsString};
use ivy_host_runtime::{
    codex_home::{self, HomeContext},
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
fn native_codex_home_settings_preserve_selection_and_ownership() {
    let fixture = ServiceFixture::new();
    let paths = node_path::Context::current().unwrap();
    let account = node_path::from_native(&fixture.0.join("account"));
    let inherited = node_path::from_native(&fixture.0.join("inherited"));
    let environment = BTreeMap::from([("CODEX_HOME".into(), inherited.clone())]);
    let context = HomeContext {
        paths: &paths,
        environment: &environment,
        user_home: None,
        cwd: &paths.cwd,
        account_home: &account,
    };
    let data_path = fixture.0.join("data");
    let data = node_path::from_native(&data_path);
    let owned = node_path::from_native(&data_path.join("home"));
    let normalized = node_path::from_native(&data_path.join("home").join("child").join(".."));
    for name in ["codexHome", "nativeHome"] {
        let settings = IvyJson::object([(name, IvyJson::String(normalized.clone()))]);
        let result = codex_home::resolve_codex_home_json(&settings, &context).unwrap();
        assert_eq!(result.path, owned);
        assert_eq!(
            result.source,
            if name == "codexHome" {
                "host-configuration"
            } else {
                "existing-native-home"
            }
        );
        assert_eq!(
            codex_home::instance_owned_codex_home_json(&settings, &data, &context).unwrap(),
            Some(owned.clone())
        );
        assert_eq!(
            codex_home::instance_owned_codex_home_data_json(
                &settings,
                Some(&IvyJson::String(data.clone())),
                &context
            )
            .unwrap(),
            Some(owned.clone())
        );
    }
    let same = IvyJson::object([
        ("codexHome", IvyJson::String(normalized)),
        ("nativeHome", IvyJson::String(owned.clone())),
    ]);
    assert_eq!(
        codex_home::resolve_codex_home_json(&same, &context)
            .unwrap()
            .source,
        "host-configuration"
    );
    let mut conflict = same.clone();
    conflict.insert("nativeHome", path(&fixture.0.join("other")));
    assert_eq!(
        codex_home::resolve_codex_home_json(&conflict, &context)
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    let result = codex_home::resolve_codex_home_json(&empty(), &context).unwrap();
    assert_eq!((result.path, result.source), (inherited, "environment"));
    assert_eq!(
        codex_home::instance_owned_codex_home_data_json(
            &empty(),
            Some(&IvyJson::Bool(false)),
            &context
        )
        .unwrap(),
        None
    );
    let no_environment = BTreeMap::new();
    let default_context = HomeContext {
        environment: &no_environment,
        ..context
    };
    let result = codex_home::resolve_codex_home_json(&empty(), &default_context).unwrap();
    assert_eq!(
        (result.path, result.source),
        (
            node_path::from_native(&fixture.0.join("account").join(".codex")),
            "user-home"
        )
    );
    assert_eq!(
        codex_home::instance_owned_codex_home_json(&empty(), &account, &default_context).unwrap(),
        None
    );
    let cwd = node_path::from_native(&fixture.0);
    for (inherited, expected, source) in [
        (" \tchild\r\n", fixture.0.join("child"), "environment"),
        ("\u{85}child\u{85}", fixture.0.join("child"), "environment"),
        (
            "\u{feff}child",
            fixture.0.join("\u{feff}child"),
            "environment",
        ),
        (
            " \u{85}\t",
            fixture.0.join("account").join(".codex"),
            "user-home",
        ),
    ] {
        let environment = BTreeMap::from([("CODEX_HOME".into(), inherited.into())]);
        let context = HomeContext {
            environment: &environment,
            cwd: &cwd,
            ..default_context
        };
        let actual = codex_home::resolve_codex_home_json(&empty(), &context).unwrap();
        assert_eq!(
            (actual.path, actual.source),
            (node_path::from_native(&expected), source)
        );
    }
    for home in [
        data_path.clone(),
        fixture.0.join("data-other").join("home"),
        data_path.join("..").join("outside"),
        fixture.0.join("shared"),
    ] {
        let settings = IvyJson::object([("codexHome", path(&home))]);
        assert_eq!(
            codex_home::instance_owned_codex_home_json(&settings, &data, &context).unwrap(),
            None
        );
    }
    #[cfg(windows)]
    {
        let mut opaque = owned.clone();
        opaque.0.extend([92, 0xd800]);
        let settings = IvyJson::object([("codexHome", IvyJson::String(opaque.clone()))]);
        assert_eq!(
            codex_home::resolve_codex_home_json(&settings, &context)
                .unwrap()
                .path,
            opaque
        );
        assert_eq!(
            codex_home::instance_owned_codex_home_json(&settings, &data, &context).unwrap(),
            Some(opaque)
        );
        let mut aliases = same;
        aliases.insert(
            "nativeHome",
            IvyJson::String(JsString(
                owned
                    .0
                    .iter()
                    .map(|c| if (97..=122).contains(c) { c - 32 } else { *c })
                    .collect(),
            )),
        );
        assert_eq!(
            codex_home::resolve_codex_home_json(&aliases, &context)
                .unwrap()
                .source,
            "host-configuration"
        );
    }
}

#[test]
fn native_codex_home_admits_declared_fields_and_defers_external_home_validation() {
    let fixture = ServiceFixture::new();
    let paths = node_path::Context::current().unwrap();
    let environment = BTreeMap::new();
    let account = node_path::from_native(&fixture.0.join("account"));
    let context = HomeContext {
        paths: &paths,
        environment: &environment,
        user_home: None,
        cwd: &paths.cwd,
        account_home: &account,
    };
    let malformed = [
        IvyJson::Null,
        false.into(),
        true.into(),
        IvyJson::Number(0.0),
        IvyJson::Number(7.0),
        IvyJson::Array(vec![]),
        empty(),
    ];
    for settings in malformed.iter().chain([IvyJson::from("primitive")].iter()) {
        if settings.as_object().is_some() {
            continue;
        }
        assert_eq!(
            codex_home::resolve_codex_home_json(settings, &context)
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
        assert_eq!(
            codex_home::uses_external_app_server_json(settings)
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
    }
    for name in ["codexHome", "nativeHome"] {
        for value in &malformed {
            let mut settings = IvyJson::object([("codexHome", path(&fixture.0.join("valid")))]);
            settings.insert(name, value.clone());
            assert_eq!(
                codex_home::resolve_codex_home_json(&settings, &context)
                    .unwrap_err()
                    .code,
                "invalid_arguments"
            );
            assert_eq!(
                codex_home::instance_owned_codex_home_json(&settings, &account, &context)
                    .unwrap_err()
                    .code,
                "invalid_arguments"
            );
        }
        for home in ["", "  ", "relative"] {
            let settings = IvyJson::object([(name, home.into())]);
            assert_eq!(
                codex_home::resolve_codex_home_json(&settings, &context)
                    .unwrap_err()
                    .code,
                "invalid_arguments"
            );
        }
    }
    assert!(codex_home::uses_external_app_server_json(&empty()).unwrap());
    for (mode, external) in [
        ("owned-stdio", false),
        ("claude-adapter", false),
        ("external-proxy", true),
        ("future-mode", true),
    ] {
        let settings = IvyJson::object([("appServer", IvyJson::object([("mode", mode.into())]))]);
        assert_eq!(
            codex_home::uses_external_app_server_json(&settings).unwrap(),
            external
        );
        assert_eq!(
            codex_home::expected_server_home_json(&settings, &context).unwrap(),
            path(&fixture.0.join("account").join(".codex"))
        );
    }
    for value in &malformed {
        if value.as_object().is_none() {
            let settings = IvyJson::object([("appServer", value.clone())]);
            assert_eq!(
                codex_home::uses_external_app_server_json(&settings)
                    .unwrap_err()
                    .code,
                "invalid_arguments"
            );
        }
        let settings = IvyJson::object([("appServer", IvyJson::object([("mode", value.clone())]))]);
        assert_eq!(
            codex_home::uses_external_app_server_json(&settings)
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
    }
    for expected in malformed
        .into_iter()
        .chain([path(&fixture.0.join("shared")), "relative".into()])
    {
        let settings = IvyJson::object([
            ("codexHome", false.into()),
            ("nativeHome", "relative-ignored".into()),
            (
                "appServer",
                IvyJson::object([
                    ("mode", "external-proxy".into()),
                    ("expectedCodexHome", expected.clone()),
                ]),
            ),
        ]);
        assert_eq!(
            codex_home::expected_server_home_json(&settings, &context).unwrap(),
            expected
        );
    }
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
