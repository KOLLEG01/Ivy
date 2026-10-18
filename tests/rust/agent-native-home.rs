use ivy_host_runtime::node_path;
#[path = "../../packages/host-runtime/rust/src/codex_home.rs"]
mod codex_home;
use codex_home::*;
use ivy_contracts::{IvyJson, IvyResult, JsString, json::parse};
use std::{collections::BTreeMap, path::PathBuf, process::Command};
fn map(value: &IvyJson) -> BTreeMap<JsString, JsString> {
    value
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_js_string().unwrap().clone()))
        .collect()
}
fn capture(result: IvyResult<IvyJson>) -> IvyJson {
    match result {
        Ok(value) => IvyJson::object([("result", value)]),
        Err(error) => IvyJson::object([(
            "error",
            IvyJson::object([
                ("code", error.code.into()),
                ("message", error.message.into()),
            ]),
        )]),
    }
}
fn optional(value: &IvyJson, key: &str) -> Option<JsString> {
    value.get(key).and_then(IvyJson::as_js_string).cloned()
}
#[test]
fn home_resolution_external_reference_and_strict_ownership_match_node() {
    let out = Command::new("node")
        .arg("tests/fixtures/agent-native-home-oracle.mjs")
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let data = parse(std::str::from_utf8(&out.stdout).unwrap(), 16 * 1024 * 1024).unwrap();
    let rows = data.as_array().unwrap();
    assert_eq!(rows.len(), 1182);
    for (i, row) in rows.iter().enumerate() {
        let settings = row.get("settings").unwrap();
        let settings = HomeSettings {
            codex_home: optional(settings, "codexHome"),
            native_home: optional(settings, "nativeHome"),
        };
        let options = row.get("options").unwrap();
        let style = if row.get("platform") == Some(&IvyJson::from("win32")) {
            node_path::Style::Windows
        } else {
            node_path::Style::Posix
        };
        let paths = node_path::Context {
            style,
            cwd: row
                .get("processCwd")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
            drive_cwds: map(row.get("driveCwds").unwrap()),
        };
        let environment = map(options.get("environment").unwrap());
        let context = HomeContext {
            paths: &paths,
            environment: &environment,
            user_home: options.get("userHome").and_then(IvyJson::as_js_string),
            cwd: options.get("cwd").and_then(IvyJson::as_js_string),
            account_home: row.get("accountHome").unwrap().as_js_string().unwrap(),
        };
        let resolution = capture(resolve_codex_home(&settings, &context).map(|v| {
            IvyJson::object([
                ("path", IvyJson::String(v.path)),
                ("source", v.source.into()),
            ])
        }));
        let owned = row
            .get("roots")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|root| {
                capture(
                    instance_owned_codex_home(&settings, root.as_js_string().unwrap(), &context)
                        .map(|v| v.map(IvyJson::String).unwrap_or(IvyJson::Null)),
                )
            })
            .collect();
        let expected = row.get("expected").unwrap();
        assert_eq!(
            Some(&resolution),
            expected.get("resolution"),
            "resolution {i}"
        );
        assert_eq!(
            Some(&IvyJson::Array(owned)),
            expected.get("owned"),
            "ownership {i}"
        );
        // This API intentionally uses native ambient context, independent of
        // the explicit platform/options used by the separate resolver above.
        let ambient_paths = node_path::Context {
            style: node_path::Style::native(),
            cwd: paths.cwd.clone(),
            drive_cwds: paths.drive_cwds.clone(),
        };
        let ambient_environment = map(row.get("ambientEnvironment").unwrap());
        let ambient = HomeContext {
            paths: &ambient_paths,
            environment: &ambient_environment,
            user_home: None,
            cwd: None,
            account_home: context.account_home,
        };
        for server in expected.get("servers").unwrap().as_array().unwrap() {
            let mode = server.get("mode").unwrap();
            let mode = match mode
                .get("mode")
                .and_then(IvyJson::as_js_string)
                .map(|s| s.to_utf8().unwrap())
                .as_deref()
            {
                Some("owned-stdio") => AppServerMode::OwnedStdio,
                Some("claude-adapter") => AppServerMode::ClaudeAdapter,
                Some("external-proxy") => AppServerMode::ExternalProxy {
                    expected_codex_home: optional(mode, "expectedCodexHome"),
                },
                _ => AppServerMode::Default,
            };
            assert_eq!(
                Some(&IvyJson::Bool(uses_external_app_server(&mode))),
                server.get("external")
            );
            assert_eq!(
                Some(&capture(
                    expected_server_home(&settings, &mode, &ambient).map(IvyJson::String)
                )),
                server.get("expected"),
                "server home {i}"
            );
        }
    }
}
