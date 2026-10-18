use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
};
use ivy_host_runtime::build_identity::build_identity;
use std::{path::PathBuf, process::Command};

#[test]
#[ignore = "isolated environment entrypoint"]
fn identity_child() {
    let root = PathBuf::from(std::env::var_os("IVY_IDENTITY_TEST_ROOT").unwrap());
    let result = match build_identity(&root) {
        Ok(value) => IvyJson::object([("ok", true.into()), ("value", value.json())]),
        Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
    };
    println!(
        "IDENTITY_RESULT={}",
        stringify(&result, usize::MAX).unwrap()
    );
}

#[test]
fn build_identity_matches_original_filesystem_and_metadata_boundaries() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let oracle = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/build-identity-oracle.mjs",
        ])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        oracle.status.success(),
        "{}",
        String::from_utf8_lossy(&oracle.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&oracle.stdout).unwrap();
    assert!(rows.len() > 60);
    let root = std::env::temp_dir().join(format!("ivy-build-identity-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    let _cleanup = Cleanup(root.clone());
    let mut generated = std::collections::HashSet::new();
    for (index, row) in rows.iter().enumerate() {
        let directory = root.join(index.to_string());
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("package.json");
        match row["shape"].as_str() {
            Some("missing") => {}
            Some("directory") => std::fs::create_dir(&path).unwrap(),
            Some(shape) => {
                let body = match shape {
                    "large" => format!(
                        "{{\"version\":\"x\",\"unused\":\"{}\"}}",
                        "x".repeat(1048577)
                    ),
                    "deep" => format!(
                        "{{\"version\":\"x\",\"unused\":{}0{}}}",
                        "[".repeat(256),
                        "]".repeat(256)
                    ),
                    "wide" => format!(
                        "{{\"version\":\"x\",\"unused\":[0{}]}}",
                        ",0".repeat(400000)
                    ),
                    _ => unreachable!(),
                };
                std::fs::write(&path, body).unwrap();
            }
            None => {
                let bytes: Vec<u8> = row["bytes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_u64().unwrap() as u8)
                    .collect();
                std::fs::write(&path, bytes).unwrap();
            }
        }
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--ignored", "--exact", "identity_child", "--nocapture"])
            .env("IVY_IDENTITY_TEST_ROOT", &directory);
        if let Some(id) = row["supplied"].as_str() {
            command.env("IVY_BUILD_ID", id);
        } else {
            command.env_remove("IVY_BUILD_ID");
        }
        let child = command.output().unwrap();
        assert!(
            child.status.success(),
            "{}: {}",
            row["label"],
            String::from_utf8_lossy(&child.stderr)
        );
        let stdout = String::from_utf8(child.stdout).unwrap();
        let text = stdout
            .lines()
            .find_map(|s| s.strip_prefix("IDENTITY_RESULT="))
            .unwrap();
        let mut actual = parse(text, usize::MAX).unwrap();
        if row["opaque"] == true && actual.get("ok") == Some(&true.into()) {
            let mut value = actual.remove("value").unwrap();
            let id = value.get("buildId").unwrap().as_str().unwrap();
            assert!(
                id.len() == 71
                    && id.starts_with("sha256:")
                    && id[7..]
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            );
            assert!(generated.insert(id));
            value.insert("buildId", "<opaque>".into());
            actual.insert("value", value);
        }
        let expected = parse(row["result"].as_str().unwrap(), usize::MAX).unwrap();
        assert_eq!(
            actual, expected,
            "{} supplied={}",
            row["label"], row["supplied"]
        );
    }
}
