use ivy_contracts::{
    IvyJson, IvyResult, JsString,
    json::{canonical, parse},
};
use ivy_host_runtime::{
    codex_home::*,
    node_path::{Context, Style},
};
use std::{collections::BTreeMap, path::PathBuf, process::Command};
fn field<'a>(v: &'a IvyJson, k: &str) -> &'a IvyJson {
    v.get(k).unwrap()
}
fn text<'a>(v: &'a IvyJson, k: &str) -> &'a JsString {
    field(v, k).as_js_string().unwrap()
}
fn map(v: &IvyJson) -> BTreeMap<JsString, JsString> {
    v.as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_js_string().unwrap().clone()))
        .collect()
}
fn capture(result: IvyResult<IvyJson>) -> IvyJson {
    match result {
        Ok(v) => IvyJson::object([("result", v)]),
        Err(e) => IvyJson::object([("error", e.to_wire())]),
    }
}
#[test]
fn raw_settings_keep_null_presence_type_failures_and_external_values() {
    let out = Command::new("node")
        .arg("tests/fixtures/agent-native-home-json-oracle.mjs")
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let data = parse(std::str::from_utf8(&out.stdout).unwrap(), 8 * 1024 * 1024).unwrap();
    let c = field(&data, "context");
    let environment = map(field(c, "environment"));
    let drive_cwds = map(field(c, "driveCwds"));
    let native_platform = text(c, "nativePlatform");
    let native = Context {
        style: if native_platform == &JsString::from("win32") {
            Style::Windows
        } else {
            Style::Posix
        },
        cwd: field(field(c, "lexicalCwds"), &native_platform.to_utf8().unwrap())
            .as_js_string()
            .unwrap()
            .clone(),
        drive_cwds: drive_cwds.clone(),
    };
    let native_context = HomeContext {
        paths: &native,
        environment: &environment,
        user_home: None,
        cwd: text(c, "processCwd"),
        account_home: text(c, "accountHome"),
    };
    let rows = field(&data, "rows").as_array().unwrap();
    assert_eq!(rows.len(), 626);
    for (index, row) in rows.iter().enumerate() {
        let settings = field(row, "settings");
        let options = field(row, "options");
        let platform = text(row, "platform").to_utf8().unwrap();
        let paths = Context {
            style: if platform == "win32" {
                Style::Windows
            } else {
                Style::Posix
            },
            cwd: field(field(c, "lexicalCwds"), &platform)
                .as_js_string()
                .unwrap()
                .clone(),
            drive_cwds: drive_cwds.clone(),
        };
        let option_env = map(field(options, "environment"));
        let selected = HomeContext {
            paths: &paths,
            environment: &option_env,
            user_home: Some(text(options, "userHome")),
            cwd: text(options, "cwd"),
            account_home: text(c, "accountHome"),
        };
        let ambient = HomeContext {
            paths: &paths,
            environment: &environment,
            user_home: None,
            cwd: text(c, "processCwd"),
            account_home: text(c, "accountHome"),
        };
        let resolution = resolve_codex_home_json(settings, &selected).map(|r| {
            IvyJson::object([
                ("path", IvyJson::String(r.path)),
                ("source", r.source.into()),
            ])
        });
        let actual = IvyJson::object([
            ("resolution", capture(resolution)),
            (
                "external",
                capture(uses_external_app_server_json(settings).map(IvyJson::Bool)),
            ),
            (
                "expectedHome",
                capture(expected_server_home_json(settings, &native_context)),
            ),
            (
                "owned",
                IvyJson::Array(
                    field(row, "roots")
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|root| {
                            capture(
                                instance_owned_codex_home_json(
                                    settings,
                                    root.as_js_string().unwrap(),
                                    &ambient,
                                )
                                .map(|r| r.map(IvyJson::String).unwrap_or(IvyJson::Null)),
                            )
                        })
                        .collect(),
                ),
            ),
        ]);
        assert_eq!(
            canonical(&actual, usize::MAX).unwrap(),
            canonical(field(row, "expected"), usize::MAX).unwrap(),
            "raw settings case {index}"
        );
    }
}
