//! Original/native reset traces on separate disposable on-disk databases.
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    json::{parse, stringify},
};
use ivy_hive::{objects, runtime_reset, store::Store};
use rusqlite::{Connection, OpenFlags, types::ValueRef};
use serde_json::{Value, json};
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

fn ivy(value: &Value) -> IvyJson {
    parse(&value.to_string(), usize::MAX).unwrap()
}
fn json_value(value: &IvyJson) -> Value {
    serde_json::from_str(&stringify(value, usize::MAX).unwrap()).unwrap()
}
fn oracle(input: Value) -> Value {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut child =
        Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()))
            .arg(root.join("tests/fixtures/hive-reset-storage-oracle.mjs"))
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "Original reset oracle: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn observe(db: &Connection, queries: &Value) -> Value {
    let mut state = serde_json::Map::new();
    for (name, sql) in queries.as_object().unwrap() {
        let mut statement = db.prepare(sql.as_str().unwrap()).unwrap();
        let names = statement
            .column_names()
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>();
        let rows = statement
            .query_map([], |row| {
                let mut values = serde_json::Map::new();
                for (i, name) in names.iter().enumerate() {
                    let value = match row.get_ref(i)? {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(v) => json!(v),
                        ValueRef::Real(v) => json!(v),
                        ValueRef::Text(v) => json!(String::from_utf8_lossy(v)),
                        ValueRef::Blob(v) => json!({"blob":v}),
                    };
                    values.insert(name.clone(), value);
                }
                Ok(Value::Object(values))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        state.insert(name.clone(), json!(rows));
    }
    Value::Object(state)
}
fn close(store: &mut Option<Store>) {
    if let Some(store) = store.take() {
        store.close().unwrap();
    }
}
fn ids(step: &Value) -> Vec<JsString> {
    if let Some(units) = step.get("idUnits") {
        return units
            .as_array()
            .unwrap()
            .iter()
            .map(|units| {
                JsString(
                    units
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|n| n.as_u64().unwrap() as u16)
                        .collect(),
                )
            })
            .collect();
    }
    step["ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().into())
        .collect()
}
fn native(path: &Path, steps: &[Value], queries: &Value) -> Value {
    let mut store = None;
    let mut saved = IvyJson::Array(vec![]);
    let mut rows = vec![];
    let mut clock_calls = 0;
    for step in steps {
        let result = (|| -> IvyResult<IvyJson> {
            match step["op"].as_str().unwrap() {
                "open" => {
                    let opened = Store::open(path)?;
                    opened.set_metadata("runtime_epoch", "epoch-before")?;
                    store = Some(opened);
                }
                "sql" => {
                    let store = store.as_ref().unwrap();
                    store
                        .connection()
                        .execute_batch(step["sql"].as_str().unwrap())
                        .map_err(|e| store.query_error(e))?;
                }
                "capture" => {
                    close(&mut store);
                    saved = runtime_reset::saved_instructions(path)?;
                    return Ok(saved.clone());
                }
                "discard" => {
                    close(&mut store);
                    std::fs::remove_file(path).unwrap();
                }
                "restore" => runtime_reset::restore_instructions(
                    store.as_mut().unwrap(),
                    &step.get("values").map(ivy).unwrap_or_else(|| saved.clone()),
                )?,
                "roots" => objects::bootstrap_scope_roots(
                    store.as_mut().unwrap(),
                    &ids(step),
                    &step["at"].as_str().unwrap().into(),
                )?,
                "contracts" => objects::bootstrap_service_contracts(
                    store.as_mut().unwrap(),
                    &step["definitions"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(ivy)
                        .collect::<Vec<_>>(),
                )?,
                "rebootstrap" => {
                    close(&mut store);
                    runtime_reset::rebootstrap(
                        path,
                        &step["epoch"].as_str().unwrap().into(),
                        &step.get("values").map(ivy).unwrap_or_else(|| saved.clone()),
                        &ids(step),
                        &step["definitions"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(ivy)
                            .collect::<Vec<_>>(),
                        || {
                            clock_calls += 1;
                            if step["clockError"] == true {
                                Err(IvyError::internal())
                            } else {
                                Ok(step["at"].as_str().unwrap().into())
                            }
                        },
                    )?;
                }
                _ => panic!("Unknown reset fixture operation"),
            }
            Ok(IvyJson::Null)
        })();
        let (value, error) = match result {
            Ok(value) => (json_value(&value), Value::Null),
            Err(error) => (Value::Null, json_value(&error.to_wire())),
        };
        let state = if let Some(store) = &store {
            observe(store.connection(), queries)
        } else if path.exists() {
            observe(
                &Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap(),
                queries,
            )
        } else {
            Value::Null
        };
        rows.push(json!({"value":value,"error":error,"state":state,"clockCalls":clock_calls}));
    }
    close(&mut store);
    json!(rows)
}

#[test]
fn disposable_original_reset_capture_restore_bootstrap_and_failure_boundaries_match() {
    let root = super::TemporaryDirectory::new();
    let original = oracle(json!({"root":root.0}));
    if let Ok(path) = std::env::var("IVY_HIVE_RESET_CORPUS") {
        std::fs::write(format!("{path}.oracle.json"), original.to_string()).unwrap();
    }
    let queries: Value = serde_json::from_str(include_str!(
        "../fixtures/hive-reset-storage-observations.json"
    ))
    .unwrap();
    let traces = original["traces"].as_array().unwrap();
    assert_eq!(traces.len(), 29);
    let mut mismatches = vec![];
    let mut pairs = vec![];
    for (index, trace) in traces.iter().enumerate() {
        let steps = trace["steps"].as_array().unwrap();
        let expected_codes: &[&str] = match trace["name"].as_str().unwrap() {
            "restore-late-root-guard-rolls-back"
            | "restore-owner-guard-rolls-back"
            | "restore-literal-references-guard-rolls-back"
            | "restore-guard-short-circuits-missing-revision"
            | "rebootstrap-epoch-survives-restore-failure" => &["storage_invalid"],
            "restore-late-foreign-key-commit-rolls-back"
            | "restore-duplicate-object-rolls-back"
            | "blob-capture-string-and-restore-refusal"
            | "service-null-owner-precedes-validation"
            | "rebootstrap-restore-survives-clock-failure"
            | "restore-null-object-is-internal" => &["internal_error"],
            "invalid-root-list-precedes-registration" => &[
                "invalid_arguments",
                "invalid_arguments",
                "invalid_arguments",
            ],
            "root-conflict-rolls-back-objects-keeps-contract" | "archived-root-conflict" => {
                &["target_conflict"]
            }
            "root-position-overflow-keeps-contract" => &["limit_exceeded"],
            "service-late-owner-guard-keeps-earlier"
            | "rebootstrap-roots-service-survive-late-failure" => &["contract_owner_mismatch"],
            "service-invalid-definition-keeps-earlier"
            | "rebootstrap-restore-survives-root-failure" => &["invalid_arguments"],
            _ => &[],
        };
        let codes = trace["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|row| row["error"]["data"]["code"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            codes, expected_codes,
            "Original fixture did not reach its intended outcome: {}",
            trace["name"]
        );
        for (step, row) in steps.iter().zip(trace["rows"].as_array().unwrap()) {
            if matches!(step["op"].as_str(), Some("open" | "sql" | "discard")) {
                assert!(
                    row["error"].is_null(),
                    "Original fixture setup failed: {} {row}",
                    trace["name"]
                );
            }
        }
        let native_path = root.0.join(format!("native-{index}.sqlite"));
        let actual = native(&native_path, steps, &queries);
        let matched = trace["rows"] == actual;
        pairs.push(json!({"name":trace["name"],"steps":steps,"original":trace["rows"],"native":actual,"originalDiagnostics":trace["diagnostics"],"execution":"completed","finding":{"conditionMet":matched}}));
        if !matched {
            mismatches.push(pairs.last().unwrap().clone());
        }
        // Cross-open each completed database with the opposite implementation.
        if native_path.exists() {
            let original_path = PathBuf::from(trace["path"].as_str().unwrap());
            let from_native = oracle(json!({"cross":native_path}));
            let from_original =
                json_value(&runtime_reset::saved_instructions(&original_path).unwrap());
            if from_native != from_original {
                mismatches.push(json!({"name":trace["name"],"crossOriginal":from_native,"crossNative":from_original}));
            }
        }
    }
    if let Ok(path) = std::env::var("IVY_HIVE_RESET_CORPUS") {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .unwrap();
        writeln!(file,"{}", json!({"originalNode":original["node"],"originalSqlite":original["sqlite"],"nativeSqlite":rusqlite::version(),"traces":pairs,"mismatches":mismatches})).unwrap();
    }
    assert!(
        mismatches.is_empty(),
        "Reset storage differences: {}",
        serde_json::to_string_pretty(&mismatches).unwrap()
    );
}
