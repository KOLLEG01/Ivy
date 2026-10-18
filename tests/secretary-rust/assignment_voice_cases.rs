use super::*;
use ivy_secretary::assignment_voice::{
    VoiceMethod, call_voice, forwarded_voice_outcome, voice_prompt,
};

#[tokio::test]
async fn original_voice_prompt_receipt_and_provider_calls() {
    let mut command = std::process::Command::new("node");
    command
        .arg("tests/secretary-rust/assignment-voice-oracle.mjs")
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().expect("Original Node oracle required");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let corpus = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        8 * 1024 * 1024,
    )
    .unwrap();
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 96);
    for (index, row) in rows.iter().enumerate() {
        let input = row.required("input").unwrap();
        let client = Arc::new(Scripted {
            steps: Mutex::new(
                input
                    .get("steps")
                    .and_then(IvyJson::as_array)
                    .unwrap_or(&[])
                    .iter()
                    .cloned()
                    .collect(),
            ),
            trace: Mutex::new(Vec::new()),
        });
        let tools = ToolClients::new(client.clone());
        let result = match input.required("kind").unwrap().as_str().unwrap().as_str() {
            "prompt" => Ok(IvyJson::String(voice_prompt(
                input.required("text").unwrap().as_js_string().unwrap(),
            ))),
            "proof" => forwarded_voice_outcome(
                input.required("operation").unwrap(),
                input.required("callId").unwrap(),
                input.required("operationId").unwrap(),
                input.required("prompt").unwrap(),
            )
            .map(|v| v.as_str().into()),
            _ => {
                call_voice(
                    client.as_ref(),
                    &tools,
                    if input.get("method") == Some(&"request".into()) {
                        VoiceMethod::Request
                    } else {
                        VoiceMethod::Operation
                    },
                    input.required("target").unwrap(),
                    input.required("args").unwrap().clone(),
                    input
                        .get("operationId")
                        .and_then(IvyJson::as_js_string)
                        .cloned(),
                )
                .await
            }
        };
        let result = match result {
            Ok(value) => IvyJson::object([("value", value)]),
            Err(e) => IvyJson::object([
                ("code", e.code.into()),
                ("message", e.message.into()),
                ("outcome", e.outcome.as_str().into()),
            ]),
        };
        let actual = IvyJson::object([
            ("result", result),
            (
                "trace",
                IvyJson::Array(client.trace.lock().unwrap().clone()),
            ),
            (
                "remaining",
                IvyJson::Number(client.steps.lock().unwrap().len() as f64),
            ),
        ]);
        assert_eq!(
            &actual,
            row.required("expected").unwrap(),
            "Voice case {index}"
        );
    }
}
