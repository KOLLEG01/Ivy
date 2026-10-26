#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use ivy_agent_manager::skills::normalize_skills;
use ivy_contracts::{IvyJson, JsString, hash::digest, json::parse};
use std::{path::PathBuf, process::Command};

#[test]
fn skill_preflight_matches_original_path_order_and_utf8_byte_limits() {
    let output = reference_data::read(include_bytes!("../fixtures/agent-skills-normalize-cases.json.gz"));
    let oracle = parse(std::str::from_utf8(&output).unwrap(), 1024 * 1024).unwrap();
    let rows = oracle.get("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 41);
    for row in rows {
        let files = row
            .get("files")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|file| {
                let repeat = match file.get("repeat").unwrap() {
                    IvyJson::Number(n) => *n as usize,
                    _ => panic!("repeat"),
                };
                let content = JsString(
                    file.get("text")
                        .unwrap()
                        .as_js_string()
                        .unwrap()
                        .0
                        .repeat(repeat),
                );
                IvyJson::object([
                    ("path", file.get("path").unwrap().clone()),
                    ("content", IvyJson::String(content)),
                ])
            })
            .collect();
        let document = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("hostId", IvyJson::Null),
            ("enabled", row.get("enabled").unwrap().clone()),
            ("files", IvyJson::Array(files)),
        ]);
        let actual = match normalize_skills(&document) {
            Ok(groups) => IvyJson::object([(
                "result",
                IvyJson::Array(
                    groups
                        .into_iter()
                        .map(|group| {
                            IvyJson::object([
                                ("name", IvyJson::String(group.name)),
                                (
                                    "files",
                                    IvyJson::Array(
                                        group
                                            .files
                                            .into_iter()
                                            .map(|(path, bytes)| {
                                                IvyJson::object([
                                                    ("path", IvyJson::String(path)),
                                                    ("bytes", IvyJson::Number(bytes.len() as f64)),
                                                    ("hash", digest(bytes).into()),
                                                ])
                                            })
                                            .collect(),
                                    ),
                                ),
                            ])
                        })
                        .collect(),
                ),
            )]),
            Err(error) => IvyJson::object([("error", error.to_wire())]),
        };
        assert_eq!(
            &actual,
            row.get("expected").unwrap(),
            "{:?}",
            row.get("name")
        );
    }
}
