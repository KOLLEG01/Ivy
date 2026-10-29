use ivy_contracts::{
    IvyJson, IvyResult,
    checked_native_contract::{CheckedNativeContracts, native_versions},
    hash::hash_json,
    json::{parse, stringify},
    native_contract::NativeContract,
};
use std::{path::PathBuf, sync::Arc};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "ivy-checked-rust-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let path = std::fs::canonicalize(&self.0).unwrap();
        let parent = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        assert_eq!(path.parent(), Some(parent.as_path()));
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ivy-checked-rust-")
        );
        std::fs::remove_dir_all(path).unwrap();
    }
}
fn capture(result: IvyResult<IvyJson>) -> IvyJson {
    match result {
        Ok(value) => IvyJson::object([("ok", true.into()), ("value", value)]),
        Err(e) => IvyJson::object([("ok", false.into()), ("error", e.to_wire())]),
    }
}

#[test]
fn claude_catalog_keeps_saved_plan_identity_and_selects_rust_sdk_by_default() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let catalogs = CheckedNativeContracts::new(repo);
    let version = "0.142.3".into();
    let rust_identity = IvyJson::object([
        (
            "sourceHash",
            "sha256:0430db0d2378063bc34323ff6b9a6b36aa6511da44d853f5bd2cae3416a4715f".into(),
        ),
        (
            "nativeExecutableHash",
            "sha256:d13d3e2a03135e08f7db3bd2509c086dab0968549834c0c46df03af3002e65a3".into(),
        ),
    ]);
    let saved_identity = IvyJson::object([
        (
            "sourceHash",
            "sha256:c55467a1bc4b9a8df8287db12568238351e8782580e28e3d8ddfa38264a3fe44".into(),
        ),
        (
            "nativeExecutableHash",
            "sha256:d2bbd218eeceea5c93b60aa6220944a4cc299aa6b78f5170259d98075a265e69".into(),
        ),
    ]);
    let current = catalogs.checked_native_contract(&version, None).unwrap();
    let rust = catalogs
        .checked_native_contract(&version, Some(&rust_identity))
        .unwrap();
    let saved = catalogs
        .checked_native_contract(&version, Some(&saved_identity))
        .unwrap();
    assert_eq!(current.catalog_hash(), rust.catalog_hash());
    assert_ne!(current.catalog_hash(), saved.catalog_hash());
    // Saved TaskBoard plans pin the source hash even after the live owner changes.
    let saved_plan = IvyJson::object([(
        "sourceHash",
        saved_identity.get("sourceHash").unwrap().clone(),
    )]);
    assert_eq!(
        catalogs
            .checked_native_contract(&version, Some(&saved_plan))
            .unwrap()
            .catalog_hash(),
        saved.catalog_hash()
    );
}

#[test]
fn candidate_reuse_keeps_file_changes_errors_and_failed_selections_fresh() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut catalog = parse(
        &std::fs::read_to_string(repo.join("specs/native/codex-0.159.2/catalog.json")).unwrap(),
        usize::MAX,
    )
    .unwrap();
    let mut method = catalog.get("clientRequests").unwrap().as_array().unwrap()[0].clone();
    method.insert("inputSchema", true.into());
    method.insert("outputSchema", true.into());
    catalog.insert("clientRequests", IvyJson::Array(vec![method.clone()]));
    for key in [
        "serverRequests",
        "serverNotifications",
        "clientNotifications",
        "derivedLegacyTypes",
    ] {
        catalog.insert(key, IvyJson::Array(vec![]));
    }
    let identity = |letter: char| {
        IvyJson::object([(
            "sourceHash",
            format!("sha256:{}", letter.to_string().repeat(64)).into(),
        )])
    };
    catalog.insert(
        "sourceHash",
        identity('a').get("sourceHash").unwrap().clone(),
    );
    let version = catalog
        .get("version")
        .unwrap()
        .as_js_string()
        .unwrap()
        .clone();
    let root = Directory::new();
    let directory = root
        .0
        .join("specs/native")
        .join(format!("codex-{}", version.to_utf8().unwrap()));
    std::fs::create_dir_all(&directory).unwrap();
    let first = directory.join("catalog.aaa-x64.json");
    let write = |path: &std::path::Path, value: &IvyJson| {
        std::fs::write(path, stringify(value, usize::MAX).unwrap()).unwrap()
    };
    write(&first, &catalog);
    let mut last = catalog.clone();
    last.insert(
        "sourceHash",
        identity('b').get("sourceHash").unwrap().clone(),
    );
    write(&directory.join("catalog.json"), &last);
    let store = CheckedNativeContracts::new(&root.0);
    let missing = identity('0');
    let error = || {
        store
            .checked_native_contract(&version, Some(&missing))
            .err()
            .expect("Selection must fail")
    };
    assert_eq!(error().code, "native_catalog_mismatch");
    assert_eq!(error().code, "native_catalog_mismatch");

    let selected = store
        .checked_native_contract(&version, Some(&identity('a')))
        .unwrap();
    let by_hash = IvyJson::object([("catalogHash", hash_json(&catalog).unwrap().into())]);
    let shared = store
        .checked_native_contract(&version, Some(&by_hash))
        .unwrap();
    // Reuse the validated candidate across selector cache keys.
    assert!(Arc::ptr_eq(&selected, &shared));

    let platform = match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        other => other,
    };
    let architecture = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    write(&directory.join(format!("catalog.{platform}-{architecture}.json")), &catalog);
    let default = store.checked_native_contract(&version, None).unwrap();
    assert!(Arc::ptr_eq(&selected, &default));

    std::fs::write(&first, "{").unwrap();
    assert_eq!(error().code, "internal_error");
    assert!(Arc::ptr_eq(
        &selected,
        &store
            .checked_native_contract(&version, Some(&identity('a')))
            .unwrap()
    ));
    let mut duplicate = catalog.clone();
    duplicate.insert(
        "clientRequests",
        IvyJson::Array(vec![method.clone(), method]),
    );
    write(&first, &duplicate);
    assert_eq!(
        error().to_wire(),
        NativeContract::new(&duplicate).err().unwrap().to_wire()
    );
    let mut invalid = catalog.clone();
    invalid.insert("sourceHash", false.into());
    write(&first, &invalid);
    assert_eq!(error().code, "invalid_arguments");

    let original_length = stringify(&catalog, usize::MAX).unwrap().len();
    catalog.insert(
        "sourceHash",
        identity('c').get("sourceHash").unwrap().clone(),
    );
    assert_eq!(
        stringify(&catalog, usize::MAX).unwrap().len(),
        original_length
    );
    write(&first, &catalog);
    let changed = store
        .checked_native_contract(&version, Some(&identity('c')))
        .unwrap();
    assert_eq!(changed.catalog_hash(), hash_json(&catalog).unwrap());
    assert!(!Arc::ptr_eq(&selected, &changed));

    std::fs::remove_file(&first).unwrap();
    std::fs::create_dir(&first).unwrap();
    assert_eq!(error().code, "internal_error");
    std::fs::remove_dir(&first).unwrap();
    catalog.insert("sourceHash", missing.get("sourceHash").unwrap().clone());
    write(&first, &catalog);
    let repaired = store
        .checked_native_contract(&version, Some(&missing))
        .unwrap();
    assert_eq!(repaired.catalog_hash(), hash_json(&catalog).unwrap());
    // A previously accepted selector keeps its immutable snapshot after repair.
    assert!(Arc::ptr_eq(
        &selected,
        &store
            .checked_native_contract(&version, Some(&identity('a')))
            .unwrap()
    ));
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
    let root = Directory::new();
    let mut count = 0;
    for (index, row) in suite["rows"].as_array().unwrap().iter().enumerate() {
        let directory = root.0.join(index.to_string());
        std::fs::create_dir(&directory).unwrap();
        let store = CheckedNativeContracts::new(&directory);
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
                            let mut value =
                                IvyJson::object([("catalogHash", contract.catalog_hash().into())]);
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
