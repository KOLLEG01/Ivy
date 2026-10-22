//! Synthetic worker actions exercise the real framed entry/SQLite/OS boundary.
//! This executable is never a deployed Hive backend.
use ivy_contracts::{IvyError, IvyJson, IvyResult, json::stringify};
use ivy_hive::{
    store::Store,
    worker_protocol::Notification,
    worker_server::{self, BackupTask, Progress, WorkerService},
};
use std::{fs, future::poll_fn, io::Write, path::PathBuf, task::Poll, time::Duration};
#[path = "hive-observed-runtime.rs"]
mod observed_runtime;

fn kernel_startup_ports() -> ivy_hive::kernel::StartupPorts {
    use ivy_hive::kernel::StartupPorts;
    use std::{rc::Rc, sync::Arc};
    let profile = ivy_contracts::json::parse(
        &fs::read_to_string(
            std::env::var_os("IVY_HIVE_PROCESS_PROFILE").expect("Explicit fixture profile"),
        )
        .unwrap(),
        16 * 1024 * 1024,
    )
    .unwrap();
    let profile = profile
        .required("original")
        .unwrap()
        .required("registryRuntime")
        .unwrap();
    let root = PathBuf::from(std::env::var_os("IVY_HIVE_PROCESS_ROOT").unwrap());
    fs::write(root.join("kernel-pid"), std::process::id().to_string()).unwrap();
    StartupPorts {
        runtime: Rc::new(observed_runtime::ObservedRuntime::from_json(profile)),
        query: Arc::new(observed_runtime::ObservedRuntime::from_json(profile)),
        clock: Rc::new(move || {
            // Deliberately stall an injected host callback in the actual
            // initialized owner; no synthetic storage action is added.
            if root.join("block-clock").is_file() {
                fs::write(root.join("clock-blocked"), b"entered").unwrap();
                while root.join("block-clock").is_file() {
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            100000.0
        }),
        time_zones: Rc::new(|_: &ivy_contracts::JsString| {
            panic!("No Intl admission in process controls")
        }),
        temporary_directory: Box::new(|| panic!("Explicit disk-root controls")),
    }
}

fn initialized_kernel_entry() -> ! {
    ivy_hive::worker_entry::run(
        kernel_startup_ports(),
        std::rc::Rc::new(ivy_hive::browser_push_native::NativeCrypto),
    )
}

fn initialized_kernel_temporary_entry() -> ! {
    let mut ports = kernel_startup_ports();
    let directory =
        PathBuf::from(std::env::var_os("IVY_HIVE_PROCESS_ROOT").unwrap()).join("temporary");
    // This explicit test port supplies no production os.tmpdir policy.
    ports.temporary_directory =
        Box::new(move || Ok(ivy_host_runtime::node_path::from_native(&directory)));
    ivy_hive::worker_entry::run(
        ports,
        std::rc::Rc::new(ivy_hive::browser_push_native::NativeCrypto),
    )
}

struct GatedKernel {
    inner: ivy_hive::worker_kernel::KernelWorker,
    root: PathBuf,
    hashing: bool,
    natural_progress: bool,
}
impl WorkerService for GatedKernel {
    fn after_ready(&mut self) -> IvyResult<()> {
        self.inner.after_ready()
    }
    fn handle(&mut self, payload: IvyJson) -> IvyResult<IvyJson> {
        self.inner.handle(payload)
    }
    fn notifications(&mut self) -> Vec<Notification> {
        self.inner.notifications()
    }
    fn dispatch(&mut self, payload: IvyJson) -> worker_server::Dispatch {
        // Preserve the real Kernel's publication failure on close.
        self.inner.dispatch(payload)
    }
    fn backup(&mut self, payload: IvyJson, progress: Progress) -> IvyResult<BackupTask> {
        if self.natural_progress {
            use std::sync::{
                Arc, Mutex,
                atomic::{AtomicUsize, Ordering},
            };
            fn utc_ms() -> u128 {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis()
            }
            let stepped = self.root.join("step-natural-progress").is_file();
            let times = Arc::new(Mutex::new(Vec::new()));
            let callback_times = times.clone();
            let destination = PathBuf::from(payload.required("destination")?.as_str()?);
            let callbacks = Arc::new(AtomicUsize::new(0));
            let observed = callbacks.clone();
            let callback_root = self.root.clone();
            let mut task = self.inner.backup(
                payload,
                Arc::new(move || {
                    let before = stepped.then(utc_ms);
                    progress();
                    if let Some(before) = before {
                        callback_times.lock().unwrap().push((before, utc_ms()));
                    }
                    if observed.fetch_add(1, Ordering::SeqCst) != 0 {
                        return;
                    }
                    let target = rusqlite::Connection::open_with_flags(
                        &destination,
                        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                    )
                    .unwrap();
                    target.busy_timeout(Duration::ZERO).unwrap();
                    let phase =
                        match target.query_row("SELECT COUNT(*) FROM sqlite_schema", [], |row| {
                            row.get::<_, i64>(0)
                        }) {
                            Ok(_) => "hash",
                            Err(rusqlite::Error::SqliteFailure(error, _))
                                if matches!(
                                    error.code,
                                    rusqlite::ErrorCode::DatabaseBusy
                                        | rusqlite::ErrorCode::DatabaseLocked
                                ) =>
                            {
                                "copy"
                            }
                            Err(error) => {
                                panic!("Unexpected natural-progress phase probe: {error}")
                            }
                        };
                    drop(target);
                    fs::write(callback_root.join("first-natural-progress"), phase).unwrap();
                }),
            )?;
            let root = self.root.clone();
            return Ok(Box::pin(async move {
                fs::write(
                    root.join("backup-awaiting-copy"),
                    b"Started before actual task",
                )
                .unwrap();
                while !root.join("begin-copy").is_file() {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                // Drive only real callbacks and stop at their yielding polls.
                // Stepped mode observes the real throttle without clock hooks.
                let steps = if stepped { 3 } else { 1 };
                for step in 1..=steps {
                    let observed = poll_fn(|cx| {
                        let result = task.as_mut().poll(cx);
                        if callbacks.load(Ordering::SeqCst) >= step || result.is_ready() {
                            Poll::Ready(result)
                        } else {
                            Poll::Pending
                        }
                    })
                    .await;
                    if let Poll::Ready(result) = observed {
                        return result;
                    }
                    assert_eq!(callbacks.load(Ordering::SeqCst), step);
                    let release = if stepped {
                        let (before, after) = times.lock().unwrap()[step - 1];
                        fs::write(
                            root.join(format!("natural-step-{step}")),
                            stringify(
                                &IvyJson::object([
                                    ("beforeMs", IvyJson::Number(before as f64)),
                                    ("afterMs", IvyJson::Number(after as f64)),
                                ]),
                                1 << 20,
                            )
                            .unwrap(),
                        )
                        .unwrap();
                        format!("advance-natural-{step}")
                    } else {
                        fs::write(
                            root.join("natural-progress-paused"),
                            b"one actual callback yielded",
                        )
                        .unwrap();
                        "continue-backup".into()
                    };
                    while !root.join(release.as_str()).is_file() {
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }
                }
                let result = task.await;
                fs::write(
                    root.join("natural-progress-callback-count"),
                    callbacks.load(Ordering::SeqCst).to_string(),
                )
                .unwrap();
                result
            }));
        }
        if self.hashing {
            let destination = PathBuf::from(payload.required("destination")?.as_str()?);
            let root = self.root.clone();
            return self.inner.backup(
                payload,
                std::sync::Arc::new(move || {
                    progress();
                    let target = rusqlite::Connection::open_with_flags(
                        &destination,
                        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                    )
                    .unwrap();
                    target.busy_timeout(Duration::ZERO).unwrap();
                    let readable =
                        match target.query_row("SELECT COUNT(*) FROM sqlite_schema", [], |row| {
                            row.get::<_, i64>(0)
                        }) {
                            Ok(_) => true,
                            Err(rusqlite::Error::SqliteFailure(error, _))
                                if matches!(
                                    error.code,
                                    rusqlite::ErrorCode::DatabaseBusy
                                        | rusqlite::ErrorCode::DatabaseLocked
                                ) =>
                            {
                                false
                            }
                            Err(error) => panic!("Unexpected backup phase probe: {error}"),
                        };
                    drop(target);
                    if !readable {
                        fs::write(
                            root.join("copy-progress-before-hash"),
                            b"destination locked",
                        )
                        .unwrap();
                        return;
                    }
                    // Store::backup_to finalizes the SQLite target before its first
                    // hash-read progress callback. Observe that lock transition,
                    // then block the actual callback with the hash file still open.
                    assert!(root.join("copy-progress-before-hash").is_file());
                    fs::write(root.join("hash-paused"), b"destination readable").unwrap();
                    while !root.join("release-hash").is_file() {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                }),
            );
        }
        let mut task = self.inner.backup(payload, progress)?;
        let root = self.root.clone();
        Ok(Box::pin(async move {
            // Pause only after polling the real copy's first yielding batch.
            // The test seeds more than one batch and keeps ordinary dispatch
            // on the same actual initialized connection while this is pending.
            if let Poll::Ready(result) = poll_fn(|cx| Poll::Ready(task.as_mut().poll(cx))).await {
                return result;
            }
            fs::write(root.join("backup-paused"), b"first batch yielded")
                .map_err(|_| IvyError::internal())?;
            while !root.join("release-backup").is_file() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            task.await
        }))
    }
}

fn initialized_kernel_gated_backup(hashing: bool, natural_progress: bool) -> ! {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = runtime.block_on(worker_server::serve_retained(
        tokio::io::stdin(),
        tokio::io::stdout(),
        |data| {
            ivy_hive::worker_kernel::KernelWorker::open_worker_data(
                data,
                kernel_startup_ports(),
                std::rc::Rc::new(ivy_hive::browser_push_native::NativeCrypto),
            )
            .map(|inner| GatedKernel {
                inner,
                root: PathBuf::from(std::env::var_os("IVY_HIVE_PROCESS_ROOT").unwrap()),
                hashing,
                natural_progress,
            })
        },
    ));
    std::process::exit(if result.is_ok() { 0 } else { 1 });
}

async fn abrupt_parent_exit(root: &std::path::Path) -> ! {
    fs::write(root.join("parent-ready"), b"blocked child owned").unwrap();
    while !root.join("parent-exit").is_file() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // Abrupt OS exit deliberately skips StorageClient close and Rust Drop.
    std::process::exit(17);
}

#[derive(Clone, Copy)]
enum ParentMode {
    Initializing,
    Ready,
    Copy,
    Hash,
}

fn initialized_kernel_parent(mode: ParentMode, temporary: bool) -> ! {
    use ivy_hive::worker_runtime::{StorageClient, WorkerLaunch};
    use std::{collections::BTreeMap, sync::Arc};
    let root = PathBuf::from(std::env::var_os("IVY_HIVE_PROCESS_ROOT").unwrap());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let client = StorageClient::new(
            WorkerLaunch {
                root: root.clone(),
                executable: std::env::current_exe().unwrap(),
                args: vec![
                    if temporary {
                        "--kernel-temporary"
                    } else {
                        match mode {
                            ParentMode::Copy => "--kernel-gated-backup",
                            ParentMode::Hash => "--kernel-gated-hash",
                            ParentMode::Initializing | ParentMode::Ready => "--kernel",
                        }
                    }
                    .into(),
                ],
                options: ivy_contracts::json::parse(
                    &std::env::var("IVY_HIVE_PROCESS_OPTIONS").unwrap(),
                    1 << 20,
                )
                .unwrap(),
                job_launcher: std::env::var_os("IVY_HIVE_TEST_JOB").map(PathBuf::from),
                environment: BTreeMap::from([
                    ("IVY_HIVE_PROCESS_ROOT".into(), root.as_os_str().into()),
                    (
                        "IVY_HIVE_PROCESS_PROFILE".into(),
                        std::env::var_os("IVY_HIVE_PROCESS_PROFILE").unwrap(),
                    ),
                ]),
            },
            Arc::new(|_| {}),
        );
        if matches!(mode, ParentMode::Initializing) {
            fs::write(root.join("block-clock"), b"hold initial constructor").unwrap();
        }
        let starting = client.start();
        if matches!(mode, ParentMode::Initializing) {
            while !root.join("clock-blocked").is_file() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert!(matches!(
                client.lifecycle(),
                ivy_hive::worker_client::Lifecycle::Starting(_)
            ));
            assert_eq!(client.pending_count(), 0);
            assert_eq!(client.queued_bytes(), 0);
            fs::write(
                root.join("parent-startup-state"),
                b"starting; no Ready or ordinary request; clock gate held",
            )
            .unwrap();
            abrupt_parent_exit(&root).await;
        }
        starting.wait().await.unwrap();
        if matches!(mode, ParentMode::Ready) {
            fs::write(root.join("block-clock"), b"hold").unwrap();
        }
        let mut blocked = client.request(
            ivy_contracts::json::parse(
                &std::env::var("IVY_HIVE_PROCESS_BLOCKED_REQUEST").unwrap(),
                1 << 20,
            )
            .unwrap(),
            ivy_hive::worker_client::RequestOptions {
                timeout_ms: 60000,
                ..Default::default()
            },
        );
        let gate = match mode {
            ParentMode::Copy => "backup-paused",
            ParentMode::Hash => "hash-paused",
            ParentMode::Ready => "clock-blocked",
            ParentMode::Initializing => unreachable!(),
        };
        while !root.join(gate).is_file() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        if temporary && matches!(mode, ParentMode::Ready) {
            assert!(matches!(
                blocked.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ));
            assert!(matches!(
                client.lifecycle(),
                ivy_hive::worker_client::Lifecycle::Ready(_)
            ));
            assert_eq!(client.pending_count(), 1);
            assert!(client.queued_bytes() > 0);
            fs::write(
                root.join("parent-temporary-state"),
                stringify(
                    &IvyJson::object([
                        ("ready", true.into()),
                        ("requestPending", true.into()),
                        (
                            "pendingCount",
                            IvyJson::Number(client.pending_count() as f64),
                        ),
                        ("queuedBytes", IvyJson::Number(client.queued_bytes() as f64)),
                    ]),
                    1 << 20,
                )
                .unwrap(),
            )
            .unwrap();
        }
        if matches!(mode, ParentMode::Hash) {
            assert!(matches!(
                blocked.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ));
            assert!(matches!(
                client.lifecycle(),
                ivy_hive::worker_client::Lifecycle::Ready(_)
            ));
            assert_eq!(client.pending_count(), 1);
            assert!(client.queued_bytes() > 0);
            fs::write(
                root.join("parent-hash-state"),
                stringify(
                    &IvyJson::object([
                        ("ready", true.into()),
                        ("backupPending", true.into()),
                        (
                            "pendingCount",
                            IvyJson::Number(client.pending_count() as f64),
                        ),
                        ("queuedBytes", IvyJson::Number(client.queued_bytes() as f64)),
                    ]),
                    1 << 20,
                )
                .unwrap(),
            )
            .unwrap();
        }
        if matches!(mode, ParentMode::Copy) {
            let health = client
                .request(
                    IvyJson::object([("action", "health".into())]),
                    Default::default(),
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(health.get("ready"), Some(&IvyJson::Bool(true)));
            assert!(matches!(
                blocked.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ));
            // Keep the actual copy transaction open while a synchronous host
            // callback blocks the request loop. Parent EOF cannot retire this
            // owner; the existing Host parent-death fence must stop it.
            fs::write(root.join("block-clock"), b"hold while backup is active").unwrap();
            let mut credentials = client.request(
                IvyJson::object([
                    ("action", "credentials.configure".into()),
                    (
                        "credentials",
                        IvyJson::Array(vec![IvyJson::object([
                            ("principalId", "beta".into()),
                            ("digest", format!("sha256:{}", "b".repeat(64)).into()),
                        ])]),
                    ),
                ]),
                ivy_hive::worker_client::RequestOptions {
                    timeout_ms: 60000,
                    ..Default::default()
                },
            );
            while !root.join("clock-blocked").is_file() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert!(matches!(
                credentials.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ));
            assert!(matches!(
                blocked.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ));
            assert_eq!(client.pending_count(), 2);
            assert!(client.queued_bytes() > 0);
            fs::write(
                root.join("parent-backup-state"),
                stringify(
                    &IvyJson::object([
                        ("healthReady", true.into()),
                        ("backupPending", true.into()),
                        ("credentialsPending", true.into()),
                        ("clockGateHeld", true.into()),
                        (
                            "pendingCount",
                            IvyJson::Number(client.pending_count() as f64),
                        ),
                        ("queuedBytes", IvyJson::Number(client.queued_bytes() as f64)),
                    ]),
                    1 << 20,
                )
                .unwrap(),
            )
            .unwrap();
        }
        abrupt_parent_exit(&root).await;
    })
}

struct Service {
    store: Store,
    root: PathBuf,
    notifications: Vec<Notification>,
}
impl WorkerService for Service {
    fn handle(&mut self, payload: IvyJson) -> IvyResult<IvyJson> {
        let action = payload.required("action")?.as_str()?;
        match action.as_str() {
            "health" => Ok(IvyJson::object([("ready", true.into())])),
            "echo" => {
                self.notifications.extend([
                    Notification::Routing(IvyJson::object([("node", "fixture".into())])),
                    Notification::EventsAvailable(IvyJson::object([(
                        "throughSequence",
                        IvyJson::Number(7.0),
                    )])),
                    Notification::Changes(vec!["objects".into()]),
                ]);
                Ok(payload.required("value")?.clone())
            }
            "hold" => {
                self.store.set_metadata("posted", "yes")?;
                fs::write(self.root.join("blocked"), b"started").unwrap();
                while !self.root.join("release").is_file() {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Ok(true.into())
            }
            "write" => {
                self.store
                    .connection()
                    .execute(
                        "UPDATE metadata SET value=CAST(value AS INTEGER)+1 WHERE key='calls'",
                        [],
                    )
                    .unwrap();
                self.store
                    .connection()
                    .execute("UPDATE credentials SET revoked=1", [])
                    .unwrap();
                Ok(self.store.metadata("calls")?.unwrap().into())
            }
            "calls" => Ok(self.store.metadata("calls")?.unwrap().into()),
            "malformed" => {
                std::io::stdout()
                    .write_all(&u32::MAX.to_be_bytes())
                    .unwrap();
                std::io::stdout().flush().unwrap();
                std::thread::sleep(Duration::from_secs(60));
                Ok(IvyJson::Null)
            }
            "exit" => std::process::exit(7),
            "error" => Err(IvyError::new_js(
                "fixture_error".into(),
                ivy_contracts::JsString(vec![0xd800, 120, 0xdc00]),
            )
            .with_outcome(ivy_contracts::Outcome::Completed)
            .with_details(payload.required("value")?.clone())),
            "close" => Ok(IvyJson::object([("closed", true.into())])),
            _ => panic!("Unknown synthetic worker action"),
        }
    }
    fn backup(&mut self, payload: IvyJson, progress: Progress) -> IvyResult<BackupTask> {
        let destination = payload
            .required("destination")?
            .as_js_string()
            .cloned()
            .ok_or_else(IvyError::internal)?;
        let copying = progress.clone();
        let mut backup = self.store.backup_to(destination, move || {
            copying();
            Ok(())
        });
        let delay = payload
            .get("delayMs")
            .and_then(IvyJson::as_f64)
            .unwrap_or(0.0) as u64;
        Ok(Box::pin(async move {
            // The optional heartbeat fixture delay holds the actual copy after
            // its first yielding batch, while the actor handles other actions.
            if delay > 0 {
                if let Poll::Ready(result) =
                    poll_fn(|cx| Poll::Ready(backup.as_mut().poll(cx))).await
                {
                    return result.map(|snapshot| snapshot.to_json());
                }
            }
            let until = tokio::time::Instant::now() + Duration::from_millis(delay);
            while tokio::time::Instant::now() < until {
                progress();
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            backup.await.map(|snapshot| snapshot.to_json())
        }))
    }
    fn notifications(&mut self) -> Vec<Notification> {
        std::mem::take(&mut self.notifications)
    }
}
fn main() {
    if std::env::args().nth(1).as_deref() == Some("--kernel-startup-parent") {
        initialized_kernel_parent(ParentMode::Initializing, false);
    }
    if std::env::args().nth(1).as_deref() == Some("--kernel-backup-parent") {
        initialized_kernel_parent(ParentMode::Copy, false);
    }
    if std::env::args().nth(1).as_deref() == Some("--kernel-hash-parent") {
        initialized_kernel_parent(ParentMode::Hash, false);
    }
    if std::env::args().nth(1).as_deref() == Some("--kernel-temporary-startup-parent") {
        initialized_kernel_parent(ParentMode::Initializing, true);
    }
    if std::env::args().nth(1).as_deref() == Some("--kernel-temporary-parent") {
        initialized_kernel_parent(ParentMode::Ready, true);
    }
    if std::env::args().nth(1).as_deref() == Some("--kernel-temporary") {
        initialized_kernel_temporary_entry();
    }
    if std::env::args().nth(1).as_deref() == Some("--kernel-gated-backup") {
        initialized_kernel_gated_backup(false, false);
    }
    if std::env::args().nth(1).as_deref() == Some("--kernel-gated-hash") {
        initialized_kernel_gated_backup(true, false);
    }
    if std::env::args().nth(1).as_deref() == Some("--kernel-natural-progress") {
        initialized_kernel_gated_backup(false, true);
    }
    if std::env::args().nth(1).as_deref() == Some("--kernel-parent") {
        initialized_kernel_parent(ParentMode::Ready, false);
    }
    if std::env::args().nth(1).as_deref() == Some("--kernel") {
        initialized_kernel_entry();
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = runtime.block_on(worker_server::serve(tokio::io::stdin(), tokio::io::stdout(), |options| {
        let root = PathBuf::from(options.required("root")?.as_str()?);
        let store = Store::open(&root.join("hive.sqlite"))?;
        store.set_metadata("calls", &store.metadata("calls")?.unwrap_or_else(|| "0".into()))?;
        store.connection().execute_batch("CREATE TABLE IF NOT EXISTS fixture_payload(id INTEGER PRIMARY KEY, bytes BLOB); WITH RECURSIVE n(value) AS (VALUES(1) UNION ALL SELECT value+1 FROM n WHERE value<256) INSERT OR IGNORE INTO fixture_payload SELECT value,zeroblob(4096) FROM n;").unwrap();
        fs::write(root.join("pid"), std::process::id().to_string()).unwrap();
        if options.get("neverReady") == Some(&IvyJson::Bool(true)) { loop { std::thread::park(); } }
        Ok(Service { store, root, notifications: vec![] })
    }));
    // Dropped kernel/backup closes SQLite before exit. Tokio stdin uses a
    // blocking reader; private worker exit must not wait for another input byte.
    if let Err(error) = &result {
        eprintln!(
            "{}",
            stringify(&IvyJson::from(error.kind().to_string()), 1024).unwrap()
        );
    }
    std::process::exit(if result.is_ok() { 0 } else { 1 });
}
