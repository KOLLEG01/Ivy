use super::*;
use ivy_secretary::{
    assignment_prompt::assignment_prompt,
    assignment_result::{assignment_completion, assignment_turn},
};

fn outcome(result: ivy_contracts::IvyResult<IvyJson>) -> IvyJson {
    match result {
        Ok(value) => IvyJson::object([("value", value)]),
        Err(e) => IvyJson::object([
            ("code", e.code.into()),
            ("message", e.message.into()),
            ("outcome", e.outcome.as_str().into()),
        ]),
    }
}

#[test]
fn original_assignment_prompt_and_completion_policy() {
    let mut command = std::process::Command::new("node");
    command
        .arg("tests/secretary-rust/assignment-result-oracle.mjs")
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
    let prompts = corpus.required("prompts").unwrap().as_array().unwrap();
    assert_eq!(prompts.len(), 20);
    for row in prompts {
        let actual = outcome(assignment_prompt(
            row.required("execution").unwrap(),
            policy,
        ));
        assert_eq!(
            &actual,
            row.required("expected").unwrap(),
            "Prompt {:?}",
            row.get("label")
        );
    }
    let now = corpus.required("at").unwrap().as_js_string().unwrap();
    let rows = corpus.required("results").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 43);
    for row in rows {
        let current = row.required("current").unwrap();
        let execution = current.required("value").unwrap();
        let items = row.required("items").unwrap().as_array().unwrap();
        let mut saves = 0;
        let result = assignment_turn(execution, row.required("read").unwrap()).map(|turn| {
            let mut next = current.clone();
            if let Some(value) = assignment_completion(execution, turn, items, now) {
                saves = 1;
                next.insert("value", value);
            }
            next
        });
        assert_eq!(
            &outcome(result),
            row.required("expected").unwrap(),
            "Completion {:?}",
            row.get("label")
        );
        // Each terminal classification reaches exactly one save and lease release
        // in the original runner; pending/invalid-read paths reach neither.
        assert_eq!(row.required("saves").unwrap().as_f64(), Some(saves as f64));
        assert_eq!(
            row.required("releases").unwrap().as_f64(),
            Some(saves as f64)
        );
    }
}
