use ivy_host_runtime::process;
#[path = "../../packages/host-runtime/rust/src/codex_daemon.rs"]
mod daemon;
use ivy_contracts::{
    IvyError, IvyJson, Outcome,
    hash::digest,
    json::{parse, stringify},
};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::Arc,
};

fn set(value: &IvyJson, key: &str, replacement: Option<IvyJson>) -> IvyJson {
    let mut fields = value.as_object().unwrap().to_vec();
    fields.retain(|(name, _)| name != &key.into());
    if let Some(value) = replacement {
        fields.push((key.into(), value));
    }
    IvyJson::Object(fields)
}
fn observation(managed: &str) -> IvyJson {
    IvyJson::object([
        ("status", "alreadyRunning".into()),
        (
            "socketPath",
            if cfg!(windows) {
                "C:\\native.sock"
            } else {
                "/native.sock"
            }
            .into(),
        ),
        ("managedCodexPath", managed.into()),
        ("managedCodexVersion", "0.154.0".into()),
        ("cliVersion", "0.154.0".into()),
        ("appServerVersion", "0.154.0".into()),
        ("pid", IvyJson::Number(123.0)),
    ])
}

#[test]
fn daemon_observation_matches_original_structural_and_coercion_boundary() {
    let base = observation(if cfg!(windows) {
        "C:\\codex.exe"
    } else {
        "/codex"
    });
    let mut values = vec![
        base.clone(),
        IvyJson::Null,
        false.into(),
        IvyJson::Number(12.0),
        "started".into(),
        IvyJson::Array(vec![]),
        IvyJson::object([] as [(&str, IvyJson); 0]),
    ];
    for status in [
        "started",
        "alreadyRunning",
        "stopped",
        " Started",
        "started\n",
    ] {
        values.push(set(&base, "status", Some(status.into())));
        values.push(set(
            &base,
            "status",
            Some(IvyJson::Array(vec![status.into()])),
        ));
    }
    for value in [
        IvyJson::Null,
        true.into(),
        IvyJson::object([] as [(&str, IvyJson); 0]),
        IvyJson::object([("toString", "started".into())]),
        IvyJson::Array(vec![IvyJson::Array(vec!["started".into()])]),
        IvyJson::Array(vec![IvyJson::object([("toString", IvyJson::Null)])]),
    ] {
        values.push(set(&base, "status", Some(value)));
    }
    for key in [
        "status",
        "socketPath",
        "managedCodexPath",
        "cliVersion",
        "managedCodexVersion",
        "appServerVersion",
        "pid",
    ] {
        values.push(set(&base, key, None));
        values.push(set(&base, key, Some(IvyJson::Null)));
    }
    for n in [
        -1.0,
        0.0,
        -0.0,
        0.5,
        1.0,
        9_007_199_254_740_991.0,
        9_007_199_254_740_992.0,
    ] {
        values.push(set(&base, "pid", Some(IvyJson::Number(n))));
    }
    values.push(set(&base, "pid", Some("123".into())));
    for key in ["cliVersion", "managedCodexVersion", "appServerVersion"] {
        values.push(set(&base, key, Some("0.154.1".into())));
    }
    for key in ["socketPath", "managedCodexPath"] {
        for path in [
            "",
            "relative",
            "/",
            "\\",
            "C:",
            "C:relative",
            "C:\\",
            "//server/share",
            "\\\\server\\share",
            "1:\\",
            "/foo/../",
            "\0",
            " /root",
        ] {
            values.push(set(&base, key, Some(path.into())));
        }
        values.push(set(
            &base,
            key,
            Some(IvyJson::String(ivy_contracts::JsString(vec![47, 0xd800]))),
        ));
    }
    let actual = IvyJson::Array(
        values
            .iter()
            .map(
                |value| match daemon::daemon_observation(value, &"0.154.0".into()) {
                    Ok(value) => IvyJson::object([("result", value)]),
                    Err(error) => IvyJson::object([
                        ("code", error.code.into()),
                        ("message", error.message.into()),
                        ("outcome", error.outcome.to_json()),
                    ]),
                },
            )
            .collect(),
    );
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut child = Command::new("node")
        .arg(root.join("tests/fixtures/native-daemon-oracle.mjs"))
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            stringify(&IvyJson::Array(values), 1024 * 1024)
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        actual,
        parse(std::str::from_utf8(&output.stdout).unwrap(), 1024 * 1024).unwrap()
    );
}

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ivy-daemon-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn managed(&self, bin: bool) -> PathBuf {
        self.0
            .join("packages/standalone/current")
            .join(if bin { "bin" } else { "" })
            .join(if cfg!(windows) { "codex.exe" } else { "codex" })
    }
    fn install(&self, bin: bool) -> PathBuf {
        let path = self.managed(bin);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"verified fixture bytes").unwrap();
        path
    }
    fn settings(&self) -> daemon::Settings {
        daemon::Settings {
            executable: self.0.join("selected-native"),
            executable_hash: digest(b"verified fixture bytes"),
            version: "0.154.0".into(),
            home: self.0.clone(),
            job_launcher: self.0.join("ivy-job.exe"),
            environment: BTreeMap::from([
                ("CODEX_HOME".into(), "wrong inherited home".into()),
                ("CUSTOM".into(), "retained".into()),
            ]),
        }
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn result(value: &IvyJson) -> Arc<process::ProcessResult> {
    Arc::new(process::ProcessResult {
        exit_code: Some(0),
        signal: None,
        error_code: None,
        stdout: stringify(value, 65536).unwrap(),
        stderr: String::new(),
        truncated: false,
    })
}

#[tokio::test]
async fn daemon_checks_installation_before_start_and_revalidates_after_start() {
    for layout in [true, false] {
        let root = Root::new();
        let managed = root.install(layout);
        let expected = observation(managed.to_str().unwrap());
        let expected_ref = &expected;
        let root_ref = &root;
        let actual = daemon::ensure_with(root.settings(), |launch| async move {
            assert_eq!(launch.command.executable, "codex");
            assert_eq!(
                launch.command.args,
                vec![
                    OsString::from("app-server"),
                    "daemon".into(),
                    "start".into()
                ]
            );
            assert_eq!(launch.command.timeout_ms, 100_000);
            assert!(launch.context.allow_windows_breakaway);
            assert!(!launch.context.without_job);
            assert_eq!(
                launch
                    .context
                    .environment
                    .get(&OsString::from("CODEX_HOME")),
                Some(&root_ref.0.clone().into_os_string())
            );
            assert_eq!(
                launch.context.environment.get(&OsString::from("CUSTOM")),
                Some(&OsString::from("retained"))
            );
            Ok(result(expected_ref))
        })
        .await
        .unwrap();
        assert_eq!(
            actual,
            daemon::daemon_observation(&expected, &"0.154.0".into()).unwrap()
        );
    }
    for mode in ["missing", "directory", "hash"] {
        let root = Root::new();
        if mode == "directory" {
            std::fs::create_dir_all(root.managed(true)).unwrap();
        }
        if mode == "hash" {
            let managed = root.install(true);
            std::fs::write(managed, b"changed").unwrap();
        }
        let error = daemon::ensure_with(root.settings(), |_| async {
            panic!("Must reject before starting")
        })
        .await
        .unwrap_err();
        assert_eq!(
            error.code,
            if mode == "hash" {
                "native_daemon_installation_mismatch"
            } else {
                "native_daemon_installation_missing"
            }
        );
    }
    for mode in [
        "changed-after",
        "different-path",
        "wrong-version",
        "truncated",
        "invalid-json",
        "failed-command",
    ] {
        let root = Root::new();
        let managed = root.install(true);
        let expected = observation(managed.to_str().unwrap());
        let error = daemon::ensure_with(root.settings(), |_| async {
            let mut response = result(&expected);
            match mode {
                "changed-after" => std::fs::write(&managed, b"changed").unwrap(),
                "different-path" => {
                    let other = root.0.join("other-native");
                    std::fs::write(&other, b"verified fixture bytes").unwrap();
                    response = result(&set(
                        &expected,
                        "managedCodexPath",
                        Some(other.to_str().unwrap().into()),
                    ));
                }
                "wrong-version" => {
                    response = result(&set(&expected, "appServerVersion", Some("0.153.0".into())))
                }
                "truncated" => Arc::make_mut(&mut response).truncated = true,
                "invalid-json" => Arc::make_mut(&mut response).stdout = "{bad".into(),
                "failed-command" => {
                    return Err(IvyError::new("command_failed", "Original process failure")
                        .with_outcome(Outcome::Unknown));
                }
                _ => unreachable!(),
            }
            Ok(response)
        })
        .await
        .unwrap_err();
        assert_eq!(
            error.code,
            match mode {
                "changed-after" | "different-path" => "native_daemon_installation_mismatch",
                "wrong-version" => "native_server_version_mismatch",
                "truncated" => "native_daemon_start_failed",
                "invalid-json" => "native_daemon_response_invalid",
                _ => "command_failed",
            }
        );
        if mode == "failed-command" {
            assert_eq!(error.outcome, Outcome::Unknown);
            assert_eq!(error.message, "Original process failure");
        }
    }
}

#[test]
fn daemon_linux_scope_is_only_for_runtime_service_and_keeps_exact_official_command() {
    for (text, expected) in [
        ("0::/system.slice/ivy-next-demo.service", true),
        ("0::/ivy-next-x.service/child", true),
        ("0::/ivy-next-x.service\n", true),
        ("0::/ivy-next-x.service\r\n", true),
        ("0::/ivy-next-x.service\u{2028}", true),
        ("0::/ivy-next-.service", false),
        ("0::/ivy-next-x.service.other", false),
        ("0::/ivy-next-x/other.service", false),
        ("0::/ivy-next-x\nother.service", false),
        ("0::/user.slice/native.service", false),
    ] {
        assert_eq!(daemon::in_runtime_service(text), expected, "{text:?}");
    }
    let root = Root::new();
    let settings = root.settings();
    let launch = daemon::invocation(&settings, Some((1234, "fixture-unit")));
    assert_eq!(launch.command.executable, "systemd-run");
    assert_eq!(
        launch.executables.get("systemd-run"),
        Some(&PathBuf::from("/usr/bin/systemd-run"))
    );
    let mut args: Vec<OsString> = [
        "--user",
        "--scope",
        "--quiet",
        "--unit=ivy-codex-start-fixture-unit",
        "--",
    ]
    .map(Into::into)
    .into();
    args.push(settings.executable.into_os_string());
    args.extend(["app-server", "daemon", "start"].map(OsString::from));
    assert_eq!(launch.command.args, args);
    assert_eq!(
        launch
            .context
            .environment
            .get(&OsString::from("DBUS_SESSION_BUS_ADDRESS")),
        Some(&OsString::from("unix:path=/run/user/1234/bus"))
    );
    assert_eq!(
        launch
            .context
            .environment
            .get(&OsString::from("XDG_RUNTIME_DIR")),
        Some(&OsString::from("/run/user/1234"))
    );
}
