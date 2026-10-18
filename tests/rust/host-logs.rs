use ivy_host_runtime::{logs::ServiceLog, process::OutputStream};
use std::fs;

#[test]
fn bounded_rotation_preserves_permanent_work_and_rejects_nonfiles() {
    let root = std::env::temp_dir().join(format!("ivy-log-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let result = (|| {
        fs::create_dir(root.join("work")).unwrap();
        fs::create_dir(root.join("data")).unwrap();
        fs::write(root.join("work/permanent"), "retain-work").unwrap();
        fs::write(root.join("data/permanent"), "retain-data").unwrap();
        let mut log = ServiceLog::open(&root.join("logs")).unwrap();
        for _ in 0..40 {
            log.append(
                OutputStream::Stderr,
                &"x".repeat(65536),
                "2026-01-01T00:00:00.000Z",
            )
            .unwrap();
        }
        for name in ["process.log", "process.previous.log"] {
            assert!(fs::metadata(root.join("logs").join(name)).unwrap().len() <= 1048576);
        }
        assert_eq!(
            fs::read_to_string(root.join("work/permanent")).unwrap(),
            "retain-work"
        );
        assert_eq!(
            fs::read_to_string(root.join("data/permanent")).unwrap(),
            "retain-data"
        );
        fs::create_dir_all(root.join("invalid/process.log")).unwrap();
        assert_eq!(
            ServiceLog::open(&root.join("invalid")).err().unwrap().code,
            "target_conflict"
        );
        Ok::<_, ()>(())
    })();
    assert!(root.starts_with(std::env::temp_dir()));
    fs::remove_dir_all(root).unwrap();
    result.unwrap();
}
