use ivy_contracts::{IvyJson, hash::digest};
use ivy_host_runtime::{
    codex_windows_shell::verify_windows_shell,
    native_path::{self, Context},
};
use std::path::PathBuf;

#[tokio::test]
async fn native_shell_admission_rejects_missing_malformed_and_changed_inputs_before_spawn() {
    let paths = Context::current().unwrap();
    let root = std::env::temp_dir().join(format!("ivy-agent-shell-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let launcher = PathBuf::from("absent-job.exe");
    assert_eq!(
        verify_windows_shell(None, &root, &launcher, &paths)
            .await
            .unwrap_err()
            .code,
        "native_shell_required"
    );
    if cfg!(windows) {
        for invalid in [
            IvyJson::Bool(false),
            IvyJson::Number(1.0),
            IvyJson::Array(vec![]),
        ] {
            assert_eq!(
                verify_windows_shell(Some(&invalid), &root, &launcher, &paths)
                    .await
                    .unwrap_err()
                    .code,
                "invalid_arguments"
            );
        }
        let executable = root.join("pwsh.exe");
        std::fs::write(&executable, b"never execute").unwrap();
        let settings = IvyJson::object([
            (
                "executable",
                IvyJson::String(native_path::from_native(&executable)),
            ),
            ("executableHash", digest(b"different bytes").into()),
        ]);
        assert_eq!(
            verify_windows_shell(Some(&settings), &root, &launcher, &paths)
                .await
                .unwrap_err()
                .code,
            "native_shell_mismatch"
        );
        assert_eq!(std::fs::read(executable).unwrap(), b"never execute");
    }
    std::fs::remove_dir_all(root).unwrap();
}
