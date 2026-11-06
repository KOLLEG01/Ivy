use super::{Root, config, journal};
use ivy_agent_manager::agent_health::{AgentHealth, HealthRun};
use ivy_contracts::{IvyJson, JsString, Outcome, json::parse};
use ivy_host_runtime::health::HealthFile;
use std::{
    fs,
    sync::{Arc, mpsc},
    time::Duration,
};
use tokio::{runtime::Builder, sync::oneshot};

struct Blocked(Option<mpsc::Sender<()>>);
impl Drop for Blocked {
    fn drop(&mut self) {
        if let Some(send) = self.0.take() {
            let _ = send.send(());
        }
    }
}
fn block_disk() -> (Blocked, oneshot::Receiver<()>) {
    let (release, held) = mpsc::channel();
    let (started, ready) = oneshot::channel();
    tokio::task::spawn_blocking(move || {
        let _ = started.send(());
        // A failed assertion must never strand the fixture's blocking worker.
        let _ = held.recv_timeout(Duration::from_secs(30));
    });
    (Blocked(Some(release)), ready)
}
fn health(root: &Root) -> AgentHealth {
    AgentHealth::new(
        Arc::new(HealthFile::with_launch_id(
            config(root, "0.154.0".into()),
            None,
        )),
        journal("0.154.0".into()),
    )
}
fn stored(root: &Root) -> IvyJson {
    parse(
        &fs::read_to_string(root.0.join("health.json")).unwrap(),
        1024 * 1024,
    )
    .unwrap()
}
fn refused(result: Result<HealthRun, ivy_contracts::IvyError>) {
    match result {
        Err(error) => {
            assert_eq!(error.code, "native_capacity");
            assert_eq!(error.outcome, Outcome::NotExecuted);
        }
        Ok(_) => panic!("Accepted or unread health work must retain its bounded admission"),
    }
}

#[test]
fn queued_and_unread_health_writes_keep_their_admission_and_release_cache_only_results() {
    let runtime = Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let root = Root::new();
        let health = health(&root);
        let (held_disk, started) = block_disk();
        started.await.unwrap();
        let mut held = Vec::new();
        for index in 1..=64 {
            let mut message = Vec::with_capacity(32768);
            message.extend([65, 0xd800, 66, 0xdfff]);
            held.push(
                health
                    .write(index % 2 == 0, Some(index), JsString(message))
                    .unwrap(),
            );
        }
        let denied = health.write(false, Some(65), "unadmitted".into());
        let denied_before_dispatch = denied.is_err();
        if denied_before_dispatch {
            refused(denied);
        }
        drop(held_disk);
        health.drain_snapshot().await.unwrap();
        assert!(
            denied_before_dispatch,
            "Queued health work must refuse a 65th write before another task or file write"
        );
        let value = stored(&root);
        assert_eq!(value.get("generation"), Some(&IvyJson::Number(64.0)));
        assert_eq!(
            value.get("details"),
            Some(&IvyJson::String(JsString(vec![65, 0xd800, 66, 0xdfff])))
        );
        refused(health.clone().write(false, Some(65), "still unread".into()));
        // A completed predecessor owned only by the coordinator may be released.
        drop(held.pop());
        health
            .write(false, Some(65), "recovered".into())
            .unwrap()
            .wait()
            .await
            .unwrap();
        assert_eq!(
            stored(&root).get("generation"),
            Some(&IvyJson::Number(65.0))
        );
        drop(held);
        for index in 66..150 {
            health
                .write(true, Some(index), "cache only".into())
                .unwrap()
                .wait()
                .await
                .unwrap();
        }
        health.drain_snapshot().await.unwrap();
        assert_eq!(
            stored(&root).get("generation"),
            Some(&IvyJson::Number(149.0))
        );
    });
}

#[test]
fn health_snapshot_keeps_its_original_write_and_abandoned_callers_cannot_cancel_later_work() {
    let runtime = Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let root = Root::new();
        let health = health(&root);
        let (first_gate, started) = block_disk();
        started.await.unwrap();
        drop(health.write(true, Some(1), "first".into()).unwrap());
        let snapshot = health.drain_snapshot();
        // The first write now owns the worker queued behind first_gate.
        tokio::task::yield_now().await;
        let (second_gate, started) = block_disk();
        drop(health.write(false, Some(2), "second".into()).unwrap());
        drop(first_gate);
        started.await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), snapshot)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored(&root).get("generation"), Some(&IvyJson::Number(1.0)));
        let final_snapshot = health.drain_snapshot();
        drop(second_gate);
        final_snapshot.await.unwrap();
        let value = stored(&root);
        assert_eq!(value.get("generation"), Some(&IvyJson::Number(2.0)));
        assert_eq!(value.get("ready"), Some(&IvyJson::Bool(false)));
        assert_eq!(value.get("details"), Some(&IvyJson::from("second")));
    });
}

#[tokio::test]
async fn original_health_write_errors_remain_observable_and_later_writes_still_run() {
    let root = Root::new();
    let health = health(&root);
    let path = root.0.join("health.json");
    fs::create_dir(&path).unwrap();
    let original = health
        .write(true, Some(1), "failed".into())
        .unwrap()
        .wait()
        .await
        .unwrap_err();
    assert_eq!(health.drain_snapshot().await.unwrap_err(), original);
    assert!(path.starts_with(&root.0));
    fs::remove_dir(&path).unwrap();
    health
        .write(false, Some(2), "recovered".into())
        .unwrap()
        .wait()
        .await
        .unwrap();
    health.drain_snapshot().await.unwrap();
    assert_eq!(stored(&root).get("generation"), Some(&IvyJson::Number(2.0)));
    assert_eq!(stored(&root).get("ready"), Some(&IvyJson::Bool(false)));
}
