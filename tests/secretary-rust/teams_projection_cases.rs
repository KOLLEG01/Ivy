use super::*;
use ivy_contracts::json::canonical;
use ivy_secretary::teams_projection;
use std::{path::Path, process::Command};
#[test]
fn original_teams_projection_identity_completeness_and_exclusions() {
    let mut command = Command::new("node");
    command
        .arg("tests/secretary-rust/teams-projection-oracle.mjs")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().unwrap();
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
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert!(rows.len() > 100);
    for row in rows {
        let input = row.required("input").unwrap();
        let value = input.required("value").unwrap();
        let option = |key| input.get(key).unwrap_or_else(|| corpus.get(key).unwrap());
        let actual = match input.required("action").unwrap().as_str().unwrap().as_str() {
            "arguments" => teams_projection::teams_arguments(option("configuration"), value),
            "time" => teams_projection::teams_time(value).map(IvyJson::from),
            "source" => teams_projection::teams_source(option("configuration"), value)
                .map(|_| IvyJson::Null),
            _ => teams_projection::teams_page(
                value,
                option("configuration"),
                option("source"),
                option("selection"),
                option("observedAt"),
            ),
        };
        let actual = match actual {
            Ok(v) => IvyJson::object([("value", v)]),
            Err(e) => {
                let mut error = IvyJson::object([
                    ("code", IvyJson::String(e.code_js())),
                    ("message", IvyJson::String(e.message_js())),
                    ("outcome", e.outcome.as_str().into()),
                ]);
                if let Some(details) = e.details {
                    error.insert("details", details);
                }
                IvyJson::object([("error", error)])
            }
        };
        assert_eq!(
            canonical(&actual, 32 * 1024 * 1024).unwrap(),
            canonical(row.required("expected").unwrap(), 32 * 1024 * 1024).unwrap(),
            "{}",
            input.required("label").unwrap().as_str().unwrap()
        );
    }
    println!(
        "teams-projection original comparisons: {} cases",
        rows.len()
    );
}
