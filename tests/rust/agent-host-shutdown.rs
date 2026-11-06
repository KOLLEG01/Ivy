//! Exercise the actual host shutdown with a failed owner and another live writer.
use super::prepare_backend;
use crate::{
    agent_health::AgentHealth,
    backend_runtime::{BackendResources, slot},
    host_service::HostResources,
    local_state::{Limits, LocalAgentState},
};
use ivy_contracts::{
    IvyJson as J, JsString, Outcome,
    hash::digest,
    json::parse,
    native_backend::{BackendIdentity, NativeProvider},
};
use ivy_host_runtime::{config::InstanceConfig, health::HealthFile, native_path};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::Duration,
};
use tokio::sync::{oneshot, watch};
#[path = "../fixtures/reference-data.rs"]
mod reference_data;

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ivy-host-shutdown-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn limits() -> Limits {
    Limits {
        max_operations: 100,
        max_journal_bytes: 64 * 1024 * 1024,
    }
}
fn config(root: &Path, name: &str) -> InstanceConfig {
    let bytes = reference_data::read(include_bytes!(
        "../fixtures/agent-service-kernel-cases.json.gz"
    ));
    let data = parse(std::str::from_utf8(&bytes).unwrap(), 32 * 1024 * 1024).unwrap();
    let mut value = data.required("config").unwrap().clone();
    for (key, field) in [
        ("schemaVersion", J::Number(1.0)),
        ("componentId", "agent-manager".into()),
        ("instanceId", "fixture-agent".into()),
        (
            "dataRoot",
            J::String(native_path::from_native(&root.join(name))),
        ),
        (
            "artifactRoot",
            J::String(native_path::from_native(&root.join("artifacts"))),
        ),
        (
            "settings",
            J::object([
                ("nativeVersion", "0.154.0".into()),
                ("nativeExecutableHash", digest("fixture").into()),
                (
                    "limits",
                    J::object([
                        ("maxOperations", J::Number(100.0)),
                        ("maxJournalBytes", J::Number(64.0 * 1024.0 * 1024.0)),
                        ("maxPendingInputs", J::Number(8.0)),
                        ("maxNotificationBytes", J::Number(1024.0 * 1024.0)),
                    ]),
                ),
            ]),
        ),
    ] {
        value.insert(key, field);
    }
    InstanceConfig::from_json(value).unwrap()
}
async fn owner(root: &Root, name: &str, health: &AgentHealth) -> Arc<BackendResources> {
    let backend =
        BackendIdentity::new(name.to_owned(), NativeProvider::Codex, name.to_owned()).unwrap();
    let owner = prepare_backend(
        Arc::new(config(&root.0, name)),
        Some(backend),
        health.clone(),
    )
    .await
    .unwrap();
    // Preparation is real; this fixture owns no provider process or Hive connection.
    owner.startup.send_replace(Some(Ok(())));
    owner
}

#[tokio::test]
async fn failed_inventory_owner_still_drains_every_backend_before_sqlite_and_fails_the_shared_close()
 {
    for failed_first in [true, false] {
        let root = Root::new();
        fs::create_dir(root.0.join("host")).unwrap();
        let health = AgentHealth::host(Arc::new(HealthFile::with_launch_id(
            config(&root.0, "host"),
            None,
        )));
        let first = owner(&root, "first", &health).await;
        let second = owner(&root, "second", &health).await;
        let (failed, writer) = if failed_first {
            (&first, &second)
        } else {
            (&second, &first)
        };
        let original = J::object([
            ("body", J::String(JsString(vec![65, 0xd800, 66]))),
            ("phase", "before".into()),
        ]);
        let mut updated = original.clone();
        updated.insert("phase", "after".into());
        for owner in [&first, &second] {
            owner
                .state
                .lock()
                .unwrap()
                .write("shutdown-test", "marker", &original, None)
                .unwrap();
        }
        let previous = writer
            .state
            .lock()
            .unwrap()
            .read("shutdown-test", "marker")
            .unwrap()
            .unwrap()
            .pin;
        let (entered, waiting) = oneshot::channel();
        let (release, gate) = oneshot::channel();
        let state = writer.state.clone();
        let committed = updated.clone();
        let receipt = writer
            .receipts
            .track(async move {
                entered.send(()).unwrap();
                gate.await.unwrap();
                state.lock().unwrap().write(
                    "shutdown-test",
                    "marker",
                    &committed,
                    Some(&previous),
                )?;
                Ok(())
            })
            .unwrap();
        drop(receipt);
        tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .unwrap()
            .unwrap();
        // A real owner-slot failure, without replacing any production shutdown step.
        let inventory = failed.inventory.clone();
        assert!(
            std::panic::catch_unwind(move || {
                let _guard = inventory.lock().unwrap();
                panic!("Fixture inventory owner unavailable");
            })
            .is_err()
        );
        let host = Arc::new(HostResources {
            backends: Mutex::new(vec![first.clone(), second.clone()]),
            preparation_failures: Mutex::new(Vec::new()),
            service: slot(),
            health,
            stop_timer: watch::channel(false).0,
        });
        let shutdown = host.shutdown(Arc::new(AtomicBool::new(false)));
        let abandoned = shutdown.close("agent_stopped".into()).unwrap();
        let joined = shutdown.close("ignored".into()).unwrap();
        assert!(abandoned.shares_completion(&joined));
        drop(abandoned);
        let mut stopped = Box::pin(joined.wait());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut stopped)
                .await
                .is_err()
        );
        for owner in [&first, &second] {
            assert_eq!(
                owner
                    .state
                    .lock()
                    .unwrap()
                    .read("shutdown-test", "marker")
                    .unwrap()
                    .unwrap()
                    .value,
                original
            );
        }
        release.send(()).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), stopped)
            .await
            .unwrap();
        for owner in [&first, &second] {
            assert!(
                owner
                    .state
                    .lock()
                    .unwrap()
                    .read("shutdown-test", "marker")
                    .is_err(),
                "SQLite must close after draining all owners"
            );
            let state = LocalAgentState::open(
                &owner
                    .config
                    .data_root()
                    .join("journal/agent-runtime.sqlite"),
                "fixture.agent".into(),
                limits(),
            )
            .unwrap();
            let expected = if Arc::ptr_eq(owner, writer) {
                &updated
            } else {
                &original
            };
            assert_eq!(
                &state
                    .read("shutdown-test", "marker")
                    .unwrap()
                    .unwrap()
                    .value,
                expected
            );
            state.close().unwrap();
        }
        let error =
            result.expect_err("A failed drain owner must not report a successful host stop");
        assert_eq!(error.code, "internal_error");
        assert_eq!(error.outcome, Outcome::Unknown);
        assert_eq!(
            shutdown.closed().await.unwrap(),
            JsString::from("agent_shutdown_failed")
        );
        let status = parse(
            &fs::read_to_string(root.0.join("host/health.json")).unwrap(),
            64 * 1024,
        )
        .unwrap();
        assert_eq!(status.get("ready"), Some(&false.into()));
        assert_eq!(
            status.get("details"),
            Some(&"AgentManager stopped: agent_shutdown_failed.".into())
        );
    }
}
