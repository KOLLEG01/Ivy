//! Fixed original outcomes and plain OS/SQLite interoperability observations.
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};
#[path = "reference-data.rs"]
mod reference_data;

fn cases() -> &'static Value {
    static CASES: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    CASES.get_or_init(|| {
        serde_json::from_slice(&reference_data::read(include_bytes!(
            "hive-storage-reference.json.gz"
        )))
        .unwrap()
    })
}
fn normalized(value: &Value, field: &str, aliases: &[(String, String)]) -> Value {
    match value {
        Value::String(text) => aliases
            .iter()
            .fold(text.clone(), |text, (path, alias)| {
                text.replace(path, alias)
            })
            .into(),
        Value::Array(units) if field == "units" => {
            let mut units = units
                .iter()
                .map(|value| value.as_u64().unwrap() as u16)
                .collect::<Vec<_>>();
            for (path, alias) in aliases {
                let prefix = path.encode_utf16().collect::<Vec<_>>();
                if units.starts_with(&prefix) {
                    units.splice(..prefix.len(), alias.encode_utf16());
                    break;
                }
            }
            json!(units)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|value| normalized(value, "", aliases))
                .collect(),
        ),
        Value::Object(fields) => serde_json::to_value(
            fields
                .iter()
                .map(|(key, value)| (key, normalized(value, key, aliases)))
                .collect::<BTreeMap<_, _>>(),
        )
        .unwrap(),
        value => value.clone(),
    }
}
pub fn reference(subject: &str, input: &Value, root: &Path) -> Value {
    let root = root.to_str().unwrap();
    let mut aliases = vec![
        (root.to_owned(), "$ROOT".to_owned()),
        (root.replace('\\', "/"), "$ROOT".to_owned()),
    ];
    if cfg!(windows) && root.as_bytes().get(1) == Some(&b':') {
        aliases.extend([
            (root[2..].to_owned(), "$DRIVE_ROOT".to_owned()),
            (root[2..].replace('\\', "/"), "$DRIVE_ROOT".to_owned()),
        ]);
    }
    aliases.sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));
    aliases.dedup();
    let key =
        ivy_contracts::hash::digest(serde_json::to_vec(&normalized(input, "", &aliases)).unwrap());
    cases()[std::env::consts::OS][subject]
        .get(&key)
        .unwrap_or_else(|| panic!("Missing original {subject} trace {key}"))
        .clone()
}
pub fn reset() -> &'static Value {
    static RESET: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    RESET.get_or_init(|| {
        serde_json::from_slice(&reference_data::read(include_bytes!(
            "hive-reset-reference.json.gz"
        )))
        .unwrap()
    })
}
pub fn materialize(image: &Value, path: &Path) {
    let bytes = STANDARD.decode(image["bytes"].as_str().unwrap()).unwrap();
    assert_eq!(ivy_contracts::hash::digest(&bytes), image["bytesHash"]);
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .unwrap();
    file.write_all(&bytes).unwrap();
}
pub fn sqlite(input: Value) -> Value {
    let mut command =
        Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../tests/fixtures/hive-storage-fixture.mjs"),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "SQLite fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
