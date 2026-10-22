use super::{ServiceFixture, actions, assert_json, host};
use ivy_contracts::{IvyJson as J, json};
use ivy_host_runtime::{
    config,
    host_config::HostContext,
    layout::StorageContext,
    node_path,
    runtime_reset::{ResetHostPhase, ResetPhaseArguments},
};
use std::{path::PathBuf, sync::Arc};

fn storage() -> Arc<StorageContext> {
    let account = HostContext::current().unwrap();
    Arc::new(StorageContext {
        paths: account.paths,
        account_home: account.account_home,
        // Explicit fixture order only; no instance/path comparisons are admitted.
        compare: Arc::new(|_, _| panic!("No locale comparisons admitted")),
    })
}
fn input(f: &ServiceFixture, identity: &str) -> J {
    let mut input = host(f);
    input.insert("instances", J::Array(vec![]));
    input.insert(
        "deployment",
        J::object([
            ("identity", identity.into()),
            (
                "hosts",
                J::Array(vec![J::object([
                    ("hostId", "fixture-host".into()),
                    (
                        "configPath",
                        J::String(node_path::from_native(&f.0.join("host.json"))),
                    ),
                ])]),
            ),
        ]),
    );
    input
}
async fn save(f: &ServiceFixture) -> PathBuf {
    let path = f.0.join("host.json");
    config::atomic_json(&path, input(f, "first")).await.unwrap();
    path
}
fn closed(f: &ServiceFixture) {
    let db = ivy_store::rusqlite::Connection::open(f.0.join("state/deployments.sqlite")).unwrap();
    assert_eq!(
        db.query_row("PRAGMA journal_mode=DELETE", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "delete"
    );
    assert!(!f.0.join("reset/current.json").exists());
    assert!(!f.0.join("unused-current-package").exists());
}
#[tokio::test]
async fn local_plan_reloads_configuration_and_closes_request_and_planner_journals() {
    let f = ServiceFixture::new();
    let path = save(&f).await;
    let actions = actions(&f);
    for identity in ["first", "second"] {
        config::atomic_json(&path, input(&f, identity))
            .await
            .unwrap();
        let actual = actions.reset_plan(path.clone(), storage()).await.unwrap();
        assert_json(
            &actual,
            &J::object([
                ("schemaVersion", J::Number(1.0)),
                ("deploymentIdentity", identity.into()),
                ("hostId", "fixture-host".into()),
                ("configPath", J::String(node_path::from_native(&path))),
                ("instances", J::Array(vec![])),
                ("scopeRootIds", J::Array(vec![])),
                ("delete", J::Array(vec![])),
                ("preserve", J::Array(vec![])),
                ("completionMarkers", J::Array(vec![])),
            ]),
        );
        closed(&f);
    }
}
#[tokio::test]
async fn host_phases_preserve_identity_scope_and_publisher_refusals_before_os_admission() {
    let f = ServiceFixture::new();
    let path = save(&f).await;
    let actions = actions(&f);
    for phase in [
        ResetHostPhase::Prepare,
        ResetHostPhase::Complete,
        ResetHostPhase::Bootstrap,
        ResetHostPhase::Release,
        ResetHostPhase::Confirm,
    ] {
        let error = actions
            .reset_host_phase(
                path.clone(),
                phase,
                ResetPhaseArguments::default(),
                super::fixture_node("24.18.0"),
                storage(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "invalid_arguments");
        assert_eq!(
            error.message,
            "Mutating runtime reset phases require reset and epoch identities."
        );
        closed(&f);
    }
    for (roots, digest, expected) in [
        (
            None,
            None,
            "Reset prepare requires coordinated scope roots and ui publisher identity.",
        ),
        (
            Some("not-json"),
            Some("invalid"),
            "Invalid coordinated scope roots.",
        ),
        (
            Some("W10"),
            Some("invalid"),
            "Runtime reset requires the checked ui publisher credential digest.",
        ),
    ] {
        let args = ResetPhaseArguments {
            reset_id: Some("reset".into()),
            runtime_epoch: Some("epoch".into()),
            scope_roots: roots.map(Into::into),
            publisher_digest: digest.map(Into::into),
        };
        let error = actions
            .reset_host_phase(
                path.clone(),
                ResetHostPhase::Prepare,
                args,
                super::fixture_node("24.18.0"),
                storage(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "invalid_arguments");
        assert_eq!(error.message, expected);
        closed(&f);
    }
}
#[tokio::test]
async fn invalid_plan_precedes_phase_identity_refusal_and_preserves_current_state() {
    let f = ServiceFixture::new();
    let path = save(&f).await;
    let actions = actions(&f);
    actions.reset_plan(path.clone(), storage()).await.unwrap();
    std::fs::write(f.0.join("state/retained.txt"), b"unchanged").unwrap();
    let mut missing = input(&f, "first");
    missing.remove("deployment");
    config::atomic_json(&path, missing).await.unwrap();
    let error = actions
        .reset_host_phase(
            path,
            ResetHostPhase::Prepare,
            ResetPhaseArguments::default(),
            super::fixture_node("24.18.0"),
            storage(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
    assert_eq!(
        error.message,
        "Runtime reset requires an explicit deployment identity and host list containing this host."
    );
    assert_eq!(
        std::fs::read(f.0.join("state/retained.txt")).unwrap(),
        b"unchanged"
    );
    closed(&f);
}
#[tokio::test]
#[ignore = "Requires an explicitly pinned original CLI and its dependency bounds"]
async fn original_host_phase_identity_and_plan_refusals() {
    let f = ServiceFixture::new();
    let path = save(&f).await;
    let actions = actions(&f);
    let original = std::env::var_os("IVY_TEST_ORIGINAL_CLI")
        .map(PathBuf::from)
        .expect("Explicit original CLI required");
    for remove_deployment in [false, true] {
        if remove_deployment {
            let mut value = input(&f, "first");
            value.remove("deployment");
            config::atomic_json(&path, value).await.unwrap();
        }
        let output = tokio::process::Command::new(super::node())
            .arg(&original)
            .args(["runtime-reset-host", "--config"])
            .arg(&path)
            .args(["--phase", "prepare"])
            .output()
            .await
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let actual = json::parse(&String::from_utf8(output.stdout).unwrap(), 1024 * 1024).unwrap();
        let error = actions
            .reset_host_phase(
                path.clone(),
                ResetHostPhase::Prepare,
                ResetPhaseArguments::default(),
                super::fixture_node("24.18.0"),
                storage(),
            )
            .await
            .unwrap_err();
        let mut data = J::object([
            ("message", J::String(error.message_js())),
            ("outcome", error.outcome_json()),
        ]);
        if let Some(details) = error.details.clone() {
            data.insert("details", details);
        }
        assert_json(
            &actual,
            &J::object([
                ("schemaVersion", J::Number(1.0)),
                ("ok", false.into()),
                ("code", J::String(error.code_js())),
                ("data", data),
            ]),
        );
        closed(&f);
    }
}
