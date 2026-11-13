#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use super::*;
use ivy_contracts::json::canonical;
use ivy_secretary::config;
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0.join("config.json"));
        let _ = std::fs::remove_dir(&self.0);
    }
}
fn envelope(result: Result<IvyJson, IvyError>) -> IvyJson {
    match result {
        Ok(value) => IvyJson::object([("value", value)]),
        Err(error) => IvyJson::object([(
            "error",
            IvyJson::object([
                ("code", IvyJson::String(error.code_js())),
                ("message", IvyJson::String(error.message_js())),
                ("outcome", error.outcome.to_json()),
            ]),
        )]),
    }
}
fn put_file(path: &Path, input: &IvyJson) {
    if input.get("missing") == Some(&true.into()) {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("{error}"),
        }
        return;
    }
    let bytes = match input
        .get("rawKind")
        .and_then(|v| v.as_str().ok())
        .as_deref()
    {
        Some("syntax") => b"{".to_vec(),
        Some("bom") => "\u{feff}{\"schemaVersion\":1}".as_bytes().to_vec(),
        Some("oversized") => vec![b' '; config::CONFIGURATION_BYTES + 1],
        None => canonical(input.required("value").unwrap(), 8 * 1024 * 1024)
            .unwrap()
            .into_bytes(),
        _ => panic!("Unexpected raw config fixture"),
    };
    std::fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[tokio::test]
async fn original_startup_config_values_and_composed_protected_loaders() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = reference_data::read(include_bytes!("config-cases.json.gz"));
    let corpus = parse(&String::from_utf8(output).unwrap(), 8 * 1024 * 1024).unwrap();
    let same_platform = corpus.required("platform").unwrap()
        == &IvyJson::from(if cfg!(windows) {
            "win32"
        } else if cfg!(target_os = "macos") {
            "darwin"
        } else {
            "linux"
        });
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 91);
    let directory = Directory(std::env::temp_dir().join(format!(
        "secretary-config-consumer-{}",
        uuid::Uuid::new_v4()
    )));
    std::fs::create_dir(&directory.0).unwrap();
    let file = directory.0.join("config.json");
    for (index, row) in rows.iter().enumerate() {
        let input = row.required("input").unwrap();
        let action = input.required("action").unwrap().as_str().unwrap();
        // Absolute path fixtures retain their captured platform's syntax. The
        // startup values and protected-file admission cases are portable.
        if action == "path" && !same_platform {
            continue;
        }
        let result = match action.as_str() {
            "minimum" => {
                let special = input.get("specialNumber").map(|value| {
                    IvyJson::Number(match value.as_str().unwrap().as_str() {
                        "NaN" => f64::NAN,
                        "Infinity" => f64::INFINITY,
                        "-Infinity" => f64::NEG_INFINITY,
                        _ => panic!("Unknown number fixture"),
                    })
                });
                config::minimum_message_age_minutes(special.as_ref().or(input.get("value")))
                    .map(IvyJson::Number)
            }
            "path" => config::service_configuration_path(
                input.required("dataRoot").unwrap().as_js_string().unwrap(),
            )
            .map(IvyJson::String),
            "startup" => config::startup_settings(input.required("value").unwrap()).cloned(),
            "envelopeFile" | "deliveryFile" => {
                put_file(&file, input);
                match action.as_str() {
                    "envelopeFile" => config::load_service_configuration(
                        &file,
                        config::CONFIGURATION_BYTES,
                        IvyError::new(
                            "fixture_envelope_invalid",
                            "Protected configuration fixture is invalid.",
                        ),
                    )
                    .await
                    .map(|v| v.unwrap_or(IvyJson::Null)),
                    "deliveryFile" => config::load_minimum_message_age_minutes(&file)
                        .await
                        .map(IvyJson::Number),
                    _ => unreachable!(),
                }
            }
            _ => panic!("Unexpected config fixture action"),
        };
        assert_eq!(
            canonical(&envelope(result), 8 * 1024 * 1024).unwrap(),
            canonical(row.required("expected").unwrap(), 8 * 1024 * 1024).unwrap(),
            "Original case {index}: {action}"
        );
    }
}
