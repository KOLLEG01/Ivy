use super::target_cases::{Entry, jp, state};
use super::*;
use ivy_contracts::{core_schema::validate_host, hash::hash_json};
use ivy_host_runtime::config;
use std::time::Duration;

fn captured_record(root: &Path, phase: &str) -> (String, J) {
    let definitions = J::Array(
        ["executor", "manager"]
            .into_iter()
            .map(|name| {
                J::object([
                    ("instanceId", name.into()),
                    ("componentId", format!("fixture-{name}").into()),
                    ("name", format!("fixture-{name}").into()),
                ])
            })
            .collect(),
    );
    let candidate = format!("sha256:{}", "a".repeat(64));
    let configuration_hash = format!("sha256:{}", "b".repeat(64));
    let request = J::object([
        ("schemaVersion", J::Number(1.0)),
        ("operationId", "restored-maintenance".into()),
        (
            "next",
            J::object([
                ("schemaVersion", J::Number(1.0)),
                ("hostId", "restore-fixture".into()),
                ("os", if cfg!(windows) { "win32" } else { "linux" }.into()),
                ("installationId", "restored-installation".into()),
                ("candidateId", candidate.clone().into()),
                ("artifactRoot", jp(&root.join("source-artifacts"))),
                ("runtimeRoot", jp(&root.join("source-state"))),
                ("configPath", jp(&root.join("source.json"))),
                ("configHash", configuration_hash.clone().into()),
                ("nodeExecutable", jp(&root.join("never-execute"))),
                ("processes", definitions.clone()),
            ]),
        ),
        (
            "previous",
            J::object([
                ("schemaVersion", J::Number(1.0)),
                ("hostId", "restore-fixture".into()),
                ("installationId", "restored-installation".into()),
                ("configPath", jp(&root.join("source.json"))),
                ("configurationHash", configuration_hash.into()),
                ("observedAt", "2026-10-20T00:00:00.000Z".into()),
                (
                    "owners",
                    J::Array(
                        definitions
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|definition| {
                                J::object([
                                    (
                                        "instanceId",
                                        definition.required("instanceId").unwrap().clone(),
                                    ),
                                    (
                                        "componentId",
                                        definition.required("componentId").unwrap().clone(),
                                    ),
                                    ("name", definition.required("name").unwrap().clone()),
                                    ("candidateId", candidate.clone().into()),
                                    ("definitionHash", hash_json(definition).unwrap().into()),
                                    ("enabled", true.into()),
                                    ("running", false.into()),
                                ])
                            })
                            .collect(),
                    ),
                ),
            ]),
        ),
    ]);
    let record = J::object([
        ("schemaVersion", J::Number(1.0)),
        (
            "operationId",
            request.required("operationId").unwrap().clone(),
        ),
        ("requestHash", hash_json(&request).unwrap().into()),
        ("request", request),
        ("phase", phase.into()),
        ("createdAt", "2026-10-20T00:00:00.000Z".into()),
        ("updatedAt", "2026-10-20T00:00:00.000Z".into()),
        ("errorCode", J::Null),
        ("definitions", J::Array(vec![])),
    ]);
    validate_host("BootstrapMaintenanceRecord", &record).unwrap();
    (
        hash_json(record.required("operationId").unwrap()).unwrap()[7..].to_owned(),
        record,
    )
}

#[tokio::test]
async fn restored_bootstrap_holds_save_exact_before_images_without_os_replay() {
    const AT: &str = "2026-10-21T00:00:00.000Z";
    let mut cases = vec![
        "checking",
        "needs-attention",
        "succeeded",
        "invalid-identity",
        "unexpected-entry",
        "unicode-dot-runtime",
        "canonical-runtime",
        "linked-runtime",
        "missing-parent",
        "missing-record",
        "invalid-schema",
        "oversized-record",
        "record-link",
        "directory-link",
        "existing-before-image",
        "ordered-partial",
        "uppercase-directory",
        "wrong-operation-directory",
        "dropped-observer",
    ];
    #[cfg(windows)]
    cases.extend(["opaque-runtime-high", "opaque-runtime-low"]);
    for case in cases {
        let f = ServiceFixture::new();
        let root = f.0.join("restore");
        fs::create_dir_all(&root).unwrap();
        #[cfg(windows)]
        let root = if case.starts_with("opaque-runtime") {
            fs::canonicalize(&root).unwrap()
        } else {
            root
        };
        let mut runtime = root.join("state");
        if case == "unicode-dot-runtime" {
            runtime = root.join("state-漢字 space");
        }
        #[cfg(windows)]
        if case.starts_with("opaque-runtime") {
            use std::{ffi::OsString, os::windows::ffi::OsStringExt};
            let mut name: Vec<u16> = "state".encode_utf16().collect();
            name.push(if case.ends_with("high") {
                0xd800
            } else {
                0xdfff
            });
            runtime = root.join(OsString::from_wide(&name));
            for name in [OsString::from("state�"), OsString::from("state. ")] {
                let alias = root.join(name);
                fs::create_dir_all(&alias).unwrap();
                fs::write(alias.join("untouched"), b"unrelated native alias").unwrap();
            }
        }
        fs::create_dir_all(&runtime).unwrap();
        fs::write(root.join("never-execute"), b"not an executable").unwrap();
        fs::write(runtime.join("unrelated"), b"retained runtime evidence").unwrap();
        let mut configured_runtime = runtime.clone();
        if case == "unicode-dot-runtime" {
            configured_runtime = root.join(".").join(runtime.file_name().unwrap());
        } else if case == "canonical-runtime" {
            configured_runtime = fs::canonicalize(&runtime).unwrap();
        } else if case == "linked-runtime" {
            configured_runtime = root.join("linked-state");
            #[cfg(windows)]
            std::os::windows::fs::symlink_dir(&runtime, &configured_runtime).unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink(&runtime, &configured_runtime).unwrap();
        }
        let configuration = J::object([("runtimeRoot", jp(&configured_runtime))]);
        let (mut name, mut record) = captured_record(
            &root,
            match case {
                "succeeded" => "succeeded",
                "needs-attention" => "needs_attention",
                _ => "checking",
            },
        );
        if case == "invalid-identity" {
            record.insert("requestHash", format!("sha256:{}", "e".repeat(64)).into());
        } else if case == "invalid-schema" {
            record.insert("phase", "invalid".into());
        } else if case == "uppercase-directory" {
            name.make_ascii_uppercase();
        } else if case == "wrong-operation-directory" {
            name = "0".repeat(64);
        }
        let parent = runtime.join("bootstrap-maintenance");
        let directory = parent.join(&name);
        let path = directory.join("record.json");
        let original = runtime
            .join("recovery-original/bootstrap-maintenance")
            .join(&name)
            .join("record.json");
        let bytes = format!(" \n{}\n ", json::canonical(&record, usize::MAX).unwrap()).into_bytes();
        if case != "missing-parent" {
            fs::create_dir_all(&directory).unwrap();
            if case != "missing-record" {
                fs::write(&path, &bytes).unwrap();
            }
        }
        if case == "unexpected-entry" {
            fs::write(parent.join("unknown"), b"unknown").unwrap();
        } else if case == "ordered-partial" {
            fs::create_dir(parent.join("z".repeat(64))).unwrap();
        } else if case == "existing-before-image" {
            fs::create_dir_all(original.parent().unwrap()).unwrap();
            fs::write(&original, b"immutable earlier recovery authority").unwrap();
        } else if case == "oversized-record" {
            fs::write(&path, vec![b' '; config::CONFIGURATION_BYTES + 1]).unwrap();
        } else if case == "record-link" {
            let external = root.join("external-record.json");
            fs::rename(&path, &external).unwrap();
            #[cfg(windows)]
            std::os::windows::fs::symlink_file(&external, &path).unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink(&external, &path).unwrap();
        } else if case == "directory-link" {
            let external = root.join("external-operation");
            fs::rename(&directory, &external).unwrap();
            #[cfg(windows)]
            std::os::windows::fs::symlink_dir(&external, &directory).unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink(&external, &directory).unwrap();
        }
        let mut expected = state(&root);
        let mut held = record.clone();
        held.insert("phase", "needs_attention".into());
        held.insert("errorCode", "restore_reconciliation_required".into());
        held.insert("updatedAt", AT.into());
        let mut action = Box::pin(restore::hold_restored_bootstrap_maintenance(
            configuration,
            AT.into(),
        ));
        let result = if case == "dropped-observer" {
            use std::task::Poll;
            let expected_json = json::canonical(&held, usize::MAX).unwrap();
            std::future::poll_fn(|cx| {
                assert!(action.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            drop(action);
            tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    if let Ok(value) =
                        config::json_file(path.clone(), config::CONFIGURATION_BYTES).await
                    {
                        if json::canonical(&value, usize::MAX).unwrap() == expected_json {
                            break;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
            .await
            .expect("admitted maintenance must finish after observer is dropped");
            None
        } else {
            Some(
                tokio::time::timeout(Duration::from_secs(30), action)
                    .await
                    .expect("maintenance must terminate"),
            )
        };
        let error = match case {
            "invalid-identity"
            | "directory-link"
            | "uppercase-directory"
            | "wrong-operation-directory"
            | "unexpected-entry"
            | "ordered-partial" => Some("storage_invalid"),
            "invalid-schema" | "oversized-record" => Some("invalid_arguments"),
            "record-link" => Some("target_conflict"),
            "existing-before-image" => Some("internal_error"),
            _ => None,
        };
        if let Some(result) = result {
            if let Some(code) = error {
                assert_eq!(result.unwrap_err().code, code, "{case}");
            } else {
                let held_bytes =
                    if ["succeeded", "missing-parent", "missing-record"].contains(&case) {
                        0
                    } else {
                        bytes.len() as u64
                    };
                assert_eq!(result.unwrap(), held_bytes, "{case}");
            }
        }
        let changed = error.is_none()
            && !["succeeded", "missing-parent", "missing-record"].contains(&case)
            || ["ordered-partial", "unexpected-entry"].contains(&case);
        if changed {
            assert_json_eq(
                &config::json_file(path.clone(), config::CONFIGURATION_BYTES)
                    .await
                    .unwrap(),
                &held,
            );
            validate_host("BootstrapMaintenanceRecord", &held).unwrap();
            assert_eq!(
                fs::read(&original).unwrap(),
                bytes,
                "{case}: exact original bytes"
            );
            expected.insert(
                path.strip_prefix(&root).unwrap().to_owned(),
                Entry::File(fs::read(&path).unwrap()),
            );
            let mut directory = original.parent();
            while let Some(path) = directory.filter(|path| *path != runtime) {
                expected.insert(
                    path.strip_prefix(&root).unwrap().to_owned(),
                    Entry::Directory,
                );
                directory = path.parent();
            }
            expected.insert(
                original.strip_prefix(&root).unwrap().to_owned(),
                Entry::File(bytes),
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
