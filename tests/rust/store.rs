use ivy_store::{receipts::*, rusqlite::Connection};

fn assert_history_usage(db: &Connection) -> (i64, i64) {
    let derived: (i64, i64) = db
        .query_row(
            "SELECT records,bytes FROM receipt_archive_history_usage WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let canonical: (i64, i64) = db
        .query_row(
            "SELECT count(*),COALESCE(sum(length(CAST(a.value AS BLOB))),0)
         FROM receipt_archive_history h JOIN receipt_archive a ON a.kind=h.kind AND a.key=h.key",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(derived, canonical);
    derived
}

#[test]
fn history_usage_initializes_once_and_follows_other_writers_and_rollback() {
    use ivy_store::rusqlite::{OpenFlags, params};
    let uri = format!(
        "file:ivy-history-{}?mode=memory&cache=shared",
        uuid::Uuid::new_v4()
    );
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_URI;
    let db = Connection::open_with_flags(&uri, flags).unwrap();
    // An existing canonical database, before the derived counter was introduced.
    db.execute_batch("CREATE TABLE receipt_archive(kind TEXT NOT NULL,key TEXT NOT NULL,value TEXT NOT NULL,PRIMARY KEY(kind,key)) STRICT;
      CREATE TABLE receipt_archive_usage(id INTEGER PRIMARY KEY,records INTEGER NOT NULL,bytes INTEGER NOT NULL) STRICT;
      CREATE TABLE receipt_archive_history(sequence INTEGER PRIMARY KEY AUTOINCREMENT,kind TEXT NOT NULL,key TEXT NOT NULL,UNIQUE(kind,key));
      INSERT INTO receipt_archive VALUES('k','a','α'),('k','b','😀');
      INSERT INTO receipt_archive_usage SELECT 1,count(*),sum(length(CAST(value AS BLOB))) FROM receipt_archive;
      INSERT INTO receipt_archive_history(kind,key) VALUES('k','a');").unwrap();
    let archive = ReceiptArchive::new(&db, ReceiptLimits::default()).unwrap();
    assert_eq!(assert_history_usage(&db), (1, 2));
    // This writer never creates the new helper. The shared SQLite triggers must
    // still maintain the counter; no per-connection cache is authoritative.
    let other = Connection::open_with_flags(&uri, flags).unwrap();
    other
        .execute(
            "INSERT INTO receipt_archive_history(kind,key) VALUES('k','b')",
            [],
        )
        .unwrap();
    assert_eq!(assert_history_usage(&db), (2, 6));
    other.execute_batch("BEGIN IMMEDIATE").unwrap();
    other
        .execute("INSERT INTO receipt_archive VALUES('k','c',?)", ["😀α"])
        .unwrap();
    other
        .execute(
            "INSERT INTO receipt_archive_history(kind,key) VALUES('k','c')",
            [],
        )
        .unwrap();
    assert_eq!(assert_history_usage(&other), (3, 12));
    other.execute_batch("ROLLBACK").unwrap();
    assert_eq!(assert_history_usage(&db), (2, 6));
    other
        .execute(
            "UPDATE receipt_archive_history SET key='orphan' WHERE key='b'",
            [],
        )
        .unwrap();
    assert_eq!(assert_history_usage(&db), (1, 2));
    other
        .execute(
            "INSERT INTO receipt_archive VALUES(?,?,?)",
            params!["k", "orphan", "old owner"],
        )
        .unwrap();
    assert_eq!(assert_history_usage(&db), (2, 11));
    other
        .execute("DELETE FROM receipt_archive WHERE key='orphan'", [])
        .unwrap();
    other
        .execute("DELETE FROM receipt_archive_history WHERE key='orphan'", [])
        .unwrap();
    assert_eq!(assert_history_usage(&db), (1, 2));
    archive.release("k", "b").unwrap();
    assert_eq!(assert_history_usage(&other), (2, 6));
    ReceiptArchive::new(&other, ReceiptLimits::default()).unwrap();
    assert_eq!(assert_history_usage(&db), (2, 6));
}

#[test]
fn history_limit_reduction_retires_originals_in_order_with_utf8_bytes() {
    let db = Connection::open_in_memory().unwrap();
    let large = ReceiptArchive::new(
        &db,
        ReceiptLimits {
            count: 512,
            bytes: 1024 * 1024,
        },
    )
    .unwrap();
    let text = |n| format!(r#"{{"phase":"done","blob":"{}","n":{n}}}"#, "😀".repeat(60));
    for n in 0..256 {
        large.retain("op", &n.to_string(), &text(n)).unwrap();
        large.release("op", &n.to_string()).unwrap();
    }
    assert_eq!(assert_history_usage(&db).0, 256);
    let small = ReceiptArchive::new(
        &db,
        ReceiptLimits {
            count: 3,
            bytes: 700,
        },
    )
    .unwrap();
    small.release("op", "255").unwrap();
    assert_eq!(
        assert_history_usage(&db),
        (2, (text(254).len() + text(255).len()) as i64)
    );
    assert_eq!(small.status().unwrap().records, 2);
    assert_eq!(small.read("op", "0").unwrap_err().code, "operation_expired");
    assert_eq!(small.read("op", "254").unwrap(), Some(text(254)));
    assert_eq!(small.read("op", "255").unwrap(), Some(text(255)));
    assert_eq!(
        small.retain("op", "0", &text(0)).unwrap_err().code,
        "operation_expired"
    );
    assert_eq!(
        small.retain("op", "0", "changed").unwrap_err().code,
        "receipt_archive_conflict"
    );
    db.execute_batch("BEGIN").unwrap();
    let zero = ReceiptArchive::new(&db, ReceiptLimits { count: 0, bytes: 0 }).unwrap();
    zero.release("op", "255").unwrap();
    assert_eq!(assert_history_usage(&db), (0, 0));
    db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        assert_history_usage(&db),
        (2, (text(254).len() + text(255).len()) as i64)
    );
    assert_eq!(small.read("op", "255").unwrap(), Some(text(255)));
}
#[path = "store-sqlite-compat.rs"]
mod sqlite_compat;
#[path = "store-mechanics.rs"]
mod storage_mechanics;
#[test]
fn typed_outcome_guards_preserve_original_receipts_and_transactional_retirement() {
    use ivy_contracts::{hash::digest, json::parse};
    for (text, protected) in [
        (r#"{"phase":"done","value":"😀"}"#, false),
        (r#"{"phase":null,"error":null}"#, false),
        (
            r#"{"error":{"code":-32601,"message":"Unsupported"}}"#,
            false,
        ),
        (r#"{"error":{"code":"known","outcome":"completed"}}"#, false),
        (r#"{"ordinary":{"toString":null}}"#, false),
        (r#"{"outcome":"unknown"}"#, true),
        (
            r#"{"error":{"code":-1,"data":{"outcome":"unknown"}}}"#,
            true,
        ),
        (r#"{"code":"\ud800outcome_unknown\udfff"}"#, true),
        (r#"{"state":["completed"]}"#, true),
        (r#"{"phase":{"toString":null}}"#, true),
        (r#"{"execution":false}"#, true),
        (r#"{"code":{"nested":"unknown"}}"#, true),
        (r#"{"error":{}}"#, true),
        (r#"{"errorCode":1.5}"#, true),
    ] {
        let value = parse(text, usize::MAX).unwrap();
        assert_eq!(has_unresolved_outcome(&value).unwrap(), protected, "{text}");
        for transaction in [false, true] {
            let db = Connection::open_in_memory().unwrap();
            let archive = ReceiptArchive::new(&db, ReceiptLimits { count: 0, bytes: 0 }).unwrap();
            archive.retain("op", "case", text).unwrap();
            if transaction {
                db.execute_batch("BEGIN").unwrap();
            }
            let result = archive.release("op", "case");
            let count = |table: &str| -> i64 {
                db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap()
            };
            if protected {
                assert_eq!(result.unwrap_err().code, "receipt_archive_unresolved");
                assert_eq!(archive.read("op", "case").unwrap().as_deref(), Some(text));
                assert_eq!(
                    archive.status().unwrap(),
                    ReceiptUsage {
                        records: 1,
                        bytes: text.len()
                    }
                );
                assert_eq!(count("receipt_archive_retired"), 0);
            } else {
                result.unwrap();
                assert_eq!(
                    archive.read("op", "case").unwrap_err().code,
                    "operation_expired"
                );
                assert_eq!(
                    archive.status().unwrap(),
                    ReceiptUsage {
                        records: 0,
                        bytes: 0
                    }
                );
                assert_eq!(count("receipt_archive_retired"), 1);
                let hash: String = db.query_row("SELECT value_hash FROM receipt_archive_retired WHERE kind='op' AND key='case'", [], |row| row.get(0)).unwrap();
                assert_eq!(hash, digest(text));
            }
            assert_eq!(count("receipt_archive_history"), 0);
            assert_eq!(db.is_autocommit(), !transaction);
            if transaction {
                db.execute_batch("ROLLBACK").unwrap();
                assert_eq!(archive.read("op", "case").unwrap().as_deref(), Some(text));
                assert_eq!(
                    archive.status().unwrap(),
                    ReceiptUsage {
                        records: 1,
                        bytes: text.len()
                    }
                );
                assert_eq!(count("receipt_archive_retired"), 0);
            }
        }
    }
}
#[test]
fn unresolved_guards_protect_opaque_nested_values_and_unpaired_surrogates() {
    let db = Connection::open_in_memory().unwrap();
    let archive = ReceiptArchive::new(&db, ReceiptLimits::default()).unwrap();
    for (index, text) in [
        r#"{"errorCode":["prefix","outcome_unknown"]}"#,
        r#"{"code":"\ud800outcome_unknown"}"#,
        r#"{"phase":[["unknown"]]}"#,
        r#"{"error":["native_epoch_lost"]}"#,
    ]
    .iter()
    .enumerate()
    {
        let value = ivy_contracts::json::parse(text, 1024).unwrap();
        assert!(has_unresolved_outcome(&value).unwrap(), "{text}");
        archive.retain("op", &index.to_string(), text).unwrap();
        assert_eq!(
            archive.release("op", &index.to_string()).unwrap_err().code,
            "receipt_archive_unresolved"
        );
    }
    // Opaque receipts use ordinary JSON parsing, not the validator's node budget.
    // The unresolved marker must still protect a receipt after a wide prefix.
    let wide = format!(r#"[{}{{"phase":"unknown"}}]"#, "null,".repeat(400_000));
    archive.retain("op", "wide", &wide).unwrap();
    assert_eq!(
        archive.release("op", "wide").unwrap_err().code,
        "receipt_archive_unresolved"
    );
    assert_eq!(
        archive.read("op", "wide").unwrap().as_deref(),
        Some(wide.as_str())
    );
    let deep = format!(
        r#"{}{{"phase":"unknown"}}{}"#,
        "[".repeat(20_000),
        "]".repeat(20_000)
    );
    archive.retain("op", "deep", &deep).unwrap();
    assert_eq!(
        archive.release("op", "deep").unwrap_err().code,
        "receipt_archive_unresolved"
    );
    assert_eq!(archive.status().unwrap().records, 6);
}
#[test]
fn original_receipts_eviction_and_unknown_are_durable() {
    let db = Connection::open_in_memory().unwrap();
    let archive = ReceiptArchive::new(
        &db,
        ReceiptLimits {
            count: 1,
            bytes: 100,
        },
    )
    .unwrap();
    archive.retain("op", "a", "{\"phase\":\"done\"}").unwrap();
    assert_eq!(
        archive.retain("op", "a", "other").unwrap_err().code,
        "receipt_archive_conflict"
    );
    archive
        .retain("op", "b", "{\"error\":{\"code\":\"native_epoch_lost\"}}")
        .unwrap();
    assert_eq!(
        archive.release("op", "b").unwrap_err().code,
        "receipt_archive_unresolved"
    );
    archive.release("op", "a").unwrap();
    archive.retain("op", "c", "😀").unwrap();
    archive.release("op", "c").unwrap();
    assert_eq!(
        archive.read("op", "a").unwrap_err().code,
        "operation_expired"
    );
    assert!(archive.read("op", "b").unwrap().is_some());
    assert_eq!(archive.status().unwrap().records, 2);
}
#[test]
fn owner_transaction_rollback_includes_receipt_and_eviction() {
    let mut db = Connection::open_in_memory().unwrap();
    ReceiptArchive::new(&db, ReceiptLimits::default()).unwrap();
    {
        let tx = db.transaction().unwrap();
        let archive = ReceiptArchive::new(&tx, ReceiptLimits { count: 0, bytes: 0 }).unwrap();
        archive.retain("k", "v", "original").unwrap();
        archive.release("k", "v").unwrap();
        tx.rollback().unwrap();
    }
    let archive = ReceiptArchive::new(&db, ReceiptLimits::default()).unwrap();
    assert_eq!(archive.read("k", "v").unwrap(), None);
    assert_eq!(archive.status().unwrap().records, 0);
}

#[test]
fn javascript_and_rust_cross_open_the_same_receipt_database() {
    struct Temporary(std::path::PathBuf);
    impl Drop for Temporary {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let temp =
        Temporary(std::env::temp_dir().join(format!("ivy-receipts-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&temp.0).unwrap();
    let path = temp.0.join("state.sqlite");
    let oracle = |mode: &str| {
        let result = std::process::Command::new("node")
            .args([
                "--experimental-transform-types",
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/receipt-oracle.mjs",
                mode,
            ])
            .arg(&path)
            .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    oracle("seed");
    {
        let db = Connection::open(&path).unwrap();
        let archive = ReceiptArchive::new(
            &db,
            ReceiptLimits {
                count: 1,
                bytes: 1000,
            },
        )
        .unwrap();
        assert_eq!(
            archive.read("operation", "js").unwrap().as_deref(),
            Some("{\"value\":\"😀\",\"phase\":\"done\"}")
        );
        archive.release("operation", "js").unwrap();
        archive
            .retain("operation", "rust", "original Rust receipt")
            .unwrap();
        archive.release("operation", "rust").unwrap();
    }
    oracle("check");
}
