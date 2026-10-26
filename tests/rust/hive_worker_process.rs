use ivy_contracts::{IvyError, IvyJson, JsString, Outcome, json::stringify};
use ivy_hive::{
    worker_client::{Lifecycle, MAX_PENDING, RequestOptions},
    worker_protocol::{FrameReader, MAX_FRAME_BYTES, Message, Request, write_frame},
    worker_runtime::{Event, StorageClient, WorkerLaunch},
};
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    runtime::Builder,
    time::{sleep, timeout},
};
#[path = "hive_worker_online_backup.rs"]
mod online_backup;
#[path = "../fixtures/hive-startup-profile.rs"]
mod startup_profile;

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ivy-hive-pipe-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        assert!(self.0.starts_with(std::env::temp_dir()));
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn launch(root: &Root) -> WorkerLaunch {
    let executable = std::env::var_os("IVY_HIVE_TEST_WORKER")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("examples")
                .join(format!(
                    "ivy-hive-worker-fixture{}",
                    std::env::consts::EXE_SUFFIX
                ))
        });
    WorkerLaunch {
        root: root.0.clone(),
        executable,
        args: vec![],
        options: IvyJson::object([("root", root.0.to_str().unwrap().into())]),
        job_launcher: if cfg!(windows) {
            Some(PathBuf::from(
                std::env::var_os("IVY_HIVE_TEST_JOB").unwrap_or_else(|| {
                    let target = std::env::var_os("CARGO_TARGET_DIR")
                        .map(PathBuf::from)
                        .unwrap_or_else(|| {
                            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../target")
                        });
                    target.join("debug/ivy-job.exe").into_os_string()
                }),
            ))
        } else {
            None
        },
        environment: BTreeMap::new(),
    }
}
fn client(root: &Root) -> (StorageClient, Arc<Mutex<Vec<Event>>>) {
    let events = Arc::new(Mutex::new(vec![]));
    let sink = events.clone();
    (
        StorageClient::new(
            launch(root),
            Arc::new(move |event| sink.lock().unwrap().push(event)),
        ),
        events,
    )
}
fn payload(action: &str) -> IvyJson {
    IvyJson::object([("action", action.into())])
}
async fn wait_file(path: PathBuf) {
    timeout(Duration::from_secs(10), async {
        while !path.is_file() {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
async fn stopped(client: &StorageClient) {
    timeout(Duration::from_secs(5), async {
        while client.lifecycle() != Lifecycle::Stopped {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("Database owner not confirmed reaped");
}
async fn request(client: &StorageClient, value: IvyJson) -> Result<IvyJson, IvyError> {
    timeout(
        Duration::from_secs(10),
        client.request(value, RequestOptions::default()),
    )
    .await
    .unwrap()
    .unwrap()
}

fn original_profile(root: &Root) -> PathBuf {
    let path = root.0.join("profile.json");
    let bytes =
        serde_json::json!({"original":{"registryRuntime":startup_profile::registry(false)}})
            .to_string();
    fs::write(&path, &bytes).unwrap();
    path
}
fn kernel_launch(root: &Root, profile: &std::path::Path) -> WorkerLaunch {
    let mut value = launch(root);
    value.args = vec!["--kernel".into()];
    value.environment.insert(
        "IVY_HIVE_PROCESS_PROFILE".into(),
        profile.as_os_str().into(),
    );
    value
        .environment
        .insert("IVY_HIVE_PROCESS_ROOT".into(), root.0.as_os_str().into());
    value.options = IvyJson::object([
        (
            "filename",
            IvyJson::String(ivy_host_runtime::node_path::from_native(
                &root.0.join("hive.sqlite"),
            )),
        ),
        (
            "uiRoot",
            IvyJson::String(ivy_host_runtime::node_path::from_native(&root.0.join("ui"))),
        ),
        ("publicBaseUrl", "https://example.invalid/base".into()),
        ("version", "1.0.0".into()),
        ("buildId", format!("sha256:{}", "a".repeat(64)).into()),
        (
            "credentials",
            IvyJson::Array(vec![IvyJson::object([
                ("principalId", "alpha".into()),
                ("digest", format!("sha256:{}", "a".repeat(64)).into()),
            ])]),
        ),
    ]);
    value
}
fn database_locked(root: &Root) -> bool {
    let db = rusqlite::Connection::open_with_flags(
        root.0.join("hive.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    db.busy_timeout(Duration::ZERO).unwrap();
    match db.query_row("SELECT COUNT(*) FROM sqlite_schema", [], |r| {
        r.get::<_, i64>(0)
    }) {
        Ok(_) => false,
        Err(rusqlite::Error::SqliteFailure(error, _))
            if matches!(
                error.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ) =>
        {
            true
        }
        Err(error) => panic!("Unexpected owner probe: {error}"),
    }
}
fn save_kernel_process_cases(cases: serde_json::Value) {
    if let Some(path) = std::env::var_os("IVY_HIVE_KERNEL_PROCESS_CORPUS") {
        fs::write(path, cases.to_string()).unwrap();
    }
}

#[test]
#[cfg(windows)]
fn nonlast_client_drop_preserves_held_kernel_and_actual_reply() {
    use ivy_host_runtime::platform::windows_process::process_identity;
    // Keep an uncertain owner root/gate intact; restore normal removal only
    // after the retained client has explicitly confirmed retirement.
    let root = std::mem::ManuallyDrop::new(Root::new());
    fs::write(root.0.join("client-clone-case"), b"nonlast").unwrap();
    let profile = original_profile(&root);
    let store = ivy_hive::store::Store::open(&root.0.join("hive.sqlite")).unwrap();
    store.connection().execute_batch("CREATE TABLE fixture_payload(value TEXT); INSERT INTO fixture_payload VALUES('retained');").unwrap();
    store.close().unwrap();
    let events = Arc::new(Mutex::new(vec![]));
    let sink = events.clone();
    let mut case = Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let client = StorageClient::new(kernel_launch(&root, &profile), Arc::new(move |event| sink.lock().unwrap().push(event)));
        client.start().wait().await.unwrap();
        assert_eq!(request(&client, payload("health")).await.unwrap().get("ready"), Some(&IvyJson::Bool(true)));
        let Lifecycle::Ready(generation) = client.lifecycle() else { panic!("Actual owner not Ready"); };
        let pid = fs::read_to_string(root.0.join("kernel-pid")).unwrap().parse::<u32>().unwrap();
        let identity = process_identity(pid).unwrap().expect("Actual owner vanished");
        let retained = client.clone();
        let beta_digest = format!("sha256:{}", "b".repeat(64));
        fs::write(root.0.join("block-clock"), b"hold actual credential configuration").unwrap();
        let mut operation = client.request(IvyJson::object([
            ("action", "credentials.configure".into()),
            ("credentials", IvyJson::Array(vec![
                IvyJson::object([("principalId", "alpha".into()), ("digest", format!("sha256:{}", "a".repeat(64)).into())]),
                IvyJson::object([("principalId", "beta".into()), ("digest", beta_digest.clone().into())]),
            ])),
        ]), RequestOptions { timeout_ms:60_000, ..Default::default() });
        wait_file(root.0.join("clock-blocked")).await;
        assert!(database_locked(&root));
        assert_eq!(client.pending_count(), 1);
        assert_eq!(client.queued_bytes(), 4096);
        let dropped_at = tokio::time::Instant::now();
        drop(client);
        // Yield the actual client/Host pumps while the child cannot process its
        // blocked callback. A surviving clone must keep both owner and caller.
        sleep(Duration::from_millis(300)).await;
        let held_after_drop_ms = dropped_at.elapsed().as_millis();
        assert!(held_after_drop_ms >= 300);
        assert_eq!(retained.lifecycle(), Lifecycle::Ready(generation));
        assert_eq!(retained.pending_count(), 1);
        assert_eq!(retained.queued_bytes(), 4096);
        assert!(matches!(operation.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
        assert_eq!(process_identity(pid).unwrap().unwrap().created_filetime, identity.created_filetime);
        assert_eq!(fs::read_to_string(root.0.join("kernel-pid")).unwrap().parse::<u32>().unwrap(), pid);
        assert!(database_locked(&root));
        assert!(root.0.join("block-clock").is_file());
        fs::remove_file(root.0.join("block-clock")).unwrap();
        let reply = timeout(Duration::from_secs(5), operation).await.unwrap().unwrap().unwrap();
        assert_eq!(reply, IvyJson::object([("configured", true.into())]));
        assert_eq!(retained.lifecycle(), Lifecycle::Ready(generation));
        assert_eq!(retained.pending_count(), 0);
        assert_eq!(retained.queued_bytes(), 0);
        assert_eq!(process_identity(pid).unwrap().unwrap().created_filetime, identity.created_filetime);
        assert_eq!(request(&retained, payload("health")).await.unwrap().get("ready"), Some(&IvyJson::Bool(true)));
        let authenticated = request(&retained, IvyJson::object([
            ("action", "authenticate".into()),
            ("context", IvyJson::object([("credentialDigest", beta_digest.clone().into())])),
        ])).await.unwrap();
        assert_eq!(authenticated.get("principalId"), Some(&IvyJson::from("beta")));
        assert_eq!(authenticated.get("credentialDigest"), Some(&IvyJson::from(beta_digest)));
        assert!(events.lock().unwrap().iter().all(|event| !matches!(event, Event::Failure(_))));
        retained.close().wait().await.unwrap();
        stopped(&retained).await;
        assert_eq!(retained.pending_count(), 0);
        assert_eq!(retained.queued_bytes(), 0);
        assert!(!database_locked(&root));
        let db = rusqlite::Connection::open_with_flags(root.0.join("hive.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
        assert_eq!(db.query_row("SELECT value FROM fixture_payload", [], |row| row.get::<_, String>(0)).unwrap(), "retained");
        for principal in ["alpha", "beta"] {
            assert_eq!(db.query_row("SELECT revoked FROM credentials WHERE principal_id=?", [principal], |row| row.get::<_, i64>(0)).unwrap(), 0);
        }
        serde_json::json!({
            "case":"nonlast-client-drop", "generation":format!("{generation:?}"),
            "workerPid":pid,"workerCreatedFiletime":identity.created_filetime.to_string(),"heldAfterDropMs":held_after_drop_ms,
            "clientCloneRetained":true,"callerPendingAfterDrop":true,"sameProcessDuringHold":true,"sameProcessAfterReply":true,
            "sameGenerationAfterReply":true,"pendingWhileHeld":1,"queuedBytesWhileHeld":4096,"clockHeldDuringObservation":true,"sourceLockedDuringHold":true,
            "reply":serde_json::from_str::<serde_json::Value>(&stringify(&reply, 1<<20).unwrap()).unwrap(),
            "pendingAfterReply":0,"queuedBytesAfterReply":0,"betaAuthenticated":true,"healthAfterReply":true,"failureBeforeClose":false,
            "explicitRetainedClientCloseConfirmed":true,"sourceLockedAfterClose":false,"sourceIntegrity":"ok","sourcePayload":"retained","alphaRevoked":0,"betaRevoked":0,
        })
    });
    let canonical_root = fs::canonicalize(&root.0).unwrap();
    let canonical_parent = fs::canonicalize(std::env::temp_dir()).unwrap();
    assert_eq!(canonical_root.parent(), Some(canonical_parent.as_path()));
    assert_eq!(
        fs::read(root.0.join("client-clone-case")).unwrap(),
        b"nonlast"
    );
    let removed_path = root.0.clone();
    drop(std::mem::ManuallyDrop::into_inner(root));
    assert!(!removed_path.exists());
    case.as_object_mut().unwrap().extend(serde_json::json!({
        "normalRootRemovedAfterConfirmation":true,"uncertainRootDropRemovalEnabled":false,
        "productionEntryExecuted":true,"qualifiedProductionProviders":false,"originalReplay":false,
        "startupCloneDropQualified":false,"allJobDescendantsIndependentlyProved":false,"automaticReplay":false,
    }).as_object().unwrap().clone());
    save_kernel_process_cases(case);
}

#[test]
#[cfg(windows)]
fn last_client_drop_retires_initialized_owner_without_releasing_clock_gate() {
    use ivy_host_runtime::platform::windows_process::process_identity;
    let profile_root = Root::new();
    let profile = original_profile(&profile_root);
    let mut cases = vec![];
    Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        for ready in [false, true] {
            // An uncertain owner must retain its exact root and clock gate,
            // including on assertion panic. Restore normal Drop only after
            // definite original-process absence and fresh-client retirement.
            let root = std::mem::ManuallyDrop::new(Root::new());
            fs::write(root.0.join("client-drop-case"), if ready { b"ready".as_slice() } else { b"starting".as_slice() }).unwrap();
            let store = ivy_hive::store::Store::open(&root.0.join("hive.sqlite")).unwrap();
            store.connection().execute_batch("CREATE TABLE fixture_payload(value TEXT); INSERT INTO fixture_payload VALUES('retained');").unwrap();
            store.close().unwrap();
            if !ready { fs::write(root.0.join("block-clock"), b"hold constructor").unwrap(); }
            let events = Arc::new(Mutex::new(vec![]));
            let sink = events.clone();
            let client = StorageClient::new(kernel_launch(&root, &profile), Arc::new(move |event| sink.lock().unwrap().push(event)));
            let starting = client.start();
            let mut blocked = None;
            if ready {
                starting.wait().await.unwrap();
                assert_eq!(request(&client, payload("health")).await.unwrap().get("ready"), Some(&IvyJson::Bool(true)));
                assert!(!root.0.join("clock-blocked").exists());
                fs::write(root.0.join("block-clock"), b"hold actual admitted execute").unwrap();
                blocked = Some(client.request(IvyJson::object([
                    ("action", "execute".into()),
                    ("context", IvyJson::object([("principalId", "alpha".into()), ("credentialDigest", format!("sha256:{}", "a".repeat(64)).into()), ("transport", "http".into())])),
                    ("request", IvyJson::object([("jsonrpc", "2.0".into()), ("id", "drop-blocked".into()), ("method", "system.status".into()), ("params", IvyJson::object(std::iter::empty::<(&str, IvyJson)>()))])),
                ]), RequestOptions { timeout_ms:60_000, ..Default::default() }));
            }
            wait_file(root.0.join("clock-blocked")).await;
            let generation = match client.lifecycle() {
                Lifecycle::Ready(generation) if ready => generation,
                Lifecycle::Starting(generation) if !ready => generation,
                state => panic!("Unexpected owner lifecycle: {state:?}"),
            };
            let pending_before_drop = client.pending_count();
            let bytes_before_drop = client.queued_bytes();
            assert_eq!(pending_before_drop, usize::from(ready));
            assert_eq!(bytes_before_drop, if ready { 4096 } else { 0 });
            assert!(database_locked(&root));
            let pid = fs::read_to_string(root.0.join("kernel-pid")).unwrap().parse::<u32>().unwrap();
            let identity = process_identity(pid).unwrap().expect("Actual owner vanished before client Drop");
            let dropped_at = tokio::time::Instant::now();
            drop(client);
            let mut identity_errors = vec![];
            let retired = timeout(Duration::from_secs(5), async {
                loop {
                    match process_identity(pid) {
                        Ok(None) => break "absent",
                        Ok(Some(current)) if current.created_filetime != identity.created_filetime => break "reused",
                        Ok(Some(_)) => {},
                        Err(error) => identity_errors.push(error.raw_os_error()),
                    }
                    sleep(Duration::from_millis(5)).await;
                }
            }).await;
            if retired.is_err() {
                save_kernel_process_cases(serde_json::json!({"case":if ready { "ready" } else { "starting" },"execution":"blocked","identityFinding":null,"identityProbeErrors":identity_errors,"workerPid":pid,"workerCreatedFiletime":identity.created_filetime.to_string(),"retainedRoot":root.0.to_string_lossy(),"clockGateReleased":false,"databaseRecoveryNotRun":true}));
            }
            let absence = retired.expect("No definite original-process absence/reuse after last client Drop");
            let absent_after_drop_ms = dropped_at.elapsed().as_millis();
            assert!(absent_after_drop_ms <= 5_000);
            assert!(root.0.join("block-clock").is_file());
            assert!(root.0.join("clock-blocked").is_file());
            let startup_error = if ready {
                // Destruction drops the stored sender; it does not publish an
                // IvyError or a successful/NotExecuted operation result.
                assert!(timeout(Duration::from_secs(1), blocked.take().unwrap()).await.unwrap().is_err());
                None
            } else {
                let error = timeout(Duration::from_secs(1), starting.wait()).await.unwrap().unwrap_err();
                assert_eq!(error.code, "storage_unavailable");
                assert_eq!(error.message, "Hive storage connection failed.");
                assert_eq!(error.outcome, Outcome::Unknown);
                Some(serde_json::from_str::<serde_json::Value>(&stringify(&error.to_wire(), 1<<20).unwrap()).unwrap())
            };
            assert!(!database_locked(&root));
            assert!(events.lock().unwrap().iter().all(|event| !matches!(event, Event::Failure(_))));
            {
                let db = rusqlite::Connection::open_with_flags(root.0.join("hive.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
                assert_eq!(db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
                assert_eq!(db.query_row("SELECT value FROM fixture_payload", [], |row| row.get::<_, String>(0)).unwrap(), "retained");
                assert_eq!(db.query_row("SELECT revoked FROM credentials WHERE principal_id='alpha'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
            }
            fs::remove_file(root.0.join("block-clock")).unwrap();
            let fresh = StorageClient::new(kernel_launch(&root, &profile), Arc::new(|_| {}));
            fresh.start().wait().await.unwrap();
            let fresh_pid = fs::read_to_string(root.0.join("kernel-pid")).unwrap().parse::<u32>().unwrap();
            let fresh_identity = process_identity(fresh_pid).unwrap().expect("Fresh owner vanished");
            assert!((fresh_pid, fresh_identity.created_filetime) != (pid, identity.created_filetime));
            assert!(matches!(fresh.lifecycle(), Lifecycle::Ready(_)));
            assert!(database_locked(&root));
            assert_eq!(request(&fresh, payload("health")).await.unwrap().get("ready"), Some(&IvyJson::Bool(true)));
            fresh.close().wait().await.unwrap();
            stopped(&fresh).await;
            assert!(!database_locked(&root));
            {
                let db = rusqlite::Connection::open_with_flags(root.0.join("hive.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
                assert_eq!(db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
                assert_eq!(db.query_row("SELECT value FROM fixture_payload", [], |row| row.get::<_, String>(0)).unwrap(), "retained");
                assert_eq!(db.query_row("SELECT revoked FROM credentials WHERE principal_id='alpha'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
            }
            drop(fresh);
            let canonical_root = fs::canonicalize(&root.0).unwrap();
            let canonical_parent = fs::canonicalize(std::env::temp_dir()).unwrap();
            assert_eq!(canonical_root.parent(), Some(canonical_parent.as_path()));
            assert_eq!(fs::read(root.0.join("client-drop-case")).unwrap(), if ready { b"ready".as_slice() } else { b"starting".as_slice() });
            let removed_path = root.0.clone();
            drop(std::mem::ManuallyDrop::into_inner(root));
            assert!(!removed_path.exists());
            let mut case = serde_json::json!({
                "case":if ready { "ready" } else { "starting" },"oldGeneration":format!("{generation:?}"),
                "workerPid":pid,"workerCreatedFiletime":identity.created_filetime.to_string(),"identityFinal":absence,"identityProbeErrors":identity_errors,"absentAfterDropMs":absent_after_drop_ms,
                "sourceLockedBeforeDrop":true,"pendingBeforeDrop":pending_before_drop,"queuedBytesBeforeDrop":bytes_before_drop,
                "lastClientDropped":true,"callerObserversRetained":true,"publicCloseSent":false,"explicitStopSent":false,"clockHeldThroughAbsence":true,
                "startupError":startup_error,"pendingRequestSenderClosed":ready,"pendingRequestIvyError":null,"pendingAfterDrop":null,"queuedBytesAfterDrop":null,
            });
            case.as_object_mut().unwrap().extend(serde_json::json!({
                "failureEventObserved":false,"databaseLockedAfterAbsence":false,"sourceIntegrityAfterAbsence":"ok","sourcePayloadAfterAbsence":"retained","alphaRevokedAfterAbsence":0,
                "explicitFreshClientAfterAbsence":true,"sameClientRestart":false,"freshPid":fresh_pid,"freshCreatedFiletime":fresh_identity.created_filetime.to_string(),"freshIdentityDistinct":true,
                "freshHealth":true,"freshCloseConfirmed":true,"databaseLockedAfterFreshClose":false,"sourceIntegrityAfterFreshClose":"ok","sourcePayloadAfterFreshClose":"retained","alphaRevokedAfterFreshClose":0,
                "normalRootRemovedAfterConfirmation":true,"uncertainRootDropRemovalEnabled":false,"allJobDescendantsProved":false,
                "startupRollbackClaimed":false,"productionEntryExecuted":true,"qualifiedProductionProviders":false,"originalReplay":false
            }).as_object().unwrap().clone());
            cases.push(case);
        }
    });
    save_kernel_process_cases(serde_json::json!(cases));
}

#[test]
#[cfg(windows)]
fn actual_startup_deadline_retires_held_kernel_before_explicit_restart() {
    let profile_root = Root::new();
    let profile = original_profile(&profile_root);
    Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let root = Root::new();
        let store = ivy_hive::store::Store::open(&root.0.join("hive.sqlite")).unwrap();
        store.connection().execute_batch("CREATE TABLE fixture_payload(value TEXT); INSERT INTO fixture_payload VALUES('retained');").unwrap();
        store.close().unwrap();
        fs::write(root.0.join("block-clock"), b"hold initialized constructor through startup deadline").unwrap();
        struct Unblock(PathBuf);
        impl Drop for Unblock { fn drop(&mut self) { let _ = fs::remove_file(&self.0); } }
        let _unblock = Unblock(root.0.join("block-clock"));
        let events = Arc::new(Mutex::new(vec![]));
        let sink = events.clone();
        let client = StorageClient::new(kernel_launch(&root, &profile), Arc::new(move |event| sink.lock().unwrap().push(event)));
        let started_at = tokio::time::Instant::now();
        let startup = client.start();
        wait_file(root.0.join("clock-blocked")).await;
        let first_pid = fs::read_to_string(root.0.join("kernel-pid")).unwrap().parse::<u32>().unwrap();
        let Lifecycle::Starting(first_generation) = client.lifecycle() else { panic!("Constructor was not held before Ready"); };
        assert!(database_locked(&root));
        assert_eq!(client.pending_count(), 0);
        assert_eq!(client.queued_bytes(), 0);
        let error = timeout(Duration::from_secs(125), startup.wait()).await.unwrap().unwrap_err();
        let deadline_elapsed_ms = started_at.elapsed().as_millis();
        assert!((119_990..=125_000).contains(&deadline_elapsed_ms));
        assert_eq!(error.code, "deadline_exceeded");
        assert_eq!(error.message, "Hive storage startup exceeded its deadline.");
        // Preserve the original public deadline error. Its inherited outcome
        // does not establish rollback of already committed constructor effects.
        assert_eq!(error.outcome, Outcome::NotExecuted);
        assert_eq!(startup.wait().await.unwrap_err(), error);
        let retirement_at = tokio::time::Instant::now();
        stopped(&client).await;
        let retirement_elapsed_ms = retirement_at.elapsed().as_millis();
        assert!(retirement_elapsed_ms <= 5_000);
        assert!(root.0.join("block-clock").is_file());
        assert!(root.0.join("clock-blocked").is_file());
        assert!(!database_locked(&root));
        assert_eq!(client.pending_count(), 0);
        assert_eq!(client.queued_bytes(), 0);
        let failures = events.lock().unwrap().iter().filter_map(|event| match event { Event::Failure(error) => Some(error.clone()), _ => None }).collect::<Vec<_>>();
        assert_eq!(failures, vec![error.clone()]);
        client.close().wait().await.unwrap();
        {
            let db = rusqlite::Connection::open_with_flags(root.0.join("hive.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            assert_eq!(db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
            assert_eq!(db.query_row("SELECT value FROM fixture_payload", [], |row| row.get::<_, String>(0)).unwrap(), "retained");
            assert_eq!(db.query_row("SELECT revoked FROM credentials WHERE principal_id='alpha'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
        }
        fs::remove_file(root.0.join("block-clock")).unwrap();
        client.start().wait().await.unwrap();
        let Lifecycle::Ready(fresh_generation) = client.lifecycle() else { panic!("Explicit fresh owner not Ready"); };
        assert_ne!(fresh_generation, first_generation);
        let fresh_pid = fs::read_to_string(root.0.join("kernel-pid")).unwrap().parse::<u32>().unwrap();
        assert!(database_locked(&root));
        assert_eq!(request(&client, payload("health")).await.unwrap().get("ready"), Some(&IvyJson::Bool(true)));
        client.close().wait().await.unwrap();
        stopped(&client).await;
        assert!(!database_locked(&root));
        {
            let db = rusqlite::Connection::open_with_flags(root.0.join("hive.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            assert_eq!(db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
            assert_eq!(db.query_row("SELECT value FROM fixture_payload", [], |row| row.get::<_, String>(0)).unwrap(), "retained");
            assert_eq!(db.query_row("SELECT revoked FROM credentials WHERE principal_id='alpha'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
        }
        save_kernel_process_cases(serde_json::json!({
            "case":"actual-startup-deadline","firstPid":first_pid,"freshPid":fresh_pid,
            "firstGeneration":format!("{first_generation:?}"),"freshGeneration":format!("{fresh_generation:?}"),
            "startingAtClockGate":true,"sourceLockedAtClockGate":true,"pendingBeforeDeadline":0,"queuedBytesBeforeDeadline":0,
            "deadlineElapsedMs":deadline_elapsed_ms,"startupError":serde_json::from_str::<serde_json::Value>(&stringify(&error.to_wire(),1<<20).unwrap()).unwrap(),
            "cachedStartupErrorUnchanged":true,"initialFailureEvents":failures.len(),"failureEventMatchesStartupError":true,
            "retirementElapsedAfterErrorMs":retirement_elapsed_ms,"stoppedWithinFiveSeconds":true,"clockGateHeldUntilRetirement":true,
            "databaseLockedAfterRetirement":false,"pendingAfterRetirement":0,"queuedBytesAfterRetirement":0,"cachedCloseSucceeded":true,
            "sourceIntegrityAfterRetirement":"ok","sourcePayloadAfterRetirement":"retained","alphaRevokedAfterRetirement":0,
            "explicitRestartAfterGateRelease":true,"freshGenerationDistinct":true,"freshHealth":true,
            "confirmedStoppedAfterFreshClose":true,"databaseLockedAfterFreshClose":false,
            "sourceIntegrityAfterFreshClose":"ok","sourcePayloadAfterFreshClose":"retained","alphaRevokedAfterFreshClose":0,
            "publicCloseBeforeDeadline":false,"cooperativeKernelCloseBeforeRetirement":false,"startupRollbackClaimed":false,
            "productionEntryExecuted":true,"qualifiedProductionProviders":false,"originalReplay":false
        }));
    });
}

#[test]
#[cfg(windows)]
fn closing_blocked_actual_startup_owns_retirement_after_waiters_are_abandoned() {
    let profile_root = Root::new();
    let profile = original_profile(&profile_root);
    let mut cases = vec![];
    Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        for abandoned in [false, true] {
            let root = Root::new();
            let store = ivy_hive::store::Store::open(&root.0.join("hive.sqlite")).unwrap();
            store.connection().execute_batch("CREATE TABLE fixture_payload(value TEXT); INSERT INTO fixture_payload VALUES('retained');").unwrap();
            store.close().unwrap();
            fs::write(root.0.join("block-clock"), b"hold initialized constructor").unwrap();
            struct Unblock(PathBuf);
            impl Drop for Unblock { fn drop(&mut self) { let _ = fs::remove_file(&self.0); } }
            let _unblock = Unblock(root.0.join("block-clock"));
            let events = Arc::new(Mutex::new(vec![]));
            let sink = events.clone();
            let client = StorageClient::new(kernel_launch(&root, &profile), Arc::new(move |event| sink.lock().unwrap().push(event)));
            let mut starting = Some(client.start());
            wait_file(root.0.join("clock-blocked")).await;
            let first_pid = fs::read_to_string(root.0.join("kernel-pid")).unwrap().parse::<u32>().unwrap();
            let Lifecycle::Starting(first_generation) = client.lifecycle() else { panic!("Constructor was not held before Ready"); };
            assert!(database_locked(&root));
            assert_eq!(client.pending_count(), 0);
            assert_eq!(client.queued_bytes(), 0);
            let mut closing = Some(client.close());
            assert_eq!(client.lifecycle(), Lifecycle::Retiring(first_generation));
            let refused = client.start().wait().await.unwrap_err();
            assert_eq!(refused.code, "service_unavailable");
            assert_eq!(refused.message, "Hive storage is not ready.");
            let startup_error = if abandoned {
                // Neither waiter owns the eager launch/shutdown work. Keep the
                // client itself, but abandon both observation handles entirely.
                drop(starting.take());
                drop(closing.take());
                stopped(&client).await;
                None
            } else {
                let error = starting.as_ref().unwrap().wait().await.unwrap_err();
                assert_eq!(error.code, "service_unavailable");
                assert_eq!(error.message, "Hive is shutting down.");
                assert_eq!(error.outcome, Outcome::Unknown);
                timeout(Duration::from_secs(5), closing.as_ref().unwrap().wait()).await.unwrap().unwrap();
                Some(serde_json::from_str::<serde_json::Value>(&stringify(&error.to_wire(),1<<20).unwrap()).unwrap())
            };
            assert_eq!(client.lifecycle(), Lifecycle::Stopped);
            assert_eq!(client.pending_count(), 0);
            assert_eq!(client.queued_bytes(), 0);
            // This synchronous constructor cannot read EOF or cooperative
            // Close while its clock stays blocked. Actual Host retirement is
            // required to unlock the source, even with every waiter dropped.
            assert!(root.0.join("block-clock").is_file());
            assert!(root.0.join("clock-blocked").is_file());
            assert!(!database_locked(&root));
            client.close().wait().await.unwrap();
            assert_eq!(client.lifecycle(), Lifecycle::Stopped);
            assert!(events.lock().unwrap().iter().all(|event| !matches!(event, Event::Failure(_))));
            {
                let db = rusqlite::Connection::open_with_flags(root.0.join("hive.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
                assert_eq!(db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
                assert_eq!(db.query_row("SELECT value FROM fixture_payload", [], |row| row.get::<_, String>(0)).unwrap(), "retained");
                assert_eq!(db.query_row("SELECT revoked FROM credentials WHERE principal_id='alpha'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
            }
            fs::remove_file(root.0.join("block-clock")).unwrap();
            client.start().wait().await.unwrap();
            let Lifecycle::Ready(fresh_generation) = client.lifecycle() else { panic!("Explicit fresh owner not Ready"); };
            assert_ne!(fresh_generation, first_generation);
            let fresh_pid = fs::read_to_string(root.0.join("kernel-pid")).unwrap().parse::<u32>().unwrap();
            assert!(database_locked(&root));
            assert_eq!(request(&client, payload("health")).await.unwrap().get("ready"), Some(&IvyJson::Bool(true)));
            client.close().wait().await.unwrap();
            stopped(&client).await;
            assert!(!database_locked(&root));
            {
                let db = rusqlite::Connection::open_with_flags(root.0.join("hive.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
                assert_eq!(db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
                assert_eq!(db.query_row("SELECT value FROM fixture_payload", [], |row| row.get::<_, String>(0)).unwrap(), "retained");
                assert_eq!(db.query_row("SELECT revoked FROM credentials WHERE principal_id='alpha'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
            }
            cases.push(serde_json::json!({
                "case":if abandoned { "abandoned-waiters" } else { "retained-waiters" },
                "failedStartupPid":first_pid,"freshStartupPid":fresh_pid,
                "firstGeneration":format!("{first_generation:?}"),"freshGeneration":format!("{fresh_generation:?}"),
                "startingAtClockGate":true,"sourceLockedAtClockGate":true,"pendingBeforeClose":0,"queuedBytesBeforeClose":0,
                "retiringBeforeYield":true,"startRefusedDuringRetirement":true,"startupError":startup_error,
                "allStartupAndCloseWaitersDropped":abandoned,"clientRetained":true,"stoppedWithinFiveSeconds":true,
                "clockGateHeldUntilRetirement":true,"databaseLockedAfterRetirement":false,
                "pendingAfterRetirement":0,"queuedBytesAfterRetirement":0,"cachedCloseSucceeded":true,"failureEventBeforeRestart":false,
                "sourceIntegrityAfterRetirement":"ok","sourcePayloadAfterRetirement":"retained","alphaRevokedAfterRetirement":0,
                "explicitRestartAfterGateRelease":true,"freshGenerationDistinct":true,"freshHealth":true,
                "confirmedStoppedAfterFreshClose":true,"databaseLockedAfterFreshClose":false,
                "sourceIntegrityAfterFreshClose":"ok","sourcePayloadAfterFreshClose":"retained","alphaRevokedAfterFreshClose":0,
                "cooperativeKernelCloseBeforeRetirement":false,"startupRollbackClaimed":false,
                "productionEntryExecuted":true,"qualifiedProductionProviders":false,"originalReplay":false
            }));
        }
    });
    save_kernel_process_cases(serde_json::json!(cases));
}

#[test]
#[cfg(windows)]
fn pre_ready_output_loss_retires_actual_entry_owner_and_allows_fresh_start() {
    use ivy_host_runtime::process::{self, Command, LaunchContext};
    let profile_root = Root::new();
    let profile = original_profile(&profile_root);
    let mut cases = vec![];
    Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        for publication in ["ready", "late-fatal"] {
            let root = Root::new();
            let store = ivy_hive::store::Store::open(&root.0.join("hive.sqlite")).unwrap();
            store.connection().execute_batch("CREATE TABLE fixture_payload(value TEXT); INSERT INTO fixture_payload VALUES('retained');").unwrap();
            store.close().unwrap();
            fs::write(root.0.join("block-clock"), b"hold constructor after credentials commit").unwrap();
            struct Unblock(PathBuf);
            impl Drop for Unblock { fn drop(&mut self) { let _ = fs::remove_file(&self.0); } }
            let _unblock = Unblock(root.0.join("block-clock"));
            let mut launch = kernel_launch(&root, &profile);
            if publication == "late-fatal" { launch.options.insert("publicBaseUrl", "invalid".into()); }
            let mut child = process::spawn(
                &Command { executable:"worker".into(), args:launch.args, cwd:None, timeout_ms:30000 },
                &root.0, &BTreeMap::from([("worker".into(),launch.executable)]),
                LaunchContext { environment:launch.environment, job_launcher:launch.job_launcher, raw_output:true, ..Default::default() },
            ).await.unwrap();
            let mut stdin = child.stdin.take().unwrap();
            let stdout = child.stdout.take().unwrap();
            write_frame(&mut stdin, &launch.options).await.unwrap();
            wait_file(root.0.join("clock-blocked")).await;
            let first_pid = fs::read_to_string(root.0.join("kernel-pid")).unwrap().parse::<u32>().unwrap();
            assert!(database_locked(&root));
            assert!(root.0.join("block-clock").is_file());
            // Both constructors reached the real Registry clock after Store
            // credentials committed. Ready/Fatal has not been published yet.
            // Closing the real reader forces that forthcoming write to fail.
            drop(stdout);
            fs::remove_file(root.0.join("block-clock")).unwrap();
            let completion = match timeout(Duration::from_secs(5), child.completion.wait()).await {
                Ok(completion) => completion,
                Err(error) => {
                    let cleanup = child.stop(Duration::ZERO).await;
                    panic!("Pre-Ready {publication} output loss did not exit: {error}; cleanup {cleanup:?}");
                }
            };
            assert_eq!(completion.exit_code, Some(1));
            assert!(completion.error_code.is_none());
            // Keep stdin open through actual Host completion. No EOF, Close,
            // forced stop or parent death is allowed to supply normal cleanup.
            assert!(!database_locked(&root));
            drop(stdin);
            {
                let db = rusqlite::Connection::open_with_flags(root.0.join("hive.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
                assert_eq!(db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
                assert_eq!(db.query_row("SELECT value FROM fixture_payload", [], |row| row.get::<_, String>(0)).unwrap(), "retained");
                assert_eq!(db.query_row("SELECT revoked FROM credentials WHERE principal_id='alpha'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
            }
            // Confirm the original owner is gone before a separate real client
            // launch on the same database. Constructor effects are preserved;
            // this is not atomic startup rollback or replay of a posted action.
            let fresh = StorageClient::new(kernel_launch(&root, &profile), Arc::new(|_| {}));
            fresh.start().wait().await.unwrap();
            let fresh_pid = fs::read_to_string(root.0.join("kernel-pid")).unwrap().parse::<u32>().unwrap();
            assert!(database_locked(&root));
            assert_eq!(request(&fresh, payload("health")).await.unwrap().get("ready"), Some(&IvyJson::Bool(true)));
            fresh.close().wait().await.unwrap();
            stopped(&fresh).await;
            assert!(!database_locked(&root));
            {
                let db = rusqlite::Connection::open_with_flags(root.0.join("hive.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
                assert_eq!(db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
                assert_eq!(db.query_row("SELECT value FROM fixture_payload", [], |row| row.get::<_, String>(0)).unwrap(), "retained");
                assert_eq!(db.query_row("SELECT revoked FROM credentials WHERE principal_id='alpha'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
            }
            cases.push(serde_json::json!({
                "case":publication,"failedEntryPid":first_pid,"freshEntryPid":fresh_pid,
                "sourceLockedAtPreReadyClockGate":true,"readerClosedBeforeClockRelease":true,
                "stdinRetainedUntilCompletion":true,"closeRequestBeforeFailedExit":false,
                "exitCode":completion.exit_code,"errorCode":completion.error_code,"forcedStopUsed":false,
                "observedFrame":null,"publicationBranchEvidence":"source order and explicit startup options only; actual reader intentionally closed",
                "databaseLockedAfterFailedExit":false,"sourceIntegrityAfterFailedExit":"ok",
                "sourcePayloadAfterFailedExit":"retained","alphaRevokedAfterFailedExit":0,
                "freshLaunchAfterConfirmedCompletion":true,"freshHealth":true,"sourceLockedWhileFreshReady":true,
                "confirmedStoppedAfterFreshClose":true,"databaseLockedAfterFreshClose":false,
                "sourceIntegrityAfterFreshClose":"ok","sourcePayloadAfterFreshClose":"retained","alphaRevokedAfterFreshClose":0,
                "startupRollbackClaimed":false,"firstReadyOrFatalFrameObserved":false,
                "productionEntryExecuted":true,"qualifiedProductionProviders":false,"originalReplay":false
            }));
        }
    });
    save_kernel_process_cases(serde_json::json!(cases));
}

#[test]
#[cfg(windows)]
fn repeated_real_backup_progress_preserves_timeout_ownership_until_late_reply() {
    use ivy_host_runtime::platform::windows_process::process_identity;
    let profile_root = Root::new();
    let profile = original_profile(&profile_root);
    let mut cases = vec![];
    Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        for phase in ["copy", "hash"] {
            let root = Root::new();
            let source_path = root.0.join("hive.sqlite");
            let payload_rows = if phase == "hash" { 128 } else { 256 };
            let store = ivy_hive::store::Store::open(&source_path).unwrap();
            store.connection().execute_batch(&format!("CREATE TABLE fixture_payload(id INTEGER PRIMARY KEY, bytes BLOB); WITH RECURSIVE n(value) AS (VALUES(1) UNION ALL SELECT value+1 FROM n WHERE value<{payload_rows}) INSERT INTO fixture_payload SELECT value,zeroblob(4096) FROM n;")).unwrap();
            store.close().unwrap();
            if phase == "hash" {
                rusqlite::Connection::open(&source_path).unwrap().execute_batch("PRAGMA journal_mode=DELETE; PRAGMA page_size=65536; VACUUM;").unwrap();
            }
            fs::write(root.0.join("step-natural-progress"), b"hold real yielding callbacks").unwrap();
            let destination = root.0.join("first.sqlite");
            let mut launch = kernel_launch(&root, &profile);
            launch.args = vec!["--kernel-natural-progress".into()];
            let events = Arc::new(Mutex::new(vec![]));
            let sink = events.clone();
            let client = StorageClient::new(launch, Arc::new(move |event| sink.lock().unwrap().push(event)));
            client.start().wait().await.unwrap();
            let pid = fs::read_to_string(root.0.join("kernel-pid")).unwrap().parse::<u32>().unwrap();
            let identity = process_identity(pid).unwrap().expect("Ready owner vanished");
            let mut backup = client.request(IvyJson::object([
                ("action", "backup".into()), ("destination", destination.to_str().unwrap().into()),
            ]), RequestOptions { timeout_ms:3000, terminate_on_timeout:false, ..Default::default() });
            wait_file(root.0.join("backup-awaiting-copy")).await;
            fs::write(root.0.join("begin-copy"), b"first actual callback").unwrap();
            wait_file(root.0.join("natural-step-1")).await;
            assert_eq!(fs::read_to_string(root.0.join("first-natural-progress")).unwrap(), phase);
            let mut samples = vec![serde_json::from_slice::<serde_json::Value>(&fs::read(root.0.join("natural-step-1")).unwrap()).unwrap()];
            // The real server drains progress before handling this health
            // request. Its Reply bounds consumption of the first Progress.
            assert_eq!(request(&client, payload("health")).await.unwrap().get("ready"), Some(&IvyJson::Bool(true)));
            let baseline = tokio::time::Instant::now();
            sleep(Duration::from_millis(1600)).await;
            assert!(matches!(backup.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
            fs::write(root.0.join("advance-natural-1"), b"second actual callback").unwrap();
            wait_file(root.0.join("natural-step-2")).await;
            samples.push(serde_json::from_slice(&fs::read(root.0.join("natural-step-2")).unwrap()).unwrap());
            assert!(samples[1]["beforeMs"].as_u64().unwrap() >= samples[0]["afterMs"].as_u64().unwrap() + 1000);
            assert_eq!(request(&client, payload("health")).await.unwrap().get("ready"), Some(&IvyJson::Bool(true)));
            let second_consumed_ms = baseline.elapsed().as_millis();
            // The first Progress's deadline is no later than baseline+3000ms.
            // Surviving this checkpoint needs the second actual Progress.
            tokio::time::sleep_until(baseline + Duration::from_millis(3300)).await;
            let survived_ms = baseline.elapsed().as_millis();
            assert!(matches!(backup.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)), "Second real {phase} callback did not rearm the deadline");
            assert_eq!(client.pending_count(), 1);
            assert_eq!(client.queued_bytes(), 4096);
            assert!(matches!(client.lifecycle(), Lifecycle::Ready(_)));
            assert!(database_locked(&root));
            assert!(!root.0.join("advance-natural-2").exists());
            let error = timeout(Duration::from_secs(5), &mut backup).await.unwrap().unwrap().unwrap_err();
            let expired_ms = baseline.elapsed().as_millis();
            assert_eq!(error.code, "deadline_exceeded");
            assert_eq!(error.outcome, Outcome::Unknown);
            assert_eq!(error.message, "Hive storage response deadline exceeded; reconcile operation identity.");
            assert!(expired_ms > survived_ms);
            assert_eq!(client.pending_count(), 1);
            assert_eq!(client.queued_bytes(), 4096);
            assert_eq!(request(&client, payload("health")).await.unwrap().get("ready"), Some(&IvyJson::Bool(true)));
            assert_eq!(process_identity(pid).unwrap().unwrap().created_filetime, identity.created_filetime);
            assert!(database_locked(&root));
            // A later real callback may rearm the retained operation, but it
            // must not settle its already-timed-out caller a second time.
            fs::write(root.0.join("advance-natural-2"), b"progress after caller timeout").unwrap();
            wait_file(root.0.join("natural-step-3")).await;
            samples.push(serde_json::from_slice(&fs::read(root.0.join("natural-step-3")).unwrap()).unwrap());
            assert!(samples[2]["beforeMs"].as_u64().unwrap() >= samples[1]["afterMs"].as_u64().unwrap() + 1000);
            assert_eq!(request(&client, payload("health")).await.unwrap().get("ready"), Some(&IvyJson::Bool(true)));
            let late_progress_ms = baseline.elapsed().as_millis();
            sleep(Duration::from_millis(3300)).await;
            let retained_ms = baseline.elapsed().as_millis();
            assert!(matches!(backup.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Closed)));
            assert_eq!(client.pending_count(), 1);
            assert_eq!(client.queued_bytes(), 4096);
            assert!(matches!(client.lifecycle(), Lifecycle::Ready(_)));
            assert_eq!(process_identity(pid).unwrap().unwrap().created_filetime, identity.created_filetime);
            assert!(database_locked(&root));
            assert_eq!(request(&client, payload("health")).await.unwrap().get("ready"), Some(&IvyJson::Bool(true)));
            assert!(events.lock().unwrap().iter().all(|event| !matches!(event, Event::Failure(_))));
            assert!(!root.0.join("advance-natural-3").exists());
            fs::write(root.0.join("advance-natural-3"), b"complete retained operation").unwrap();
            timeout(Duration::from_secs(10), async {
                while client.pending_count() != 0 { sleep(Duration::from_millis(5)).await; }
            }).await.unwrap();
            let late_reply_ms = baseline.elapsed().as_millis();
            let callbacks = fs::read_to_string(root.0.join("natural-progress-callback-count")).unwrap().parse::<usize>().unwrap();
            assert!(callbacks > 3);
            assert_eq!(client.queued_bytes(), 0);
            assert!(matches!(client.lifecycle(), Lifecycle::Ready(_)));
            let first_hash = ivy_contracts::hash::digest(fs::read(&destination).unwrap());
            let first_pages = {
                let target = rusqlite::Connection::open_with_flags(&destination, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
                assert_eq!(target.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
                assert_eq!(target.query_row("SELECT COUNT(*) FROM fixture_payload WHERE length(bytes)=4096", [], |row| row.get::<_, i64>(0)).unwrap(), payload_rows);
                assert_eq!(target.query_row("SELECT revoked FROM credentials WHERE principal_id='alpha'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
                target.query_row("PRAGMA page_count", [], |row| row.get::<_, i64>(0)).unwrap()
            };
            if phase == "copy" { assert!(first_pages > 300); } else { assert!(first_pages <= 100); }
            // This is a separately named new request after the old reservation
            // is gone, not replay of the timed-out request or recovered result.
            let new_destination = root.0.join("separate.sqlite");
            let separate = request(&client, IvyJson::object([
                ("action", "backup".into()), ("destination", new_destination.to_str().unwrap().into()),
            ])).await.unwrap();
            let separate_hash = ivy_contracts::hash::digest(fs::read(&new_destination).unwrap());
            assert_eq!(separate.get("bytesHash"), Some(&separate_hash.clone().into()));
            assert_eq!(separate.get("pages"), Some(&IvyJson::Number(first_pages as f64)));
            assert_eq!(client.pending_count(), 0);
            assert_eq!(client.queued_bytes(), 0);
            assert_eq!(process_identity(pid).unwrap().unwrap().created_filetime, identity.created_filetime);
            assert!(events.lock().unwrap().iter().all(|event| !matches!(event, Event::Failure(_))));
            client.close().wait().await.unwrap();
            stopped(&client).await;
            assert!(!database_locked(&root));
            cases.push(serde_json::json!({
                "case":phase,"workerPid":pid,"workerCreatedFiletime":identity.created_filetime.to_string(),
                "callbackWallRanges":samples,"requestTimeoutMs":3000,"firstProgressDeadlineUpperBoundMs":3000,
                "secondProgressConsumedAfterBaselineMs":second_consumed_ms,"pendingObservedAfterBaselineMs":survived_ms,
                "callerTimedOutAfterBaselineMs":expired_ms,"lateProgressConsumedAfterBaselineMs":late_progress_ms,
                "retainedAfterLateProgressMs":retained_ms,"lateReplyConsumedAfterBaselineMs":late_reply_ms,
                "error":serde_json::from_str::<serde_json::Value>(&stringify(&error.to_wire(),1<<20).unwrap()).unwrap(),
                "pendingAfterCallerTimeout":1,"queuedBytesAfterCallerTimeout":4096,
                "pendingAfterLateProgressDeadline":1,"queuedBytesAfterLateProgressDeadline":4096,
                "sameOwnerUntilSeparateBackup":true,"sourceLockedUntilClose":true,"healthThroughout":true,
                "callerRemainsClosed":true,"failureEventBeforeClose":false,"pendingAfterLateReply":0,"queuedBytesAfterLateReply":0,
                "firstDestinationIntegrity":"ok","firstDestinationPayloadRows":payload_rows,"firstDestinationAlphaRevoked":0,
                "firstDestinationPages":first_pages,"firstDestinationFullFileHash":first_hash,"firstReturnedResultRecovered":false,
                "naturalCallbacksBeforeSeparateBackup":callbacks,"separateRequestCompleted":true,
                "separateDestinationPages":first_pages,"separateReturnedFullFileHash":separate_hash,
                "confirmedStoppedAfterClose":true,"databaseLockedAfterClose":false,
                "syntheticProgressCallbacks":0,"automaticReplay":false,"productionEntryExecuted":false,"originalReplay":false,
                "deadlineProof":"Actual client consumes progress through FIFO/biased health barriers; second callback sustains pending beyond first upper bound, later caller timeout retains ownership, third callback cannot resettle, late reply clears reservation without retirement"
            }));
        }
    });
    save_kernel_process_cases(serde_json::json!(cases));
}

#[test]
#[cfg(windows)]
fn real_copy_and_hash_callbacks_obey_progress_throttle_in_captured_frames() {
    use ivy_host_runtime::process::{self, Command, LaunchContext};
    async fn health_boundary(
        stdin: &mut tokio::process::ChildStdin,
        reader: &mut FrameReader<tokio::process::ChildStdout>,
        frames: &mut Vec<IvyJson>,
        id: u64,
    ) {
        write_frame(
            stdin,
            &Request {
                id,
                payload: payload("health"),
            }
            .to_json(),
        )
        .await
        .unwrap();
        timeout(Duration::from_secs(5), async {
            loop {
                let frame = reader.next().await.unwrap().unwrap();
                let message = Message::from_json(frame.clone()).unwrap().unwrap();
                assert!(
                    !matches!(message, Message::Reply { id: 1, .. }),
                    "Held backup replied early"
                );
                let done = match message {
                    Message::Reply {
                        id: current,
                        result: Ok(value),
                    } if current == id => {
                        assert_eq!(value.get("ready"), Some(&IvyJson::Bool(true)));
                        true
                    }
                    Message::Reply { id: current, .. } if current == id => panic!("Health failed"),
                    _ => false,
                };
                frames.push(frame);
                if done {
                    return;
                }
            }
        })
        .await
        .unwrap();
    }
    fn progress_count(frames: &[IvyJson]) -> usize {
        frames
            .iter()
            .filter(|frame| {
                matches!(
                    Message::from_json((*frame).clone()).unwrap(),
                    Some(Message::Progress(1))
                )
            })
            .count()
    }
    let profile_root = Root::new();
    let profile = original_profile(&profile_root);
    let mut cases = vec![];
    Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        for phase in ["copy", "hash"] {
            let root = Root::new();
            let source_path = root.0.join("hive.sqlite");
            let payload_rows = if phase == "hash" { 128 } else { 256 };
            let store = ivy_hive::store::Store::open(&source_path).unwrap();
            store.connection().execute_batch(&format!("CREATE TABLE fixture_payload(id INTEGER PRIMARY KEY, bytes BLOB); WITH RECURSIVE n(value) AS (VALUES(1) UNION ALL SELECT value+1 FROM n WHERE value<{payload_rows}) INSERT INTO fixture_payload SELECT value,zeroblob(4096) FROM n;")).unwrap();
            store.close().unwrap();
            if phase == "hash" {
                rusqlite::Connection::open(&source_path).unwrap().execute_batch("PRAGMA journal_mode=DELETE; PRAGMA page_size=65536; VACUUM;").unwrap();
            }
            fs::write(root.0.join("step-natural-progress"), b"measure actual callback windows").unwrap();
            let destination = root.0.join("copy.sqlite");
            let mut launch = kernel_launch(&root, &profile);
            launch.args = vec!["--kernel-natural-progress".into()];
            let mut child = process::spawn(&Command { executable:"worker".into(), args:launch.args, cwd:None, timeout_ms:30000 }, &root.0,
                &BTreeMap::from([("worker".into(),launch.executable)]),
                LaunchContext { environment:launch.environment, job_launcher:launch.job_launcher, raw_output:true, ..Default::default() },
            ).await.unwrap();
            let mut stdin = child.stdin.take().unwrap();
            let mut reader = FrameReader::new(child.stdout.take().unwrap());
            write_frame(&mut stdin, &launch.options).await.unwrap();
            let ready = timeout(Duration::from_secs(10), reader.next()).await.unwrap().unwrap().unwrap();
            assert_eq!(ready, Message::Ready.to_json());
            let mut frames = vec![ready];
            write_frame(&mut stdin, &Request { id:1, payload:IvyJson::object([
                ("action", "backup".into()), ("destination", destination.to_str().unwrap().into()),
            ]) }.to_json()).await.unwrap();
            wait_file(root.0.join("backup-awaiting-copy")).await;
            fs::write(root.0.join("begin-copy"), b"run actual callbacks").unwrap();
            let mut observations = vec![];
            let mut counts = vec![];
            for step in 1..=3 {
                wait_file(root.0.join(format!("natural-step-{step}"))).await;
                let sample: serde_json::Value = serde_json::from_slice(&fs::read(root.0.join(format!("natural-step-{step}"))).unwrap()).unwrap();
                assert!(sample["beforeMs"].as_u64().unwrap() <= sample["afterMs"].as_u64().unwrap());
                assert_eq!(fs::read_to_string(root.0.join("first-natural-progress")).unwrap(), phase);
                health_boundary(&mut stdin, &mut reader, &mut frames, step as u64 + 1).await;
                observations.push(sample);
                counts.push(progress_count(&frames));
                assert!(database_locked(&root));
                if step == 1 {
                    assert_eq!(counts[0], 1);
                    fs::write(root.0.join("advance-natural-1"), b"next actual callback").unwrap();
                } else if step == 2 {
                    // The third real callback is separated from the second
                    // health barrier; timestamps verify the actual wall window.
                    sleep(Duration::from_millis(1100)).await;
                    fs::write(root.0.join("advance-natural-2"), b"later actual callback").unwrap();
                }
            }
            let before = |i:usize| observations[i]["beforeMs"].as_u64().unwrap();
            let after = |i:usize| observations[i]["afterMs"].as_u64().unwrap();
            let fast_upper = after(1).saturating_sub(before(0));
            let fast_lower = before(1).saturating_sub(after(0));
            let suppression_certain = after(1) >= before(0) && fast_upper < 1000;
            let second_report_certain = before(1) >= after(0) && fast_lower >= 1000;
            if suppression_certain { assert_eq!(counts[1], 1); }
            else if second_report_certain { assert_eq!(counts[1], 2); }
            else { assert!(matches!(counts[1], 1 | 2)); }
            let last = if counts[1] == 2 { 1 } else { 0 };
            let late_lower = before(2).saturating_sub(after(last));
            let late_upper = after(2).saturating_sub(before(last));
            let later_report_certain = before(2) >= after(last) && late_lower >= 1000;
            let later_suppression_certain = after(2) >= before(last) && late_upper < 1000;
            if later_report_certain { assert_eq!(counts[2], counts[1] + 1); }
            else if later_suppression_certain { assert_eq!(counts[2], counts[1]); }
            else { assert!(counts[2] == counts[1] || counts[2] == counts[1] + 1); }
            fs::write(root.0.join("advance-natural-3"), b"finish actual backup").unwrap();
            let snapshot = timeout(Duration::from_secs(10), async {
                loop {
                    let frame = reader.next().await.unwrap().unwrap();
                    let message = Message::from_json(frame.clone()).unwrap().unwrap();
                    frames.push(frame);
                    if let Message::Reply { id:1, result } = message { break result.unwrap(); }
                }
            }).await.unwrap();
            let pages = snapshot.required("pages").unwrap().as_f64().unwrap() as i64;
            if phase == "copy" { assert!(pages > 300); } else { assert!(pages <= 100); }
            let bytes = fs::read(&destination).unwrap();
            assert!(bytes.len() > 3 * 64 * 1024);
            let expected_hash = ivy_contracts::hash::digest(bytes);
            assert_eq!(snapshot.get("bytesHash"), Some(&expected_hash.clone().into()));
            {
                let target = rusqlite::Connection::open_with_flags(&destination, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
                assert_eq!(target.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
                assert_eq!(target.query_row("SELECT COUNT(*) FROM fixture_payload WHERE length(bytes)=4096", [], |row| row.get::<_, i64>(0)).unwrap(), payload_rows);
                assert_eq!(target.query_row("SELECT revoked FROM credentials WHERE principal_id='alpha'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
            }
            write_frame(&mut stdin, &Request { id:5, payload:payload("close") }.to_json()).await.unwrap();
            timeout(Duration::from_secs(5), async { while let Some(frame) = reader.next().await.unwrap() { frames.push(frame); } }).await.unwrap();
            let completion = timeout(Duration::from_secs(5), child.completion.wait()).await.unwrap();
            assert_eq!(completion.exit_code, Some(1));
            assert!(completion.error_code.is_none());
            drop(stdin);
            assert!(!database_locked(&root));
            assert!(!frames.iter().any(|frame| matches!(Message::from_json(frame.clone()).unwrap(), Some(Message::Reply { id:5, .. } | Message::Fatal(_)))));
            cases.push(serde_json::json!({
                "case":phase,"callbackWallRanges":observations,"progressFramesAtHealthBarriers":counts,
                "throttleMs":1000,"fastWindowLowerMs":fast_lower,"fastWindowUpperMs":fast_upper,
                "lateWindowLowerMs":late_lower,"lateWindowUpperMs":late_upper,
                "suppressionWindowExercised":suppression_certain,"laterWindowExercised":later_report_certain,
                "ambiguousWindow":(!suppression_certain && !second_report_certain) || (!later_report_certain && !later_suppression_certain),
                "healthBarriers":3,"backupPages":pages,"fullFileBytesHash":expected_hash,
                "targetIntegrity":"ok","targetPayloadRows":payload_rows,"targetAlphaRevoked":0,
                "frames":serde_json::from_str::<serde_json::Value>(&stringify(&IvyJson::Array(frames), 1 << 20).unwrap()).unwrap(),
                "exitCode":completion.exit_code,"errorCode":completion.error_code,"databaseLockedAfterReap":false,
                "closeReplyObserved":false,"syntheticProgressCallbacks":0,"productionEntryExecuted":false,"originalReplay":false
            }));
        }
    });
    save_kernel_process_cases(serde_json::json!(cases));
}

#[test]
#[cfg(windows)]
fn actual_copy_and_hash_progress_rearm_client_deadline_while_health_remains_responsive() {
    use ivy_host_runtime::platform::windows_process::process_identity;
    use sha2::{Digest, Sha256};
    let profile_root = Root::new();
    let profile = original_profile(&profile_root);
    let mut cases = vec![];
    Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        for phase in ["copy", "hash"] {
            let root = Root::new();
            let source_path = root.0.join("hive.sqlite");
            let payload_rows = if phase == "hash" { 128 } else { 256 };
            let store = ivy_hive::store::Store::open(&source_path).unwrap();
            store.connection().execute_batch(&format!("CREATE TABLE fixture_payload(id INTEGER PRIMARY KEY, bytes BLOB); WITH RECURSIVE n(value) AS (VALUES(1) UNION ALL SELECT value+1 FROM n WHERE value<{payload_rows}) INSERT INTO fixture_payload SELECT value,zeroblob(4096) FROM n;")).unwrap();
            store.close().unwrap();
            if phase == "hash" {
                // A valid large-page SQLite variant fits in one copy batch;
                // its first natural progress callback must be from hashing.
                let source = rusqlite::Connection::open(&source_path).unwrap();
                source.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA page_size=65536; VACUUM;").unwrap();
                assert_eq!(source.query_row("PRAGMA page_size", [], |row| row.get::<_, i64>(0)).unwrap(), 65536);
            }
            let destination = root.0.join("copy.sqlite");
            let mut launch = kernel_launch(&root, &profile);
            launch.args = vec!["--kernel-natural-progress".into()];
            let client = StorageClient::new(launch, Arc::new(|_| {}));
            client.start().wait().await.unwrap();
            let pid = fs::read_to_string(root.0.join("kernel-pid")).unwrap().parse::<u32>().unwrap();
            let identity = process_identity(pid).unwrap().expect("Ready owner vanished");
            let mut backup = client.request(IvyJson::object([
                ("action", "backup".into()), ("destination", destination.to_str().unwrap().into()),
            ]), RequestOptions { timeout_ms:3000, terminate_on_timeout:false, ..Default::default() });
            wait_file(root.0.join("backup-awaiting-copy")).await;
            assert_eq!(request(&client, payload("health")).await.unwrap().get("ready"), Some(&IvyJson::Bool(true)));
            // The ordered health Reply proves the client already consumed this
            // backup's Started frame. Its unrearmed deadline is no later than
            // baseline+3000ms, even if frame delivery/scheduling was delayed.
            let baseline = tokio::time::Instant::now();
            assert!(!root.0.join("first-natural-progress").exists());
            sleep(Duration::from_millis(1600)).await;
            assert!(matches!(backup.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
            fs::write(root.0.join("begin-copy"), b"run actual task").unwrap();
            wait_file(root.0.join("natural-progress-paused")).await;
            let phase_observed_at = baseline.elapsed().as_millis();
            assert_eq!(fs::read_to_string(root.0.join("first-natural-progress")).unwrap(), phase);
            // The task now yields on a test gate. The real server's biased
            // progress-before-request order puts its Progress frame ahead of
            // this health Reply, which the actual StorageClient consumes.
            assert_eq!(request(&client, payload("health")).await.unwrap().get("ready"), Some(&IvyJson::Bool(true)));
            assert_eq!(client.pending_count(), 1);
            assert_eq!(client.queued_bytes(), 4096);
            assert!(database_locked(&root));
            assert_eq!(process_identity(pid).unwrap().unwrap().created_filetime, identity.created_filetime);
            {
                let target = rusqlite::Connection::open_with_flags(&destination, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
                target.busy_timeout(Duration::ZERO).unwrap();
                match target.query_row("SELECT COUNT(*) FROM sqlite_schema", [], |row| row.get::<_, i64>(0)) {
                    Ok(_) => assert_eq!(phase, "hash"),
                    Err(rusqlite::Error::SqliteFailure(error, _)) if matches!(error.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => assert_eq!(phase, "copy"),
                    Err(error) => panic!("Unexpected gated target probe: {error}"),
                }
            }
            tokio::time::sleep_until(baseline + Duration::from_millis(3300)).await;
            let survived_ms = baseline.elapsed().as_millis();
            assert!(survived_ms >= 3300);
            assert!(matches!(backup.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)), "Actual {phase} Progress did not rearm its client deadline");
            assert!(matches!(client.lifecycle(), Lifecycle::Ready(_)));
            assert_eq!(client.pending_count(), 1);
            assert_eq!(client.queued_bytes(), 4096);
            assert!(!root.0.join("continue-backup").exists());
            fs::write(root.0.join("continue-backup"), b"finish actual backup").unwrap();
            let result = timeout(Duration::from_secs(5), backup).await.unwrap().unwrap().unwrap();
            let completed_ms = baseline.elapsed().as_millis();
            let callbacks = fs::read_to_string(root.0.join("natural-progress-callback-count")).unwrap().parse::<usize>().unwrap();
            assert!(callbacks > 1);
            let pages = result.required("pages").unwrap().as_f64().unwrap() as i64;
            if phase == "hash" { assert!(pages <= 100); } else { assert!(pages > 100); }
            let expected_hash = format!("sha256:{}", Sha256::digest(fs::read(&destination).unwrap()).iter().map(|byte| format!("{byte:02x}")).collect::<String>());
            assert_eq!(result.required("bytesHash").unwrap().as_str().unwrap(), expected_hash);
            {
                let target = rusqlite::Connection::open_with_flags(&destination, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
                assert_eq!(target.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
                assert_eq!(target.query_row("SELECT COUNT(*) FROM fixture_payload WHERE length(bytes)=4096", [], |row| row.get::<_, i64>(0)).unwrap(), payload_rows);
                assert_eq!(target.query_row("SELECT revoked FROM credentials WHERE principal_id='alpha'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
                assert_eq!(target.query_row("PRAGMA page_count", [], |row| row.get::<_, i64>(0)).unwrap(), pages);
            }
            assert_eq!(client.pending_count(), 0);
            assert_eq!(client.queued_bytes(), 0);
            assert_eq!(request(&client, payload("health")).await.unwrap().get("ready"), Some(&IvyJson::Bool(true)));
            client.close().wait().await.unwrap();
            stopped(&client).await;
            assert!(!database_locked(&root));
            cases.push(serde_json::json!({
                "case":phase,"workerPid":pid,"workerCreatedFiletime":identity.created_filetime.to_string(),
                "requestTimeoutMs":3000,"initialWorkDelayMs":1600,"firstPhaseObservedAfterBaselineMs":phase_observed_at,
                "unrearmedDeadlineUpperBoundMs":3000,"pendingObservedAfterBaselineMs":survived_ms,"completedAfterBaselineMs":completed_ms,
                "firstNaturalCallbackPhase":phase,"firstCallbackCountAtGate":1,"totalNaturalCallbacks":callbacks,
                "healthBeforeWork":true,"healthAfterProgress":true,"healthAfterBackup":true,
                "pendingWhilePaused":1,"queuedBytesWhilePaused":4096,"sameOwnerWhilePaused":true,
                "sourceLockedWhilePaused":true,"targetLockedWhilePaused":phase == "copy",
                "backupCompleted":true,"pages":pages,"fullFileBytesHash":expected_hash,"targetIntegrity":"ok",
                "targetPayloadRows":payload_rows,"targetAlphaRevoked":0,"pendingAfterBackup":0,"queuedBytesAfterBackup":0,
                "confirmedStoppedAfterClose":true,"databaseLockedAfterClose":false,
                "syntheticProgressCallbacks":0,"productionEntryExecuted":false,"originalReplay":false,
                "deadlineProof":"Started precedes first health Reply; real first phase callback Progress precedes second health Reply; request stays pending beyond baseline+3000ms with task gate held"
            }));
        }
    });
    save_kernel_process_cases(serde_json::json!(cases));
}

#[test]
#[cfg(windows)]
fn initialized_entry_backup_frames_publish_progress_before_reply_and_preserve_close_order() {
    use ivy_host_runtime::process::{self, Command, LaunchContext};
    let root = Root::new();
    let profile = original_profile(&root);
    let store = ivy_hive::store::Store::open(&root.0.join("hive.sqlite")).unwrap();
    store.connection().execute_batch("CREATE TABLE fixture_payload(id INTEGER PRIMARY KEY, bytes BLOB); WITH RECURSIVE n(value) AS (VALUES(1) UNION ALL SELECT value+1 FROM n WHERE value<4096) INSERT INTO fixture_payload SELECT value,zeroblob(4096) FROM n;").unwrap();
    drop(store);
    let destination = root.0.join("copy.sqlite");
    let launch = kernel_launch(&root, &profile);
    Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let mut child = process::spawn(
            &Command { executable:"worker".into(), args:launch.args, cwd:None, timeout_ms:30000 },
            &root.0, &BTreeMap::from([("worker".into(),launch.executable)]),
            LaunchContext { environment:launch.environment, job_launcher:launch.job_launcher, raw_output:true, ..Default::default() },
        ).await.unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut reader = FrameReader::new(child.stdout.take().unwrap());
        write_frame(&mut stdin, &launch.options).await.unwrap();
        let ready = timeout(Duration::from_secs(10), reader.next()).await.unwrap().unwrap().unwrap();
        assert_eq!(ready, Message::Ready.to_json());
        assert!(database_locked(&root));
        let backup = IvyJson::object([
            ("action", "backup".into()),
            ("destination", destination.to_str().unwrap().into()),
        ]);
        // All three ordinary requests follow Ready. There is no artificial
        // pause in the actual KernelWorker copy or streaming hash.
        for (id, value) in [(1, backup), (2, payload("health")), (3, payload("close"))] {
            write_frame(&mut stdin, &Request { id, payload:value }.to_json()).await.unwrap();
        }
        let mut frames = vec![ready];
        let drained = timeout(Duration::from_secs(20), async {
            while let Some(frame) = reader.next().await.unwrap() { frames.push(frame); }
        }).await;
        if let Err(error) = drained {
            let cleanup = child.stop(Duration::ZERO).await;
            panic!("Production backup/close did not end: {error}; cleanup {cleanup:?}");
        }
        // Keep stdin open through actual exit: the entry must not wait for
        // Tokio's blocking stdin reader after the existing close failure.
        let completion = timeout(Duration::from_secs(5), child.completion.wait()).await.unwrap();
        assert_eq!(completion.exit_code, Some(1));
        assert!(completion.error_code.is_none());
        drop(stdin);
        assert!(!database_locked(&root));
        assert!(!root.0.join("backup-paused").exists());
        assert!(!root.0.join("block-clock").exists());
        let messages = frames.iter().cloned().map(|frame| Message::from_json(frame).unwrap().unwrap()).collect::<Vec<_>>();
        let started = |id| messages.iter().position(|m| matches!(m, Message::Started(current) if *current == id)).unwrap();
        let reply = |id| messages.iter().position(|m| matches!(m, Message::Reply { id: current, .. } if *current == id)).unwrap();
        let backup_reply = reply(1);
        let health_reply = reply(2);
        let close_started = started(3);
        assert!(started(1) < started(2) && started(2) < close_started);
        assert!(started(2) < health_reply && health_reply < close_started);
        let progress = messages.iter().enumerate().filter_map(|(index, m)| matches!(m, Message::Progress(1)).then_some(index)).collect::<Vec<_>>();
        assert!(!progress.is_empty());
        assert!(progress.iter().all(|index| started(1) < *index && *index < backup_reply));
        assert!(!messages.iter().any(|m| matches!(m, Message::Progress(id) if *id != 1)));
        for id in [1,2,3] {
            assert_eq!(messages.iter().filter(|m| matches!(m, Message::Started(current) if *current == id)).count(), 1);
            assert_eq!(messages.iter().filter(|m| matches!(m, Message::Reply { id: current, .. } if *current == id)).count(), usize::from(id != 3));
        }
        assert!(!messages.iter().any(|m| matches!(m, Message::Fatal(_))));
        let snapshot = match &messages[backup_reply] { Message::Reply { result:Ok(value), .. } => value, _ => panic!("Backup failed: {}", stringify(&frames[backup_reply], 1 << 20).unwrap()) };
        match &messages[health_reply] { Message::Reply { result:Ok(value), .. } => assert_eq!(value.get("ready"), Some(&IvyJson::Bool(true))), _ => panic!("Health failed: {}", stringify(&frames[health_reply], 1 << 20).unwrap()) }
        let bytes_hash = ivy_contracts::hash::digest(fs::read(&destination).unwrap());
        assert_eq!(snapshot.get("bytesHash"), Some(&bytes_hash.clone().into()));
        let copy = rusqlite::Connection::open_with_flags(&destination, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let pages = copy.query_row("PRAGMA page_count", [], |row| row.get::<_, i64>(0)).unwrap();
        assert!(pages > 100);
        assert_eq!(snapshot.get("pages").and_then(IvyJson::as_f64), Some(pages as f64));
        assert_eq!(copy.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
        assert_eq!(copy.query_row("SELECT COUNT(*) FROM fixture_payload", [], |row| row.get::<_, i64>(0)).unwrap(), 4096);
        assert_eq!(copy.query_row("SELECT revoked FROM credentials WHERE principal_id='alpha'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
        let source = rusqlite::Connection::open_with_flags(root.0.join("hive.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(source.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
        assert_eq!(source.query_row("SELECT COUNT(*) FROM fixture_payload", [], |row| row.get::<_, i64>(0)).unwrap(), 4096);
        let projected = IvyJson::Array(frames);
        save_kernel_process_cases(serde_json::json!({
            "case":"production-entry-backup-progress-close","frames":serde_json::from_str::<serde_json::Value>(&stringify(&projected, 1 << 20).unwrap()).unwrap(),
            "progressFrameIndices":progress,"backupReplyIndex":backup_reply,"healthReplyIndex":health_reply,"closeStartedIndex":close_started,
            "healthBeforeBackupReply":health_reply < backup_reply,"closeStartedBeforeBackupReply":close_started < backup_reply,
            "snapshotPages":pages,"bytesHash":bytes_hash,"payloadRows":4096,"alphaRevoked":0,"sourceRowsAfterReap":4096,
            "sourceIntegrityAfterReap":"ok","targetIntegrity":"ok","databaseLockedBeforeBackup":true,"databaseLockedAfterReap":false,
            "exitCode":completion.exit_code,"errorCode":completion.error_code,"stdinRetainedUntilReap":true,
            "productionEntryExecuted":true,"copyGateUsed":false,"clockGateUsed":false,"originalReplay":false,
            "closeReplyObserved":false,"fatalFrameObserved":false,"progressPhaseDistinguished":false
        }));
    });
}

#[test]
#[cfg(windows)]
fn initializing_entry_parent_death_retires_pre_ready_owner_with_constructor_blocked() {
    use ivy_host_runtime::platform::windows_process::process_identity;
    let root = Root::new();
    let profile = original_profile(&root);
    let store = ivy_hive::store::Store::open(&root.0.join("hive.sqlite")).unwrap();
    store.connection().execute_batch("CREATE TABLE fixture_payload(value TEXT); INSERT INTO fixture_payload VALUES('retained');").unwrap();
    drop(store);
    let launch = kernel_launch(&root, &profile);
    Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        // No outer Host Job may mask the StorageClient parent's own fence.
        let mut command = tokio::process::Command::new(&launch.executable);
        command.arg("--kernel-startup-parent").envs(&launch.environment)
            .env("IVY_HIVE_PROCESS_OPTIONS", stringify(&launch.options, 1 << 20).unwrap())
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped())
            .kill_on_drop(true).creation_flags(0x08000000);
        if let Some(job) = launch.job_launcher { command.env("IVY_HIVE_TEST_JOB", job); }
        let mut parent = command.spawn().unwrap();
        let parent_pid = parent.id().unwrap();
        let ready = timeout(Duration::from_secs(10), async {
            while !root.0.join("parent-ready").is_file() { sleep(Duration::from_millis(5)).await; }
        }).await;
        if ready.is_err() {
            let _ = parent.start_kill();
            let output = timeout(Duration::from_secs(5), parent.wait_with_output()).await;
            panic!("Initializing owner fixture did not block: {output:?}");
        }
        assert_eq!(fs::read_to_string(root.0.join("parent-startup-state")).unwrap(), "starting; no Ready or ordinary request; clock gate held");
        assert!(root.0.join("block-clock").is_file());
        assert!(root.0.join("clock-blocked").is_file());
        let pid = fs::read_to_string(root.0.join("kernel-pid")).unwrap().parse::<u32>().unwrap();
        let identity = process_identity(pid).unwrap().expect("Pre-Ready owner vanished");
        assert!(database_locked(&root));
        fs::write(root.0.join("parent-exit"), b"exit during actual initialization").unwrap();
        let status = timeout(Duration::from_secs(5), parent.wait()).await.unwrap().unwrap();
        assert_eq!(status.code(), Some(17));
        let mut identity_errors = vec![];
        let retired = timeout(Duration::from_secs(5), async {
            loop {
                match process_identity(pid) {
                    Ok(None) => break "absent",
                    Ok(Some(current)) if current.created_filetime != identity.created_filetime => break "reused",
                    Ok(Some(_)) => {},
                    Err(error) => identity_errors.push(error.raw_os_error()),
                }
                sleep(Duration::from_millis(5)).await;
            }
        }).await;
        if retired.is_err() {
            save_kernel_process_cases(serde_json::json!({"case":"initializing-parent-death","execution":"blocked","identityFinding":null,"identityProbeErrors":identity_errors,"workerPid":pid,"workerCreatedFiletime":identity.created_filetime.to_string(),"parentExitCode":status.code(),"databaseRecoveryNotRun":true}));
        }
        let identity_final = retired.expect("No definite absence/reuse of pre-Ready owner after parent death");
        assert!(root.0.join("block-clock").is_file());
        assert!(root.0.join("clock-blocked").is_file());
        assert!(!database_locked(&root));
        let source = rusqlite::Connection::open_with_flags(root.0.join("hive.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(source.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
        assert_eq!(source.query_row("SELECT value FROM fixture_payload", [], |row| row.get::<_, String>(0)).unwrap(), "retained");
        // Credential initialization commits before the first Registry clock
        // call. Pre-Ready owner death does not promise atomic startup rollback.
        assert_eq!(source.query_row("SELECT revoked FROM credentials WHERE principal_id='alpha'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
        save_kernel_process_cases(serde_json::json!({
            "case":"initializing-parent-death","parentPid":parent_pid,"parentExitCode":status.code(),
            "workerPid":pid,"workerCreatedFiletime":identity.created_filetime.to_string(),"workerIdentityFinal":identity_final,
            "identityProbeErrors":identity_errors,"identityDeadlineMs":5000,"identityRetryMs":5,"identityFinding":true,
            "lifecycleAtDeath":"starting","ordinaryRequestsPosted":0,"pendingCountAtDeath":0,"queuedBytesAtDeath":0,
            "clockGateHeldUntilRetirement":true,"databaseLockedBeforeDeath":true,"databaseLockedAfterRetirement":false,
            "sourceIntegrityAfterRetirement":"ok","sourcePayloadAfterRetirement":"retained","alphaRevokedAfterRetirement":0,
            "productionEntryExecuted":true,"cooperativeClose":false,"outerHostJob":false,"forcedStopUsed":false,"originalReplay":false,
            "readyObserved":false,"startupRollbackClaimed":false
        }));
    });
}

#[test]
#[cfg(windows)]
fn initialized_entry_backup_lock_failure_preserves_owner_and_allows_same_process_retry() {
    use ivy_host_runtime::platform::windows_process::process_identity;
    let root = Root::new();
    let profile = original_profile(&root);
    let store = ivy_hive::store::Store::open(&root.0.join("hive.sqlite")).unwrap();
    store.connection().execute_batch("CREATE TABLE fixture_payload(id INTEGER PRIMARY KEY, bytes BLOB); WITH RECURSIVE n(value) AS (VALUES(1) UNION ALL SELECT value+1 FROM n WHERE value<256) INSERT INTO fixture_payload SELECT value,zeroblob(4096) FROM n;").unwrap();
    drop(store);
    let destination = root.0.join("existing-copy.sqlite");
    let locker = rusqlite::Connection::open(&destination).unwrap();
    locker.execute_batch("CREATE TABLE sentinel(value TEXT); INSERT INTO sentinel VALUES('retained'); BEGIN IMMEDIATE;").unwrap();
    let launch = kernel_launch(&root, &profile);
    assert_eq!(launch.args, vec![std::ffi::OsString::from("--kernel")]);
    Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        // Exercise worker_entry::run and its unwrapped KernelWorker backup.
        // The competing destination transaction is the only copy restriction.
        let client = StorageClient::new(launch, Arc::new(|_| {}));
        client.start().wait().await.unwrap();
        let pid = fs::read_to_string(root.0.join("kernel-pid")).unwrap().parse::<u32>().unwrap();
        let identity = process_identity(pid).unwrap().unwrap();
        let backup = IvyJson::object([
            ("action", "backup".into()),
            ("destination", destination.to_str().unwrap().into()),
        ]);
        let error = request(&client, backup.clone()).await.unwrap_err();
        assert_eq!(error.to_wire(), IvyError::internal().to_wire());
        assert!(matches!(client.lifecycle(), Lifecycle::Ready(_)));
        assert_eq!(client.pending_count(), 0);
        assert_eq!(client.queued_bytes(), 0);
        assert!(database_locked(&root));
        assert_eq!(process_identity(pid).unwrap().unwrap().created_filetime, identity.created_filetime);
        assert_eq!(request(&client, payload("health")).await.unwrap().get("ready"), Some(&IvyJson::Bool(true)));
        assert_eq!(locker.query_row("SELECT value FROM sentinel", [], |row| row.get::<_, String>(0)).unwrap(), "retained");
        locker.execute_batch("ROLLBACK").unwrap();
        drop(locker);
        {
            let target = rusqlite::Connection::open_with_flags(&destination, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            assert_eq!(target.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
            assert_eq!(target.query_row("SELECT value FROM sentinel", [], |row| row.get::<_, String>(0)).unwrap(), "retained");
        }
        // This is an explicit new request after a definitive error Reply and
        // removal of its external cause, never replay of an uncertain request.
        let snapshot = request(&client, backup).await.unwrap();
        assert_eq!(process_identity(pid).unwrap().unwrap().created_filetime, identity.created_filetime);
        assert!(matches!(client.lifecycle(), Lifecycle::Ready(_)));
        assert_eq!(client.pending_count(), 0);
        assert_eq!(client.queued_bytes(), 0);
        assert!(!root.0.join("backup-paused").exists());
        assert!(!root.0.join("block-clock").exists());
        let bytes_hash = ivy_contracts::hash::digest(fs::read(&destination).unwrap());
        assert_eq!(snapshot.get("bytesHash"), Some(&bytes_hash.clone().into()));
        let copy = rusqlite::Connection::open_with_flags(&destination, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let pages = copy.query_row("PRAGMA page_count", [], |row| row.get::<_, i64>(0)).unwrap();
        assert!(pages > 100);
        assert_eq!(snapshot.get("pages").and_then(IvyJson::as_f64), Some(pages as f64));
        assert_eq!(copy.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
        assert_eq!(copy.query_row("SELECT COUNT(*) FROM fixture_payload", [], |row| row.get::<_, i64>(0)).unwrap(), 256);
        assert_eq!(copy.query_row("SELECT revoked FROM credentials WHERE principal_id='alpha'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(copy.query_row("SELECT COUNT(*) FROM sqlite_schema WHERE name='sentinel'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
        drop(copy);
        client.close().wait().await.unwrap();
        stopped(&client).await;
        assert!(!database_locked(&root));
        let source = rusqlite::Connection::open_with_flags(root.0.join("hive.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(source.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
        assert_eq!(source.query_row("SELECT COUNT(*) FROM fixture_payload", [], |row| row.get::<_, i64>(0)).unwrap(), 256);
        save_kernel_process_cases(serde_json::json!({
            "case":"production-entry-backup-lock-error-retry","workerPid":pid,"workerCreatedFiletime":identity.created_filetime.to_string(),
            "error":serde_json::from_str::<serde_json::Value>(&stringify(&error.to_wire(), 1 << 20).unwrap()).unwrap(),
            "sameOwnerAfterErrorAndRetry":true,"readyAfterErrorAndRetry":true,"healthAfterError":true,
            "reservationsAfterError":0,"queuedBytesAfterError":0,"sentinelAfterError":"retained","targetIntegrityAfterError":"ok",
            "destinationLock":"external BEGIN IMMEDIATE; rollback before explicit next request",
            "snapshotPages":pages,"bytesHash":bytes_hash,"payloadRows":256,"alphaRevoked":0,"sentinelTablesAfterSuccess":0,"targetIntegrityAfterSuccess":"ok",
            "sourceIntegrityAfterClose":"ok","sourceRowsAfterClose":256,"databaseLockedBeforeClose":true,"databaseLockedAfterClose":false,
            "productionEntryExecuted":true,"copyGateUsed":false,"clockGateUsed":false,"originalReplay":false,
            "closeReplyClaimed":false,"automaticReplay":false
        }));
    });
}

#[test]
#[cfg(windows)]
fn initialized_hash_parent_death_retires_reader_and_preserves_completed_destination() {
    use ivy_host_runtime::platform::windows_process::process_identity;
    use sha2::{Digest, Sha256};
    use std::os::windows::fs::OpenOptionsExt;
    let root = Root::new();
    let profile = original_profile(&root);
    let store = ivy_hive::store::Store::open(&root.0.join("hive.sqlite")).unwrap();
    store.connection().execute_batch("CREATE TABLE fixture_payload(id INTEGER PRIMARY KEY, bytes BLOB); WITH RECURSIVE n(value) AS (VALUES(1) UNION ALL SELECT value+1 FROM n WHERE value<256) INSERT INTO fixture_payload SELECT value,zeroblob(4096) FROM n;").unwrap();
    drop(store);
    let destination = root.0.join("completed-copy.sqlite");
    {
        let target = rusqlite::Connection::open(&destination).unwrap();
        target
            .execute_batch("CREATE TABLE sentinel(value TEXT); INSERT INTO sentinel VALUES('old');")
            .unwrap();
    }
    let check_destination = || {
        let target = rusqlite::Connection::open_with_flags(
            &destination,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        target.busy_timeout(Duration::ZERO).unwrap();
        assert_eq!(
            target
                .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        assert_eq!(
            target
                .query_row(
                    "SELECT COUNT(*) FROM fixture_payload WHERE length(bytes)=4096",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            256
        );
        assert_eq!(
            target
                .query_row(
                    "SELECT revoked FROM credentials WHERE principal_id='alpha'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            target
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE name='sentinel'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        let pages: i64 = target
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .unwrap();
        assert!(pages > 100);
        pages
    };
    let launch = kernel_launch(&root, &profile);
    let backup = IvyJson::object([
        ("action", "backup".into()),
        ("destination", destination.to_str().unwrap().into()),
    ]);
    Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        // No outer Host Job: only the real StorageClient parent owns the child.
        let mut command = tokio::process::Command::new(&launch.executable);
        command.arg("--kernel-hash-parent").envs(&launch.environment)
            .env("IVY_HIVE_PROCESS_OPTIONS", stringify(&launch.options, 1 << 20).unwrap())
            .env("IVY_HIVE_PROCESS_BLOCKED_REQUEST", stringify(&backup, 1 << 20).unwrap())
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped())
            .kill_on_drop(true).creation_flags(0x08000000);
        if let Some(job) = launch.job_launcher { command.env("IVY_HIVE_TEST_JOB", job); }
        let mut parent = command.spawn().unwrap();
        let parent_pid = parent.id().unwrap();
        let ready = timeout(Duration::from_secs(10), async {
            while !root.0.join("parent-ready").is_file() { sleep(Duration::from_millis(5)).await; }
        }).await;
        if ready.is_err() {
            let _ = parent.start_kill();
            let output = timeout(Duration::from_secs(5), parent.wait_with_output()).await;
            panic!("Hash owner fixture did not become ready: {output:?}");
        }
        let state: serde_json::Value = serde_json::from_slice(&fs::read(root.0.join("parent-hash-state")).unwrap()).unwrap();
        assert_eq!(state["ready"], true);
        assert_eq!(state["backupPending"], true);
        assert_eq!(state["pendingCount"], 1);
        assert!(state["queuedBytes"].as_f64().unwrap() > 0.0);
        assert!(root.0.join("copy-progress-before-hash").is_file());
        assert!(root.0.join("hash-paused").is_file());
        assert!(!root.0.join("release-hash").exists());
        assert!(!root.0.join("backup-paused").exists());
        assert!(!root.0.join("block-clock").exists());
        let pid = fs::read_to_string(root.0.join("kernel-pid")).unwrap().parse::<u32>().unwrap();
        let identity = process_identity(pid).unwrap().expect("Hash reader vanished");
        assert!(database_locked(&root));
        let pages = check_destination();
        let hash_destination = || format!("sha256:{}", Sha256::digest(fs::read(&destination).unwrap()).iter().map(|byte| format!("{byte:02x}")).collect::<String>());
        let before_hash = hash_destination();
        // All local SQLite/read handles above are closed. A share-zero open
        // must still fail while Store::backup_to retains its actual hash file.
        let sharing_error = fs::OpenOptions::new().read(true).share_mode(0).open(&destination).unwrap_err();
        assert_eq!(sharing_error.raw_os_error(), Some(32));
        fs::write(root.0.join("parent-exit"), b"exit during hash callback").unwrap();
        let status = timeout(Duration::from_secs(5), parent.wait()).await.unwrap().unwrap();
        assert_eq!(status.code(), Some(17));
        let mut identity_errors = vec![];
        let identity_result = timeout(Duration::from_secs(5), async {
            loop {
                match process_identity(pid) {
                    Ok(None) => break "absent",
                    Ok(Some(current)) if current.created_filetime != identity.created_filetime => break "reused",
                    Ok(Some(_)) => {},
                    Err(error) => identity_errors.push(error.raw_os_error()),
                }
                sleep(Duration::from_millis(5)).await;
            }
        }).await;
        if identity_result.is_err() {
            save_kernel_process_cases(serde_json::json!({"case":"hash-parent-death","execution":"blocked","identityFinding":null,"identityProbeErrors":identity_errors,"workerPid":pid,"workerCreatedFiletime":identity.created_filetime.to_string(),"parentExitCode":status.code(),"destinationAfterRetirementNotRun":true}));
        }
        let identity_final = identity_result.expect("No definite absent/reused hash reader identity after owner death");
        assert!(!root.0.join("release-hash").exists());
        assert!(!database_locked(&root));
        drop(fs::OpenOptions::new().read(true).share_mode(0).open(&destination).expect("Hash reader handle survived retirement"));
        assert_eq!(check_destination(), pages);
        let after_hash = hash_destination();
        assert_eq!(after_hash, before_hash);
        {
            let source = rusqlite::Connection::open_with_flags(root.0.join("hive.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            assert_eq!(source.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
            assert_eq!(source.query_row("SELECT COUNT(*) FROM fixture_payload WHERE length(bytes)=4096", [], |row| row.get::<_, i64>(0)).unwrap(), 256);
            assert_eq!(source.query_row("SELECT revoked FROM credentials WHERE principal_id='alpha'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
        }
        save_kernel_process_cases(serde_json::json!({
            "case":"hash-parent-death","parentPid":parent_pid,"parentExitCode":status.code(),
            "workerPid":pid,"workerCreatedFiletime":identity.created_filetime.to_string(),
            "workerIdentityFinal":identity_final,"identityProbeErrors":identity_errors,
            "identityDeadlineMs":5000,"identityRetryMs":5,"identityFinding":true,
            "databaseLockedBeforeDeath":true,"databaseLockedAfterRetirement":false,
            "sourceIntegrityAfterRetirement":"ok","sourcePayloadRowsAfterRetirement":256,
            "sourceAlphaRevokedAfterRetirement":0,"targetReadableBeforeDeath":true,
            "targetIntegrityBeforeAndAfterRetirement":"ok","targetPayloadRowsBeforeAndAfterRetirement":256,
            "targetAlphaRevokedBeforeAndAfterRetirement":0,"targetSentinelTablesBeforeAndAfterRetirement":0,
            "targetPagesBeforeAndAfterRetirement":pages,"targetFullHashBeforeDeath":before_hash,"targetFullHashAfterRetirement":after_hash,
            "exclusiveOpenErrorBeforeDeath":sharing_error.raw_os_error(),"exclusiveOpenAfterRetirement":true,
            "copyProgressObserved":true,"hashGateHeldUntilRetirement":true,"parentState":state,
            "backupReplyObserved":false,"backupCompletionInferred":false,"notExecutedInferred":false,
            "returnedBackupHash":null,"cooperativeClose":false,"outerHostJob":false,"forcedStopUsed":false,
            "productionEntryExecuted":false,"originalReplay":false,
            "phaseEvidence":"Store copy/hash callback sites plus actual destination lock-to-readable transition; synchronous callback holds real hash reader"
        }));
    });
}

#[test]
#[cfg(windows)]
fn initialized_backup_parent_death_retires_owner_and_recovers_interrupted_destination() {
    use ivy_host_runtime::platform::windows_process::process_identity;
    let root = Root::new();
    let profile = original_profile(&root);
    let store = ivy_hive::store::Store::open(&root.0.join("hive.sqlite")).unwrap();
    store.connection().execute_batch("CREATE TABLE fixture_payload(id INTEGER PRIMARY KEY, bytes BLOB); WITH RECURSIVE n(value) AS (VALUES(1) UNION ALL SELECT value+1 FROM n WHERE value<256) INSERT INTO fixture_payload SELECT value,zeroblob(4096) FROM n;").unwrap();
    let source_pages: i64 = store
        .connection()
        .query_row("PRAGMA page_count", [], |row| row.get(0))
        .unwrap();
    assert!(source_pages > 100);
    drop(store);
    let destination = root.0.join("existing-copy.sqlite");
    {
        let target = rusqlite::Connection::open(&destination).unwrap();
        target
            .execute_batch(
                "CREATE TABLE sentinel(value TEXT); INSERT INTO sentinel VALUES('retained');",
            )
            .unwrap();
    }
    let launch = kernel_launch(&root, &profile);
    let backup = IvyJson::object([
        ("action", "backup".into()),
        ("destination", destination.to_str().unwrap().into()),
    ]);
    Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        // The only parent-death fence is the actual StorageClient's Host Job.
        // An additional outer Host Job would mask this ownership boundary.
        let mut command = tokio::process::Command::new(&launch.executable);
        command.arg("--kernel-backup-parent").envs(&launch.environment)
            .env("IVY_HIVE_PROCESS_OPTIONS", stringify(&launch.options, 1 << 20).unwrap())
            .env("IVY_HIVE_PROCESS_BLOCKED_REQUEST", stringify(&backup, 1 << 20).unwrap())
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped())
            .kill_on_drop(true).creation_flags(0x08000000);
        if let Some(job) = launch.job_launcher { command.env("IVY_HIVE_TEST_JOB", job); }
        let mut parent = command.spawn().unwrap();
        let parent_pid = parent.id().unwrap();
        let ready = timeout(Duration::from_secs(10), async {
            while !root.0.join("parent-ready").is_file() { sleep(Duration::from_millis(5)).await; }
        }).await;
        if ready.is_err() {
            let _ = parent.start_kill();
            let output = timeout(Duration::from_secs(5), parent.wait_with_output()).await;
            panic!("Backup owner fixture did not become ready: {output:?}");
        }
        let state: serde_json::Value = serde_json::from_slice(&fs::read(root.0.join("parent-backup-state")).unwrap()).unwrap();
        assert_eq!(state["healthReady"], true);
        assert_eq!(state["backupPending"], true);
        assert_eq!(state["credentialsPending"], true);
        assert_eq!(state["clockGateHeld"], true);
        assert_eq!(state["pendingCount"], 2);
        assert!(state["queuedBytes"].as_f64().unwrap() > 0.0);
        assert!(root.0.join("backup-paused").is_file());
        assert!(!root.0.join("release-backup").exists());
        assert!(root.0.join("block-clock").is_file());
        assert!(root.0.join("clock-blocked").is_file());
        let pid = fs::read_to_string(root.0.join("kernel-pid")).unwrap().parse::<u32>().unwrap();
        let identity = process_identity(pid).unwrap().expect("Initialized backup worker vanished");
        assert!(database_locked(&root));
        {
            let target = rusqlite::Connection::open_with_flags(&destination, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            target.busy_timeout(Duration::ZERO).unwrap();
            assert!(matches!(target.query_row("SELECT COUNT(*) FROM sqlite_schema", [], |row| row.get::<_, i64>(0)), Err(rusqlite::Error::SqliteFailure(error, _)) if matches!(error.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)));
        }
        fs::write(root.0.join("parent-exit"), b"exit abruptly during actual backup").unwrap();
        let status = timeout(Duration::from_secs(5), parent.wait()).await.unwrap().unwrap();
        assert_eq!(status.code(), Some(17));
        let mut identity_errors = vec![];
        let identity_result = timeout(Duration::from_secs(5), async {
            loop {
                match process_identity(pid) {
                    Ok(None) => break "absent",
                    Ok(Some(current)) if current.created_filetime != identity.created_filetime => break "reused",
                    Ok(Some(_)) => {},
                    Err(error) => identity_errors.push(error.raw_os_error()),
                }
                sleep(Duration::from_millis(5)).await;
            }
        }).await;
        if identity_result.is_err() {
            save_kernel_process_cases(serde_json::json!({"case":"backup-parent-death","execution":"blocked","identityFinding":null,"identityProbeErrors":identity_errors,"workerPid":pid,"workerCreatedFiletime":identity.created_filetime.to_string(),"parentExitCode":status.code(),"databaseRecoveryNotRun":true}));
        }
        let identity_final = identity_result.expect("No definite absent/reused backup worker identity after owner death");
        assert!(!root.0.join("release-backup").exists());
        assert!(root.0.join("block-clock").is_file());
        assert!(root.0.join("clock-blocked").is_file());
        assert!(!database_locked(&root));
        {
            let source = rusqlite::Connection::open_with_flags(root.0.join("hive.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            assert_eq!(source.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
            assert_eq!(source.query_row("SELECT COUNT(*) FROM fixture_payload", [], |row| row.get::<_, i64>(0)).unwrap(), 256);
            assert_eq!(source.query_row("SELECT revoked FROM credentials WHERE principal_id='alpha'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
            assert_eq!(source.query_row("SELECT COUNT(*) FROM credentials WHERE principal_id='beta'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
            // Reopening read/write allows SQLite hot-journal recovery; this is
            // a logical recovery check, not immediate byte-identical durability.
            let target = rusqlite::Connection::open(&destination).unwrap();
            assert_eq!(target.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
            assert_eq!(target.query_row("SELECT value FROM sentinel", [], |row| row.get::<_, String>(0)).unwrap(), "retained");
            assert_eq!(target.query_row("SELECT COUNT(*) FROM sqlite_schema WHERE name='fixture_payload'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
        }
        save_kernel_process_cases(serde_json::json!({
            "case":"backup-parent-death","parentPid":parent_pid,"parentExitCode":status.code(),
            "workerPid":pid,"workerCreatedFiletime":identity.created_filetime.to_string(),
            "workerIdentityFinal":identity_final,"identityProbeErrors":identity_errors,
            "identityDeadlineMs":5000,"identityRetryMs":5,"identityFinding":true,
            "sourcePagesBeforeStartup":source_pages,"sourcePayloadRowsAfterRetirement":256,
            "sourceAlphaRevokedAfterRetirement":0,"sourceIntegrityAfterRetirement":"ok",
            "sourceBetaRowsAfterRetirement":0,"clockGateHeldUntilRetirement":true,
            "databaseLockedBeforeDeath":true,"databaseLockedAfterRetirement":false,
            "targetLockedDuringCopy":true,"targetIntegrityAfterRecovery":"ok",
            "targetSentinelAfterRecovery":"retained","targetPayloadTablesAfterRecovery":0,
            "parentState":state,"copyGateReleased":false,"cooperativeClose":false,"outerHostJob":false,
            "forcedStopUsed":false,"productionEntryExecuted":false,"originalReplay":false,
            "testGate":"after first real yielding batch; initialized Kernel/WorkerActions/Store"
        }));
    });
}

#[test]
#[cfg(windows)]
fn initialized_backup_pipe_loss_exits_without_releasing_copy_and_preserves_destination() {
    use ivy_host_runtime::{
        platform::windows_process::process_identity,
        process::{self, Command, LaunchContext},
    };
    let profile_root = Root::new();
    let profile = original_profile(&profile_root);
    let mut cases = vec![];
    Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        for name in ["backup-input-eof", "backup-truncated-input", "backup-broken-output"] {
            let root = Root::new();
            let store = ivy_hive::store::Store::open(&root.0.join("hive.sqlite")).unwrap();
            store.connection().execute_batch("CREATE TABLE fixture_payload(id INTEGER PRIMARY KEY, bytes BLOB); WITH RECURSIVE n(value) AS (VALUES(1) UNION ALL SELECT value+1 FROM n WHERE value<256) INSERT INTO fixture_payload SELECT value,zeroblob(4096) FROM n;").unwrap();
            let source_pages: i64 = store.connection().query_row("PRAGMA page_count", [], |row| row.get(0)).unwrap();
            assert!(source_pages > 100, "The actual copy must yield before completion");
            drop(store);
            let destination = root.0.join("existing-copy.sqlite");
            {
                let target = rusqlite::Connection::open(&destination).unwrap();
                target.execute_batch("CREATE TABLE sentinel(value TEXT); INSERT INTO sentinel VALUES('retained');").unwrap();
            }
            let mut launch = kernel_launch(&root, &profile);
            launch.args = vec!["--kernel-gated-backup".into()];
            let mut child = process::spawn(&Command {
                executable: "worker".into(), args: launch.args, cwd: None, timeout_ms: 30000,
            }, &root.0, &BTreeMap::from([("worker".into(), launch.executable)]), LaunchContext {
                environment: launch.environment, job_launcher: launch.job_launcher,
                raw_output: true, ..Default::default()
            }).await.unwrap();
            let mut stdin = child.stdin.take();
            let mut frames = Some(FrameReader::new(child.stdout.take().unwrap()));
            write_frame(stdin.as_mut().unwrap(), &launch.options).await.unwrap();
            assert_eq!(timeout(Duration::from_secs(10), frames.as_mut().unwrap().next()).await.unwrap().unwrap().unwrap(), Message::Ready.to_json());
            let pid = fs::read_to_string(root.0.join("kernel-pid")).unwrap().parse::<u32>().unwrap();
            let identity = process_identity(pid).unwrap().expect("Ready worker vanished");
            write_frame(stdin.as_mut().unwrap(), &Request {
                id: 1, payload: IvyJson::object([
                    ("action", "backup".into()),
                    ("destination", destination.to_str().unwrap().into()),
                ]),
            }.to_json()).await.unwrap();
            assert_eq!(timeout(Duration::from_secs(10), frames.as_mut().unwrap().next()).await.unwrap().unwrap().unwrap(), Message::Started(1).to_json());
            wait_file(root.0.join("backup-paused")).await;
            assert_eq!(timeout(Duration::from_secs(10), frames.as_mut().unwrap().next()).await.unwrap().unwrap().unwrap(), Message::Progress(1).to_json());
            assert!(database_locked(&root));
            {
                let target = rusqlite::Connection::open_with_flags(&destination, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
                target.busy_timeout(Duration::ZERO).unwrap();
                assert!(matches!(target.query_row("SELECT COUNT(*) FROM sqlite_schema", [], |row| row.get::<_, i64>(0)), Err(rusqlite::Error::SqliteFailure(error, _)) if matches!(error.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)));
            }
            assert!(!root.0.join("release-backup").exists());
            match name {
                "backup-input-eof" => drop(stdin.take()),
                "backup-truncated-input" => {
                    stdin.as_mut().unwrap().write_all(&[0, 0]).await.unwrap();
                    stdin.as_mut().unwrap().flush().await.unwrap();
                    // A real pipe-handle drop, rather than Windows shutdown().
                    drop(stdin.take());
                }
                "backup-broken-output" => {
                    drop(frames.take());
                    write_frame(stdin.as_mut().unwrap(), &Request {
                        id: 2, payload: payload("health"),
                    }.to_json()).await.unwrap();
                }
                _ => unreachable!(),
            }
            let completion = match timeout(Duration::from_secs(5), child.completion.wait()).await {
                Ok(completion) => completion,
                Err(error) => {
                    let cleanup = child.stop(Duration::ZERO).await;
                    panic!("Initialized backup did not exit after {name}: {error}; cleanup {cleanup:?}");
                }
            };
            assert_eq!(completion.exit_code, Some(if name == "backup-input-eof" { 0 } else { 1 }));
            assert!(completion.error_code.is_none());
            // Host completion is the retirement proof. This additional lookup
            // may remain unavailable for an exited Windows process object; its
            // error or retained identity never becomes an absence finding.
            let (identity_lookup, identity_error) = match process_identity(pid) {
                Ok(None) => (Some("absent"), None),
                Ok(Some(current)) if current.created_filetime != identity.created_filetime => (Some("reused"), None),
                Ok(Some(_)) => (Some("same-creation"), None),
                Err(error) => (None, error.raw_os_error()),
            };
            assert!(!root.0.join("release-backup").exists());
            assert!(!database_locked(&root));
            // SQLite recovery is allowed here; no byte-for-byte durability or
            // observation of the dropped output pipe is inferred from it.
            {
                let source = rusqlite::Connection::open_with_flags(root.0.join("hive.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
                assert_eq!(source.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
                assert_eq!(source.query_row("SELECT COUNT(*) FROM fixture_payload", [], |row| row.get::<_, i64>(0)).unwrap(), 256);
                assert_eq!(source.query_row("SELECT revoked FROM credentials WHERE principal_id='alpha'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
                let target = rusqlite::Connection::open(&destination).unwrap();
                assert_eq!(target.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
                assert_eq!(target.query_row("SELECT value FROM sentinel", [], |row| row.get::<_, String>(0)).unwrap(), "retained");
                assert_eq!(target.query_row("SELECT COUNT(*) FROM sqlite_schema WHERE name='fixture_payload'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
            }
            let trailing = if let Some(frames) = frames.as_mut() {
                let mut trailing = vec![];
                while let Some(frame) = timeout(Duration::from_secs(5), frames.next()).await.unwrap().unwrap() {
                    trailing.push(frame);
                }
                assert!(trailing.is_empty());
                Some(trailing.iter().map(|frame| serde_json::from_str::<serde_json::Value>(&stringify(frame, 1 << 20).unwrap()).unwrap()).collect::<Vec<_>>())
            } else { None };
            cases.push(serde_json::json!({
                "case":name,"exitCode":completion.exit_code,"errorCode":completion.error_code,
                "pid":pid,"creationFiletime":identity.created_filetime,"hostRetirementConfirmed":true,
                "identityLookupAfterReap":identity_lookup,"identityProbeErrorCode":identity_error,
                "identityProbeFinding":identity_lookup.map(|value| value != "same-creation"),
                "identityProbeUsedAsRetirementProof":false,
                "sourcePagesBeforeStartup":source_pages,"sourcePayloadRowsAfterReap":256,
                "sourceAlphaRevokedAfterReap":0,"sourceIntegrityAfterReap":"ok",
                "targetLockedDuringCopy":true,"targetIntegrityAfterRecovery":"ok",
                "targetSentinelAfterRecovery":"retained","targetPayloadTablesAfterRecovery":0,
                "databaseLockedDuringCopy":true,"databaseLockedAfterReap":false,
                "backupStartedObserved":true,"actualFirstBatchProgressObserved":true,
                "copyGateReleased":false,"stdinRetainedUntilReap":name == "backup-broken-output",
                "trailingFrames":trailing,"forcedStopUsed":false,
                "productionEntryExecuted":false,"originalReplay":false,
                "testGate":"after first real yielding batch; initialized Kernel/WorkerActions/Store"
            }));
        }
    });
    save_kernel_process_cases(serde_json::json!(cases));
}

#[test]
#[cfg(windows)]
fn initialized_backup_timeout_retains_copy_until_fatal_reap_then_allows_explicit_replacement() {
    use ivy_host_runtime::platform::windows_process::process_identity;
    let root = Root::new();
    let profile = original_profile(&root);
    let store = ivy_hive::store::Store::open(&root.0.join("hive.sqlite")).unwrap();
    store.connection().execute_batch("CREATE TABLE fixture_payload(id INTEGER PRIMARY KEY, bytes BLOB); WITH RECURSIVE n(value) AS (VALUES(1) UNION ALL SELECT value+1 FROM n WHERE value<256) INSERT INTO fixture_payload SELECT value,zeroblob(4096) FROM n;").unwrap();
    let source_pages: i64 = store
        .connection()
        .query_row("PRAGMA page_count", [], |row| row.get(0))
        .unwrap();
    assert!(source_pages > 100);
    drop(store);
    let destination = root.0.join("existing-copy.sqlite");
    {
        let target = rusqlite::Connection::open(&destination).unwrap();
        target
            .execute_batch(
                "CREATE TABLE sentinel(value TEXT); INSERT INTO sentinel VALUES('retained');",
            )
            .unwrap();
    }
    let mut launch = kernel_launch(&root, &profile);
    launch.args = vec!["--kernel-gated-backup".into()];
    Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let client = StorageClient::new(launch, Arc::new(|_| {}));
        client.start().wait().await.unwrap();
        let first_pid = fs::read_to_string(root.0.join("kernel-pid")).unwrap().parse::<u32>().unwrap();
        let identity = process_identity(first_pid).unwrap().expect("Ready worker vanished");
        let backup_payload = IvyJson::object([
            ("action", "backup".into()),
            ("destination", destination.to_str().unwrap().into()),
        ]);
        let backup = client.request(backup_payload.clone(), RequestOptions {
            timeout_ms: 1000, terminate_on_timeout: false, ..Default::default()
        });
        wait_file(root.0.join("backup-paused")).await;
        let backup_error = timeout(Duration::from_secs(5), backup).await.unwrap().unwrap().unwrap_err();
        assert_eq!(backup_error.code, "deadline_exceeded");
        assert_eq!(backup_error.outcome, Outcome::Unknown);
        assert!(matches!(client.lifecycle(), Lifecycle::Ready(_)));
        assert_eq!(client.pending_count(), 1);
        assert!(client.queued_bytes() > 0);
        assert!(database_locked(&root));
        assert!(!root.0.join("release-backup").exists());
        client.start().wait().await.unwrap();
        assert_eq!(fs::read_to_string(root.0.join("kernel-pid")).unwrap().parse::<u32>().unwrap(), first_pid);
        assert_eq!(process_identity(first_pid).unwrap().unwrap().created_filetime, identity.created_filetime);
        let second = request(&client, backup_payload).await.unwrap_err();
        assert_eq!(second.message, "A storage backup is already active.");
        assert_eq!(client.pending_count(), 1);
        fs::write(root.0.join("block-clock"), b"hold actual credential action").unwrap();
        struct Unblock(PathBuf);
        impl Drop for Unblock { fn drop(&mut self) { let _ = fs::remove_file(&self.0); } }
        let _unblock = Unblock(root.0.join("block-clock"));
        let credentials = client.request(IvyJson::object([
            ("action", "credentials.configure".into()),
            ("credentials", IvyJson::Array(vec![IvyJson::object([
                ("principalId", "beta".into()),
                ("digest", format!("sha256:{}", "b".repeat(64)).into()),
            ])])),
        ]), RequestOptions { timeout_ms: 1500, ..Default::default() });
        wait_file(root.0.join("clock-blocked")).await;
        let queued = client.request(payload("health"), RequestOptions {
            timeout_ms: 100, terminate_on_timeout: false, ..Default::default()
        });
        let queued_error = timeout(Duration::from_secs(5), queued).await.unwrap().unwrap().unwrap_err();
        assert_eq!(queued_error.outcome, Outcome::Unknown);
        assert_eq!(client.pending_count(), 3);
        assert!(database_locked(&root));
        let running_error = timeout(Duration::from_secs(5), credentials).await.unwrap().unwrap().unwrap_err();
        assert_eq!(running_error.code, "deadline_exceeded");
        assert_eq!(running_error.message, "Hive storage response deadline exceeded; reconcile operation identity.");
        assert_eq!(running_error.outcome, Outcome::Unknown);
        stopped(&client).await;
        assert!(process_identity(first_pid).unwrap().is_none_or(|current| current.created_filetime != identity.created_filetime));
        assert!(root.0.join("block-clock").is_file());
        assert!(!root.0.join("release-backup").exists());
        assert!(!database_locked(&root));
        assert_eq!(client.pending_count(), 0);
        assert_eq!(client.queued_bytes(), 0);
        {
            let source = rusqlite::Connection::open_with_flags(root.0.join("hive.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            assert_eq!(source.query_row("SELECT COUNT(*) FROM fixture_payload", [], |row| row.get::<_, i64>(0)).unwrap(), 256);
            assert_eq!(source.query_row("SELECT revoked FROM credentials WHERE principal_id='alpha'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
            assert_eq!(source.query_row("SELECT COUNT(*) FROM credentials WHERE principal_id='beta'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
            let target = rusqlite::Connection::open(&destination).unwrap();
            assert_eq!(target.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
            assert_eq!(target.query_row("SELECT value FROM sentinel", [], |row| row.get::<_, String>(0)).unwrap(), "retained");
            assert_eq!(target.query_row("SELECT COUNT(*) FROM sqlite_schema WHERE name='fixture_payload'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
        }
        fs::remove_file(root.0.join("block-clock")).unwrap();
        client.start().wait().await.unwrap();
        let second_pid = fs::read_to_string(root.0.join("kernel-pid")).unwrap().parse::<u32>().unwrap();
        let replacement = process_identity(second_pid).unwrap().expect("Replacement vanished");
        assert_ne!((first_pid, identity.created_filetime), (second_pid, replacement.created_filetime));
        assert_eq!(request(&client, payload("health")).await.unwrap().get("ready"), Some(&IvyJson::Bool(true)));
        assert!(database_locked(&root));
        client.close().wait().await.unwrap();
        stopped(&client).await;
        assert!(!database_locked(&root));
        save_kernel_process_cases(serde_json::json!({
            "case":"initialized-backup-timeout-retirement-replacement",
            "sourcePagesBeforeStartup":source_pages,"firstPid":first_pid,
            "firstCreatedFiletime":identity.created_filetime.to_string(),
            "replacementPid":second_pid,"replacementCreatedFiletime":replacement.created_filetime.to_string(),
            "backupError":serde_json::from_str::<serde_json::Value>(&stringify(&backup_error.to_wire(), 1 << 20).unwrap()).unwrap(),
            "queuedError":serde_json::from_str::<serde_json::Value>(&stringify(&queued_error.to_wire(), 1 << 20).unwrap()).unwrap(),
            "runningError":serde_json::from_str::<serde_json::Value>(&stringify(&running_error.to_wire(), 1 << 20).unwrap()).unwrap(),
            "nonTerminatingBackupTimeoutRetainsOwnerAndReservation":true,
            "concurrentStartSameIdentity":true,"secondBackupStillRefused":true,
            "reservationsBeforeFatalTimeout":3,"databaseLockedUntilRetirement":true,
            "copyNeverReleased":true,"clockHeldUntilRetirement":true,"sameWorkerIdentityGone":true,
            "sourcePayloadRowsAfterReap":256,"alphaRevokedAfterReap":0,"betaRowsAfterReap":0,
            "destinationSentinelAfterReap":"retained","destinationIntegrityAfterReap":"ok","destinationPayloadTableAfterReap":false,
            "pendingAfterReap":0,"queuedBytesAfterReap":0,"explicitReplacementReady":true,"databaseLockedAfterReplacementClose":false,
            "productionEntryExecuted":false,"originalReplay":false
        }));
    });
}

#[test]
#[cfg(windows)]
fn temporary_entry_parent_death_preserves_orphan_and_isolates_fresh_owner_cleanup() {
    use ivy_host_runtime::platform::windows_process::process_identity;
    let profile_root = Root::new();
    let profile = original_profile(&profile_root);
    let mut cases = vec![];
    Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        for phase in ["before-ready", "blocked-ready"] {
            let root = Root::new();
            let temporary = root.0.join("temporary");
            fs::create_dir(&temporary).unwrap();
            let mut launch = kernel_launch(&root, &profile);
            launch.args = vec!["--kernel-temporary".into()];
            launch.options.insert("filename", ":memory:".into());
            launch.options.insert("uiRoot", IvyJson::Null);
            let blocked = IvyJson::object([
                ("action", "execute".into()),
                ("context", IvyJson::object([
                    ("principalId", "alpha".into()),
                    ("credentialDigest", format!("sha256:{}", "a".repeat(64)).into()),
                    ("transport", "http".into()),
                ])),
                ("request", IvyJson::object([
                    ("jsonrpc", "2.0".into()), ("id", "blocked".into()),
                    ("method", "system.status".into()),
                    ("params", IvyJson::object(std::iter::empty::<(&str, IvyJson)>())),
                ])),
            ]);
            let mut command = tokio::process::Command::new(&launch.executable);
            // No outer Job may mask the real StorageClient parent-death fence.
            command.arg(if phase == "before-ready" { "--kernel-temporary-startup-parent" } else { "--kernel-temporary-parent" })
                .envs(&launch.environment)
                .env("IVY_HIVE_PROCESS_OPTIONS", stringify(&launch.options, 1 << 20).unwrap())
                .env("IVY_HIVE_PROCESS_BLOCKED_REQUEST", stringify(&blocked, 1 << 20).unwrap())
                .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped())
                .kill_on_drop(true).creation_flags(0x08000000);
            if let Some(job) = &launch.job_launcher { command.env("IVY_HIVE_TEST_JOB", job); }
            let mut parent = command.spawn().unwrap();
            let parent_pid = parent.id().unwrap();
            let ready = timeout(Duration::from_secs(10), async {
                while !root.0.join("parent-ready").is_file() { sleep(Duration::from_millis(5)).await; }
            }).await;
            if ready.is_err() {
                let _ = parent.start_kill();
                let output = timeout(Duration::from_secs(5), parent.wait_with_output()).await;
                panic!("Temporary owner did not reach {phase}: {output:?}");
            }
            let state = if phase == "before-ready" {
                assert_eq!(fs::read_to_string(root.0.join("parent-startup-state")).unwrap(), "starting; no Ready or ordinary request; clock gate held");
                serde_json::json!({"lifecycle":"starting","readyObserved":false,"pendingCount":0,"queuedBytes":0})
            } else {
                let state: serde_json::Value = serde_json::from_slice(&fs::read(root.0.join("parent-temporary-state")).unwrap()).unwrap();
                assert_eq!(state["ready"], true);
                assert_eq!(state["requestPending"], true);
                assert_eq!(state["pendingCount"], 1);
                assert!(state["queuedBytes"].as_f64().unwrap() > 0.0);
                state
            };
            assert!(root.0.join("block-clock").is_file());
            assert!(root.0.join("clock-blocked").is_file());
            let roots = fs::read_dir(&temporary).unwrap().map(|entry| entry.unwrap().path()).collect::<Vec<_>>();
            assert_eq!(roots.len(), 1);
            let orphan = &roots[0];
            assert_eq!(orphan.parent(), Some(temporary.as_path()));
            assert!(orphan.is_dir());
            let id = uuid::Uuid::parse_str(orphan.file_name().unwrap().to_str().unwrap().strip_prefix("ivy-ui-").unwrap()).unwrap();
            assert_eq!(id.get_version_num(), 4);
            let marker = orphan.join("retained-owner-data");
            fs::write(&marker, b"retained across owner death").unwrap();
            // No external file handle denies removal: these probe handles close.
            assert!(!root.0.join("hive.sqlite").exists());
            let pid = fs::read_to_string(root.0.join("kernel-pid")).unwrap().parse::<u32>().unwrap();
            let identity = process_identity(pid).unwrap().expect("Temporary owner vanished");
            fs::write(root.0.join("parent-exit"), b"exit with temporary UI root").unwrap();
            let status = timeout(Duration::from_secs(5), parent.wait()).await.unwrap().unwrap();
            assert_eq!(status.code(), Some(17));
            let mut identity_errors = vec![];
            let retired = timeout(Duration::from_secs(5), async {
                loop {
                    match process_identity(pid) {
                        Ok(None) => break "absent",
                        Ok(Some(current)) if current.created_filetime != identity.created_filetime => break "reused",
                        Ok(Some(_)) => {},
                        Err(error) => identity_errors.push(error.raw_os_error()),
                    }
                    sleep(Duration::from_millis(5)).await;
                }
            }).await;
            if retired.is_err() {
                save_kernel_process_cases(serde_json::json!({"casesCompleted":cases,"case":phase,"execution":"blocked","identityFinding":null,"identityProbeErrors":identity_errors,"workerPid":pid,"workerCreatedFiletime":identity.created_filetime.to_string(),"freshLaunchNotRun":true}));
            }
            let identity_final = retired.expect("No definite absent/reused temporary owner identity");
            assert!(root.0.join("block-clock").is_file());
            assert!(root.0.join("clock-blocked").is_file());
            assert_eq!(fs::read(&marker).unwrap(), b"retained across owner death");
            assert_eq!(fs::read_dir(&temporary).unwrap().count(), 1);
            assert!(!root.0.join("hive.sqlite").exists());
            // Prepare a new explicit launch only after original identity death
            // and orphan assertions. Releasing this gate is not retirement proof.
            fs::remove_file(root.0.join("block-clock")).unwrap();
            let replacement = StorageClient::new(launch.clone(), Arc::new(|_| {}));
            replacement.start().wait().await.unwrap();
            assert_eq!(request(&replacement, payload("health")).await.unwrap().get("ready"), Some(&IvyJson::Bool(true)));
            let replacement_pid = fs::read_to_string(root.0.join("kernel-pid")).unwrap().parse::<u32>().unwrap();
            let replacement_identity = process_identity(replacement_pid).unwrap().expect("Fresh temporary owner vanished");
            assert!(replacement_pid != pid || replacement_identity.created_filetime != identity.created_filetime);
            let roots = fs::read_dir(&temporary).unwrap().map(|entry| entry.unwrap().path()).collect::<Vec<_>>();
            assert_eq!(roots.len(), 2);
            let fresh = roots.iter().find(|path| *path != orphan).unwrap();
            let fresh_id = uuid::Uuid::parse_str(fresh.file_name().unwrap().to_str().unwrap().strip_prefix("ivy-ui-").unwrap()).unwrap();
            assert_eq!(fresh_id.get_version_num(), 4);
            assert_ne!(fresh_id, id);
            assert_eq!(fs::read(&marker).unwrap(), b"retained across owner death");
            replacement.close().wait().await.unwrap();
            stopped(&replacement).await;
            assert!(!fresh.exists());
            assert!(orphan.is_dir());
            assert_eq!(fs::read(&marker).unwrap(), b"retained across owner death");
            assert_eq!(fs::read_dir(&temporary).unwrap().count(), 1);
            assert!(!root.0.join("hive.sqlite").exists());
            cases.push(serde_json::json!({
                "case":phase,"parentPid":parent_pid,"parentExitCode":status.code(),"parentState":state,
                "workerPid":pid,"workerCreatedFiletime":identity.created_filetime.to_string(),
                "workerIdentityFinal":identity_final,"identityProbeErrors":identity_errors,"identityFinding":true,
                "identityDeadlineMs":5000,"identityRetryMs":5,"clockGateHeldUntilRetirement":true,
                "orphanUuidVersion":id.get_version_num(),"orphanPresentAfterRetirement":true,
                "orphanMarkerAfterRetirement":"retained across owner death","externalRemovalDenyingHandle":false,
                "replacementPid":replacement_pid,"replacementCreatedFiletime":replacement_identity.created_filetime.to_string(),
                "replacementReady":true,"freshUuidVersion":fresh_id.get_version_num(),"freshRootDistinct":true,
                "freshRootRemovedAfterClose":true,"orphanPresentAfterFreshClose":true,"orphanMarkerUnchangedAfterFreshClose":true,
                "diskDatabaseCreated":false,"memoryDatabaseRecoveryClaimed":false,"startupRollbackClaimed":false,
                "originalOwnerCooperativeClose":false,"outerHostJob":false,"forcedStopUsed":false,
                "productionEntryExecuted":true,"productionTmpdirPolicyQualified":false,"originalReplay":false
            }));
        }
    });
    save_kernel_process_cases(serde_json::json!(cases));
}

#[test]
#[cfg(windows)]
fn initialized_entry_temporary_ui_root_lifetime_tracks_close_eof_fatal_and_locked_cleanup() {
    use ivy_host_runtime::process::{self, Command, LaunchContext};
    use std::os::windows::fs::OpenOptionsExt;
    let profile_root = Root::new();
    let profile = original_profile(&profile_root);
    let mut cases = vec![];
    Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        for name in ["cooperative-close", "ready-eof", "late-fatal", "locked-close"] {
            let root = Root::new();
            let temporary = root.0.join("temporary");
            fs::create_dir(&temporary).unwrap();
            let mut launch = kernel_launch(&root, &profile);
            launch.args = vec!["--kernel-temporary".into()];
            launch.options.insert("filename", ":memory:".into());
            launch.options.insert("uiRoot", IvyJson::Null);
            if name == "late-fatal" { launch.options.insert("publicBaseUrl", "invalid".into()); }
            let mut child = process::spawn(&Command {
                executable: "worker".into(), args: launch.args, cwd: None, timeout_ms: 30000,
            }, &root.0, &BTreeMap::from([("worker".into(), launch.executable)]), LaunchContext {
                environment: launch.environment, job_launcher: launch.job_launcher,
                raw_output: true, ..Default::default()
            }).await.unwrap();
            let mut stdin = Some(child.stdin.take().unwrap());
            let mut frames = FrameReader::new(child.stdout.take().unwrap());
            write_frame(stdin.as_mut().unwrap(), &launch.options).await.unwrap();
            let first = timeout(Duration::from_secs(10), frames.next()).await.unwrap().unwrap().unwrap();
            if name == "late-fatal" {
                assert_eq!(first.required("fatal").unwrap().required("data").unwrap().get("code"), Some(&IvyJson::from("internal_error")));
            } else {
                assert_eq!(first, Message::Ready.to_json());
            }
            let roots = fs::read_dir(&temporary).unwrap().map(|entry| entry.unwrap().path()).collect::<Vec<_>>();
            assert_eq!(roots.len(), 1);
            let ui_root = &roots[0];
            assert_eq!(ui_root.parent(), Some(temporary.as_path()));
            assert!(ui_root.is_dir());
            let leaf = ui_root.file_name().unwrap().to_str().unwrap();
            let id = uuid::Uuid::parse_str(leaf.strip_prefix("ivy-ui-").unwrap()).unwrap();
            assert_eq!(id.get_version_num(), 4);
            assert!(!root.0.join("hive.sqlite").exists());
            let held_path = ui_root.join("held");
            let held = if name == "locked-close" {
                fs::write(&held_path, b"retained by external test owner").unwrap();
                Some(fs::OpenOptions::new().read(true).share_mode(3).open(&held_path).unwrap())
            } else { None };
            let close = matches!(name, "cooperative-close" | "locked-close");
            if close {
                write_frame(stdin.as_mut().unwrap(), &Request { id: 1, payload: payload("close") }.to_json()).await.unwrap();
            } else if name == "ready-eof" {
                // Dropping the actual Windows pipe handle supplies real EOF.
                drop(stdin.take());
            }
            let completion = match timeout(Duration::from_secs(5), child.completion.wait()).await {
                Ok(completion) => completion,
                Err(error) => {
                    drop(held);
                    let cleanup = child.stop(Duration::ZERO).await;
                    panic!("Temporary-root entry did not exit after {name}: {error}; cleanup {cleanup:?}");
                }
            };
            assert_eq!(completion.exit_code, Some(if name == "ready-eof" { 0 } else { 1 }));
            assert!(completion.error_code.is_none());
            let mut trailing = vec![];
            while let Some(frame) = timeout(Duration::from_secs(5), frames.next()).await.unwrap().unwrap() {
                trailing.push(frame);
            }
            assert!(!trailing.iter().any(|frame| matches!(Message::from_json(frame.clone()).unwrap(), Some(Message::Reply { .. }))), "Original close-publication failure must not fabricate a reply");
            if close {
                assert!(trailing.iter().any(|frame| matches!(Message::from_json(frame.clone()).unwrap(), Some(Message::Started(1)))));
            } else {
                assert!(trailing.is_empty());
            }
            let retained = name != "cooperative-close";
            assert_eq!(ui_root.exists(), retained);
            if name == "locked-close" { assert!(held_path.is_file()); }
            // Release the external handle only after the actual exit/root-state
            // assertions; fixture cleanup contributes no successful deletion.
            drop(held);
            cases.push(serde_json::json!({
                "case":name,"exitCode":completion.exit_code,"errorCode":completion.error_code,
                "initialFrame":serde_json::from_str::<serde_json::Value>(&stringify(&first, 1 << 20).unwrap()).unwrap(),
                "temporaryRootUuidVersion":id.get_version_num(),"temporaryRootPresentBeforeExit":true,
                "temporaryRootPresentAfterExit":retained,"diskDatabaseCreated":false,
                "stdinRetainedUntilReap":name != "ready-eof","closeSubmitted":close,
                "closeReplyObserved":false,"lockedHandleHeldThroughReap":name == "locked-close",
                "trailingFrames":trailing.iter().map(|frame| serde_json::from_str::<serde_json::Value>(&stringify(frame, 1 << 20).unwrap()).unwrap()).collect::<Vec<_>>(),
                "productionEntryExecuted":true,"productionTmpdirPolicyQualified":false,"originalReplay":false
            }));
        }
    });
    save_kernel_process_cases(serde_json::json!(cases));
}

#[test]
fn initialized_entry_exits_with_open_stdin_after_ready_eof_or_exact_fatal() {
    use ivy_host_runtime::process::{self, Command, LaunchContext};
    let profile_root = Root::new();
    let profile = original_profile(&profile_root);
    let mut cases = vec![];
    Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        for name in ["ready-eof","credential-fatal","url-fatal"] {
            let root=Root::new();let mut launch=kernel_launch(&root,&profile);
            if name=="credential-fatal" { launch.options.insert("credentials",IvyJson::Array(vec![IvyJson::object([("principalId","alpha".into()),("digest","invalid".into())])])); }
            if name=="url-fatal" { launch.options.insert("publicBaseUrl","invalid".into()); }
            let mut child=process::spawn(&Command{executable:"worker".into(),args:launch.args,cwd:None,timeout_ms:30000},&root.0,&BTreeMap::from([("worker".into(),launch.executable)]),LaunchContext{environment:launch.environment,job_launcher:launch.job_launcher,raw_output:true,..Default::default()}).await.unwrap();
            let mut stdin=child.stdin.take().unwrap();let mut frames=FrameReader::new(child.stdout.take().unwrap());
            write_frame(&mut stdin,&launch.options).await.unwrap();
            let frame=timeout(Duration::from_secs(10),frames.next()).await.unwrap().unwrap().unwrap();
            let ready=name=="ready-eof";
            if ready { assert_eq!(frame,Message::Ready.to_json());stdin.shutdown().await.unwrap();drop(stdin); }
            else { assert!(frame.get("fatal").is_some()); }
            // Fatal must exit even though its parent's stdin stays open. EOF
            // after Ready must also exit without joining Tokio's input thread.
            let result=match timeout(Duration::from_secs(5),child.completion.wait()).await {Ok(result)=>result,Err(error)=>{let _=child.stop(Duration::ZERO).await;panic!("Private entry did not exit: {error}")}};
            assert_eq!(result.exit_code,Some(if ready{0}else{1}));assert!(result.error_code.is_none());
            assert!(frames.next().await.unwrap().is_none());assert!(!database_locked(&root));
            let projected=ivy_contracts::json::stringify(&frame,1<<20).unwrap();
            cases.push(serde_json::json!({"case":name,"frame":serde_json::from_str::<serde_json::Value>(&projected).unwrap(),"exitCode":result.exit_code,"errorCode":result.error_code,"databaseLockedAfterReap":false,"stdinRetainedUntilReap":!ready}));
        }
    });
    save_kernel_process_cases(serde_json::json!(cases));
}

#[test]
fn initialized_kernel_deadline_reaps_blocked_owner_before_explicit_replacement() {
    let root = Root::new();
    let profile = original_profile(&root);
    Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let client=StorageClient::new(kernel_launch(&root,&profile),Arc::new(|_|{}));
        client.start().wait().await.unwrap();let first_pid=fs::read_to_string(root.0.join("kernel-pid")).unwrap();
        assert_eq!(request(&client,payload("health")).await.unwrap().get("ready"),Some(&IvyJson::Bool(true)));
        assert!(database_locked(&root));fs::write(root.0.join("block-clock"),b"hold").unwrap();
        let blocked=client.request(IvyJson::object([
            ("action","execute".into()),
            ("context",IvyJson::object([("principalId","alpha".into()),("credentialDigest",format!("sha256:{}","a".repeat(64)).into()),("transport","http".into())])),
            ("request",IvyJson::object([("jsonrpc","2.0".into()),("id","blocked".into()),("method","system.status".into()),("params",IvyJson::object(std::iter::empty::<(&str,IvyJson)>()))])),
        ]),RequestOptions{timeout_ms:2500,..Default::default()});
        wait_file(root.0.join("clock-blocked")).await;
        let health=client.request(payload("health"),RequestOptions{timeout_ms:100,terminate_on_timeout:false,..Default::default()});
        let health_error=health.await.unwrap().unwrap_err();assert_eq!(health_error.outcome,Outcome::Unknown);assert_eq!(client.pending_count(),2);assert!(database_locked(&root));
        let error=blocked.await.unwrap().unwrap_err();assert_eq!(error.code,"deadline_exceeded");assert_eq!(error.outcome,Outcome::Unknown);
        stopped(&client).await;assert!(!database_locked(&root));assert_eq!(client.pending_count(),0);assert_eq!(client.queued_bytes(),0);
        fs::remove_file(root.0.join("block-clock")).unwrap();client.start().wait().await.unwrap();
        let second_pid=fs::read_to_string(root.0.join("kernel-pid")).unwrap();assert_ne!(first_pid,second_pid);
        assert_eq!(request(&client,payload("health")).await.unwrap().get("ready"),Some(&IvyJson::Bool(true)));assert!(database_locked(&root));
        client.close().wait().await.unwrap();stopped(&client).await;assert!(!database_locked(&root));
        save_kernel_process_cases(serde_json::json!({"case":"blocked-clock-retirement","firstPid":first_pid,"secondPid":second_pid,"runningError":serde_json::from_str::<serde_json::Value>(&stringify(&error.to_wire(),1<<20).unwrap()).unwrap(),"queuedHealthError":serde_json::from_str::<serde_json::Value>(&stringify(&health_error.to_wire(),1<<20).unwrap()).unwrap(),"databaseLockedWhileRunning":true,"databaseLockedAfterReap":false,"replacementReady":true,"databaseLockedAfterClose":false,"pendingAfterReap":0,"queuedBytesAfterReap":0}));
    });
}

#[test]
fn initialized_entry_pipe_loss_exits_and_releases_actual_database_owner() {
    use ivy_host_runtime::process::{self, Command, LaunchContext};
    let profile_root = Root::new();
    let profile = original_profile(&profile_root);
    let mut cases = vec![];
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            for name in ["truncated-input", "broken-output"] {
                let root = Root::new();
                let launch = kernel_launch(&root, &profile);
                let mut child = process::spawn(
                    &Command {
                        executable: "worker".into(),
                        args: launch.args,
                        cwd: None,
                        timeout_ms: 30000,
                    },
                    &root.0,
                    &BTreeMap::from([("worker".into(), launch.executable)]),
                    LaunchContext {
                        environment: launch.environment,
                        job_launcher: launch.job_launcher,
                        raw_output: true,
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
                let mut stdin = child.stdin.take();
                let mut frames = Some(FrameReader::new(child.stdout.take().unwrap()));
                write_frame(stdin.as_mut().unwrap(), &launch.options)
                    .await
                    .unwrap();
                assert_eq!(
                    timeout(Duration::from_secs(10), frames.as_mut().unwrap().next())
                        .await
                        .unwrap()
                        .unwrap()
                        .unwrap(),
                    Message::Ready.to_json()
                );
                assert!(database_locked(&root));
                let pid = fs::read_to_string(root.0.join("kernel-pid")).unwrap();
                if name == "truncated-input" {
                    // A partial length header followed by actual EOF. Windows
                    // ChildStdin.shutdown alone does not close the pipe handle.
                    stdin.as_mut().unwrap().write_all(&[0, 0]).await.unwrap();
                    stdin.as_mut().unwrap().flush().await.unwrap();
                    drop(stdin.take());
                } else {
                    // Close the real parent reader before a request forces the
                    // worker to publish Started. Keep its input open until reap.
                    drop(frames.take());
                    write_frame(
                        stdin.as_mut().unwrap(),
                        &Request { id: 1, payload: payload("health") }.to_json(),
                    )
                    .await
                    .unwrap();
                }
                let result = match timeout(Duration::from_secs(5), child.completion.wait()).await {
                    Ok(result) => result,
                    Err(error) => {
                        let cleanup = child.stop(Duration::ZERO).await;
                        panic!("Private entry did not exit after {name}: {error}; cleanup {cleanup:?}");
                    }
                };
                assert_eq!(result.exit_code, Some(1));
                assert!(result.error_code.is_none());
                assert!(!database_locked(&root));
                if let Some(frames) = frames.as_mut() {
                    assert!(frames.next().await.unwrap().is_none());
                }
                cases.push(serde_json::json!({
                    "case":name,"pid":pid,"exitCode":result.exit_code,
                    "errorCode":result.error_code,"databaseLockedBeforeFault":true,
                    "databaseLockedAfterReap":false,"inputRetainedUntilReap":name=="broken-output",
                    "outputReadableAfterFault":name=="truncated-input",
                    "unexpectedFrameAfterFault":if name=="truncated-input" {Some(false)} else {None},
                }));
            }
        });
    save_kernel_process_cases(serde_json::json!(cases));
}

#[test]
#[cfg(windows)]
fn initialized_owner_parent_death_retires_blocked_kernel_without_cooperative_close() {
    use ivy_host_runtime::platform::windows_process::process_identity;
    let root = Root::new();
    let profile = original_profile(&root);
    let launch = kernel_launch(&root, &profile);
    let blocked = IvyJson::object([
        ("action", "execute".into()),
        (
            "context",
            IvyJson::object([
                ("principalId", "alpha".into()),
                (
                    "credentialDigest",
                    format!("sha256:{}", "a".repeat(64)).into(),
                ),
                ("transport", "http".into()),
            ]),
        ),
        (
            "request",
            IvyJson::object([
                ("jsonrpc", "2.0".into()),
                ("id", "blocked".into()),
                ("method", "system.status".into()),
                (
                    "params",
                    IvyJson::object(std::iter::empty::<(&str, IvyJson)>()),
                ),
            ]),
        ),
    ]);
    Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        // Launch the owner directly, without an outer Host Job that could mask
        // the inner storage owner's own parent-death retirement behavior.
        let mut command = tokio::process::Command::new(&launch.executable);
        command.arg("--kernel-parent").envs(&launch.environment)
            .env("IVY_HIVE_PROCESS_OPTIONS", stringify(&launch.options, 1 << 20).unwrap())
            .env("IVY_HIVE_PROCESS_BLOCKED_REQUEST", stringify(&blocked, 1 << 20).unwrap())
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped())
            .kill_on_drop(true).creation_flags(0x08000000);
        if let Some(job) = launch.job_launcher { command.env("IVY_HIVE_TEST_JOB", job); }
        let mut parent = command.spawn().unwrap();
        struct Unblock(PathBuf);
        impl Drop for Unblock { fn drop(&mut self) { let _ = fs::remove_file(&self.0); } }
        let _unblock = Unblock(root.0.join("block-clock"));
        let ready = timeout(Duration::from_secs(10), async {
            while !root.0.join("parent-ready").is_file() {
                sleep(Duration::from_millis(5)).await;
            }
        }).await;
        if ready.is_err() {
            let _ = fs::remove_file(root.0.join("block-clock"));
            let _ = parent.start_kill();
            let output = timeout(Duration::from_secs(5), parent.wait_with_output()).await;
            panic!("Owner fixture did not become ready: {output:?}");
        }
        let pid = fs::read_to_string(root.0.join("kernel-pid")).unwrap().parse::<u32>().unwrap();
        let identity = process_identity(pid).unwrap().expect("Initialized worker vanished");
        assert!(database_locked(&root));
        fs::write(root.0.join("parent-exit"), b"exit abruptly").unwrap();
        let status = timeout(Duration::from_secs(5), parent.wait()).await.unwrap().unwrap();
        assert_eq!(status.code(), Some(17));
        timeout(Duration::from_secs(5), async {
            loop {
                match process_identity(pid).unwrap() {
                    None => break,
                    Some(current) if current.created_filetime != identity.created_filetime => break,
                    Some(_) => sleep(Duration::from_millis(5)).await,
                }
            }
        }).await.expect("Blocked storage child survived owner death");
        assert!(root.0.join("block-clock").is_file());
        assert!(!database_locked(&root));
        save_kernel_process_cases(serde_json::json!({"case":"abrupt-parent-death","parentExitCode":status.code(),"workerPid":pid,"workerCreatedFiletime":identity.created_filetime.to_string(),"databaseLockedBeforeDeath":true,"databaseLockedAfterRetirement":false,"sameWorkerIdentityGone":true,"blockingGateRetainedUntilRetirement":true,"cooperativeClose":false,"outerHostJob":false}));
    });
}

#[test]
fn private_frames_survive_cancelled_partial_reads_and_preserve_full_u64_utf16_errors() {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let value =
                IvyJson::object([("x", IvyJson::String(JsString(vec![0xd800, 10, 0xdc00])))]);
            let encoded = stringify(&value, MAX_FRAME_BYTES).unwrap().into_bytes();
            let header = (encoded.len() as u32).to_be_bytes();
            let (mut input, output) = tokio::io::duplex(4);
            let mut reader = FrameReader::new(output);
            // Poll exactly to Pending, then drop that read future. Bytes are
            // withheld by the writer rather than guessed from wall-clock sleeps.
            // This exercises cancellation inside both the header and body even
            // when the runtime is delayed past several timer deadlines.
            input.write_all(&header[..2]).await.unwrap();
            {
                let mut reading = Box::pin(reader.next());
                assert!(
                    std::future::poll_fn(|cx| std::task::Poll::Ready(
                        std::future::Future::poll(reading.as_mut(), cx).is_pending()
                    ))
                    .await
                );
            }
            input.write_all(&header[2..]).await.unwrap();
            input.write_all(&encoded[..2]).await.unwrap();
            {
                let mut reading = Box::pin(reader.next());
                assert!(
                    std::future::poll_fn(|cx| std::task::Poll::Ready(
                        std::future::Future::poll(reading.as_mut(), cx).is_pending()
                    ))
                    .await
                );
            }
            let expected = value.clone();
            let writer = tokio::spawn(async move {
                input.write_all(&encoded[2..]).await.unwrap();
                write_frame(
                    &mut input,
                    &Request {
                        id: u64::MAX,
                        payload: expected,
                    }
                    .to_json(),
                )
                .await
                .unwrap();
            });
            assert_eq!(reader.next().await.unwrap().unwrap(), value);
            let received = Request::from_json(reader.next().await.unwrap().unwrap()).unwrap();
            assert_eq!(received.id, u64::MAX);
            assert_eq!(received.payload, value);
            assert!(reader.next().await.unwrap().is_none());
            writer.await.unwrap();
            let error = IvyError::new_js(JsString(vec![0xd800]), JsString(vec![0xdc00]))
                .with_outcome(Outcome::Completed)
                .with_details(value);
            match Message::from_json(
                Message::Reply {
                    id: u64::MAX,
                    result: Err(error.clone()),
                }
                .to_json(),
            )
            .unwrap()
            .unwrap()
            {
                Message::Reply {
                    id,
                    result: Err(received),
                } => {
                    assert_eq!(id, u64::MAX);
                    assert_eq!(received.to_wire(), error.to_wire());
                }
                _ => panic!("Private error lost"),
            }
        });
}

#[test]
fn private_frames_reject_invalid_lengths_utf8_json_and_truncation_without_allocating_declared_garbage()
 {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            for (bytes, kind) in [
                (vec![0, 0, 0, 0], std::io::ErrorKind::InvalidData),
                (
                    u32::MAX.to_be_bytes().to_vec(),
                    std::io::ErrorKind::InvalidData,
                ),
                (vec![0, 0], std::io::ErrorKind::UnexpectedEof),
                (vec![0, 0, 0, 2, b'{'], std::io::ErrorKind::UnexpectedEof),
                (vec![0, 0, 0, 1, 255], std::io::ErrorKind::InvalidData),
                (vec![0, 0, 0, 1, b'x'], std::io::ErrorKind::InvalidData),
            ] {
                let mut reader = FrameReader::new(bytes.as_slice());
                assert_eq!(reader.next().await.unwrap_err().kind(), kind);
            }
        });
}

#[test]
fn completed_backup_reports_its_last_progress_before_reply_even_after_sender_closes() {
    use ivy_hive::worker_server::{self, BackupTask, Progress, WorkerService};
    struct Service(bool);
    impl WorkerService for Service {
        fn handle(&mut self, _: IvyJson) -> Result<IvyJson, IvyError> {
            Ok(true.into())
        }
        fn backup(&mut self, _: IvyJson, progress: Progress) -> Result<BackupTask, IvyError> {
            let delayed = self.0;
            Ok(Box::pin(async move {
                if delayed {
                    sleep(Duration::from_millis(5)).await;
                }
                progress();
                Ok(true.into())
            }))
        }
        fn notifications(&mut self) -> Vec<ivy_hive::worker_protocol::Notification> {
            vec![]
        }
    }
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            // The first case completes in the request loop, the second inside Close.
            for delayed in [false, true] {
                let (parent, child) = tokio::io::duplex(1024);
                let (input, mut output) = tokio::io::split(parent);
                let (reader, writer) = tokio::io::split(child);
                let server = worker_server::serve(reader, writer, |_| Ok(Service(delayed)));
                let caller = async move {
                    write_frame(&mut output, &IvyJson::Null).await.unwrap();
                    for (id, action) in [(1, "backup"), (2, "close")] {
                        write_frame(
                            &mut output,
                            &Request {
                                id,
                                payload: payload(action),
                            }
                            .to_json(),
                        )
                        .await
                        .unwrap();
                    }
                    let mut input = FrameReader::new(input);
                    let mut received = vec![];
                    while let Some(value) = input.next().await.unwrap() {
                        received.push(value);
                    }
                    let ready = Message::Ready.to_json();
                    let started = |id| Message::Started(id).to_json();
                    let progress = Message::Progress(1).to_json();
                    let reply = |id| {
                        Message::Reply {
                            id,
                            result: Ok(true.into()),
                        }
                        .to_json()
                    };
                    let expected = if delayed {
                        vec![ready, started(1), started(2), progress, reply(1), reply(2)]
                    } else {
                        vec![ready, started(1), progress, reply(1), started(2), reply(2)]
                    };
                    assert_eq!(received, expected);
                };
                let (served, ()) = tokio::join!(server, caller);
                served.unwrap();
            }
        });
}

#[test]
fn real_storage_child_shared_start_notifications_errors_and_close_reopen() {
    let root = Root::new();
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (client, events) = client(&root);
            // Closing an absent owner must not leave a delayed shutdown capable
            // of retiring the next explicit start.
            client.close().wait().await.unwrap();
            drop(client.start());
            let shared = client.start();
            timeout(Duration::from_secs(10), shared.wait())
                .await
                .unwrap()
                .unwrap();
            let value = IvyJson::String(JsString(vec![0xd800, 65, 0xdc00]));
            let result = ivy_hive::browser_push::Worker::request(
                &client,
                IvyJson::object([("action", "echo".into()), ("value", value.clone())]),
            )
            .await
            .unwrap();
            assert_eq!(result, value);
            assert_eq!(events.lock().unwrap().len(), 3);
            let error = ivy_hive::browser_push::Worker::request(
                &client,
                IvyJson::object([("action", "error".into()), ("value", value.clone())]),
            )
            .await
            .unwrap_err();
            assert_eq!(error.outcome, Outcome::Completed);
            assert_eq!(error.message_js(), JsString(vec![0xd800, 120, 0xdc00]));
            assert_eq!(error.details, Some(value));
            let a = client.close();
            let b = client.close();
            a.wait().await.unwrap();
            b.wait().await.unwrap();
            assert_eq!(client.lifecycle(), Lifecycle::Stopped);
            assert_eq!(client.pending_count(), 0);
            client.start().wait().await.unwrap();
            assert_eq!(
                request(&client, payload("health"))
                    .await
                    .unwrap()
                    .get("ready"),
                Some(&true.into())
            );
            client.close().wait().await.unwrap();
            assert!(
                !events
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|e| matches!(e, Event::Failure(_)))
            );
        });
}

#[test]
fn real_storage_child_queue_timeout_and_dropped_receiver_keep_posted_work_until_reply() {
    let root = Root::new();
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (client, events) = client(&root);
            client.start().wait().await.unwrap();
            let active = client.request(payload("hold"), RequestOptions::default());
            wait_file(root.0.join("blocked")).await;
            let queued = client.request(
                payload("write"),
                RequestOptions {
                    timeout_ms: 30,
                    ..Default::default()
                },
            );
            let dropped = client.request(payload("write"), RequestOptions::default());
            drop(dropped);
            let error = queued.await.unwrap().unwrap_err();
            assert_eq!(error.outcome, Outcome::Unknown);
            assert_eq!(
                error.message,
                "Hive storage queue wait expired; reconcile operation identity."
            );
            assert_eq!(client.pending_count(), 3);
            assert_eq!(client.queued_bytes(), 12288);
            assert!(matches!(client.lifecycle(), Lifecycle::Ready(_)));
            fs::write(root.0.join("release"), b"go").unwrap();
            active.await.unwrap().unwrap();
            assert_eq!(
                request(&client, payload("calls")).await.unwrap(),
                IvyJson::from("2")
            );
            assert_eq!(client.pending_count(), 0);
            assert_eq!(client.queued_bytes(), 0);
            client.close().wait().await.unwrap();
            assert!(
                !events
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|e| matches!(e, Event::Failure(_)))
            );
        });
}

#[test]
fn real_nonterminating_started_read_timeout_retains_its_slot_and_accepts_late_reply() {
    let root = Root::new();
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (client, events) = client(&root);
            client.start().wait().await.unwrap();
            let held = client.request(
                payload("hold"),
                RequestOptions {
                    timeout_ms: 100,
                    terminate_on_timeout: false,
                    ..Default::default()
                },
            );
            wait_file(root.0.join("blocked")).await;
            let error = held.await.unwrap().unwrap_err();
            assert_eq!(error.outcome, Outcome::Unknown);
            assert_eq!(
                error.message,
                "Hive storage response deadline exceeded; reconcile operation identity."
            );
            assert!(matches!(client.lifecycle(), Lifecycle::Ready(_)));
            assert_eq!(client.pending_count(), 1);
            assert_eq!(client.queued_bytes(), 4096);
            fs::write(root.0.join("release"), b"go").unwrap();
            request(&client, payload("health")).await.unwrap();
            assert_eq!(client.pending_count(), 0);
            assert_eq!(client.queued_bytes(), 0);
            client.close().wait().await.unwrap();
            assert!(
                events
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|e| !matches!(e, Event::Failure(_)))
            );
        });
}

#[test]
fn close_during_real_child_initialization_settles_start_only_after_fencing_the_owner() {
    let root = Root::new();
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut launch = launch(&root);
            launch.options.insert("neverReady", true.into());
            let client = StorageClient::new(launch, Arc::new(|_| {}));
            let starting = client.start();
            wait_file(root.0.join("pid")).await;
            let closing = client.close();
            let error = starting.wait().await.unwrap_err();
            assert_eq!(error.code, "service_unavailable");
            assert_eq!(error.outcome, Outcome::Unknown);
            timeout(Duration::from_secs(5), closing.wait())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(client.lifecycle(), Lifecycle::Stopped);
            // Opening the same path proves the actual retained SQLite owner is gone.
            ivy_hive::store::Store::open(&root.0.join("hive.sqlite")).unwrap();
        });
}

#[test]
fn real_started_timeout_reaps_blocked_sqlite_writer_before_explicit_restart_without_replay() {
    let root = Root::new();
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (client, events) = client(&root);
            client.start().wait().await.unwrap();
            let held = client.request(
                payload("hold"),
                RequestOptions {
                    timeout_ms: 150,
                    ..Default::default()
                },
            );
            wait_file(root.0.join("blocked")).await;
            let error = held.await.unwrap().unwrap_err();
            assert_eq!(error.outcome, Outcome::Unknown);
            assert_eq!(
                error.message,
                "Hive storage response deadline exceeded; reconcile operation identity."
            );
            let attempted = client.start();
            // Whether native retirement won the race or is still in progress, the
            // next ready owner can open the exclusive DB only after definite death.
            let result = attempted.wait().await;
            if result.is_err() {
                stopped(&client).await;
                client.start().wait().await.unwrap();
            }
            assert_eq!(
                request(&client, payload("calls")).await.unwrap(),
                IvyJson::from("0")
            );
            assert!(
                events
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|e| matches!(e, Event::Failure(_)))
            );
            client.close().wait().await.unwrap();
            let db = rusqlite::Connection::open(root.0.join("hive.sqlite")).unwrap();
            assert_eq!(
                db.query_row("SELECT value FROM metadata WHERE key='posted'", [], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap(),
                "yes"
            );
        });
}

#[test]
fn real_storage_child_bounded_backpressure_frees_all_reservations_only_after_retirement() {
    let root = Root::new();
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (client, _) = client(&root);
            client.start().wait().await.unwrap();
            let mut receivers = vec![client.request(
                payload("hold"),
                RequestOptions {
                    timeout_ms: 60_000,
                    ..Default::default()
                },
            )];
            wait_file(root.0.join("blocked")).await;
            for _ in 1..MAX_PENDING {
                receivers.push(client.request(
                    IvyJson::object([
                        ("action", "echo".into()),
                        ("value", "x".repeat(8192).into()),
                    ]),
                    RequestOptions {
                        timeout_ms: 60_000,
                        ..Default::default()
                    },
                ));
            }
            let refused = client
                .request(payload("health"), RequestOptions::default())
                .await
                .unwrap()
                .unwrap_err();
            assert_eq!(refused.code, "limit_exceeded");
            assert_eq!(refused.outcome, Outcome::NotExecuted);
            assert_eq!(client.pending_count(), MAX_PENDING);
            assert_eq!(client.queued_bytes(), MAX_PENDING as u64 * 4096);
            let close = client.close();
            for receiver in receivers {
                assert_eq!(
                    receiver.await.unwrap().unwrap_err().outcome,
                    Outcome::Unknown
                );
            }
            close.wait().await.unwrap();
            assert_eq!(client.pending_count(), 0);
            assert_eq!(client.queued_bytes(), 0);
        });
}

#[test]
fn real_storage_child_startup_deadline_and_fatal_preserve_failure_and_owning_retirement() {
    let root = Root::new();
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut launch = launch(&root);
            launch.options.insert("neverReady", true.into());
            let client = StorageClient::new(launch, Arc::new(|_| {}));
            let starting = client.start();
            // Options can reach the child only after the Host launch handshake and
            // pipe binding. Pause only the policy deadline; resume for real OS reap.
            wait_file(root.0.join("pid")).await;
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(30)).await;
            for _ in 0..32 {
                tokio::task::yield_now().await;
            }
            tokio::time::resume();
            let error = starting.wait().await.unwrap_err();
            assert_eq!(error.code, "deadline_exceeded");
            assert_eq!(error.outcome, Outcome::NotExecuted);
            stopped(&client).await;
            assert_eq!(client.pending_count(), 0);
            client.close().wait().await.unwrap();
        });
    let root = Root::new();
    let db = rusqlite::Connection::open(root.0.join("hive.sqlite")).unwrap();
    db.execute_batch("PRAGMA user_version=9").unwrap();
    drop(db);
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (client, _) = client(&root);
            let error = client.start().wait().await.unwrap_err();
            assert_eq!(error.code, "unsupported_storage_format");
            assert_eq!(error.outcome, Outcome::NotExecuted);
            stopped(&client).await;
            assert_eq!(
                rusqlite::Connection::open(root.0.join("hive.sqlite"))
                    .unwrap()
                    .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                9
            );
        });
}

#[test]
fn real_storage_child_malformed_frame_and_unexpected_exit_never_prove_not_executed() {
    for action in ["malformed", "exit"] {
        let root = Root::new();
        Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let (client, _) = client(&root);
                client.start().wait().await.unwrap();
                let error = request(&client, payload(action)).await.unwrap_err();
                assert_eq!(error.outcome, Outcome::Unknown);
                assert_eq!(error.code, "storage_unavailable");
                stopped(&client).await;
                client.start().wait().await.unwrap();
                request(&client, payload("health")).await.unwrap();
                client.close().wait().await.unwrap();
            });
    }
}

#[test]
fn concurrent_failure_close_and_explicit_restart_keep_settlements_in_their_generation() {
    let root = Root::new();
    Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (client, _) = client(&root);
            for _ in 0..8 {
                client.start().wait().await.unwrap();
                let _ = fs::remove_file(root.0.join("blocked"));
                let held = client.request(
                    payload("hold"),
                    RequestOptions {
                        timeout_ms: 150,
                        ..Default::default()
                    },
                );
                wait_file(root.0.join("blocked")).await;
                let queued = (0..8)
                    .map(|_| client.request(payload("write"), RequestOptions::default()))
                    .collect::<Vec<_>>();
                let closing = client.close();
                let replacement = client.clone();
                let restarting = tokio::spawn(async move {
                    stopped(&replacement).await;
                    timeout(Duration::from_secs(10), replacement.start().wait())
                        .await
                        .unwrap()
                        .unwrap();
                });
                assert_eq!(
                    held.await
                        .expect("Lost running settlement")
                        .unwrap_err()
                        .outcome,
                    Outcome::Unknown
                );
                for receiver in queued {
                    assert_eq!(
                        receiver
                            .await
                            .expect("Lost queued settlement")
                            .unwrap_err()
                            .outcome,
                        Outcome::Unknown
                    );
                }
                timeout(Duration::from_secs(5), closing.wait())
                    .await
                    .unwrap()
                    .unwrap();
                restarting.await.unwrap();
                assert_eq!(
                    request(&client, payload("calls")).await.unwrap(),
                    IvyJson::from("0")
                );
                client.close().wait().await.unwrap();
                assert_eq!(client.pending_count(), 0);
                assert_eq!(client.queued_bytes(), 0);
            }
        });
}

#[test]
fn native_launch_errors_preserve_worker_failure_and_require_explicit_absence_proof() {
    let root = Root::new();
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut launch = launch(&root);
            // A directory is an explicit Host NotExecuted rejection. The adapter
            // preserves the original worker error while accepting absence proof.
            launch.executable = root.0.clone();
            let client = StorageClient::new(launch.clone(), Arc::new(|_| {}));
            for _ in 0..2 {
                let error = client.start().wait().await.unwrap_err();
                assert_eq!(error.code, "storage_unavailable");
                assert_eq!(error.message, "Hive storage worker failed.");
                assert_eq!(error.outcome, Outcome::Unknown);
                assert_eq!(client.lifecycle(), Lifecycle::Stopped);
                assert!(!root.0.join("pid").exists());
                assert!(!root.0.join("hive.sqlite").exists());
            }
            client.close().wait().await.unwrap();
            // Pre-spawn filesystem errors currently carry Unknown in Host. Even
            // this known test path's absence is not a retirement proof to use.
            launch.executable = root.0.join("absent-worker.exe");
            let client = StorageClient::new(launch, Arc::new(|_| {}));
            let error = client.start().wait().await.unwrap_err();
            assert_eq!(error.code, "storage_unavailable");
            assert_eq!(error.message, "Hive storage worker failed.");
            assert_eq!(error.outcome, Outcome::Unknown);
            assert!(matches!(client.lifecycle(), Lifecycle::StopUnconfirmed(_)));
            let retry = client.start().wait().await.unwrap_err();
            assert_eq!(retry.message, "Hive storage is not ready.");
            assert_eq!(retry.outcome, Outcome::NotExecuted);
            let close = client.close().wait().await.unwrap_err();
            assert_eq!(close.message, "Hive storage stop is unconfirmed.");
            assert_eq!(close.outcome, Outcome::Unknown);
            assert_eq!(client.pending_count(), 0);
            assert!(!root.0.join("pid").exists());
            assert!(!root.0.join("hive.sqlite").exists());
        });
}
