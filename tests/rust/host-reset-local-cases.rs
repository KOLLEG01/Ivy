use super::{ServiceFixture, assert_json_eq, host_configuration};
use ivy_contracts::{IvyJson as J, IvyResult, JsString, json};
use ivy_host_runtime::{journal::ExecutorLock, node_path, runtime_reset};
use ivy_sdk::clock::{Clock, parse_date};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
    },
    time::Instant,
};
const DATE: &str = "2026-10-20T12:00:00.000Z";
static CORPUS: Mutex<()> = Mutex::new(());
struct FixedClock;
impl Clock for FixedClock {
    fn utc_millis(&self) -> i64 {
        parse_date(&DATE.into()) as i64
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
struct DeadlineClock(AtomicI64);
impl Clock for DeadlineClock {
    fn utc_millis(&self) -> i64 {
        self.0.fetch_add(20_000, Ordering::SeqCst)
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
fn oracle(
    fixture: &ServiceFixture,
    group: &str,
    case: &str,
    kind: &str,
    extra: Vec<(&str, J)>,
) -> J {
    let mut input = J::object([
        ("fixtureRoot", J::String(node_path::from_native(&fixture.0))),
        ("group", group.into()),
        ("case", case.into()),
        ("kind", kind.into()),
        ("config", host_configuration(fixture)),
        ("candidate", super::journal_cases::candidate()),
        ("manifest", super::journal_cases::manifest()),
    ]);
    for (key, value) in extra {
        input.insert(key, value);
    }
    let mut child =
        Command::new(std::env::var_os("IVY_RESTORE_NODE").unwrap_or_else(|| "node".into()))
            .args([
                "--experimental-transform-types",
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/host-reset-local-oracle.mjs",
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
        "{group}/{case}/{kind}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    json::parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
}
fn field(value: &J, key: &str) -> JsString {
    value.get(key).unwrap().as_js_string().unwrap().clone()
}
fn wrap(result: IvyResult<J>) -> J {
    match result {
        Ok(value) => J::object([("ok", true.into()), ("value", value)]),
        Err(error) => J::object([("ok", false.into()), ("error", error.to_wire())]),
    }
}
fn retain(group: &str, case: &str, setup: J, actual: J, expected: J) {
    if let Some(path) = std::env::var_os("IVY_HOST_RESET_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(
            file,
            "{}",
            json::canonical(
                &J::object([
                    ("group", group.into()),
                    ("case", case.into()),
                    ("setup", setup),
                    ("actual", actual),
                    ("expected", expected)
                ]),
                usize::MAX
            )
            .unwrap()
        )
        .unwrap();
    }
}
async fn compare(group: &str, case: &str) -> J {
    let fixture = ServiceFixture::new();
    let setup = oracle(&fixture, group, case, "setup", vec![]);
    let config = setup.get("config").unwrap().clone();
    let args = setup.get("args").unwrap();
    let result = match group {
        "saved" => runtime_reset::saved_reset_runtime(config.clone())
            .await
            .map(J::Array),
        "prepare" => {
            runtime_reset::prepare_reset_hold(
                config.clone(),
                setup.get("plan").unwrap().clone(),
                field(args, "resetId"),
                field(args, "runtimeEpoch"),
                field(args, "publisherDigest"),
                Arc::new(FixedClock),
            )
            .await
        }
        "complete" => {
            runtime_reset::complete_reset_record(
                config.clone(),
                field(args, "resetId"),
                field(args, "runtimeEpoch"),
                Arc::new(FixedClock),
            )
            .await
        }
        "confirm" => {
            runtime_reset::confirm_reset_record(
                config.clone(),
                field(args, "resetId"),
                field(args, "runtimeEpoch"),
                Arc::new(FixedClock),
            )
            .await
        }
        _ => panic!("unknown reset stage"),
    };
    let actual = J::object([
        ("outcome", wrap(result)),
        ("state", oracle(&fixture, group, case, "observe", vec![])),
    ]);
    oracle(&fixture, group, case, "reset", vec![]);
    let expected = J::object([
        ("outcome", oracle(&fixture, group, case, "run", vec![])),
        ("state", oracle(&fixture, group, case, "observe", vec![])),
    ]);
    retain(group, case, setup, actual.clone(), expected.clone());
    assert_json_eq(&actual, &expected);
    actual
}
fn ok(value: &J) -> bool {
    value.get("outcome").unwrap().get("ok") == Some(&true.into())
}

#[tokio::test]
async fn reset_local_prepare_checkpoints_preserve_identity_authority_and_hold_order() {
    for case in [
        "fresh",
        "fallback-root",
        "malformed-record",
        "previous-null",
        "previous-falsy",
        "retained-prepared",
        "retained-cleared",
        "retained-complete",
        "retained-released",
        "retained-missing-instructions",
        "conflicting-reset",
        "conflicting-epoch",
        "conflicting-plan",
        "conflicting-publisher",
        "completed-new-identity",
        "completed-same-identity",
        "invalid-digest",
        "record-failure",
        "marker-failure",
    ] {
        let result = compare("prepare", case).await;
        assert_eq!(
            ok(&result),
            !case.starts_with("conflicting-")
                && !["invalid-digest", "record-failure", "marker-failure"].contains(&case),
            "{case}"
        );
        let state = result.get("state").unwrap();
        if case == "marker-failure" {
            assert_eq!(
                state.get("record").unwrap().get("kind"),
                Some(&"file".into())
            );
            assert_eq!(
                state.get("marker").unwrap().get("kind"),
                Some(&"directory".into())
            );
        }
        if ok(&result) {
            let value = result.get("outcome").unwrap().get("value").unwrap();
            assert_eq!(value.get("resetId"), Some(&"reset-one".into()));
            assert_eq!(value.get("runtimeEpoch"), Some(&"epoch-one".into()));
            assert_eq!(value.get("deploymentComplete"), Some(&false.into()));
        }
    }
}

#[tokio::test]
async fn reset_local_saved_runtime_reads_actual_journal_authority_and_rejects_missing_candidates() {
    for case in ["none", "installed", "disabled", "missing-candidate"] {
        let result = compare("saved", case).await;
        assert_eq!(ok(&result), case != "missing-candidate");
        if case == "none" {
            assert_eq!(
                result
                    .get("outcome")
                    .unwrap()
                    .get("value")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .len(),
                0
            );
        }
        if case == "disabled" {
            assert_eq!(
                result
                    .get("outcome")
                    .unwrap()
                    .get("value")
                    .unwrap()
                    .as_array()
                    .unwrap()[0]
                    .get("enabled"),
                Some(&false.into())
            );
        }
    }
}

#[tokio::test]
async fn reset_local_completion_and_confirmation_require_exact_durable_phase() {
    for group in ["complete", "confirm"] {
        for case in [
            "valid",
            "prepared",
            "cleared",
            "complete",
            "released",
            "wrong-reset",
            "wrong-epoch",
            "already-confirmed",
            "truthy-completion",
            "null-record",
            "missing-record",
            "malformed-record",
        ] {
            let result = compare(group, case).await;
            let allowed = if group == "complete" {
                [
                    "valid",
                    "cleared",
                    "complete",
                    "already-confirmed",
                    "truthy-completion",
                ]
                .contains(&case)
            } else {
                [
                    "valid",
                    "released",
                    "already-confirmed",
                    "truthy-completion",
                ]
                .contains(&case)
            };
            assert_eq!(ok(&result), allowed, "{group}/{case}");
            let marker = result.get("state").unwrap().get("marker").unwrap();
            assert_eq!(marker.get("kind"), Some(&"file".into()));
        }
    }
}

#[tokio::test]
async fn reset_local_lock_proof_requires_executor_and_every_instance_owner() {
    for variant in ["ordinary", "unicode", "dot-alias"] {
        for held_index in [None, Some(0), Some(1), Some(2), Some(3)] {
            let fixture = ServiceFixture::new();
            let mut config = host_configuration(&fixture);
            let mut names: Vec<PathBuf> = ["first", "middle", "last"]
                .into_iter()
                .map(PathBuf::from)
                .collect();
            let mut state_name = PathBuf::from("state");
            if variant == "unicode" {
                state_name = "état 中 🚀".into();
                names = ["première", "中間", "last 🚀"]
                    .into_iter()
                    .map(PathBuf::from)
                    .collect();
            }
            let runtime = fixture.0.join(state_name);
            let configured_runtime = if variant == "dot-alias" {
                runtime.join("absent/..")
            } else {
                runtime.clone()
            };
            config.insert(
                "runtimeRoot",
                J::String(node_path::from_native(&configured_runtime)),
            );
            config.insert(
                "ivyRoot",
                J::String(node_path::from_native(&fixture.0.join("installation"))),
            );
            let base = config.required("instances").unwrap().as_array().unwrap()[0].clone();
            config.insert(
                "instances",
                J::Array(
                    names
                        .iter()
                        .enumerate()
                        .map(|(index, name)| {
                            let mut instance = base.clone();
                            let configured = if variant == "dot-alias" {
                                PathBuf::from("alias/..").join(name)
                            } else {
                                name.clone()
                            };
                            instance.insert(
                                "instanceId",
                                J::String(node_path::from_native(&configured)),
                            );
                            instance.insert("serviceNodeId", format!("fixture-{index}").into());
                            instance.insert("enabled", (index != 1).into());
                            instance
                        })
                        .collect(),
                ),
            );
            let mut roots = vec![runtime.join("executor")];
            roots.extend(names.iter().map(|name| runtime.join("owners").join(name)));
            let record = runtime_reset::reset_record_path(&config).unwrap();
            let marker = record.parent().unwrap().join("maintenance.json");
            fs::create_dir_all(record.parent().unwrap()).unwrap();
            let record_bytes =
                b"{ \"resetId\": \"original\", \"runtimeEpoch\": \"original-epoch\" }\n";
            let marker_bytes = b"retained maintenance hold\n";
            fs::write(&record, record_bytes).unwrap();
            fs::write(&marker, marker_bytes).unwrap();
            let held = if let Some(index) = held_index {
                Some(
                    ExecutorLock::acquire(roots[index].clone(), false)
                        .await
                        .unwrap(),
                )
            } else {
                None
            };
            let held_bytes = held_index
                .map(|index| fs::read(roots[index].join("executor-lock.sqlite")).unwrap());
            let result = runtime_reset::prove_reset_locks_released(
                config,
                Arc::new(DeadlineClock(AtomicI64::new(0))),
            )
            .await;
            if let Some(index) = held_index {
                let probe = ExecutorLock::acquire(roots[index].clone(), false).await;
                assert_eq!(probe.err().unwrap().code, "executor_already_running");
                assert_eq!(
                    fs::read(roots[index].join("executor-lock.sqlite")).unwrap(),
                    held_bytes.unwrap(),
                    "{variant}/{held_index:?}: live owner's bytes changed"
                );
            }
            if let Some(held) = held {
                held.close().await.unwrap();
            }
            if held_index.is_some() {
                let error = result.unwrap_err();
                assert_eq!(error.code, "outcome_unknown", "{variant}/{held_index:?}");
                assert_eq!(
                    error.message,
                    "A reset writer still owns its maintenance lock."
                );
            } else {
                result.unwrap();
            }
            for (index, root) in roots.iter().enumerate() {
                assert_eq!(
                    root.join("executor-lock.sqlite").is_file(),
                    held_index.is_none_or(|held| index <= held),
                    "{variant}/{held_index:?}: proof skipped or passed owner {index}"
                );
            }
            for root in roots {
                let lock = ExecutorLock::acquire(root, false).await.unwrap();
                lock.close().await.unwrap();
            }
            assert_eq!(fs::read(&record).unwrap(), record_bytes);
            assert_eq!(fs::read(&marker).unwrap(), marker_bytes);
            assert!(
                !runtime.join("absent").exists(),
                "{variant}: runtime alias created"
            );
            assert!(
                !runtime.join("owners/alias").exists(),
                "{variant}: owner alias created"
            );
        }
    }
}

#[test]
fn reset_local_bootstrap_target_keeps_original_launch_identity() {
    let fixture = ServiceFixture::new();
    let base=json::parse(r#"{"schemaVersion":1,"os":"linux","hostId":"fixture","installationId":"fixture-installation","candidateId":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","artifactRoot":"/artifacts","configPath":"/host.json","configHash":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","runtimeRoot":"/runtime","nodeExecutable":"/usr/bin/node","processes":[{"instanceId":"fixture","componentId":"service-manager","name":"fixture-owner","candidateId":"sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","artifactRoot":"/service","entrypoint":{"executable":"node","args":["dist/main.js"],"timeoutMs":5000},"configPath":"/runtime/bootstrap/fixture.json"}]}"#,usize::MAX).unwrap();
    let candidate = format!("sha256:{}", "c".repeat(64));
    for case in [
        "valid",
        "disabled",
        "missing-candidate",
        "missing-instance",
        "missing-config",
        "empty-config",
        "executor",
    ] {
        let mut plan = base.clone();
        let mut processes = plan.get("processes").unwrap().as_array().unwrap().to_vec();
        if case == "missing-config" {
            processes[0].remove("configPath");
        }
        if case == "empty-config" {
            processes[0].insert("configPath", "".into());
        }
        if case == "executor" {
            processes[0].insert("componentId", "host-executor".into());
        }
        plan.insert("processes", J::Array(processes));
        let instance = if case == "missing-instance" {
            "absent"
        } else {
            "fixture"
        };
        let candidate = if case == "missing-candidate" {
            "different"
        } else {
            candidate.as_str()
        };
        let input = vec![
            ("plan", plan.clone()),
            ("instanceId", instance.into()),
            ("candidateId", candidate.into()),
            ("enabled", (case != "disabled").into()),
            ("at", DATE.into()),
        ];
        let actual = wrap(
            runtime_reset::restored_bootstrap_target(
                &plan,
                &instance.into(),
                &candidate.into(),
                case != "disabled",
                DATE.into(),
            )
            .map(|v| v.unwrap_or(J::Null)),
        );
        let expected = oracle(&fixture, "bootstrap", case, "target", input.clone());
        retain(
            "bootstrap",
            case,
            J::object(input),
            actual.clone(),
            expected.clone(),
        );
        assert_json_eq(&actual, &expected);
    }
}
