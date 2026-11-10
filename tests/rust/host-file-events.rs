#[path = "host-service-fixture.rs"]
#[allow(dead_code)]
mod fixture;
use fixture::{ServiceFixture, host_configuration};
use ivy_contracts::IvyJson as J;
use ivy_host_runtime::{
    config,
    file_events::FileEvents,
    journal::{HostJournal, change_path},
};
use std::{fs, time::Duration};

async fn changed(events: &mut FileEvents) {
    tokio::time::timeout(Duration::from_secs(3), events.changed())
        .await
        .expect("native change notification");
}

#[tokio::test]
async fn native_events_follow_atomic_replacement_and_buffer_changes_before_wait() {
    let fixture = ServiceFixture::new();
    let path = fixture.0.join("control.json");
    let mut events = FileEvents::new([path.clone()]);
    assert!(events.native());
    assert_eq!(events.fallback(), Duration::from_secs(30));
    for generation in 0..3 {
        config::atomic_json(path.clone(), J::Number(generation as f64))
            .await
            .unwrap();
        // The caller may be busy validating other state when the event arrives.
        tokio::time::sleep(Duration::from_millis(100)).await;
        changed(&mut events).await;
        assert_eq!(
            config::json_file(path.clone(), 1024).await.unwrap(),
            J::Number(generation as f64)
        );
    }
}

#[tokio::test]
async fn unrelated_health_writes_do_not_feed_back_into_control_watches() {
    let fixture = ServiceFixture::new();
    let mut events = FileEvents::new([fixture.0.join("control.json")]);
    assert!(events.native());
    for generation in 0..10 {
        config::atomic_json(fixture.0.join("health.json"), J::Number(generation as f64))
            .await
            .unwrap();
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(400), events.changed())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn native_events_cover_missing_parents_and_bounded_notification_loss_recovery() {
    let fixture = ServiceFixture::new();
    let path = fixture.0.join("new/data/control.json");
    let mut events = FileEvents::new([path.clone()]);
    assert!(events.native());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    config::atomic_json(path.clone(), true.into())
        .await
        .unwrap();
    changed(&mut events).await;
    assert_eq!(config::json_file(path, 1024).await.unwrap(), true.into());
    let mut unavailable = FileEvents::new([]);
    assert!(!unavailable.native());
    assert_eq!(unavailable.fallback(), Duration::from_millis(500));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), unavailable.changed())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn committed_journal_work_wakes_other_connections_without_read_feedback() {
    let fixture = ServiceFixture::new();
    let config = host_configuration(&fixture);
    let first = HostJournal::open(config.clone()).await.unwrap();
    let second = HostJournal::open(config.clone()).await.unwrap();
    let hash = J::from(format!("sha256:{}", "a".repeat(64)));
    first
        .save_candidate(
            J::object([
                ("candidateId", hash.clone()),
                ("buildId", hash.clone()),
                ("componentId", "fixture".into()),
                (
                    "artifactRoot",
                    fixture
                        .0
                        .join("artifact")
                        .to_string_lossy()
                        .to_string()
                        .into(),
                ),
                (
                    "manifestPath",
                    fixture
                        .0
                        .join("manifest.json")
                        .to_string_lossy()
                        .to_string()
                        .into(),
                ),
                ("createdAt", "2026-01-01T00:00:00.000Z".into()),
                (
                    "platform",
                    J::object([
                        ("os", "darwin".into()),
                        ("arch", "arm64".into()),
                        ("node", "24.18.0".into()),
                    ]),
                ),
            ]),
            J::object([
                ("schemaVersion", 1.0.into()),
                ("componentId", "fixture".into()),
                ("buildId", hash.clone()),
                ("kind", "app".into()),
                ("version", "1.0.0".into()),
                ("connectsToHive", false.into()),
                (
                    "requirements",
                    J::object([
                        ("node", ">=24.18.0 <25.0.0".into()),
                        ("hiveProtocol", J::Null),
                        ("contracts", J::Array(vec![])),
                    ]),
                ),
                (
                    "app",
                    J::object([
                        ("appId", "fixture".into()),
                        ("dist", "dist/apps/fixture".into()),
                        ("entryPath", "index.html".into()),
                    ]),
                ),
            ]),
        )
        .await
        .unwrap();
    first
        .set_installed(J::object([
            ("instanceId", "fixture".into()),
            ("candidateId", hash.clone()),
            ("buildId", hash),
            ("enabled", true.into()),
            ("installedAt", "2026-01-01T00:00:00.000Z".into()),
        ]))
        .await
        .unwrap();
    let path = change_path(&config).unwrap();
    let mut events = FileEvents::new([path.clone()]);
    first
        .accept(
            J::object([
                ("action", "disable".into()),
                ("instanceId", "fixture".into()),
                ("operationId", "event-test".into()),
            ]),
            None,
        )
        .await
        .unwrap();
    changed(&mut events).await;
    let hint = fs::read(&path).unwrap();
    assert_eq!(second.unfinished().await.unwrap().len(), 1);
    for _ in 0..5 {
        second.config().await.unwrap();
        second.observations().await.unwrap();
    }
    second.management_snapshot(None, 0.0, None).await.unwrap();
    assert_eq!(fs::read(&path).unwrap(), hint);
    first.close().await.unwrap();
    second.close().await.unwrap();
}
