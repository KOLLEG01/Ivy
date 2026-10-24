use super::*;

#[tokio::test]
async fn complete_backup_publishes_actual_current_state_and_releases_ownership() {
    let mut names = vec![std::ffi::OsString::from("master Ω.json")];
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        for unit in [0xd800, 0xdfff] {
            names.push(std::ffi::OsString::from_wide(&[
                109, unit, 46, 106, 115, 111, 110,
            ]));
        }
    }
    for name in names {
        let f = ServiceFixture::new();
        let (c, b) = setup(&f).await;
        let config_dir = f.0.join("configuration Ω");
        fs::create_dir_all(config_dir.join("route")).unwrap();
        let source = config_dir.join(&name);
        fs::copy(f.0.join("host.json"), &source).unwrap();
        let replacement = config_dir.join(name.to_string_lossy().as_ref());
        if replacement != source {
            fs::write(&replacement, b"distinct replacement alias").unwrap();
        }
        let config_path = config_dir.join("route").join("..").join(&name);
        let destination = f.0.join("route").join("..").join("backup Ω");
        fs::create_dir(f.0.join("route")).unwrap();
        let result =
            private_backup::backup_host(config_path.clone(), destination.clone(), b.clone())
                .await
                .unwrap();
        validate_host("BackupResult", &result).unwrap();
        assert_eq!(result.get("directory"), Some(&jp(&destination)));
        let manifest_path = destination.join("backup.json");
        let manifest = config::json_file(manifest_path.clone(), 12 * 1024 * 1024)
            .await
            .unwrap();
        validate_host("BackupManifest", &manifest).unwrap();
        assert_eq!(manifest.get("backupId"), result.get("backupId"));
        assert_eq!(
            result.get("manifestHash").unwrap().as_str().unwrap(),
            streaming_hash::file_hash(manifest_path).await.unwrap()
        );
        assert_eq!(
            manifest.get("configurationHash").unwrap().as_str().unwrap(),
            hash_json(&c).unwrap()
        );
        let copied_configuration = config::json_file(
            destination.join("configuration.json"),
            config::CONFIGURATION_BYTES,
        )
        .await
        .unwrap();
        assert_json_eq(&copied_configuration, &c);
        let files = manifest.get("files").unwrap().as_array().unwrap();
        assert!(
            files
                .iter()
                .any(|v| v.get("path") == Some(&"state/instances/fixture/data/owned.txt".into()))
        );
        assert!(!files.iter().any(|v| {
            v.get("path")
                .unwrap()
                .as_str()
                .unwrap()
                .contains("executor-lock.sqlite")
        }));
        let mut bytes = 0;
        for file in files {
            let path = backup_files::relative_file(
                &node_path::from_native(&destination),
                file.get("path").unwrap().as_js_string().unwrap(),
            )
            .unwrap();
            let path = node_path::filesystem_path(&path);
            assert_eq!(
                streaming_hash::file_hash(path.clone()).await.unwrap(),
                file.get("hash").unwrap().as_str().unwrap()
            );
            let length = fs::metadata(path).unwrap().len();
            assert_eq!(file.get("bytes"), Some(&J::Number(length as f64)));
            bytes += length;
        }
        assert_eq!(result.get("files"), Some(&J::Number(files.len() as f64)));
        assert_eq!(result.get("bytes"), Some(&J::Number(bytes as f64)));
        assert_eq!(
            fs::read(destination.join("state/instances/fixture/data/owned.txt")).unwrap(),
            b"current owned state"
        );
        let detached = f.0.join("verified-snapshot.sqlite");
        fs::copy(destination.join("state/deployments.sqlite"), &detached).unwrap();
        let snapshot = Connection::open_with_flags(
            detached,
            ivy_store::rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert_eq!(
            snapshot
                .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        drop(snapshot);
        let lock = ExecutorLock::acquire(f.0.join("state/executor"), false)
            .await
            .unwrap();
        lock.close().await.unwrap();
        if replacement != source {
            assert_eq!(
                fs::read(&replacement).unwrap(),
                b"distinct replacement alias"
            );
        }
        for (destination, code) in [
            (PathBuf::from("relative-backup"), "invalid_arguments"),
            (f.0.join("state/overlap"), "target_conflict"),
            (f.0.join("artifacts/overlap"), "target_conflict"),
            (f.0.join("staging/overlap"), "target_conflict"),
            (f.0.clone(), "target_conflict"),
            (destination.clone(), "target_conflict"),
            (f.0.join("missing-parent/backup"), "internal_error"),
        ] {
            let prior = destination.exists();
            let error =
                private_backup::backup_host(config_path.clone(), destination.clone(), b.clone())
                    .await
                    .unwrap_err();
            assert_eq!(error.code, code, "{destination:?}");
            assert_eq!(destination.exists(), prior, "{destination:?}");
            let lock = ExecutorLock::acquire(f.0.join("state/executor"), false)
                .await
                .unwrap();
            lock.close().await.unwrap();
        }
        let linked = f.0.join("linked-state");
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(f.0.join("state"), &linked).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(f.0.join("state"), &linked).unwrap();
        let error = private_backup::backup_host(config_path, linked.join("overlap"), b)
            .await
            .unwrap_err();
        assert_eq!(error.code, "target_conflict");
        assert!(!f.0.join("state/overlap").exists());
        #[cfg(windows)]
        fs::remove_dir(linked).unwrap();
        #[cfg(unix)]
        fs::remove_file(linked).unwrap();
    }
    let f = ServiceFixture::new();
    let (mut c, b) = setup(&f).await;
    c.insert(
        "instances",
        J::Array(vec![native_agent(&f, "retained", false)]),
    );
    config::atomic_json(f.0.join("host.json"), c).await.unwrap();
    host_config::host_config(f.0.join("host.json"))
        .await
        .unwrap();
    let retained = f.0.join("retained-work Ω");
    fs::create_dir(&retained).unwrap();
    config::atomic_json(
        f.0.join("state/instances/retained/data/project-backup-paths.json"),
        J::Array(vec![jp(&retained)]),
    )
    .await
    .unwrap();
    for destination in [retained.clone(), retained.join("overlap")] {
        let error = private_backup::backup_host(f.0.join("host.json"), destination, b.clone())
            .await
            .unwrap_err();
        assert_eq!(error.code, "target_conflict");
        assert!(!retained.join("overlap").exists());
        let lock = ExecutorLock::acquire(f.0.join("state/executor"), false)
            .await
            .unwrap();
        lock.close().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admitted_backup_keeps_locks_after_waiter_drop_and_rechecks_master_configuration() {
    for change in [false, true] {
        let f = ServiceFixture::new();
        let (config, b) = setup(&f).await;
        let gate = Arc::new(PolicyGate {
            entered: Default::default(),
            released: Mutex::new(false),
            wake: Default::default(),
            calls: Default::default(),
        });
        let _release = ReleaseGate(gate.clone());
        let bindings = Arc::new(BackupBindings {
            compare: b.compare.clone(),
            native_paths: gate.clone(),
            node: b.node.clone(),
            hive_snapshot: b.hive_snapshot,
            distribution_root: b.distribution_root.clone(),
            clock: b.clock.clone(),
        });
        let source = f.0.join("host.json");
        let destination = f.0.join("admitted-backup");
        let source2 = source.clone();
        let destination2 = destination.clone();
        let waiter = tokio::spawn(async move {
            private_backup::backup_host(source2, destination2, bindings).await
        });
        tokio::time::timeout(Duration::from_secs(10), gate.entered.notified())
            .await
            .unwrap();
        waiter.abort();
        let _ = waiter.await;
        assert_eq!(
            ExecutorLock::acquire(f.0.join("state/executor"), false)
                .await
                .err()
                .unwrap()
                .code,
            "executor_already_running"
        );
        if change {
            let mut edited = config.clone();
            edited.insert("publicBaseUrl", "http://127.0.0.1:39082/ivy".into());
            config::atomic_json(&source, edited).await.unwrap();
        }
        gate.release();
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if destination
                    .join(if change {
                        "backup-failure.json"
                    } else {
                        "backup.json"
                    })
                    .exists()
                {
                    if let Ok(lock) = ExecutorLock::acquire(f.0.join("state/executor"), false).await
                    {
                        lock.close().await.unwrap();
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            *gate.calls.lock().unwrap(),
            vec!["exclusions", "shared_homes"]
        );
        if change {
            assert!(!destination.join("backup.json").exists());
            let failure = config::json_file(destination.join("backup-failure.json"), 1024 * 1024)
                .await
                .unwrap();
            assert_eq!(failure.get("code"), Some(&"backup_changed".into()));
            retain(
                "changed master prevents publication after waiter drop",
                jp(&destination),
                failure,
                J::Null,
            );
        } else {
            let manifest = config::json_file(destination.join("backup.json"), 12 * 1024 * 1024)
                .await
                .unwrap();
            validate_host("BackupManifest", &manifest).unwrap();
            retain(
                "admitted publication survives waiter drop",
                jp(&destination),
                manifest,
                J::Null,
            );
        }
    }
}
