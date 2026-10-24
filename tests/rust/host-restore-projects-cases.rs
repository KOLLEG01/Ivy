use super::target_cases::{Entry, instance, instances, jp, property, state};
use super::*;
use ivy_contracts::{core_schema::validate_host, hash::hash_json};
use ivy_host_runtime::config;
use std::time::Duration;

#[tokio::test]
async fn project_restore_relocates_only_the_disposable_backup_index() {
    let mut cases = vec![
        "valid",
        "malformed-index",
        "unicode-dot-data",
        "canonical-data",
        "linked-data",
        "missing-index",
        "missing-prior",
        "missing-before-root",
        "missing-after-root",
        "nonstring-member",
        "oversized-index",
        "index-directory",
        "index-link",
        "retained-priority",
        "foreign-agent-prefix",
        "invalid-retained-identity",
        "identity-before-array",
        "dropped-observer",
    ];
    #[cfg(windows)]
    cases.extend(["opaque-data-high", "opaque-data-low"]);
    for case in cases {
        let f = ServiceFixture::new();
        let root = f.0.join("restore");
        fs::create_dir_all(&root).unwrap();
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
                fs::create_dir_all(&alias).unwrap();
                fs::write(alias.join("untouched"), b"unrelated native alias").unwrap();
            }
        }
        fs::create_dir_all(&data).unwrap();
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
        let old_projects = root.join("old-projects");
        let new_projects = root.join("new-projects");
        let retained = if case == "retained-priority" {
            old_projects.join("retained")
        } else {
            root.join("retained")
        };
        fs::create_dir_all(old_projects.join("one")).unwrap();
        fs::create_dir_all(retained.join("nested")).unwrap();
        fs::write(
            old_projects.join("one/plan.json"),
            b"saved project plan authority",
        )
        .unwrap();
        fs::write(
            retained.join("nested/assignment.json"),
            b"saved Hive assignment authority",
        )
        .unwrap();
        fs::write(root.join("never-execute"), b"not an executable").unwrap();
        let mut agent = instance(
            "agent",
            "agent-manager",
            J::object([("internalProjectRoot", jp(&new_projects))]),
        );
        agent.insert("enabled", false.into());
        agent.insert(
            "paths",
            J::object([
                ("data", jp(&configured_data)),
                ("work", jp(&root.join("work"))),
                ("logs", jp(&root.join("logs"))),
            ]),
        );
        let mut other = instance("fixture", "fixture", J::object(Vec::<(String, J)>::new()));
        other.insert(
            "paths",
            J::object([
                ("data", jp(&root.join("other-data"))),
                ("work", jp(&root.join("other-work"))),
                ("logs", jp(&root.join("other-logs"))),
            ]),
        );
        let mut configuration = J::object([
            ("schemaVersion", J::Number(1.0)),
            ("hostId", "restore-fixture".into()),
            ("runtimeRoot", jp(&root.join("state"))),
            ("artifactRoot", jp(&root.join("artifacts"))),
            ("stagingRoot", jp(&root.join("staging"))),
            ("publicBaseUrl", "http://127.0.0.1:38081/ivy".into()),
            (
                "executables",
                J::object([("node", jp(&root.join("never-execute")))]),
            ),
            (
                "instances",
                J::Array(vec![
                    agent,
                    other,
                    instance(
                        "agent-other",
                        "agent-manager",
                        J::object([("internalProjectRoot", jp(&root.join("secondary-projects")))]),
                    ),
                ]),
            ),
        ]);
        validate_host("HostConfig", &configuration).unwrap();
        let mut original = configuration.clone();
        property(&mut instances(&mut original)[0], "settings")
            .insert("internalProjectRoot", jp(&old_projects));
        if ["missing-index", "missing-prior"].contains(&case) {
            instances(&mut original).remove(0);
        } else if case == "missing-before-root" {
            property(&mut instances(&mut original)[0], "settings").remove("internalProjectRoot");
        } else if case == "missing-after-root" {
            property(&mut instances(&mut configuration)[0], "settings")
                .remove("internalProjectRoot");
        }
        let retained_hash = hash_json(&jp(&retained)).unwrap()[7..].to_owned();
        let retained_owner = if case == "foreign-agent-prefix" {
            "agent-other"
        } else {
            "agent"
        };
        let mut captured = vec![StorageArea {
            key: format!("retained-projects/{retained_owner}/{retained_hash}").into(),
            path: node_path::from_native(&retained),
        }];
        if [
            "invalid-retained-identity",
            "identity-before-array",
            "missing-index",
        ]
        .contains(&case)
        {
            captured[0].key = "retained-projects/agent/invalid".into();
        }
        let paths = J::Array(vec![
            jp(&old_projects.join("one")),
            jp(&retained.join("nested")),
            jp(&root.join("unrelated")),
            jp(&old_projects),
            jp(&retained),
            jp(&old_projects.join("one")),
        ]);
        let restored_retained = new_projects.join(format!("restored-{retained_hash}"));
        let expected_paths = J::Array(vec![
            jp(&new_projects.join("one")),
            jp(&if case == "foreign-agent-prefix" {
                retained.join("nested")
            } else {
                restored_retained.join("nested")
            }),
            jp(&root.join("unrelated")),
            jp(&new_projects),
            jp(&if case == "foreign-agent-prefix" {
                retained.clone()
            } else {
                restored_retained
            }),
            jp(&new_projects.join("one")),
        ]);
        let input = match case {
            "malformed-index" | "identity-before-array" => J::object(Vec::<(String, J)>::new()),
            "nonstring-member" => {
                J::Array(vec![paths.as_array().unwrap()[0].clone(), J::Number(1.0)])
            }
            _ => paths,
        };
        let before = format!(" \n{}\n ", json::canonical(&input, usize::MAX).unwrap()).into_bytes();
        let index = data.join("project-backup-paths.json");
        if case != "missing-index" {
            if case == "index-directory" {
                fs::create_dir(&index).unwrap();
            } else if case == "oversized-index" {
                fs::write(&index, vec![b' '; config::CONFIGURATION_BYTES + 1]).unwrap();
            } else {
                fs::write(&index, &before).unwrap();
            }
        }
        if case == "index-link" {
            let linked = root.join("linked-index.json");
            fs::rename(&index, &linked).unwrap();
            #[cfg(windows)]
            std::os::windows::fs::symlink_file(&linked, &index).unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink(&linked, &index).unwrap();
        }
        fs::create_dir_all(root.join("source")).unwrap();
        fs::create_dir_all(root.join("state/recovery-original")).unwrap();
        fs::create_dir_all(root.join("other-data")).unwrap();
        fs::write(root.join("source/project-backup-paths.json"), &before).unwrap();
        fs::write(
            root.join("state/recovery-original/project-backup-paths.json"),
            &before,
        )
        .unwrap();
        fs::write(
            root.join("other-data/project-backup-paths.json"),
            b"not an agent index",
        )
        .unwrap();
        config::atomic_json(root.join("source/configuration.json"), original.clone())
            .await
            .unwrap();
        let host = HostContext::current().unwrap();
        let storage = Arc::new(StorageContext {
            paths: host.paths,
            account_home: host.account_home,
            compare: Arc::new(|a, b| a.cmp(b)),
        });
        let mut expected = state(&root);
        let mut action = Box::pin(restore::relocate_projects(
            original,
            configuration,
            captured,
            storage,
        ));
        let result = if case == "dropped-observer" {
            use std::task::Poll;
            let expected_json = json::canonical(&expected_paths, usize::MAX).unwrap();
            std::future::poll_fn(|cx| {
                assert!(action.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            drop(action);
            tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    if let Ok(value) =
                        config::json_file(index.clone(), config::CONFIGURATION_BYTES).await
                    {
                        if json::canonical(&value, usize::MAX).unwrap() == expected_json {
                            break;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
            .await
            .expect("admitted project relocation must finish after observer is dropped");
            None
        } else {
            Some(
                tokio::time::timeout(Duration::from_secs(30), action)
                    .await
                    .expect("project relocation must terminate"),
            )
        };
        let error = match case {
            "malformed-index" | "missing-prior" | "nonstring-member" => Some("internal_error"),
            "missing-before-root"
            | "missing-after-root"
            | "invalid-retained-identity"
            | "identity-before-array" => Some("backup_invalid"),
            "oversized-index" | "index-directory" => Some("invalid_arguments"),
            _ => None,
        };
        if let Some(result) = result {
            if let Some(code) = error {
                assert_eq!(result.unwrap_err().code, code, "{case}");
            } else {
                result.unwrap_or_else(|error| panic!("{case}: {}", error.code));
            }
        }
        if error.is_none() && case != "missing-index" {
            let after = format!(
                "{}\n",
                json::canonical(&expected_paths, usize::MAX).unwrap()
            )
            .into_bytes();
            assert_eq!(
                fs::read(&index).unwrap(),
                after,
                "{case}: exact ordered project index"
            );
            expected.insert(
                index.strip_prefix(&root).unwrap().to_owned(),
                Entry::File(after),
            );
        }
        let actual = state(&root);
        assert_eq!(
            actual.keys().collect::<Vec<_>>(),
            expected.keys().collect::<Vec<_>>(),
            "{case}: exact retained layout"
        );
        for (path, entry) in &expected {
            assert!(
                actual.get(path) == Some(entry),
                "{case}: changed protected entry {path:?}"
            );
        }
    }
}
