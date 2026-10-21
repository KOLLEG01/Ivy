use serde_json::Value;
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

pub fn original_output(fixture: &str, input: &Value) -> Value {
    let mut child =
        Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()))
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../..")
                    .join(fixture),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "Original sender: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&result.stdout).unwrap()
}
