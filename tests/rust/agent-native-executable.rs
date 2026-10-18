use ivy_host_runtime::{codex_rpc, process};
#[path = "../../packages/host-runtime/rust/src/codex_daemon.rs"]
mod codex_daemon;
#[path = "../../packages/host-runtime/rust/src/codex_handshake.rs"]
mod codex_handshake;
#[path = "../../packages/host-runtime/rust/src/codex_process.rs"]
mod native_process;
use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
};
use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("ivy-native-executable-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn executable_and_cli_version_match_original_boundaries_without_native_launch() {
    let fixture = Root::new();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut child = Command::new("node")
        .arg(repo.join("tests/fixtures/native-executable-oracle.mjs"))
        .current_dir(repo)
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
            stringify(
                &IvyJson::object([("root", fixture.0.to_str().unwrap().into())]),
                65536,
            )
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
    let data = parse(std::str::from_utf8(&output.stdout).unwrap(), 1024 * 1024).unwrap();
    let rows = data.get("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 9);
    for row in rows {
        let configured = PathBuf::from(
            row.get("executable")
                .unwrap()
                .as_js_string()
                .unwrap()
                .to_utf8()
                .unwrap(),
        );
        let actual = match native_process::verify_executable(
            &configured,
            row.get("expectedHash").unwrap().as_js_string().unwrap(),
        )
        .await
        {
            Ok(path) => {
                assert_eq!(path, std::fs::canonicalize(&configured).unwrap());
                IvyJson::object([("ok", true.into())])
            }
            Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
        };
        assert_eq!(
            &actual,
            row.get("expected").unwrap(),
            "{:?}",
            row.get("mode")
        );
    }
    let versions = data.get("versions").unwrap().as_array().unwrap();
    assert_eq!(versions.len(), 40);
    for row in versions {
        let result = process::ProcessResult {
            exit_code: match row.get("exitCode").unwrap() {
                IvyJson::Number(n) => Some(*n as i32),
                _ => None,
            },
            signal: None,
            error_code: None,
            stdout: row
                .get("stdout")
                .unwrap()
                .as_js_string()
                .unwrap()
                .to_utf8()
                .unwrap(),
            stderr: String::new(),
            truncated: false,
        };
        let actual = native_process::verify_version_result(&result, &"0.154.0".into());
        assert_eq!(
            actual.is_ok(),
            row.get("expected") == Some(&IvyJson::Bool(true)),
            "{:?}",
            row.get("stdout")
        );
        if let Err(error) = actual {
            assert_eq!(error.code, "native_build_mismatch");
            assert_eq!(
                error.message,
                "Codex CLI version differs from the selected catalog."
            );
        }
    }
    let command = native_process::version_command();
    assert_eq!(command.executable, "codex");
    assert_eq!(command.args, vec![std::ffi::OsString::from("--version")]);
    assert_eq!(command.timeout_ms, 15000);
    assert!(command.cwd.is_none());
}
