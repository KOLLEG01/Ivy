use super::*;
use ivy_host_runtime::portable_native_files::NativeHelperLayout;

fn inspector() -> NativeOfflineHiveInspector {
    NativeOfflineHiveInspector::new(
        preparation().node,
        PathBuf::from(env!("CARGO_BIN_EXE_ivy-job")),
        NativeHelperLayout::Target,
    )
}
fn request(f: &Fixture, target: &IvyJson, contracts: IvyJson) -> OfflineInspection {
    OfflineInspection {
        configuration_path: native(target.required("configPath").unwrap()),
        artifact_root: native(f.candidate.required("artifactRoot").unwrap()),
        contracts,
        host: f.host.clone(),
    }
}
#[tokio::test]
async fn native_offline_inspector_retains_configuration_build_and_explicit_payload_admission() {
    for case in [
        "missing-target",
        "build-mismatch",
        "oversized-config",
        "wrong-artifact",
    ] {
        let f = Fixture::new("hive", |host, _, manifest| hive(host, manifest, None)).await;
        let target = materialize(&f, false).await;
        let root = native(f.candidate.required("artifactRoot").unwrap());
        let config_path = native(target.required("configPath").unwrap());
        let mut invocation = request(&f, &target, IvyJson::Array(vec![]));
        match case {
            "missing-target" => {
                fs::create_dir_all(root.join("dist/native")).unwrap();
                fs::write(
                    root.join(if cfg!(windows) {
                        "dist/native/ivy-hive-inspect-storage.exe"
                    } else {
                        "dist/native/ivy-hive-inspect-storage"
                    }),
                    b"flat payload must never substitute for target payload",
                )
                .unwrap();
            }
            "build-mismatch" => fs::write(
                root.join("dist/build-info.json"),
                r#"{"buildId":"other","version":"1.0.0"}"#,
            )
            .unwrap(),
            "oversized-config" => {
                fs::write(&config_path, " ".repeat(config::CONFIGURATION_BYTES + 1)).unwrap()
            }
            "wrong-artifact" => invocation.artifact_root = f.root.0.join("another-release"),
            _ => unreachable!(),
        }
        let error = inspector().inspect(invocation).await.unwrap_err();
        assert_eq!(
            error.code,
            match case {
                "missing-target" => "artifact_missing",
                "build-mismatch" => "build_mismatch",
                "oversized-config" => "invalid_arguments",
                "wrong-artifact" => "target_conflict",
                _ => unreachable!(),
            },
            "{case}"
        );
        assert!(f.journal.target("fixture").await.unwrap().is_none());
        f.close().await;
    }
}

#[cfg(all(windows, target_arch = "x86_64"))]
fn owner_oracle(config_path: &Path, contracts: &IvyJson) -> std::process::Output {
    let input = IvyJson::object([
        ("mode", "offline-inspection".into()),
        ("configurationPath", path(config_path)),
        ("contracts", contracts.clone()),
    ]);
    let mut child = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/host-executor-runtime-oracle.mjs",
        ])
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json::stringify(&input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

// This check consumes an independently built Hive owner. The normal Host-only
// test target has no Hive crate dependency or embedded foreign executable.
#[cfg(all(windows, target_arch = "x86_64"))]
#[tokio::test]
#[ignore = "requires pinned IVY_HOST_OFFLINE_HIVE_INSPECTOR and IVY_HOST_OFFLINE_HIVE_INSPECTOR_SHA256"]
async fn native_offline_hive_owner_matches_original_cli_with_real_sqlite_and_keeps_bytes() {
    use ivy_store::rusqlite::Connection;
    use sha2::{Digest, Sha256};
    let executable = PathBuf::from(
        std::env::var_os("IVY_HOST_OFFLINE_HIVE_INSPECTOR").expect("explicit owner executable"),
    );
    let expected =
        std::env::var("IVY_HOST_OFFLINE_HIVE_INSPECTOR_SHA256").expect("explicit owner digest");
    let bytes = fs::read(&executable).unwrap();
    assert_eq!(
        Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        expected
    );
    for case in [
        "missing",
        "historical",
        "scoped-current",
        "invalid-format",
        "exclusive-writer",
        "large-config",
    ] {
        let f = Fixture::new("hive", |host, _, manifest| {
            hive(host, manifest, None);
            manifest.insert(
                "storage",
                v(r#"{"minReadableFormat":4,"maxReadableFormat":4,"writeFormat":4}"#),
            );
        })
        .await;
        let target = materialize(&f, false).await;
        let root = native(f.candidate.required("artifactRoot").unwrap());
        let payload = root.join("dist/native/win32-x64/ivy-hive-inspect-storage.exe");
        fs::create_dir_all(payload.parent().unwrap()).unwrap();
        fs::copy(&executable, payload).unwrap();
        let config_path = native(target.required("configPath").unwrap());
        let mut config = load_json(&config_path);
        if case == "large-config" {
            nested(
                &mut config,
                "settings",
                "fixtureLargeValue",
                "x".repeat(70_000).into(),
            );
            fs::write(&config_path, json::stringify(&config, usize::MAX).unwrap()).unwrap();
        }
        let data_root = native(config.required("dataRoot").unwrap());
        fs::create_dir_all(&data_root).unwrap();
        let database = data_root.join("hive.sqlite");
        let db = (case != "missing").then(|| {
            let db = Connection::open(&database).unwrap();
            db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA user_version=4;
                CREATE TABLE objects(id TEXT PRIMARY KEY,parent_id TEXT,effective_archive INTEGER,current_revision INTEGER);
                CREATE TABLE revisions(object_id TEXT,revision INTEGER,contract_key TEXT,contract_version TEXT);
                INSERT INTO objects VALUES('11111111-2222-4333-8444-555555555555',NULL,0,2);
                INSERT INTO revisions VALUES('11111111-2222-4333-8444-555555555555',1,'fixture.contract','1.0.0');
                INSERT INTO revisions VALUES('11111111-2222-4333-8444-555555555555',2,'fixture.contract','2.0.0');").unwrap();
            if case == "invalid-format" { db.pragma_update(None, "user_version", 99).unwrap(); }
            if case == "exclusive-writer" { db.execute_batch("BEGIN EXCLUSIVE").unwrap(); }
            db
        });
        let contracts = if case == "scoped-current" {
            v(
                r#"[{"key":"fixture.contract","readScope":{"roots":["11111111-2222-4333-8444-555555555555"],"history":"current","includeArchived":false,"references":[]}}]"#,
            )
        } else {
            v(r#"[{"key":"fixture.contract"}]"#)
        };
        let before = fs::read(&database).ok();
        let files = fs::read_dir(&data_root)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect::<Vec<_>>();
        let actual = inspector()
            .inspect(request(&f, &target, contracts.clone()))
            .await;
        let original = owner_oracle(&config_path, &contracts);
        let owner_value = json::parse(
            std::str::from_utf8(if original.status.success() {
                &original.stdout
            } else {
                &original.stderr
            })
            .unwrap(),
            65536,
        )
        .unwrap();
        let expected = if original.status.success() {
            assert!(original.stderr.is_empty());
            IvyJson::object([("ok", true.into()), ("inspection", owner_value.clone())])
        } else {
            assert_eq!(original.status.code(), Some(1));
            assert!(original.stdout.is_empty());
            assert_eq!(
                owner_value.required("message").unwrap(),
                &"Hive storage inspection failed without mutation.".into()
            );
            IvyJson::object([
                ("ok", false.into()),
                (
                    "error",
                    IvyError::new(
                        "command_failed",
                        "Command failed; bounded diagnostics record the outcome.",
                    )
                    .with_outcome(ivy_contracts::Outcome::Unknown)
                    .with_details(v(r#"{"exitCode":1,"signal":null}"#))
                    .to_wire(),
                ),
            ])
        };
        let actual = match actual {
            Ok(value) => IvyJson::object([("ok", true.into()), ("inspection", value)]),
            Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
        };
        pair(&format!("offline-{case}"), &actual, &expected);
        assert_eq!(fs::read(&database).ok(), before, "{case}");
        assert_eq!(
            fs::read_dir(&data_root)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect::<Vec<_>>(),
            files,
            "{case}"
        );
        assert!(f.journal.target("fixture").await.unwrap().is_none());
        assert!(f.journal.installed("fixture").await.unwrap().is_none());
        if let Some(db) = db {
            if case == "exclusive-writer" {
                db.execute_batch("ROLLBACK").unwrap();
            }
            drop(db);
        }
        f.close().await;
    }
}

#[cfg(all(windows, target_arch = "x86_64"))]
#[tokio::test]
#[ignore = "requires pinned IVY_HOST_OFFLINE_HIVE_INSPECTOR and IVY_HOST_OFFLINE_HIVE_INSPECTOR_SHA256"]
async fn native_compatibility_invokes_real_offline_owner_after_stopped_admission() {
    use sha2::{Digest, Sha256};
    let executable = PathBuf::from(std::env::var_os("IVY_HOST_OFFLINE_HIVE_INSPECTOR").unwrap());
    assert_eq!(
        Sha256::digest(fs::read(&executable).unwrap())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        std::env::var("IVY_HOST_OFFLINE_HIVE_INSPECTOR_SHA256").unwrap()
    );
    let f = Fixture::new("hive", |host, _, manifest| {
        hive(host, manifest, None);
        manifest.insert(
            "storage",
            v(r#"{"minReadableFormat":4,"maxReadableFormat":4,"writeFormat":4}"#),
        );
    })
    .await;
    let root = native(f.candidate.required("artifactRoot").unwrap());
    let payload = root.join("dist/native/win32-x64/ivy-hive-inspect-storage.exe");
    fs::create_dir_all(payload.parent().unwrap()).unwrap();
    fs::copy(&executable, &payload).unwrap();
    observe(&f, None, "idle", true).await;
    let runtime = NativeActivationRuntime::new(
        f.root.0.clone(),
        preparation(),
        PathBuf::from(env!("CARGO_BIN_EXE_ivy-job")),
        Arc::new(inspector()),
    )
    .unwrap();
    runtime
        .compatibility(&f.context, &hash().as_str().unwrap(), None)
        .await
        .unwrap();
    assert!(f.journal.target("fixture").await.unwrap().is_none());
    assert!(f.journal.installed("fixture").await.unwrap().is_none());
    assert!(f.inspector.calls.lock().unwrap().is_empty());
    runtime.dispose().await.unwrap();
    f.close().await;
}

#[cfg(windows)]
fn held_process(pid: u32) -> std::os::windows::io::OwnedHandle {
    use std::os::windows::io::FromRawHandle;
    let raw = unsafe {
        windows_sys::Win32::System::Threading::OpenProcess(
            windows_sys::Win32::System::Threading::PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    assert!(!raw.is_null(), "{}", std::io::Error::last_os_error());
    unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(raw) }
}
#[cfg(windows)]
fn signaled(handle: &std::os::windows::io::OwnedHandle) -> bool {
    use std::os::windows::io::AsRawHandle;
    unsafe {
        windows_sys::Win32::System::Threading::WaitForSingleObject(handle.as_raw_handle(), 0)
            == windows_sys::Win32::Foundation::WAIT_OBJECT_0
    }
}
#[cfg(windows)]
#[tokio::test]
#[ignore = "requires pinned IVY_HOST_OFFLINE_INSPECTOR_FIXTURE and IVY_HOST_OFFLINE_INSPECTOR_FIXTURE_SHA256"]
async fn native_inspector_deadline_and_dropped_waiter_retain_the_whole_child_tree_and_early_exit() {
    let executable = PathBuf::from(
        std::env::var_os("IVY_HOST_OFFLINE_INSPECTOR_FIXTURE")
            .expect("explicit controlled inspector executable"),
    );
    assert_eq!(
        ivy_contracts::hash::digest(fs::read(&executable).unwrap()),
        std::env::var("IVY_HOST_OFFLINE_INSPECTOR_FIXTURE_SHA256").unwrap()
    );
    for case in ["deadline", "blocked-input", "dropped-waiter", "early-exit"] {
        let f = Fixture::new("hive", |host, _, manifest| hive(host, manifest, None)).await;
        let target = materialize(&f, false).await;
        let root = native(f.candidate.required("artifactRoot").unwrap());
        let platform = if cfg!(target_arch = "aarch64") {
            "win32-arm64"
        } else {
            "win32-x64"
        };
        let payload = root.join(format!(
            "dist/native/{platform}/ivy-hive-inspect-storage.exe"
        ));
        fs::create_dir_all(payload.parent().unwrap()).unwrap();
        fs::copy(&executable, payload).unwrap();
        let config_path = native(target.required("configPath").unwrap());
        let mut config = load_json(&config_path);
        nested(
            &mut config,
            "settings",
            "fixtureLargeValue",
            "x".repeat(70_000).into(),
        );
        fs::write(&config_path, json::stringify(&config, usize::MAX).unwrap()).unwrap();
        let invocation = request(
            &f,
            &target,
            IvyJson::Array(vec![IvyJson::object([(
                "key",
                format!("fixture.{case}").into(),
            )])]),
        );
        let mut waiter = tokio::spawn(async move { inspector().inspect(invocation).await });
        if case == "early-exit" {
            let error = tokio::time::timeout(Duration::from_secs(15), waiter)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err();
            assert_eq!(error.code, "command_failed");
            assert_eq!(error.outcome, ivy_contracts::Outcome::Unknown);
        } else {
            let ready = tokio::time::Instant::now() + Duration::from_secs(8);
            while !root.join("inspector-child.pid").exists() {
                assert!(tokio::time::Instant::now() < ready);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let parent = held_process(
                fs::read_to_string(root.join("inspector-parent.pid"))
                    .unwrap()
                    .parse()
                    .unwrap(),
            );
            let child = held_process(
                fs::read_to_string(root.join("inspector-child.pid"))
                    .unwrap()
                    .parse()
                    .unwrap(),
            );
            let input_length = fs::read_to_string(root.join("inspector-input-length"))
                .unwrap()
                .parse::<usize>()
                .unwrap();
            if case == "blocked-input" {
                assert_eq!(input_length, 0);
            } else {
                assert!(input_length > 65_536);
            }
            if case == "dropped-waiter" {
                waiter.abort();
                assert!(waiter.await.unwrap_err().is_cancelled());
                assert!(!signaled(&parent));
                assert!(!signaled(&child));
            } else {
                let error = tokio::time::timeout(Duration::from_secs(15), &mut waiter)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap_err();
                assert_eq!(error.code, "deadline_exceeded");
                assert_eq!(error.outcome, ivy_contracts::Outcome::Unknown);
            }
            let end = tokio::time::Instant::now() + Duration::from_secs(15);
            while !signaled(&parent) || !signaled(&child) {
                assert!(tokio::time::Instant::now() < end);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        assert!(f.journal.target("fixture").await.unwrap().is_none());
        f.close().await;
    }
}
