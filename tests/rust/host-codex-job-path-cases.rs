use super::*;

#[tokio::test]
async fn launch_fields_refuse_malformed_selected_values_before_catalog_or_directory_effects() {
    artifact_root_admission().await;
    let fixture = ServiceFixture::new();
    let base = launch_settings(
        &fixture,
        &fixture.0.join("codex.exe"),
        &format!("sha256:{}", "0".repeat(64)),
    );
    let malformed = [
        J::Null,
        false.into(),
        J::Number(0.0),
        "primitive".into(),
        J::Array(vec![]),
        J::object([] as [(&str, J); 0]),
    ];
    let mut cases = Vec::new();
    for (index, value) in malformed.iter().enumerate() {
        if value.as_object().is_none() {
            cases.push((
                format!("settings-{index}"),
                value.clone(),
                "invalid_arguments",
            ));
        }
        if value.as_object().is_none() {
            let mut settings = base.clone();
            settings.insert("appServer", value.clone());
            cases.push((format!("app-server-{index}"), settings, "invalid_arguments"));
        }
        if value.as_js_string().is_none() {
            let mut settings = base.clone();
            settings.insert("appServer", J::object([("mode", value.clone())]));
            cases.push((format!("mode-{index}"), settings, "invalid_arguments"));
        }
        for (field, code) in [
            ("nativeExecutable", "invalid_arguments"),
            ("nativeExecutableHash", "invalid_arguments"),
            ("nativeVersion", "native_version_unsupported"),
        ] {
            if value.as_js_string().is_some() {
                continue;
            }
            let mut settings = base.clone();
            settings.insert(field, value.clone());
            cases.push((format!("{field}-{index}"), settings, code));
        }
        for field in ["socketPath", "expectedCodexHome"] {
            let mut settings = base.clone();
            settings.insert(
                "appServer",
                J::object([("mode", "external-proxy".into()), (field, value.clone())]),
            );
            cases.push((format!("{field}-{index}"), settings, "invalid_arguments"));
        }
    }
    for (field, code) in [
        ("nativeExecutable", "invalid_arguments"),
        ("nativeExecutableHash", "invalid_arguments"),
        ("nativeVersion", "native_version_unsupported"),
    ] {
        let mut settings = base.clone();
        settings.remove(field);
        cases.push((format!("missing-{field}"), settings, code));
    }
    for (field, value, code) in [
        ("nativeExecutable", J::from("relative"), "invalid_arguments"),
        (
            "nativeVersion",
            "../escape".into(),
            "native_version_unsupported",
        ),
        (
            "codexHome",
            J::String(node_path::from_native(
                &fixture.0.join("artifact").join("home"),
            )),
            "invalid_arguments",
        ),
        (
            "codexHome",
            J::String(node_path::from_native(&fixture.0)),
            "invalid_arguments",
        ),
    ] {
        let mut settings = base.clone();
        settings.insert(field, value);
        cases.push((field.into(), settings, code));
    }
    let mut claude = base.clone();
    claude.remove("nativeExecutable");
    claude.insert("appServer", J::object([("mode", "claude-adapter".into())]));
    cases.push((
        "claude-transport".into(),
        claude,
        "native_transport_unavailable",
    ));
    for (name, settings, code) in cases {
        let error = launch(&fixture, &settings)
            .await
            .err()
            .expect("Preflight must refuse this fixture");
        assert_eq!(error.code, code, "{name}");
        assert!(!fixture.0.join("home").exists(), "{name}");
        assert!(!fixture.0.join("data").exists(), "{name}");
    }
    let fence = J::object([
        ("schemaVersion", J::Number(1.0)),
        ("owner", "native".into()),
        ("ownerId", "fixture-owner".into()),
        ("processId", J::Number(1.0)),
        ("observedAt", "2026-01-01T00:00:00.000Z".into()),
        ("code", "outcome_unknown".into()),
    ]);
    let data = fixture.0.join("data");
    process_fence::record_process_stop_fence(data.clone(), fence)
        .await
        .unwrap();
    let before = fs::read(data.join("process-stop-fence.json")).unwrap();
    let mut settings = base;
    settings.insert("nativeVersion", J::Null);
    assert_eq!(
        launch(&fixture, &settings).await.err().unwrap().code,
        "outcome_unknown"
    );
    assert_eq!(
        fs::read(data.join("process-stop-fence.json")).unwrap(),
        before
    );
    assert!(!fixture.0.join("home").exists());
}

#[cfg(windows)]
#[tokio::test]
async fn launch_preserves_catalog_executable_and_shell_preflight_without_account_execution() {
    let fixture = ServiceFixture::new();
    let binary = compile_peer(&fixture);
    let hash = ivy_host_runtime::streaming_hash::file_hash(&binary)
        .await
        .unwrap();
    let base = launch_settings(&fixture, &binary, &hash);
    let mut pinned = json::parse(
        include_str!("../../specs/native/codex-0.154.0/catalog.json"),
        usize::MAX,
    )
    .unwrap();
    pinned.insert("nativeExecutableHash", hash.clone().into());
    let path = ivy_contracts::checked_native_contract::native_catalog_path(
        &node_path::from_native(&fixture.0.join("artifact").join("dist")),
        &"0.154.0".into(),
        None,
        None,
    )
    .unwrap();
    let path = node_path::to_native(&path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let write_catalog =
        |catalog: &J| fs::write(&path, json::stringify(catalog, usize::MAX).unwrap()).unwrap();
    let wrong_hash = format!("sha256:{}", "0".repeat(64));
    for kind in [
        "configured-hash",
        "actual-bytes",
        "version",
        "realtime",
        "native-socket",
    ] {
        let mut settings = base.clone();
        let mut catalog = pinned.clone();
        match kind {
            "configured-hash" => settings.insert("nativeExecutableHash", wrong_hash.clone().into()),
            "actual-bytes" => {
                settings.insert("nativeExecutableHash", wrong_hash.clone().into());
                catalog.insert("nativeExecutableHash", wrong_hash.clone().into());
            }
            "version" => catalog.insert("version", "0.153.4".into()),
            "realtime" => catalog.insert("clientRequests", J::Array(vec![])),
            "native-socket" => {
                let mut socket = node_path::from_native(&fixture.0.join("proxy.sock"));
                socket.0.push(0xd800);
                settings.insert(
                    "appServer",
                    J::object([
                        ("mode", "external-proxy".into()),
                        ("socketPath", J::String(socket)),
                    ]),
                );
                settings.insert("nativeExecutableHash", wrong_hash.clone().into());
            }
            _ => unreachable!(),
        }
        write_catalog(&catalog);
        assert_eq!(
            launch(&fixture, &settings).await.err().unwrap().code,
            "native_build_mismatch",
            "{kind}"
        );
        assert!(!fixture.0.join("home").exists());
        assert!(!fixture.0.join("data").exists());
    }
    write_catalog(&pinned);
    for server in [
        None,
        Some(J::object([] as [(&str, J); 0])),
        Some(J::object([
            ("mode", "owned-stdio".into()),
            ("socketPath", false.into()),
            ("expectedCodexHome", J::Null),
        ])),
        Some(J::object([
            ("mode", "future-mode".into()),
            ("socketPath", false.into()),
            ("expectedCodexHome", J::Null),
        ])),
        Some(J::object([("mode", "external-proxy".into())])),
    ] {
        let mut settings = base.clone();
        settings.insert("unused", J::String(JsString(vec![0xd800])));
        if let Some(server) = server {
            settings.insert("appServer", server);
        } else {
            settings.remove("appServer");
        }
        assert_eq!(
            launch(&fixture, &settings).await.err().unwrap().code,
            "native_shell_required"
        );
        assert!(fixture.0.join("home").is_dir());
        assert!(fixture.0.join("data").is_dir());
    }
    let other_home = fixture.0.join("other-home");
    fs::create_dir(&other_home).unwrap();
    let mut shared = base.clone();
    shared.insert(
        "appServer",
        J::object([
            ("mode", "external-proxy".into()),
            (
                "expectedCodexHome",
                J::String(node_path::from_native(&other_home)),
            ),
        ]),
    );
    assert_eq!(
        launch(&fixture, &shared).await.err().unwrap().code,
        "native_home_mismatch"
    );
    assert!(!fixture.0.join("home").join("frames.jsonl").exists());
    assert!(fs::read_dir(&other_home).unwrap().next().is_none());
    job_path_selection(&fixture, &binary, &pinned, &base).await;
}

async fn launch_with_artifact(
    fixture: &ServiceFixture,
    settings: &J,
    artifact: &Path,
) -> IvyResult<codex_connection::NativeConnection> {
    let paths = node_path::Context::current().unwrap();
    let environment = BTreeMap::new();
    let account_home = node_path::from_native(&fixture.0.join("account"));
    let context = HomeContext {
        paths: &paths,
        environment: &environment,
        user_home: None,
        cwd: &paths.cwd,
        account_home: &account_home,
    };
    codex_process::start_codex_process(codex_process::StartOptions {
        settings,
        artifact_root: &node_path::from_native(artifact),
        data_root: &node_path::from_native(&fixture.0.join("data")),
        home_context: &context,
        client_name: client_name(),
        client_version: "1.0.0".into(),
        hive_credential: None,
        events: Events::default(),
    })
    .await
}
async fn artifact_root_admission() {
    let fixture = ServiceFixture::new();
    let settings = launch_settings(
        &fixture,
        &fixture.0.join("not-launched.exe"),
        &format!("sha256:{}", "0".repeat(64)),
    );
    let mut roots = vec![PathBuf::new(), PathBuf::from("relative/artifact")];
    #[cfg(windows)]
    {
        roots.push(PathBuf::from(r"\root-relative"));
        roots.push(PathBuf::from("D:drive-relative"));
    }
    for root in roots {
        assert_eq!(
            launch_with_artifact(&fixture, &settings, &root)
                .await
                .err()
                .unwrap()
                .code,
            "invalid_arguments"
        );
        assert!(!fixture.0.join("home").exists());
        assert!(!fixture.0.join("data").exists());
    }
}

#[cfg(windows)]
fn artifact_root(fixture: &ServiceFixture, index: usize, variant: &str) -> (PathBuf, PathBuf) {
    use std::os::windows::ffi::OsStringExt;
    let parent = fixture.0.join("artifact-cases").join(index.to_string());
    fs::create_dir_all(&parent).unwrap();
    let root = match variant {
        "unicode" => parent.join("owned 目录-🚀"),
        "opaque-high" | "opaque-low" => {
            let mut units: Vec<_> = "owned".encode_utf16().collect();
            units.push(if variant == "opaque-high" {
                0xd800
            } else {
                0xdfff
            });
            fs::canonicalize(&parent)
                .unwrap()
                .join(OsString::from_wide(&units))
        }
        _ => parent.join("owned"),
    };
    fs::create_dir(&root).unwrap();
    let configured = match variant {
        "canonical" => fs::canonicalize(&root).unwrap(),
        "linked" => {
            let alias = parent.join("linked");
            ivy_host_runtime::filesystem::directory_link(&root, &alias).unwrap();
            alias
        }
        "missing-parent" => parent.join("uncreated/../owned"),
        "junction-parent" => {
            let target = parent.join("physical/target");
            fs::create_dir_all(&target).unwrap();
            let alias = parent.join("junction");
            ivy_host_runtime::filesystem::directory_link(&target, &alias).unwrap();
            alias.join("../owned")
        }
        _ => root.clone(),
    };
    (root, configured)
}
#[cfg(windows)]
async fn job_path_selection(fixture: &ServiceFixture, binary: &Path, pinned: &J, base: &J) {
    let shell_root = fixture.0.join("shell");
    fs::create_dir(&shell_root).unwrap();
    let shell = shell_root.join("pwsh.exe");
    fs::copy(binary, &shell).unwrap();
    let marker = shell_root.join("shell-probe-called");
    let observation = J::object([
        ("packageStatus", J::Number(0.0)),
        ("version", "7.6.0".into()),
        ("executable", J::String(node_path::from_native(&shell))),
    ]);
    fs::write(
        shell_root.join("shell-probe.json"),
        json::stringify(&observation, usize::MAX).unwrap(),
    )
    .unwrap();
    let mut settings = base.clone();
    settings.insert(
        "windowsShell",
        shell_settings(
            &shell,
            &base
                .required("nativeExecutableHash")
                .unwrap()
                .as_str()
                .unwrap(),
        ),
    );
    let image = job_launcher();
    let image_hash = ivy_host_runtime::streaming_hash::file_hash(&image)
        .await
        .unwrap();
    let catalog_bytes = json::stringify(pinned, usize::MAX).unwrap().into_bytes();
    for (index, variant) in [
        "plain",
        "unicode",
        "canonical",
        "linked",
        "missing-parent",
        "junction-parent",
        "opaque-high",
        "opaque-low",
        "missing-helper",
        "directory-helper",
        "invalid-helper",
    ]
    .into_iter()
    .enumerate()
    {
        let (root, configured) = artifact_root(fixture, index, variant);
        let catalog = ivy_contracts::checked_native_contract::native_catalog_path(
            &node_path::from_native(&root.join("dist")),
            &"0.154.0".into(),
            None,
            None,
        )
        .unwrap();
        let catalog = node_path::to_native(&catalog);
        fs::create_dir_all(catalog.parent().unwrap()).unwrap();
        fs::write(&catalog, &catalog_bytes).unwrap();
        let helper = root.join("dist/native/ivy-job.exe");
        fs::create_dir_all(helper.parent().unwrap()).unwrap();
        match variant {
            "missing-helper" => {}
            "directory-helper" => fs::create_dir(&helper).unwrap(),
            "invalid-helper" => fs::write(&helper, b"not an executable image").unwrap(),
            _ => {
                fs::copy(&image, &helper).unwrap();
            }
        }
        let error = launch_with_artifact(fixture, &settings, &configured)
            .await
            .err()
            .expect("The disposable peer must refuse during preflight");
        match variant {
            "missing-helper" => {
                assert_eq!(error.code, "internal_error", "{variant}");
                assert!(!marker.exists());
            }
            "directory-helper" | "invalid-helper" => {
                assert_eq!(error.code, "spawn_failed", "{variant}");
                assert!(!marker.exists());
            }
            _ => {
                assert_eq!(error.code, "native_shell_packaged", "{variant}");
                assert_eq!(fs::read(&marker).unwrap(), b"called");
                fs::remove_file(&marker).unwrap();
                assert_eq!(
                    ivy_host_runtime::streaming_hash::file_hash(&helper)
                        .await
                        .unwrap(),
                    image_hash
                );
            }
        }
        assert_eq!(fs::read(&catalog).unwrap(), catalog_bytes);
        assert!(!fixture.0.join("home/frames.jsonl").exists());
        assert!(!root.join("frames.jsonl").exists());
        assert!(!root.parent().unwrap().join("uncreated").exists());
    }
    assert_eq!(
        ivy_host_runtime::streaming_hash::file_hash(&image)
            .await
            .unwrap(),
        image_hash
    );
    assert!(!marker.exists());
}
