use super::*;

fn config(root: &Path) -> Value {
    json!({"schemaVersion":1,"hostId":"host","instanceId":"hive","serviceNodeId":"hive","componentId":"hive","publicBaseUrl":"https://example.invalid","dataRoot":root,"artifactRoot":root.with_extension("artifact"),"buildId":format!("sha256:{}","a".repeat(64)),"version":"1.0.0","settings":{}})
}
fn invoke(input: &Value) -> Value {
    let mut process = Command::new(env!("CARGO_BIN_EXE_ivy-hive-inspect-storage"));
    for arg in input["args"].as_array().unwrap() {
        process.arg(arg.as_str().unwrap());
    }
    let mut child = process
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input["config"].to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    json!({"status":out.status.code(),"stdout":String::from_utf8(out.stdout).unwrap(),"stderr":String::from_utf8(out.stderr).unwrap()})
}
fn compare(inputs: &[Value]) -> Vec<Value> {
    let expected = oracle(inputs);
    inputs
        .iter()
        .zip(expected)
        .enumerate()
        .map(|(index, (input, expected))| {
            let actual = invoke(input);
            assert_eq!(actual, expected, "Inspector process case{index}: {input}");
            actual
        })
        .collect()
}
fn action(config: &Value, args: Value) -> Value {
    json!({"action":"inspector","config":config,"args":args})
}

#[test]
fn admitted_offline_inspector_process_matches_original_arguments_domains_errors_and_missing_output_validation()
 {
    let root = Root::new();
    let path = root.0.join("hive.sqlite");
    seed(&path);
    let before = fs::read(&path).unwrap();
    let config = config(&root.0);
    let mut inputs = vec![action(&config, json!([]))];
    for requirements in [
        json!([{"key":"fixture/value"}]),
        json!([{"key":"fixture/value","readScope":{"roots":["root"],"history":"current","includeArchived":false,"references":[{"objectId":"archived","revision":1}]}}]),
        json!([{"key":"fixture/many"}]),
        json!([{"key":"fixture/value"},{"key":"fixture/value"}]),
        json!([{}]),
        json!([null]),
        json!({}),
        json!([{"key":"x".repeat(193)}]),
        json!([{"key":"🙂".repeat(96)}]),
    ] {
        inputs.push(action(
            &config,
            json!(["--contracts", requirements.to_string()]),
        ));
    }
    for args in [
        json!(["--contracts", "["]),
        json!(["--contracts"]),
        json!(["--contracts", "[{\"key\":\"\\ud800\"}]"]),
        json!(["ignored", "--contracts", "[]", "--contracts", "["]),
    ] {
        inputs.push(action(&config, args));
    }
    let mut other = config.clone();
    other["componentId"] = "fixture".into();
    inputs.push(action(&other, json!(["--contracts", "["])));
    let mut missing = config.clone();
    missing["dataRoot"] = root.0.join("missing").to_string_lossy().into_owned().into();
    for requirements in [json!([{"key":"fixture/value"}]), json!([{}]), json!([null])] {
        inputs.push(action(
            &missing,
            json!(["--contracts", requirements.to_string()]),
        ));
    }
    let results = compare(&inputs);
    assert_eq!(results[0]["status"], 0);
    assert_eq!(results[1]["status"], 0);
    assert!(
        results
            .iter()
            .any(|r| r["stderr"].as_str().unwrap().contains("target_conflict"))
    );
    assert!(
        results
            .iter()
            .any(|r| r["stderr"].as_str().unwrap().contains("limit_exceeded"))
    );
    assert_eq!(fs::read(path).unwrap(), before);
    assert!(!root.0.join("missing").exists());
    assert!(!root.0.join("hive.sqlite-wal").exists());
}

#[test]
fn admitted_offline_inspector_process_obeys_stopped_writer_lock_timeout_and_format_without_mutation()
 {
    let root = Root::new();
    let path = root.0.join("hive.sqlite");
    seed(&path);
    let input = action(
        &config(&root.0),
        json!(["--contracts", "[{\"key\":\"fixture/value\"}]"]),
    );
    let db = Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE;")
        .unwrap();
    let locked = compare(&[input.clone()]);
    assert_eq!(locked[0]["status"], 1);
    assert_eq!(locked[0]["stdout"], "");
    db.execute_batch("ROLLBACK;").unwrap();
    drop(db);
    let before = fs::read(&path).unwrap();
    assert_eq!(compare(&[input.clone()])[0]["status"], 0);
    assert_eq!(fs::read(&path).unwrap(), before);
    let db = Connection::open(&path).unwrap();
    db.pragma_update(None, "user_version", 3).unwrap();
    drop(db);
    let before = fs::read(&path).unwrap();
    let unsupported = compare(&[input]);
    assert_eq!(unsupported[0]["status"], 1);
    assert!(
        unsupported[0]["stderr"]
            .as_str()
            .unwrap()
            .contains("unsupported_storage_format")
    );
    assert_eq!(fs::read(&path).unwrap(), before);
}
