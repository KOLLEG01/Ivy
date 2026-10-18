use ivy_contracts::{
    IvyJson, IvyResult,
    checked_native_contract::{CheckedNativeContracts, native_versions},
    json::parse,
};
use std::{path::PathBuf, sync::Arc};
fn capture(result: IvyResult<IvyJson>) -> IvyJson {
    match result {
        Ok(value) => IvyJson::object([("ok", true.into()), ("value", value)]),
        Err(e) => IvyJson::object([("ok", false.into()), ("error", e.to_wire())]),
    }
}
#[test]
fn retained_filesystem_selection_matches_original_typescript() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/checked-native-contract-oracle.mjs",
        ])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let suite: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        IvyJson::Array(
            native_versions()
                .unwrap()
                .iter()
                .cloned()
                .map(IvyJson::String)
                .collect()
        ),
        parse(suite["versions"].as_str().unwrap(), usize::MAX).unwrap()
    );
    let root = std::env::temp_dir().join(format!(
        "ivy-checked-rust-{}-{}",
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
    let mut count = 0;
    for (index, row) in suite["rows"].as_array().unwrap().iter().enumerate() {
        let directory = root.join(index.to_string());
        std::fs::create_dir(&directory).unwrap();
        let store = CheckedNativeContracts::new(&directory);
        let mut identities = Vec::new();
        for op in row["ops"].as_array().unwrap() {
            let path = op["path"].as_str().map(|p| directory.join(p));
            match op["kind"].as_str().unwrap() {
                "write" => {
                    let path = path.unwrap();
                    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                    std::fs::write(
                        path,
                        op["bytes"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|v| v.as_u64().unwrap() as u8)
                            .collect::<Vec<_>>(),
                    )
                    .unwrap();
                }
                "copy" => {
                    let path = path.unwrap();
                    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                    std::fs::copy(repo.join(op["source"].as_str().unwrap()), path).unwrap();
                }
                "mkdir" => std::fs::create_dir_all(path.unwrap()).unwrap(),
                "remove" => {
                    let path = path.unwrap();
                    if path.is_dir() {
                        std::fs::remove_dir_all(path).unwrap();
                    } else if path.exists() {
                        std::fs::remove_file(path).unwrap();
                    }
                }
                "read" => {
                    let version = parse(op["version"].as_str().unwrap(), usize::MAX)
                        .unwrap()
                        .into_js_string()
                        .unwrap();
                    let identity = op["identity"]
                        .as_str()
                        .map(|v| parse(v, usize::MAX).unwrap());
                    let actual = store
                        .checked_native_contract(&version, identity.as_ref())
                        .map(|contract| {
                            let id = if let Some(index) =
                                identities.iter().position(|v| Arc::ptr_eq(v, &contract))
                            {
                                index + 1
                            } else {
                                identities.push(contract.clone());
                                identities.len()
                            };
                            let mut value = IvyJson::object([
                                ("id", IvyJson::Number(id as f64)),
                                ("catalogHash", contract.catalog_hash().into()),
                            ]);
                            for key in ["version", "sourceHash", "nativeExecutableHash"] {
                                value.insert(key, contract.catalog().get(key).unwrap().clone());
                            }
                            value.insert(
                                "definitions",
                                IvyJson::Array(
                                    contract
                                        .definitions()
                                        .into_iter()
                                        .map(|(name, _)| IvyJson::String(name))
                                        .collect(),
                                ),
                            );
                            value
                        });
                    assert_eq!(
                        capture(actual),
                        parse(op["expected"].as_str().unwrap(), usize::MAX).unwrap(),
                        "{}: {op}",
                        row["label"]
                    );
                    count += 1;
                }
                _ => unreachable!(),
            }
        }
    }
    assert!(count > 70);
}
