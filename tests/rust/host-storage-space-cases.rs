#[path = "host-service-fixture.rs"]
#[allow(dead_code)]
mod fixture;
use fixture::ServiceFixture;
use ivy_contracts::IvyJson;
use ivy_host_runtime::storage_space::{self, RetentionSchedule};
use std::{fs, path::Path};

fn path(value: &Path) -> IvyJson {
    value.to_string_lossy().to_string().into()
}

#[test]
fn storage_pressure_and_retry_boundaries() {
    let gib = 1024.0_f64.powi(3);
    for (available, total, low) in [
        (2.0 * gib - 1.0, 0.0, true),
        (2.0 * gib, 0.0, false),
        (3.0 * gib, 30.0 * gib, false),
        (3.0 * gib, 30.0 * gib + 1.0, true),
        (10.0 * gib, 20.0 * gib, false),
    ] {
        assert_eq!(storage_space::storage_is_low(available, total), low);
    }
    let mut schedule = RetentionSchedule::new(1000.0);
    assert!(!schedule.due(60_999.0, false));
    assert!(schedule.due(61_000.0, false));
    assert!(schedule.due(1000.0, true));
    schedule.started(2000.0);
    assert!(!schedule.due(3_601_999.0, true));
    assert!(schedule.due(3_602_000.0, true));
    assert!(!schedule.due(86_401_999.0, false));
    assert!(schedule.due(86_402_000.0, false));
    schedule.failed(3000.0);
    assert!(!schedule.due(3_602_999.0, false));
    assert!(schedule.due(3_603_000.0, false));
}

#[tokio::test]
async fn storage_inventory_counts_hardlinks_without_following_directory_links() {
    let fixture = ServiceFixture::new();
    let root = fixture.0.join("tree");
    fs::create_dir_all(root.join("nested")).unwrap();
    fs::write(root.join("file"), b"abc").unwrap();
    fs::hard_link(root.join("file"), root.join("nested/hardlink")).unwrap();
    let outside = fixture.0.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("file"), b"must not count").unwrap();
    ivy_host_runtime::filesystem::directory_link(&outside, &root.join("link")).unwrap();
    assert_eq!(
        storage_space::tree_bytes(root.clone(), None).await.unwrap(),
        6.0
    );
    assert_eq!(
        storage_space::tree_bytes(root.join("link"), Some(root.clone()))
            .await
            .unwrap(),
        0.0
    );
    fs::remove_dir_all(&outside).unwrap();
    assert_eq!(
        storage_space::tree_bytes(root.join("link"), Some(root.clone()))
            .await
            .unwrap(),
        0.0
    );
    assert!(
        storage_space::tree_bytes(root.join("link"), None)
            .await
            .is_err()
    );
    assert_eq!(
        storage_space::tree_bytes(root.clone(), Some(root.join("nested")))
            .await
            .unwrap_err()
            .code,
        "target_conflict"
    );
}

#[tokio::test]
async fn storage_inventory_enforces_depth_and_handles_files() {
    let fixture = ServiceFixture::new();
    let root = fixture.0.join("tree");
    fs::create_dir(&root).unwrap();
    let mut nested = root.clone();
    for _ in 0..64 {
        nested = nested.join("d");
        fs::create_dir(&nested).unwrap();
    }
    assert_eq!(
        storage_space::tree_bytes(root.clone(), None).await.unwrap(),
        0.0
    );
    fs::create_dir(nested.join("d")).unwrap();
    assert_eq!(
        storage_space::tree_bytes(root, None)
            .await
            .unwrap_err()
            .code,
        "limit_exceeded"
    );
    let file = fixture.0.join("bytes");
    fs::write(&file, b"12345").unwrap();
    assert_eq!(storage_space::tree_bytes(file, None).await.unwrap(), 5.0);
}

#[tokio::test]
async fn measured_storage_supports_files_missing_host_roots_and_pressure_gate() {
    let fixture = ServiceFixture::new();
    let file = fixture.0.join("file");
    fs::write(&file, b"fixture").unwrap();
    storage_space::available_bytes(&fixture.0).unwrap();
    storage_space::available_bytes(&file).unwrap();
    let missing = storage_space::available_bytes(&fixture.0.join("missing")).unwrap_err();
    assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);
    assert!(missing.raw_os_error().is_some());
    let directory_bytes = storage_space::available_storage_bytes(fixture.0.clone())
        .await
        .unwrap();
    let file_bytes = storage_space::available_storage_bytes(file).await.unwrap();
    // The volume can change between calls. The public JSON count stays integral
    // and within its schema's number range; native callers retain a full u64.
    assert!((0.0..=9_007_199_254_740_991.0).contains(&directory_bytes));
    assert!(file_bytes >= 0.0);
    assert_eq!(directory_bytes.fract(), 0.0);
    assert_eq!(file_bytes.fract(), 0.0);
    assert!(
        storage_space::available_storage_bytes(fixture.0.join("missing"))
            .await
            .is_err()
    );
    let config = IvyJson::object([
        ("runtimeRoot", path(&fixture.0.join("missing/runtime"))),
        ("artifactRoot", path(&fixture.0.join("missing/artifacts"))),
        ("stagingRoot", path(&fixture.0.join("missing/staging"))),
    ]);
    let value = storage_space::host_storage_space(config).await.unwrap();
    for area in ["runtime", "artifacts", "staging"] {
        assert!(
            value
                .required("availableBytes")
                .unwrap()
                .get(area)
                .is_some()
        );
    }
    assert!(!fixture.0.join("missing").exists());
    storage_space::require_storage_space(fixture.0.clone(), 0.0)
        .await
        .unwrap();
    let failure = storage_space::require_storage_space(fixture.0.clone(), 9_007_199_254_740_992.0)
        .await
        .unwrap_err();
    assert_eq!(failure.code, "storage_pressure");
    assert_eq!(failure.outcome, ivy_contracts::Outcome::NotExecuted);
    let details = failure.details.unwrap();
    assert!(matches!(details.get("availableBytes"), Some(IvyJson::Number(v)) if *v >= 0.0));
    assert_eq!(
        details.required("requiredBytes").unwrap(),
        &IvyJson::Number(9_007_199_254_740_992.0)
    );
}
