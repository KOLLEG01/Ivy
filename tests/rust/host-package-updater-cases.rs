use super::{ServiceFixture, assert_json_eq, host_configuration};
use futures_util::future::BoxFuture;
use ivy_contracts::{IvyJson as J, JsString, core_schema::validate_host, hash::hash_json, json};
use ivy_host_runtime::{
    IvyError, IvyResult, artifact, backup_files, bootstrap,
    bootstrap_automatic::{self, AutomaticBootstrapRuntime, AutomaticBootstrapUpdater},
    config,
    executor_lock::ExecutorLock,
    journal::HostJournal,
    node_path,
    node_runtime::NodeRuntime,
    package_updater::{
        BootstrapPackageComponent, NativePackageClient, PackageClient, PackageUpdater,
    },
    package_validation,
};
use ivy_sdk::clock::Clock;
use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
const AT: i64 = 1760000000000;
const DATE: &str = "2025-10-09T08:53:20.000Z";
static CORPUS: Mutex<()> = Mutex::new(());
fn p(path: &Path) -> J {
    J::String(node_path::from_native(path))
}
fn path(value: &J) -> PathBuf {
    node_path::to_native(value.as_js_string().unwrap())
}
fn id(value: &J, key: &str) -> String {
    value.required(key).unwrap().as_str().unwrap()
}
fn hash(value: &str) -> J {
    ivy_contracts::hash::digest(value.as_bytes()).into()
}
fn empty() -> J {
    J::object([] as [(&str, J); 0])
}
fn wrap(value: IvyResult<J>) -> J {
    match value {
        Ok(value) => J::object([("ok", true.into()), ("value", value)]),
        Err(error) => J::object([("ok", false.into()), ("error", error.to_wire())]),
    }
}
fn oracle(input: &J) -> J {
    let mut child = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
        ])
        .arg(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../../tests/fixtures/host-package-updater-oracle.mjs"),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json::stringify(input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    json::parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
}
fn record(case: &str, input: &J, actual: &J, original: &J, equal: bool) {
    if let Some(file) = std::env::var_os("IVY_HOST_PACKAGE_UPDATER_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        writeln!(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(file)
                .unwrap(),
            "{}",
            json::stringify(
                &J::object([
                    ("case", case.into()),
                    ("input", input.clone()),
                    ("actual", actual.clone()),
                    ("original", original.clone()),
                    ("equal", equal.into())
                ]),
                usize::MAX
            )
            .unwrap()
        )
        .unwrap();
    }
    if equal {
        assert_json_eq(actual, original);
    }
}
struct Fixed;
impl Clock for Fixed {
    fn utc_millis(&self) -> i64 {
        AT
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
pub(super) fn job() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ivy-job"))
}
pub(super) fn node() -> NodeRuntime {
    NodeRuntime {
        executable: PathBuf::from("unused-intentional-node"),
        version: "24.18.0".into(),
    }
}
pub(super) fn instance(id: &str, component: &str, enabled: bool) -> J {
    J::object([
        ("instanceId", id.into()),
        ("componentId", component.into()),
        ("serviceNodeId", id.into()),
        ("engine", "process".into()),
        ("enabled", enabled.into()),
        ("settings", empty()),
    ])
}
pub(super) struct Fixture {
    pub(super) root: ServiceFixture,
    pub(super) config: J,
    pub(super) pairs: Vec<J>,
    pub(super) installed: Vec<J>,
    pub(super) journal: HostJournal,
}
impl Fixture {
    pub(super) async fn new(instances: Vec<J>) -> Self {
        Self::with_node(instances, "fixture-node").await
    }
    pub(super) async fn with_node(instances: Vec<J>, executable: &str) -> Self {
        let root = ServiceFixture::new();
        let mut config = host_configuration(&root);
        config.insert("executables", J::object([("node", executable.into())]));
        config.insert("instances", J::Array(instances));
        config.insert(
            "packageUpdates",
            J::object([("intervalSeconds", J::Number(60.0))]),
        );
        fs::create_dir_all(root.0.join("state")).unwrap();
        validate_host("HostConfig", &config).unwrap();
        let journal = HostJournal::open_with_clock(config.clone(), Arc::new(Fixed))
            .await
            .unwrap();
        Self {
            root,
            config,
            pairs: vec![],
            installed: vec![],
            journal,
        }
    }
    pub(super) fn config_path(&self) -> PathBuf {
        self.root.0.join("host.json")
    }
    pub(super) async fn release(&mut self, component: &str, version: &str, seed: &str) -> J {
        let build = hash(seed);
        let root = self
            .root
            .0
            .join("artifacts/retained")
            .join(&id(&J::object([("id", build.clone())]), "id")[7..]);
        fs::create_dir_all(root.join("artifact/dist")).unwrap();
        let command = |file: &str| {
            J::object([
                ("executable", "node".into()),
                ("args", J::Array(vec![file.into()])),
                ("timeoutMs", J::Number(5000.0)),
            ])
        };
        let manifest = J::object([
            ("schemaVersion", J::Number(1.0)),
            ("componentId", component.into()),
            ("kind", "service".into()),
            ("version", version.into()),
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
                    ("maximumDelayMs", J::Number(1000.0)),
                ]),
            ),
        ]);
        config::atomic_json(root.join("component.json"), manifest.clone())
            .await
            .unwrap();
        config::atomic_json(
            root.join("artifact/dist/build-info.json"),
            J::object([
                ("schemaVersion", J::Number(1.0)),
                ("componentId", component.into()),
                ("version", version.into()),
                ("buildId", build.clone()),
            ]),
        )
        .await
        .unwrap();
        for file in ["dist/main.js", "dist/health.js"] {
            fs::write(
                root.join("artifact").join(file),
                b"throw Error('a package selection check must not run JS');",
            )
            .unwrap();
        }
        for file in ivy_host_runtime::portable_native_files::portable_native_files(component) {
            let file = root.join("artifact").join(file);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, b"retained native payload fixture").unwrap();
        }
        let candidate = J::object([
            ("candidateId", build.clone()),
            ("buildId", build),
            ("componentId", component.into()),
            ("artifactRoot", p(&root.join("artifact"))),
            ("manifestPath", p(&root.join("component.json"))),
            ("archivePath", p(&root.join("package.tar.gz"))),
            ("archiveHash", hash(&format!("archive-{seed}"))),
            ("createdAt", DATE.into()),
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
                    ("node", "v24.18.0".into()),
                ]),
            ),
        ]);
        validate_host("Candidate", &candidate).unwrap();
        validate_host("ReleaseManifest", &manifest).unwrap();
        artifact::verify_candidate(
            candidate.clone(),
            path(self.config.required("artifactRoot").unwrap()),
            &node(),
        )
        .await
        .unwrap();
        self.journal
            .save_candidate(candidate.clone(), manifest.clone())
            .await
            .unwrap();
        let pair = J::object([("candidate", candidate), ("manifest", manifest)]);
        self.pairs.push(pair.clone());
        pair
    }
    pub(super) async fn install(&mut self, instance: &str, pair: &J) -> J {
        let candidate = pair.required("candidate").unwrap();
        let value = J::object([
            ("instanceId", instance.into()),
            (
                "candidateId",
                candidate.required("candidateId").unwrap().clone(),
            ),
            ("buildId", candidate.required("buildId").unwrap().clone()),
            ("enabled", true.into()),
            ("installedAt", DATE.into()),
        ]);
        self.journal.set_installed(value.clone()).await.unwrap();
        self.installed.push(value.clone());
        value
    }
    fn input(&self, kind: &str, steps: Vec<J>) -> J {
        J::object([
            ("kind", kind.into()),
            ("config", self.config.clone()),
            ("configPath", p(&self.config_path())),
            ("distribution", p(&self.root.0)),
            ("pairs", J::Array(self.pairs.clone())),
            ("installed", J::Array(self.installed.clone())),
            ("steps", J::Array(steps)),
            ("at", J::Number(AT as f64)),
            ("actualImport", true.into()),
        ])
    }
}
fn entry(pair: &J, revision: f64) -> J {
    let c = pair.required("candidate").unwrap();
    let m = pair.required("manifest").unwrap();
    let value = J::object([
        ("componentId", c.required("componentId").unwrap().clone()),
        ("buildId", c.required("buildId").unwrap().clone()),
        ("version", m.required("version").unwrap().clone()),
        ("archiveHash", c.required("archiveHash").unwrap().clone()),
        ("bytes", J::Number(100.0)),
        ("manifest", m.clone()),
        ("revision", J::Number(revision)),
        ("publishedAt", DATE.into()),
        ("publisherPrincipalId", "fixture-publisher".into()),
    ]);
    package_validation::validate_package_catalog(&catalog(vec![value.clone()], revision)).unwrap();
    value
}
fn catalog(entries: Vec<J>, revision: f64) -> J {
    J::object([
        ("schemaVersion", J::Number(1.0)),
        ("revision", J::Number(revision)),
        ("packages", J::Array(entries)),
    ])
}
fn step(value: J) -> J {
    J::object([("value", value)])
}
#[derive(Default)]
struct Client {
    responses: Mutex<VecDeque<J>>,
    calls: Mutex<Vec<J>>,
    downloads: AtomicUsize,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    block: AtomicBool,
}
impl PackageClient for Client {
    fn catalog(
        &self,
        base: String,
        credential: String,
        revision: f64,
    ) -> BoxFuture<'_, IvyResult<Option<J>>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(J::object([
                ("base", base.into()),
                ("credential", credential.into()),
                ("revision", J::Number(revision)),
            ]));
            self.entered.notify_one();
            if self.block.load(Ordering::SeqCst) {
                self.release.notified().await;
            }
            let response = self.responses.lock().unwrap().pop_front().unwrap();
            if let Some(error) = response.get("error") {
                return Err(IvyError::new(
                    error.as_str().unwrap(),
                    "Fixture catalog failure.",
                ));
            }
            if response.get("status") == Some(&J::Number(204.0)) {
                Ok(None)
            } else {
                Ok(Some(response.required("value")?.clone()))
            }
        })
    }
    fn download(
        &self,
        _base: String,
        _credential: String,
        _entry: J,
        _archive: PathBuf,
    ) -> BoxFuture<'_, IvyResult<()>> {
        Box::pin(async move {
            self.downloads.fetch_add(1, Ordering::SeqCst);
            panic!("registered candidates do not download")
        })
    }
}
async fn native_packages(f: &Fixture, input: &J) -> J {
    let steps = input.required("steps").unwrap().as_array().unwrap();
    let client = Arc::new(Client::default());
    *client.responses.lock().unwrap() = steps.to_vec().into();
    let updater = PackageUpdater::new(
        f.journal.clone(),
        "fixture-credential".into(),
        node(),
        client.clone(),
    );
    let mut results = vec![];
    for step in steps {
        if step.get("failInstance").is_some() || step.get("removeFailure").is_some() {
            let db = ivy_store::rusqlite::Connection::open(
                path(f.config.required("runtimeRoot").unwrap()).join("deployments.sqlite"),
            )
            .unwrap();
            if let Some(instance) = step.get("failInstance") {
                let instance = instance.as_str().unwrap();
                assert!(
                    instance
                        .bytes()
                        .all(|v| v.is_ascii_lowercase() || v == b'-')
                );
                db.execute_batch(&format!("CREATE TRIGGER fixture_fail_package BEFORE INSERT ON operations WHEN json_extract(NEW.entry_json,'$.request.instanceId')='{instance}' BEGIN SELECT RAISE(ABORT,'fixture interrupted acceptance'); END")).unwrap();
            }
            if step.get("removeFailure").is_some() {
                db.execute_batch("DROP TRIGGER fixture_fail_package")
                    .unwrap();
            }
        }
        if let Some(config) = step.get("config") {
            f.journal.use_configuration(config.clone()).await.unwrap();
        }
        for value in step.get("installed").and_then(J::as_array).unwrap_or(&[]) {
            f.journal.set_installed(value.clone()).await.unwrap();
        }
        results.push(wrap(updater.sync().await.map(|v| v.to_json())));
    }
    assert_eq!(client.downloads.load(Ordering::SeqCst), 0);
    let mut requests = f
        .journal
        .list(200.0)
        .await
        .unwrap()
        .into_iter()
        .map(|v| v.required("request").unwrap().clone())
        .collect::<Vec<_>>();
    requests.sort_by_key(|v| id(v, "operationId"));
    let calls = client.calls.lock().unwrap().clone();
    J::object([
        ("results", J::Array(results)),
        ("requests", J::Array(requests)),
        ("calls", J::Array(calls)),
    ])
}

#[tokio::test]
async fn package_updater_bootstraps_executor_before_newer_unknown_component_schema_and_prunes_only_installed_release()
 {
    let mut f = Fixture::new(vec![instance("host-executor", "host-executor", true)]).await;
    let old = f.release("host-executor", "1.0.0", "old-executor").await;
    let next = f.release("host-executor", "2.0.0", "next-executor").await;
    f.install("host-executor", &old).await;
    let mut future = entry(&next, 2.0);
    future.insert("componentId", "future".into());
    let mut m = future.required("manifest").unwrap().clone();
    m.insert("futureManifestField", true.into());
    future.insert("manifest", m);
    let installed = J::object([
        ("instanceId", "host-executor".into()),
        (
            "candidateId",
            next.required("candidate")
                .unwrap()
                .required("candidateId")
                .unwrap()
                .clone(),
        ),
        (
            "buildId",
            next.required("candidate")
                .unwrap()
                .required("buildId")
                .unwrap()
                .clone(),
        ),
        ("enabled", true.into()),
        ("installedAt", DATE.into()),
    ]);
    let input = f.input(
        "packages",
        vec![
            step(catalog(vec![entry(&next, 2.0), future], 2.0)),
            J::object([("status", J::Number(204.0))]),
            J::object([
                ("status", J::Number(204.0)),
                ("installed", J::Array(vec![installed])),
            ]),
        ],
    );
    let original = oracle(&input);
    let actual = native_packages(&f, &input).await;
    assert_eq!(
        actual
            .required("requests")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        actual.required("results").unwrap().as_array().unwrap()[0]
            .required("value")
            .unwrap()
            .required("bootstrap")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    record(
        "executor-first-and-204-retention",
        &input,
        &actual,
        &original,
        true,
    );
}

#[tokio::test]
async fn package_updater_preserves_newest_insertion_order_partial_replay_and_configured_executor_build_identity()
 {
    for renamed in [false, true] {
        let own = if renamed {
            "renamed-executor"
        } else {
            "host-executor"
        };
        let mut f = Fixture::new(vec![
            instance(own, "host-executor", true),
            instance("one", "domain", true),
            instance("two", "domain", false),
            instance("unchanged", "stable", true),
        ])
        .await;
        let own_pair = f.release("host-executor", "1.0.0", "executor-build").await;
        f.install(own, &own_pair).await;
        let upgraded_executor = f
            .release("host-executor", "1.1.0", "upgraded-executor-build")
            .await;
        let first = f.release("domain", "1.0.0", "first-domain").await;
        let latest = f.release("domain", "3.0.0", "latest-domain").await;
        let middle = f.release("domain", "2.0.0", "middle-domain").await;
        let stable = f.release("stable", "2.0.0", "stable").await;
        f.install("unchanged", &stable).await;
        let catalog = catalog(
            vec![
                entry(&first, 1.0),
                entry(&stable, 1.0),
                entry(&latest, 2.0),
                entry(&middle, 2.0),
            ],
            2.0,
        );
        let mut after_upgrade = step(catalog.clone());
        after_upgrade.insert(
            "installed",
            J::Array(vec![J::object([
                ("instanceId", own.into()),
                (
                    "candidateId",
                    upgraded_executor
                        .required("candidate")
                        .unwrap()
                        .required("candidateId")
                        .unwrap()
                        .clone(),
                ),
                (
                    "buildId",
                    upgraded_executor
                        .required("candidate")
                        .unwrap()
                        .required("buildId")
                        .unwrap()
                        .clone(),
                ),
                ("enabled", true.into()),
                ("installedAt", DATE.into()),
            ])]),
        );
        let input = f.input(
            "packages",
            vec![
                step(catalog.clone()),
                step(catalog),
                after_upgrade,
                J::object([("status", J::Number(204.0))]),
            ],
        );
        let original = oracle(&input);
        let actual = native_packages(&f, &input).await;
        let requests = actual.required("requests").unwrap().as_array().unwrap();
        assert_eq!(
            requests.len(),
            4,
            "one replay-stable operation per domain instance and installed executor build"
        );
        for instance in ["one", "two"] {
            assert_eq!(
                requests
                    .iter()
                    .filter(|request| request.get("instanceId") == Some(&J::from(instance)))
                    .count(),
                2
            );
        }
        for request in requests {
            assert_eq!(
                request.get("candidateId"),
                latest.required("candidate").unwrap().get("candidateId")
            );
        }
        if renamed {
            assert_ne!(
                actual.required("requests").unwrap(),
                original.required("requests").unwrap(),
                "HE02 is the explicitly accepted original defect correction"
            );
            let mut fixed = original.clone();
            fixed.insert("requests", actual.required("requests").unwrap().clone());
            assert_json_eq(&fixed, &actual);
            assert_eq!(
                original
                    .required("requests")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .len(),
                2,
                "original literal lookup loses the renamed executor's upgrade boundary"
            );
            record(
                "HE02-renamed-executor-hash-correction",
                &input,
                &actual,
                &original,
                false,
            );
        } else {
            record("newest-order-and-repeat", &input, &actual, &original, true);
        }
    }
}

#[tokio::test]
async fn package_updater_does_not_advance_revision_on_admission_error_and_keeps_admitted_sync_owned()
 {
    let mut f = Fixture::new(vec![instance("host-executor", "host-executor", true)]).await;
    let old = f.release("host-executor", "1.0.0", "old").await;
    let next = f.release("host-executor", "2.0.0", "next").await;
    f.install("host-executor", &old).await;
    let client = Arc::new(Client::default());
    *client.responses.lock().unwrap() = vec![
        step(J::Null),
        step(catalog(vec![entry(&next, 2.0)], 2.0)),
        J::object([("status", J::Number(204.0))]),
    ]
    .into();
    let updater = PackageUpdater::new(
        f.journal.clone(),
        "fixture-credential".into(),
        node(),
        client.clone(),
    );
    assert_eq!(updater.sync().await.unwrap_err().code, "invalid_arguments");
    client.block.store(true, Ordering::SeqCst);
    let owner = updater.clone();
    let waiter = tokio::spawn(async move { owner.sync().await });
    client.entered.notified().await;
    // Consume the first call's stored notification before waiting for the new call.
    while client.calls.lock().unwrap().len() < 2 {
        tokio::task::yield_now().await;
    }
    waiter.abort();
    client.block.store(false, Ordering::SeqCst);
    client.release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(5), updater.sync())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.bootstrap.len(), 1);
    assert_json_eq(
        &J::Array(
            client
                .calls
                .lock()
                .unwrap()
                .iter()
                .map(|v| v.required("revision").unwrap().clone())
                .collect(),
        ),
        &J::Array(vec![J::Number(0.0), J::Number(0.0), J::Number(2.0)]),
    );
    let root = path(f.config.required("stagingRoot").unwrap())
        .join("build-locks")
        .join(&id(next.required("candidate").unwrap(), "buildId")[7..]);
    ExecutorLock::acquire(root, false)
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
}

#[tokio::test]
async fn package_updater_replays_partial_acceptance_without_duplicate_or_revised_operations() {
    let mut f = Fixture::new(vec![
        instance("first", "domain", true),
        instance("second", "domain", true),
    ])
    .await;
    let pair = f.release("domain", "1.0.0", "partial-domain").await;
    let catalog = catalog(vec![entry(&pair, 5.0)], 5.0);
    let mut first = step(catalog.clone());
    first.insert("failInstance", "second".into());
    let mut second = step(catalog);
    second.insert("removeFailure", true.into());
    let input = f.input("packages", vec![first, second]);
    let original = oracle(&input);
    let actual = native_packages(&f, &input).await;
    assert_eq!(
        actual.required("results").unwrap().as_array().unwrap()[0].get("ok"),
        Some(&false.into())
    );
    assert_eq!(
        actual
            .required("requests")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    record(
        "partial-acceptance-exact-replay",
        &input,
        &actual,
        &original,
        true,
    );
}

pub(super) fn comparator(values: Vec<J>) -> artifact::FileCompare {
    let sorted = oracle(&J::object([
        ("kind", "collation".into()),
        ("values", J::Array(values)),
    ]));
    let ranks = sorted
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(index, v)| (v.as_js_string().unwrap().clone(), index))
        .collect::<BTreeMap<_, _>>();
    Arc::new(move |left, right| {
        ranks
            .get(left)
            .expect("fixture comparator admitted left")
            .cmp(ranks.get(right).expect("fixture comparator admitted right"))
    })
}
#[derive(Default)]
struct Automatic {
    snapshot: Mutex<Option<J>>,
    events: Mutex<Vec<J>>,
    fail: AtomicBool,
    no_launch: AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    block: AtomicBool,
}
impl AutomaticBootstrapRuntime for Automatic {
    fn inspect(&self, _path: PathBuf, _distribution: PathBuf) -> BoxFuture<'_, IvyResult<J>> {
        Box::pin(async move {
            self.events
                .lock()
                .unwrap()
                .push(J::object([("kind", "inspect".into())]));
            Ok(self.snapshot.lock().unwrap().clone().unwrap())
        })
    }
    fn launch_automatic(
        &self,
        plan: J,
        request_path: PathBuf,
        operation_id: String,
    ) -> Option<BoxFuture<'_, IvyResult<()>>> {
        if self.no_launch.load(Ordering::SeqCst) {
            return None;
        }
        Some(Box::pin(async move {
            self.events.lock().unwrap().push(J::object([
                ("kind", "launch".into()),
                ("plan", plan),
                ("requestPath", p(&request_path)),
                ("operationId", operation_id.into()),
            ]));
            self.entered.notify_one();
            if self.block.load(Ordering::SeqCst) {
                self.release.notified().await;
            }
            if self.fail.load(Ordering::SeqCst) {
                Err(
                    IvyError::new("outcome_unknown", "Fixture automatic launch failed.")
                        .with_outcome(ivy_host_runtime::Outcome::Unknown),
                )
            } else {
                Ok(())
            }
        }))
    }
}
async fn automatic_fixture() -> (Fixture, J, J, J, Arc<Automatic>, artifact::FileCompare) {
    let mut f = Fixture::new(vec![
        instance("executor-renamed", "host-executor", true),
        instance("manager-renamed", "service-manager", true),
    ])
    .await;
    let old = f.release("host-executor", "1.0.0", "boot-old").await;
    let next = f.release("host-executor", "2.0.0", "boot-next").await;
    let manager = f.release("service-manager", "1.0.0", "manager-old").await;
    f.install("executor-renamed", &old).await;
    f.install("manager-renamed", &manager).await;
    config::atomic_json(f.config_path(), f.config.clone())
        .await
        .unwrap();
    let compare = comparator(vec![
        "executor-renamed".into(),
        "manager-renamed".into(),
        DATE.into(),
    ]);
    let components = bootstrap::bootstrap_components(
        &f.config,
        &f.journal,
        old.required("candidate").unwrap(),
        &[],
        &compare,
    )
    .await
    .unwrap();
    let plan = bootstrap::configuration_bootstrap_plan(
        &f.config,
        &node_path::from_native(&f.config_path()),
        old.required("candidate").unwrap(),
        None,
        &components,
    )
    .unwrap();
    validate_host("BootstrapPlan", &plan).unwrap();
    assert!(
        !plan
            .required("processes")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    let selected_path = f
        .root
        .0
        .join("state/bootstrap")
        .join(format!("{}.json", id(&plan, "installationId")));
    config::atomic_json(selected_path, plan.clone())
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
        ("configPath", p(&f.config_path())),
        ("configurationHash", hash_json(&f.config).unwrap().into()),
        ("observedAt", DATE.into()),
        ("owners", J::Array(owners)),
    ]);
    validate_host("BootstrapSnapshot", &snapshot).unwrap();
    let runtime = Arc::new(Automatic::default());
    *runtime.snapshot.lock().unwrap() = Some(snapshot.clone());
    (f, next, plan, snapshot, runtime, compare)
}
fn selected(pair: &J) -> BootstrapPackageComponent {
    BootstrapPackageComponent {
        instance_id: "executor-renamed".into(),
        candidate: pair.required("candidate").unwrap().clone(),
        manifest: pair.required("manifest").unwrap().clone(),
    }
}
async fn read_optional(path: &Path) -> J {
    if backup_files::exists(path).await.unwrap() {
        config::json_file(path.to_owned(), config::CONFIGURATION_BYTES)
            .await
            .unwrap()
    } else {
        J::Null
    }
}
async fn native_automatic(
    f: &Fixture,
    input: &J,
    runtime: Arc<Automatic>,
    compare: artifact::FileCompare,
    selected: Vec<BootstrapPackageComponent>,
) -> J {
    let updater = AutomaticBootstrapUpdater::with_clock(
        f.config_path(),
        f.root.0.clone(),
        f.journal.clone(),
        runtime.clone(),
        compare,
        job(),
        Arc::new(Fixed),
    );
    runtime.no_launch.store(
        input.get("noLaunch") == Some(&true.into()),
        Ordering::SeqCst,
    );
    let mut results = vec![];
    let mut files = vec![];
    let mut last = None;
    for step in input.required("steps").unwrap().as_array().unwrap() {
        runtime.fail.store(
            step.get("failLaunch") == Some(&true.into()),
            Ordering::SeqCst,
        );
        if let Some(path) = last.as_ref() {
            let path: &PathBuf = path;
            let record_path = path.parent().unwrap().join("record.json");
            if let Some(phase) = step.get("phase") {
                let mut record = read_optional(&record_path).await;
                record.insert("phase", phase.clone());
                record.insert(
                    "errorCode",
                    step.get("errorCode").cloned().unwrap_or(J::Null),
                );
                bootstrap_automatic::save_automatic_bootstrap_record(
                    &f.config,
                    &mut record,
                    &Fixed,
                )
                .await
                .unwrap();
            }
            if step.get("deleteRequest") == Some(&true.into()) {
                fs::remove_file(path).unwrap();
            }
            if step.get("changeRequest") == Some(&true.into())
                || step.get("changeNext") == Some(&true.into())
            {
                let mut request = read_optional(path).await;
                if step.get("changeNext") == Some(&true.into()) {
                    let mut next = request.required("next").unwrap().clone();
                    next.insert("candidateId", format!("sha256:{}", "f".repeat(64)).into());
                    request.insert("next", next);
                } else {
                    let mut previous = request.required("previous").unwrap().clone();
                    previous.insert("observedAt", "2000-01-01T00:00:00.000Z".into());
                    request.insert("previous", previous);
                }
                config::atomic_json(path.clone(), request).await.unwrap();
            }
        }
        results.push(wrap(updater.reconcile(selected.clone()).await.map(J::from)));
        last = runtime
            .events
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|v| v.get("kind") == Some(&"launch".into()))
            .map(|v| path(v.required("requestPath").unwrap()))
            .or_else(|| input.get("expectedRequestPath").map(path));
        if let Some(path) = last.as_ref() {
            files.push(J::object([
                ("request", read_optional(path).await),
                (
                    "record",
                    read_optional(&path.parent().unwrap().join("record.json")).await,
                ),
            ]));
        }
    }
    let events = runtime.events.lock().unwrap().clone();
    J::object([
        ("results", J::Array(results)),
        ("events", J::Array(events)),
        ("files", J::Array(files)),
    ])
}

#[tokio::test]
async fn automatic_bootstrap_handoff_replays_exact_prior_snapshot_and_never_retries_terminal_release()
 {
    for terminal in ["succeeded", "rolled_back", "needs_attention"] {
        let (f, next, _plan, snapshot, runtime, compare) = automatic_fixture().await;
        let mut input = f.input(
            "automatic",
            vec![
                empty(),
                empty(),
                J::object([
                    ("phase", terminal.into()),
                    ("errorCode", "readiness_failed".into()),
                ]),
            ],
        );
        input.insert("snapshot", snapshot);
        input.insert("selected", J::Array(vec![selected(&next).to_json()]));
        let original = oracle(&input);
        fs::remove_dir_all(f.root.0.join("state/bootstrap-automatic")).unwrap();
        let actual = native_automatic(&f, &input, runtime, compare, vec![selected(&next)]).await;
        assert_eq!(
            actual
                .required("events")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| v.get("kind") == Some(&"inspect".into()))
                .count(),
            1
        );
        assert_eq!(
            actual
                .required("events")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| v.get("kind") == Some(&"launch".into()))
                .count(),
            2
        );
        record(terminal, &input, &actual, &original, true);
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
    ] {
        let (f, next, _plan, snapshot, runtime, compare) = automatic_fixture().await;
        let steps = match variant {
            "launch-failure" => vec![J::object([("failLaunch", true.into())]), empty()],
            "missing-request" => vec![empty(), J::object([("deleteRequest", true.into())])],
            "changed-prior" => vec![empty(), J::object([("changeRequest", true.into())])],
            "changed-target" => vec![empty(), J::object([("changeNext", true.into())])],
            _ => vec![empty(), J::object([("failLaunch", true.into())])],
        };
        let mut input = f.input("automatic", steps);
        input.insert("snapshot", snapshot);
        input.insert("selected", J::Array(vec![selected(&next).to_json()]));
        let original = oracle(&input);
        fs::remove_dir_all(f.root.0.join("state/bootstrap-automatic")).unwrap();
        let actual = native_automatic(&f, &input, runtime, compare, vec![selected(&next)]).await;
        record(variant, &input, &actual, &original, true);
    }
}

#[tokio::test]
async fn automatic_bootstrap_admitted_launch_outlives_waiter_after_request_and_record_publication()
{
    let (f, next, _, _, runtime, compare) = automatic_fixture().await;
    runtime.block.store(true, Ordering::SeqCst);
    let updater = AutomaticBootstrapUpdater::with_clock(
        f.config_path(),
        f.root.0.clone(),
        f.journal.clone(),
        runtime.clone(),
        compare,
        job(),
        Arc::new(Fixed),
    );
    let owner = updater.clone();
    let chosen = selected(&next);
    let waiter = tokio::spawn(async move { owner.reconcile(vec![chosen]).await });
    runtime.entered.notified().await;
    let event = runtime.events.lock().unwrap().last().unwrap().clone();
    let request = path(event.required("requestPath").unwrap());
    assert_eq!(
        read_optional(&request)
            .await
            .required("next")
            .unwrap()
            .get("candidateId"),
        next.required("candidate").unwrap().get("candidateId")
    );
    assert_eq!(
        read_optional(&request.parent().unwrap().join("record.json"))
            .await
            .get("phase"),
        Some(&"launched".into())
    );
    waiter.abort();
    runtime.block.store(false, Ordering::SeqCst);
    runtime.release.notify_one();
    assert!(
        tokio::time::timeout(
            Duration::from_secs(5),
            updater.reconcile(vec![selected(&next)])
        )
        .await
        .unwrap()
        .unwrap()
    );
    assert_eq!(
        runtime
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v.get("kind") == Some(&"inspect".into()))
            .count(),
        1
    );
}

#[tokio::test]
async fn automatic_bootstrap_missing_independent_launcher_keeps_exact_launched_record_without_claiming_unknown_effect()
 {
    let (f, next, _, snapshot, runtime, compare) = automatic_fixture().await;
    let mut input = f.input("automatic", vec![empty()]);
    input.insert("snapshot", snapshot);
    input.insert("selected", J::Array(vec![selected(&next).to_json()]));
    // Obtain the original's concrete request identity from its valid first launch,
    // then test the missing capability from a fresh durable operation directory.
    let probe = oracle(&input);
    let request = probe
        .required("events")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v.get("kind") == Some(&"launch".into()))
        .unwrap()
        .required("requestPath")
        .unwrap()
        .clone();
    fs::remove_dir_all(f.root.0.join("state/bootstrap-automatic")).unwrap();
    input.insert("noLaunch", true.into());
    input.insert("expectedRequestPath", request.clone());
    input.insert("originalProbe", probe);
    let original = oracle(&input);
    fs::remove_dir_all(f.root.0.join("state/bootstrap-automatic")).unwrap();
    let actual = native_automatic(&f, &input, runtime, compare, vec![selected(&next)]).await;
    assert_eq!(
        actual.required("results").unwrap().as_array().unwrap()[0]
            .required("error")
            .unwrap()
            .required("data")
            .unwrap()
            .get("code"),
        Some(&"unsupported_runtime".into())
    );
    assert_eq!(
        read_optional(&path(&request).parent().unwrap().join("record.json"))
            .await
            .get("phase"),
        Some(&"launched".into())
    );
    record(
        "missing-independent-launch",
        &input,
        &actual,
        &original,
        true,
    );
}

#[tokio::test]
async fn bootstrap_components_select_installed_preferred_fresh_and_disabled_missing_candidates() {
    for mode in [
        "preferred",
        "installed",
        "fresh",
        "disabled-missing",
        "enabled-missing",
    ] {
        let mut f = Fixture::new(vec![
            instance("executor-renamed", "host-executor", true),
            instance(
                "manager-renamed",
                "service-manager",
                mode != "disabled-missing",
            ),
        ])
        .await;
        let host = f
            .release("host-executor", "1.0.0", "component-executor")
            .await;
        // Exercise the Windows-owned component selection on every test host.
        let mut host_candidate = host.required("candidate").unwrap().clone();
        let mut platform = host_candidate.required("platform").unwrap().clone();
        platform.insert("os", "win32".into());
        host_candidate.insert("platform", platform);
        let mut preferred = vec![];
        if !mode.ends_with("missing") {
            let old = f
                .release("service-manager", "1.0.0", "component-manager-old")
                .await;
            let next = f
                .release("service-manager", "2.0.0", "component-manager-next")
                .await;
            if mode != "fresh" {
                f.install("manager-renamed", &old).await;
            }
            if mode == "preferred" {
                preferred.push(next);
            }
        }
        let compare = comparator(vec![DATE.into()]);
        let mut input = f.input("components", vec![]);
        input.insert("hostCandidate", host_candidate.clone());
        input.insert("preferred", J::Array(preferred.clone()));
        let original = oracle(&input);
        let actual = wrap(
            bootstrap::bootstrap_components(
                &f.config,
                &f.journal,
                &host_candidate,
                &preferred,
                &compare,
            )
            .await
            .map(J::Array),
        );
        if mode == "enabled-missing" {
            assert_eq!(actual.get("ok"), Some(&false.into()));
        } else {
            assert!(
                !actual
                    .required("value")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
        record(mode, &input, &actual, &original, true);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn package_polling_keeps_heartbeat_and_executor_lock_until_blocked_catalog_owner_finishes() {
    use ivy_host_runtime::executor::{ExecutorOptions, HostExecutor, PackageUpdateRuntime};
    let mut f = Fixture::new(vec![instance("domain", "domain", true)]).await;
    // A closed loopback endpoint makes the independent configuration request
    // finish before the deliberately blocked package call; no service is contacted.
    f.config
        .insert("publicBaseUrl", "http://127.0.0.1:9/ivy".into());
    f.journal.close().await.unwrap();
    let client = Arc::new(Client::default());
    client.block.store(true, Ordering::SeqCst);
    client.responses.lock().unwrap().extend([
        J::object([("status", J::Number(204.0))]),
        J::object([("status", J::Number(204.0))]),
    ]);
    let runtime = Arc::new(Automatic::default());
    let executor = HostExecutor::open_with_updates(
        f.config.clone(),
        f.config_path(),
        Some("fixture-credential".into()),
        ExecutorOptions {
            own_instance_id: None,
            account_home: node_path::from_native(&f.root.0),
            build_id: None,
        },
        Arc::new(super::configuration_updater_cases::NoActions),
        Arc::new(ivy_sdk::clock::SystemClock),
        None,
        PackageUpdateRuntime {
            node: Some(node()),
            client: client.clone(),
            automatic: runtime.clone(),
            compare: comparator(vec![]),
            distribution: f.root.0.clone(),
            job_launcher: job(),
        },
    )
    .await
    .unwrap();
    executor.start().unwrap();
    tokio::time::timeout(Duration::from_secs(10), client.entered.notified())
        .await
        .unwrap();
    client.release.notify_one();
    let hint_path = f.root.0.join("state/desired-state-hint.json");
    for (index, (configuration, packages)) in [
        (false.into(), J::object([] as [(&str, J); 0])),
        (J::Null, true.into()),
        (false.into(), false.into()),
    ]
    .into_iter()
    .enumerate()
    {
        let hint = J::object([
            ("schemaVersion", J::Number(1.0)),
            ("observedAt", format!("ignored-{index}").into()),
            ("configuration", configuration),
            ("packages", packages),
        ]);
        config::atomic_json(&hint_path, hint.clone()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(550)).await;
        assert_eq!(client.calls.lock().unwrap().len(), 1, "{hint:?}");
    }
    config::atomic_json(
        &hint_path,
        J::object([
            ("schemaVersion", J::Number(1.0)),
            ("observedAt", "package-update".into()),
            ("configuration", false.into()),
            ("packages", true.into()),
        ]),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), client.entered.notified())
        .await
        .unwrap();
    let status = f.root.0.join("state/executor.json");
    let before = read_optional(&status).await;
    let owner = executor.clone();
    let close = tokio::spawn(async move { owner.close().await });
    tokio::time::sleep(Duration::from_millis(1250)).await;
    assert!(!close.is_finished());
    let after = read_optional(&status).await;
    assert_eq!(after.get("state"), Some(&"ready".into()));
    assert_ne!(before.get("observedAt"), after.get("observedAt"));
    assert_eq!(
        ExecutorLock::acquire(f.root.0.join("state/executor"), false)
            .await
            .err()
            .unwrap()
            .code,
        "executor_already_running"
    );
    client.block.store(false, Ordering::SeqCst);
    client.release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), close)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    ExecutorLock::acquire(f.root.0.join("state/executor"), false)
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    assert_eq!(client.calls.lock().unwrap().len(), 2);
    assert!(runtime.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn package_updater_skips_unconfigured_app_hive_and_incompatible_packages_without_advancing_failed_catalogs()
 {
    let mut f = Fixture::new(vec![
        instance("domain", "domain", true),
        instance("hive-local", "hive", true),
    ])
    .await;
    let pair = f.release("domain", "1.0.0", "domain-old").await;
    f.install("domain", &pair).await;
    let mut incompatible = entry(&f.release("domain", "2.0.0", "future-domain").await, 2.0);
    let mut m = incompatible.required("manifest").unwrap().clone();
    let mut requirements = m.required("requirements").unwrap().clone();
    requirements.insert("node", ">=25.0.0 <26.0.0".into());
    m.insert("requirements", requirements);
    incompatible.insert("manifest", m);
    let missing = f.release("not-configured", "9.0.0", "no-instance").await;
    let mut hive = entry(&f.release("hive", "1.0.0", "hive").await, 1.0);
    let mut m = hive.required("manifest").unwrap().clone();
    m.insert("kind", "hive".into());
    m.insert("connectsToHive", true.into());
    hive.insert("manifest", m);
    let valid = catalog(
        vec![entry(&pair, 1.0), incompatible, entry(&missing, 1.0), hive],
        3.0,
    );
    let mut invalid = valid.clone();
    invalid.insert("revision", J::Number(-1.0));
    let input = f.input(
        "packages",
        vec![
            step(invalid),
            step(valid),
            J::object([("status", J::Number(204.0))]),
        ],
    );
    let original = oracle(&input);
    let actual = native_packages(&f, &input).await;
    assert!(
        actual
            .required("requests")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    record(
        "skip-runtime-hive-unconfigured",
        &input,
        &actual,
        &original,
        true,
    );
}

#[tokio::test]
async fn automatic_bootstrap_record_preserves_utf16_identity_and_original_validation_boundaries() {
    let f = Fixture::new(vec![instance("executor-renamed", "host-executor", true)]).await;
    for (index, (operation, value)) in vec![
        (J::from(""), None),
        (J::from("a".repeat(129)), None),
        (J::String(JsString(vec![0xd800, 47, 0xdc00])), None),
        (J::from("record"), Some(J::Null)),
        (
            J::from("record"),
            Some(J::object([
                ("schemaVersion", J::Number(1.0)),
                ("operationId", "record".into()),
                ("requestHash", J::Null),
            ])),
        ),
        (
            J::from("record"),
            Some(J::object([
                ("schemaVersion", J::Number(1.0)),
                ("operationId", "record".into()),
                ("requestHash", "sha256:anything".into()),
                ("phase", "retained-unknown-phase".into()),
                ("createdAt", "not-a-date".into()),
                ("updatedAt", "not-a-date".into()),
            ])),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut input = J::object([
            ("kind", "record".into()),
            ("config", f.config.clone()),
            ("operationId", operation.clone()),
        ]);
        if let Some(value) = value {
            input.insert("value", value);
        }
        let original = oracle(&input);
        let actual = wrap(
            async {
                let root = bootstrap_automatic::automatic_bootstrap_root(
                    &f.config,
                    operation.as_js_string().unwrap(),
                )?;
                // The original oracle wrote the same retained bytes to these isolated roots.
                Ok(J::object([
                    ("root", p(&root)),
                    (
                        "record",
                        bootstrap_automatic::automatic_bootstrap_record(
                            &f.config,
                            operation.as_js_string().unwrap(),
                        )
                        .await?
                        .unwrap_or(J::Null),
                    ),
                ]))
            }
            .await,
        );
        record(&format!("record-{index}"), &input, &actual, &original, true);
    }
}

struct Server {
    base: String,
    requests: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn server(archive: Vec<u8>) -> Server {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/prefix%20space/?old=yes#old",
        listener.local_addr().unwrap()
    );
    let requests = Arc::new(Mutex::new(vec![]));
    let seen = requests.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = vec![];
            loop {
                let mut chunk = [0; 1024];
                let count = socket.read(&mut chunk).await.unwrap();
                if count == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..count]);
                if bytes.windows(4).any(|v| v == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8(bytes).unwrap();
            seen.lock().unwrap().push(request.clone());
            let (status, body) = if request
                .starts_with("GET /prefix%20space/api/v1/packages/domain/1.0.0/artifact ")
            {
                ("200 OK", archive.clone())
            } else if request.contains("/missing/") {
                ("404 Not Found", vec![])
            } else {
                ("204 No Content", vec![])
            };
            let header = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(header.as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
            socket.shutdown().await.unwrap();
        }
    });
    Server {
        base,
        requests,
        task,
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn package_import_streams_real_archive_validates_and_replays_registered_and_unregistered_current_directories()
 {
    let mut f = Fixture::new(vec![instance("domain", "domain", true)]).await;
    let pair = f.release("domain", "1.0.0", "download-domain").await;
    let candidate = pair.required("candidate").unwrap();
    let source = path(candidate.required("manifestPath").unwrap())
        .parent()
        .unwrap()
        .to_owned();
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut tar = tar::Builder::new(encoder);
    tar.append_path_with_name(source.join("component.json"), "component.json")
        .unwrap();
    tar.append_dir_all("artifact", source.join("artifact"))
        .unwrap();
    let archive = tar.into_inner().unwrap().finish().unwrap();
    if let Some(corpus) = std::env::var_os("IVY_HOST_PACKAGE_UPDATER_CORPUS") {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(
                PathBuf::from(corpus)
                    .parent()
                    .unwrap()
                    .join("download-archive.tar.gz"),
            )
            .unwrap()
            .write_all(&archive)
            .unwrap();
    }
    let mut entry = entry(&pair, 1.0);
    entry.insert("archiveHash", ivy_contracts::hash::digest(&archive).into());
    entry.insert("bytes", J::Number(archive.len() as f64));
    let build = id(candidate, "candidateId");
    assert!(f.journal.retire_candidate(&build).await.unwrap());
    f.journal.finish_candidate_retirement(&build).await.unwrap();
    let server = server(archive).await;
    f.config.insert("publicBaseUrl", server.base.clone().into());
    f.journal.use_configuration(f.config.clone()).await.unwrap();
    let mut input = f.input("import", vec![]);
    input.insert("pairs", J::Array(vec![]));
    input.insert("entry", entry.clone());
    // Node's real global fetch and the native explicit no-proxy client use the
    // same isolated HTTP server and exact archive, sequentially at identical roots.
    let original = tokio::task::spawn_blocking({
        let input = input.clone();
        move || oracle(&input)
    })
    .await
    .unwrap();
    assert_eq!(
        original.required("result").unwrap().get("ok"),
        Some(&true.into())
    );
    let destination = path(f.config.required("artifactRoot").unwrap())
        .join("candidates")
        .join(&build[7..]);
    fs::remove_dir_all(&destination).unwrap();
    let client = Arc::new(NativePackageClient(
        reqwest::Client::builder().no_proxy().build().unwrap(),
    ));
    let updater = PackageUpdater::new(
        f.journal.clone(),
        "fixture-credential".into(),
        node(),
        client.clone(),
    );
    let imported = updater.import_package(entry.clone()).await.unwrap();
    let actual = J::object([
        ("result", wrap(Ok(imported.clone()))),
        (
            "saved",
            J::Array(vec![J::object([
                ("candidate", imported.clone()),
                ("manifest", f.journal.manifest(&build).await.unwrap()),
            ])]),
        ),
    ]);
    record(
        "real-global-fetch-vs-native-archive-import",
        &input,
        &actual,
        &original,
        true,
    );
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    for request in server.requests.lock().unwrap().iter() {
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer fixture-credential\r\n")
        );
        assert!(request.starts_with("GET /prefix%20space/api/v1/packages/domain/1.0.0/artifact "));
    }
    assert_json_eq(
        &updater.import_package(entry.clone()).await.unwrap(),
        &imported,
    );
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    assert!(f.journal.retire_candidate(&build).await.unwrap());
    f.journal.finish_candidate_retirement(&build).await.unwrap();
    assert_json_eq(
        &updater.import_package(entry.clone()).await.unwrap(),
        &imported,
    );
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    let mut wrong = entry.clone();
    wrong.insert("archiveHash", hash("changed-archive"));
    assert_eq!(
        updater.import_package(wrong).await.unwrap_err().code,
        "artifact_changed"
    );
    assert!(
        client
            .catalog(server.base.clone(), "fixture-credential".into(), 7.0)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        server
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .starts_with("GET /prefix%20space/api/v1/packages/catalog?after=7 ")
    );
    assert!(
        !path(f.config.required("stagingRoot").unwrap())
            .join(format!(
                "package-{}-extract",
                &id(&entry, "archiveHash")[7..]
            ))
            .exists()
    );
    assert!(
        !path(f.config.required("artifactRoot").unwrap())
            .join(format!(
                "incoming-package-{}",
                &id(&entry, "archiveHash")[7..]
            ))
            .exists()
    );
    let mut missing = entry;
    missing.insert("componentId", "missing".into());
    assert_eq!(
        client
            .download(
                server.base.clone(),
                "fixture-credential".into(),
                missing,
                f.root.0.join("must-not-exist")
            )
            .await
            .unwrap_err()
            .code,
        "not_found"
    );
    assert!(!f.root.0.join("must-not-exist").exists());
    assert!(f.journal.retire_candidate(&build).await.unwrap());
    f.journal.finish_candidate_retirement(&build).await.unwrap();
    fs::remove_dir_all(&destination).unwrap();
    let good_entry = input.required("entry").unwrap().clone();
    for failure in ["changed-archive", "stale-download"] {
        let mut selected = good_entry.clone();
        if failure == "changed-archive" {
            selected.insert("archiveHash", hash("unexpected-download"));
        }
        let temporary = path(f.config.required("stagingRoot").unwrap()).join(format!(
            "package-{}.tar.gz",
            &id(&selected, "archiveHash")[7..]
        ));
        let mut failed_input = input.clone();
        failed_input.insert("entry", selected.clone());
        if failure == "stale-download" {
            fs::write(&temporary, b"retained interrupted download").unwrap();
        }
        let expected = tokio::task::spawn_blocking({
            let input = failed_input.clone();
            move || oracle(&input)
        })
        .await
        .unwrap();
        assert_eq!(
            expected.required("result").unwrap().get("ok"),
            Some(&false.into())
        );
        assert!(!temporary.exists());
        if failure == "stale-download" {
            fs::write(&temporary, b"retained interrupted download").unwrap();
        }
        let result = wrap(updater.import_package(selected).await);
        let actual = J::object([("result", result), ("saved", J::Array(vec![]))]);
        assert!(!temporary.exists());
        assert!(!destination.exists());
        assert_eq!(
            f.journal.candidate(&build).await.unwrap_err().code,
            "not_found"
        );
        record(failure, &failed_input, &actual, &expected, true);
    }
}
