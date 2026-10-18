use ivy_host_runtime::{node_path, paths, process};
#[path = "../../packages/host-runtime/rust/src/codex_daemon.rs"]
mod codex_daemon;
#[path = "../../packages/host-runtime/rust/src/codex_windows_shell.rs"]
mod shell;
use ivy_contracts::{IvyJson, json::parse};
use std::{path::PathBuf, process::Command};

#[test]
fn shell_probe_observations_match_original_with_injected_process_results() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = Command::new("node")
        .arg("tests/fixtures/native-shell-observation-oracle.mjs")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data = parse(std::str::from_utf8(&output.stdout).unwrap(), 1024 * 1024).unwrap();
    let rows = data.get("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 63);
    for row in rows {
        let result = process::ProcessResult {
            exit_code: match row.get("exitCode").unwrap() {
                IvyJson::Number(n) => Some(*n as i32),
                _ => None,
            },
            signal: None,
            error_code: None,
            stderr: String::new(),
            truncated: false,
            stdout: row
                .get("stdout")
                .unwrap()
                .as_js_string()
                .unwrap()
                .to_utf8()
                .unwrap(),
        };
        let actual = match shell::probe_observation(&result) {
            Ok(value) => IvyJson::object([(
                "result",
                IvyJson::object([
                    ("version", IvyJson::String(value.version)),
                    ("executable", IvyJson::String(value.executable)),
                ]),
            )]),
            Err(error) => IvyJson::object([("error", error.to_wire())]),
        };
        assert_eq!(
            &actual,
            row.get("expected").unwrap(),
            "{:?}",
            row.get("name")
        );
    }
    let command = shell::probe_command();
    let original = data.get("command").unwrap().get("command").unwrap();
    let actual = IvyJson::object([
        ("executable", command.executable.into()),
        (
            "args",
            IvyJson::Array(
                command
                    .args
                    .iter()
                    .map(|arg| arg.to_str().unwrap().into())
                    .collect(),
            ),
        ),
        ("timeoutMs", IvyJson::Number(command.timeout_ms as f64)),
    ]);
    assert_eq!(&actual, original);
    assert!(command.cwd.is_none());
}
