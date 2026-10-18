use ivy_store::{receipts::*, rusqlite::Connection};
#[test]
fn unresolved_guards_preserve_array_coercion_and_unpaired_surrogates() {
    let db=Connection::open_in_memory().unwrap();
    let archive=ReceiptArchive::new(&db,ReceiptLimits::default()).unwrap();
    for (index,text) in [r#"{"errorCode":["prefix","outcome_unknown"]}"#,r#"{"code":"\ud800outcome_unknown"}"#,r#"{"phase":[["unknown"]]}"#,r#"{"error":["native_epoch_lost"]}"#].iter().enumerate() {
        let value=ivy_contracts::json::parse(text,1024).unwrap();
        assert!(has_unresolved_outcome(&value),"{text}");
        archive.retain("op",&index.to_string(),text).unwrap();
        assert_eq!(archive.release("op",&index.to_string()).unwrap_err().code,"receipt_archive_unresolved");
    }
    // Opaque receipts use ordinary JSON parsing, not the validator's node budget.
    // The unresolved marker must still protect a receipt after a wide prefix.
    let wide = format!(r#"[{}{{"phase":"unknown"}}]"#, "null,".repeat(400_000));
    archive.retain("op", "wide", &wide).unwrap();
    assert_eq!(archive.release("op", "wide").unwrap_err().code, "receipt_archive_unresolved");
    assert_eq!(archive.read("op", "wide").unwrap().as_deref(), Some(wide.as_str()));
    let deep = format!(r#"{}{{"phase":"unknown"}}{}"#, "[".repeat(20_000), "]".repeat(20_000));
    archive.retain("op", "deep", &deep).unwrap();
    assert_eq!(archive.release("op", "deep").unwrap_err().code, "receipt_archive_unresolved");
    assert_eq!(archive.status().unwrap().records,6);
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
