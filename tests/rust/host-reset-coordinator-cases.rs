use super::assert_json_eq;
use ivy_contracts::{IvyError, IvyJson as J, IvyResult, Outcome, json};
use ivy_host_runtime::runtime_reset;
use ivy_host_runtime::{host_config::HostContext, layout::StorageContext};
use std::{
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
};

fn plan_declaration(area: &str, path: &str) -> J {
    J::object([("area", area.into()), ("path", path.into())])
}

#[tokio::test]
async fn reset_local_plan_preserves_declared_trees_and_rejects_path_conflicts() {
    use ivy_contracts::{
        JsString,
        hash::{digest, hash_json},
    };
    use ivy_host_runtime::{filesystem, journal::HostJournal, node_path};
    use std::fs;

    let path_json = |path: &std::path::Path| J::String(node_path::from_native(path));
    for case in [
        "valid",
        "unicode-tree",
        "missing-preserve",
        "missing-layout",
        "delete-escape",
        "data-root",
        "preserve-overlap",
        "duplicate-target",
        "completion-root",
        "linked-delete",
        "completion-link",
        "protected-software",
        "no-deployment",
        "duplicate-host",
        "remote-missing-cli",
        "native-future",
    ] {
        let fixture = super::ServiceFixture::new();
        let root = fixture.0.join("reset-plan");
        let distribution = root.join("distribution");
        let config_path = root.join("config.json");
        let plan_path = distribution.join("services/fixture/deploy.json");
        fs::create_dir_all(plan_path.parent().unwrap()).unwrap();
        let mut data = root.join("data");
        let mut work = root.join("work");
        let mut logs = root.join("logs");
        if case == "native-future" {
            let name = JsString::from("native area 中 🚀");
            #[cfg(windows)]
            let name = {
                let mut name = name;
                name.0.push(0xd800);
                name
            };
            let actual = root.join(node_path::node_filesystem_path(&name).unwrap());
            fs::create_dir(&actual).unwrap();
            data = root.join("data-link");
            filesystem::directory_link(&actual, &data).unwrap();
            work = data.join("missing-work");
            logs = data.join("missing-logs");
        } else {
            fs::create_dir(&data).unwrap();
        }
        fs::write(data.join("settings.json"), b"settings").unwrap();
        fs::write(data.join("journal.sqlite"), b"runtime").unwrap();

        let mut config = super::host_configuration(&fixture);
        config.insert("runtimeRoot", path_json(&root.join("runtime")));
        config.insert("artifactRoot", path_json(&root.join("artifacts")));
        config.insert("stagingRoot", path_json(&root.join("staging")));
        let mut instance = config.required("instances").unwrap().as_array().unwrap()[0].clone();
        instance.insert("instanceId", "worker".into());
        instance.insert("serviceNodeId", "worker".into());
        instance.insert("enabled", false.into());
        instance.insert(
            "paths",
            J::object([
                ("data", path_json(&data)),
                ("work", path_json(&work)),
                ("logs", path_json(&logs)),
            ]),
        );
        config.insert(
            "deployment",
            J::object([
                ("identity", "reference-rollout".into()),
                (
                    "hosts",
                    J::Array(vec![J::object([
                        ("hostId", config.required("hostId").unwrap().clone()),
                        ("configPath", path_json(&config_path)),
                    ])]),
                ),
            ]),
        );

        let mut component = json::parse(r#"{
            "schemaVersion":1,"componentId":"fixture","kind":"service","version":"1.0.0",
            "description":"Reset planning fixture.","connectsToHive":false,
            "requirements":{"node":">=24.18.0 <25.0.0","hiveProtocol":null,"contracts":[]},
            "prepare":[],"checks":[],
            "entrypoint":{"executable":"node","args":["dist/fixture.js"],"timeoutMs":5000},
            "readiness":{"timeoutMs":5000,"command":{"executable":"node","args":["dist/health.js"],"timeoutMs":1000}},
            "shutdown":{"timeoutMs":5000},
            "restart":{"policy":"never","minimumDelayMs":1000,"maximumDelayMs":1000}
        }"#, usize::MAX).unwrap();
        let mut deleting = vec![
            plan_declaration("data", "journal.sqlite"),
            plan_declaration("work", "."),
            plan_declaration("logs", "."),
        ];
        let mut preserve_member = "settings.json";
        let mut marker = "runtime-reset.json";
        match case {
            "delete-escape" => deleting[0] = plan_declaration("data", "../escaped"),
            "data-root" => deleting[0] = plan_declaration("data", "."),
            "preserve-overlap" => deleting.push(plan_declaration("data", "settings.json")),
            "duplicate-target" => deleting.push(deleting[0].clone()),
            "completion-root" => marker = ".",
            "missing-preserve" => preserve_member = "absent.json",
            "unicode-tree" => preserve_member = "settings",
            "protected-software" => {
                let protected = root.join("artifacts");
                fs::create_dir(&protected).unwrap();
                let mut paths = instance.required("paths").unwrap().clone();
                paths.insert("data", path_json(&protected));
                instance.insert("paths", paths);
            }
            "linked-delete" | "completion-link" => {
                let outside = root.join("outside");
                fs::create_dir(&outside).unwrap();
                fs::write(outside.join("kept.txt"), b"outside fixture authority").unwrap();
                filesystem::directory_link(&outside, &data.join("linked")).unwrap();
                if case == "linked-delete" {
                    deleting[0] = plan_declaration("data", "linked/runtime");
                } else {
                    marker = "linked/marker.json";
                }
            }
            "native-future" => {
                deleting[1] = plan_declaration("work", "nested/leaf");
                marker = "future/marker.json";
            }
            _ => (),
        }
        let mut rows = vec![J::object([
            ("path", "".into()),
            ("hash", digest(b"settings").into()),
        ])];
        if case == "unicode-tree" {
            fs::create_dir(data.join("settings")).unwrap();
            rows.clear();
            // Distinct names on both case-sensitive and case-insensitive hosts.
            for name in ["a", "a\u{0308}", "b", "ä", "中", "🚀"] {
                let bytes = format!("preserved {name}");
                fs::write(data.join("settings").join(name), bytes.as_bytes()).unwrap();
                rows.push(J::object([
                    ("path", name.into()),
                    ("hash", digest(bytes).into()),
                ]));
            }
        }
        if case != "missing-layout" {
            component.insert(
                "runtimeReset",
                J::object([
                    ("delete", J::Array(deleting)),
                    (
                        "preserve",
                        J::Array(if case == "protected-software" {
                            vec![]
                        } else {
                            vec![plan_declaration("data", preserve_member)]
                        }),
                    ),
                    ("completionMarker", marker.into()),
                ]),
            );
        }
        config.insert("instances", J::Array(vec![instance]));
        let journal = HostJournal::open(config.clone()).await.unwrap();
        journal.close().await.unwrap();
        match case {
            "no-deployment" => {
                config.remove("deployment");
            }
            "duplicate-host" | "remote-missing-cli" => {
                let mut deployment = config.required("deployment").unwrap().clone();
                let mut hosts = deployment
                    .required("hosts")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .to_vec();
                hosts.push(if case == "duplicate-host" {
                    hosts[0].clone()
                } else {
                    J::object([
                        ("hostId", "remote".into()),
                        ("configPath", path_json(&root.join("remote-config.json"))),
                    ])
                });
                deployment.insert("hosts", J::Array(hosts));
                config.insert("deployment", deployment);
            }
            _ => (),
        }
        fs::write(&config_path, json::stringify(&config, usize::MAX).unwrap()).unwrap();
        fs::write(&plan_path, json::stringify(&component, usize::MAX).unwrap()).unwrap();
        let journal_path = root.join("runtime/deployments.sqlite");
        let mut saved_paths = vec![
            config_path.clone(),
            plan_path,
            data.join("settings.json"),
            data.join("journal.sqlite"),
        ];
        if case == "linked-delete" || case == "completion-link" {
            saved_paths.push(root.join("outside/kept.txt"));
        }
        let before = saved_paths
            .iter()
            .map(|p| fs::read(p).unwrap())
            .collect::<Vec<_>>();
        let absent = [
            work.clone(),
            logs.clone(),
            data.join(marker),
            root.join("escaped"),
        ];
        let host = HostContext::current().unwrap();
        let result = runtime_reset::local_runtime_reset_plan(
            config.clone(),
            node_path::from_native(&config_path),
            distribution,
            Arc::new(StorageContext {
                paths: host.paths,
                account_home: host.account_home,
                compare: Arc::new(|a, b| a.0.cmp(&b.0)),
            }),
        )
        .await;
        if ["valid", "unicode-tree", "missing-preserve", "native-future"].contains(&case) {
            let row = |area: &str, member: &str, path: PathBuf| {
                J::object([
                    ("instanceId", "worker".into()),
                    ("componentId", "fixture".into()),
                    ("area", area.into()),
                    ("path", member.into()),
                    ("absolutePath", path_json(&path)),
                ])
            };
            let mut preserved = row("data", preserve_member, data.join(preserve_member));
            preserved.insert(
                "checksum",
                if case == "missing-preserve" {
                    J::Null
                } else {
                    hash_json(&J::Array(rows)).unwrap().into()
                },
            );
            let expected = J::object([
                ("schemaVersion", J::Number(1.0)),
                ("deploymentIdentity", "reference-rollout".into()),
                ("hostId", config.required("hostId").unwrap().clone()),
                ("configPath", path_json(&config_path)),
                (
                    "instances",
                    J::Array(vec![J::object([
                        ("instanceId", "worker".into()),
                        ("componentId", "fixture".into()),
                        ("enabled", false.into()),
                    ])]),
                ),
                ("scopeRootIds", J::Array(vec![])),
                (
                    "delete",
                    J::Array(vec![
                        row("data", "journal.sqlite", data.join("journal.sqlite")),
                        if case == "native-future" {
                            row("work", "nested/leaf", work.join("nested").join("leaf"))
                        } else {
                            row("work", ".", work.clone())
                        },
                        row("logs", ".", logs.clone()),
                    ]),
                ),
                ("preserve", J::Array(vec![preserved])),
                (
                    "completionMarkers",
                    J::Array(vec![J::object([
                        ("instanceId", "worker".into()),
                        ("componentId", "fixture".into()),
                        ("path", marker.into()),
                        (
                            "absolutePath",
                            path_json(&if case == "native-future" {
                                data.join("future").join("marker.json")
                            } else {
                                data.join(marker)
                            }),
                        ),
                    ])]),
                ),
            ]);
            assert_json_eq(
                &result.unwrap_or_else(|error| panic!("{case}: {error:?}")),
                &expected,
            );
        } else {
            let (code, message) = match case {
                "missing-layout" => (
                    "unsupported_storage",
                    "Component fixture has no fixed runtime reset layout.",
                ),
                "delete-escape" => (
                    "invalid_arguments",
                    "Value does not match its host configuration/operation contract.",
                ),
                "data-root" => (
                    "target_conflict",
                    "Runtime reset target leaves its declared component area.",
                ),
                "preserve-overlap" => (
                    "target_conflict",
                    "A preserved component setting overlaps a deletion target.",
                ),
                "duplicate-target" => (
                    "target_conflict",
                    "Runtime reset deletion targets overlap between component declarations.",
                ),
                "completion-root" => (
                    "target_conflict",
                    "Runtime reset completion marker leaves its instance data.",
                ),
                "linked-delete" => (
                    "target_conflict",
                    "Runtime reset target traverses a filesystem link.",
                ),
                "completion-link" => (
                    "target_conflict",
                    "Runtime reset completion marker traverses a filesystem link.",
                ),
                "protected-software" => (
                    "target_conflict",
                    "Runtime reset target overlaps configuration, software, source or project storage.",
                ),
                "no-deployment" => (
                    "invalid_arguments",
                    "Runtime reset requires an explicit deployment identity and host list containing this host.",
                ),
                "duplicate-host" => (
                    "invalid_arguments",
                    "Runtime reset deployment hosts must be unique.",
                ),
                "remote-missing-cli" => (
                    "invalid_arguments",
                    "Every remote reset host requires an SSH identity and absolute CLI path.",
                ),
                _ => unreachable!(),
            };
            let error = result.unwrap_err();
            assert_eq!(error.code, code, "{case}");
            assert_eq!(error.message, message, "{case}");
        }
        for (path, bytes) in saved_paths.iter().zip(before) {
            assert_eq!(fs::read(path).unwrap(), bytes, "{case}: {path:?}");
        }
        for path in absent {
            // The deliberately invalid data-root marker is the area itself.
            if case != "completion-root" || path != data {
                assert!(!path.exists(), "{case}: preview created {path:?}");
            }
        }
        // An unclosed SQLite handle prevents this on Windows, including error paths.
        let moved = root.join("closed-journal.sqlite");
        fs::rename(&journal_path, &moved).unwrap();
        fs::rename(moved, journal_path).unwrap();
    }
}

fn failure(input: &J) -> IvyError {
    let value = input.get("failure").unwrap();
    let mut error = IvyError::new_js(
        value.get("code").unwrap().as_js_string().unwrap().clone(),
        value
            .get("message")
            .unwrap()
            .as_js_string()
            .unwrap()
            .clone(),
    )
    .with_outcome(Outcome::Unknown);
    if let Some(details) = value.get("details") {
        error = error.with_details(details.clone());
    }
    error
}
fn wrap(result: IvyResult<J>) -> J {
    match result {
        Ok(value) => J::object([("ok", true.into()), ("value", value)]),
        Err(error) => J::object([("ok", false.into()), ("error", error.to_wire())]),
    }
}

#[tokio::test]
async fn reset_coordinator_retains_identity_and_recovers_every_uncertain_host() {
    let output =
        Command::new(std::env::var_os("IVY_RESTORE_NODE").unwrap_or_else(|| "node".into()))
            .args([
                "--experimental-transform-types",
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/host-reset-coordinator-oracle.mjs",
            ])
            .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .output()
            .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let fixture = json::parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    for case in fixture.get("cases").unwrap().as_array().unwrap() {
        let input = case.get("input").unwrap();
        let events = Arc::new(Mutex::new(Vec::<J>::new()));
        let group = case.get("group").unwrap().as_str().unwrap();
        let result = match group.as_str() {
            "identity" => Ok(runtime_reset::retained_reset_identity(
                input.get("previous"),
                input.get("hash").unwrap().as_js_string().unwrap(),
            )
            .unwrap_or(J::Null)),
            "scopes" => runtime_reset::configured_scope_root_ids(input.get("config").unwrap())
                .map(|v| J::Array(v.into_iter().map(J::String).collect())),
            "plans" => runtime_reset::coordinated_runtime_reset_plans(
                input.get("plans").unwrap().as_array().unwrap(),
            )
            .map(J::Array),
            "release" | "bootstrap" => {
                let phase = |host: J| {
                    let events = events.clone();
                    let group = group.clone();
                    async move {
                        let id = host.get("hostId").unwrap();
                        events
                            .lock()
                            .unwrap()
                            .push(format!("{} {}", group, id.as_str().unwrap()).into());
                        if input.get("failAt") == Some(id) {
                            Err(failure(input))
                        } else {
                            Ok(())
                        }
                    }
                };
                let reprepare = |host: J| {
                    let events = events.clone();
                    async move {
                        let id = host.get("hostId").unwrap();
                        events
                            .lock()
                            .unwrap()
                            .push(format!("prepare {}", id.as_str().unwrap()).into());
                        if input
                            .get("reprepareFailures")
                            .unwrap()
                            .as_array()
                            .unwrap()
                            .contains(id)
                        {
                            Err(IvyError::internal())
                        } else {
                            Ok(())
                        }
                    }
                };
                let after = async {
                    events.lock().unwrap().push("after".into());
                    if input.get("afterFailure") == Some(&true.into()) {
                        Err(failure(input))
                    } else {
                        Ok(())
                    }
                };
                let hosts = input.get("hosts").unwrap().as_array().unwrap();
                if group == "release" {
                    runtime_reset::release_reset_hosts(hosts, phase, reprepare, after).await
                } else {
                    runtime_reset::bootstrap_reset_uis(hosts, phase, reprepare, after).await
                }
                .map(|_| J::Null)
            }
            _ => panic!("unknown reset group"),
        };
        let actual = J::object([
            ("outcome", wrap(result)),
            ("events", J::Array(events.lock().unwrap().clone())),
        ]);
        if let Some(path) = std::env::var_os("IVY_HOST_RESET_CORPUS") {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap();
            writeln!(
                file,
                "{}",
                json::canonical(
                    &J::object([
                        ("case", case.clone()),
                        ("actual", actual.clone()),
                        ("sourceHash", fixture.get("sourceHash").unwrap().clone()),
                        (
                            "declarationHash",
                            fixture.get("declarationHash").unwrap().clone()
                        )
                    ]),
                    usize::MAX
                )
                .unwrap()
            )
            .unwrap();
        }
        assert_json_eq(&actual, case.get("expected").unwrap());
    }
}
