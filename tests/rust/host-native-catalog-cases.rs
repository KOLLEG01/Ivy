use super::{Fixture, text};
use ivy_contracts::{
    IvyError, IvyJson, JsString,
    checked_native_contract::{native_catalog_path, native_versions},
    json::stringify,
};
use ivy_host_runtime::{
    native_catalog_compatibility::verify_configured_native_catalog, native_path,
};
use std::{ffi::OsString, fs, path::Path};

fn registered_host() -> IvyJson {
    IvyJson::object([
        ("serviceNodeId", "host.agent-manager".into()),
        ("hostId", "host".into()),
        ("serviceName", "agent-manager".into()),
        ("instanceMode", "multiple".into()),
        ("principalId", "fixture-caller".into()),
        ("version", "0.4.33".into()),
        ("buildId", format!("sha256:{}", "a".repeat(64)).into()),
        ("hiveProtocol", 1.0.into()),
        ("nativeVersion", IvyJson::Null),
        ("connected", true.into()),
        ("synced", true.into()),
        ("ready", true.into()),
        ("desiredEnabled", true.into()),
        ("stale", false.into()),
        ("lastContactAt", "2026-01-02T03:04:05.000Z".into()),
        ("lastSuccessfulSyncAt", "2026-01-02T03:04:05.000Z".into()),
        ("lastFailedSyncAt", IvyJson::Null),
        ("lastObservationAt", IvyJson::Null),
        ("diagnostic", IvyJson::Null),
    ])
}

#[tokio::test]
async fn catalog_admission_requires_bounded_complete_discovery_and_preserves_read_errors() {
    use ivy_contracts::Outcome;
    use ivy_host_runtime::native_catalog_compatibility::verify_native_catalog_compatibility;
    use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
    use std::sync::{Arc, Mutex};

    struct Client {
        total: usize,
        pages: usize,
        continued: bool,
        repeated: bool,
        error_at: Option<usize>,
        requests: Mutex<Vec<IvyJson>>,
    }
    fn cursor(index: usize) -> IvyJson {
        // Keep the maximum legal opaque cursor live across the page boundary.
        format!("{}-{index:04}", "c".repeat(8187)).into()
    }
    impl RpcClient for Client {
        fn request<'a>(
            &'a self,
            method: &'a str,
            args: IvyJson,
            _: RequestOptions,
        ) -> RequestFuture<'a> {
            Box::pin(async move {
                assert_eq!(
                    method, "serviceNodes.list",
                    "Unready nodes must not trigger native discovery."
                );
                ivy_contracts::core_schema::validate_input(method, &args)?;
                assert_eq!(
                    args.required("serviceName")?,
                    &IvyJson::from("agent-manager")
                );
                assert_eq!(args.required("limit")?, &IvyJson::from(200.0));
                let index = {
                    let mut requests = self.requests.lock().unwrap();
                    let index = requests.len();
                    if index == 0 {
                        assert!(args.get("cursor").is_none());
                    } else {
                        assert_eq!(
                            args.required("cursor")?,
                            &cursor(if self.repeated { 1 } else { index })
                        );
                    }
                    requests.push(args);
                    index
                };
                if self.error_at == Some(index) {
                    return Err(IvyError::new(
                        "source_unavailable",
                        "Original registry read failed.",
                    )
                    .with_outcome(Outcome::Unknown)
                    .with_details(IvyJson::object([("originalPage", (index as f64).into())])));
                }
                if index >= self.pages {
                    return Err(IvyError::new(
                        "late_page_requested",
                        "The bounded consumer must stop before this read.",
                    ));
                }
                let mut template = registered_host();
                template.insert("ready", false.into());
                template.insert("desiredEnabled", false.into());
                let items = (index * 200..self.total.min((index + 1) * 200))
                    .map(|id| {
                        let mut node = template.clone();
                        node.insert("serviceNodeId", format!("node-{id:04}").into());
                        node
                    })
                    .collect();
                let page = IvyJson::object([
                    ("items", IvyJson::Array(items)),
                    (
                        "nextCursor",
                        if index + 1 < self.pages || self.continued {
                            cursor(if self.repeated { 1 } else { index + 1 })
                        } else {
                            IvyJson::Null
                        },
                    ),
                ]);
                ivy_contracts::core_schema::validate_output(method, &page)?;
                Ok(page)
            })
        }
    }
    let fixture = Fixture::new();
    let root = fixture.0.join("candidate");
    let file = root.join("dist/specs/native/codex-0.163.0/catalog.json");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(
        file,
        catalog(
            &"0.163.0".into(),
            &format!("sha256:{}", "a".repeat(64)).into(),
        ),
    )
    .unwrap();
    for (name, total, pages, continued, repeated, error_at, reads, error_code) in [
        ("node-boundary", 1000, 5, false, false, None, 5, None),
        ("page-boundary", 0, 20, false, false, None, 20, None),
        (
            "node-limit",
            1001,
            6,
            false,
            false,
            None,
            6,
            Some("compatibility_unknown"),
        ),
        (
            "empty-continuation",
            0,
            20,
            true,
            false,
            None,
            20,
            Some("compatibility_unknown"),
        ),
        (
            "repeated-cursor",
            0,
            2,
            true,
            true,
            None,
            2,
            Some("compatibility_unknown"),
        ),
        (
            "original-read-error",
            0,
            2,
            false,
            false,
            Some(1),
            2,
            Some("source_unavailable"),
        ),
    ] {
        let client = Arc::new(Client {
            total,
            pages,
            continued,
            repeated,
            error_at,
            requests: Mutex::default(),
        });
        let result =
            verify_native_catalog_compatibility(client.clone(), &root, &"consumer".into()).await;
        if let Some(code) = error_code {
            let error = result.unwrap_err();
            assert_eq!(error.code, code, "{name}");
            if error_at.is_some() {
                assert_eq!(error.outcome, Outcome::Unknown);
                assert_eq!(error.message, "Original registry read failed.");
                assert_eq!(
                    error.details,
                    Some(IvyJson::object([("originalPage", 1.0.into())]))
                );
            }
        } else {
            result.unwrap();
        }
        assert_eq!(client.requests.lock().unwrap().len(), reads, "{name}");
    }
}

#[tokio::test]
async fn hosted_catalog_checks_every_ready_account_and_never_reuses_owner_status() {
    use ivy_contracts::{
        hash::hash_json,
        json::parse,
        native_backend::{BackendIdentity, NativeProvider},
    };
    use ivy_host_runtime::native_catalog_compatibility::verify_native_catalog_compatibility;
    use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
    use std::sync::{Arc, Mutex};
    struct Client {
        statuses: Vec<IvyJson>,
        legacy: bool,
        pages: usize,
        calls: Mutex<Vec<String>>,
    }
    impl RpcClient for Client {
        fn request<'a>(
            &'a self,
            method: &'a str,
            args: IvyJson,
            _: RequestOptions,
        ) -> RequestFuture<'a> {
            Box::pin(async move {
                match method {
                    "serviceNodes.list" => {
                        let page = args
                            .get("cursor")
                            .map_or(0, |value| value.as_str().unwrap().parse::<usize>().unwrap());
                        assert!(page < self.pages);
                        let mut node = registered_host();
                        if self.legacy {
                            node.insert("nativeVersion", "0.163.0".into());
                        }
                        let page = IvyJson::object([
                            (
                                "items",
                                IvyJson::Array(if page + 1 == self.pages {
                                    vec![node]
                                } else {
                                    vec![]
                                }),
                            ),
                            (
                                "nextCursor",
                                if page + 1 == self.pages {
                                    IvyJson::Null
                                } else {
                                    (page + 1).to_string().into()
                                },
                            ),
                        ]);
                        ivy_contracts::core_schema::validate_output("serviceNodes.list", &page)?;
                        Ok(page)
                    }
                    "tools.list" => {
                        let name = args.required("namePrefix")?.as_str()?;
                        Ok(IvyJson::object([
                            (
                                "provider",
                                IvyJson::object([(
                                    "node",
                                    IvyJson::object([(
                                        "serviceNodeId",
                                        "host.agent-manager".into(),
                                    )]),
                                )]),
                            ),
                            (
                                "items",
                                IvyJson::Array(vec![IvyJson::object([
                                    ("qualifiedName", format!("agent.{name}").into()),
                                    (
                                        "definition",
                                        IvyJson::object([
                                            ("namespace", "agent".into()),
                                            ("name", name.into()),
                                            ("interfaceVersion", "1.0.0".into()),
                                            (
                                                "annotations",
                                                IvyJson::object([("readOnlyHint", true.into())]),
                                            ),
                                            ("inputSchema", IvyJson::Object(vec![])),
                                            ("outputSchema", IvyJson::Object(vec![])),
                                        ]),
                                    ),
                                    (
                                        "definitionHash",
                                        format!("sha256:{}", "a".repeat(64)).into(),
                                    ),
                                    ("catalogHash", format!("sha256:{}", "b".repeat(64)).into()),
                                ])]),
                            ),
                            ("nextCursor", IvyJson::Null),
                        ]))
                    }
                    "tools.call" => {
                        let name = args.required("qualifiedName")?.as_str()?;
                        self.calls.lock().unwrap().push(name.clone());
                        assert_eq!(args.required("arguments")?, &IvyJson::Object(vec![]));
                        assert_eq!(
                            name,
                            if self.legacy {
                                "agent.status"
                            } else {
                                "agent.backends"
                            }
                        );
                        Ok(if self.legacy {
                            self.statuses[0].clone()
                        } else {
                            IvyJson::object([("backends", IvyJson::Array(self.statuses.clone()))])
                        })
                    }
                    other => panic!("Unexpected compatibility effect: {other}"),
                }
            })
        }
    }
    let fixture = Fixture::new();
    let root = fixture.0.join("candidate");
    let mut statuses = Vec::new();
    let mut configured = Vec::new();
    for (id, provider, account, version, digit) in [
        ("alpha", NativeProvider::Codex, "personal", "0.163.0", "c"),
        ("beta", NativeProvider::Claude, "work", "0.142.3", "d"),
    ] {
        let executable: JsString = format!("sha256:{}", digit.repeat(64)).into();
        let bytes = catalog(&version.into(), &executable);
        let value = parse(std::str::from_utf8(&bytes).unwrap(), usize::MAX).unwrap();
        let file = root.join(format!("dist/specs/native/codex-{version}/catalog.json"));
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, bytes).unwrap();
        let identity = BackendIdentity::new(id.into(), provider, account.into()).unwrap();
        statuses.push(IvyJson::object([
            ("backend", identity.json()),
            ("serviceNodeId", "host.agent-manager".into()),
            ("hostId", "host".into()),
            ("nativeVersion", version.into()),
            ("nativeExecutableHash", IvyJson::String(executable.clone())),
            ("catalogHash", hash_json(&value).unwrap().into()),
            ("epoch", "epoch".into()),
            ("pid", 42.0.into()),
            ("state", "ready".into()),
            ("observedAt", "2026-01-02T03:04:05.000Z".into()),
            ("code", IvyJson::Null),
            ("initialized", IvyJson::Null),
            ("pendingInputs", 0.0.into()),
            ("observedMethods", IvyJson::Array(vec![])),
            (
                "operations",
                IvyJson::object([
                    ("retained", 0.0.into()),
                    ("maximum", 100.0.into()),
                    ("bytes", 0.0.into()),
                    ("maximumBytes", 1048576.0.into()),
                ]),
            ),
        ]));
        configured.push(IvyJson::object([
            ("dataRoot", text(&fixture.0.join(id)).into()),
            (
                "settings",
                IvyJson::object([
                    ("backend", identity.json()),
                    (
                        "appServer",
                        IvyJson::object([(
                            "mode",
                            if provider == NativeProvider::Claude {
                                "claude-adapter"
                            } else {
                                "owned-stdio"
                            }
                            .into(),
                        )]),
                    ),
                    ("nativeVersion", version.into()),
                    ("nativeExecutableHash", IvyJson::String(executable)),
                ]),
            ),
        ]));
    }
    let settings = IvyJson::object([("backends", IvyJson::Array(configured.clone()))]);
    verify_configured_native_catalog(&root, &"manager".into(), &settings)
        .await
        .unwrap();
    let mut wrong = configured[1].required("settings").unwrap().clone();
    wrong.insert(
        "nativeExecutableHash",
        format!("sha256:{}", "e".repeat(64)).into(),
    );
    configured[1].insert("settings", wrong);
    assert_eq!(
        verify_configured_native_catalog(
            &root,
            &"manager".into(),
            &IvyJson::object([("backends", IvyJson::Array(configured))])
        )
        .await
        .unwrap_err()
        .code,
        "incompatible_native_catalog"
    );
    for variant in 0..8 {
        let mut observed = statuses.clone();
        match variant {
            1 | 2 => {
                observed[1].insert("catalogHash", format!("sha256:{}", "f".repeat(64)).into());
                if variant == 2 {
                    observed[1].insert("state", "failed".into());
                }
            }
            3 => {
                observed[1].insert("hostId", "foreign".into());
            }
            4 => {
                let owner = observed[0].required("backend").unwrap().clone();
                observed[1].insert("backend", owner);
            }
            5 => {
                observed[1].remove("backend");
            }
            6 => {
                observed.truncate(1);
                observed[0].remove("backend");
            }
            7 => {
                let mut failed = IvyJson::object(
                    observed[1]
                        .as_object()
                        .unwrap()
                        .iter()
                        .filter(|(key, _)| {
                            [
                                "backend",
                                "serviceNodeId",
                                "hostId",
                                "nativeVersion",
                                "nativeExecutableHash",
                                "observedAt",
                            ]
                            .iter()
                            .any(|name| key.eq_str(name))
                        })
                        .cloned(),
                );
                failed.insert("phase", "preparation".into());
                failed.insert("state", "failed".into());
                failed.insert("code", "native_backend_mismatch".into());
                failed.insert("operationReadsAvailable", false.into());
                observed[1] = failed;
            }
            _ => {}
        }
        let client = Arc::new(Client {
            statuses: observed,
            legacy: variant == 6,
            pages: if variant == 0 { 20 } else { 1 },
            calls: Mutex::new(vec![]),
        });
        let result =
            verify_native_catalog_compatibility(client.clone(), &root, &"consumer".into()).await;
        if [0, 2, 6, 7].contains(&variant) {
            result.unwrap();
        } else {
            assert_eq!(
                result.unwrap_err().code,
                if variant == 1 {
                    "incompatible_native_catalog"
                } else {
                    "compatibility_unknown"
                },
                "variant {variant}"
            );
        }
        assert_eq!(client.calls.lock().unwrap().len(), 1);
    }
}

fn catalog(version: &JsString, executable: &JsString) -> Vec<u8> {
    stringify(
        &IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("provider", "codex".into()),
            ("version", IvyJson::String(version.clone())),
            ("experimental", true.into()),
            ("sourceHash", format!("sha256:{}", "11".repeat(32)).into()),
            ("nativeExecutableHash", IvyJson::String(executable.clone())),
            ("clientRequests", IvyJson::Array(vec![])),
            ("serverRequests", IvyJson::Array(vec![])),
            ("serverNotifications", IvyJson::Array(vec![])),
            ("clientNotifications", IvyJson::Array(vec![])),
            ("derivedLegacyTypes", IvyJson::Array(vec![])),
        ]),
        usize::MAX,
    )
    .unwrap()
    .into_bytes()
}

#[tokio::test]
async fn configured_catalog_reads_native_files_and_preserves_selection_and_refusals() {
    let fixture = Fixture::new();
    let version = &native_versions().unwrap()[0];
    let executable: JsString = format!("sha256:{}", "22".repeat(32)).into();
    let platform_executable: JsString = format!("sha256:{}", "33".repeat(32)).into();
    let fallback_bytes = catalog(version, &executable);
    let platform_bytes = catalog(version, &platform_executable);
    let mut names = vec![OsString::from("space; quote' 日本語 e\u{301} 😀")];
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        names.extend([0xD800, 0xDFFF].map(|unit| OsString::from_wide(&[b'n' as u16, unit])));
    }
    for (index, name) in names.into_iter().enumerate() {
        let parent = fixture.0.join(index.to_string());
        let root = parent.join(&name);
        let instance = text(Path::new(&name));
        let dist = root.join("dist");
        let fallback = native_path::to_native(
            &native_catalog_path(&text(&dist), version, None, None).unwrap(),
        );
        fs::create_dir_all(fallback.parent().unwrap()).unwrap();
        fs::create_dir(root.join("segment")).unwrap();
        let settings = |hash: &JsString| {
            IvyJson::object([
                ("nativeVersion", IvyJson::String(version.clone())),
                ("nativeExecutableHash", IvyJson::String(hash.clone())),
            ])
        };

        // A different, existing filename must not supply a missing native file.
        let alias = parent.join(name.to_string_lossy().as_ref());
        let alias_file = alias.join(fallback.strip_prefix(&root).unwrap());
        if alias != root {
            fs::create_dir_all(alias_file.parent().unwrap()).unwrap();
            fs::write(&alias_file, &fallback_bytes).unwrap();
        }
        assert_eq!(
            verify_configured_native_catalog(&root, &instance, &settings(&executable))
                .await
                .unwrap_err()
                .code,
            "internal_error"
        );
        fs::write(&fallback, &fallback_bytes).unwrap();
        if alias != root {
            fs::write(&alias_file, &platform_bytes).unwrap();
        }
        for input in [&root, &root.join("segment/..")] {
            verify_configured_native_catalog(input, &instance, &settings(&executable))
                .await
                .unwrap();
        }

        // Supply the current target's file while retaining the fallback catalog.
        let platform = match std::env::consts::OS {
            "windows" => "win32",
            "macos" => "darwin",
            name => name,
        };
        let arch = match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            "x86" => "ia32",
            name => name,
        };
        let selected = fallback.with_file_name(format!("catalog.{platform}-{arch}.json"));
        fs::write(&selected, &platform_bytes).unwrap();
        let checked = native_catalog_path(&text(&dist), version, None, None).unwrap();
        assert_eq!(native_path::to_native(&checked), selected);
        verify_configured_native_catalog(&root, &instance, &settings(&platform_executable))
            .await
            .unwrap();

        let error = verify_configured_native_catalog(&root, &instance, &settings(&executable))
            .await
            .unwrap_err();
        let message = JsString(
            [
                instance.clone(),
                " is configured for native version ".into(),
                version.clone(),
                ", executable ".into(),
                executable.clone(),
                ", but the candidate requires executable ".into(),
                platform_executable.clone(),
                ". Prepare the matching native runtime and configuration before retrying.".into(),
            ]
            .into_iter()
            .flat_map(|part| part.0)
            .collect(),
        );
        assert_eq!(
            error,
            IvyError::new_js("incompatible_native_catalog".into(), message)
        );
        assert_eq!(fs::read(&fallback).unwrap(), fallback_bytes);
        assert_eq!(fs::read(&selected).unwrap(), platform_bytes);
        if alias != root {
            assert_eq!(fs::read(&alias_file).unwrap(), platform_bytes);
        }

        fs::write(
            &selected,
            catalog(&"different-version".into(), &platform_executable),
        )
        .unwrap();
        assert_eq!(
            verify_configured_native_catalog(&root, &instance, &settings(&platform_executable))
                .await
                .unwrap_err()
                .code,
            "incompatible_native_catalog"
        );
        fs::remove_file(&selected).unwrap();
        fs::create_dir(&selected).unwrap();
        assert_eq!(
            verify_configured_native_catalog(&root, &instance, &settings(&executable))
                .await
                .unwrap_err()
                .code,
            "internal_error",
            "An unreadable selected catalog must not fall back."
        );
        assert_eq!(fs::read(&fallback).unwrap(), fallback_bytes);
    }
}
