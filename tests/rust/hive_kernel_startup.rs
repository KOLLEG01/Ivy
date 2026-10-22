use super::*;
use ivy_hive::kernel::{StartupOptions, StartupPorts};
use ivy_hive::objects;
use ivy_host_runtime::node_path;
use rusqlite::{Connection, OpenFlags, types::ValueRef};

fn sql_rows(db: &Connection, sql: &str) -> Value {
    let mut statement = db.prepare(sql).unwrap();
    let names = statement
        .column_names()
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    json!(statement.query_map([], |row| {
        let mut result = serde_json::Map::new();
        for (i, name) in names.iter().enumerate() {
            result.insert(name.clone(), match row.get_ref(i)? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(v) => json!(v),
                ValueRef::Real(v) => json!(v),
                ValueRef::Text(v) => json!(std::str::from_utf8(v).unwrap()),
                ValueRef::Blob(v) => json!({"blob":v.iter().map(|b|format!("{b:02x}")).collect::<String>()}),
            });
        }
        Ok(Value::Object(result))
    }).unwrap().collect::<Result<Vec<_>,_>>().unwrap())
}
pub(super) fn state(db: &Connection) -> Value {
    let definitions = sql_rows(
        db,
        "SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY type,name",
    );
    let mut tables = serde_json::Map::new();
    for row in sql_rows(db,"SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'object_fts_%' ORDER BY name").as_array().unwrap() {
        let name=row["name"].as_str().unwrap();
        tables.insert(name.into(),sql_rows(db,&format!("SELECT {}* FROM \"{}\"{} ORDER BY rowid",if name=="object_fts"{"rowid,"}else{""},name.replace('"',"\"\""),if name=="metadata"{" WHERE key NOT IN ('cursor_secret','runtime_epoch')"}else{""})));
    }
    json!({"definitions":definitions,"tables":tables})
}
pub(super) fn files(root: &Path) -> Value {
    if !root.exists() {
        return json!({"kind":"absent"});
    }
    if root.is_file() {
        use base64::Engine;
        return json!({"kind":"file","data":base64::engine::general_purpose::STANDARD.encode(fs::read(root).unwrap())});
    }
    json!({"kind":"directory","tree":ui_files::tree(root)})
}
pub(super) fn credentials(input: &Value) -> Vec<Credential> {
    input["credentials"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| Credential {
            principal_id: v["principalId"].as_str().unwrap().into(),
            digest: v["digest"].as_str().unwrap().into(),
        })
        .collect()
}
pub(super) fn prepare(input: &Value) {
    let filename = Path::new(input["filename"].as_str().unwrap()).to_path_buf();
    if let Some(sql) = input["rawSql"].as_str() {
        Connection::open(&filename)
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }
    if input["seed"].as_array().is_some_and(|v| !v.is_empty()) {
        let mut store = Store::open(&filename).unwrap();
        for step in input["seed"].as_array().unwrap() {
            match step["op"].as_str().unwrap() {
                "sql" => store
                    .connection()
                    .execute_batch(step["sql"].as_str().unwrap())
                    .unwrap(),
                "register" => {
                    objects::register(
                        &mut store,
                        &IvyJson::from_serde(&step["definition"]).unwrap(),
                        &IvyJson::from_serde(&step["owner"]).unwrap(),
                    )
                    .unwrap();
                }
                _ => panic!("Unexpected seed action"),
            }
        }
        store.close().unwrap();
    }
    if input["rootFile"] == true {
        let root = Path::new(input["uiRoot"].as_str().unwrap());
        fs::create_dir_all(root.parent().unwrap()).unwrap();
        fs::write(root, "root-file").unwrap();
    }
}
fn run_native(input: &mut Value, profile: &Value) -> Value {
    prepare(input);
    let filename = Path::new(input["filename"].as_str().unwrap()).to_path_buf();
    let mut result = vec![];
    for _ in 0..input["cycles"].as_u64().unwrap_or(1) {
        let directory = JsString::from(input["temporaryDirectory"].as_str().unwrap());
        let temporary = input["filename"] == ":memory:" && input.get("uiRoot").is_none();
        let outcome = Kernel::open(
            StartupOptions {
                filename: node_path::from_native(&filename),
                ui_root: input.get("uiRoot").map(|v| v.as_str().unwrap().into()),
                credentials: credentials(input),
                kernel: Options {
                    validate_messages: true,
                    ..options()
                },
            },
            StartupPorts {
                runtime: Rc::new(registry::ObservedRuntime::new(profile)),
                query: Arc::new(registry::ObservedRuntime::new(profile)),
                clock: Rc::new(|| 100000.0),
                time_zones: Rc::new(|_: &JsString| {
                    panic!("No Intl admission is performed by these startup/get cases")
                }),
                temporary_directory: Box::new(move || {
                    assert!(temporary, "Unused temp-directory provider was called");
                    Ok(directory)
                }),
            },
        );
        let (mut kernel, error) = match outcome {
            Ok(kernel) => (Some(kernel), Value::Null),
            Err(error) => (None, wire(error.to_wire())),
        };
        let root = kernel
            .as_ref()
            .map(|k| k.parts.ui_files.root.clone())
            .unwrap_or_else(|| {
                input
                    .get("uiRoot")
                    .map(|v| PathBuf::from(v.as_str().unwrap()))
                    .unwrap_or_else(|| filename.parent().unwrap().join("ui-releases"))
            });
        if temporary {
            let name = root.file_name().unwrap().to_str().unwrap();
            let id = name.strip_prefix("ivy-ui-").unwrap();
            assert_eq!(uuid::Uuid::parse_str(id).unwrap().get_version_num(), 4);
            input["temporaryUuid"] = id.into();
        }
        let state = if let Some(kernel) = kernel.as_ref() {
            state(kernel.parts.store.connection())
        } else if filename != Path::new(":memory:") && filename.exists() {
            let db =
                Connection::open_with_flags(&filename, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            state(&db)
        } else {
            Value::Null
        };
        let mut requests = vec![];
        if let Some(kernel) = kernel.as_mut() {
            for method in ["settings.get", "openai.getConfiguration"] {
                requests.push(wire(kernel.execute(&IvyJson::from_serde(&context("http")).unwrap(),&IvyJson::from_serde(&json!({"jsonrpc":"2.0","id":"request","method":method,"params":{}})).unwrap()).unwrap()));
            }
        }
        let catalog_probe = if let Some(kernel) = kernel.as_mut() {
            let actions =
                ivy_hive::worker_actions::WorkerActions::new(kernel, &credentials(input)).unwrap();
            match actions
                .handle(
                    kernel,
                    &IvyJson::from_serde(
                        &json!({"action":"package.catalog","after":0,"includeUis":false}),
                    )
                    .unwrap(),
                )
                .unwrap()
            {
                Ok(value) => json!({"value":wire(value),"error":null}),
                Err(error) => json!({"value":null,"error":wire(error.to_wire())}),
            }
        } else {
            Value::Null
        };
        let before = files(&root);
        let close_error = kernel
            .take()
            .and_then(|k| k.close().err())
            .map(|e| wire(e.to_wire()))
            .unwrap_or(Value::Null);
        // Generic post-configuration failures are inspected via a fresh read-only
        // connection after Rust drops the partial owner. Their original lock
        // lifetime is recorded separately, not asserted equivalent here.
        result.push(json!({"error":error,"state":state,"requests":requests,"catalogProbe":catalog_probe,"root":root,"before":before,"after":files(&root),"closeError":close_error,"storeClosed":true}));
    }
    json!({"rows":result})
}
fn original(input: &Value) -> Value {
    let mut child =
        Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()))
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../tests/fixtures/hive-kernel-startup.mjs"),
            )
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
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
#[test]
fn actual_constructor_preserves_initialization_order_legacy_reopen_credentials_and_ui_close() {
    let root = Root::new();
    let profile = if let Some(path) = std::env::var_os("IVY_HIVE_STARTUP_PROFILE") {
        let prior: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        prior["original"]["registryRuntime"].clone()
    } else {
        oracle(&root.0.join("profile.sqlite"), &[op("constructor")])["registryRuntime"].clone()
    };
    let mut corpus = vec![];
    for name in [
        "fresh-disk",
        "explicit-disk",
        "memory-default",
        "memory-explicit",
        "empty-root",
        "bad-credential-before-ui",
        "ui-before-contracts",
        "settings-contract-conflict",
        "legacy-reopen",
        "ready-root-file",
    ] {
        let base = root.0.join(name);
        fs::create_dir(&base).unwrap();
        let memory = name.starts_with("memory");
        let mut input = json!({"case":name,"filename":if memory{PathBuf::from(":memory:")}else{base.join("hive.sqlite")},"temporaryDirectory":base.join("temp"),"credentials":[{"principalId":"alpha","digest":format!("sha256:{}","a".repeat(64))}]});
        if matches!(
            name,
            "explicit-disk"
                | "memory-explicit"
                | "bad-credential-before-ui"
                | "ui-before-contracts"
                | "ready-root-file"
        ) {
            input["uiRoot"] = json!(base.join("chosen"));
        }
        if name == "empty-root" {
            input["uiRoot"] = "".into();
        }
        if matches!(
            name,
            "bad-credential-before-ui" | "ui-before-contracts" | "ready-root-file"
        ) {
            input["rootFile"] = true.into();
        }
        if name == "bad-credential-before-ui" {
            input["credentials"][0]["digest"] = "invalid".into();
        }
        if name == "settings-contract-conflict" {
            input["seed"] = json!([{"op":"register","owner":{"kind":"hive"},"definition":{"key":"ivy/settings","version":"1.0.0","owner":{"kind":"hive"},"mediaType":"application/json","retention":{"objects":{"mode":"retain"},"revisions":{"mode":"bounded","maximumCount":100}},"specMarkdown":"Different immutable contract.","jsonSchema":{"type":"object"}}}]);
        }
        if name == "legacy-reopen" {
            input["seed"] = json!(ui_files::legacy_seed());
            input["cycles"] = 2.into();
        }
        if name == "ready-root-file" {
            input["seed"] =
                json!([{"op":"sql","sql":"INSERT INTO metadata VALUES('ui_files_v1','ready');"}]);
        }
        let actual = run_native(&mut input, &profile);
        // Both runs receive the same root spelling; reset only this bounded
        // fixture directory after the native owner and all observers close.
        assert!(base.starts_with(&root.0));
        fs::remove_dir_all(&base).unwrap();
        fs::create_dir(&base).unwrap();
        let expected = original(&input);
        corpus.push(json!({"input":input,"native":actual,"original":expected}));
    }
    if let Some(path) = std::env::var_os("IVY_HIVE_STARTUP_CORPUS") {
        fs::write(path, json!(corpus).to_string()).unwrap();
    }
    for case in &corpus {
        let name = case["input"]["case"].as_str().unwrap();
        assert_eq!(
            case["native"]["rows"].as_array().unwrap().len(),
            case["original"]["rows"].as_array().unwrap().len()
        );
        for (a, b) in case["native"]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .zip(case["original"]["rows"].as_array().unwrap())
        {
            for key in [
                "error",
                "state",
                "requests",
                "catalogProbe",
                "root",
                "before",
                "after",
                "closeError",
            ] {
                assert_eq!(a[key], b[key], "Constructor {name}: {key}");
            }
            if matches!(name, "bad-credential-before-ui") {
                assert_eq!(b["storeClosed"], true);
            }
            if !matches!(
                name,
                "ui-before-contracts" | "settings-contract-conflict" | "empty-root"
            ) {
                assert_eq!(a["storeClosed"], b["storeClosed"]);
            }
        }
    }
    assert_eq!(corpus[2]["native"]["rows"][0]["after"]["kind"], "absent");
    assert_eq!(corpus[3]["native"]["rows"][0]["after"]["kind"], "directory");
    assert_eq!(
        corpus[4]["native"]["rows"][0]["error"]["data"]["code"],
        "internal_error"
    );
    assert_eq!(
        corpus[5]["native"]["rows"][0]["error"]["data"]["code"],
        "invalid_arguments"
    );
    assert_eq!(
        corpus[6]["native"]["rows"][0]["error"]["data"]["code"],
        "internal_error"
    );
    assert_eq!(
        corpus[7]["native"]["rows"][0]["error"]["data"]["code"],
        "contract_definition_conflict"
    );
    for i in [0, 1, 2, 3, 8, 9] {
        for row in corpus[i]["native"]["rows"].as_array().unwrap() {
            assert!(row["error"].is_null(), "{}", corpus[i]["input"]);
        }
    }
}
