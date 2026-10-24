use super::*;
use ivy_contracts::{core_schema::validate_host, hash::hash_json};
use ivy_host_runtime::{IvyError, Outcome};

async fn native_fixture() -> Fixture {
    let root = ServiceFixture::new();
    let source = root.0.join("checkout");
    fs::create_dir_all(source.join("packages")).unwrap();
    let mut locate = Command::new(if cfg!(windows) { "where.exe" } else { "which" });
    locate.arg("git");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        locate.creation_flags(0x08000000);
    }
    let git = locate.output().unwrap();
    assert!(git.status.success());
    let git = PathBuf::from(
        String::from_utf8(git.stdout)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    );
    let node = PathBuf::from(std::env::var_os("IVY_TEST_NODE").expect("Pinned Node path required"));
    let mut config = host_configuration(&root);
    config.insert(
        "executables",
        IvyJson::object([("node", text(&node)), ("git", text(&git))]),
    );
    let configuration = root.0.join("host.json");
    config.insert("configPath", text(&configuration));
    config::atomic_json(configuration, config.clone())
        .await
        .unwrap();
    #[cfg(windows)]
    {
        fs::create_dir_all(root.0.join("dist/native")).unwrap();
        fs::copy(
            env!("CARGO_BIN_EXE_ivy-job"),
            root.0.join("dist/native/ivy-job.exe"),
        )
        .unwrap();
    }
    let fixture = Fixture {
        root,
        config,
        git,
        source,
    };
    fixture.write(
        "package.json",
        b"{\"name\":\"fixture\",\"version\":\"1.0.0\",\"private\":true}\n",
    );
    fixture.write(
        "package-lock.json",
        b"{\"name\":\"fixture\",\"lockfileVersion\":3,\"packages\":{}}\n",
    );
    fixture.write(
        ".gitignore",
        b"node_modules/\n.env\nconfig.json\ngenerated/\n",
    );
    fixture.write("packages/kept.ts", b"before\n");
    fixture.write("packages/deleted.ts", b"removed\n");
    fixture.git(&["init", "--quiet"]);
    fixture.git(&["add", "."]);
    fixture.write("packages/kept.ts", b"changed\r\n");
    fs::remove_file(fixture.source.join("packages/deleted.ts")).unwrap();
    fixture
}
fn action(source: Option<JsString>) -> LocalAction {
    LocalAction {
        action: Action::Deploy,
        instance_id: "fixture".into(),
        operation_id: "source-path".into(),
        candidate_id: None,
        source,
        target_build: None,
    }
}
fn relative(path: &Path) -> PathBuf {
    let cwd = std::env::current_dir().unwrap();
    let mut base = cwd.as_path();
    let mut result = PathBuf::new();
    while !path.starts_with(base) {
        base = base
            .parent()
            .expect("Fixture and CWD must share a native root");
        result.push("..");
    }
    result.push(path.strip_prefix(base).unwrap());
    result
}
fn source_name(opaque: Option<u16>) -> std::ffi::OsString {
    let name = std::ffi::OsString::from("source-目录-🚀");
    #[cfg(windows)]
    let name = {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let mut units: Vec<_> = name.encode_wide().collect();
        if let Some(unit) = opaque {
            units.push(unit);
        }
        std::ffi::OsString::from_wide(&units)
    };
    #[cfg(not(windows))]
    let _ = opaque;
    name
}
fn input_paths() {
    let fixture = ServiceFixture::new();
    let mut variants = vec![
        "absolute",
        "relative",
        "missing-parent",
        "junction-parent",
        "canonical",
        "trailing",
        "dot",
        "unicode",
        "nul",
    ];
    #[cfg(windows)]
    variants.extend([
        "root-relative",
        "drive-relative",
        "opaque-high",
        "opaque-low",
    ]);
    for (index, variant) in variants.into_iter().enumerate() {
        let parent = fixture.0.join(index.to_string());
        let name = source_name(match variant {
            "opaque-high" => Some(0xd800),
            "opaque-low" => Some(0xdfff),
            _ => None,
        });
        let source = parent.join(&name);
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("retained"), b"unrelated source bytes").unwrap();
        let mut expected = source.clone();
        let input = match variant {
            "relative" => relative(&source),
            "missing-parent" => parent.join("uncreated/../.").join(&name),
            "junction-parent" => {
                let target = parent.join("physical/target");
                fs::create_dir_all(&target).unwrap();
                let link = parent.join("link");
                ivy_host_runtime::filesystem::directory_link(&target, &link).unwrap();
                link.join("..").join(&name)
            }
            "canonical" => {
                expected = fs::canonicalize(&source).unwrap();
                expected.clone()
            }
            "trailing" => {
                let mut path = source.as_os_str().to_owned();
                path.push(std::path::MAIN_SEPARATOR_STR);
                PathBuf::from(path)
            }
            "dot" => source.join("."),
            "nul" => {
                expected = std::env::current_dir().unwrap().join("invalid\0source");
                PathBuf::from("invalid\0source")
            }
            #[cfg(windows)]
            "root-relative" => {
                use std::os::windows::ffi::{OsStrExt, OsStringExt};
                let units: Vec<_> = source.as_os_str().encode_wide().skip(2).collect();
                PathBuf::from(std::ffi::OsString::from_wide(&units))
            }
            #[cfg(windows)]
            "drive-relative" => {
                use std::os::windows::ffi::{OsStrExt, OsStringExt};
                let units: Vec<_> = source.as_os_str().encode_wide().take(2).collect();
                let mut path = std::ffi::OsString::from_wide(&units);
                path.push(relative(&source));
                PathBuf::from(path)
            }
            _ => source.clone(),
        };
        let source_value = node_path::from_native(&input);
        let request = action(Some(source_value.clone())).request().unwrap();
        assert_json_eq(
            &request,
            &IvyJson::object([
                ("action", "deploy".into()),
                ("instanceId", "fixture".into()),
                ("operationId", "source-path".into()),
                ("source", text(&expected)),
            ]),
        );
        assert_eq!(
            request
                .as_object()
                .unwrap()
                .iter()
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>(),
            ["action", "instanceId", "operationId", "source"]
                .map(JsString::from)
                .to_vec()
        );
        let args = IvyJson::object([
            ("instanceId", "fixture".into()),
            ("operationId", "source-path".into()),
            ("source", IvyJson::String(source_value)),
        ]);
        let managed = LocalAction::from_manager(Action::Deploy, &args, Some(&"source-path".into()))
            .unwrap()
            .request()
            .unwrap();
        assert_json_eq(&managed, &request);
        assert_eq!(
            fs::read(source.join("retained")).unwrap(),
            b"unrelated source bytes"
        );
        assert!(!parent.join("uncreated").exists());
    }
}
#[test]
fn manager_action_admission_binds_original_operation_before_any_io() {
    input_paths();
    let args = IvyJson::object([
        ("instanceId", "fixture".into()),
        ("operationId", "local-operation".into()),
        ("source", "relative-checkout".into()),
    ]);
    assert_eq!(
        LocalAction::from_manager(Action::Deploy, &args, Some(&"other-operation".into()))
            .err()
            .unwrap()
            .code,
        "mutation_conflict"
    );
    let request = LocalAction::from_manager(Action::Deploy, &args, Some(&"local-operation".into()))
        .unwrap()
        .request()
        .unwrap();
    assert_eq!(
        request.get("source"),
        Some(&text(
            &std::env::current_dir().unwrap().join("relative-checkout")
        ))
    );
    assert_eq!(
        LocalAction::from_manager(Action::Rollback, &args, None)
            .err()
            .unwrap()
            .message,
        "This option does not belong to the selected command."
    );
    let mut values = args;
    values.insert("source", IvyJson::Null);
    values.insert("candidateId", IvyJson::Number(7.0));
    assert_eq!(
        LocalAction::from_manager(Action::Deploy, &values, None)
            .err()
            .unwrap()
            .code,
        "invalid_arguments"
    );
    values.insert("candidateId", "7".into());
    let request = LocalAction::from_manager(Action::Deploy, &values, None)
        .unwrap()
        .request()
        .unwrap();
    assert_eq!(request.get("candidateId"), Some(&"7".into()));
    assert!(request.get("source").is_none());
    for empty in [None, Some(JsString::default())] {
        assert!(action(empty).request().unwrap().get("source").is_none());
    }
    for kind in [
        Action::Deploy,
        Action::Rollback,
        Action::Restart,
        Action::Enable,
        Action::Disable,
    ] {
        let mut request = action(None);
        request.action = kind;
        let request = request.request().unwrap();
        assert_eq!(request.get("action"), Some(&kind.as_str().into()));
        assert!(request.get("source").is_none());
    }
    for (instance, operation, message) in [
        ("", "", "Missing --instance."),
        ("", "operation", "Missing --instance."),
        ("fixture", "", "Missing --operation-id."),
    ] {
        let mut request = action(Some("invalid\0source".into()));
        request.instance_id = instance.into();
        request.operation_id = operation.into();
        let error = request.request().unwrap_err();
        assert_eq!(error.code, "invalid_arguments");
        assert_eq!(error.message, message);
        assert_eq!(error.outcome, Outcome::NotExecuted);
    }
    let mut request = action(Some("relative-checkout".into()));
    request.instance_id = JsString(vec![0xd800, b'i' as u16]);
    request.operation_id = JsString(vec![0xdfff, b'o' as u16]);
    request.candidate_id = Some("candidate".into());
    request.target_build = Some("target".into());
    let request = request.request().unwrap();
    assert_eq!(
        request.get("instanceId"),
        Some(&IvyJson::String(JsString(vec![0xd800, b'i' as u16])))
    );
    assert_eq!(
        request.get("operationId"),
        Some(&IvyJson::String(JsString(vec![0xdfff, b'o' as u16])))
    );
    assert_eq!(
        request
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>(),
        [
            "action",
            "instanceId",
            "operationId",
            "candidateId",
            "targetBuild",
            "source"
        ]
        .map(JsString::from)
        .to_vec()
    );
}
#[tokio::test]
async fn bootstrap_owner_guard_precedes_source_capture_and_matches_original_cli() {
    for component in ["host-executor", "service-manager"] {
        if component == "service-manager" && !cfg!(windows) {
            continue;
        }
        for variant in ["missing-parent", "nul"] {
            let mut f = native_fixture().await;
            let mut instances = f
                .config
                .required("instances")
                .unwrap()
                .as_array()
                .unwrap()
                .to_vec();
            instances[0].insert("componentId", component.into());
            f.config.insert("instances", IvyJson::Array(instances));
            config::atomic_json(
                path(f.config.required("configPath").unwrap()),
                f.config.clone(),
            )
            .await
            .unwrap();
            let journal = f.journal().await;
            seed(&journal).await;
            let mut request = f.action(Action::Deploy, "bootstrap-guard");
            request.source = Some(if variant == "nul" {
                "invalid\0source".into()
            } else {
                node_path::from_native(&f.root.0.join("uncreated/../absent"))
            });
            let error = actions::accept_local_action(journal.clone(), request, f.root.0.clone())
                .await
                .unwrap_err();
            assert_eq!(
                error,
                IvyError::new(
                    "restart_required",
                    "Bootstrap-owned releases use prepare followed by install-bootstrap; normal deployment cannot observe their OS-owned drain."
                )
            );
            assert!(!path(f.config.required("stagingRoot").unwrap()).exists());
            assert!(
                journal
                    .operation("bootstrap-guard")
                    .await
                    .unwrap()
                    .is_none()
            );
            assert!(!f.root.0.join("uncreated").exists());
            f.no_incoming();
            journal.close().await.unwrap();
        }
    }
}
#[tokio::test]
async fn local_source_replay_after_checkout_loss_keeps_the_exact_durable_entry() {
    let f = native_fixture().await;
    let journal = f.journal().await;
    let canonical_source = fs::canonicalize(&f.source).unwrap();
    let mut request = f.action(Action::Deploy, "source-fixed");
    request.source = Some(node_path::from_native(
        &f.root.0.join("uncreated/../checkout"),
    ));
    let accepted = actions::accept_local_action(journal.clone(), request, f.root.0.clone())
        .await
        .unwrap();
    validate_host("JournalEntry", &accepted).unwrap();
    assert_eq!(
        accepted.required("request").unwrap().get("source"),
        Some(&text(&f.source))
    );
    assert_eq!(
        accepted.required("record").unwrap().get("phase"),
        Some(&"preparing".into())
    );
    let snapshot = accepted.required("sourceSnapshot").unwrap().clone();
    assert_eq!(
        path(snapshot.required("originalRoot").unwrap()),
        canonical_source
    );
    let before = view(&snapshot);
    let manifest = path(snapshot.required("manifestPath").unwrap());
    let manifest_bytes = fs::read(&manifest).unwrap();
    let retained_source = path(snapshot.required("sourceRoot").unwrap());
    assert_eq!(
        fs::read(retained_source.join("packages/kept.ts")).unwrap(),
        b"changed\r\n"
    );
    assert!(!retained_source.join("packages/deleted.ts").exists());
    assert!(!f.root.0.join("uncreated").exists());
    let owner = fs::canonicalize(&f.root.0).unwrap();
    assert!(canonical_source.starts_with(&owner) && canonical_source != owner);
    fs::remove_dir_all(&canonical_source).unwrap();
    journal.close().await.unwrap();
    let journal = f.journal().await;
    let mut request = f.action(Action::Deploy, "source-fixed");
    request.source = Some(node_path::from_native(&relative(&f.source)));
    let replay = actions::accept_local_action(journal.clone(), request, f.root.0.clone())
        .await
        .unwrap();
    assert_json_eq(&replay, &accepted);
    let mut changed = f.action(Action::Deploy, "source-fixed");
    changed.source = Some(node_path::from_native(&f.root.0.join("other")));
    assert_eq!(
        actions::accept_local_action(journal.clone(), changed, f.root.0.clone())
            .await
            .unwrap_err()
            .code,
        "mutation_conflict"
    );
    let mut conflicting = f.action(Action::Deploy, "candidate-and-source");
    conflicting.source = Some(node_path::from_native(&f.source));
    conflicting.candidate_id = Some(hash().as_js_string().unwrap().clone());
    assert_eq!(
        actions::accept_local_action(journal.clone(), conflicting, f.root.0.clone())
            .await
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    assert!(
        journal
            .operation("candidate-and-source")
            .await
            .unwrap()
            .is_none()
    );
    assert_json_eq(&view(&snapshot), &before);
    assert_eq!(fs::read(&manifest).unwrap(), manifest_bytes);
    assert_eq!(
        fs::read_dir(path(f.config.required("stagingRoot").unwrap()).join("sources"))
            .unwrap()
            .count(),
        1
    );
    f.no_incoming();
    journal.close().await.unwrap();
}
#[tokio::test]
async fn dropped_action_waiter_retains_capture_until_the_original_journal_write_finishes() {
    let f = native_fixture().await;
    let journal = f.journal().await;
    let lock = ivy_store::rusqlite::Connection::open(
        path(f.config.required("runtimeRoot").unwrap()).join("deployments.sqlite"),
    )
    .unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut request = f.action(Action::Deploy, "dropped-source");
    request.source = Some(node_path::from_native(
        &f.root.0.join("uncreated/../checkout"),
    ));
    let owned = journal.clone();
    let bootstrap = f.root.0.clone();
    let waiter =
        tokio::spawn(async move { actions::accept_local_action(owned, request, bootstrap).await });
    let sources = path(f.config.required("stagingRoot").unwrap()).join("sources");
    tokio::time::timeout(Duration::from_secs(8), async {
        while !sources.is_dir() || fs::read_dir(&sources).unwrap().count() != 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    waiter.abort();
    let _ = waiter.await;
    lock.execute_batch("COMMIT").unwrap();
    let entry = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(entry) = journal.operation("dropped-source").await.unwrap() {
                break entry;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    validate_host("JournalEntry", &entry).unwrap();
    assert_eq!(
        entry.required("record").unwrap().get("phase"),
        Some(&"preparing".into())
    );
    assert_eq!(
        entry.required("request").unwrap().get("source"),
        Some(&text(&f.source))
    );
    assert_eq!(
        entry.required("record").unwrap().get("requestHash"),
        Some(
            &hash_json(entry.required("request").unwrap())
                .unwrap()
                .into()
        )
    );
    assert_eq!(fs::read_dir(&sources).unwrap().count(), 1);
    let before = view(entry.required("sourceSnapshot").unwrap());
    let mut request = f.action(Action::Deploy, "dropped-source");
    request.source = Some(node_path::from_native(&f.source));
    assert_json_eq(
        &actions::accept_local_action(journal.clone(), request, f.root.0.clone())
            .await
            .unwrap(),
        &entry,
    );
    assert_json_eq(&view(entry.required("sourceSnapshot").unwrap()), &before);
    assert_eq!(fs::read_dir(sources).unwrap().count(), 1);
    f.no_incoming();
    journal.close().await.unwrap();
}
