use super::*;
use ivy_task_board::{
    engine_factory::OpenedJournal, local_journal::LocalJournal, local_retention::HistoryLimits,
    native_journal::NativeJournal,
};

fn config(root: Option<PathBuf>) -> NativeJournal {
    NativeJournal::new(
        root,
        HistoryLimits {
            count: 37,
            bytes: 123456,
        },
    )
}
fn pragma(opened: &OpenedJournal, name: &str) -> i64 {
    opened
        .connection()
        .query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
        .unwrap()
}

#[test]
fn memory_journal_initializes_without_a_filesystem_capacity_claim() {
    let native = config(None);
    let opened = native.open().unwrap();
    assert_eq!(native.free_bytes().unwrap(), None);
    assert_eq!(opened.limits().count, 37);
    assert_eq!(opened.limits().bytes, 123456);
    assert_eq!(pragma(&opened, "synchronous"), 2);
    assert_eq!(pragma(&opened, "busy_timeout"), 5000);
    assert_eq!(
        opened
            .connection()
            .query_row("SELECT COUNT(*) FROM storage_usage", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    opened
        .connection()
        .execute(
            "INSERT INTO markers VALUES('only-this-connection','saved')",
            [],
        )
        .unwrap();
    drop(opened);
    let fresh = native.open().unwrap();
    assert_eq!(
        fresh
            .connection()
            .query_row("SELECT COUNT(*) FROM markers", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn native_file_journal_reopens_exact_records_and_measures_its_data_volume() {
    let temporary = Temporary::new();
    let root = temporary.0.join("journal-🧪");
    std::fs::create_dir(&root).unwrap();
    let native = config(Some(root.clone()));
    let opened = native.open().unwrap();
    let free = native.free_bytes().unwrap().unwrap();
    let (_, total) = ivy_host_runtime::storage_space::measure_bytes(&root).unwrap();
    assert!(free.is_finite() && free >= 0.0 && free <= total as f64);
    assert!(root.join("task-board.sqlite").is_file());
    let mode: String = opened
        .connection()
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    assert_eq!(pragma(&opened, "auto_vacuum"), 2);
    assert_eq!(pragma(&opened, "synchronous"), 2);
    assert_eq!(pragma(&opened, "busy_timeout"), 5000);
    let journal = LocalJournal {
        connection: opened.connection(),
        root_object_id: None,
    };
    let value = ivy_contracts::IvyJson::object([("text", "retained-🧪".into())]);
    journal
        .create(
            "fixture/document",
            &"name".into(),
            None,
            &value,
            &"2026-01-01T00:00:00.000Z".into(),
            &"saved-id".into(),
        )
        .unwrap();
    let blobs = LocalBlobs::new(opened.connection());
    let blob = blobs
        .save_blob(
            &"fixture/blob".into(),
            &"bytes".into(),
            &"saved-id".into(),
            b"\0saved\xff",
        )
        .unwrap();
    drop(opened);
    let reopened = native.open().unwrap();
    let bytes: Vec<u8> = reopened
        .connection()
        .query_row(
            "SELECT CAST(value_json AS BLOB) FROM revisions WHERE id='saved-id' AND revision=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        bytes,
        ivy_contracts::json::canonical(&value, usize::MAX)
            .unwrap()
            .as_bytes()
    );
    assert_eq!(
        LocalBlobs::new(reopened.connection())
            .read_blob(&"fixture/blob".into(), &blob, &"saved-id".into())
            .unwrap(),
        b"\0saved\xff"
    );
    assert_eq!(
        reopened
            .connection()
            .query_row("SELECT SUM(rows) FROM storage_usage", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert!(reopened.connection().is_autocommit());
}

#[test]
fn legacy_auto_vacuum_conversion_preserves_data_and_uses_measured_copy_headroom() {
    let temporary = Temporary::new();
    let file = temporary.0.join("task-board.sqlite");
    let legacy = Connection::open(&file).unwrap();
    legacy.execute_batch("PRAGMA auto_vacuum=NONE; CREATE TABLE retained(payload BLOB); INSERT INTO retained VALUES(x'0001ff');").unwrap();
    ivy_task_board::local_journal::initialize(&legacy).unwrap();
    legacy.execute_batch("INSERT INTO documents VALUES('signal','task-board/native-signals','signal',NULL,2,'at','at');
      INSERT INTO documents VALUES('call','task-board/native-call','call',NULL,2,'at','at');
      INSERT INTO revisions VALUES('signal',1,'{\"signal\":1}','at'),('signal',2,'{\"signal\":2}','at'),
        ('call',1,'{\"call\":1}','at'),('call',2,'{\"call\":2}','at');").unwrap();
    assert_eq!(
        legacy
            .query_row("PRAGMA auto_vacuum", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(legacy);
    let native = config(Some(temporary.0.clone()));
    assert!((std::fs::metadata(&file).unwrap().len() as f64) * 3.0 < 64.0 * 1024.0 * 1024.0);
    let has_copy_room = native.free_bytes().unwrap().unwrap() >= 64.0 * 1024.0 * 1024.0;
    let opened = native.open().unwrap();
    assert_eq!(
        pragma(&opened, "auto_vacuum"),
        if has_copy_room { 2 } else { 0 }
    );
    assert_eq!(
        opened
            .connection()
            .query_row("SELECT payload FROM retained", [], |r| r
                .get::<_, Vec<u8>>(0))
            .unwrap(),
        [0, 1, 255]
    );
    assert!(opened.connection().is_autocommit());
    let saved = |id| {
        opened
            .connection()
            .prepare("SELECT revision,value_json FROM revisions WHERE id=? ORDER BY revision")
            .unwrap()
            .query_map([id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(saved("signal"), [(2, "{\"signal\":2}".to_owned())]);
    assert_eq!(
        saved("call"),
        [
            (1, "{\"call\":1}".to_owned()),
            (2, "{\"call\":2}".to_owned())
        ]
    );
    assert_eq!(
        opened
            .connection()
            .query_row("SELECT SUM(rows) FROM storage_usage", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        3
    );
}

#[test]
fn journal_path_and_schema_failures_never_create_a_fallback_database() {
    let temporary = Temporary::new();
    let missing = temporary.0.join("missing");
    let native = config(Some(missing.clone()));
    assert_eq!(native.free_bytes().unwrap_err().code, "internal_error");
    assert_eq!(native.open().err().unwrap().code, "storage_unavailable");
    assert!(!missing.exists());
    let file = temporary.0.join("task-board.sqlite");
    let malformed = Connection::open(&file).unwrap();
    malformed.execute_batch("CREATE TABLE documents(retained TEXT); INSERT INTO documents VALUES('do not replace');").unwrap();
    drop(malformed);
    let failure = config(Some(temporary.0.clone())).open().err().unwrap();
    assert_eq!(failure.code, "storage_unavailable");
    let retained = Connection::open(&file).unwrap();
    assert_eq!(
        retained
            .query_row("SELECT retained FROM documents", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "do not replace"
    );
}
