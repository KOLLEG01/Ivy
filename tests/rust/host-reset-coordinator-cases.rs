use super::assert_json_eq;
use ivy_contracts::{IvyError, IvyJson as J, IvyResult, Outcome, json};
use ivy_host_runtime::runtime_reset;
use ivy_host_runtime::{host_config::HostContext, layout::StorageContext};
use std::{
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
};

fn plan_oracle(
    fixture: &super::ServiceFixture,
    root: &std::path::Path,
    case: &str,
    kind: &str,
) -> J {
    use std::process::Stdio;
    let input = J::object([
        (
            "fixtureRoot",
            J::String(ivy_host_runtime::node_path::from_native(&fixture.0)),
        ),
        (
            "root",
            J::String(ivy_host_runtime::node_path::from_native(root)),
        ),
        ("case", case.into()),
        ("kind", kind.into()),
    ]);
    let mut child =
        Command::new(std::env::var_os("IVY_RESTORE_NODE").unwrap_or_else(|| "node".into()))
            .args([
                "--experimental-transform-types",
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/host-reset-plan-oracle.mjs",
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
        "{case}/{kind}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    json::parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
}

#[tokio::test]
async fn reset_local_plan_preserves_declared_trees_and_rejects_path_conflicts() {
    for case in [
        "valid",
        "unicode-tree",
        "missing-preserve",
        "missing-layout",
        "delete-escape",
        "data-root",
        "preserve-overlap",
        "duplicate-target",
        "completion-root",
        "linked-delete",
        "completion-link",
        "protected-software",
        "no-deployment",
        "duplicate-host",
        "remote-missing-cli",
    ] {
        let fixture = super::ServiceFixture::new();
        let root = fixture.0.join("reset-plan");
        let setup = plan_oracle(&fixture, &root, case, "setup");
        let host = HostContext::current().unwrap();
        let pairs = setup
            .get("comparisons")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_array().unwrap().to_vec())
            .collect::<Vec<_>>();
        let storage = Arc::new(StorageContext {
            paths: host.paths,
            account_home: host.account_home,
            compare: Arc::new(move |a, b| {
                let value = pairs
                    .iter()
                    .find(|row| {
                        row[0].as_js_string() == Some(a) && row[1].as_js_string() == Some(b)
                    })
                    .expect("actual fixture locale comparison");
                match &value[2] {
                    J::Number(v) if *v < 0.0 => std::cmp::Ordering::Less,
                    J::Number(v) if *v > 0.0 => std::cmp::Ordering::Greater,
                    _ => std::cmp::Ordering::Equal,
                }
            }),
        });
        let actual = J::object([
            (
                "outcome",
                wrap(
                    runtime_reset::local_runtime_reset_plan(
                        setup.get("config").unwrap().clone(),
                        setup
                            .get("configPath")
                            .unwrap()
                            .as_js_string()
                            .unwrap()
                            .clone(),
                        PathBuf::from(setup.get("distribution").unwrap().as_str().unwrap()),
                        storage,
                    )
                    .await,
                ),
            ),
            ("state", plan_oracle(&fixture, &root, case, "observe")),
        ]);
        let expected = J::object([
            ("outcome", plan_oracle(&fixture, &root, case, "run")),
            ("state", plan_oracle(&fixture, &root, case, "observe")),
        ]);
        if let Some(path) = std::env::var_os("IVY_HOST_RESET_CORPUS") {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap();
            writeln!(
                file,
                "{}",
                json::canonical(
                    &J::object([
                        ("group", "plan".into()),
                        ("case", case.into()),
                        ("setup", setup),
                        ("actual", actual.clone()),
                        ("expected", expected.clone())
                    ]),
                    usize::MAX
                )
                .unwrap()
            )
            .unwrap();
        }
        assert_json_eq(&actual, &expected);
        assert_eq!(
            actual.get("outcome").unwrap().get("ok"),
            Some(
                &["valid", "unicode-tree", "missing-preserve"]
                    .contains(&case)
                    .into()
            ),
            "{case}"
        );
        let state = actual.get("state").unwrap();
        for key in ["settings", "journal", "plan"] {
            assert_json_eq(
                state.get(key).unwrap(),
                state.get("before").unwrap().get(key).unwrap(),
            );
        }
    }
}

fn failure(input: &J) -> IvyError {
    let value = input.get("failure").unwrap();
    let mut error = IvyError::new_js(
        value.get("code").unwrap().as_js_string().unwrap().clone(),
        value
            .get("message")
            .unwrap()
            .as_js_string()
            .unwrap()
            .clone(),
    )
    .with_outcome(Outcome::Unknown);
    if let Some(details) = value.get("details") {
        error = error.with_details(details.clone());
    }
    error
}
fn wrap(result: IvyResult<J>) -> J {
    match result {
        Ok(value) => J::object([("ok", true.into()), ("value", value)]),
        Err(error) => J::object([("ok", false.into()), ("error", error.to_wire())]),
    }
}

#[tokio::test]
async fn reset_coordinator_retains_identity_and_recovers_every_uncertain_host() {
    let output =
        Command::new(std::env::var_os("IVY_RESTORE_NODE").unwrap_or_else(|| "node".into()))
            .args([
                "--experimental-transform-types",
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/host-reset-coordinator-oracle.mjs",
            ])
            .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .output()
            .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let fixture = json::parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    for case in fixture.get("cases").unwrap().as_array().unwrap() {
        let input = case.get("input").unwrap();
        let events = Arc::new(Mutex::new(Vec::<J>::new()));
        let group = case.get("group").unwrap().as_str().unwrap();
        let result = match group.as_str() {
            "identity" => Ok(runtime_reset::retained_reset_identity(
                input.get("previous"),
                input.get("hash").unwrap().as_js_string().unwrap(),
            )
            .unwrap_or(J::Null)),
            "scopes" => runtime_reset::configured_scope_root_ids(input.get("config").unwrap())
                .map(|v| J::Array(v.into_iter().map(J::String).collect())),
            "plans" => runtime_reset::coordinated_runtime_reset_plans(
                input.get("plans").unwrap().as_array().unwrap(),
            )
            .map(J::Array),
            "release" | "bootstrap" => {
                let phase = |host: J| {
                    let events = events.clone();
                    let group = group.clone();
                    async move {
                        let id = host.get("hostId").unwrap();
                        events
                            .lock()
                            .unwrap()
                            .push(format!("{} {}", group, id.as_str().unwrap()).into());
                        if input.get("failAt") == Some(id) {
                            Err(failure(input))
                        } else {
                            Ok(())
                        }
                    }
                };
                let reprepare = |host: J| {
                    let events = events.clone();
                    async move {
                        let id = host.get("hostId").unwrap();
                        events
                            .lock()
                            .unwrap()
                            .push(format!("prepare {}", id.as_str().unwrap()).into());
                        if input
                            .get("reprepareFailures")
                            .unwrap()
                            .as_array()
                            .unwrap()
                            .contains(id)
                        {
                            Err(IvyError::internal())
                        } else {
                            Ok(())
                        }
                    }
                };
                let after = async {
                    events.lock().unwrap().push("after".into());
                    if input.get("afterFailure") == Some(&true.into()) {
                        Err(failure(input))
                    } else {
                        Ok(())
                    }
                };
                let hosts = input.get("hosts").unwrap().as_array().unwrap();
                if group == "release" {
                    runtime_reset::release_reset_hosts(hosts, phase, reprepare, after).await
                } else {
                    runtime_reset::bootstrap_reset_uis(hosts, phase, reprepare, after).await
                }
                .map(|_| J::Null)
            }
            _ => panic!("unknown reset group"),
        };
        let actual = J::object([
            ("outcome", wrap(result)),
            ("events", J::Array(events.lock().unwrap().clone())),
        ]);
        if let Some(path) = std::env::var_os("IVY_HOST_RESET_CORPUS") {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap();
            writeln!(
                file,
                "{}",
                json::canonical(
                    &J::object([
                        ("case", case.clone()),
                        ("actual", actual.clone()),
                        ("sourceHash", fixture.get("sourceHash").unwrap().clone()),
                        (
                            "declarationHash",
                            fixture.get("declarationHash").unwrap().clone()
                        )
                    ]),
                    usize::MAX
                )
                .unwrap()
            )
            .unwrap();
        }
        assert_json_eq(&actual, case.get("expected").unwrap());
    }
}
