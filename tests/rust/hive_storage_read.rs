use crate::storage_fixture;
use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
};
use ivy_hive::storage_read::{inspect_stopped_storage, snapshot_stopped_hive};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
#[path = "hive_storage_inspector.rs"]
mod inspector;

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        Self(super::temporary_directory("ivy-storage-read"))
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn seed(path: &Path) {
    let db = Connection::open(path).unwrap();
    db.execute_batch("PRAGMA user_version=4;
    CREATE TABLE objects(id TEXT PRIMARY KEY,parent_id TEXT,effective_archive INTEGER,current_revision INTEGER);
    CREATE TABLE revisions(object_id TEXT,revision INTEGER,contract_key TEXT,contract_version TEXT,bytes BLOB);
    INSERT INTO objects VALUES('root',NULL,0,2),('child','root',0,2),('archived','root',1,2),('outside',NULL,0,1);
    INSERT INTO revisions VALUES('root',1,'fixture/value','1.0.0',X'00FF'),('root',2,'fixture/value','2.0.0',X'0102'),
    ('child',1,'fixture/value','1.1.0',X'03'),('child',2,'fixture/value','2.1.0',X'04'),
    ('archived',1,'fixture/value','0.1.0',X'05'),('archived',2,'fixture/value','3.0.0',X'06'),
    ('outside',1,'fixture/other','5.0.0',X'07');").unwrap();
    for i in 0..65 {
        db.execute(
            "INSERT INTO revisions VALUES('outside',?, 'fixture/many',?,X'08')",
            rusqlite::params![i + 1, format!("1.0.{i}")],
        )
        .unwrap();
    }
}
fn native(input: &Value) -> Value {
    let result = if input["action"] == "backup" {
        snapshot_stopped_hive(
            Path::new(input["source"].as_str().unwrap()),
            Path::new(input["destination"].as_str().unwrap()),
        )
    } else {
        let requirements = parse(&input["requirements"].to_string(), 1 << 20).unwrap();
        inspect_stopped_storage(
            Path::new(input["path"].as_str().unwrap()),
            requirements.as_array().unwrap(),
        )
    };
    match result {
        Ok(value) => {
            json!({"value":serde_json::from_str::<Value>(&stringify(&value,1<<20).unwrap()).unwrap()})
        }
        Err(error) => {
            json!({"error":serde_json::from_str::<Value>(&stringify(&error.to_wire(),1<<20).unwrap()).unwrap()})
        }
    }
}
fn reference(inputs: &[Value], root: &Path) -> Vec<Value> {
    let record = storage_fixture::reference("storageRead", &json!(inputs), root);
    if let Some(images) = record["databases"].as_array() {
        for image in images {
            let index = image["index"].as_u64().unwrap() as usize;
            storage_fixture::materialize(
                image,
                Path::new(inputs[index]["destination"].as_str().unwrap()),
            );
        }
    }
    let output = record["output"].as_array().unwrap();
    assert_eq!(output.len(), inputs.len());
    output.clone()
}
fn cross_read<T: AsRef<str>>(snapshot: &Path, source: &Path, tables: &[T]) {
    let queries = tables
        .iter()
        .map(|table| {
            (
                table.as_ref().to_owned(),
                json!(format!(
                    "SELECT * FROM \"{}\"",
                    table.as_ref().replace('"', "\"\"")
                )),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let observe = |path: &Path| {
        let mut value = storage_fixture::sqlite(json!({"path":path,"queries":queries}));
        for rows in value.as_object_mut().unwrap().values_mut() {
            rows.as_array_mut()
                .unwrap()
                .sort_by_cached_key(|row| row.to_string());
        }
        value
    };
    assert_eq!(
        observe(snapshot),
        observe(source),
        "Node SQLite must read every retained row and blob"
    );
}

#[test]
fn stopped_inspection_matches_original_scopes_errors_and_never_mutates_source() {
    let root = Root::new();
    let path = root.0.join("source.sqlite");
    seed(&path);
    let before = fs::read(&path).unwrap();
    let mut requirements = vec![
        json!([]),
        json!([{"key":"fixture/value"}]),
        json!([{"key":"fixture/value","readScope":null}]),
        json!([{"key":"fixture/absent"}]),
        json!([{"key":"fixture/many"}]),
    ];
    for roots in [
        json!(null),
        json!([]),
        json!(["root"]),
        json!(["child"]),
        json!(["absent"]),
        json!(["root", "root"]),
    ] {
        for history in ["all", "current"] {
            for archived in [true, false] {
                for references in [
                    json!([]),
                    json!([{"objectId":"archived","revision":1}]),
                    json!([{"objectId":"outside","revision":1}]),
                    json!([{"objectId":"root","revision":500}]),
                ] {
                    requirements.push(json!([{"key":"fixture/value","readScope":{"roots":roots,"history":history,"includeArchived":archived,"references":references}}]));
                }
            }
        }
    }
    for key in [
        json!(""),
        json!("x".repeat(193)),
        json!("🙂".repeat(96)),
        json!("🙂".repeat(97)),
        json!(null),
        json!(12),
        json!([]),
    ] {
        requirements.push(json!([{"key":key}]));
    }
    requirements.extend([
        json!([{"key":"fixture/value"},{"key":"fixture/value"}]),
        json!([{}]),
        json!([null]),
        json!([12]),
        json!([{"key":"fixture/value","readScope":{}}]),
        json!([{"key":"fixture/value","readScope":false}]),
    ]);
    for count in [128, 129] {
        requirements.push(Value::Array(
            (0..count)
                .map(|i| json!({"key":format!("absent/{i}")}))
                .collect(),
        ));
    }
    let mut inputs: Vec<_> = requirements
        .into_iter()
        .map(|requirements| json!({"path":path,"requirements":requirements}))
        .collect();
    inputs.push(
        json!({"path":root.0.join("absent.sqlite"),"requirements":[{"key":"fixture/value"}]}),
    );
    inputs.push(json!({"path":root.0.join("absent.sqlite"),"requirements":[null]}));
    let expected = reference(&inputs, &root.0);
    for (index, (input, expected)) in inputs.iter().zip(expected).enumerate() {
        assert_eq!(native(input), expected, "row {index}: {input}");
    }
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(!root.0.join("absent.sqlite").exists());
    assert!(!root.0.join("source.sqlite-wal").exists());
}

#[test]
fn historical_format_four_fixture_snapshot_preserves_all_tables_and_reopens_in_node() {
    let root = Root::new();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/hive-current.sqlite.gz");
    let source = root.0.join("historical.sqlite");
    let status=Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(||"node".into()))
        .args(["--input-type=module","-e","import {readFileSync,writeFileSync} from 'node:fs';import {gunzipSync} from 'node:zlib';writeFileSync(process.argv[2],gunzipSync(readFileSync(process.argv[1])));"])
        .arg(fixture).arg(&source).status().unwrap();
    assert!(status.success());
    let original = fs::read(&source).unwrap();
    let target = root.0.join("snapshot.sqlite");
    snapshot_stopped_hive(&source, &target).unwrap();
    let db = Connection::open(&target).unwrap();
    db.execute("ATTACH ? AS original", [source.to_str().unwrap()])
        .unwrap();
    let tables = db
        .prepare("SELECT name FROM main.sqlite_schema WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(tables.len() > 10);
    for table in &tables {
        let quoted = table.replace('"', "\"\"");
        for (left, right) in [("main", "original"), ("original", "main")] {
            let different:i64=db.query_row(&format!("SELECT count(*) FROM (SELECT * FROM {left}.\"{quoted}\" EXCEPT SELECT * FROM {right}.\"{quoted}\")"),[],|r|r.get(0)).unwrap();
            assert_eq!(different, 0, "{left}.{table}");
        }
    }
    cross_read(&target, &source, &tables);
    let requirements = db
        .prepare("SELECT DISTINCT contract_key FROM revisions ORDER BY contract_key")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|key| json!({"key":key.unwrap()}))
        .collect::<Vec<_>>();
    drop(db);
    let input = json!({"path":target,"requirements":requirements});
    assert_eq!(native(&input), reference(&[input.clone()], &root.0)[0]);
    assert_eq!(fs::read(&source).unwrap(), original);
}

#[test]
fn stopped_backup_matches_original_and_copies_exact_retained_sql_rows() {
    let root = Root::new();
    let source = root.0.join("source.sqlite");
    seed(&source);
    let before = fs::read(&source).unwrap();
    let reference_path = root.0.join("reference.sqlite");
    let native_path = root.0.join("native.sqlite");
    let expected = reference(
        &[json!({"action":"backup","source":source,"destination":reference_path})],
        &root.0,
    );
    assert_eq!(
        native(&json!({"action":"backup","source":source,"destination":native_path})),
        expected[0]
    );
    let db = Connection::open(&native_path).unwrap();
    db.execute("ATTACH ? AS reference", [reference_path.to_str().unwrap()])
        .unwrap();
    for table in ["objects", "revisions"] {
        for (left, right) in [("main", "reference"), ("reference", "main")] {
            let count:i64=db.query_row(&format!("SELECT count(*) FROM (SELECT * FROM {left}.{table} EXCEPT SELECT * FROM {right}.{table})"),[],|r|r.get(0)).unwrap();
            assert_eq!(count, 0);
        }
    }
    assert_eq!(fs::read(&source).unwrap(), before);
    cross_read(&native_path, &reference_path, &["objects", "revisions"]);
    drop(db);
    let invalid = root.0.join("future.sqlite");
    let db = Connection::open(&invalid).unwrap();
    db.execute_batch("PRAGMA user_version=99;").unwrap();
    drop(db);
    let mut inputs = vec![
        json!({"action":"backup","source":source,"destination":native_path}),
        json!({"action":"backup","source":"relative.sqlite","destination":root.0.join("never.sqlite")}),
        json!({"action":"backup","source":invalid,"destination":root.0.join("future-output.sqlite")}),
    ];
    if cfg!(windows) {
        let fixture = json!({"action":"fixture","root":root.0,"actions":[{"kind":"mkdir","path":"target"},{"kind":"junction","path":"dangling","target":"target"},{"kind":"remove","path":"target"}]});
        assert_eq!(
            storage_fixture::sqlite(json!({"root":root.0,"files":fixture["actions"]})),
            Value::Null
        );
        inputs.push(
            json!({"action":"backup","source":invalid,"destination":root.0.join("dangling")}),
        );
        inputs.push(json!({"action":"backup","source":root.0.join("dangling"),"destination":root.0.join("dangling-output.sqlite")}));
        let absolute = invalid.to_str().unwrap();
        assert_eq!(&absolute[1..2], ":");
        inputs.push(json!({"action":"backup","source":&absolute[2..],"destination":root.0.join("rooted-output.sqlite")}));
    }
    let expected = reference(&inputs, &root.0);
    for (input, expected) in inputs.iter().zip(expected) {
        assert_eq!(native(input), expected, "Offline snapshot input: {input}");
    }
    assert!(!root.0.join("future-output.sqlite").exists());
}

#[test]
fn backup_executable_has_original_output_and_redacted_failure_contract() {
    let root = Root::new();
    let source = root.0.join("source.sqlite");
    seed(&source);
    let out = Command::new(env!("CARGO_BIN_EXE_ivy-hive-offline-backup"))
        .arg(&source)
        .arg(root.0.join("copy.sqlite"))
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(out.stderr.is_empty());
    let value: IvyJson = parse(std::str::from_utf8(&out.stdout).unwrap(), 4096).unwrap();
    assert_eq!(value.get("format"), Some(&IvyJson::Number(4.0)));
    let out = Command::new(env!("CARGO_BIN_EXE_ivy-hive-offline-backup"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stderr).unwrap(),
        json!({"code":"invalid_arguments","message":"Offline Hive snapshot failed."})
    );
}
