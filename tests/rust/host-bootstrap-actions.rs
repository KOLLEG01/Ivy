use super::{ServiceFixture, actions, assert_json, journal, node, save};
use ivy_contracts::{IvyJson as J, JsString, json};
use ivy_host_runtime::config;
use std::path::PathBuf;

fn closed(f: &ServiceFixture) {
    let db = ivy_store::rusqlite::Connection::open(f.0.join("state/deployments.sqlite")).unwrap();
    assert_eq!(
        db.query_row("PRAGMA journal_mode=DELETE", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "delete"
    );
}

#[tokio::test]
async fn bootstrap_command_required_options_follow_configuration_and_close_request_journal() {
    let f = ServiceFixture::new();
    let path = save(&f).await;
    let actions = actions(&f);
    for (command, option) in [
        ("install-bootstrap", "candidate"),
        ("update-bootstrap", "request"),
        ("bootstrap-status", "operation-id"),
    ] {
        let error = match command {
            "install-bootstrap" => actions.install_bootstrap(path.clone(), "".into()).await,
            "update-bootstrap" => actions.update_bootstrap(path.clone(), "".into()).await,
            _ => actions.bootstrap_status(path.clone(), "".into()).await,
        }
        .unwrap_err();
        assert_eq!(error.code, "invalid_arguments", "{error:?}");
        assert_eq!(error.message, format!("Missing --{option}."));
        closed(&f);
    }
    // A later invocation must load the changed configuration before checking
    // its required option; no stale HostConfig is retained by HostActions.
    config::atomic_json(&path, J::Null).await.unwrap();
    let error = actions
        .install_bootstrap(path, "".into())
        .await
        .unwrap_err();
    assert_ne!(error.message, "Missing --candidate.");
    assert!(!f.0.join("state/bootstrap-maintenance-lock").exists());
    closed(&f);
}

#[tokio::test]
async fn bootstrap_command_real_owner_refusals_preserve_state_before_os_admission() {
    let f = ServiceFixture::new();
    let path = save(&f).await;
    let actions = actions(&f);
    std::fs::create_dir_all(f.0.join("state")).unwrap();
    std::fs::write(f.0.join("state/sentinel"), b"retained").unwrap();
    let inspect = actions.inspect_bootstrap(path.clone()).await.unwrap_err();
    assert_eq!(inspect.code, "limit_exceeded", "{inspect:?}");
    assert_eq!(
        inspect.message,
        "Expected a bounded set of retained host-executor candidates."
    );
    closed(&f);
    let request = f.0.join("request.json");
    config::atomic_json(&request, J::object([] as [(&str, J); 0]))
        .await
        .unwrap();
    let update = actions
        .update_bootstrap(
            path.clone(),
            ivy_host_runtime::node_path::from_native(&request),
        )
        .await
        .unwrap_err();
    assert_eq!(update.code, "invalid_arguments", "{update:?}");
    closed(&f);
    let install = actions
        .install_bootstrap(path.clone(), "absent-candidate".into())
        .await
        .unwrap_err();
    assert_eq!(install.code, "not_found", "{install:?}");
    closed(&f);
    let status = actions
        .bootstrap_status(path, "absent-operation".into())
        .await
        .unwrap_err();
    assert_eq!(status.code, "not_found", "{status:?}");
    assert_eq!(
        std::fs::read(f.0.join("state/sentinel")).unwrap(),
        b"retained"
    );
    assert!(!f.0.join("state/bootstrap-maintenance-lock").exists());
    assert!(!f.0.join("state/bootstrap").exists());
    let journal = journal(&f).await;
    assert!(journal.list(100.0).await.unwrap().is_empty());
    journal.close().await.unwrap();
}

#[tokio::test]
#[ignore = "Requires an explicitly pinned original CLI and its dependency bounds"]
async fn original_bootstrap_command_refusals() {
    let f = ServiceFixture::new();
    let path = save(&f).await;
    let actions = actions(&f);
    let original = std::env::var_os("IVY_TEST_ORIGINAL_CLI")
        .map(PathBuf::from)
        .expect("Explicit original CLI required");
    for command in [
        "install-bootstrap",
        "update-bootstrap",
        "inspect-bootstrap",
        "bootstrap-status",
    ] {
        let output = tokio::process::Command::new(node())
            .arg(&original)
            .args([command, "--config"])
            .arg(&path)
            .output()
            .await
            .unwrap();
        let error = match command {
            "install-bootstrap" => {
                actions
                    .install_bootstrap(path.clone(), JsString::from(""))
                    .await
            }
            "update-bootstrap" => actions.update_bootstrap(path.clone(), "".into()).await,
            "inspect-bootstrap" => actions.inspect_bootstrap(path.clone()).await,
            _ => actions.bootstrap_status(path.clone(), "".into()).await,
        }
        .unwrap_err();
        assert_eq!(
            output.status.code(),
            Some(if error.code == "invalid_arguments" {
                2
            } else {
                1
            }),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let actual = json::parse(&String::from_utf8(output.stdout).unwrap(), 1024 * 1024).unwrap();
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
