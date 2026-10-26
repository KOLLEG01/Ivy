#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use super::*;
use ivy_contracts::json::canonical;
use ivy_secretary::{config, outlook_media, outlook_projection};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

fn corpus() -> IvyJson {
    let output = reference_data::read(include_bytes!("outlook-projection-cases.json.gz"));
    parse(
        std::str::from_utf8(&output).unwrap(),
        32 * 1024 * 1024,
    )
    .unwrap()
}
fn result(value: Result<IvyJson, IvyError>) -> IvyJson {
    match value {
        Ok(value) => IvyJson::object([("value", value)]),
        Err(e) => {
            let mut value = IvyJson::object([
                ("code", IvyJson::String(e.code_js())),
                ("message", IvyJson::String(e.message_js())),
                ("outcome", e.outcome.to_json()),
            ]);
            if let Some(details) = e.details {
                value.insert("details", details);
            }
            IvyJson::object([("error", value)])
        }
    }
}
fn compare(label: &str, actual: &IvyJson, expected: &IvyJson) {
    assert_eq!(
        canonical(actual, 32 * 1024 * 1024).unwrap(),
        canonical(expected, 32 * 1024 * 1024).unwrap(),
        "{label}"
    );
}
#[test]
fn original_outlook_projection_metadata_and_collector_admission() {
    let corpus = corpus();
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 247);
    for row in rows {
        let input = row.required("input").unwrap();
        let value = input.required("value").unwrap();
        let label = input.required("label").unwrap().as_str().unwrap();
        let option = |key| {
            input
                .get(key)
                .unwrap_or_else(|| corpus.required(key).unwrap())
        };
        let actual = match input.required("action").unwrap().as_str().unwrap().as_str() {
            "microsoftProfile" => outlook_projection::microsoft_profile(value),
            "microsoftMcp" => outlook_projection::microsoft_mcp(value),
            "outlookArguments" => outlook_projection::outlook_arguments(value),
            "outlookInlineReferences" => outlook_projection::outlook_inline_references(value)
                .map(|v| IvyJson::Array(v.into_iter().map(IvyJson::from).collect())),
            "outlookHasMedia" => outlook_projection::outlook_has_media(value).map(IvyJson::Bool),
            "outlookBody" => outlook_projection::outlook_body(value).map(IvyJson::from),
            "page" => outlook_projection::outlook_page(
                value,
                option("profile"),
                option("source"),
                option("selected"),
                option("observedAt"),
            ),
            "metadata" => outlook_media::outlook_attachments(
                value,
                input.get("messageId").unwrap_or(&"message".into()),
            )
            .map(IvyJson::Array),
            "materialization" => {
                outlook_media::outlook_materialization(value, option("attachment"))
            }
            "microsoftConfig" => config::microsoft_configuration(value, option("settings")),
            "teamsConfig" => {
                config::teams_configuration(value, corpus.required("settings").unwrap())
            }
            other => panic!("Unknown projection fixture {other}"),
        };
        compare(&label, &result(actual), row.required("expected").unwrap());
    }
    println!(
        "outlook-projection original comparisons: {} cases",
        rows.len()
    );
}
struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0.join("config.json"));
        let _ = std::fs::remove_dir(&self.0);
    }
}
#[tokio::test]
async fn original_outlook_teams_composed_protected_configuration_loaders() {
    let corpus = corpus();
    assert_eq!(
        corpus.required("platform").unwrap(),
        &IvyJson::from(if cfg!(windows) { "win32" } else { "linux" })
    );
    let rows = corpus.required("fileRows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 22);
    let directory = Directory(std::env::temp_dir().join(format!(
        "secretary-outlook-projection-rust-{}",
        uuid::Uuid::new_v4()
    )));
    std::fs::create_dir(&directory.0).unwrap();
    let path = directory.0.join("config.json");
    for (index, row) in rows.iter().enumerate() {
        let input = row.required("input").unwrap();
        if input.get("missing") == Some(&true.into()) {
            if let Err(e) = std::fs::remove_file(&path) {
                assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
            }
        } else {
            let bytes = match input
                .get("rawKind")
                .and_then(|v| v.as_str().ok())
                .as_deref()
            {
                Some("syntax") => b"{".to_vec(),
                Some("bom") => "\u{feff}{\"schemaVersion\":1}".as_bytes().to_vec(),
                Some("oversized") => vec![b' '; 1048577],
                None => canonical(input.required("value").unwrap(), 8 * 1024 * 1024)
                    .unwrap()
                    .into_bytes(),
                other => panic!("Unknown file case {other:?}"),
            };
            std::fs::write(&path, bytes).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
        }
        let actual = if input.required("action").unwrap() == &"microsoftFile".into() {
            config::load_microsoft_configuration(&path, corpus.required("settings").unwrap()).await
        } else {
            config::load_teams_configuration(&path, corpus.required("settings").unwrap()).await
        };
        compare(
            &format!("Composed loader {index}"),
            &result(actual),
            row.required("expected").unwrap(),
        );
    }
    println!(
        "outlook-configuration original comparisons: {} protected-loader cases",
        rows.len()
    );
}
