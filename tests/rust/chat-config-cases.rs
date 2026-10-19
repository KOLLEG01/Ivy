use ivy_chat_bridge::whatsapp::config::read_whatsapp_config;
use ivy_contracts::{
    IvyJson as Json,
    json::{js_string, object_entries, parse, stringify},
};
use std::{path::Path, process::Command};

fn describe(value: &Json) -> Json {
    let (kind, body) = match value {
        Json::Null => return Json::Array(vec!["null".into()]),
        Json::Number(number) => (
            "number",
            if *number == 0.0 && number.is_sign_negative() {
                "-0".into()
            } else {
                Json::String(js_string(value))
            },
        ),
        Json::String(_) => ("string", value.clone()),
        Json::Bool(_) => ("boolean", value.clone()),
        Json::Array(values) => ("array", Json::Array(values.iter().map(describe).collect())),
        Json::Object(_) => (
            "object",
            Json::Array(
                object_entries(value)
                    .into_iter()
                    .map(|(key, value)| {
                        Json::Array(vec![Json::String(key.clone()), describe(value)])
                    })
                    .collect(),
            ),
        ),
    };
    Json::Array(vec![kind.into(), body])
}

#[tokio::test]
async fn protected_configuration_matches_original_reader_on_the_same_files() {
    let output = Command::new("node")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .arg("tests/rust/fixtures/chat-whatsapp-config-oracle.mjs")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let oracle = parse(&String::from_utf8(output.stdout).unwrap(), usize::MAX).unwrap();
    let root = std::path::PathBuf::from(oracle.required("root").unwrap().as_str().unwrap());
    let canonical_root = root.canonicalize().unwrap();
    let canonical_temp = std::env::temp_dir().canonicalize().unwrap();
    assert!(canonical_root.starts_with(&canonical_temp) && canonical_root != canonical_temp);
    assert!(
        root.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("ivy-chat-config-")
    );
    let Json::Array(cases) = oracle.required("cases").unwrap() else {
        panic!("Expected original reader cases")
    };
    let mut actual = Vec::new();
    for row in cases {
        let Json::String(path) = row.required("path").unwrap() else {
            panic!("Expected UTF-16 path")
        };
        let result = match read_whatsapp_config(path, row.required("definition").unwrap()).await {
            Ok(value) => Json::object([("value", describe(&value))]),
            Err(error) => Json::object([("error", error.to_wire())]),
        };
        actual.push(Json::object([
            ("name", row.required("name").unwrap().clone()),
            ("actual", result),
        ]));
    }
    if let Some(path) = std::env::var_os("IVY_CHAT_CONFIG_ACTUAL_CAPTURE") {
        std::fs::write(
            path,
            stringify(&Json::Array(actual.clone()), usize::MAX).unwrap(),
        )
        .unwrap();
    }
    // Both readers have finished. Remove only this newly created, verified root.
    assert_eq!(root.canonicalize().unwrap(), canonical_root);
    std::fs::remove_dir_all(&root).unwrap();
    for (row, actual) in cases.iter().zip(&actual) {
        assert_eq!(
            actual.required("actual").unwrap(),
            row.required("expected").unwrap(),
            "{}",
            row.required("name").unwrap().as_str().unwrap()
        );
    }
    eprintln!("{} original configuration/file cases matched", cases.len());
}
