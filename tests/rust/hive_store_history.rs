use super::{Root, native, reference_data};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{fs, path::Path};

#[test]
fn historical_writable_store_matches_original_initialization_and_all_retained_rows() {
    let root = Root::new();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/hive-current.sqlite.gz");
    let fixture_bytes = fs::read(&fixture).unwrap();
    let cases: Value = serde_json::from_slice(&reference_data::read(include_bytes!(
        "../fixtures/hive-store-history-reference.json.gz"
    )))
    .unwrap();
    assert_eq!(
        ivy_contracts::hash::digest(&fixture_bytes),
        format!("sha256:{}", cases["fixtureSha256"].as_str().unwrap())
    );
    let steps = [json!({"op":"reopen"})];
    assert_eq!(json!(steps), cases["steps"]);
    assert_eq!(cases["cases"].as_array().unwrap().len(), 2);
    for (index, legacy) in [false, true].into_iter().enumerate() {
        let expected = &cases["cases"][index];
        assert_eq!(expected["legacy"], legacy);
        let reference = root.0.join(format!("reference-{legacy}.sqlite"));
        let path = root.0.join(format!("native-{legacy}.sqlite"));
        fs::write(&path, reference_data::read(&fixture_bytes)).unwrap();
        if legacy {
            let db = rusqlite::Connection::open(&path).unwrap();
            db.execute_batch("DROP TABLE IF EXISTS durable_subscriptions; CREATE INDEX IF NOT EXISTS mutations_expiry ON mutations(principal_id);").unwrap();
        }
        let reference_bytes = STANDARD
            .decode(expected["image"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            ivy_contracts::hash::digest(&reference_bytes),
            format!("sha256:{}", expected["imageSha256"].as_str().unwrap())
        );
        fs::write(&reference, reference_bytes).unwrap();
        assert_eq!(native(&path, &steps), expected["output"]);
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute("ATTACH ? AS reference", [reference.to_str().unwrap()])
            .unwrap();
        let tables = db
            .prepare("SELECT name FROM main.sqlite_schema WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(tables.len() > 10);
        assert_eq!(json!(tables), expected["tables"]);
        for table in tables {
            let quoted = table.replace('"', "\"\"");
            for (left, right) in [("main", "reference"), ("reference", "main")] {
                let count: i64 = db.query_row(&format!("SELECT count(*) FROM (SELECT * FROM {left}.\"{quoted}\" EXCEPT SELECT * FROM {right}.\"{quoted}\")"), [], |r|r.get(0)).unwrap();
                assert_eq!(count, 0, "legacy={legacy}: {left}.{table}");
            }
        }
    }
    assert_eq!(fs::read(&fixture).unwrap(), fixture_bytes);
}
