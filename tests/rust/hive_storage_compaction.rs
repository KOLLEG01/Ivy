use ivy_hive::storage_compaction::{open_with_available, reclaim_pages};
use rusqlite::Connection;
use std::{fs, path::PathBuf};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ivy-compaction-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn scalar(db: &Connection, name: &str) -> i64 {
    db.query_row(&format!("PRAGMA {name}"), [], |r| r.get(0))
        .unwrap()
}
fn seed(root: &Root) -> PathBuf {
    let path = root.0.join("hive.sqlite");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA user_version=4; CREATE TABLE retained(id INTEGER PRIMARY KEY,value BLOB); INSERT INTO retained VALUES(1,zeroblob(2097152)),(2,zeroblob(2097152)); DELETE FROM retained WHERE id=2;").unwrap();
    assert_eq!(scalar(&db, "auto_vacuum"), 0);
    path
}
#[test]
fn conversion_preserves_data_and_reclaims_pages_without_a_full_live_rewrite() {
    let root = Root::new();
    let path = seed(&root);
    let (db, pending) = open_with_available(&path, || Ok(u64::MAX)).unwrap();
    assert!(pending.is_none());
    assert_eq!(scalar(&db, "auto_vacuum"), 2);
    assert_eq!(scalar(&db, "user_version"), 4);
    assert_eq!(
        db.query_row("SELECT length(value) FROM retained WHERE id=1", [], |r| r
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        2097152
    );
    db.execute("DELETE FROM retained", []).unwrap();
    let before = scalar(&db, "page_count");
    assert!(scalar(&db, "freelist_count") >= 64);
    reclaim_pages(&db).unwrap();
    assert!(scalar(&db, "page_count") < before);
    assert!(!root.0.join("hive.sqlite.compacting").exists());
}
#[test]
fn unavailable_space_and_unknown_temporary_paths_leave_the_original_usable() {
    let root = Root::new();
    let path = seed(&root);
    let (db, pending) = open_with_available(&path, || Ok(0)).unwrap();
    assert!(pending.unwrap().contains("temporary disk space"));
    assert_eq!(scalar(&db, "auto_vacuum"), 0);
    db.close().unwrap();
    let temporary = root.0.join("hive.sqlite.compacting");
    fs::create_dir(&temporary).unwrap();
    let (db, pending) = open_with_available(&path, || Ok(u64::MAX)).unwrap();
    assert!(pending.is_some());
    assert!(temporary.is_dir());
    assert_eq!(
        db.query_row("SELECT count(*) FROM retained", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    db.pragma_update(None, "user_version", 999).unwrap();
    db.close().unwrap();
    assert_eq!(
        open_with_available(&path, || Ok(u64::MAX))
            .err()
            .unwrap()
            .code,
        "unsupported_storage_format"
    );
    assert!(temporary.is_dir());
}
