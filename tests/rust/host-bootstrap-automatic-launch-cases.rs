use super::ServiceFixture;
use ivy_contracts::{IvyError, IvyJson as J, IvyResult, JsString, hash::digest};
use ivy_host_runtime::{
    bootstrap_automatic::{AutomaticBootstrapRuntime, NativeAutomaticBootstrapRuntime},
    bootstrap_maintenance::BootstrapMaintenance,
    bootstrap_os::{BootstrapOs, SystemdBootstrapOs},
    executor::ExecutorFuture,
    linux_host::{FileMetadata, LinuxHost},
    node_path,
    node_runtime::NodeRuntime,
    systemd::NativeSystemdHost,
};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

type HostFuture<T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'static>>;

fn p(path: &Path) -> J {
    J::String(node_path::from_native(path))
}
fn node() -> NodeRuntime {
    NodeRuntime {
        executable: "unused-node".into(),
        version: "24.18.0".into(),
    }
}
fn target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "win32-x64",
        ("windows", "aarch64") => "win32-arm64",
        ("linux", "x86_64") => "linux-x64",
        ("linux", "aarch64") => "linux-arm64",
        _ => panic!("Unqualified target"),
    }
}
fn plan(f: &ServiceFixture, os: &str) -> J {
    J::object([
        ("schemaVersion", J::Number(1.0)),
        ("hostId", "host".into()),
        ("os", os.into()),
        ("installationId", "ivy-next-0123456789ab".into()),
        ("candidateId", format!("sha256:{}", "a".repeat(64)).into()),
        ("artifactRoot", p(&f.0.join("current package café"))),
        ("configPath", p(&f.0.join("host config.json"))),
        ("configHash", format!("sha256:{}", "b".repeat(64)).into()),
        ("runtimeRoot", p(&f.0.join("runtime"))),
        ("nodeExecutable", p(&f.0.join("Node Work/node.exe"))),
        (
            "processes",
            J::Array(vec![J::object([
                ("instanceId", "host".into()),
                ("componentId", "host-executor".into()),
                ("name", "ivy-next-0123456789ab-111111111111".into()),
                (
                    "entrypoint",
                    J::object([
                        ("executable", "node".into()),
                        ("args", J::Array(vec!["dist/main.js".into()])),
                        ("timeoutMs", J::Number(5000.0)),
                    ]),
                ),
            ])]),
        ),
    ])
}
fn artifact(plan: &J) -> PathBuf {
    node_path::to_native(
        plan.required("artifactRoot")
            .unwrap()
            .as_js_string()
            .unwrap(),
    )
}
fn payload(plan: &J, name: &str) -> PathBuf {
    let file = artifact(plan).join("dist/native").join(target()).join(name);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, b"inert path-admission fixture; never executed").unwrap();
    file
}

#[derive(Clone, Debug)]
struct Call {
    file: String,
    args: Vec<JsString>,
    timeout: u32,
    maximum: usize,
}
struct Linux {
    uid: u32,
    state: Option<&'static str>,
    fail_launch: bool,
    blocked: bool,
    calls: Arc<Mutex<Vec<Call>>>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    done: Arc<AtomicBool>,
}
impl Linux {
    fn new(uid: u32, state: Option<&'static str>, fail_launch: bool, blocked: bool) -> Arc<Self> {
        Arc::new(Self {
            uid,
            state,
            fail_launch,
            blocked,
            calls: Default::default(),
            entered: Default::default(),
            release: Default::default(),
            done: Default::default(),
        })
    }
    fn os(self: &Arc<Self>, f: &ServiceFixture) -> SystemdBootstrapOs {
        SystemdBootstrapOs::with_hosts(
            node(),
            Arc::new(NativeSystemdHost::new(f.0.clone())),
            self.clone(),
        )
    }
}
impl LinuxHost for Linux {
    fn platform(&self) -> &str {
        "linux"
    }
    fn user_id(&self) -> Option<u32> {
        Some(self.uid)
    }
    fn metadata(&self, _: JsString) -> HostFuture<io::Result<FileMetadata>> {
        panic!("No system files are touched")
    }
    fn read(&self, _: JsString) -> HostFuture<io::Result<Vec<u8>>> {
        panic!("No system files are read")
    }
    fn create(&self, _: JsString, _: Vec<u8>, _: u32, _: JsString) -> HostFuture<io::Result<()>> {
        panic!("No unit is installed")
    }
    fn command(
        &self,
        file: &'static str,
        args: Vec<JsString>,
        timeout: u32,
        maximum: usize,
    ) -> HostFuture<IvyResult<JsString>> {
        self.calls.lock().unwrap().push(Call {
            file: file.into(),
            args,
            timeout,
            maximum,
        });
        let state = self.state;
        let fail = self.fail_launch;
        let blocked = self.blocked;
        let entered = self.entered.clone();
        let release = self.release.clone();
        let done = self.done.clone();
        Box::pin(async move {
            if file == "/usr/bin/systemctl" {
                return state.map(JsString::from).ok_or_else(IvyError::internal);
            }
            assert_eq!(file, "/usr/bin/systemd-run");
            if blocked {
                entered.notify_one();
                release.notified().await;
            }
            done.store(true, Ordering::SeqCst);
            if fail {
                Err(IvyError::new("fixture_exit", "injected command failure"))
            } else {
                Ok("".into())
            }
        })
    }
}

#[tokio::test]
async fn systemd_native_launch_recipe_active_replay_and_admin_gate() {
    let f = ServiceFixture::new();
    let mut plan = plan(&f, "linux");
    plan.insert(
        "processes",
        J::Array(vec![J::object([
            ("instanceId", "host-executor".into()),
            ("componentId", "host-executor".into()),
            ("name", "fixture-host".into()),
        ])]),
    );
    let worker = payload(&plan, "ivy-bootstrap-auto-worker");
    let request = f.0.join("runtime/bootstrap-automatic/current/request.json");
    let operation = "bootstrap-package-original";
    let expected_unit = format!(
        "ivy-next-0123456789ab-maintenance-{}",
        &digest(operation.as_bytes())[7..19]
    );
    let host = Linux::new(0, None, false, false);
    host.os(&f)
        .launch_automatic(plan.clone(), request.clone(), operation.into())
        .unwrap()
        .await
        .unwrap();
    let calls = host.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].file, "/usr/bin/systemctl");
    assert_eq!((calls[0].timeout, calls[0].maximum), (10_000, 65_536));
    assert_eq!(
        calls[0].args,
        vec![
            "show".into(),
            format!("{expected_unit}.service").into(),
            "--property=LoadState,ActiveState".into()
        ]
    );
    assert_eq!(calls[1].file, "/usr/bin/systemd-run");
    assert_eq!(
        (calls[1].timeout, calls[1].maximum),
        (60_000, 2 * 1024 * 1024)
    );
    let actual_worker = ivy_host_runtime::paths::realpath(&worker).unwrap();
    assert_eq!(
        calls[1].args,
        vec![
            "--unit".into(),
            expected_unit.into(),
            "--collect".into(),
            "--property=Type=exec".into(),
            "--property=RuntimeMaxSec=15min".into(),
            "--".into(),
            node_path::from_native(&actual_worker),
            "--config".into(),
            plan.required("configPath")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
            "--request".into(),
            node_path::from_native(&request),
            "--distribution".into(),
            plan.required("artifactRoot")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
            "--node-executable".into(),
            plan.required("nodeExecutable")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone()
        ]
    );
    let mut native_plan = plan.clone();
    let mut native_processes = native_plan
        .required("processes")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    for process in &mut native_processes {
        process.insert(
            "entrypoint",
            J::object([
                (
                    "executable",
                    "dist/native/linux-x64/ivy-host-executor".into(),
                ),
                ("args", J::Array(vec![])),
                ("timeoutMs", J::Number(30000.0)),
            ]),
        );
    }
    native_plan.insert("processes", J::Array(native_processes));
    native_plan.remove("nodeExecutable");
    let native_host = Linux::new(0, None, false, false);
    native_host
        .os(&f)
        .launch_automatic(native_plan, request.clone(), "native-no-node".into())
        .unwrap()
        .await
        .unwrap();
    assert!(
        !native_host.calls.lock().unwrap()[1]
            .args
            .contains(&"--node-executable".into())
    );
    fs::remove_file(worker).unwrap();
    for state in [
        "LoadState=loaded\nActiveState=active\n",
        "LoadState=loaded\nActiveState=activating\n",
    ] {
        let host = Linux::new(0, Some(state), false, false);
        host.os(&f)
            .launch_automatic(plan.clone(), request.clone(), operation.into())
            .unwrap()
            .await
            .unwrap();
        assert_eq!(host.calls.lock().unwrap().len(), 1);
    }
    let host = Linux::new(1000, None, false, false);
    let error = host
        .os(&f)
        .launch_automatic(plan, request, operation.into())
        .unwrap()
        .await
        .unwrap_err();
    assert_eq!(error.code, "unsupported_runtime");
    assert!(host.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn systemd_missing_payload_and_unknown_command_failure_stay_distinct() {
    let f = ServiceFixture::new();
    let plan = plan(&f, "linux");
    let request = f.0.join("request.json");
    let host = Linux::new(
        0,
        Some("LoadState=not-found\nActiveState=inactive\n"),
        false,
        false,
    );
    let error = host
        .os(&f)
        .launch_automatic(plan.clone(), request.clone(), "same-id".into())
        .unwrap()
        .await
        .unwrap_err();
    assert_eq!(error.code, "artifact_missing");
    assert_eq!(error.outcome, ivy_host_runtime::Outcome::NotExecuted);
    assert_eq!(host.calls.lock().unwrap().len(), 1);
    payload(&plan, "ivy-bootstrap-auto-worker");
    let host = Linux::new(0, None, true, false);
    let error = host
        .os(&f)
        .launch_automatic(plan, request, "same-id".into())
        .unwrap()
        .await
        .unwrap_err();
    assert_eq!(error.code, "bootstrap_os_unavailable");
    assert_eq!(error.outcome, ivy_host_runtime::Outcome::Unknown);
    assert_eq!(host.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn dropped_systemd_launch_observer_keeps_the_admitted_command() {
    let f = ServiceFixture::new();
    let plan = plan(&f, "linux");
    payload(&plan, "ivy-bootstrap-auto-worker");
    let host = Linux::new(0, None, false, true);
    let os = host.os(&f);
    let launch = os
        .launch_automatic(plan, f.0.join("request.json"), "retained-op".into())
        .unwrap();
    // The returned owner is admitted already; dropping it without polling must
    // preserve the command just as dropping a response waiter does.
    drop(launch);
    tokio::time::timeout(Duration::from_secs(5), host.entered.notified())
        .await
        .unwrap();
    host.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !host.done.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(host.calls.lock().unwrap().len(), 2);
}

struct LaunchOs;
impl BootstrapOs for LaunchOs {
    fn inspect(&self, _: Vec<J>) -> ExecutorFuture<'_, Vec<J>> {
        panic!("Missing config fails in maintenance before OS inspection")
    }
    fn render(&self, _: J, _: J) -> ExecutorFuture<'_, J> {
        unreachable!()
    }
    fn check(&self, _: J) -> ExecutorFuture<'_, ()> {
        unreachable!()
    }
    fn apply(&self, _: J, _: J, _: Vec<J>) -> ExecutorFuture<'_, ()> {
        unreachable!()
    }
    fn reload(&self) -> ExecutorFuture<'_, ()> {
        unreachable!()
    }
    fn launch_automatic(
        &self,
        plan: J,
        request: PathBuf,
        operation: String,
    ) -> Option<ExecutorFuture<'_, ()>> {
        assert_eq!(plan.get("hostId"), Some(&"host".into()));
        assert!(request.ends_with("request.json"));
        assert_eq!(operation, "exact-id");
        Some(Box::pin(async {
            Err(IvyError::new("owner_error", "retained owner error")
                .with_outcome(ivy_host_runtime::Outcome::Unknown)
                .with_details(J::object([("marker", "exact".into())])))
        }))
    }
}
#[tokio::test]
async fn native_adapter_uses_maintenance_inspection_and_preserves_os_error() {
    let f = ServiceFixture::new();
    let maintenance = BootstrapMaintenance::new(
        node(),
        Arc::new(|a, b| a.0.cmp(&b.0)),
        Arc::new(LaunchOs),
        f.0.join("unused-job"),
    );
    let runtime = NativeAutomaticBootstrapRuntime(maintenance);
    let error = runtime
        .inspect(f.0.join("absent-config.json"), f.0.clone())
        .await
        .unwrap_err();
    assert_eq!(error.code, "internal_error");
    let error = runtime
        .launch_automatic(
            plan(&f, "linux"),
            f.0.join("request.json"),
            "exact-id".into(),
        )
        .unwrap()
        .await
        .unwrap_err();
    assert_eq!(error.code, "owner_error");
    assert_eq!(error.outcome, ivy_host_runtime::Outcome::Unknown);
    assert_eq!(error.details, Some(J::object([("marker", "exact".into())])));
}

#[cfg(windows)]
#[tokio::test]
async fn native_package_factory_refuses_a_legacy_only_worker_before_scheduler_access() {
    struct Packages;
    impl ivy_host_runtime::package_updater::PackageClient for Packages {
        fn catalog(&self, _: String, _: String, _: f64) -> ExecutorFuture<'_, Option<J>> {
            panic!("No package request is made")
        }
        fn download(&self, _: String, _: String, _: J, _: PathBuf) -> ExecutorFuture<'_, ()> {
            panic!("No artifact is downloaded")
        }
    }
    let f = ServiceFixture::new();
    let plan = plan(&f, "win32");
    let artifact = artifact(&plan);
    fs::create_dir_all(artifact.join("dist/packages/host-runtime/src")).unwrap();
    fs::write(
        artifact.join("dist/packages/host-runtime/src/bootstrap-auto-worker.js"),
        b"inert legacy file",
    )
    .unwrap();
    fs::create_dir_all(artifact.join("dist/native")).unwrap();
    fs::write(
        artifact.join("dist/native/ivy-host-job.exe"),
        b"inert legacy helper",
    )
    .unwrap();
    let runtime = ivy_host_runtime::executor::PackageUpdateRuntime::native(
        node(),
        Arc::new(Packages),
        Arc::new(|a, b| a.0.cmp(&b.0)),
        artifact,
        f.0.join("unused-job.exe"),
    )
    .unwrap();
    let error = runtime
        .automatic
        .launch_automatic(plan, f.0.join("request.json"), "same-op".into())
        .unwrap()
        .await
        .unwrap_err();
    assert_eq!(error.code, "artifact_missing");
    assert_eq!(error.outcome, ivy_host_runtime::Outcome::NotExecuted);
}

#[cfg(windows)]
#[test]
fn scheduler_native_and_legacy_recipes_use_the_actual_powershell_function() {
    use std::{
        io::Write,
        os::windows::process::CommandExt,
        process::{Command, Stdio},
    };
    let f = ServiceFixture::new();
    let plan = plan(&f, "win32");
    let root = artifact(&plan);
    let x64 = root.join("dist/native/win32-x64");
    let arm64 = root.join("dist/native/win32-arm64");
    for target in [&x64, &arm64] {
        fs::create_dir_all(target).unwrap();
        for name in ["ivy-bootstrap-auto-worker.exe", "ivy-host-job.exe"] {
            fs::write(target.join(name), b"inert file").unwrap();
        }
    }
    fs::write(
        root.join("dist/native/ivy-host-job.exe"),
        b"legacy inert helper",
    )
    .unwrap();
    fs::create_dir_all(root.join("dist/packages/host-runtime/src")).unwrap();
    fs::write(
        root.join("dist/packages/host-runtime/src/bootstrap-auto-worker.js"),
        b"inert legacy path",
    )
    .unwrap();
    let request = f.0.join("runtime/request.json");
    let data = |worker: PathBuf, launcher: PathBuf| {
        J::object([
            ("requestPath", p(&request)),
            ("workerExecutable", p(&worker)),
            ("hostJobLauncher", p(&launcher)),
        ])
    };
    let mut cases = vec![J::object([
        ("plan", plan.clone()),
        ("data", J::object([("requestPath", p(&request))])),
        ("ok", true.into()),
    ])];
    for target in [&x64, &arm64] {
        cases.push(J::object([
            ("plan", plan.clone()),
            (
                "data",
                data(
                    target.join("ivy-bootstrap-auto-worker.exe"),
                    target.join("ivy-host-job.exe"),
                ),
            ),
            ("ok", true.into()),
        ]));
    }
    for native in [
        data(
            f.0.join("ivy-bootstrap-auto-worker.exe"),
            x64.join("ivy-host-job.exe"),
        ),
        data(
            x64.join("ivy-bootstrap-auto-worker.exe"),
            arm64.join("ivy-host-job.exe"),
        ),
        data(x64.join("other.exe"), x64.join("ivy-host-job.exe")),
        J::object([
            ("requestPath", p(&request)),
            (
                "workerExecutable",
                p(&x64.join("ivy-bootstrap-auto-worker.exe")),
            ),
        ]),
    ] {
        cases.push(J::object([
            ("plan", plan.clone()),
            ("data", native),
            ("ok", false.into()),
        ]));
    }
    let mut native_data = data(
        x64.join("ivy-bootstrap-auto-worker.exe"),
        x64.join("ivy-host-job.exe"),
    );
    native_data.insert("nodeRequired", false.into());
    let mut native_plan = plan.clone();
    native_plan.remove("nodeExecutable");
    let mut native_processes = native_plan
        .required("processes")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    for process in &mut native_processes {
        process.insert(
            "entrypoint",
            J::object([
                (
                    "executable",
                    "dist/native/win32-x64/ivy-host-executor.exe".into(),
                ),
                ("args", J::Array(vec![])),
                ("timeoutMs", J::Number(30000.0)),
            ]),
        );
    }
    native_plan.insert("processes", J::Array(native_processes));
    cases.push(J::object([
        ("plan", native_plan),
        ("data", native_data.clone()),
        ("ok", true.into()),
    ]));
    let mut missing_node = plan.clone();
    missing_node.remove("nodeExecutable");
    cases.push(J::object([
        ("plan", missing_node),
        ("data", native_data),
        ("ok", false.into()),
    ]));
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/host-bootstrap-automatic-launch.ps1");
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../packages/host-runtime/assets/bootstrap-maintenance-windows.ps1");
    let powershell =
        PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let mut child = Command::new(powershell)
        .creation_flags(0x08000000)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(script)
        .arg(source)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            ivy_contracts::json::stringify(&J::Array(cases.clone()), 1024 * 1024)
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows =
        ivy_contracts::json::parse(std::str::from_utf8(&output.stdout).unwrap(), 1024 * 1024)
            .unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 9);
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(row.get("ok"), cases[index].get("ok"), "case {index}");
        if index == 7 {
            let args = row
                .required("value")
                .unwrap()
                .required("args")
                .unwrap()
                .as_array()
                .unwrap();
            assert!(!args.contains(&"--node-executable".into()));
            assert!(args.contains(&"--distribution".into()));
        }
        if index < 3 {
            let target = if index == 2 { &arm64 } else { &x64 };
            let launcher = if index == 0 {
                root.join("dist/native/ivy-host-job.exe")
            } else {
                target.join("ivy-host-job.exe")
            };
            assert_eq!(
                row.required("value").unwrap().get("launcher"),
                Some(&p(&ivy_host_runtime::paths::absolute(&launcher).unwrap()))
            );
            let mut args = vec![J::from("--parent"), "0".into()];
            if index == 0 {
                args.extend([
                    plan.required("nodeExecutable").unwrap().clone(),
                    p(&ivy_host_runtime::paths::absolute(
                        &root.join("dist/packages/host-runtime/src/bootstrap-auto-worker.js"),
                    )
                    .unwrap()),
                ]);
            } else {
                args.push(p(&ivy_host_runtime::paths::absolute(
                    &target.join("ivy-bootstrap-auto-worker.exe"),
                )
                .unwrap()));
            }
            args.extend([
                "--config".into(),
                plan.required("configPath").unwrap().clone(),
                "--request".into(),
                p(&request),
                "--distribution".into(),
                p(&root),
            ]);
            if index != 0 {
                args.extend([
                    "--node-executable".into(),
                    plan.required("nodeExecutable").unwrap().clone(),
                ]);
            }
            assert_eq!(
                row.required("value").unwrap().get("args"),
                Some(&J::Array(args))
            );
        }
    }
}
