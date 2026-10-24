use super::*;
use std::collections::BTreeMap;

#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Directory,
    File(Vec<u8>),
    Link(PathBuf),
}
fn image(root: &Path) -> BTreeMap<PathBuf, Entry> {
    let mut result = BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    while let Some(path) = pending.pop() {
        let info = fs::symlink_metadata(&path).unwrap();
        let entry = if info.file_type().is_symlink() {
            Entry::Link(fs::read_link(&path).unwrap())
        } else if info.is_file() {
            Entry::File(fs::read(&path).unwrap())
        } else {
            pending.extend(fs::read_dir(&path).unwrap().map(|v| v.unwrap().path()));
            Entry::Directory
        };
        result.insert(path.strip_prefix(root).unwrap().to_owned(), entry);
    }
    result
}
fn area(key: impl Into<JsString>, path: &Path) -> StorageArea {
    StorageArea {
        key: key.into(),
        path: node_path::from_native(path),
    }
}

#[tokio::test]
async fn retained_projects_follow_cached_owned_inventory_and_restored_identity() {
    let mut cases = vec![
        "null",
        "relative",
        "valid",
        "empty",
        "missing",
        "nonstring",
        "limit",
        "too-many",
        "syntax",
        "oversized",
        "directory",
        "linked-index",
        "unicode-dot-data",
        "canonical-data",
        "linked-data",
        "hash-text-identity",
        "locale-order",
        "ordered-refusal",
    ];
    #[cfg(windows)]
    cases.extend(["opaque-data-high", "opaque-data-low"]);
    for case in cases {
        let f = ServiceFixture::new();
        let root = f.0.join("inventory");
        fs::create_dir(&root).unwrap();
        #[cfg(windows)]
        let root = if case.starts_with("opaque-data") {
            fs::canonicalize(&root).unwrap()
        } else {
            root
        };
        let mut data = root.join("data");
        if case == "unicode-dot-data" {
            data = root.join("data-漢字 space");
        }
        #[cfg(windows)]
        if case.starts_with("opaque-data") {
            use std::{ffi::OsString, os::windows::ffi::OsStringExt};
            let mut name: Vec<u16> = "data".encode_utf16().collect();
            name.push(if case.ends_with("high") {
                0xd800
            } else {
                0xdfff
            });
            data = root.join(OsString::from_wide(&name));
            for name in ["data�", "data. "] {
                let alias = root.join(name);
                fs::create_dir(&alias).unwrap();
                fs::write(
                    alias.join("project-backup-paths.json"),
                    b"unrelated native alias",
                )
                .unwrap();
            }
        }
        fs::create_dir(&data).unwrap();
        let mut configured_data = data.clone();
        if case == "unicode-dot-data" {
            configured_data = root.join(".").join(data.file_name().unwrap());
        } else if case == "canonical-data" {
            configured_data = fs::canonicalize(&data).unwrap();
        } else if case == "linked-data" {
            configured_data = root.join("linked-data");
            #[cfg(windows)]
            std::os::windows::fs::symlink_dir(&data, &configured_data).unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink(&data, &configured_data).unwrap();
        }
        let runtime = root.join("state");
        let internal = root.join("internal");
        let mut configuration = host_configuration(&f);
        configuration.insert("runtimeRoot", jp(&runtime));
        configuration.insert("artifactRoot", jp(&root.join("artifacts")));
        configuration.insert("stagingRoot", jp(&root.join("staging")));
        let mut agent = configuration
            .required("instances")
            .unwrap()
            .as_array()
            .unwrap()[0]
            .clone();
        agent.insert("componentId", "agent-manager".into());
        agent.insert(
            "settings",
            J::object([("internalProjectRoot", jp(&internal))]),
        );
        agent.insert(
            "paths",
            J::object([
                ("data", jp(&configured_data)),
                ("work", jp(&runtime)),
                ("logs", jp(&runtime)),
            ]),
        );
        let mut ctx = context(&root);
        let mut base = vec![
            area("state", &runtime),
            area("services/fixture/data", &configured_data),
            area("projects/fixture", &internal),
        ];
        let ordered = ["locale-order", "ordered-refusal"].contains(&case);
        if ordered {
            let mut second = agent.clone();
            second.insert("instanceId", "a".into());
            second.insert("serviceNodeId", "a-node".into());
            let second_data = root.join("second-data");
            second.insert(
                "paths",
                J::object([
                    ("data", jp(&second_data)),
                    ("work", jp(&runtime)),
                    ("logs", jp(&runtime)),
                ]),
            );
            configuration.insert("instances", J::Array(vec![second, agent]));
            ctx.compare = Arc::new(|a, b| b.cmp(a));
            base.push(area("services/a/data", &second_data));
            fs::create_dir(&second_data).unwrap();
            fs::write(
                second_data.join("project-backup-paths.json"),
                if case == "ordered-refusal" {
                    b"{".to_vec()
                } else {
                    json::canonical(&J::Array(vec![jp(&root.join("old-internal"))]), usize::MAX)
                        .unwrap()
                        .into_bytes()
                },
            )
            .unwrap();
        } else {
            configuration.insert("instances", J::Array(vec![agent]));
        }
        let mut instances = configuration
            .required("instances")
            .unwrap()
            .as_array()
            .unwrap()
            .to_vec();
        let mut unrelated = instances[0].clone();
        unrelated.insert("instanceId", "other".into());
        unrelated.insert("serviceNodeId", "other-node".into());
        unrelated.insert("componentId", "extension".into());
        unrelated.insert("settings", J::object(Vec::<(String, J)>::new()));
        unrelated.insert(
            "paths",
            J::object([
                ("data", jp(&runtime)),
                ("work", jp(&runtime)),
                ("logs", jp(&runtime)),
            ]),
        );
        instances.push(unrelated);
        configuration.insert("instances", J::Array(instances));
        validate_host("HostConfig", &configuration).unwrap();
        fs::create_dir(&runtime).unwrap();
        fs::write(
            runtime.join("project-backup-paths.json"),
            b"not an agent cache",
        )
        .unwrap();
        let outside = if case == "hash-text-identity" {
            root.join("route").join("..").join("old-internal")
        } else {
            root.join("old-internal")
        };
        let input = match case {
            "null" => J::Null,
            "relative" | "ordered-refusal" => J::Array(vec!["relative".into()]),
            "empty" => J::Array(vec![]),
            "nonstring" => J::Array(vec![jp(&outside), J::Number(1.0)]),
            "limit" | "too-many" => {
                J::Array(vec![jp(&outside); if case == "limit" { 256 } else { 257 }])
            }
            _ => J::Array(vec![
                jp(&runtime.join("owned")),
                jp(&outside),
                jp(&outside.join("child")),
                jp(&outside),
            ]),
        };
        let cache = data.join("project-backup-paths.json");
        if case == "directory" {
            fs::create_dir(&cache).unwrap();
        } else if case != "missing" {
            let bytes = match case {
                "syntax" => b"{".to_vec(),
                "oversized" => vec![b' '; config::CONFIGURATION_BYTES + 1],
                _ => format!(" \n{}\n ", json::canonical(&input, usize::MAX).unwrap()).into_bytes(),
            };
            fs::write(&cache, bytes).unwrap();
        }
        if case == "linked-index" {
            let linked = root.join("linked-index.json");
            fs::rename(&cache, &linked).unwrap();
            #[cfg(windows)]
            std::os::windows::fs::symlink_file(&linked, &cache).unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink(&linked, &cache).unwrap();
        }
        fs::write(
            root.join("saved-plan.json"),
            b"original project plan authority",
        )
        .unwrap();
        fs::write(
            root.join("assignment.json"),
            b"original assignment authority",
        )
        .unwrap();
        config::atomic_json(root.join("configuration.json"), configuration.clone())
            .await
            .unwrap();
        let before = image(&root);
        let result = project_storage::project_storage_areas(&configuration, &ctx).await;
        let error = match case {
            "null" | "relative" | "nonstring" | "too-many" | "ordered-refusal" => {
                Some("backup_invalid")
            }
            "syntax" => Some("internal_error"),
            "oversized" | "directory" => Some("invalid_arguments"),
            _ => None,
        };
        if let Some(code) = error {
            assert_eq!(result.unwrap_err().code, code, "{case}");
        } else {
            let mut expected = base.clone();
            if !["empty", "missing"].contains(&case) {
                expected.push(area(
                    format!(
                        "retained-projects/fixture/{}",
                        &hash_json(&jp(&outside)).unwrap()[7..]
                    ),
                    &outside,
                ));
            }
            assert_eq!(
                result.unwrap(),
                expected,
                "{case}: exact ordered storage keys and public paths"
            );
        }
        if case == "valid" {
            let old = root.join("old");
            let key = format!(
                "retained-projects/fixture/{}",
                &hash_json(&jp(&old)).unwrap()[7..]
            );
            let mut captured = base.clone();
            captured.push(area(key.clone(), &old));
            let mut expected = base;
            expected.push(area(
                key.clone(),
                &internal.join(format!(
                    "restored-{}",
                    &key["retained-projects/fixture/".len()..]
                )),
            ));
            assert_eq!(
                project_storage::restored_storage_areas(&configuration, &captured, &ctx).unwrap(),
                expected
            );
        }
        let after = image(&root);
        assert_eq!(
            after.keys().collect::<Vec<_>>(),
            before.keys().collect::<Vec<_>>(),
            "{case}: exact retained layout"
        );
        for (path, entry) in &before {
            assert!(
                after.get(path) == Some(entry),
                "{case}: changed protected entry {path:?}"
            );
        }
    }
}
