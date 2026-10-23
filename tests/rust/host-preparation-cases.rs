use super::{ServiceFixture,host_configuration,assert_json_eq};
use ivy_contracts::{IvyJson,IvyResult,JsString,json,hash::hash_json};
use ivy_host_runtime::{config,preparations,preparation_progress::{self,PreparationProgress,StepOptions},dependency_cache,executor_lock::ExecutorLock,journal::HostJournal,native_path};
use ivy_sdk::clock::Clock;
use std::{fs,io::Write,path::{Path,PathBuf},process::{Command,Stdio},sync::{Arc,Mutex,atomic::{AtomicI64,Ordering}},time::{Duration,Instant}};
const AT:i64=1_767_225_600_000;
static CORPUS:Mutex<()>=Mutex::new(());
struct Fixed(AtomicI64);
impl Clock for Fixed{fn utc_millis(&self)->i64{self.0.load(Ordering::SeqCst)}fn monotonic(&self)->Instant{Instant::now()}}
fn text(path:&Path)->IvyJson{IvyJson::String(native_path::from_native(path))}
fn path(value:&IvyJson)->PathBuf{native_path::to_native(value.as_js_string().unwrap())}
fn outcome(result:IvyResult<IvyJson>)->IvyJson{match result{Ok(value)=>IvyJson::object([("ok",true.into()),("value",value)]),Err(error)=>IvyJson::object([("ok",false.into()),("error",error.to_wire())])}}
fn oracle(input:&IvyJson)->IvyJson{
 let mut child=Command::new("node").args(["--experimental-transform-types","--disable-warning=ExperimentalWarning","tests/fixtures/host-preparation-oracle.mjs"]).current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
 .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();child.stdin.take().unwrap().write_all(json::stringify(input,usize::MAX).unwrap().as_bytes()).unwrap();let output=child.wait_with_output().unwrap();assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));json::parse(std::str::from_utf8(&output.stdout).unwrap(),usize::MAX).unwrap()
}
fn compare(input:IvyJson,actual:IvyJson,expected:IvyJson){
 if let Some(file)=std::env::var_os("IVY_HOST_PREPARATION_CORPUS"){let _guard=CORPUS.lock().unwrap();writeln!(fs::OpenOptions::new().create(true).append(true).open(file).unwrap(),"{}",json::stringify(&IvyJson::object([("input",input.clone()),("actual",actual.clone()),("expected",expected.clone())]),usize::MAX).unwrap()).unwrap();}
 assert_json_eq(&actual,&expected);
}
fn write(path:&Path,bytes:&[u8]){fs::create_dir_all(path.parent().unwrap()).unwrap();fs::write(path,bytes).unwrap();}
fn load(path:&Path)->Option<IvyJson>{fs::read_to_string(path).ok().and_then(|s|json::parse(&s,usize::MAX).ok())}
fn opaque_name() -> std::ffi::OsString {
    let mut name = std::ffi::OsString::from("native ü 空 😀; ' ");
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        name.push(std::ffi::OsString::from_wide(&[0xd800]));
    }
    name
}
fn progress_host(fixture: &ServiceFixture) -> IvyJson {
    let mut host = host_configuration(fixture);
    host.insert(
        "runtimeRoot",
        text(&fixture.0.join(opaque_name()).join("runtime")),
    );
    host
}
fn progress_path(host: &IvyJson) -> PathBuf {
    path(host.required("runtimeRoot").unwrap())
        .join("preparation-progress")
        .join(format!(
            "{}.json",
            &hash_json(&IvyJson::object([
                ("componentId", "fixture".into()),
                ("snapshotId", "snapshot".into())
            ]))
            .unwrap()[7..]
        ))
}

#[tokio::test]
async fn progress_writer_preserves_state_bytes_reset_and_failure_isolation() {
    for mode in ["normal", "missing-file", "blocked-root", "utf16"] {
        let fixture = ServiceFixture::new();
        let mut host = progress_host(&fixture);
        let output = fixture.0.join(opaque_name()).join("build-output.tar");
        write(&output, b"fifteen bytes!!");
        if mode == "blocked-root" {
            let root = fixture.0.join("root-file");
            write(&root, b"blocked");
            host.insert("runtimeRoot", text(&root));
        }
        let clock = Arc::new(Fixed(AtomicI64::new(AT)));
        let progress = PreparationProgress::with_clock(
            &host,
            "fixture".into(),
            "snapshot".into(),
            clock.clone(),
        )
        .unwrap();
        let file = if mode == "missing-file" {
            fixture.0.join("missing")
        } else {
            output
        };
        let command = JsString(vec![0x61, 0xd800, 0x62]);
        progress
            .step(
                "archiving".into(),
                StepOptions {
                    command: Some(command.clone()),
                    file_path: Some(native_path::from_native(&file)),
                },
            )
            .await
            .unwrap();
        if mode != "blocked-root" {
            assert_eq!(
                load(&progress_path(&host)).unwrap().get("fileBytes"),
                Some(&if mode == "missing-file" {
                    IvyJson::Null
                } else {
                    IvyJson::Number(15.0)
                })
            );
        }
        clock.0.store(AT + 25, Ordering::SeqCst);
        progress.output(&JsString(vec![0x61, 0xd800, 0x62, 0xd83d, 0xde00]));
        let complete = mode == "normal";
        if complete {
            clock.0.store(AT + 40, Ordering::SeqCst);
            progress
                .step("complete".into(), StepOptions::default())
                .await
                .unwrap();
        }
        progress.close().await.unwrap();
        let actual = load(&progress_path(&host));
        if mode == "blocked-root" {
            assert!(actual.is_none());
            assert_eq!(
                fs::read(path(host.required("runtimeRoot").unwrap())).unwrap(),
                b"blocked"
            );
            continue;
        }
        let expected = IvyJson::object([
            ("componentId", "fixture".into()),
            ("snapshotId", "snapshot".into()),
            (
                "step",
                if complete { "complete" } else { "archiving" }.into(),
            ),
            ("startedAt", "2026-01-01T00:00:00.000Z".into()),
            (
                "stepStartedAt",
                if complete {
                    "2026-01-01T00:00:00.040Z"
                } else {
                    "2026-01-01T00:00:00.000Z"
                }
                .into(),
            ),
            (
                "observedAt",
                if complete {
                    "2026-01-01T00:00:00.040Z"
                } else {
                    "2026-01-01T00:00:00.025Z"
                }
                .into(),
            ),
            (
                "lastOutputAt",
                if complete {
                    IvyJson::Null
                } else {
                    "2026-01-01T00:00:00.025Z".into()
                },
            ),
            (
                "outputBytes",
                IvyJson::Number(if complete { 0.0 } else { 9.0 }),
            ),
            ("workerPid", IvyJson::Number(std::process::id() as f64)),
            (
                "command",
                if complete {
                    IvyJson::Null
                } else {
                    IvyJson::String(command)
                },
            ),
            (
                "filePath",
                if complete { IvyJson::Null } else { text(&file) },
            ),
            (
                "fileBytes",
                if complete || mode == "missing-file" {
                    IvyJson::Null
                } else {
                    IvyJson::Number(15.0)
                },
            ),
        ]);
        assert_json_eq(&actual.unwrap(), &expected);
    }
}

#[tokio::test]
async fn progress_reader_preserves_native_dates_identity_staleness_and_extra_json() {
    let f = ServiceFixture::new();
    let host = progress_host(&f);
    let base = IvyJson::object([
        ("componentId", "fixture".into()),
        ("snapshotId", "snapshot".into()),
        ("startedAt", "2026-01-01T00:00:00.000Z".into()),
        ("stepStartedAt", "2026-01-01T00:00:00.010Z".into()),
        ("observedAt", "2026-01-01T00:00:00.020Z".into()),
        ("step", "building".into()),
        ("extra", IvyJson::String(JsString(vec![0xd800]))),
    ]);
    for mode in [
        "valid",
        "future",
        "old-attempt",
        "component",
        "snapshot",
        "bad-date",
        "missing-date",
        "array-date",
        "throwing-date",
        "phase",
        "no-snapshot",
        "malformed",
        "large",
        "primitive",
    ] {
        let mut value = base.clone();
        let mut entry = IvyJson::object([
            (
                "sourceSnapshot",
                IvyJson::object([("snapshotId", "snapshot".into())]),
            ),
            (
                "record",
                IvyJson::object([
                    ("componentId", "fixture".into()),
                    ("phase", "preparing".into()),
                    ("createdAt", "2026-01-01T00:00:00.000Z".into()),
                ]),
            ),
        ]);
        let mut raw = None;
        match mode {
            "future" => {
                for key in ["startedAt", "stepStartedAt", "observedAt"] {
                    value.insert(key, "2027-01-01T00:00:00.000Z".into());
                }
            }
            "old-attempt" => value.insert("startedAt", "2025-12-31T23:59:59.999Z".into()),
            "component" => value.insert("componentId", "other".into()),
            "snapshot" => value.insert("snapshotId", "other".into()),
            "bad-date" => value.insert("observedAt", "invalid".into()),
            "missing-date" => {
                value.remove("startedAt");
            }
            "array-date" => value.insert(
                "startedAt",
                IvyJson::Array(vec!["2026-01-01T00:00:00.000Z".into()]),
            ),
            "throwing-date" => value.insert(
                "startedAt",
                IvyJson::object([("toString", "blocked".into())]),
            ),
            "phase" => {
                let mut record = entry.required("record").unwrap().clone();
                record.insert("phase", "prepared".into());
                entry.insert("record", record);
            }
            "no-snapshot" => {
                entry.remove("sourceSnapshot");
            }
            "malformed" => raw = Some("{".to_owned()),
            "large" => raw = Some(format!("\"{}\"", "x".repeat(16385))),
            "primitive" => value = "text".into(),
            _ => {}
        }
        let file = progress_path(&host);
        write(
            &file,
            raw.clone()
                .unwrap_or_else(|| json::stringify(&value, usize::MAX).unwrap())
                .as_bytes(),
        );
        let actual = preparation_progress::preparation_progress_with_clock(
            &host,
            &entry,
            &Fixed(AtomicI64::new(AT + 100)),
        )
        .await
        .unwrap();
        if mode == "valid" || mode == "future" {
            let mut expected = value;
            expected.insert(
                "elapsedMs",
                IvyJson::Number(if mode == "future" { 0.0 } else { 100.0 }),
            );
            expected.insert(
                "stepElapsedMs",
                IvyJson::Number(if mode == "future" { 0.0 } else { 90.0 }),
            );
            expected.insert(
                "heartbeatAgeMs",
                IvyJson::Number(if mode == "future" { 0.0 } else { 80.0 }),
            );
            assert_json_eq(&actual.unwrap(), &expected);
        } else {
            assert!(actual.is_none(), "{mode}: {actual:?}");
        }
    }
}

#[tokio::test]
async fn progress_heartbeat_and_drop_stop_background_admission() {
    let f = ServiceFixture::new();
    let host = progress_host(&f);
    let clock = Arc::new(Fixed(AtomicI64::new(AT)));
    let progress =
        PreparationProgress::with_clock(&host, "fixture".into(), "snapshot".into(), clock.clone())
            .unwrap();
    progress
        .step("building".into(), StepOptions::default())
        .await
        .unwrap();
    clock.0.store(AT + 5000, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(15), async {
        while load(&progress_path(&host)).and_then(|value| value.get("observedAt").cloned())
            != Some("2026-01-01T00:00:05.000Z".into())
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    progress.close().await.unwrap();
    drop(progress);
    fs::remove_file(progress_path(&host)).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!progress_path(&host).exists());
}

#[cfg(windows)]
#[tokio::test]
async fn dropped_progress_waiter_cannot_reorder_the_active_writer() {
    use std::os::windows::fs::OpenOptionsExt;
    let f = ServiceFixture::new();
    let host = progress_host(&f);
    let progress =
        Arc::new(PreparationProgress::new(&host, "fixture".into(), "snapshot".into()).unwrap());
    progress
        .step("initial".into(), StepOptions::default())
        .await
        .unwrap();
    let file = progress_path(&host);
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&file)
        .unwrap();
    let pending = progress.clone();
    let first = tokio::spawn(async move {
        pending
            .step("cancelled-waiter".into(), StepOptions::default())
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if fs::read_dir(file.parent().unwrap())
                .unwrap()
                .any(|e| e.unwrap().file_name().to_string_lossy().ends_with(".tmp"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    first.abort();
    let _ = first.await;
    let pending = progress.clone();
    let mut second =
        tokio::spawn(async move { pending.step("last".into(), StepOptions::default()).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut second)
            .await
            .is_err()
    );
    drop(held);
    second.await.unwrap().unwrap();
    progress.close().await.unwrap();
    assert_eq!(load(&file).unwrap().get("step"), Some(&"last".into()));
}

fn build() -> String {
    format!("sha256:{}", "a".repeat(64))
}
struct Prepared {
    host: IvyJson,
    root: PathBuf,
    snapshot_file: PathBuf,
    outside_file: PathBuf,
}
impl Prepared {
    async fn new(parent: &ServiceFixture, name: &str, mode: &str) -> Self {
        // SQLite-backed lock files use Unicode paths; native record and
        // artifact I/O exercise surrogate names independently.
        let base = parent.0.join(name).join("native ü 空 😀; '");
        fs::create_dir_all(&base).unwrap();
        let base = fs::canonicalize(base).unwrap();
        let mut host = host_configuration(parent);
        for (key, name) in [("artifactRoot", "artifacts"), ("stagingRoot", "staging")] {
            let root = base.join(name);
            host.insert(
                key,
                text(&if key == "artifactRoot" {
                    root.join(opaque_name())
                } else {
                    root
                }),
            );
        }
        let staging = path(host.required("stagingRoot").unwrap());
        let root = staging.join(format!(
            "build-{}",
            preparations::preparation_id_for_build(&build()).unwrap()
        ));
        fs::create_dir_all(&root).unwrap();
        let snapshot_root = staging.join("sources").join("snapshot");
        let source = snapshot_root.join("source");
        let manifest_path = snapshot_root.join("snapshot.json");
        let snapshot_file = source.join("kept.txt");
        write(&snapshot_file, b"authoritative snapshot");
        let snapshot = IvyJson::object([
            ("snapshotId", "snapshot".into()),
            ("sourceRoot", text(&source)),
            ("originalRoot", text(&base.join("checkout"))),
            ("manifestPath", text(&manifest_path)),
            ("capturedAt", "2026-01-01T00:00:00.000Z".into()),
        ]);
        config::atomic_json(
            &manifest_path,
            IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                ("snapshot", snapshot.clone()),
            ]),
        )
        .await
        .unwrap();
        let mut plan = super::source_cases::build_plan();
        plan.insert("checks", IvyJson::Array(vec![]));
        for (key, executable) in [
            ("entrypoint", "dist/native/ivy-fixture"),
            ("readiness", "dist/native/ivy-health"),
        ] {
            let command = IvyJson::object([
                ("executable", executable.into()),
                ("args", IvyJson::Array(vec![])),
                ("timeoutMs", IvyJson::Number(5000.0)),
            ]);
            if key == "entrypoint" {
                plan.insert(key, command);
            } else {
                let mut readiness = plan.required(key).unwrap().clone();
                readiness.insert("command", command);
                plan.insert(key, readiness);
            }
        }
        config::atomic_json(source.join("services/fixture/deploy.json"), plan)
            .await
            .unwrap();
        let mut plan = ivy_host_runtime::source::source_build_plan(source.clone(), "fixture")
            .await
            .unwrap();
        let destination = path(host.required("artifactRoot").unwrap())
            .join("candidates")
            .join(&build()[7..]);
        let artifact = destination.join("artifact");
        let candidate = IvyJson::object([
            ("candidateId", build().into()),
            ("buildId", build().into()),
            ("componentId", "fixture".into()),
            ("artifactRoot", text(&artifact)),
            ("manifestPath", text(&destination.join("component.json"))),
            ("archivePath", text(&destination.join("package.tar.gz"))),
            ("archiveHash", format!("sha256:{}", "b".repeat(64)).into()),
            ("createdAt", "2026-01-01T00:00:00.000Z".into()),
            (
                "platform",
                IvyJson::object([
                    ("os", if cfg!(windows) { "win32" } else { "linux" }.into()),
                    ("arch", "x64".into()),
                    ("node", "v24.18.0".into()),
                ]),
            ),
        ]);
        let mut manifest = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("buildId", build().into()),
        ]);
        for key in [
            "componentId",
            "kind",
            "version",
            "connectsToHive",
            "requirements",
            "entrypoint",
            "readiness",
            "shutdown",
            "restart",
            "storage",
            "runtimeReset",
        ] {
            if let Some(value) = plan.get(key) {
                manifest.insert(key, value.clone());
            }
        }
        config::atomic_json(destination.join("component.json"), manifest.clone())
            .await
            .unwrap();
        config::atomic_json(
            artifact.join("dist/build-info.json"),
            IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                ("buildId", build().into()),
                ("version", plan.required("version").unwrap().clone()),
                ("componentId", "fixture".into()),
            ]),
        )
        .await
        .unwrap();
        let entry = plan
            .required("entrypoint")
            .unwrap()
            .required("executable")
            .unwrap()
            .as_str()
            .unwrap();
        let health = plan
            .required("readiness")
            .unwrap()
            .required("command")
            .unwrap()
            .required("executable")
            .unwrap()
            .as_str()
            .unwrap();
        write(&artifact.join(&entry), b"fixture native entrypoint");
        write(&artifact.join(health), b"fixture native health");
        config::atomic_json(
            artifact
                .join(Path::new(&entry).parent().unwrap())
                .join("build-info.json"),
            load(&artifact.join("dist/build-info.json")).unwrap(),
        )
        .await
        .unwrap();
        write(&destination.join("package.tar.gz"), b"archive");
        write(&root.join("source/duplicate"), b"source duplicate");
        write(&root.join("artifact/duplicate"), b"artifact duplicate");
        write(&root.join("artifact.tar.gz"), b"temporary archive");
        let phase = match mode {
            "publishing" | "corrupt-publication" | "registered-cleanup-failure" => "publishing",
            "building" | "wrong-build" => "building",
            "failed" => "failed",
            "unknown" | "unknown-empty" => "unknown",
            "compacted" | "lost-registration" | "missing-compacted-source" => "compacted",
            _ => "verified",
        };
        let mut record = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            (
                "preparationId",
                preparations::preparation_id_for_build(&build())
                    .unwrap()
                    .into(),
            ),
            ("componentId", "fixture".into()),
            ("buildId", build().into()),
            ("snapshot", snapshot.clone()),
            ("phase", phase.into()),
            (
                "candidate",
                if ["building", "wrong-build", "unknown-empty"].contains(&mode) {
                    IvyJson::Null
                } else {
                    candidate.clone()
                },
            ),
            ("startedAt", "2026-01-01T00:00:00.000Z".into()),
            ("updatedAt", "2026-01-01T00:00:00.000Z".into()),
            (
                "errorCode",
                if ["failed", "unknown", "unknown-empty"].contains(&mode) {
                    "prior_failure".into()
                } else {
                    IvyJson::Null
                },
            ),
        ]);
        let outside_file = base.join("outside/keep");
        write(&outside_file, b"outside survives");
        if [
            "registered",
            "compacted",
            "missing-compacted-source",
            "candidate-conflict",
            "registered-cleanup-failure",
        ]
        .contains(&mode)
        {
            let journal = HostJournal::open(host.clone()).await.unwrap();
            let mut registered = candidate.clone();
            if mode == "candidate-conflict" {
                registered.insert("createdAt", "2026-01-02T00:00:00.000Z".into());
            }
            journal
                .save_candidate(registered, manifest.clone())
                .await
                .unwrap();
            journal.close().await.unwrap();
        }
        if phase == "compacted" {
            for name in ["source", "artifact"] {
                fs::remove_dir_all(root.join(name)).unwrap();
            }
            fs::remove_file(root.join("artifact.tar.gz")).unwrap();
        }
        match mode {
            "missing-compacted-source" => fs::remove_dir_all(&source).unwrap(),
            "manifest-mismatch" => {
                manifest.insert("version", "9.0.0".into());
                config::atomic_json(destination.join("component.json"), manifest)
                    .await
                    .unwrap();
            }
            "checks" => {
                plan.insert(
                    "checks",
                    IvyJson::Array(vec![IvyJson::object([
                        ("executable", "node".into()),
                        ("args", IvyJson::Array(vec!["check".into()])),
                        ("timeoutMs", IvyJson::Number(1000.0)),
                    ])]),
                );
                config::atomic_json(source.join("services/fixture/deploy.json"), plan.clone())
                    .await
                    .unwrap();
            }
            "candidate-path" => {
                let mut candidate = candidate.clone();
                candidate.insert("artifactRoot", text(&root.join("artifact")));
                record.insert("candidate", candidate);
            }
            "linked-temp" | "registered-cleanup-failure" => {
                fs::remove_dir_all(root.join("source")).unwrap();
                #[cfg(windows)]
                std::os::windows::fs::symlink_dir(base.join("outside"), root.join("source"))
                    .unwrap();
                #[cfg(unix)]
                std::os::unix::fs::symlink(base.join("outside"), root.join("source")).unwrap();
            }
            "read-only" => {
                let file = root.join("source/duplicate");
                let mut permissions = fs::metadata(&file).unwrap().permissions();
                permissions.set_readonly(true);
                fs::set_permissions(file, permissions).unwrap();
            }
            "corrupt-publication" => write(&artifact.join("dist/build-info.json"), b"{}"),
            "wrong-build" => record.insert("buildId", format!("sha256:{}", "c".repeat(64)).into()),
            _ => {}
        }
        if mode != "unrecognized" {
            config::atomic_json(root.join("preparation.json"), record)
                .await
                .unwrap();
        }
        if mode == "malformed" {
            write(&root.join("preparation.json"), b"{");
        }
        if mode == "missing" {
            fs::remove_dir_all(&root).unwrap();
        }
        Self {
            host,
            root,
            snapshot_file,
            outside_file,
        }
    }
    async fn observation(&self, operation: IvyResult<IvyJson>) -> IvyJson {
        let journal = HostJournal::open(self.host.clone()).await.unwrap();
        let candidate = journal
            .candidate(&build())
            .await
            .ok()
            .unwrap_or(IvyJson::Null);
        journal.close().await.unwrap();
        let entries = fs::read_dir(&self.root)
            .ok()
            .map(|values| {
                let mut values = values
                    .map(|v| v.unwrap().file_name().to_string_lossy().to_string())
                    .collect::<Vec<_>>();
                values.sort();
                IvyJson::Array(values.into_iter().map(IvyJson::from).collect())
            })
            .unwrap_or(IvyJson::Null);
        IvyJson::object([
            ("operation", outcome(operation)),
            (
                "record",
                load(&self.root.join("preparation.json")).unwrap_or(IvyJson::Null),
            ),
            (
                "before",
                load(&self.root.join("preparation-before-recovery.json")).unwrap_or(IvyJson::Null),
            ),
            ("entries", entries),
            ("candidate", candidate),
            (
                "snapshotBytes",
                fs::read_to_string(&self.snapshot_file)
                    .ok()
                    .map(IvyJson::from)
                    .unwrap_or(IvyJson::Null),
            ),
            (
                "outsideBytes",
                fs::read_to_string(&self.outside_file)
                    .ok()
                    .map(IvyJson::from)
                    .unwrap_or(IvyJson::Null),
            ),
        ])
    }
}
#[tokio::test]
async fn compaction_preserves_native_publication_recovery_retention_and_safe_cleanup() {
    for mode in [
        "verified",
        "publishing",
        "registered",
        "unknown",
        "failed",
        "building",
        "unknown-empty",
        "compacted",
        "missing-compacted-source",
        "lost-registration",
        "candidate-conflict",
        "manifest-mismatch",
        "checks",
        "candidate-path",
        "linked-temp",
        "read-only",
        "unrecognized",
        "malformed",
        "only-filter",
        "busy",
    ] {
        let f = ServiceFixture::new();
        let native = Prepared::new(&f, "native", mode).await;
        let mut owners = Vec::new();
        if mode == "busy" {
            for prepared in [&native] {
                owners.push(
                    ExecutorLock::acquire(
                        path(prepared.host.required("stagingRoot").unwrap())
                            .join("build-locks")
                            .join(&build()[7..]),
                        false,
                    )
                    .await
                    .unwrap(),
                );
            }
        }
        let filter = (mode == "only-filter").then(|| format!("sha256:{}", "f".repeat(64)));
        let result = preparations::compact_preparations(
            native.host.clone(),
            filter.clone(),
            true,
            None::<ivy_host_runtime::node_runtime::NodeRuntime>,
        )
        .await;
        let actual = native.observation(result).await;
        let entries = actual
            .required("operation")
            .unwrap()
            .required("value")
            .unwrap()
            .required("entries")
            .unwrap()
            .as_array()
            .unwrap();
        if mode == "only-filter" {
            assert!(entries.is_empty());
        } else {
            let expected = match mode {
                "verified" | "publishing" | "unknown" | "lost-registration" | "read-only" => {
                    ("recovered", None)
                }
                "registered" => ("compacted", None),
                "compacted" | "missing-compacted-source" => ("retained", Some("already_compacted")),
                "failed" | "unknown-empty" => ("retained", Some("prior_failure")),
                "building" => ("retained", Some("preparation_interrupted")),
                "candidate-conflict" | "manifest-mismatch" => ("retained", Some("build_mismatch")),
                "checks" => ("retained", Some("checks_required")),
                "candidate-path" | "linked-temp" => ("retained", Some("target_conflict")),
                "unrecognized" => ("retained", Some("preparation_unrecognized")),
                "malformed" => ("retained", Some("internal_error")),
                "busy" => ("busy", Some("executor_already_running")),
                _ => unreachable!(),
            };
            assert_eq!(entries.len(), 1, "{mode}");
            assert_eq!(
                entries[0].get("action"),
                Some(&expected.0.into()),
                "{mode}: {actual:?}"
            );
            assert_eq!(
                entries[0].get("code"),
                Some(&expected.1.map_or(IvyJson::Null, IvyJson::from)),
                "{mode}"
            );
            if expected.1.is_none() {
                assert_eq!(
                    actual.required("record").unwrap().get("phase"),
                    Some(&"compacted".into()),
                    "{mode}"
                );
                assert!(!native.root.join("source").exists());
                assert!(native.snapshot_file.exists());
            }
        }
        if mode == "only-filter"
            || entries.first().is_some_and(|entry| {
                entry
                    .get("code")
                    .is_some_and(|code| !code.is_null() && code != &"already_compacted".into())
            })
        {
            assert_eq!(
                fs::read(native.root.join("artifact/duplicate")).unwrap(),
                b"artifact duplicate",
                "{mode}"
            );
        }
        if mode == "unknown" {
            assert_eq!(
                actual.required("before").unwrap().get("phase"),
                Some(&"unknown".into())
            );
            assert_eq!(
                actual.required("before").unwrap().get("errorCode"),
                Some(&"prior_failure".into())
            );
        }
        assert_eq!(
            fs::read(&native.outside_file).unwrap(),
            b"outside survives",
            "{mode}"
        );
        for owner in owners {
            owner.close().await.unwrap();
        }
    }
}

#[cfg(windows)]
#[tokio::test]
async fn dropped_preparation_waiters_retain_the_original_build_lock_and_publication() {
    use std::os::windows::fs::OpenOptionsExt;
    for operation in ["recover", "compact"] {
        let f = ServiceFixture::new();
        let prepared = Prepared::new(&f, "native", "publishing").await;
        let journal = HostJournal::open(prepared.host.clone()).await.unwrap();
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(prepared.root.join("preparation.json"))
            .unwrap();
        let requested = prepared.host.clone();
        let observer = journal.clone();
        let waiter = tokio::spawn(async move {
            if operation == "recover" {
                preparations::recover_prepared_build(
                    observer,
                    build(),
                    None::<ivy_host_runtime::node_runtime::NodeRuntime>,
                )
                .await
                .map(|_| ())
            } else {
                preparations::compact_preparations(
                    requested,
                    None,
                    false,
                    None::<ivy_host_runtime::node_runtime::NodeRuntime>,
                )
                .await
                .map(|_| ())
            }
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if fs::read_dir(&prepared.root).unwrap().any(|entry| {
                    entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .ends_with(".tmp")
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        waiter.abort();
        let _ = waiter.await;
        let lock = path(prepared.host.required("stagingRoot").unwrap())
            .join("build-locks")
            .join(&build()[7..]);
        assert_eq!(
            ExecutorLock::acquire(lock.clone(), false)
                .await
                .err()
                .unwrap()
                .code,
            "executor_already_running"
        );
        assert_eq!(
            journal.candidate(&build()).await.unwrap().get("buildId"),
            Some(&build().into())
        );
        drop(held);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if load(&prepared.root.join("preparation.json"))
                    .is_some_and(|record| record.get("phase") == Some(&"compacted".into()))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let released = ExecutorLock::acquire(lock, false).await.unwrap();
        released.close().await.unwrap();
        journal.close().await.unwrap();
        assert!(!prepared.root.join("source").exists());
        assert!(prepared.snapshot_file.exists());
        assert_eq!(
            fs::read(&prepared.outside_file).unwrap(),
            b"outside survives"
        );
    }
}
#[tokio::test]
async fn automatic_recovery_uses_only_the_requested_build_and_preserves_unknown_outcomes() {
    for mode in [
        "missing",
        "building",
        "wrong-build",
        "unknown",
        "failed",
        "publishing",
        "corrupt-publication",
        "registered-cleanup-failure",
        "missing-compacted-source",
    ] {
        let f = ServiceFixture::new();
        let native = Prepared::new(&f, "native", mode).await;
        let journal = HostJournal::open(native.host.clone()).await.unwrap();
        let result = preparations::recover_prepared_build(
            journal.clone(),
            build(),
            None::<ivy_host_runtime::node_runtime::NodeRuntime>,
        )
        .await
        .map(|v| v.unwrap_or(IvyJson::Null));
        journal.close().await.unwrap();
        let actual = native.observation(result).await;
        if [
            "publishing",
            "registered-cleanup-failure",
            "missing-compacted-source",
        ]
        .contains(&mode)
        {
            assert_eq!(
                actual.required("operation").unwrap().get("ok"),
                Some(&true.into()),
                "{mode}"
            );
            assert_eq!(
                actual
                    .required("operation")
                    .unwrap()
                    .required("value")
                    .unwrap()
                    .get("buildId"),
                Some(&build().into()),
                "{mode}"
            );
        }
        if mode == "publishing" {
            assert_eq!(
                actual.required("record").unwrap().get("phase"),
                Some(&"compacted".into())
            );
            assert!(!native.root.join("source").exists());
        }
        if mode == "registered-cleanup-failure" {
            assert_eq!(
                actual.required("record").unwrap().get("phase"),
                Some(&"verified".into())
            );
            assert_eq!(
                actual.required("record").unwrap().get("errorCode"),
                Some(&"target_conflict".into())
            );
        }
        let operation = actual.required("operation").unwrap();
        match mode {
            "missing" | "building" => {
                assert_eq!(
                    operation.get("value"),
                    Some(&IvyJson::Null),
                    "{mode}: {operation:?}"
                )
            }
            "wrong-build" => assert_eq!(
                operation
                    .required("error")
                    .unwrap()
                    .required("data")
                    .unwrap()
                    .get("code"),
                Some(&"build_mismatch".into())
            ),
            "unknown" | "failed" | "corrupt-publication" => assert_eq!(
                operation
                    .required("error")
                    .unwrap()
                    .required("data")
                    .unwrap()
                    .get("code"),
                Some(&"outcome_unknown".into()),
                "{mode}"
            ),
            _ => {}
        }
        if ["unknown", "failed", "corrupt-publication"].contains(&mode) {
            let data = operation
                .required("error")
                .unwrap()
                .required("data")
                .unwrap();
            assert_eq!(data.get("outcome"), Some(&"unknown".into()));
            assert_eq!(
                data.required("details").unwrap().get("preparationId"),
                Some(
                    &preparations::preparation_id_for_build(&build())
                        .unwrap()
                        .into()
                ),
            );
        }
        if mode == "building" {
            assert_eq!(
                actual.required("record").unwrap().get("phase"),
                Some(&"unknown".into())
            );
            assert_eq!(
                actual.required("record").unwrap().get("errorCode"),
                Some(&"preparation_interrupted".into())
            );
        }
        if mode != "missing-compacted-source" {
            assert_eq!(
                fs::read(&native.snapshot_file).unwrap(),
                b"authoritative snapshot"
            );
        }
        assert_eq!(fs::read(&native.outside_file).unwrap(), b"outside survives");
    }
}

#[tokio::test]
async fn cache_leases_share_builds_exclude_retirement_and_wait_for_original_owner(){
 let f=ServiceFixture::new();let staging=f.0.join("staging");let key="a".repeat(64);let root=staging.join("dependency-cache-locks").join(&key);
 let first=dependency_cache::use_dependency_cache(staging.clone(),&key).await.unwrap();let second=dependency_cache::use_dependency_cache(staging.clone(),&key).await.unwrap();
 let result=ExecutorLock::acquire(root.clone(),false).await.map(|lock|{drop(lock);IvyJson::Null});assert_eq!(result.as_ref().unwrap_err().code,"executor_already_running");let input=IvyJson::object([("kind","lock".into()),("path",text(&root)),("shared",false.into())]);compare(input.clone(),outcome(result),oracle(&input));
 first.close().await.unwrap();assert!(ExecutorLock::acquire(root.clone(),false).await.is_err());second.close().await.unwrap();
 let exclusive=ExecutorLock::acquire(root.clone(),false).await.unwrap();let requested=staging.clone();let requested_key=key.clone();let mut waiter=tokio::spawn(async move{dependency_cache::use_dependency_cache(requested,&requested_key).await});assert!(tokio::time::timeout(Duration::from_millis(200),&mut waiter).await.is_err());exclusive.close().await.unwrap();let shared=waiter.await.unwrap().unwrap();shared.close().await.unwrap();
 let exclusive=ExecutorLock::acquire(root,false).await.unwrap();let started=Instant::now();assert_eq!(dependency_cache::use_dependency_cache(staging.clone(),&key).await.err().unwrap().code,"executor_already_running");assert!(started.elapsed()>=Duration::from_secs(5)&&started.elapsed()<Duration::from_secs(8));exclusive.close().await.unwrap();
 for mode in ["existing","missing","not-directory"]{
  let target=staging.join(mode);if mode=="existing"{fs::create_dir_all(&target).unwrap();}if mode=="not-directory"{write(&target,b"file");}
  let result=dependency_cache::touch_dependency_cache_at(target.clone(),AT+123).await.map(|_|fs::symlink_metadata(&target).ok().map(|m|{let at=m.modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap();IvyJson::Number(at.as_secs() as f64*1000.0+at.subsec_nanos() as f64/1_000_000.0)}).unwrap_or(IvyJson::Null));
  let input=IvyJson::object([("kind","touch".into()),("path",text(&target)),("now",IvyJson::Number((AT+123) as f64))]);compare(input.clone(),outcome(result),oracle(&input));
 }
}

#[derive(Default)]
struct CacheProgress(Mutex<Vec<IvyJson>>);
impl ivy_host_runtime::storage_space::RetentionProgress for CacheProgress{
 fn removed(&self,area:&str){self.0.lock().unwrap().push(IvyJson::object([("kind","removed".into()),("area",area.into())]));}
 fn skipped(&self,area:&str,reason:&str,attention:bool){self.0.lock().unwrap().push(IvyJson::object([("kind","skipped".into()),("area",area.into()),("reason",reason.into()),("requiresAttention",attention.into())]));}
}
fn directory_link(target:&Path,link:&Path){
 #[cfg(windows)]{let status=Command::new("node").args(["-e","require('node:fs').symlinkSync(process.argv[1],process.argv[2],'junction')"]).arg(target).arg(link).status().unwrap();assert!(status.success());}
 #[cfg(unix)]std::os::unix::fs::symlink(target,link).unwrap();
}
async fn cache_fixture(f:&ServiceFixture,name:&str,mode:&str)->PathBuf{
 let staging=f.0.join(name).join("staging");let root=staging.join("dependency-cache");
 if mode=="empty"{return staging}
 if mode=="root-file"{write(&root,b"root is a file");return staging}
 if mode=="staging-alias"{let actual=f.0.join(name).join("real-staging");fs::create_dir_all(&actual).unwrap();directory_link(&actual,&staging);}
 #[cfg(windows)]if mode=="root-case"{fs::create_dir_all(staging.join("DEPENDENCY-CACHE")).unwrap();}
 for (index,digit) in ['a','b','c'].into_iter().enumerate(){
  if mode=="single"&&index!=0{continue}
  let directory=root.join(digit.to_string().repeat(64));write(&directory.join("cache.json"),format!("{{\"key\":\"sha256:{}\"}}\n",digit.to_string().repeat(64)).as_bytes());write(&directory.join("payload"),digit.to_string().as_bytes());
  if mode=="read-only"&&digit=='a'{let file=directory.join("payload");let mut permissions=fs::metadata(&file).unwrap().permissions();permissions.set_readonly(true);fs::set_permissions(file,permissions).unwrap();}
  dependency_cache::touch_dependency_cache_at(directory,AT+if mode=="same-time"{0}else{index as i64*1000}).await.unwrap();
 }
 let a=root.join("a".repeat(64));let b=root.join("b".repeat(64));
 match mode{
  "missing-manifest"=>fs::remove_file(a.join("cache.json")).unwrap(),"wrong-key"=>write(&a.join("cache.json"),b"{\"key\":\"other\"}"),
  "malformed"=>write(&b.join("cache.json"),b"{"),"oversize"=>write(&b.join("cache.json"),&vec![b' ';65537]),
  "retired"=>write(&root.join(format!(".retiring-{}","d".repeat(64))).join("payload"),b"interrupted deletion"),
  "unrecognized"=>{for name in ["not-a-cache","Aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",".retiring-bad"]{write(&root.join(name).join("payload"),b"keep unknown");}write(&root.join(format!(".retiring-{}","e".repeat(64))),b"keep regular file");let outside=staging.join("linked-target");write(&outside.join("keep"),b"linked child survives");directory_link(&outside,&root.join("d".repeat(64)));},_=>{},
 }
 staging
}
fn cache_observation(staging:&Path,result:IvyResult<()>,progress:&CacheProgress)->IvyJson{
 fn walk(root:&Path,directory:&Path,files:&mut Vec<IvyJson>)->std::io::Result<()>{
  let mut entries=fs::read_dir(directory)?.collect::<Result<Vec<_>,_>>()?;entries.sort_by_key(|entry|entry.file_name());
  for entry in entries{let path=entry.path();let metadata=fs::symlink_metadata(&path)?;let local=path.strip_prefix(root).unwrap().to_str().unwrap().replace('\\',"/");if metadata.file_type().is_symlink(){files.push(IvyJson::object([("path",local.into()),("type","link".into())]));}else if metadata.is_dir(){walk(root,&path,files)?;}else{files.push(IvyJson::object([("path",local.into()),("hex",fs::read(&path)?.iter().map(|b|format!("{b:02x}")).collect::<String>().into())]));}}
  Ok(())
 }
 let root=staging.join("dependency-cache");let entries=fs::read_dir(&root).ok().map(|values|{let mut names=values.map(|entry|entry.unwrap().file_name().to_str().unwrap().to_owned()).collect::<Vec<_>>();names.sort();IvyJson::Array(names.into_iter().map(IvyJson::from).collect())}).unwrap_or(IvyJson::Null);
 let mut files=Vec::new();let files=if walk(&root,&root,&mut files).is_ok(){IvyJson::Array(files)}else{IvyJson::Null};
 IvyJson::object([("operation",outcome(result.map(|_|IvyJson::Null))),("events",IvyJson::Array(progress.0.lock().unwrap().clone())),("entries",entries),("files",files)])
}

#[tokio::test]
async fn cache_collection_preserves_regular_roots_leases_retired_entries_and_diagnostics(){
 let mut modes=vec!["empty","single","regular","same-time","active-lease","active-newest","missing-manifest","wrong-key","malformed","oversize","retired","read-only","root-file","unrecognized","staging-alias"];
 #[cfg(windows)]modes.push("root-case");
 for mode in modes{
  let f=ServiceFixture::new();let native=cache_fixture(&f,"native",mode).await;let original=cache_fixture(&f,"oracle",mode).await;let mut owners=Vec::new();
  if mode=="active-lease"||mode=="active-newest"{let key=if mode=="active-lease"{"a"}else{"c"}.repeat(64);for staging in [&native,&original]{owners.push(dependency_cache::use_dependency_cache(staging.clone(),&key).await.unwrap());}}
  let progress=Arc::new(CacheProgress::default());let result=dependency_cache::collect_dependency_caches(native.clone(),progress.clone()).await;
  let expected_success=!["malformed","oversize","root-file"].contains(&mode);assert_eq!(result.is_ok(),expected_success,"{mode}: {result:?}");
  if expected_success&&!["empty","missing-manifest","wrong-key","unrecognized"].contains(&mode){let retained=if mode=="single"||mode=="same-time"{"a"}else{"c"}.repeat(64);assert!(native.join("dependency-cache").join(retained).join("payload").exists(),"{mode}");}
  if mode=="active-lease"{assert!(native.join("dependency-cache").join("a".repeat(64)).exists());assert!(!native.join("dependency-cache").join("b".repeat(64)).exists());}
  let actual=cache_observation(&native,result,&progress);let input=IvyJson::object([("kind","cache-collect".into()),("case",mode.into()),("staging",text(&original))]);compare(input.clone(),actual,oracle(&input));
  for owner in owners{owner.close().await.unwrap();}
 }
}

#[tokio::test]
async fn cache_collection_rejects_linked_roots_without_changing_any_target_bytes(){
 for mode in ["outside","inside","dangling"]{
  let f=ServiceFixture::new();let staging=f.0.join("staging");fs::create_dir_all(&staging).unwrap();let target=if mode=="inside"{staging.join("other")}else{f.0.join("outside-staging")};
  if mode!="dangling"{for digit in ["a","b"]{let cache=target.join(digit.repeat(64));write(&cache.join("cache.json"),format!("{{\"key\":\"sha256:{}\"}}",digit.repeat(64)).as_bytes());write(&cache.join("payload"),format!("preserved {digit}").as_bytes());dependency_cache::touch_dependency_cache_at(cache,AT+if digit=="a"{0}else{1000}).await.unwrap();}}
  directory_link(&target,&staging.join("dependency-cache"));let progress=Arc::new(CacheProgress::default());let result=dependency_cache::collect_dependency_caches(staging.clone(),progress.clone()).await;
  let error=result.unwrap_err();assert_eq!(error.code,"target_conflict","{mode}");assert!(fs::symlink_metadata(staging.join("dependency-cache")).unwrap().file_type().is_symlink());assert!(progress.0.lock().unwrap().is_empty());assert!(!staging.join("dependency-cache-locks").exists());
  if mode=="dangling"{assert!(!target.exists());}else{for digit in ["a","b"]{assert_eq!(fs::read(target.join(digit.repeat(64)).join("payload")).unwrap(),format!("preserved {digit}").as_bytes());assert_eq!(fs::read(target.join(digit.repeat(64)).join("cache.json")).unwrap(),format!("{{\"key\":\"sha256:{}\"}}",digit.repeat(64)).as_bytes());}assert_eq!(fs::read_dir(&target).unwrap().count(),2);}
  let payloads=if mode=="dangling"{IvyJson::Array(vec![])}else{IvyJson::Array(["a","b"].into_iter().map(|digit|{let directory=target.join(digit.repeat(64));IvyJson::object([("name",digit.repeat(64).into()),("manifest",fs::read_to_string(directory.join("cache.json")).unwrap().into()),("payload",fs::read_to_string(directory.join("payload")).unwrap().into())])}).collect())};
  let expected_payloads=if mode=="dangling"{IvyJson::Array(vec![])}else{IvyJson::Array(["a","b"].into_iter().map(|digit|IvyJson::object([("name",digit.repeat(64).into()),("manifest",format!("{{\"key\":\"sha256:{}\"}}",digit.repeat(64)).into()),("payload",format!("preserved {digit}").into())])).collect())};
  let actual=IvyJson::object([("code",error.code.into()),("linkPreserved",fs::symlink_metadata(staging.join("dependency-cache")).unwrap().file_type().is_symlink().into()),("targetExists",target.exists().into()),("payloads",payloads),("events",IvyJson::Array(progress.0.lock().unwrap().clone()))]);
  let expected=IvyJson::object([("code","target_conflict".into()),("linkPreserved",true.into()),("targetExists",(mode!="dangling").into()),("payloads",expected_payloads),("events",IvyJson::Array(vec![]))]);
  compare(IvyJson::object([("kind","cache-root-guard".into()),("case",mode.into()),("basis","task0072-cache-root-policy-agreement-138-v1; intentional correction, not original parity".into())]),actual,expected);
 }
}
