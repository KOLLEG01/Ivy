use ivy_contracts::{IvyJson as J, JsString, json};
use ivy_host_runtime::{
    filesystem, node_path, node_runtime::NodeRuntime, preparation_tools,
    runtime_dependencies as dependencies,
};
use std::{
    fs,
    path::{Path, PathBuf},
};
#[path = "host-service-fixture.rs"]
#[allow(dead_code)]
mod fixture;
use fixture::ServiceFixture;
#[path = "host-build-source-cases.rs"]
mod source_inputs;
fn parsed(text: &str) -> J {
    json::parse(text, usize::MAX).unwrap()
}
fn path(path: &Path) -> J {
    J::String(node_path::from_native(path))
}

#[test]
fn exact_npm_locations_keep_development_workspace_and_runtime_closure_order() {
    let lock = parsed(
        r#"{"lockfileVersion":3,"packages":{"":{},"node_modules/dev":{"dev":true},"node_modules/dev-extra":{"devOptional":true},"node_modules/workspace":{"link":true,"resolved":"packages/workspace"},"node_modules/runtime":{"dependencies":{"shared":"1","nested":"1"},"optionalDependencies":{"optional":"1","absent":"1"},"peerDependencies":{"peer":"1"}},"node_modules/shared":{"dependencies":{"leaf":"1"}},"node_modules/leaf":{"dependencies":{"runtime":"1"}},"node_modules/runtime/node_modules/nested":{"dependencies":{"shared":"1"}},"node_modules/optional":{},"node_modules/peer":{}}}"#,
    );
    assert_eq!(
        dependencies::development_package_locations(&lock).unwrap(),
        vec![JsString::from("node_modules/dev")]
    );
    assert_eq!(
        dependencies::workspace_package_locations(&lock).unwrap(),
        vec![("node_modules/workspace".into(), "packages/workspace".into())]
    );
    assert_eq!(
        dependencies::runtime_package_locations(
            &lock,
            ["runtime".into(), "shared".into(), "runtime".into()]
        )
        .unwrap(),
        [
            "node_modules/runtime",
            "node_modules/shared",
            "node_modules/runtime/node_modules/nested",
            "node_modules/optional",
            "node_modules/peer",
            "node_modules/leaf"
        ]
        .map(JsString::from)
    );
    for name in ["workspace", "unlocked"] {
        assert!(dependencies::runtime_package_locations(&lock, [name.into()]).is_err());
    }
}
#[test]
fn lock_classification_requires_objects_booleans_and_contained_package_locations() {
    for lock in [
        J::Null,
        parsed("[]"),
        parsed("{}"),
        parsed(r#"{"lockfileVersion":2,"packages":{}}"#),
        parsed(r#"{"lockfileVersion":3,"packages":[]}"#),
    ] {
        assert!(dependencies::development_package_locations(&lock).is_err());
        assert!(dependencies::runtime_package_locations(&lock, ["a".into()]).is_err());
        assert!(
            dependencies::runtime_package_locations(&lock, [])
                .unwrap()
                .is_empty()
        );
    }
    for field in ["dev", "devOptional", "optional", "link"] {
        for value in [J::Null, "true".into(), J::Number(1.0), J::Array(vec![])] {
            let lock = J::object([
                ("lockfileVersion", J::Number(3.0)),
                (
                    "packages",
                    J::object([("node_modules/a", J::object([(field, value)]))]),
                ),
            ]);
            assert!(dependencies::development_package_locations(&lock).is_err());
        }
    }
    for name in [
        "node_modules/../outside",
        "node_modules/a/file.js",
        "node_modules/@scope/../../outside",
        "node_modules//a",
    ] {
        let lock = J::object([
            ("lockfileVersion", J::Number(3.0)),
            (
                "packages",
                J::object([(name, J::object([] as [(&str, J); 0]))]),
            ),
        ]);
        assert!(dependencies::development_package_locations(&lock).is_err());
    }
}
#[test]
fn workspace_targets_remain_lossless_and_dependency_order_uses_the_stored_dictionary() {
    for target in ["packages\\workspace".into(), JsString(vec![0xd800])] {
        let lock = J::object([
            ("lockfileVersion", J::Number(3.0)),
            (
                "packages",
                J::object([(
                    "node_modules/workspace",
                    J::object([
                        ("link", true.into()),
                        ("resolved", J::String(target.clone())),
                    ]),
                )]),
            ),
        ]);
        let normalized = JsString(
            target
                .0
                .iter()
                .map(|unit| if *unit == 92 { 47 } else { *unit })
                .collect(),
        );
        assert_eq!(
            dependencies::workspace_package_locations(&lock).unwrap(),
            vec![("node_modules/workspace".into(), normalized)]
        );
    }
    let lock = parsed(
        r#"{"lockfileVersion":3,"packages":{"node_modules/a":{"dependencies":{"10":"1","2":"1","b":"1"}},"node_modules/2":{},"node_modules/10":{},"node_modules/b":{}}}"#,
    );
    assert_eq!(
        dependencies::runtime_package_locations(&lock, ["a".into()]).unwrap(),
        [
            "node_modules/a",
            "node_modules/10",
            "node_modules/2",
            "node_modules/b"
        ]
        .map(JsString::from)
    );
    for target in [
        "../outside",
        "/outside",
        "C:/outside",
        "packages/../a",
        "packages//a",
    ] {
        let lock = J::object([
            ("lockfileVersion", J::Number(3.0)),
            (
                "packages",
                J::object([(
                    "node_modules/workspace",
                    J::object([("link", true.into()), ("resolved", target.into())]),
                )]),
            ),
        ]);
        assert!(dependencies::workspace_package_locations(&lock).is_err());
    }
}
fn tool_runtime() -> NodeRuntime {
    NodeRuntime {
        executable: std::env::current_exe().unwrap(),
        version: "24.18.0".into(),
    }
}
fn bootstrap(f: &ServiceFixture) -> PathBuf {
    let root = f.0.join("bootstrap");
    fs::create_dir_all(root.join("dist/native")).unwrap();
    fs::write(
        root.join("dist/native/ivy-job.exe"),
        b"tool metadata fixture; never executed",
    )
    .unwrap();
    root
}
#[tokio::test]
async fn explicit_preparation_tools_keep_private_environment_and_native_paths() {
    let f = ServiceFixture::new();
    let root = bootstrap(&f);
    let temporary = f.0.join("temporary ü 空 😀");
    fs::create_dir(&temporary).unwrap();
    let binary = f.0.join("compiler.exe");
    fs::write(&binary, b"tool metadata fixture; never executed").unwrap();
    let tools = preparation_tools::preparation_tools(
        J::object([
            ("dotnet", path(&binary)),
            ("gcc", path(&binary)),
            ("$node", "relative overridden".into()),
            ("$job", "relative overridden".into()),
        ]),
        root.clone(),
        temporary.clone(),
        tool_runtime(),
    )
    .await
    .unwrap();
    for key in ["TEMP", "TMP", "TMPDIR"] {
        assert_eq!(
            tools.environment.get(std::ffi::OsStr::new(key)),
            Some(&fs::canonicalize(&temporary).unwrap().into_os_string())
        );
    }
    assert_eq!(tools.environment.len(), 5);
    let linked = f.0.join("linked-temp");
    filesystem::directory_link(&temporary, &linked).unwrap();
    preparation_tools::preparation_tools(
        J::object([] as [(&str, J); 0]),
        root,
        linked,
        tool_runtime(),
    )
    .await
    .unwrap();
}
#[tokio::test]
async fn preparation_tools_reject_nonobjects_relative_scripts_missing_and_directory_tools() {
    let f = ServiceFixture::new();
    let root = bootstrap(&f);
    assert!(
        preparation_tools::preparation_tools(
            J::Array(vec![]),
            root.clone(),
            f.0.clone(),
            tool_runtime()
        )
        .await
        .is_err()
    );
    for name in ["file.cmd", "file.bat", "file.PS1", "directory", "missing"] {
        let selected = f.0.join(name);
        if name == "directory" {
            fs::create_dir(&selected).unwrap();
        } else if name != "missing" {
            fs::write(&selected, b"metadata").unwrap();
        }
        assert!(
            preparation_tools::preparation_tools(
                J::object([("tool", path(&selected))]),
                root.clone(),
                f.0.clone(),
                tool_runtime()
            )
            .await
            .is_err()
        );
    }
    assert!(
        preparation_tools::preparation_tools(
            J::object([("tool", "relative".into())]),
            root,
            f.0.clone(),
            tool_runtime()
        )
        .await
        .is_err()
    );
}

#[test]
fn manager_actions_require_literal_identifiers_and_options_without_string_coercion() {
    use ivy_host_runtime::actions::{Action, LocalAction};
    let args = J::object([
        ("instanceId", "fixture".into()),
        ("operationId", "operation".into()),
        ("candidateId", "candidate".into()),
    ]);
    let operation = JsString::from("operation");
    LocalAction::from_manager(Action::Deploy, &args, Some(&operation))
        .unwrap()
        .request()
        .unwrap();
    for key in ["instanceId", "operationId", "candidateId"] {
        for value in [
            true.into(),
            J::Number(7.0),
            J::Array(vec!["value".into()]),
            J::object([] as [(&str, J); 0]),
        ] {
            let mut malformed = args.clone();
            malformed.insert(key, value);
            assert!(
                LocalAction::from_manager(Action::Deploy, &malformed, None).is_err(),
                "{key}"
            );
        }
    }
    let mut missing = args.clone();
    missing.remove("instanceId");
    assert!(LocalAction::from_manager(Action::Deploy, &missing, None).is_err());
    assert_eq!(
        LocalAction::from_manager(Action::Deploy, &args, Some(&"different".into()))
            .err()
            .unwrap()
            .code,
        "mutation_conflict"
    );
}
#[tokio::test]
async fn pinned_package_manager_requires_a_string_and_preserves_its_cache_input() {
    let f = ServiceFixture::new();
    for value in [
        "npm@11.0.0".into(),
        J::Array(vec!["npm@11.0.0".into()]),
        true.into(),
        J::Null,
        "npm@latest".into(),
    ] {
        ivy_host_runtime::config::atomic_json(
            f.0.join("package.json"),
            J::object([("packageManager", value.clone())]),
        )
        .await
        .unwrap();
        let actual =
            ivy_host_runtime::preparation_inputs::preparation_package_manager(f.0.clone()).await;
        if value == J::from("npm@11.0.0") {
            assert_eq!(actual.unwrap(), value);
        } else {
            assert!(actual.is_err());
        }
    }
}
#[test]
fn length_headers_and_native_systemd_pids_use_decimal_numbers() {
    use ivy_host_runtime::{
        portable_dependencies::{MAXIMUM_PORTABLE_PACKAGE_BYTES, check_portable_response},
        systemd::parse_systemd_state,
    };
    for value in [None, Some("0"), Some("42")] {
        check_portable_response(true, value).unwrap();
    }
    for value in ["NaN", "0x100", "-1", "1e3", "not bytes", ""] {
        assert!(
            check_portable_response(true, Some(value)).is_err(),
            "{value}"
        );
    }
    assert!(
        check_portable_response(
            true,
            Some(&(MAXIMUM_PORTABLE_PACKAGE_BYTES as u64 + 1).to_string())
        )
        .is_err()
    );
    assert!(check_portable_response(false, None).is_err());
    assert_eq!(
        parse_systemd_state(&"MainPID=123\nLoadState=loaded".into()).main_pid,
        Some(123)
    );
    for value in ["0", "0x123", "1e3", "NaN", "-1", "4294967296"] {
        assert_eq!(
            parse_systemd_state(&format!("MainPID={value}").into()).main_pid,
            None,
            "{value}"
        );
    }
}
