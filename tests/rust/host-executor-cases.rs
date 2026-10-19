use super::{ServiceFixture, assert_json_eq, host_configuration};
use ivy_contracts::{core_schema::validate_host, json};
use ivy_host_runtime::{
    IvyError, IvyJson, config,
    executor::{
        self, ActivationRuntime, ExecutorFuture, ExecutorOptions, HostExecutor, OperationContext,
    },
    journal::HostJournal,
};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

fn path(value: &IvyJson) -> PathBuf {
    ivy_host_runtime::node_path::to_native(value.as_js_string().unwrap())
}
fn hash(c: &str) -> IvyJson {
    format!("sha256:{}", c.repeat(64)).into()
}
fn text(v: &IvyJson, k: &str) -> String {
    v.required(k).unwrap().as_str().unwrap()
}
fn field<'a>(v: &'a IvyJson, k: &str) -> &'a IvyJson {
    static NULL: IvyJson = IvyJson::Null;
    v.get(k).unwrap_or(&NULL)
}
fn options(f: &ServiceFixture) -> ExecutorOptions {
    ExecutorOptions {
        own_instance_id: Some("independent-executor".into()),
        account_home: f.0.clone().to_string_lossy().to_string().into(),
        build_id: None,
    }
}
fn oracle(input: IvyJson) -> IvyJson {
    let mut child = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/host-executor-oracle.mjs",
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
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    json::parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
}
static CORPUS: Mutex<()> = Mutex::new(());
fn pair(case: &str, actual: &IvyJson, expected: &IvyJson) {
    if let Some(file) = std::env::var_os("IVY_HOST_EXECUTOR_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        writeln!(
            fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(file)
                .unwrap(),
            "{}",
            json::stringify(
                &IvyJson::object([
                    ("case", case.into()),
                    ("actual", actual.clone()),
                    ("expected", expected.clone())
                ]),
                usize::MAX
            )
            .unwrap()
        )
        .unwrap();
    }
    assert_json_eq(actual, expected);
}

async fn candidate(f: &ServiceFixture, letter: &str, component: &str) -> (IvyJson, IvyJson) {
    let mut candidate = super::journal_cases::candidate();
    let mut manifest = super::journal_cases::manifest();
    let root = f.0.join("releases").join(letter);
    fs::create_dir_all(root.join("dist")).unwrap();
    for key in ["candidateId", "buildId"] {
        candidate.insert(key, hash(letter));
    }
    candidate.insert("componentId", component.into());
    candidate.insert("artifactRoot", root.to_string_lossy().to_string().into());
    candidate.insert(
        "manifestPath",
        root.join("manifest.json")
            .to_string_lossy()
            .to_string()
            .into(),
    );
    manifest.insert("componentId", component.into());
    manifest.insert("buildId", hash(letter));
    manifest.insert("kind", "native".into());
    manifest.remove("app");
    let command = IvyJson::object([
        ("executable", "node".into()),
        ("args", IvyJson::Array(vec!["dist/main.js".into()])),
        ("timeoutMs", IvyJson::Number(1000.0)),
    ]);
    manifest.insert("entrypoint", command.clone());
    manifest.insert(
        "readiness",
        IvyJson::object([("timeoutMs", IvyJson::Number(1000.0)), ("command", command)]),
    );
    manifest.insert(
        "shutdown",
        IvyJson::object([("timeoutMs", IvyJson::Number(100.0))]),
    );
    manifest.insert(
        "restart",
        IvyJson::object([
            ("policy", "never".into()),
            ("minimumDelayMs", IvyJson::Number(100.0)),
            ("maximumDelayMs", IvyJson::Number(100.0)),
        ]),
    );
    validate_host("ReleaseManifest", &manifest).unwrap();
    config::atomic_json(root.join("manifest.json"), manifest.clone())
        .await
        .unwrap();
    (candidate, manifest)
}
fn host(f: &ServiceFixture) -> IvyJson {
    let mut host = host_configuration(f);
    let mut executables = host.required("executables").unwrap().clone();
    executables.insert("tool", "original-tool".into());
    host.insert("executables", executables);
    let mut instances = host
        .required("instances")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    instances[0].insert("settings", IvyJson::object([("marker", "original".into())]));
    host.insert("instances", IvyJson::Array(instances));
    host
}
fn request(id: &str, candidate: IvyJson, operation: &str) -> IvyJson {
    IvyJson::object([
        ("action", "deploy".into()),
        ("instanceId", id.into()),
        ("candidateId", candidate),
        ("operationId", operation.into()),
    ])
}

struct Model {
    scenario: String,
    recording: AtomicBool,
    trace: Mutex<Vec<IvyJson>>,
    contexts: Mutex<Vec<(String, IvyJson)>>,
    gates: BTreeMap<String, Arc<tokio::sync::Semaphore>>,
    entered: Mutex<HashSet<String>>,
    cancel_phase: Mutex<Option<String>>,
}
impl Model {
    fn new(scenario: &str) -> Self {
        Self {
            scenario: scenario.into(),
            recording: AtomicBool::new(false),
            trace: Mutex::new(vec![]),
            contexts: Mutex::new(vec![]),
            gates: BTreeMap::new(),
            entered: Mutex::new(HashSet::new()),
            cancel_phase: Mutex::new(None),
        }
    }
    async fn phase(&self, c: &OperationContext) -> String {
        c.journal
            .unfinished()
            .await
            .unwrap()
            .iter()
            .find(|e| {
                field(field(e, "record"), "instanceId") == &IvyJson::from(c.instance_id.clone())
            })
            .map(|e| text(field(e, "record"), "phase"))
            .unwrap_or_else(|| "terminal".into())
    }
}
impl ActivationRuntime for Model {
    fn prepare<'a>(
        &'a self,
        _: &'a OperationContext,
        _: &'a IvyJson,
        _: &'a str,
    ) -> ExecutorFuture<'a, IvyJson> {
        Box::pin(async {
            Err(IvyError::new(
                "source_file_changed",
                "Fixture captured source changed.",
            ))
        })
    }
    fn compatibility<'a>(
        &'a self,
        c: &'a OperationContext,
        candidate: &'a str,
        settings: Option<IvyJson>,
    ) -> ExecutorFuture<'a, ()> {
        Box::pin(async move {
            let phase = self.phase(c).await;
            self.contexts
                .lock()
                .unwrap()
                .push((c.instance_id.clone(), c.config.clone()));
            if self.recording.load(Ordering::SeqCst) {
                self.trace.lock().unwrap().push(IvyJson::object([
                    ("operation", "compatibility".into()),
                    ("phase", phase.clone().into()),
                    ("candidate", candidate.into()),
                    (
                        "tool",
                        c.config.required("executables")?.required("tool")?.clone(),
                    ),
                    ("settings", settings.clone().unwrap_or(IvyJson::Null)),
                ]));
            }
            let first = self.entered.lock().unwrap().insert(c.instance_id.clone());
            if first {
                if let Some(gate) = self.gates.get(&c.instance_id) {
                    gate.acquire()
                        .await
                        .map_err(|_| IvyError::internal())?
                        .forget();
                }
            }
            if self.recording.load(Ordering::SeqCst)
                && ((self.scenario == "precheck" && phase == "checking")
                    || (self.scenario == "postdrain" && phase == "draining")
                    || (self.scenario == "rollback-incompatible" && settings.is_some()))
            {
                return Err(IvyError::new(
                    "incompatible_contract",
                    "Fixture compatibility refused.",
                ));
            }
            Ok(())
        })
    }
    fn reconcile<'a>(
        &'a self,
        c: &'a OperationContext,
        target: Option<IvyJson>,
        allow_start: bool,
    ) -> ExecutorFuture<'a, ()> {
        Box::pin(async move {
            let phase = self.phase(c).await;
            if self.cancel_phase.lock().unwrap().as_deref() == Some(phase.as_str()) {
                c.cancel.cancel();
                return Err(IvyError::new("cancelled", "Fixture cancellation"));
            }
            if self.recording.load(Ordering::SeqCst) {
                self.trace.lock().unwrap().push(IvyJson::object([
                    ("operation", "reconcile".into()),
                    ("phase", phase.into()),
                    (
                        "candidate",
                        target
                            .as_ref()
                            .map(|t| field(t, "candidateId"))
                            .unwrap_or(&IvyJson::Null)
                            .clone(),
                    ),
                    (
                        "desired",
                        target
                            .as_ref()
                            .map(|t| field(t, "desired"))
                            .unwrap_or(&IvyJson::Null)
                            .clone(),
                    ),
                    ("allowStart", allow_start.into()),
                ]));
            }
            let mut state = match target.as_ref() {
                None => "idle",
                Some(t) if field(t, "desired") == &"stopped".into() => "stopped",
                _ => "ready",
            };
            if self.recording.load(Ordering::SeqCst)
                && ["rollback", "rollback-incompatible", "no-previous-failure"]
                    .contains(&self.scenario.as_str())
                && target.as_ref().is_some_and(|t| {
                    field(t, "candidateId") == &hash("b")
                        && field(t, "desired") == &"running".into()
                })
            {
                state = "failed";
            }
            let at = chrono::DateTime::from_timestamp_millis(c.clock.utc_millis())
                .unwrap()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            let candidate = target
                .as_ref()
                .map(|t| field(t, "candidateId"))
                .unwrap_or(&IvyJson::Null)
                .clone();
            let revision = target
                .as_ref()
                .map(|t| field(t, "revision"))
                .unwrap_or(&IvyJson::Null)
                .clone();
            let health = if state == "ready" {
                IvyJson::object([
                    ("schemaVersion", IvyJson::Number(1.0)),
                    ("instanceId", c.instance_id.clone().into()),
                    (
                        "componentId",
                        c.journal
                            .instance(&c.instance_id)
                            .await?
                            .required("componentId")?
                            .clone(),
                    ),
                    ("buildId", candidate.clone()),
                    ("pid", IvyJson::Number(1.0)),
                    ("bootId", "fixture-boot".into()),
                    ("startedAt", at.clone().into()),
                    ("observedAt", at.clone().into()),
                    ("ready", true.into()),
                    ("generation", IvyJson::Null),
                    ("launchId", revision.clone()),
                    ("details", "".into()),
                ])
            } else {
                IvyJson::Null
            };
            c.journal
                .record_observation(
                    &c.instance_id,
                    IvyJson::object([
                        ("schemaVersion", IvyJson::Number(1.0)),
                        ("instanceId", c.instance_id.clone().into()),
                        ("ownerPid", IvyJson::Number(1.0)),
                        ("ownerBootId", "fixture-owner".into()),
                        ("observedAt", at.into()),
                        ("targetRevision", revision),
                        ("candidateId", candidate.clone()),
                        ("buildId", candidate),
                        ("state", state.into()),
                        ("health", health),
                        ("restartCount", IvyJson::Number(0.0)),
                        ("nextRestartAt", IvyJson::Null),
                        (
                            "code",
                            if state == "failed" {
                                "fixture_failed".into()
                            } else {
                                IvyJson::Null
                            },
                        ),
                        (
                            "message",
                            if state == "failed" {
                                "Fixture launch failed.".into()
                            } else {
                                "".into()
                            },
                        ),
                    ]),
                )
                .await
        })
    }
    fn dispose(&self) -> ExecutorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
async fn project(journal: &HostJournal, id: &str, model: &Model) -> IvyJson {
    let e = journal.get(id).await.unwrap();
    let target = journal.target("fixture").await.unwrap();
    let installed = journal.installed("fixture").await.unwrap();
    let target_value = if let Some(t) = &target {
        let config = config::json_file(path(t.required("configPath").unwrap()), 1024 * 1024)
            .await
            .unwrap();
        IvyJson::object([
            ("candidateId", t.required("candidateId").unwrap().clone()),
            ("desired", t.required("desired").unwrap().clone()),
            ("settings", config.required("settings").unwrap().clone()),
        ])
    } else {
        IvyJson::Null
    };
    let mut value = IvyJson::object([
        ("phase", field(field(&e, "record"), "phase").clone()),
        ("errorCode", field(field(&e, "record"), "errorCode").clone()),
        ("readiness", field(field(&e, "record"), "readiness").clone()),
        (
            "observedBuild",
            field(field(&e, "record"), "observedBuild").clone(),
        ),
        ("target", target_value),
        (
            "installed",
            installed
                .map(|i| {
                    IvyJson::object(
                        ["candidateId", "buildId", "enabled"].map(|k| (k, field(&i, k).clone())),
                    )
                })
                .unwrap_or(IvyJson::Null),
        ),
        (
            "rollbackRevisionMatches",
            if field(field(&e, "activation"), "rollbackTargetRevision").is_null() {
                IvyJson::Null
            } else {
                (target.as_ref().is_some_and(|t| {
                    t.get("revision") == field(&e, "activation").get("rollbackTargetRevision")
                }))
                .into()
            },
        ),
        ("trace", IvyJson::Array(model.trace.lock().unwrap().clone())),
    ]);
    if field(&e, "record").get("errorCode").is_none() {
        value.remove("errorCode");
    }
    value
}

#[tokio::test]
async fn actual_journal_activation_and_rollback_match_original_including_saved_prior_settings() {
    for scenario in [
        "success",
        "disabled",
        "precheck",
        "postdrain",
        "rollback",
        "rollback-incompatible",
        "no-previous-failure",
    ] {
        let f = ServiceFixture::new();
        let original_host = host(&f);
        let journal = HostJournal::open(original_host.clone()).await.unwrap();
        let good = candidate(&f, "a", "fixture").await;
        let bad = candidate(&f, "b", "fixture").await;
        for (c, m) in [good.clone(), bad.clone()] {
            journal.save_candidate(c, m).await.unwrap();
        }
        let model = Arc::new(Model::new(scenario));
        let options = options(&f);
        if scenario.starts_with("rollback") {
            let entry = journal
                .accept(request("fixture", hash("a"), "initial"), None)
                .await
                .unwrap();
            executor::execute_deployment(
                journal.clone(),
                entry.clone(),
                model.clone(),
                options.clone(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
            assert_eq!(
                text(
                    field(
                        &journal
                            .get(&text(field(&entry, "record"), "deploymentId"))
                            .await
                            .unwrap(),
                        "record"
                    ),
                    "phase"
                ),
                "succeeded"
            );
        }
        let mut desired = original_host.clone();
        let mut executables = desired.required("executables").unwrap().clone();
        executables.insert("tool", "updated-tool".into());
        desired.insert("executables", executables);
        let mut instances = desired
            .required("instances")
            .unwrap()
            .as_array()
            .unwrap()
            .to_vec();
        instances[0].insert("settings", IvyJson::object([("marker", "updated".into())]));
        if scenario == "disabled" {
            instances[0].insert("enabled", false.into());
        }
        desired.insert("instances", IvyJson::Array(instances));
        journal.use_configuration(desired).await.unwrap();
        let selected = if ["success", "disabled", "precheck", "postdrain"].contains(&scenario) {
            hash("a")
        } else {
            hash("b")
        };
        let entry = journal
            .accept(request("fixture", selected, "selected"), None)
            .await
            .unwrap();
        let id = text(field(&entry, "record"), "deploymentId");
        if scenario.starts_with("rollback") {
            let path = path(field(
                field(field(&entry, "activation"), "previousTarget"),
                "configPath",
            ));
            let mut old = config::json_file(path.clone(), 1024 * 1024).await.unwrap();
            old.insert("settings", IvyJson::object([("marker", "damaged".into())]));
            config::atomic_json(path, old).await.unwrap();
        }
        model.recording.store(true, Ordering::SeqCst);
        executor::execute_deployment(
            journal.clone(),
            entry,
            model.clone(),
            options,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let actual = project(&journal, &id, &model).await;
        let mut oracle_host = original_host;
        oracle_host.insert(
            "runtimeRoot",
            f.0.join("original-state")
                .to_string_lossy()
                .to_string()
                .into(),
        );
        oracle_host.insert(
            "stagingRoot",
            f.0.join("original-staging")
                .to_string_lossy()
                .to_string()
                .into(),
        );
        let input = IvyJson::object([
            ("kind", "lane".into()),
            ("scenario", scenario.into()),
            ("config", oracle_host),
            ("goodId", hash("a")),
            ("badId", hash("b")),
            (
                "candidates",
                IvyJson::Array(
                    [good, bad]
                        .into_iter()
                        .map(|(candidate, manifest)| {
                            IvyJson::object([("candidate", candidate), ("manifest", manifest)])
                        })
                        .collect(),
                ),
            ),
        ]);
        let expected = oracle(input);
        pair(scenario, &actual, &expected);
        journal.close().await.unwrap();
    }
}

#[test]
fn observation_and_readiness_boundaries_match_original_functions() {
    let mut cases = Vec::new();
    let mut actual = Vec::new();
    let mut add = |operation: &str, args: Vec<IvyJson>, result: IvyJson| {
        cases.push(IvyJson::object([
            ("operation", operation.into()),
            ("args", IvyJson::Array(args)),
        ]));
        actual.push(result);
    };
    for (previous, at) in [(0.0, 500.0), (1.0, 1000.0), (-1.0, 500.0)] {
        add(
            "readinessFailureStart",
            vec![IvyJson::Number(previous), IvyJson::Number(at)],
            IvyJson::Number(executor::readiness_failure_start(previous, at)),
        );
    }
    for (start, timeout, at) in [
        (0.0, 10.0, 100.0),
        (10.0, 100.0, 110.0),
        (10.0, 100.0, 111.0),
    ] {
        add(
            "readinessExpired",
            vec![
                IvyJson::Number(start),
                IvyJson::Number(timeout),
                IvyJson::Number(at),
            ],
            executor::readiness_expired(start, timeout, at).into(),
        );
    }
    for (current, installed) in [
        (None, None),
        (Some("a"), None),
        (Some("a"), Some("a")),
        (Some("b"), Some("a")),
    ] {
        add(
            "replacementNeedsSettledObservation",
            vec![
                current.map(IvyJson::from).unwrap_or(IvyJson::Null),
                installed.map(IvyJson::from).unwrap_or(IvyJson::Null),
            ],
            executor::replacement_needs_settled_observation(current, installed).into(),
        );
    }
    let idle = IvyJson::object([
        ("state", "idle".into()),
        ("targetRevision", IvyJson::Null),
        ("candidateId", IvyJson::Null),
        ("buildId", IvyJson::Null),
    ]);
    let target = IvyJson::object([
        ("revision", "selected".into()),
        ("candidateId", hash("a")),
        ("desired", "stopped".into()),
    ]);
    let stopped = IvyJson::object([
        ("state", "stopped".into()),
        ("targetRevision", "selected".into()),
        ("candidateId", hash("a")),
        ("observedAt", "old".into()),
    ]);
    for (observed, target) in [
        (None, None),
        (Some(idle), None),
        (Some(stopped.clone()), Some(target.clone())),
        (Some(stopped.clone()), None),
    ] {
        add(
            "stoppedTargetIsConclusive",
            vec![
                observed.clone().unwrap_or(IvyJson::Null),
                target.clone().unwrap_or(IvyJson::Null),
            ],
            executor::stopped_target_is_conclusive(observed.as_ref(), target.as_ref()).into(),
        );
    }
    let mut ready = stopped;
    ready.insert("state", "ready".into());
    let mut running = target;
    running.insert("desired", "running".into());
    for target in [None, Some(running.clone())] {
        add(
            "runningHiveCanBeInspected",
            vec![ready.clone(), target.clone().unwrap_or(IvyJson::Null)],
            executor::running_hive_can_be_inspected(Some(&ready), target.as_ref()).into(),
        );
    }
    drop(add);
    let at = 1_767_225_600_000i64;
    for delta in [-15_000, -14_999, 1000, 1001] {
        let observed = IvyJson::object([(
            "observedAt",
            chrono::DateTime::from_timestamp_millis(at + delta)
                .unwrap()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                .into(),
        )]);
        cases.push(IvyJson::object([
            ("operation", "fresh".into()),
            ("at", IvyJson::Number(at as f64)),
            ("args", IvyJson::Array(vec![observed.clone()])),
        ]));
        actual.push(executor::fresh_observation(&observed, at as f64).into());
    }
    let expected = oracle(IvyJson::object([
        ("kind", "policy".into()),
        ("cases", IvyJson::Array(cases)),
    ]));
    pair(
        "observation-readiness-boundaries",
        &IvyJson::Array(actual),
        &expected,
    );
}

#[tokio::test]
async fn cancellation_preserves_each_durable_phase_and_restarts_without_replacing_launch_identity()
{
    for phase in ["draining", "activating", "verifying", "rolling_back"] {
        let f = ServiceFixture::new();
        let journal = HostJournal::open(host(&f)).await.unwrap();
        for (c, m) in [
            candidate(&f, "a", "fixture").await,
            candidate(&f, "b", "fixture").await,
        ] {
            journal.save_candidate(c, m).await.unwrap();
        }
        let model = Arc::new(Model::new(if phase == "rolling_back" {
            "rollback"
        } else {
            "success"
        }));
        let options = options(&f);
        if phase == "rolling_back" {
            let initial = journal
                .accept(request("fixture", hash("a"), "initial"), None)
                .await
                .unwrap();
            executor::execute_deployment(
                journal.clone(),
                initial,
                model.clone(),
                options.clone(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        }
        *model.cancel_phase.lock().unwrap() = Some(phase.into());
        model.recording.store(true, Ordering::SeqCst);
        let selected = if phase == "rolling_back" {
            hash("b")
        } else {
            hash("a")
        };
        let entry = journal
            .accept(request("fixture", selected, "interrupted"), None)
            .await
            .unwrap();
        let id = text(field(&entry, "record"), "deploymentId");
        executor::execute_deployment(
            journal.clone(),
            entry,
            model.clone(),
            options.clone(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let retained = journal.get(&id).await.unwrap();
        assert_eq!(text(field(&retained, "record"), "phase"), phase);
        let prior = journal.target("fixture").await.unwrap();
        *model.cancel_phase.lock().unwrap() = None;
        executor::execute_deployment(
            journal.clone(),
            retained,
            model.clone(),
            options,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            text(field(&journal.get(&id).await.unwrap(), "record"), "phase"),
            if phase == "rolling_back" {
                "rolled_back"
            } else {
                "succeeded"
            }
        );
        if ["activating", "verifying"].contains(&phase) {
            assert_eq!(
                journal
                    .target("fixture")
                    .await
                    .unwrap()
                    .unwrap()
                    .get("revision"),
                prior.unwrap().get("revision")
            );
        }
        journal.close().await.unwrap();
    }
}

async fn until(mut check: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(8);
    while !check() {
        assert!(Instant::now() < end, "Fixture boundary was not reached");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn dropping_an_accepted_lane_waiter_keeps_the_real_journal_mutation_owned() {
    let f = ServiceFixture::new();
    let journal = HostJournal::open(host(&f)).await.unwrap();
    let (c, m) = candidate(&f, "a", "fixture").await;
    journal.save_candidate(c, m).await.unwrap();
    let entry = journal
        .accept(request("fixture", hash("a"), "abandoned-waiter"), None)
        .await
        .unwrap();
    let id = text(field(&entry, "record"), "deploymentId");
    let mut model = Model::new("success");
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    model.gates.insert("fixture".into(), gate.clone());
    let model = Arc::new(model);
    let base = journal.clone();
    let runtime = model.clone();
    let options = options(&f);
    let waiter = tokio::spawn(async move {
        executor::execute_deployment(base, entry, runtime, options, CancellationToken::new()).await
    });
    until(|| model.entered.lock().unwrap().contains("fixture")).await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    gate.add_permits(1);
    let end = Instant::now() + Duration::from_secs(8);
    loop {
        if text(field(&journal.get(&id).await.unwrap(), "record"), "phase") == "succeeded" {
            break;
        }
        assert!(Instant::now() < end);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    journal.close().await.unwrap();
}

struct QueueClock(AtomicI64);
impl ivy_sdk::clock::Clock for QueueClock {
    fn utc_millis(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}

#[tokio::test]
async fn coordinator_holds_two_lanes_fair_barrier_and_accepted_config_through_abandoned_close() {
    let f = ServiceFixture::new();
    let mut config = host(&f);
    let original = config.required("instances").unwrap().as_array().unwrap()[0].clone();
    let mut instances = Vec::new();
    for id in ["first", "second", "infra", "tail"] {
        let mut instance = original.clone();
        instance.insert("instanceId", id.into());
        instance.insert("serviceNodeId", format!("fixture-{id}").into());
        if id == "infra" {
            instance.insert("componentId", "host-executor".into());
        }
        instances.push(instance);
    }
    config.insert("instances", IvyJson::Array(instances));
    let clock = Arc::new(QueueClock(AtomicI64::new(1_767_225_600_000)));
    let journal = HostJournal::open_with_clock(config.clone(), clock.clone())
        .await
        .unwrap();
    for (c, m) in [
        candidate(&f, "a", "fixture").await,
        candidate(&f, "c", "host-executor").await,
        candidate(&f, "d", "fixture").await,
    ] {
        journal.save_candidate(c, m).await.unwrap();
    }
    let mut ids = Vec::new();
    for id in ["first", "second", "infra", "tail"] {
        if id == "second" {
            let mut ex = config.required("executables").unwrap().clone();
            ex.insert("tool", "updated-tool".into());
            config.insert("executables", ex);
            journal.use_configuration(config.clone()).await.unwrap();
        }
        clock.0.fetch_add(1, Ordering::SeqCst);
        let entry = journal
            .accept(
                request(
                    id,
                    hash(if id == "infra" {
                        "c"
                    } else if id == "tail" {
                        "d"
                    } else {
                        "a"
                    }),
                    id,
                ),
                None,
            )
            .await
            .unwrap();
        ids.push((id, text(field(&entry, "record"), "deploymentId")));
    }
    let mut model = Model::new("success");
    for id in ["first", "second", "infra", "tail"] {
        model
            .gates
            .insert(id.into(), Arc::new(tokio::sync::Semaphore::new(0)));
    }
    model.recording.store(true, Ordering::SeqCst);
    let model = Arc::new(model);
    let owner = HostExecutor::open_with_clock(config.clone(), options(&f), model.clone(), clock)
        .await
        .unwrap();
    let tail_manifest = journal
        .manifest(&hash("d").as_str().unwrap())
        .await
        .unwrap();
    let db = ivy_store::rusqlite::Connection::open(
        path(config.required("runtimeRoot").unwrap()).join("deployments.sqlite"),
    )
    .unwrap();
    db.execute(
        "UPDATE candidates SET manifest_json='{}' WHERE candidate_id=?",
        [hash("d").as_str().unwrap()],
    )
    .unwrap();
    owner.start().unwrap();
    owner.start().unwrap();
    until(|| {
        let entered = model.entered.lock().unwrap();
        entered.contains("first") && entered.contains("second")
    })
    .await;
    assert!(!model.entered.lock().unwrap().contains("infra"));
    assert!(!model.entered.lock().unwrap().contains("tail"));
    model.gates["first"].add_permits(1);
    model.gates["second"].add_permits(1);
    until(|| model.entered.lock().unwrap().contains("infra")).await;
    assert!(!model.entered.lock().unwrap().contains("tail"));
    // A later corrupt manifest cannot be inspected ahead of the barrier. It is
    // repaired only after both ordinary lanes and the barrier have been admitted.
    db.execute(
        "UPDATE candidates SET manifest_json=? WHERE candidate_id=?",
        [
            json::canonical(&tail_manifest, usize::MAX).unwrap(),
            hash("d").as_str().unwrap(),
        ],
    )
    .unwrap();
    drop(db);
    model.gates["infra"].add_permits(1);
    until(|| model.entered.lock().unwrap().contains("tail")).await;
    {
        let contexts = model.contexts.lock().unwrap();
        assert!(
            contexts
                .iter()
                .filter(|(id, _)| id == "first")
                .all(|(_, c)| field(field(c, "executables"), "tool") == &"original-tool".into())
        );
        assert!(
            contexts
                .iter()
                .filter(|(id, _)| id == "second")
                .all(|(_, c)| field(field(c, "executables"), "tool") == &"updated-tool".into())
        );
    }
    assert_eq!(
        field(
            field(&journal.config().await.unwrap(), "executables"),
            "tool"
        ),
        &"updated-tool".into()
    );
    let first_target = journal.target("first").await.unwrap().unwrap();
    let accepted = journal
        .accepted_target_policy(first_target)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        field(field(&accepted, "executables"), "tool"),
        &"original-tool".into()
    );
    let waiting = owner.clone();
    let close = tokio::spawn(async move { waiting.close().await });
    tokio::time::sleep(Duration::from_millis(40)).await;
    close.abort();
    assert!(close.await.unwrap_err().is_cancelled());
    let locked = ivy_host_runtime::executor_lock::ExecutorLock::acquire(
        path(config.required("runtimeRoot").unwrap()).join("executor"),
        false,
    )
    .await
    .err()
    .unwrap();
    assert_eq!(locked.code, "executor_already_running");
    model.gates["tail"].add_permits(1);
    tokio::time::timeout(Duration::from_secs(10), owner.close())
        .await
        .unwrap()
        .unwrap();
    owner.close().await.unwrap();
    let status = config::json_file(
        path(config.required("runtimeRoot").unwrap()).join("executor.json"),
        1024 * 1024,
    )
    .await
    .unwrap();
    assert_eq!(field(&status, "state"), &"stopping".into());
    let tail_id = &ids.iter().find(|(id, _)| *id == "tail").unwrap().1;
    let pending = journal.get(tail_id).await.unwrap();
    assert_eq!(text(field(&pending, "record"), "phase"), "draining");
    executor::execute_deployment(
        journal.clone(),
        pending,
        model.clone(),
        options(&f),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        text(
            field(&journal.get(tail_id).await.unwrap(), "record"),
            "phase"
        ),
        "succeeded"
    );
    let next = HostExecutor::open(config, options(&f), model)
        .await
        .unwrap();
    next.close().await.unwrap();
    journal.close().await.unwrap();
}

#[tokio::test]
async fn source_preparation_failure_is_terminal_before_drain_and_never_replays_the_capture() {
    let f = ServiceFixture::new();
    let journal = HostJournal::open(host(&f)).await.unwrap();
    let source = path(host(&f).required("stagingRoot").unwrap()).join("captured");
    let snapshot = IvyJson::object([
        ("snapshotId", "captured-id".into()),
        ("sourceRoot", source.to_string_lossy().to_string().into()),
        (
            "originalRoot",
            f.0.join("original").to_string_lossy().to_string().into(),
        ),
        (
            "manifestPath",
            source
                .join("snapshot.json")
                .to_string_lossy()
                .to_string()
                .into(),
        ),
        ("capturedAt", "2026-01-01T00:00:00.000Z".into()),
    ]);
    let request = IvyJson::object([
        ("action", "deploy".into()),
        ("instanceId", "fixture".into()),
        ("operationId", "captured-operation".into()),
        (
            "source",
            f.0.join("original").to_string_lossy().to_string().into(),
        ),
    ]);
    let entry = journal
        .accept(request.clone(), Some(snapshot))
        .await
        .unwrap();
    let id = text(field(&entry, "record"), "deploymentId");
    let model = Arc::new(Model::new("success"));
    executor::execute_deployment(
        journal.clone(),
        entry,
        model,
        options(&f),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let retained = journal.get(&id).await.unwrap();
    assert_eq!(text(field(&retained, "record"), "phase"), "failed");
    assert_eq!(
        field(field(&retained, "record"), "errorCode"),
        &"source_file_changed".into()
    );
    assert!(journal.target("fixture").await.unwrap().is_none());
    assert_json_eq(&journal.accept(request, None).await.unwrap(), &retained);
    journal.close().await.unwrap();
}
