#[path = "../../packages/host-runtime/rust/src/process_output.rs"]
mod process_output;
use process_output::DiagnosticCapture;

#[test]
fn streaming_bytes_match_the_existing_node_algorithm() {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let mut cases = Vec::new();
    let mut actual = Vec::new();
    for input in [
        "before secret-🙂-token after".as_bytes(),
        "\u{feff}é🙂\u{feff}tail".as_bytes(),
        &[0xff, 0xe2, 0x82, b'x'],
    ] {
        for split in 0..=input.len() {
            let secrets = ["secret-🙂-token"];
            let chunks = [&input[..split], &input[split..]];
            let mut capture = DiagnosticCapture::new(
                secrets.iter().map(|s| s.encode_utf16().collect()).collect(),
                65536,
            );
            let mut emitted = Vec::new();
            for chunk in chunks {
                emitted.push(capture.push(chunk, false));
            }
            emitted.push(capture.push(&[], true));
            actual.push(serde_json::json!({"text":capture.text(),"truncated":capture.truncated(),"emitted":emitted}));
            cases.push(serde_json::json!({"secrets":secrets,"chunks":chunks.iter().map(|chunk| chunk.iter().map(|byte| format!("{byte:02x}")).collect::<String>()).collect::<Vec<_>>(),"maximum":65536}));
        }
    }
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/process-output-oracle.mjs");
    let mut command = Command::new("node");
    command
        .arg("--disable-warning=ExperimentalWarning")
        .arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(&cases).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(actual, expected);
}
