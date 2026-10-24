use super::*;
use ivy_host_runtime::{Outcome, host_config};

#[derive(Clone)]
struct Launch {
    plan: J,
    request_path: PathBuf,
    operation_id: String,
}
#[derive(Default)]
struct Runtime {
    snapshot: Mutex<Option<J>>,
    inspections: Mutex<Vec<(PathBuf, PathBuf)>>,
    launches: Mutex<Vec<Launch>>,
    fail: AtomicBool,
    no_launch: AtomicBool,
    forbid: AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    block: AtomicBool,
}
impl AutomaticBootstrapRuntime for Runtime {
    fn inspect(&self, path: PathBuf, distribution: PathBuf) -> BoxFuture<'_, IvyResult<J>> {
        Box::pin(async move {
            assert!(
                !self.forbid.load(Ordering::SeqCst),
                "Replay inspected the runtime"
            );
            self.inspections.lock().unwrap().push((path, distribution));
            Ok(self.snapshot.lock().unwrap().clone().unwrap())
        })
    }
    fn launch_automatic(
        &self,
        plan: J,
        request_path: PathBuf,
        operation_id: String,
    ) -> Option<BoxFuture<'_, IvyResult<()>>> {
        assert!(
            !self.forbid.load(Ordering::SeqCst),
            "Terminal replay requested a launch"
        );
        if self.no_launch.load(Ordering::SeqCst) {
            return None;
        }
        Some(Box::pin(async move {
            assert!(request_path.is_file());
            assert!(request_path.parent().unwrap().join("record.json").is_file());
            self.launches.lock().unwrap().push(Launch {
                plan,
                request_path,
                operation_id,
            });
            self.entered.notify_one();
            if self.block.load(Ordering::SeqCst) {
                self.release.notified().await;
            }
            if self.fail.load(Ordering::SeqCst) {
                Err(
                    IvyError::new("outcome_unknown", "Fixture automatic launch failed.")
                        .with_outcome(Outcome::Unknown),
                )
            } else {
                Ok(())
            }
        }))
    }
}
struct AutomaticFixture {
    f: Fixture,
    next: J,
    plan: J,
    next_plan: J,
    snapshot: J,
    config_path: PathBuf,
    runtime: Arc<Runtime>,
    compare: artifact::FileCompare,
}
impl AutomaticFixture {
    async fn new(name: &std::ffi::OsStr) -> Self {
        let executable = std::env::var("IVY_TEST_NODE").expect("Pinned Node path required");
        let mut f = Fixture::with_node(
            vec![
                instance("executor-renamed", "host-executor", true),
                instance("manager-renamed", "service-manager", true),
            ],
            &executable,
        )
        .await;
        let config_path = f.root.0.join(name);
        f.config = host_config::checked_host_config(f.config.clone(), &config_path)
            .await
            .unwrap();
        config::atomic_json(&config_path, f.config.clone())
            .await
            .unwrap();
        f.journal.use_configuration(f.config.clone()).await.unwrap();
        let old = f.release("host-executor", "1.0.0", "boot-old").await;
        let next = f.release("host-executor", "2.0.0", "boot-next").await;
        let manager = f.release("service-manager", "1.0.0", "manager-old").await;
        f.install("executor-renamed", &old).await;
        f.install("manager-renamed", &manager).await;
        let compare: artifact::FileCompare = Arc::new(|left, right| left.cmp(right));
        let mut plans = vec![];
        for pair in [&old, &next] {
            let components = bootstrap::bootstrap_components(
                &f.config,
                &f.journal,
                pair.required("candidate").unwrap(),
                &[],
                &compare,
            )
            .await
            .unwrap();
            plans.push(
                bootstrap::configuration_bootstrap_plan(
                    &f.config,
                    &node_path::from_native(&config_path),
                    pair.required("candidate").unwrap(),
                    None,
                    &components,
                )
                .unwrap(),
            );
        }
        let plan = plans.remove(0);
        let next_plan = plans.remove(0);
        assert!(
            !plan
                .required("processes")
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
        config::atomic_json(
            f.root
                .0
                .join("state/bootstrap")
                .join(format!("{}.json", id(&plan, "installationId"))),
            plan.clone(),
        )
        .await
        .unwrap();
        let owners = plan
            .required("processes")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                J::object([
                    ("instanceId", v.required("instanceId").unwrap().clone()),
                    ("componentId", v.required("componentId").unwrap().clone()),
                    ("name", v.required("name").unwrap().clone()),
                    ("candidateId", v.required("candidateId").unwrap().clone()),
                    ("definitionHash", hash(&id(v, "name"))),
                    ("enabled", true.into()),
                    ("running", true.into()),
                ])
            })
            .collect();
        let snapshot = J::object([
            ("schemaVersion", J::Number(1.0)),
            ("hostId", f.config.required("hostId").unwrap().clone()),
            (
                "installationId",
                plan.required("installationId").unwrap().clone(),
            ),
            ("configPath", p(&config_path)),
            ("configurationHash", hash_json(&f.config).unwrap().into()),
            ("observedAt", DATE.into()),
            ("owners", J::Array(owners)),
        ]);
        validate_host("BootstrapSnapshot", &snapshot).unwrap();
        let runtime = Arc::new(Runtime::default());
        *runtime.snapshot.lock().unwrap() = Some(snapshot.clone());
        Self {
            f,
            next,
            plan,
            next_plan,
            snapshot,
            config_path,
            runtime,
            compare,
        }
    }
    async fn plain() -> Self {
        Self::new("host.json".as_ref()).await
    }
    fn selected(&self) -> Vec<BootstrapPackageComponent> {
        vec![BootstrapPackageComponent {
            instance_id: "executor-renamed".into(),
            candidate: self.next.required("candidate").unwrap().clone(),
            manifest: self.next.required("manifest").unwrap().clone(),
        }]
    }
    fn updater(&self, path: PathBuf) -> AutomaticBootstrapUpdater {
        AutomaticBootstrapUpdater::with_clock(
            path,
            self.f.root.0.clone(),
            self.f.journal.clone(),
            self.runtime.clone(),
            self.compare.clone(),
            job(),
            Arc::new(Fixed),
        )
    }
    async fn retained(&self) -> (PathBuf, J, J) {
        let root = self.f.root.0.join("state/bootstrap-automatic");
        let entries: Vec<_> = fs::read_dir(root)
            .unwrap()
            .map(|v| v.unwrap().path())
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "Exactly one operation must own publication"
        );
        let request = entries[0].join("request.json");
        let record = entries[0].join("record.json");
        (
            request.clone(),
            read_optional(&request).await,
            read_optional(&record).await,
        )
    }
    fn assert_handoff(
        &self,
        request_path: &Path,
        request: &J,
        record: &J,
        config_path: &Path,
        phase: &str,
        code: J,
    ) {
        validate_host("BootstrapMaintenanceRequest", request).unwrap();
        assert_eq!(request.as_object().unwrap().len(), 4);
        assert_json_eq(request.required("previous").unwrap(), &self.snapshot);
        let mut next = self.next_plan.clone();
        next.insert("configPath", p(config_path));
        assert_json_eq(request.required("next").unwrap(), &next);
        let operation = id(request, "operationId");
        assert!(operation.starts_with("bootstrap-package-"));
        assert_eq!(operation.len(), "bootstrap-package-".len() + 40);
        assert_eq!(
            request_path,
            bootstrap_automatic::automatic_bootstrap_root(
                &self.f.config,
                &operation.as_str().into()
            )
            .unwrap()
            .join("request.json")
        );
        assert_json_eq(
            record,
            &J::object([
                ("schemaVersion", J::Number(1.0)),
                ("operationId", operation.into()),
                ("requestHash", hash_json(request).unwrap().into()),
                ("phase", phase.into()),
                ("errorCode", code),
                ("createdAt", DATE.into()),
                ("updatedAt", DATE.into()),
            ]),
        );
    }
    fn assert_launches(&self, count: usize) {
        let calls = self.runtime.inspections.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1, self.f.root.0);
        let launches = self.runtime.launches.lock().unwrap();
        assert_eq!(launches.len(), count);
        for launch in launches.iter() {
            assert_json_eq(&launch.plan, &self.plan);
            assert!(launch.request_path.is_absolute());
            assert_eq!(launch.operation_id.len(), "bootstrap-package-".len() + 40);
        }
        if count > 1 {
            for launch in launches.iter().skip(1) {
                assert_eq!(launch.request_path, launches[0].request_path);
                assert_eq!(launch.operation_id, launches[0].operation_id);
            }
        }
    }
    async fn close(&self) {
        self.f.journal.close().await.unwrap();
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
fn filename(opaque: Option<u16>) -> std::ffi::OsString {
    let name = std::ffi::OsString::from("host-中-🚀");
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
    let mut name = name;
    name.push(".json");
    name
}
async fn input_paths() {
    let mut variants = vec![
        "absolute",
        "relative",
        "missing-parent",
        "junction-parent",
        "canonical",
        "trailing",
        "unicode",
        "empty",
        "nul",
    ];
    #[cfg(windows)]
    variants.extend([
        "root-relative",
        "drive-relative",
        "opaque-high",
        "opaque-low",
    ]);
    for variant in variants {
        let name = filename(match variant {
            "opaque-high" => Some(0xd800),
            "opaque-low" => Some(0xdfff),
            _ => None,
        });
        let fixture = AutomaticFixture::new(&name).await;
        let path = &fixture.config_path;
        let mut expected = path.clone();
        let input = match variant {
            "relative" => relative(path),
            "missing-parent" => path.parent().unwrap().join("uncreated/../.").join(&name),
            "junction-parent" => {
                let parent = path.parent().unwrap();
                let target = parent.join("physical/target");
                fs::create_dir_all(&target).unwrap();
                fs::write(
                    parent.join("physical").join(&name),
                    b"unrelated physical parent",
                )
                .unwrap();
                let link = parent.join("link");
                ivy_host_runtime::filesystem::directory_link(&target, &link).unwrap();
                link.join("..").join(&name)
            }
            "canonical" => {
                expected = fs::canonicalize(path).unwrap();
                expected.clone()
            }
            "trailing" => {
                let mut p = path.as_os_str().to_owned();
                p.push(std::path::MAIN_SEPARATOR_STR);
                PathBuf::from(p)
            }
            "empty" => {
                expected = std::env::current_dir().unwrap();
                PathBuf::new()
            }
            "nul" => {
                expected = std::env::current_dir().unwrap().join("invalid\0config");
                PathBuf::from("invalid\0config")
            }
            #[cfg(windows)]
            "root-relative" => {
                use std::os::windows::ffi::{OsStrExt, OsStringExt};
                let units: Vec<_> = path.as_os_str().encode_wide().skip(2).collect();
                PathBuf::from(std::ffi::OsString::from_wide(&units))
            }
            #[cfg(windows)]
            "drive-relative" => {
                use std::os::windows::ffi::{OsStrExt, OsStringExt};
                let prefix: Vec<_> = path.as_os_str().encode_wide().take(2).collect();
                let mut p = std::ffi::OsString::from_wide(&prefix);
                p.push(relative(path));
                PathBuf::from(p)
            }
            _ => path.clone(),
        };
        let original = fs::read(path).unwrap();
        assert!(
            fixture
                .updater(input)
                .reconcile(fixture.selected())
                .await
                .unwrap_or_else(|error| panic!("{variant}: {}", error.code))
        );
        let (request_path, request, record) = fixture.retained().await;
        fixture.assert_handoff(
            &request_path,
            &request,
            &record,
            &expected,
            "launched",
            J::Null,
        );
        fixture.assert_launches(1);
        assert_eq!(
            fixture.runtime.inspections.lock().unwrap()[0].0,
            expected,
            "{variant}"
        );
        assert_eq!(fs::read(path).unwrap(), original);
        fixture.close().await;
    }
}
async fn early_exits() {
    for variant in ["empty", "unfinished", "closed-empty", "closed-selected"] {
        let mut fixture = AutomaticFixture::plain().await;
        fixture.runtime.forbid.store(true, Ordering::SeqCst);
        if variant == "unfinished" {
            let mut instances = fixture
                .f
                .config
                .required("instances")
                .unwrap()
                .as_array()
                .unwrap()
                .to_vec();
            instances.push(instance("ordinary", "fixture-domain", true));
            fixture.f.config.insert("instances", J::Array(instances));
            fixture.f.config =
                host_config::checked_host_config(fixture.f.config.clone(), &fixture.config_path)
                    .await
                    .unwrap();
            config::atomic_json(&fixture.config_path, fixture.f.config.clone())
                .await
                .unwrap();
            fixture
                .f
                .journal
                .use_configuration(fixture.f.config.clone())
                .await
                .unwrap();
            let ordinary = fixture
                .f
                .release("fixture-domain", "1.0.0", "ordinary")
                .await;
            fixture.f.install("ordinary", &ordinary).await;
            fixture
                .f
                .journal
                .accept(
                    J::object([
                        ("operationId", "unfinished".into()),
                        ("instanceId", "ordinary".into()),
                        ("action", "restart".into()),
                    ]),
                    None,
                )
                .await
                .unwrap();
            assert_eq!(fixture.f.journal.unfinished().await.unwrap().len(), 1);
        }
        if variant.starts_with("closed") {
            fixture.f.journal.close().await.unwrap();
        }
        let selected = if variant == "empty" || variant == "closed-empty" {
            vec![]
        } else {
            fixture.selected()
        };
        let result = fixture
            .updater(PathBuf::from("invalid\0config"))
            .reconcile(selected)
            .await;
        if variant == "closed-selected" {
            assert_eq!(result.unwrap_err().code, "internal_error");
        } else {
            assert!(!result.unwrap(), "{variant}");
        }
        assert!(fixture.runtime.inspections.lock().unwrap().is_empty());
        assert!(fixture.runtime.launches.lock().unwrap().is_empty());
        assert!(!fixture.f.root.0.join("state/bootstrap-automatic").exists());
        fixture.close().await;
    }
}
#[tokio::test]
async fn automatic_bootstrap_handoff_replays_exact_prior_snapshot_and_never_retries_terminal_release()
 {
    input_paths().await;
    early_exits().await;
    for terminal in ["succeeded", "rolled_back", "needs_attention"] {
        let fixture = AutomaticFixture::plain().await;
        let updater = fixture.updater(fixture.config_path.clone());
        assert!(updater.reconcile(fixture.selected()).await.unwrap());
        let (request_path, request, record) = fixture.retained().await;
        fixture.assert_handoff(
            &request_path,
            &request,
            &record,
            &fixture.config_path,
            "launched",
            J::Null,
        );
        let bytes = (
            fs::read(&request_path).unwrap(),
            fs::read(request_path.parent().unwrap().join("record.json")).unwrap(),
        );
        fixture
            .runtime
            .snapshot
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .insert("observedAt", "2000-01-01T00:00:00.000Z".into());
        assert!(updater.reconcile(fixture.selected()).await.unwrap());
        fixture.assert_launches(2);
        assert_eq!(fs::read(&request_path).unwrap(), bytes.0);
        assert_eq!(
            fs::read(request_path.parent().unwrap().join("record.json")).unwrap(),
            bytes.1
        );
        let mut record = record;
        record.insert("phase", terminal.into());
        record.insert("errorCode", "readiness_failed".into());
        bootstrap_automatic::save_automatic_bootstrap_record(
            &fixture.f.config,
            &mut record,
            &Fixed,
        )
        .await
        .unwrap();
        let record_path = request_path.parent().unwrap().join("record.json");
        let bytes = fs::read(&record_path).unwrap();
        fixture.runtime.forbid.store(true, Ordering::SeqCst);
        assert!(!updater.reconcile(fixture.selected()).await.unwrap());
        fs::remove_file(&request_path).unwrap();
        assert!(!updater.reconcile(fixture.selected()).await.unwrap());
        fixture.assert_launches(2);
        assert_eq!(fs::read(&record_path).unwrap(), bytes);
        fixture.close().await;
    }
}
#[tokio::test]
async fn automatic_bootstrap_handoff_preserves_unknown_launch_and_rejects_changed_or_missing_exact_requests()
 {
    for variant in [
        "launch-failure",
        "missing-request",
        "changed-prior",
        "changed-target",
        "second-launch-failure",
        "record-hash",
        "record-syntax",
        "record-null",
        "request-schema",
        "request-syntax",
        "request-directory",
    ] {
        let fixture = AutomaticFixture::plain().await;
        let updater = fixture.updater(fixture.config_path.clone());
        fixture
            .runtime
            .fail
            .store(variant == "launch-failure", Ordering::SeqCst);
        let first = updater.reconcile(fixture.selected()).await;
        if variant == "launch-failure" {
            let error = first.unwrap_err();
            assert_eq!(error.code, "outcome_unknown");
            assert_eq!(error.outcome, Outcome::Unknown);
        } else {
            assert!(first.unwrap());
        }
        let (request_path, mut request, mut record) = fixture.retained().await;
        let record_path = request_path.parent().unwrap().join("record.json");
        fixture.assert_handoff(
            &request_path,
            &request,
            &record,
            &fixture.config_path,
            if variant == "launch-failure" {
                "outcome_unknown"
            } else {
                "launched"
            },
            if variant == "launch-failure" {
                "outcome_unknown".into()
            } else {
                J::Null
            },
        );
        fixture
            .runtime
            .fail
            .store(variant == "second-launch-failure", Ordering::SeqCst);
        let expected = match variant {
            "launch-failure" => None,
            "missing-request" => {
                fs::remove_file(&request_path).unwrap();
                Some("storage_invalid")
            }
            "changed-prior" => {
                let mut previous = request.required("previous").unwrap().clone();
                previous.insert("observedAt", "2000-01-01T00:00:00.000Z".into());
                request.insert("previous", previous);
                config::atomic_json(&request_path, request).await.unwrap();
                Some("operation_conflict")
            }
            "changed-target" => {
                let mut next = request.required("next").unwrap().clone();
                next.insert("candidateId", format!("sha256:{}", "f".repeat(64)).into());
                request.insert("next", next);
                config::atomic_json(&request_path, request).await.unwrap();
                Some("operation_conflict")
            }
            "second-launch-failure" => Some("outcome_unknown"),
            "record-hash" => {
                record.insert("requestHash", hash("different-request"));
                config::atomic_json(&record_path, record).await.unwrap();
                Some("operation_conflict")
            }
            "record-syntax" => {
                fs::write(&record_path, b"{").unwrap();
                Some("internal_error")
            }
            "record-null" => {
                fs::write(&record_path, b"null").unwrap();
                Some("internal_error")
            }
            "request-schema" => {
                fs::write(&request_path, b"{}").unwrap();
                Some("invalid_arguments")
            }
            "request-syntax" => {
                fs::write(&request_path, b"{").unwrap();
                Some("internal_error")
            }
            "request-directory" => {
                fs::remove_file(&request_path).unwrap();
                fs::create_dir(&request_path).unwrap();
                Some("invalid_arguments")
            }
            _ => unreachable!(),
        };
        let before = (
            fs::read(&request_path).ok(),
            fs::read(&record_path).unwrap(),
        );
        let second = updater.reconcile(fixture.selected()).await;
        match expected {
            Some(code) => {
                let error = second.unwrap_err();
                assert_eq!(error.code, code, "{variant}");
                if code == "outcome_unknown" {
                    assert_eq!(error.outcome, Outcome::Unknown);
                }
            }
            None => assert!(second.unwrap()),
        }
        fixture.assert_launches(
            if variant == "launch-failure" || variant == "second-launch-failure" {
                2
            } else {
                1
            },
        );
        assert_eq!(fs::read(&request_path).ok(), before.0, "{variant}");
        assert_eq!(fs::read(&record_path).unwrap(), before.1, "{variant}");
        fixture.close().await;
    }
}
#[tokio::test]
async fn automatic_bootstrap_admitted_launch_outlives_waiter_after_request_and_record_publication()
{
    let fixture = AutomaticFixture::plain().await;
    fixture.runtime.block.store(true, Ordering::SeqCst);
    let updater = fixture.updater(fixture.config_path.clone());
    let owner = updater.clone();
    let selected = fixture.selected();
    let waiter = tokio::spawn(async move { owner.reconcile(selected).await });
    tokio::time::timeout(Duration::from_secs(5), fixture.runtime.entered.notified())
        .await
        .unwrap();
    let (request_path, request, record) = fixture.retained().await;
    fixture.assert_handoff(
        &request_path,
        &request,
        &record,
        &fixture.config_path,
        "launched",
        J::Null,
    );
    let bytes = (
        fs::read(&request_path).unwrap(),
        fs::read(request_path.parent().unwrap().join("record.json")).unwrap(),
    );
    waiter.abort();
    let _ = waiter.await;
    let next_owner = updater.clone();
    let selected = fixture.selected();
    let queued = tokio::spawn(async move { next_owner.reconcile(selected).await });
    tokio::task::yield_now().await;
    assert!(
        !queued.is_finished(),
        "The independent owner must retain its mutex"
    );
    fixture.assert_launches(1);
    fixture.runtime.block.store(false, Ordering::SeqCst);
    fixture.runtime.release.notify_one();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), queued)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    );
    fixture.assert_launches(2);
    assert_eq!(fs::read(&request_path).unwrap(), bytes.0);
    assert_eq!(
        fs::read(request_path.parent().unwrap().join("record.json")).unwrap(),
        bytes.1
    );
    fixture.close().await;
}
#[tokio::test]
async fn automatic_bootstrap_missing_independent_launcher_keeps_exact_launched_record_without_claiming_unknown_effect()
 {
    let fixture = AutomaticFixture::plain().await;
    fixture.runtime.no_launch.store(true, Ordering::SeqCst);
    let updater = fixture.updater(fixture.config_path.clone());
    for attempt in 0..2 {
        let error = updater.reconcile(fixture.selected()).await.unwrap_err();
        assert_eq!(error.code, "unsupported_runtime");
        assert_eq!(error.outcome, Outcome::NotExecuted);
        let (request_path, request, record) = fixture.retained().await;
        fixture.assert_handoff(
            &request_path,
            &request,
            &record,
            &fixture.config_path,
            "launched",
            J::Null,
        );
        assert_eq!(
            fixture.runtime.inspections.lock().unwrap().len(),
            1,
            "{attempt}"
        );
        assert!(fixture.runtime.launches.lock().unwrap().is_empty());
    }
    fixture.runtime.no_launch.store(false, Ordering::SeqCst);
    assert!(updater.reconcile(fixture.selected()).await.unwrap());
    fixture.assert_launches(1);
    fixture.close().await;
}
