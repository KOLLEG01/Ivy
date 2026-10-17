use serde_json::Value;
use std::{path::Path, process::Command};

pub fn oracle(mode: &str, path: Option<&Path>) -> Value {
    let mut command = Command::new("node");
    command
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/task-board-evidence-oracle.mjs",
            mode,
        ])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    if let Some(path) = path {
        command.arg(path);
    }
    let result = command.output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&result.stdout).unwrap()
}
