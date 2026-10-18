use super::*;
use ivy_contracts::json::canonical;
use ivy_secretary::{
    config,
    triage_schema::{self, TriageSchema},
};
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
                ("outcome", error.outcome.as_str().into()),
            ]),
        )]),
    }
}
fn schema(name: &str) -> TriageSchema {
    [
        TriageSchema::Settings,
        TriageSchema::Input,
        TriageSchema::MediaAttachment,
        TriageSchema::Media,
        TriageSchema::Binding,
        TriageSchema::Plan,
        TriageSchema::Thread,
        TriageSchema::Work,
        TriageSchema::Call,
        TriageSchema::Completion,
    ]
    .into_iter()
    .find(|s| s.name() == name)
    .expect("Original triage schema name")
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
    let mut command = Command::new("node");
    command
        .arg("tests/secretary-rust/config-oracle.mjs")
        .current_dir(root)
        .stderr(Stdio::inherit());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command
        .output()
        .expect("Original configuration/main oracle required");
    assert!(output.status.success());
    let corpus = parse(&String::from_utf8(output.stdout).unwrap(), 8 * 1024 * 1024).unwrap();
    assert_eq!(
        corpus.required("platform").unwrap(),
        &IvyJson::from(if cfg!(windows) {
            "win32"
        } else if cfg!(target_os = "macos") {
            "darwin"
        } else {
            "linux"
        })
    );
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 146);
    let directory = Directory(std::env::temp_dir().join(format!(
        "secretary-config-consumer-{}",
        uuid::Uuid::new_v4()
    )));
    std::fs::create_dir(&directory.0).unwrap();
    let file = directory.0.join("config.json");
    for (index, row) in rows.iter().enumerate() {
        let input = row.required("input").unwrap();
        let action = input.required("action").unwrap().as_str().unwrap();
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
            "triage" => config::triage_configuration(input.required("value").unwrap()),
            "bundle" => {
                triage_schema::bundled(schema(&input.required("name").unwrap().as_str().unwrap()))
            }
            "path" => config::service_configuration_path(
                input.required("dataRoot").unwrap().as_js_string().unwrap(),
            )
            .map(IvyJson::String),
            "startup" => config::startup_settings(input.required("value").unwrap()).cloned(),
            "envelopeFile" | "deliveryFile" | "triageFile" => {
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
                    _ => config::load_triage_configuration(&file)
                        .await
                        .map(|v| v.unwrap_or(IvyJson::Null)),
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
