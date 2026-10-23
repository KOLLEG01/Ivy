use ivy_contracts::{JsString, hash::digest};
use ivy_host_runtime::codex_process::{verify_cli_version, verify_executable};
use std::{collections::BTreeMap, path::PathBuf, process::Command};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("ivy-agent-executable-{}", uuid::Uuid::new_v4()));
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
async fn native_executable_admission_checks_exact_bytes_and_rejects_scripts_without_execution() {
    let root = Root::new();
    let bytes = b"fixture file identity only; never executable";
    let executable = root.0.join("native.exe");
    std::fs::write(&executable, bytes).unwrap();
    let hash = JsString::from(digest(bytes));
    assert_eq!(
        verify_executable(&executable, &hash).await.unwrap(),
        std::fs::canonicalize(&executable).unwrap()
    );
    assert_eq!(
        verify_executable(&executable, &format!("sha256:{}", "0".repeat(64)).into())
            .await
            .unwrap_err()
            .code,
        "native_build_mismatch"
    );
    for name in ["native.cmd", "native.bat", "native.ps1", ".CMD"] {
        let script = root.0.join(name);
        std::fs::write(&script, bytes).unwrap();
        assert_eq!(
            verify_executable(&script, &hash).await.unwrap_err().code,
            "native_build_mismatch"
        );
    }
    assert_eq!(
        verify_executable(&root.0, &hash).await.unwrap_err().code,
        "native_build_mismatch"
    );
    assert_eq!(std::fs::read(executable).unwrap(), bytes);
}

#[tokio::test]
async fn native_cli_version_uses_the_owned_process_and_refuses_another_pinned_version() {
    let root = Root::new();
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let executable = root.0.join(if cfg!(windows) {
        "native.exe"
    } else {
        "native"
    });
    let output = Command::new(std::env::var_os("IVY_TEST_RUSTC").unwrap_or_else(|| "rustc".into()))
        .args(["--edition=2024", "-C", "debuginfo=0"])
        .arg(repository.join("tests/fixtures/native-launch-child.rs"))
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let marker = root.0.join("cli");
    std::fs::write(
        root.0.join("launch-fixture.txt"),
        format!("\n\n\n\n{}\n{{}}\n0.154.0\n", marker.display()),
    )
    .unwrap();
    let artifact = std::env::var_os("IVY_TEST_AGENT_ARTIFACT")
        .map(PathBuf::from)
        .unwrap_or(repository);
    let launcher = artifact.join("dist/native/win32-x64/ivy-job.exe");
    verify_cli_version(
        &executable,
        &root.0,
        &"0.154.0".into(),
        BTreeMap::new(),
        &launcher,
    )
    .await
    .unwrap();
    assert_eq!(
        verify_cli_version(
            &executable,
            &root.0,
            &"0.999.0".into(),
            BTreeMap::new(),
            &launcher
        )
        .await
        .unwrap_err()
        .code,
        "native_build_mismatch"
    );
    assert_eq!(
        std::fs::read_to_string(root.0.join("cli.launches"))
            .unwrap()
            .lines()
            .count(),
        2
    );
}
