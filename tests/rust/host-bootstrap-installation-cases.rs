use super::BootstrapInstallation;
use crate::{
    IvyJson as J, artifact, bootstrap, config, executor_lock::ExecutorLock, host_config,
    journal::HostJournal, node_path, node_runtime::NodeRuntime,
};
use ivy_contracts::{core_schema::validate_host, hash::digest, json};
use ivy_sdk::clock::SystemClock;
use std::{fs, path::PathBuf, sync::Arc};

struct Fixture {
    root: PathBuf,
    path: PathBuf,
    config: J,
    candidate: J,
    installation: BootstrapInstallation,
}
fn p(path: &std::path::Path) -> J {
    J::String(node_path::from_native(path))
}
impl Fixture {
    async fn new() -> Self {
        Self::for_plan(false).await
    }
    async fn for_plan(native: bool) -> Self {
        Self::with_plan_tools(native, false).await
    }
    async fn with_plan_tools(native: bool, declared_node: bool) -> Self {
        let root =
            std::env::temp_dir().join(format!("ivy-bootstrap-install-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let path = root.join("host.json");
        let node_path =
            PathBuf::from(std::env::var_os("IVY_TEST_NODE").expect("Pinned Node path required"));
        let host = J::object([
            ("schemaVersion", J::Number(1.0)),
            ("hostId", "bootstrap-host".into()),
            ("runtimeRoot", p(&root.join("state"))),
            ("artifactRoot", p(&root.join("artifacts"))),
            ("stagingRoot", p(&root.join("staging"))),
            ("publicBaseUrl", "https://example.invalid/ivy".into()),
            (
                "executables",
                if native && !declared_node {
                    J::object([("native-host", p(&std::env::current_exe().unwrap()))])
                } else {
                    J::object([("node", p(&node_path))])
                },
            ),
            (
                "instances",
                J::Array(vec![J::object([
                    ("instanceId", "renamed-host".into()),
                    ("serviceNodeId", "host.service".into()),
                    ("componentId", "host-executor".into()),
                    ("engine", "process".into()),
                    ("enabled", true.into()),
                    ("settings", J::object([] as [(&str, J); 0])),
                ])]),
            ),
        ]);
        config::atomic_json(&path, host).await.unwrap();
        let config = host_config::host_config(path.clone()).await.unwrap();
        let release = root.join("artifacts/release");
        fs::create_dir_all(release.join("artifact/dist")).unwrap();
        let build = J::from(digest(b"bootstrap command candidate"));
        let platform = crate::portable_native_files::native_platform().unwrap();
        let suffix = if cfg!(windows) { ".exe" } else { "" };
        let command = |file: &str| {
            J::object([
                (
                    "executable",
                    if native {
                        format!(
                            "dist/native/{platform}/{}{}",
                            if file.contains("health") {
                                "ivy-health"
                            } else {
                                "ivy-host-executor"
                            },
                            suffix
                        )
                        .into()
                    } else {
                        "$node".into()
                    },
                ),
                ("args", J::Array(vec![file.into()])),
                ("timeoutMs", J::Number(5000.0)),
            ])
        };
        let manifest = J::object([
            ("schemaVersion", J::Number(1.0)),
            ("componentId", "host-executor".into()),
            ("kind", "service".into()),
            ("version", "1.0.0".into()),
            ("buildId", build.clone()),
            ("connectsToHive", false.into()),
            (
                "requirements",
                J::object([
                    ("node", ">=24.18.0 <25.0.0".into()),
                    ("hiveProtocol", J::Null),
                    ("contracts", J::Array(vec![])),
                ]),
            ),
            ("entrypoint", command("dist/main.js")),
            (
                "readiness",
                J::object([
                    ("timeoutMs", J::Number(5000.0)),
                    ("command", command("dist/health.js")),
                ]),
            ),
            ("shutdown", J::object([("timeoutMs", J::Number(5000.0))])),
            (
                "restart",
                J::object([
                    ("policy", "never".into()),
                    ("minimumDelayMs", J::Number(100.0)),
                    ("maximumDelayMs", J::Number(100.0)),
                ]),
            ),
        ]);
        validate_host("ReleaseManifest", &manifest).unwrap();
        config::atomic_json(release.join("component.json"), manifest.clone())
            .await
            .unwrap();
        config::atomic_json(
            release.join("artifact/dist/build-info.json"),
            J::object([
                ("schemaVersion", J::Number(1.0)),
                ("componentId", "host-executor".into()),
                ("version", "1.0.0".into()),
                ("buildId", build.clone()),
            ]),
        )
        .await
        .unwrap();
        for file in ["dist/main.js", "dist/health.js"] {
            fs::write(
                release.join("artifact").join(file),
                b"throw Error('candidate verification must not run a backend');",
            )
            .unwrap();
        }
        // The existing verifier checks retained payload presence. These are
        // inert fixture bytes, never executable/runtime qualification evidence.
        for file in crate::portable_native_files::portable_native_files("host-executor") {
            let file = if native {
                format!(
                    "dist/native/{platform}/{}",
                    file.rsplit('/').next().unwrap()
                )
            } else {
                (*file).to_owned()
            };
            let file = release.join("artifact").join(file);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, b"retained native payload fixture").unwrap();
        }
        if native {
            let directory = release.join(format!("artifact/dist/native/{platform}"));
            fs::create_dir_all(&directory).unwrap();
            for name in ["ivy-host-executor", "ivy-health"] {
                fs::write(
                    directory.join(format!("{name}{suffix}")),
                    b"static plan fixture; never executed",
                )
                .unwrap();
            }
            fs::copy(
                release.join("artifact/dist/build-info.json"),
                directory.join("build-info.json"),
            )
            .unwrap();
        }
        let candidate = J::object([
            ("candidateId", build.clone()),
            ("buildId", build),
            ("componentId", "host-executor".into()),
            ("artifactRoot", p(&release.join("artifact"))),
            ("manifestPath", p(&release.join("component.json"))),
            ("createdAt", "2026-01-01T00:00:00.000Z".into()),
            (
                "platform",
                J::object([
                    ("os", if cfg!(windows) { "win32" } else { "linux" }.into()),
                    (
                        "arch",
                        if cfg!(target_arch = "x86_64") {
                            "x64"
                        } else {
                            "arm64"
                        }
                        .into(),
                    ),
                    ("node", "24.18.0".into()),
                ]),
            ),
        ]);
        validate_host("Candidate", &candidate).unwrap();
        let installation = BootstrapInstallation::native(
            (!native).then_some(NodeRuntime {
                executable: node_path,
                version: "24.18.0".into(),
            }),
            Arc::new(|_, _| panic!("One retained candidate needs no comparator")),
            root.join("unavailable-installer"),
            root.join("unavailable-job"),
            Arc::new(SystemClock),
        );
        artifact::verify_candidate(
            candidate.clone(),
            root.join("artifacts"),
            installation.node.as_ref(),
        )
        .await
        .unwrap();
        let journal = HostJournal::open(config.clone()).await.unwrap();
        journal
            .save_candidate(candidate.clone(), manifest)
            .await
            .unwrap();
        journal.close().await.unwrap();
        Self {
            root,
            path,
            config,
            candidate,
            installation,
        }
    }
    async fn plan(&self) -> J {
        self.installation
            .plan(
                self.path.clone(),
                self.candidate
                    .required("candidateId")
                    .unwrap()
                    .as_js_string()
                    .unwrap()
                    .clone(),
                None,
                &[],
            )
            .await
            .unwrap()
    }
    async fn clean(&self) {
        assert!(!self.root.join("state/bootstrap").exists());
        assert!(!self.root.join("state/bootstrap-plans").exists());
        let lock = ExecutorLock::acquire(self.root.join("state/bootstrap-maintenance-lock"), false)
            .await
            .unwrap();
        lock.close().await.unwrap();
        let db = ivy_store::rusqlite::Connection::open(self.root.join("state/deployments.sqlite"))
            .unwrap();
        assert_eq!(
            db.query_row("PRAGMA journal_mode=DELETE", [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            "delete"
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let temp = std::env::temp_dir().canonicalize().unwrap();
        assert!(self.root.canonicalize().unwrap().starts_with(&temp));
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn bootstrap_command_verified_plan_and_changed_configuration_refuse_before_publication() {
    let f = Fixture::new().await;
    let plan = f.plan().await;
    let journal = HostJournal::open(f.config.clone()).await.unwrap();
    let components = bootstrap::bootstrap_components(
        &f.config,
        &journal,
        &f.candidate,
        &[],
        &f.installation.compare,
    )
    .await
    .unwrap();
    let expected = bootstrap::configuration_bootstrap_plan(
        &f.config,
        &node_path::from_native(&f.path),
        &f.candidate,
        None,
        &components,
    )
    .unwrap();
    journal.close().await.unwrap();
    assert_eq!(
        json::canonical(&plan, usize::MAX).unwrap(),
        json::canonical(&expected, usize::MAX).unwrap()
    );
    let mut changed = f.config.clone();
    changed.insert("publicBaseUrl", "https://example.invalid/changed".into());
    config::atomic_json(&f.path, changed).await.unwrap();
    let error = f.installation.install(plan, false).await.unwrap_err();
    assert_eq!(error.code, "configuration_changed", "{error:?}");
    assert_eq!(
        error.message,
        "Host configuration changed after the reviewed installation plan."
    );
    f.clean().await;
}

#[tokio::test]
async fn bootstrap_command_reviewed_plan_and_changed_candidate_keep_exact_journal_and_no_os_effect()
{
    let f = Fixture::new().await;
    let plan = f.plan().await;
    let mut changed = plan.clone();
    let mut processes = changed
        .required("processes")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    processes[0].insert("name", "ivy-next-000000000000-111111111111".into());
    changed.insert("processes", J::Array(processes));
    let error = f.installation.install(changed, false).await.unwrap_err();
    assert_eq!(error.code, "target_conflict", "{error:?}");
    assert_eq!(
        error.message,
        "Installation plan differs from the verified candidate and host configuration."
    );
    f.clean().await;
    let artifact = node_path::to_native(
        f.candidate
            .required("artifactRoot")
            .unwrap()
            .as_js_string()
            .unwrap(),
    );
    config::atomic_json(
        artifact.join("dist/build-info.json"),
        J::object([
            ("buildId", digest(b"changed").into()),
            ("version", "1.0.0".into()),
        ]),
    )
    .await
    .unwrap();
    let error = f.installation.install(plan, false).await.unwrap_err();
    assert_eq!(error.code, "build_mismatch", "{error:?}");
    f.clean().await;
    let journal = HostJournal::open(f.config.clone()).await.unwrap();
    assert_eq!(
        json::canonical(
            &journal
                .candidate(
                    &f.candidate
                        .required("candidateId")
                        .unwrap()
                        .as_str()
                        .unwrap()
                )
                .await
                .unwrap(),
            usize::MAX
        )
        .unwrap(),
        json::canonical(&f.candidate, usize::MAX).unwrap()
    );
    assert!(journal.installed("renamed-host").await.unwrap().is_none());
    journal.close().await.unwrap();
}

// These cases exercise the initial installer's own file/command boundary. They
// do not install an owner or use the real administrative command path.
mod effects {
    use super::*;
    use crate::{
        IvyError, IvyResult, Outcome,
        linux_host::{FileMetadata, LinuxHost},
        systemd::{NativeSystemdHost, SystemdHost, UnitFile},
    };
    use futures_util::future::BoxFuture;
    use ivy_contracts::JsString;
    use std::{io, sync::Mutex};

    #[derive(Clone, Debug, PartialEq)]
    enum Event {
        Open(JsString, bool),
        Write(Vec<u8>),
        Close,
        Command(&'static str, Vec<JsString>, u32, usize),
    }
    #[derive(Clone, Copy, PartialEq)]
    enum Fault {
        None,
        Open,
        Write,
        Close,
        WriteAndClose,
    }
    struct Host {
        events: Arc<Mutex<Vec<Event>>>,
        fault: Fault,
        command_error: bool,
    }
    struct File {
        events: Arc<Mutex<Vec<Event>>>,
        fault: Fault,
    }
    impl UnitFile for File {
        fn write<'a>(&'a mut self, bytes: Vec<u8>) -> BoxFuture<'a, io::Result<()>> {
            Box::pin(async move {
                self.events.lock().unwrap().push(Event::Write(bytes));
                tokio::task::yield_now().await;
                if matches!(self.fault, Fault::Write | Fault::WriteAndClose) {
                    Err(io::Error::other("injected write failure"))
                } else {
                    Ok(())
                }
            })
        }
        fn sync(&mut self) -> BoxFuture<'_, io::Result<()>> {
            panic!("Initial unit creation has no extra sync operation")
        }
        fn close(self: Box<Self>) -> BoxFuture<'static, io::Result<()>> {
            Box::pin(async move {
                self.events.lock().unwrap().push(Event::Close);
                tokio::task::yield_now().await;
                if matches!(self.fault, Fault::Close | Fault::WriteAndClose) {
                    Err(io::Error::other("injected close failure"))
                } else {
                    Ok(())
                }
            })
        }
    }
    impl SystemdHost for Host {
        fn platform(&self) -> &str {
            "linux"
        }
        fn user_id(&self) -> Option<u32> {
            Some(0)
        }
        fn metadata(&self, _: JsString) -> BoxFuture<'static, io::Result<FileMetadata>> {
            panic!("Unexpected metadata")
        }
        fn read(&self, _: JsString) -> BoxFuture<'static, io::Result<Vec<u8>>> {
            panic!("Unexpected read")
        }
        fn open(
            &self,
            path: JsString,
            exclusive: bool,
        ) -> BoxFuture<'static, io::Result<Box<dyn UnitFile>>> {
            let events = self.events.clone();
            let fault = self.fault;
            Box::pin(async move {
                events.lock().unwrap().push(Event::Open(path, exclusive));
                tokio::task::yield_now().await;
                if fault == Fault::Open {
                    Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "injected open failure",
                    ))
                } else {
                    Ok(Box::new(File { events, fault }) as Box<dyn UnitFile>)
                }
            })
        }
        fn rename(&self, _: JsString, _: JsString) -> BoxFuture<'static, io::Result<()>> {
            panic!("Unexpected rename")
        }
        fn unlink(&self, _: JsString) -> BoxFuture<'static, io::Result<()>> {
            panic!("Unexpected unlink")
        }
        fn execute(
            &self,
            _: JsString,
            _: Vec<JsString>,
        ) -> BoxFuture<'static, IvyResult<JsString>> {
            panic!("Unexpected real command path")
        }
        fn uuid(&self) -> String {
            panic!("Unexpected uuid")
        }
        fn now_ms(&self) -> f64 {
            panic!("Unexpected clock")
        }
        fn sleep(&self, _: u64) -> BoxFuture<'static, ()> {
            panic!("Unexpected sleep")
        }
    }
    impl LinuxHost for Host {
        fn platform(&self) -> &str {
            "linux"
        }
        fn user_id(&self) -> Option<u32> {
            Some(0)
        }
        fn metadata(&self, _: JsString) -> BoxFuture<'static, io::Result<FileMetadata>> {
            panic!("Unexpected metadata")
        }
        fn read(&self, _: JsString) -> BoxFuture<'static, io::Result<Vec<u8>>> {
            panic!("Unexpected read")
        }
        fn create(
            &self,
            _: JsString,
            _: Vec<u8>,
            _: u32,
            _: JsString,
        ) -> BoxFuture<'static, io::Result<()>> {
            panic!("Unexpected create")
        }
        fn command(
            &self,
            file: &'static str,
            args: Vec<JsString>,
            timeout: u32,
            maximum: usize,
        ) -> BoxFuture<'static, IvyResult<JsString>> {
            self.events
                .lock()
                .unwrap()
                .push(Event::Command(file, args, timeout, maximum));
            let failed = self.command_error;
            Box::pin(async move {
                tokio::task::yield_now().await;
                if failed {
                    Err(IvyError::new("systemd_failed", "injected command failure")
                        .with_outcome(Outcome::Unknown)
                        .with_details(J::object([("exitCode", J::Number(7.0))])))
                } else {
                    Ok("ActiveState=inactive\nSubState=dead\n".into())
                }
            })
        }
    }
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let parent = std::env::temp_dir().canonicalize().unwrap();
            let path = parent.join(format!("ivy-bootstrap-effects-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            assert!(
                self.0
                    .canonicalize()
                    .unwrap()
                    .starts_with(std::env::temp_dir().canonicalize().unwrap())
            );
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[tokio::test]
    async fn bootstrap_install_effect_native_file_is_exclusive_closed_and_not_found_is_distinct() {
        let root = Temp::new();
        let files = NativeSystemdHost::new(root.0.clone());
        let path = root.0.join("literal [unit] Ω.service");
        assert!(
            super::super::read_unit(path.clone(), &files)
                .await
                .unwrap()
                .is_none()
        );
        let content = JsString::from("\u{feff}[Unit]\nDescription=Ω boot\n");
        super::super::create_unit(path.clone(), content.clone(), &files)
            .await
            .unwrap();
        assert_eq!(
            fs::read(&path).unwrap(),
            String::from_utf16_lossy(&content.0).as_bytes()
        );
        assert_eq!(
            super::super::read_unit(path.clone(), &files).await.unwrap(),
            Some(content)
        );
        let error = super::super::create_unit(path.clone(), "replacement".into(), &files)
            .await
            .unwrap_err();
        assert_eq!(error.code, "internal_error");
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains("Description=Ω boot")
        );
        let moved = root.0.join("closed.service");
        fs::rename(&path, &moved).unwrap();
        fs::remove_file(moved).unwrap();
        super::super::create_unit(path.clone(), "second immutable unit".into(), &files)
            .await
            .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "second immutable unit");
        let error = super::super::read_unit(root.0.clone(), &files)
            .await
            .unwrap_err();
        assert_eq!(error.code, "internal_error");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o022, 0);
        }
    }

    #[tokio::test]
    async fn bootstrap_install_effect_file_faults_close_once_and_never_report_success() {
        let root = Temp::new();
        let path = root.0.join("unused-scripted-unit.service");
        let content = JsString::from("[Unit]\nDescription=checked bytes\n");
        for fault in [
            Fault::None,
            Fault::Open,
            Fault::Write,
            Fault::Close,
            Fault::WriteAndClose,
        ] {
            let host = Host {
                events: Arc::new(Mutex::new(vec![])),
                fault,
                command_error: false,
            };
            let result = super::super::create_unit(path.clone(), content.clone(), &host).await;
            if fault == Fault::None {
                result.unwrap();
            } else {
                assert_eq!(result.unwrap_err().code, "internal_error");
            }
            let mut expected = vec![Event::Open(node_path::from_native(&path), true)];
            if fault != Fault::Open {
                expected.extend([
                    Event::Write(String::from_utf16_lossy(&content.0).into_bytes()),
                    Event::Close,
                ]);
            }
            assert_eq!(*host.events.lock().unwrap(), expected);
        }
        assert!(
            !path.exists(),
            "The scripted host must never touch its label path"
        );
    }

    #[tokio::test]
    async fn bootstrap_install_effect_commands_keep_literal_arguments_bounds_and_unknown_outcome() {
        let root = Temp::new();
        let distribution = root.0.join("distribution [Ω] with spaces;$literal");
        let plan = root.0.join("reviewed [Ω] with spaces;$plan.json");
        let ordinary = super::super::windows_command(distribution.clone(), plan.clone(), false);
        let replacement = super::super::windows_command(distribution.clone(), plan.clone(), true);
        let expected: Vec<std::ffi::OsString> = vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-ExecutionPolicy".into(),
            "Bypass".into(),
            "-File".into(),
            distribution
                .join("dist/packages/host-runtime/assets/install-windows.ps1")
                .into_os_string(),
            "-PlanPath".into(),
            plan.into_os_string(),
        ];
        assert_eq!(ordinary.args, expected);
        assert_eq!(&replacement.args[..expected.len()], expected.as_slice());
        assert_eq!(replacement.args.last().unwrap(), "-ReplaceLegacy");
        assert_eq!(ordinary.timeout_ms, 60_000);
        assert_eq!(ordinary.maximum_buffer_bytes, 65_536);
        assert!(ordinary.cwd.is_none());
        assert_eq!(
            ordinary.executable,
            PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
                .join("System32/WindowsPowerShell/v1.0/powershell.exe")
                .into_os_string()
        );

        let args: Vec<JsString> = vec![
            "show".into(),
            "--no-pager".into(),
            "--property=ActiveState,SubState".into(),
            "ivy-next-0123456789ab-abcdef012345.service".into(),
        ];
        for failed in [false, true] {
            let host = Host {
                events: Arc::new(Mutex::new(vec![])),
                fault: Fault::None,
                command_error: failed,
            };
            let result = super::super::command(&host, args.clone()).await;
            assert_eq!(
                *host.events.lock().unwrap(),
                vec![Event::Command(
                    "/usr/bin/systemctl",
                    args.clone(),
                    30_000,
                    65_536
                )]
            );
            if failed {
                let error = result.unwrap_err();
                assert_eq!(error.code, "systemd_failed");
                assert_eq!(error.message, "injected command failure");
                assert_eq!(error.outcome, Outcome::Unknown);
                assert_eq!(
                    error.details,
                    Some(J::object([("exitCode", J::Number(7.0))]))
                );
            } else {
                assert!(super::super::stopped(&result.unwrap()));
            }
        }
        for (output, allowed) in [
            ("\u{feff}ActiveState=inactive\nSubState=dead\n", true),
            ("ActiveState=failed\nSubState=failed\n", true),
            ("ActiveState=active\nSubState=dead\n", false),
            ("ActiveState=inactive\nSubState=running\n", false),
            ("ActiveState=inactive\n", false),
            ("SubState=dead\n", false),
            ("", false),
            (
                "ActiveState=inactive\nSubState=dead\nActiveState=activating",
                false,
            ),
        ] {
            assert_eq!(super::super::stopped(&output.into()), allowed, "{output}");
        }
        let name = JsString::from("ivy-next-0123456789ab-abcdef012345");
        assert!(super::super::valid_name(&name));
        assert_eq!(
            super::super::unit_path(&"ivy-next-0123456789ab-abcdef012345.service".into()),
            node_path::to_native(
                &"/etc/systemd/system/ivy-next-0123456789ab-abcdef012345.service".into()
            )
        );
        for name in [
            "../ivy-next-0123456789ab-abcdef012345",
            "ivy-next-0123456789ab-ABCDEF012345",
            "ivy-next-0123456789ab-abcdef012345.service",
            "ivy-next-0123456789ab-abcdef01234\n",
        ] {
            assert!(!super::super::valid_name(&name.into()), "{name}");
        }
    }
}

// A coherent post-review model: files survive failed later effects, stopping is
// observed rather than assumed, and reload/start inspect the real temp journal.
// Calling this private Linux phase on Windows does not qualify Linux admission.
mod phases {
    use super::super::effects::UnitDirectoryEntry;
    use super::*;
    use crate::{
        IvyError, IvyResult, Outcome, exec_file,
        linux_host::{FileMetadata, LinuxHost},
        linux_resources,
        systemd::{SystemdHost, UnitFile},
    };
    use futures_util::future::BoxFuture;
    use ivy_contracts::JsString;
    use ivy_sdk::clock::Clock;
    use std::{
        collections::{BTreeMap, VecDeque},
        io,
        sync::{
            Mutex,
            atomic::{AtomicI64, Ordering},
        },
        time::Instant,
    };

    #[derive(Clone, Debug, PartialEq)]
    enum Event {
        Scan,
        Read(JsString),
        Open(JsString, bool),
        Write(JsString),
        Close(JsString),
        Unlink(JsString),
        Sleep(u64),
        Command(Vec<JsString>),
        Metadata(JsString),
        Create(JsString, u32, JsString),
    }
    #[derive(Default)]
    struct State {
        entries: Vec<UnitDirectoryEntry>,
        files: BTreeMap<JsString, Vec<u8>>,
        events: Vec<Event>,
        counts: BTreeMap<&'static str, usize>,
        fail: Option<(&'static str, usize)>,
        states: VecDeque<JsString>,
        stays_active: bool,
        slice_output: JsString,
    }
    impl State {
        fn record(&mut self, tag: &'static str, event: Event) -> bool {
            self.events.push(event);
            let count = self.counts.entry(tag).or_default();
            *count += 1;
            self.fail == Some((tag, *count))
        }
    }
    #[derive(Clone)]
    struct Host {
        state: Arc<Mutex<State>>,
        clock: Arc<AtomicI64>,
        config: J,
        platform: &'static str,
        uid: Option<u32>,
    }
    fn io_failure() -> io::Error {
        io::Error::new(io::ErrorKind::PermissionDenied, "scripted effect failure")
    }
    fn command_failure() -> IvyError {
        IvyError::new("systemd_failed", "scripted command failure")
            .with_outcome(Outcome::Unknown)
            .with_details(J::object([("exitCode", J::Number(7.0))]))
    }
    fn unit(name: &JsString) -> JsString {
        node_path::from_native(&super::super::unit_path(name))
    }
    fn names(plan: &J) -> Vec<JsString> {
        plan.required("processes")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|p| format!("{}.service", p.required("name").unwrap().as_str().unwrap()).into())
            .collect()
    }
    fn stale(plan: &J) -> JsString {
        format!(
            "{}-000000000000.service",
            plan.required("installationId").unwrap().as_str().unwrap()
        )
        .into()
    }
    fn recognized() -> Vec<u8> {
        b"[Unit]\nDescription=IvyNext old\nExecStart=:/old\nWorkingDirectory=/old\nEnvironment=NODE_NO_WARNINGS=1\n".to_vec()
    }
    fn installed(config: &J) -> usize {
        let db = ivy_store::rusqlite::Connection::open(
            node_path::to_native(
                config
                    .required("runtimeRoot")
                    .unwrap()
                    .as_js_string()
                    .unwrap(),
            )
            .join("deployments.sqlite"),
        )
        .unwrap();
        let count: i64 = db
            .query_row("SELECT count(*) FROM installed", [], |r| r.get(0))
            .unwrap();
        db.close().unwrap();
        usize::try_from(count).unwrap()
    }
    fn journal_closed(config: &J) {
        let db = ivy_store::rusqlite::Connection::open(
            node_path::to_native(
                config
                    .required("runtimeRoot")
                    .unwrap()
                    .as_js_string()
                    .unwrap(),
            )
            .join("deployments.sqlite"),
        )
        .unwrap();
        assert_eq!(
            db.query_row("PRAGMA journal_mode=DELETE", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "delete"
        );
        db.close().unwrap();
    }
    impl Host {
        fn bind(f: &mut Fixture) -> Self {
            let host = Self {
                state: Arc::new(Mutex::new(State::default())),
                clock: Arc::new(AtomicI64::new(SystemClock.utc_millis())),
                config: f.config.clone(),
                platform: "linux",
                uid: Some(0),
            };
            f.installation.clock = Arc::new(host.clone());
            f.installation.effects.linux = Arc::new(host.clone());
            f.installation.effects.units = Arc::new(host.clone());
            let state = host.state.clone();
            f.installation.effects.unit_entries = Arc::new(move || {
                let state = state.clone();
                Box::pin(async move {
                    tokio::task::yield_now().await;
                    let mut state = state.lock().unwrap();
                    if state.record("scan", Event::Scan) {
                        return Err(IvyError::internal());
                    }
                    Ok(state.entries.clone())
                })
            });
            f.installation.effects.execute_windows =
                Arc::new(|_, _, _| panic!("Linux model cannot execute Windows"));
            host
        }
        fn owner(&self, name: JsString, bytes: Vec<u8>) {
            let mut state = self.state.lock().unwrap();
            state.entries.push(UnitDirectoryEntry {
                name: name.clone(),
                is_file: true,
            });
            state.files.insert(unit(&name), bytes);
        }
        fn count(&self, tag: &str) -> usize {
            *self.state.lock().unwrap().counts.get(tag).unwrap_or(&0)
        }
        fn fail(&self, tag: &'static str, at: usize) {
            self.state.lock().unwrap().fail = Some((tag, at));
        }
        fn read_file(
            &self,
            path: JsString,
            tag: &'static str,
        ) -> BoxFuture<'static, io::Result<Vec<u8>>> {
            let state = self.state.clone();
            Box::pin(async move {
                tokio::task::yield_now().await;
                let mut state = state.lock().unwrap();
                if state.record(tag, Event::Read(path.clone())) {
                    return Err(io_failure());
                }
                state
                    .files
                    .get(&path)
                    .cloned()
                    .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
            })
        }
    }
    impl Clock for Host {
        fn utc_millis(&self) -> i64 {
            self.clock.load(Ordering::SeqCst)
        }
        fn monotonic(&self) -> Instant {
            Instant::now()
        }
    }
    struct File {
        host: Host,
        path: JsString,
    }
    impl UnitFile for File {
        fn write(&mut self, bytes: Vec<u8>) -> BoxFuture<'_, io::Result<()>> {
            Box::pin(async move {
                tokio::task::yield_now().await;
                let mut state = self.host.state.lock().unwrap();
                let fail = state.record("write", Event::Write(self.path.clone()));
                state.files.insert(
                    self.path.clone(),
                    if fail { bytes[..3].to_vec() } else { bytes },
                );
                if fail { Err(io_failure()) } else { Ok(()) }
            })
        }
        fn sync(&mut self) -> BoxFuture<'_, io::Result<()>> {
            panic!("Initial unit write must not add sync")
        }
        fn close(self: Box<Self>) -> BoxFuture<'static, io::Result<()>> {
            Box::pin(async move {
                tokio::task::yield_now().await;
                if self
                    .host
                    .state
                    .lock()
                    .unwrap()
                    .record("close", Event::Close(self.path))
                {
                    Err(io_failure())
                } else {
                    Ok(())
                }
            })
        }
    }
    impl SystemdHost for Host {
        fn platform(&self) -> &str {
            self.platform
        }
        fn user_id(&self) -> Option<u32> {
            self.uid
        }
        fn metadata(&self, _: JsString) -> BoxFuture<'static, io::Result<FileMetadata>> {
            panic!("No added unit metadata policy")
        }
        fn read(&self, path: JsString) -> BoxFuture<'static, io::Result<Vec<u8>>> {
            self.read_file(path, "read")
        }
        fn open(
            &self,
            path: JsString,
            exclusive: bool,
        ) -> BoxFuture<'static, io::Result<Box<dyn UnitFile>>> {
            let host = self.clone();
            Box::pin(async move {
                tokio::task::yield_now().await;
                assert!(exclusive);
                {
                    let mut state = host.state.lock().unwrap();
                    if state.record("open", Event::Open(path.clone(), exclusive)) {
                        state
                            .files
                            .insert(path.clone(), b"concurrent creator".to_vec());
                    }
                    if state.files.contains_key(&path) {
                        return Err(io::Error::from(io::ErrorKind::AlreadyExists));
                    }
                    state.files.insert(path.clone(), vec![]);
                }
                Ok(Box::new(File { host, path }) as Box<dyn UnitFile>)
            })
        }
        fn rename(&self, _: JsString, _: JsString) -> BoxFuture<'static, io::Result<()>> {
            panic!("No atomic replacement")
        }
        fn unlink(&self, path: JsString) -> BoxFuture<'static, io::Result<()>> {
            let state = self.state.clone();
            Box::pin(async move {
                tokio::task::yield_now().await;
                let mut state = state.lock().unwrap();
                if state.record("unlink", Event::Unlink(path.clone())) {
                    return Err(io_failure());
                }
                assert!(state.files.remove(&path).is_some());
                Ok(())
            })
        }
        fn execute(
            &self,
            _: JsString,
            _: Vec<JsString>,
        ) -> BoxFuture<'static, IvyResult<JsString>> {
            panic!("Do not substitute maintenance command policy")
        }
        fn uuid(&self) -> String {
            panic!("No extra pending unit")
        }
        fn now_ms(&self) -> f64 {
            panic!("Stopped deadline belongs to installer Clock")
        }
        fn sleep(&self, ms: u64) -> BoxFuture<'static, ()> {
            let host = self.clone();
            Box::pin(async move {
                assert_eq!(ms, 100);
                host.state.lock().unwrap().record("sleep", Event::Sleep(ms));
                host.clock.fetch_add(ms as i64, Ordering::SeqCst);
                tokio::task::yield_now().await;
            })
        }
    }
    impl LinuxHost for Host {
        fn platform(&self) -> &str {
            self.platform
        }
        fn user_id(&self) -> Option<u32> {
            self.uid
        }
        fn metadata(&self, path: JsString) -> BoxFuture<'static, io::Result<FileMetadata>> {
            let state = self.state.clone();
            Box::pin(async move {
                let mut state = state.lock().unwrap();
                if state.record("slice-metadata", Event::Metadata(path.clone())) {
                    return Err(io_failure());
                }
                let bytes = state
                    .files
                    .get(&path)
                    .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
                Ok(FileMetadata {
                    is_file: true,
                    is_symlink: false,
                    uid: Some(0),
                    gid: Some(0),
                    mode: 0o644,
                    size: bytes.len() as u64,
                })
            })
        }
        fn read(&self, path: JsString) -> BoxFuture<'static, io::Result<Vec<u8>>> {
            self.read_file(path, "slice-read")
        }
        fn create(
            &self,
            path: JsString,
            bytes: Vec<u8>,
            mode: u32,
            parent: JsString,
        ) -> BoxFuture<'static, io::Result<()>> {
            let state = self.state.clone();
            Box::pin(async move {
                assert_eq!(mode, 0o644);
                assert_eq!(parent, JsString::from("/etc/systemd/system"));
                let mut state = state.lock().unwrap();
                if state.record("slice-create", Event::Create(path.clone(), mode, parent)) {
                    return Err(io_failure());
                }
                assert!(state.files.insert(path, bytes).is_none());
                Ok(())
            })
        }
        fn command(
            &self,
            executable: &'static str,
            args: Vec<JsString>,
            timeout: u32,
            maximum: usize,
        ) -> BoxFuture<'static, IvyResult<JsString>> {
            let host = self.clone();
            Box::pin(async move {
                let verb = args[0].to_utf8().unwrap();
                let slice = verb == "show" && args[1] != JsString::from("--no-pager");
                let tag = if slice {
                    "slice-inspect"
                } else {
                    match verb.as_str() {
                        "disable" => "disable",
                        "show" => "show",
                        "daemon-reload" => "reload",
                        "enable" => "enable",
                        _ => panic!("Unexpected command"),
                    }
                };
                assert_eq!(executable, "/usr/bin/systemctl");
                assert_eq!(
                    (timeout, maximum),
                    if slice {
                        (10_000, 16_384)
                    } else {
                        (30_000, 65_536)
                    }
                );
                if matches!(tag, "reload" | "slice-inspect" | "enable") {
                    assert!(
                        installed(&host.config) > 0,
                        "journal sync must precede reload/inspect/start"
                    );
                }
                tokio::task::yield_now().await;
                let mut state = host.state.lock().unwrap();
                if state.record(tag, Event::Command(args.clone())) {
                    return Err(command_failure());
                }
                Ok(if slice {
                    state.slice_output.clone()
                } else if tag == "show" {
                    assert_eq!(
                        &args[..3],
                        &[
                            "show".into(),
                            "--no-pager".into(),
                            "--property=ActiveState,SubState".into()
                        ]
                    );
                    state.states.pop_front().unwrap_or_else(|| {
                        if state.stays_active {
                            "ActiveState=active\nSubState=running\n".into()
                        } else {
                            "ActiveState=inactive\nSubState=dead\n".into()
                        }
                    })
                } else {
                    "".into()
                })
            })
        }
    }
    async fn pair() -> Fixture {
        let mut f = Fixture::new().await;
        let mut instances = f
            .config
            .required("instances")
            .unwrap()
            .as_array()
            .unwrap()
            .to_vec();
        let mut second = instances[0].clone();
        second.insert("instanceId", "second-host".into());
        second.insert("serviceNodeId", "second.host".into());
        instances.push(second);
        f.config.insert("instances", J::Array(instances));
        config::atomic_json(&f.path, f.config.clone())
            .await
            .unwrap();
        f.config = host_config::host_config(f.path.clone()).await.unwrap();
        f
    }
    fn linux_model_plan(mut plan: J) -> J {
        // These are command/file labels in the scripted post-review phase,
        // never native paths or evidence of Linux candidate admission.
        plan.insert("artifactRoot", "/model/artifact".into());
        plan.insert("nodeExecutable", "/model/node".into());
        plan.insert("configPath", "/model/host.json".into());
        let mut processes = plan
            .required("processes")
            .unwrap()
            .as_array()
            .unwrap()
            .to_vec();
        for process in &mut processes {
            process.insert("artifactRoot", "/model/artifact".into());
            let id = process.required("instanceId").unwrap().as_str().unwrap();
            process.insert("configPath", format!("/model/instances/{id}.json").into());
        }
        plan.insert("processes", J::Array(processes));
        plan
    }
    fn scope(plan: &mut J, host: &Host) {
        let scope = linux_resources::linux_resource_scope(
            &plan.required("installationId").unwrap().as_str().unwrap(),
            &J::object([
                ("memoryHighBytes", J::Number(67_108_864.0)),
                ("memoryMaxBytes", J::Number(134_217_728.0)),
                ("tasksMax", J::Number(16.0)),
            ]),
        )
        .unwrap();
        let slice = scope.required("slice").unwrap().as_str().unwrap();
        host.state.lock().unwrap().slice_output = format!("FragmentPath=/etc/systemd/system/{slice}\nDropInPaths=\nTransient=no\nLoadState=loaded\nMemoryHigh=67108864\nMemoryMax=134217728\nMemorySwapMax=0\nTasksMax=16\n").into();
        plan.insert("linuxResources", scope);
    }

    #[tokio::test]
    async fn bootstrap_install_phase_owner_batch_is_validated_before_any_disable() {
        let mut f = Fixture::new().await;
        let plan = linux_model_plan(f.plan().await);
        let wanted = names(&plan)[0].clone();
        let bytes = bootstrap::linux_unit(
            &plan,
            &plan.required("processes").unwrap().as_array().unwrap()[0],
            None,
        )
        .unwrap()
        .to_utf8()
        .unwrap()
        .into_bytes();
        for case in [
            "exact",
            "scan",
            "read",
            "missing",
            "foreign",
            "legacy-refused",
            "legacy-allowed",
            "mixed",
        ] {
            let host = Host::bind(&mut f);
            host.owner(wanted.clone(), bytes.clone());
            // Neither nonregular owned names nor unrelated/native UTF-16 names
            // may become unit reads, stops or removals.
            host.state.lock().unwrap().entries.extend([
                UnitDirectoryEntry {
                    name: stale(&plan),
                    is_file: false,
                },
                UnitDirectoryEntry {
                    name: "ivy-next-ffffffffffff-000000000000.service".into(),
                    is_file: true,
                },
                UnitDirectoryEntry {
                    name: JsString(vec![0xd800, 0x2e]),
                    is_file: true,
                },
            ]);
            if case != "exact" && case != "scan" {
                host.owner(stale(&plan), if case.starts_with("legacy") { b"[Unit]\nDescription=IvyNext legacy\nExecStart=guardian.js\nEnvironment=LEGACY=1\n".to_vec() } else if case == "foreign" { b"[Unit]\nDescription=another owner\n".to_vec() } else { recognized() });
            }
            if case == "scan" {
                host.fail("scan", 1);
            }
            if case == "read" {
                host.fail("read", 2);
            }
            if case == "missing" {
                host.state
                    .lock()
                    .unwrap()
                    .files
                    .remove(&unit(&stale(&plan)));
            }
            let result = f
                .installation
                .replace_systemd_owners(&plan, case == "legacy-allowed", &host)
                .await;
            if matches!(case, "exact" | "mixed" | "legacy-allowed") {
                result.unwrap();
            } else {
                let error = result.unwrap_err();
                assert_eq!(
                    error.code,
                    if matches!(case, "foreign" | "legacy-refused") {
                        "target_conflict"
                    } else {
                        "internal_error"
                    },
                    "{case}"
                );
            }
            if matches!(case, "mixed" | "legacy-allowed") {
                let events = host.state.lock().unwrap().events.clone();
                assert_eq!(
                    &events[..3],
                    &[
                        Event::Scan,
                        Event::Read(unit(&wanted)),
                        Event::Read(unit(&stale(&plan)))
                    ]
                );
                assert_eq!(host.count("disable"), 2);
                assert_eq!(host.count("unlink"), 2);
                assert!(host.state.lock().unwrap().files.is_empty());
                assert_eq!(
                    events
                        .iter()
                        .filter_map(|e| if let Event::Unlink(p) = e {
                            Some(p.clone())
                        } else {
                            None
                        })
                        .collect::<Vec<_>>(),
                    vec![unit(&wanted), unit(&stale(&plan))]
                );
            } else {
                assert_eq!(host.count("disable"), 0, "{case}");
                assert_eq!(host.count("unlink"), 0, "{case}");
                assert_eq!(
                    host.state.lock().unwrap().files.get(&unit(&wanted)),
                    Some(&bytes)
                );
            }
        }
    }

    #[tokio::test]
    async fn bootstrap_install_phase_stop_proof_deadline_and_failures_retain_partial_effects() {
        let mut f = Fixture::new().await;
        let plan = linux_model_plan(f.plan().await);
        let wanted = names(&plan)[0].clone();
        {
            let host = Host::bind(&mut f);
            host.owner(wanted.clone(), recognized());
            host.owner(stale(&plan), recognized());
            host.state.lock().unwrap().states.extend([
                "ActiveState=inactive\nSubState=running\n".into(),
                "ActiveState=active\nSubState=dead\n".into(),
                "ActiveState=failed\nSubState=failed\n".into(),
            ]);
            let result = f.installation.finish_linux(&plan, &f.config, false).await;
            result.unwrap();
            assert_eq!(host.count("sleep"), 2);
            assert_eq!(host.count("unlink"), 2);
            assert_eq!(installed(&f.config), 1);
        }
        // Each failed phase gets its own real journal; no previous installed
        // row can disguise an incorrectly early sync.
        for case in ["timeout", "disable", "show", "unlink"] {
            let mut f = Fixture::new().await;
            let plan = linux_model_plan(f.plan().await);
            let wanted = names(&plan)[0].clone();
            let host = Host::bind(&mut f);
            host.owner(wanted.clone(), recognized());
            host.owner(stale(&plan), recognized());
            let start = host.utc_millis();
            if case == "timeout" {
                host.state.lock().unwrap().stays_active = true;
            } else {
                host.fail(case, if case == "unlink" { 2 } else { 1 });
            }
            let error = f
                .installation
                .finish_linux(&plan, &f.config, false)
                .await
                .unwrap_err();
            if case == "timeout" {
                assert_eq!(error.code, "outcome_unknown");
                assert_eq!(error.outcome, Outcome::Unknown);
                assert_eq!(host.utc_millis() - start, 30_000);
                assert_eq!(host.count("show"), 301);
                assert_eq!(host.count("sleep"), 300);
            } else if case != "unlink" {
                assert_eq!(error.code, "systemd_failed");
                assert_eq!(error.outcome, Outcome::Unknown);
                assert_eq!(error.details, command_failure().details);
            } else {
                assert_eq!(error.code, "internal_error");
            }
            assert_eq!(host.count("reload"), 0);
            assert_eq!(host.count("open"), 0);
            assert_eq!(host.count("enable"), 0);
            assert_eq!(installed(&f.config), 0);
            let state = host.state.lock().unwrap();
            assert!(state.files.contains_key(&unit(&stale(&plan))));
            assert_eq!(state.files.contains_key(&unit(&wanted)), case != "unlink");
            drop(state);
            journal_closed(&f.config);
        }
    }

    #[tokio::test]
    async fn bootstrap_install_phase_creation_sync_slice_and_start_fail_stop_without_rollback() {
        for case in [
            "success",
            "slice-metadata",
            "slice-create",
            "slice-read",
            "read",
            "conflict",
            "open",
            "write",
            "close",
            "sync",
            "reload",
            "slice-inspect",
            "slice-values",
            "enable",
        ] {
            let mut f = pair().await;
            let mut plan = linux_model_plan(f.plan().await);
            let host = Host::bind(&mut f);
            scope(&mut plan, &host);
            let wanted = names(&plan);
            assert_eq!(wanted.len(), 2);
            if case == "sync" {
                let mut processes = plan
                    .required("processes")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .to_vec();
                processes[1].insert("candidateId", digest(b"unknown scripted candidate").into());
                plan.insert("processes", J::Array(processes));
            } else if case == "conflict" {
                host.state
                    .lock()
                    .unwrap()
                    .files
                    .insert(unit(&wanted[0]), b"unlisted concurrent owner".to_vec());
            } else if case == "slice-values" {
                host.state.lock().unwrap().slice_output = "LoadState=not-found\n".into();
            } else if case != "success" {
                host.fail(case, if case == "enable" { 2 } else { 1 });
            }
            let result = f.installation.finish_linux(&plan, &f.config, false).await;
            if case == "success" {
                let output = result.unwrap();
                assert_eq!(
                    output.required("data").unwrap().required("units").unwrap(),
                    &J::Array(wanted.iter().cloned().map(J::String).collect())
                );
                validate_host("CliOutput", &output).unwrap();
            } else {
                assert!(result.is_err(), "{case}");
            }
            let after_sync = matches!(
                case,
                "success" | "reload" | "slice-inspect" | "slice-values" | "enable"
            );
            assert_eq!(
                installed(&f.config),
                if after_sync { 2 } else { 0 },
                "{case}"
            );
            assert_eq!(host.count("reload"), usize::from(after_sync), "{case}");
            assert_eq!(
                host.count("enable"),
                if matches!(case, "success" | "enable") {
                    2
                } else {
                    0
                },
                "{case}"
            );
            assert_eq!(
                host.count("close"),
                if matches!(
                    case,
                    "slice-metadata" | "slice-create" | "slice-read" | "read" | "conflict" | "open"
                ) {
                    0
                } else if matches!(case, "write" | "close") {
                    1
                } else {
                    2
                },
                "{case}"
            );
            let state = host.state.lock().unwrap();
            if case == "open" {
                assert_eq!(
                    state.files.get(&unit(&wanted[0])).unwrap(),
                    b"concurrent creator"
                );
            }
            if case == "write" {
                assert_eq!(state.files.get(&unit(&wanted[0])).unwrap(), b"[Un");
            }
            if case == "conflict" {
                assert_eq!(
                    state.files.get(&unit(&wanted[0])).unwrap(),
                    b"unlisted concurrent owner"
                );
            }
            if case == "success" {
                let commands = state
                    .events
                    .iter()
                    .filter_map(|e| {
                        if let Event::Command(args) = e {
                            Some(args.clone())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                assert_eq!(commands[0], vec![JsString::from("daemon-reload")]);
                assert_eq!(
                    commands[2],
                    vec!["enable".into(), "--now".into(), wanted[0].clone()]
                );
                assert_eq!(
                    commands[3],
                    vec!["enable".into(), "--now".into(), wanted[1].clone()]
                );
                for (process, name) in plan
                    .required("processes")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .iter()
                    .zip(&wanted)
                {
                    assert_eq!(
                        state.files.get(&unit(name)).unwrap(),
                        bootstrap::linux_unit(&plan, process, None)
                            .unwrap()
                            .to_utf8()
                            .unwrap()
                            .as_bytes()
                    );
                }
            }
            drop(state);
            journal_closed(&f.config);
        }
    }

    #[tokio::test]
    async fn bootstrap_install_phase_requires_linux_administrator_before_scanning() {
        for (platform, uid) in [("win32", Some(0)), ("linux", None), ("linux", Some(1))] {
            let mut f = Fixture::new().await;
            let plan = f.plan().await;
            let mut host = Host::bind(&mut f);
            host.platform = platform;
            host.uid = uid;
            f.installation.effects.linux = Arc::new(host.clone());
            assert_eq!(
                f.installation
                    .finish_linux(&plan, &f.config, false)
                    .await
                    .unwrap_err()
                    .code,
                "unsupported_runtime"
            );
            assert!(host.state.lock().unwrap().events.is_empty());
            assert_eq!(installed(&f.config), 0);
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn bootstrap_install_phase_windows_publication_result_validation_and_sync_are_ordered() {
        fn same_json(actual: &J, expected: &J) {
            assert_eq!(
                json::canonical(actual, usize::MAX).unwrap(),
                json::canonical(expected, usize::MAX).unwrap()
            );
        }
        for case in [
            "success",
            "valid-negative",
            "command",
            "failure",
            "unicode",
            "parse",
            "schema",
            "sync",
        ] {
            let mut f = Fixture::new().await;
            let plan = f.plan().await;
            // Bypass only the separately owned private-directory/ACL effect.
            // This test never executes PowerShell, a Job helper or fixture bytes.
            fs::create_dir(f.root.join("state/bootstrap-plans")).unwrap();
            let config = f.config.clone();
            let expected = plan.clone();
            let output = J::object([
                ("schemaVersion", J::Number(1.0)),
                ("ok", (case != "valid-negative").into()),
                ("code", "complete_script_output".into()),
                ("deploymentId", "script-deployment".into()),
                (
                    "data",
                    J::object([("nested", J::Array(vec!["Ω".into(), true.into(), J::Null]))]),
                ),
            ]);
            let returned = output.clone();
            let calls = Arc::new(Mutex::new(0));
            let called = calls.clone();
            let distribution = f.installation.distribution.clone();
            let launcher = f.installation.job_launcher.clone();
            f.installation.effects.unit_entries =
                Arc::new(|| panic!("Windows must not enumerate systemd"));
            f.installation.effects.execute_windows = Arc::new(move |command, root, options| {
                *called.lock().unwrap() += 1;
                let (config, expected, output) =
                    (config.clone(), expected.clone(), returned.clone());
                assert_eq!(root, distribution);
                assert_eq!(options.job_launcher.as_ref(), Some(&launcher));
                assert_eq!(
                    options.environment,
                    Some(crate::process::runtime_environment(&BTreeMap::new()))
                );
                assert!(!options.cancellation.is_cancelled());
                assert_eq!(command.timeout_ms, 60_000);
                assert_eq!(command.maximum_buffer_bytes, 65_536);
                assert_eq!(command.args.last().unwrap(), "-ReplaceLegacy");
                Box::pin(async move {
                    let selected = PathBuf::from(&command.args[7]);
                    same_json(
                        &config::json_file(selected, config::CONFIGURATION_BYTES)
                            .await
                            .unwrap(),
                        &expected,
                    );
                    same_json(
                        &J::Array(
                            bootstrap::retained_bootstrap_plans(config.clone())
                                .await
                                .unwrap(),
                        ),
                        &J::Array(vec![expected.clone()]),
                    );
                    for process in expected.required("processes").unwrap().as_array().unwrap() {
                        let published = config::json_file(
                            node_path::to_native(
                                process
                                    .required("configPath")
                                    .unwrap()
                                    .as_js_string()
                                    .unwrap(),
                            ),
                            config::CONFIGURATION_BYTES,
                        )
                        .await
                        .unwrap();
                        assert_eq!(
                            published.required("instanceId").unwrap(),
                            process.required("instanceId").unwrap()
                        );
                        assert_eq!(
                            published.required("buildId").unwrap(),
                            expected.required("candidateId").unwrap()
                        );
                    }
                    assert_eq!(installed(&config), 0);
                    journal_closed(&config);
                    if case == "command" {
                        return Err(command_failure());
                    }
                    if case == "sync" {
                        let db = ivy_store::rusqlite::Connection::open(
                            node_path::to_native(
                                config
                                    .required("runtimeRoot")
                                    .unwrap()
                                    .as_js_string()
                                    .unwrap(),
                            )
                            .join("deployments.sqlite"),
                        )
                        .unwrap();
                        db.execute_batch("PRAGMA user_version=99").unwrap();
                        db.close().unwrap();
                    }
                    Ok(exec_file::FileResult {
                        stdout: match case {
                            "unicode" => JsString(vec![0xd800]),
                            "parse" => "{".into(),
                            "schema" => "{}".into(),
                            _ => json::canonical(&output, usize::MAX).unwrap().into(),
                        },
                        stderr: "retained diagnostic".into(),
                        exit_code: Some(0),
                        signal: None,
                        failure: (case == "failure").then_some(exec_file::FileFailure::Timeout),
                    })
                })
            });
            let result = f.installation.install(plan.clone(), true).await;
            if matches!(case, "success" | "valid-negative") {
                same_json(&result.unwrap(), &output);
            } else {
                let error = result.unwrap_err();
                if case == "command" {
                    assert_eq!(error.code, "systemd_failed");
                    assert_eq!(error.outcome, Outcome::Unknown);
                    assert_eq!(error.details, command_failure().details);
                }
            }
            assert_eq!(*calls.lock().unwrap(), 1, "{case}");
            assert_eq!(
                installed(&f.config),
                usize::from(matches!(case, "success" | "valid-negative")),
                "{case}"
            );
            same_json(
                &J::Array(
                    bootstrap::retained_bootstrap_plans(f.config.clone())
                        .await
                        .unwrap(),
                ),
                &J::Array(vec![plan]),
            );
            journal_closed(&f.config);
            let lock =
                ExecutorLock::acquire(f.root.join("state/bootstrap-maintenance-lock"), false)
                    .await
                    .unwrap();
            lock.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn native_bootstrap_plan_needs_no_node_and_legacy_candidates_still_require_it() {
    let missing = Fixture::for_plan(true).await;
    let plan = missing
        .installation
        .plan(
            missing.path.clone(),
            missing
                .candidate
                .required("candidateId")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
            None,
            &[],
        )
        .await
        .unwrap();
    validate_host("BootstrapPlan", &plan).unwrap();
    assert!(plan.get("nodeExecutable").is_none());
    assert!(missing.installation.node.is_none());
    let mut linux = plan.clone();
    linux.insert("os", "linux".into());
    let mut process = linux.required("processes").unwrap().as_array().unwrap()[0].clone();
    process.insert("artifactRoot", "/native artifact".into());
    process.insert(
        "entrypoint",
        J::object([
            (
                "executable",
                "dist/native/linux-x64/ivy-host-executor".into(),
            ),
            ("args", J::Array(vec![])),
            ("timeoutMs", J::Number(5000.0)),
        ]),
    );
    assert!(
        bootstrap::linux_unit(&linux, &process, None)
            .unwrap()
            .to_utf8()
            .unwrap()
            .contains("ExecStart=:\"/native artifact/dist/native/linux-x64/ivy-host-executor\"")
    );
    process.insert(
        "entrypoint",
        J::object([
            ("executable", "node".into()),
            ("args", J::Array(vec![])),
            ("timeoutMs", J::Number(5000.0)),
        ]),
    );
    assert_eq!(
        bootstrap::linux_unit(&linux, &process, None)
            .unwrap_err()
            .code,
        "unsupported_runtime"
    );
    missing.clean().await;
    // Existing plans keep an explicitly declared tool without executing it.
    let native = Fixture::with_plan_tools(true, true).await;
    let plan = native.plan().await;
    validate_host("BootstrapPlan", &plan).unwrap();
    assert!(plan.get("nodeExecutable").is_some());
    assert!(native.installation.node.is_none());
    assert_eq!(
        plan.required("processes")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        plan.required("processes")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .all(|process| process
                .required("entrypoint")
                .unwrap()
                .required("executable")
                .unwrap()
                .as_str()
                .unwrap()
                .starts_with("dist/native/"))
    );
    assert!(!native.root.join("state/bootstrap").exists());
    native.clean().await;

    let mut legacy = Fixture::new().await;
    legacy.installation.node = None;
    let error = legacy
        .installation
        .plan(
            legacy.path.clone(),
            legacy
                .candidate
                .required("candidateId")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
            None,
            &[],
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "unsupported_runtime");
    legacy.clean().await;
}
