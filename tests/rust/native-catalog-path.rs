use ivy_contracts::{IvyJson, JsString, checked_native_contract::native_catalog_path, json::parse};
use std::path::PathBuf;

#[test]
fn catalog_paths_match_original_typescript() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let root = std::env::temp_dir().join(format!(
        "ivy-catalog-path-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    let _cleanup = Cleanup(root.clone());
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/native-catalog-path-oracle.mjs",
        ])
        .arg(&root)
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let rows = rows.as_array().unwrap();
    assert!(rows.len() > 500);
    for row in rows {
        let args: Vec<Option<JsString>> = row["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                v.as_str()
                    .map(|s| parse(s, usize::MAX).unwrap().into_js_string().unwrap())
            })
            .collect();
        let actual = match native_catalog_path(
            args[0].as_ref().unwrap(),
            args[1].as_ref().unwrap(),
            args[2].as_ref(),
            args[3].as_ref(),
        ) {
            Ok(value) => IvyJson::object([("ok", true.into()), ("value", IvyJson::String(value))]),
            Err(e) => IvyJson::object([("ok", false.into()), ("error", e.to_wire())]),
        };
        assert_eq!(
            actual,
            parse(row["expected"].as_str().unwrap(), usize::MAX).unwrap(),
            "{row}"
        );
    }
}
