use super::{ServiceFixture, assert_json_eq};
use ivy_contracts::{IvyJson as J, IvyResult, JsString, json};
use ivy_host_runtime::{
    host_config::HostContext,
    layout::{StorageArea, StorageContext},
    node_path,
    private_backup::NativeBackupPaths,
    restore::{self, RestoreBindings},
};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};
static CORPUS: Mutex<()> = Mutex::new(());
struct FixturePolicy {
    exclusions: Vec<J>,
    shared: Vec<J>,
}
impl NativeBackupPaths for FixturePolicy {
    fn exclusions(&self, _: &J) -> IvyResult<Vec<J>> {
        Ok(self.exclusions.clone())
    }
    fn shared_homes(&self, _: &J) -> IvyResult<Vec<J>> {
        Ok(self.shared.clone())
    }
}
fn text(v: &J) -> JsString {
    v.as_js_string().unwrap().clone()
}
fn field(v: &J, k: &str) -> JsString {
    text(v.required(k).unwrap())
}
fn oracle(f: &ServiceFixture, root: &Path, group: &str, case: &str, kind: &str) -> J {
    let input = J::object([
        ("fixtureRoot", J::String(node_path::from_native(&f.0))),
        ("root", J::String(node_path::from_native(root))),
        ("group", group.into()),
        ("case", case.into()),
        ("kind", kind.into()),
    ]);
    let mut c = Command::new(std::env::var_os("IVY_RESTORE_NODE").unwrap_or_else(|| "node".into()))
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            if ["payload", "publication", "platform"].contains(&group) {
                "tests/fixtures/host-restore-publication-oracle.mjs"
            } else {
                "tests/fixtures/host-restore-oracle.mjs"
            },
        ])
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin
        .take()
        .unwrap()
        .write_all(json::stringify(&input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let out = c.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{group}/{case}/{kind}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    json::parse(std::str::from_utf8(&out.stdout).unwrap(), usize::MAX).unwrap()
}
fn bindings(setup: &J) -> RestoreBindings {
    let host = HostContext::current().unwrap();
    let order = setup
        .required("localeOrder")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(text)
        .collect::<Vec<_>>();
    RestoreBindings {
        storage: StorageContext {
            paths: host.paths,
            account_home: host.account_home,
            compare: Arc::new(move |a, b| {
                order
                    .iter()
                    .position(|v| v == a)
                    .expect("fixture locale input")
                    .cmp(
                        &order
                            .iter()
                            .position(|v| v == b)
                            .expect("fixture locale input"),
                    )
            }),
        },
        environment: host.environment,
        native_paths: Arc::new(FixturePolicy {
            exclusions: setup
                .required("exclusions")
                .unwrap()
                .as_array()
                .unwrap()
                .to_vec(),
            shared: setup
                .required("shared")
                .unwrap()
                .as_array()
                .unwrap()
                .to_vec(),
        }),
    }
}
fn wrapped(result: IvyResult<J>) -> J {
    match result {
        Ok(value) => J::object([("ok", true.into()), ("value", value)]),
        Err(error) => J::object([("ok", false.into()), ("error", error.to_wire())]),
    }
}
fn retain(group: &str, case: &str, setup: J, actual: J, expected: J) {
    if let Some(path) = std::env::var_os("IVY_HOST_RESTORE_CORPUS") {
        let _g = CORPUS.lock().unwrap();
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(
            f,
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
async fn compare_case(group: &str, case: &str) -> J {
    let fixture = ServiceFixture::new();
    let root = fixture.0.join("restore");
    let setup = oracle(&fixture, &root, group, case, "setup");
    if let Some(unavailable) = setup.get("unavailable") {
        let result = J::object([
            ("execution", "blocked".into()),
            ("finding", J::Null),
            ("unavailable", unavailable.clone()),
        ]);
        retain(group, case, setup, result.clone(), J::Null);
        eprintln!("{group}/{case}: file-symlink fixture unavailable; native/reference behavior not run");
        return result;
    }
    let b = bindings(&setup);
    let config = setup.required("config").unwrap().clone();
    let result = match group {
        "verify" => {
            restore::verify_backup(
                field(&setup, "backupRoot"),
                field(&setup, "manifestHash"),
                &b,
            )
            .await
        }
        "target" => {
            restore::target_configuration(
                field(&setup, "templatePath"),
                field(&setup, "backupRoot"),
                setup.required("manifest").unwrap(),
                field(&setup, "manifestHash"),
                field(&setup, "restoreId"),
                &b,
            )
            .await
        }
        "journal" => restore::reconcile_restored_journal(config.clone(), field(&setup, "at"))
            .await
            .map(|_| J::Null),
        "maintenance" => {
            restore::hold_restored_bootstrap_maintenance(config.clone(), field(&setup, "at"))
                .await
                .map(|v| J::Number(v as f64))
        }
        "projects" => {
            let captured = setup
                .required("captured")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|v| StorageArea {
                    key: field(v, "key"),
                    path: field(v, "path"),
                })
                .collect();
            restore::relocate_projects(
                setup.required("original").unwrap().clone(),
                config.clone(),
                captured,
                Arc::new(b.storage),
            )
            .await
            .map(|_| J::Null)
        }
        "platform" => restore::require_restore_platform(
            setup.required("manifest").unwrap(), &super::fixture_node("24.18.0"),
        ).map(|_| J::Null),
        "payload" => restore::copy_restore_payload(
            field(&setup, "backupRoot"), setup.required("manifest").unwrap().clone(),
            config.clone(), Arc::new(b), PathBuf::from(std::env::var_os("IVY_BACKUP_JOB").expect("actual Job helper")),
        ).await.map(J::Number),
        "publication" => restore::publish_restored_host(
            config.clone(), setup.required("manifest").unwrap().clone(),
            setup.required("nativeRecovery").unwrap().clone(), 31,
            field(&setup, "at"), std::time::Instant::now(), Arc::new(b),
        ).await.map(|mut value| { value.remove("elapsedMs"); value }),
        _ => panic!("unknown restore group"),
    };
    let actual = J::object([
        ("outcome", wrapped(result)),
        ("state", oracle(&fixture, &root, group, case, "observe")),
    ]);
    if ["journal", "maintenance", "projects", "payload", "publication"].contains(&group) {
        oracle(&fixture, &root, group, case, "reset");
    }
    let expected = J::object([
        ("outcome", oracle(&fixture, &root, group, case, "run")),
        ("state", oracle(&fixture, &root, group, case, "observe")),
    ]);
    retain(group, case, setup, actual.clone(), expected.clone());
    assert_json_eq(&actual, &expected);
    actual
}
fn ok(result: &J) -> bool {
    result.get("outcome").unwrap().get("ok") == Some(&true.into())
}

#[tokio::test]
async fn copied_journal_reconciliation_preserves_terminal_outcomes_and_rolls_back_failures() {
    for case in [
        "valid",
        "wrong-host",
        "wrong-version",
        "malformed-entry",
        "legacy-no-retirements",
    ] {
        let result = compare_case("journal", case).await;
        assert_eq!(
            ok(&result),
            ["valid", "legacy-no-retirements"].contains(&case),
            "{case}"
        );
        let state = result.get("state").unwrap();
        assert_eq!(state.get("beforeImagePreserved"), Some(&true.into()));
        if !ok(&result) {
            assert_json_eq(state.get("after").unwrap(), state.get("before").unwrap());
        } else {
            let after = state.get("after").unwrap().get("tables").unwrap();
            for name in ["candidates", "installed", "observations", "runtime_targets"] {
                assert!(after.get(name).unwrap().as_array().unwrap().is_empty());
            }
            let rows = after.get("operations").unwrap().as_array().unwrap();
            assert_eq!(rows.len(), 5);
            assert_eq!(
                rows.iter()
                    .filter(|r| r.get("phase") == Some(&"needs_attention".into()))
                    .count(),
                2
            );
        }
    }
}
#[tokio::test]
async fn restored_bootstrap_holds_save_exact_before_images_without_os_replay() {
    for case in [
        "checking",
        "needs-attention",
        "succeeded",
        "invalid-identity",
        "unexpected-entry",
    ] {
        let result = compare_case("maintenance", case).await;
        assert_eq!(
            ok(&result),
            ["checking", "needs-attention", "succeeded"].contains(&case),
            "{case}"
        );
        assert_eq!(
            result.get("state").unwrap().get("originalBytesPreserved"),
            Some(&true.into())
        );
    }
}
#[tokio::test]
async fn project_restore_relocates_only_the_disposable_backup_index() {
    for case in ["valid", "malformed-index"] {
        let result = compare_case("projects", case).await;
        assert_eq!(ok(&result), case == "valid");
    }
}

#[tokio::test]
async fn restore_requires_the_exact_captured_platform_and_node_workload() {
    for case in ["valid", "wrong-os", "wrong-arch", "wrong-node", "missing-node-prefix"] {
        let result=compare_case("platform",case).await;
        assert_eq!(ok(&result),case=="valid","{case}");
    }
}

#[path = "host-restore-target-cases.rs"]
mod target_cases;

#[path = "host-restore-verification-cases.rs"]
mod verification_cases;

#[path = "host-restore-payload-cases.rs"]
mod payload_cases;

#[path = "host-restore-publication-cases.rs"]
mod publication_cases;
