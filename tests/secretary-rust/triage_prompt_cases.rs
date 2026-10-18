use super::*;
use ivy_contracts::hash::hash_json;
use ivy_secretary::triage_prompt::*;

#[tokio::test]
async fn original_dormant_triage_prompt_and_native_parameters() {
    let mut command = std::process::Command::new("node");
    command
        .arg("tests/secretary-rust/triage-prompt-oracle.mjs")
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
        32 * 1024 * 1024,
    )
    .unwrap();
    let policy = corpus
        .required("notificationPolicy")
        .unwrap()
        .as_js_string()
        .unwrap();
    let first = triage_output_schema().unwrap();
    assert_eq!(
        hash_json(&first).unwrap(),
        hash_json(corpus.required("schema").unwrap()).unwrap()
    );
    let mut changed = first;
    changed.insert("changed", true.into());
    assert!(triage_output_schema().unwrap().get("changed").is_none());
    assert_eq!(
        &triage_tool_configuration(),
        corpus.required("toolConfiguration").unwrap()
    );
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 36);
    for row in rows {
        let settings = row.required("settings").unwrap();
        let result =
            triage_prompt(row.required("input").unwrap(), settings, policy).and_then(|prompt| {
                if row.get("kind") == Some(&"params".into()) {
                    triage_native_params(
                        settings,
                        &prompt,
                        row.required("media").unwrap().as_array().unwrap(),
                    )
                } else {
                    Ok(prompt)
                }
            });
        let actual = match result {
            Ok(value) => IvyJson::object([("value", value)]),
            Err(error) => IvyJson::object([
                ("code", error.code.into()),
                ("message", error.message.into()),
                ("outcome", error.outcome.as_str().into()),
            ]),
        };
        assert_eq!(
            hash_json(&actual).unwrap(),
            hash_json(row.required("expected").unwrap()).unwrap(),
            "triage prompt {}",
            row.required("label").unwrap().as_str().unwrap()
        );
    }
}
