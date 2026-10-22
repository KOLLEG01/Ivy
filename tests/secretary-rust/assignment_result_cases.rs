use super::*;
use ivy_secretary::{
    assignment_prompt::{assignment_prompt, load_notification_policy},
    assignment_result::{assignment_completion, assignment_turn},
};

struct Distribution(std::path::PathBuf);
impl Distribution {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("secretary-instructions-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("instructions")).unwrap();
        Self(root)
    }
    fn asset(&self) -> std::path::PathBuf {
        self.0
            .join("instructions/secretary-notification-policy.json")
    }
}
impl Drop for Distribution {
    fn drop(&mut self) {
        assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn packaged_notification_policy_requires_lossless_json_string_data() {
    let distribution = Distribution::new();
    let invalid = || {
        let error = load_notification_policy(&distribution.0).unwrap_err();
        assert_eq!(error.code, "internal_error");
        assert_eq!(error.message, "The operation failed internally.");
        assert_eq!(error.outcome, Outcome::Unknown);
    };
    invalid();
    for bytes in [
        &b"\xff"[..],
        b"",
        b"\"unterminated",
        b"null",
        b"{}",
        b"[]",
        b"42",
        b"true",
        b"\"text\" null",
    ] {
        std::fs::write(distribution.asset(), bytes).unwrap();
        invalid();
    }
    for (text, units) in [
        ("\"\"", vec![]),
        (
            r#""\ud800\u0000\udc00\ud83d\ude00\r\n\"\\""#,
            vec![0xd800, 0, 0xdc00, 0xd83d, 0xde00, 13, 10, 34, 92],
        ),
        (" \"é\"\n", vec![0xe9]),
    ] {
        std::fs::write(distribution.asset(), text).unwrap();
        assert_eq!(load_notification_policy(&distribution.0).unwrap().0, units);
    }
}

fn outcome(result: ivy_contracts::IvyResult<IvyJson>) -> IvyJson {
    match result {
        Ok(value) => IvyJson::object([("value", value)]),
        Err(e) => IvyJson::object([
            ("code", e.code.into()),
            ("message", e.message.into()),
            ("outcome", e.outcome.to_json()),
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
    let original_policy = corpus
        .required("notificationPolicy")
        .unwrap()
        .as_js_string()
        .unwrap();
    let distribution = Distribution::new();
    let mut command = std::process::Command::new("node");
    command
        .args([
            "tools/build/build-instructions.mjs",
            "--component",
            "secretary",
            "--out",
        ])
        .arg(&distribution.0)
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command
        .output()
        .expect("Canonical instruction builder required");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let policy = load_notification_policy(&distribution.0).unwrap();
    assert_eq!(&policy, original_policy);
    let prompts = corpus.required("prompts").unwrap().as_array().unwrap();
    assert_eq!(prompts.len(), 20);
    for row in prompts {
        let actual = outcome(assignment_prompt(
            row.required("execution").unwrap(),
            &policy,
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
