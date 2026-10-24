use super::*;
use ivy_hive::kernel::{StartupOptions, StartupPorts};
use ivy_hive::objects;
use ivy_host_runtime::native_path;
use rusqlite::{Connection, types::ValueRef};

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
fn relative_to_current(path: &Path) -> PathBuf {
    let cwd = std::env::current_dir().unwrap();
    let from: Vec<_> = cwd.components().collect();
    let to: Vec<_> = path.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    assert!(common > 0);
    let mut result: PathBuf = (common..from.len()).map(|_| "..").collect();
    for part in &to[common..] {
        result.push(part.as_os_str());
    }
    result
}

#[test]
fn native_constructor_preserves_initialization_order_legacy_reopen_credentials_and_ui_close() {
    let root = Root::new();
    let mut names = vec![
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
        "relative-db",
        "unicode-db",
        "canonical-db",
        "linked-db",
        "provider-parent",
        "provider-linked",
        "provider-error",
        "memory-empty-root",
        "store-directory",
    ];
    #[cfg(windows)]
    names.extend([
        "opaque-ui-high",
        "opaque-ui-low",
        "trailing-ui",
        "provider-opaque-high",
        "provider-opaque-low",
        "opaque-db-high",
        "opaque-db-low",
    ]);
    assert_eq!(names.len(), if cfg!(windows) { 26 } else { 19 });
    for name in names {
        let base = root.0.join(name);
        fs::create_dir(&base).unwrap();
        let memory = name.starts_with("memory") || name.starts_with("provider");
        let mut input = json!({
            "filename": if memory { PathBuf::from(":memory:") } else { base.join("hive.sqlite") },
            "credentials": [{"principalId":"alpha","digest":format!("sha256:{}", "a".repeat(64))}]
        });
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
        if matches!(name, "empty-root" | "memory-empty-root") {
            input["uiRoot"] = "".into();
        }
        if matches!(
            name,
            "bad-credential-before-ui" | "ui-before-contracts" | "ready-root-file"
        ) {
            input["rootFile"] = true.into();
        }
        if matches!(name, "bad-credential-before-ui" | "provider-error") {
            input["credentials"][0]["digest"] = "invalid".into();
        }
        if name == "settings-contract-conflict" {
            input["seed"] = json!([{"op":"register","owner":{"kind":"hive"},"definition":{
                "key":"ivy/settings","version":"1.0.0","owner":{"kind":"hive"},
                "mediaType":"application/json","specMarkdown":"Different immutable contract.",
                "retention":{"objects":{"mode":"retain"},"revisions":{"mode":"bounded","maximumCount":100}},
                "jsonSchema":{"type":"object"}}}]);
        }
        if name == "legacy-reopen" {
            input["seed"] = json!(ui_files::legacy_seed());
        }
        if name == "ready-root-file" {
            input["seed"] =
                json!([{"op":"sql","sql":"INSERT INTO metadata VALUES('ui_files_v1','ready');"}]);
        }
        prepare(&input);
        let mut filename = PathBuf::from(input["filename"].as_str().unwrap());
        let mut explicit = input
            .get("uiRoot")
            .map(|v| PathBuf::from(v.as_str().unwrap()));
        let mut expected_root = explicit.clone().unwrap_or_else(|| base.join("ui-releases"));
        let mut provider_directory = base.join("temp");
        let mut expected_provider_directory = provider_directory.clone();
        let mut alias: Option<PathBuf> = None;
        let mut link = None;
        if name == "relative-db" {
            filename = relative_to_current(&filename);
            expected_root = relative_to_current(&base.join("ui-releases"));
        } else if name == "unicode-db" {
            fs::create_dir(base.join("目录 space")).unwrap();
            filename = base.join("目录 space/hive.sqlite");
            expected_root = base.join("目录 space").join("ui-releases");
        } else if name == "canonical-db" {
            let canonical = fs::canonicalize(&base).unwrap();
            filename = canonical.join("hive.sqlite");
            expected_root = canonical.join("ui-releases");
        } else if matches!(name, "linked-db" | "provider-linked") {
            fs::create_dir(base.join("real")).unwrap();
            let path = base.join("link");
            ivy_host_runtime::filesystem::directory_link(Path::new("real"), &path).unwrap();
            link = Some((path.clone(), fs::read_link(&path).unwrap()));
            if name == "linked-db" {
                filename = path.join("hive.sqlite");
                expected_root = path.join("ui-releases");
            } else {
                provider_directory = path;
                expected_provider_directory = provider_directory.clone();
            }
        } else if name == "provider-parent" {
            provider_directory = base.join("absent").join("..").join("temp");
        } else if name == "store-directory" {
            filename = base.join("database-directory");
            fs::create_dir(&filename).unwrap();
        }
        #[cfg(windows)]
        if name.starts_with("opaque-")
            || name.starts_with("provider-opaque")
            || name == "trailing-ui"
        {
            use std::{ffi::OsString, os::windows::ffi::OsStringExt};
            let canonical = fs::canonicalize(&base).unwrap();
            let mut units: Vec<_> = "chosen".encode_utf16().collect();
            units.push(if name.ends_with("high") {
                0xd800
            } else {
                0xdfff
            });
            let opaque = canonical.join(OsString::from_wide(&units));
            let retained = canonical.join("chosen�");
            fs::create_dir(&retained).unwrap();
            let sentinel = retained.join("authority");
            fs::write(&sentinel, b"unrelated native authority").unwrap();
            alias = Some(sentinel);
            if name.starts_with("opaque-db") {
                filename = opaque;
            } else if name.starts_with("provider-opaque") {
                provider_directory = opaque;
                expected_provider_directory = provider_directory.clone();
            } else {
                let selected = if name == "trailing-ui" {
                    canonical.join("chosen. ")
                } else {
                    opaque
                };
                if name == "trailing-ui" {
                    fs::create_dir(canonical.join("chosen")).unwrap();
                    let sentinel = canonical.join("chosen/authority");
                    fs::write(&sentinel, b"unrelated native authority").unwrap();
                    alias = Some(sentinel);
                }
                explicit = Some(selected.clone());
                expected_root = selected;
            }
        }
        let provider_expected = usize::from(memory && explicit.is_none());
        for cycle in 0..if name == "legacy-reopen" { 2 } else { 1 } {
            let provider_calls = Rc::new(Cell::new(0));
            let clocks = Rc::new(Cell::new(0));
            let counted = provider_calls.clone();
            let before_provider = clocks.clone();
            let clock = clocks.clone();
            let directory = provider_directory.clone();
            let fail_provider = name == "provider-error";
            let ports = StartupPorts {
                clock: Rc::new(move || {
                    clock.set(clock.get() + 1);
                    100000.0
                }),
                temporary_directory: Box::new(move || {
                    assert_eq!(
                        before_provider.get(),
                        0,
                        "Provider precedes domain initialization"
                    );
                    counted.set(counted.get() + 1);
                    if fail_provider {
                        Err(ivy_contracts::IvyError::new(
                            "fixture_temp",
                            "Controlled provider refusal.",
                        ))
                    } else {
                        Ok(native_path::from_native(&directory))
                    }
                }),
                ..ivy_hive::native_runtime::startup_ports()
            };
            let outcome = Kernel::open_retained(
                StartupOptions {
                    filename: native_path::from_native(&filename),
                    ui_root: explicit.as_ref().map(|path| native_path::from_native(path)),
                    credentials: credentials(&input),
                    kernel: Options {
                        validate_messages: true,
                        ..options()
                    },
                },
                ports,
            );
            assert_eq!(provider_calls.get(), provider_expected, "{name}");
            let expected_error = match name {
                "bad-credential-before-ui" => Some("invalid_arguments"),
                "empty-root"
                | "memory-empty-root"
                | "ui-before-contracts"
                | "store-directory"
                | "opaque-db-high"
                | "opaque-db-low" => Some("internal_error"),
                "settings-contract-conflict" => Some("contract_definition_conflict"),
                "provider-error" => Some("fixture_temp"),
                _ => None,
            };
            match outcome {
                Err(failure) => {
                    let error: &ivy_contracts::IvyError = std::borrow::Borrow::borrow(&failure);
                    assert_eq!(Some(error.code.as_str()), expected_error, "{name}");
                    if filename.is_file() {
                        assert_eq!(
                            super::worker_startup::locked(&filename),
                            name != "bad-credential-before-ui",
                            "{name}: retained owner"
                        );
                    }
                    drop(failure.into_error());
                    if filename.is_file() {
                        assert!(
                            !super::worker_startup::locked(&filename),
                            "{name}: owner released"
                        );
                    }
                    if matches!(name, "bad-credential-before-ui" | "ui-before-contracts") {
                        assert_eq!(fs::read(&expected_root).unwrap(), b"root-file");
                    }
                    if name == "settings-contract-conflict" {
                        assert!(clocks.get() > 0);
                    } else {
                        assert_eq!(
                            clocks.get(),
                            0,
                            "{name}: failure before domain initialization"
                        );
                    }
                    if name == "store-directory" || name.starts_with("opaque-db") {
                        assert!(
                            !expected_root.exists(),
                            "{name}: no UI effect after store refusal"
                        );
                    }
                }
                Ok(mut kernel) => {
                    assert_eq!(expected_error, None, "{name}");
                    let actual_root = kernel.parts.ui_files.root.clone();
                    let temporary = memory && explicit.is_none();
                    if temporary {
                        assert_eq!(
                            actual_root.parent().unwrap().as_os_str(),
                            expected_provider_directory.as_os_str(),
                            "{name}"
                        );
                        let id = actual_root
                            .file_name()
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .strip_prefix("ivy-ui-")
                            .unwrap();
                        assert_eq!(uuid::Uuid::parse_str(id).unwrap().get_version_num(), 4);
                    } else {
                        assert_eq!(actual_root.as_os_str(), expected_root.as_os_str(), "{name}");
                    }
                    assert_eq!(
                        kernel
                            .parts
                            .store
                            .metadata("ui_files_v1")
                            .unwrap()
                            .as_deref(),
                        Some("ready"),
                        "{name}"
                    );
                    if filename.is_file() {
                        assert!(
                            super::worker_startup::locked(&filename),
                            "{name}: live owner"
                        );
                    }
                    for method in ["settings.get", "openai.getConfiguration"] {
                        let response = kernel.execute(
                            &IvyJson::from_serde(&context("http")).unwrap(),
                            &IvyJson::from_serde(&json!({"jsonrpc":"2.0","id":"request","method":method,"params":{}})).unwrap(),
                        ).unwrap();
                        let expected = if method == "settings.get" {
                            json!({"revision":0,"settings":{"schemaVersion":1,"quietHours":null}})
                        } else {
                            json!({"revision":0,"configuration":{"schemaVersion":1,"appId":null}})
                        };
                        assert_eq!(
                            wire(response),
                            json!({"kind":"result","value":expected}),
                            "{name}: {method}"
                        );
                    }
                    let actions = ivy_hive::worker_actions::WorkerActions::new(
                        &mut kernel,
                        &credentials(&input),
                    )
                    .unwrap();
                    let catalog_error = actions
                        .handle(
                            &mut kernel,
                            &IvyJson::from_serde(
                                &json!({"action":"package.catalog","after":0,"includeUis":false}),
                            )
                            .unwrap(),
                        )
                        .unwrap()
                        .unwrap_err();
                    assert_eq!(catalog_error.code, "service_unavailable", "{name}");
                    drop(actions);
                    if name == "legacy-reopen" {
                        assert_eq!(
                            fs::read(actual_root.join("sample-ui/r1/index.html")).unwrap(),
                            b"<p>legacy</p>"
                        );
                        assert_eq!(
                            fs::read(actual_root.join("sample-ui/r1/image.png")).unwrap(),
                            [0, 1, 255]
                        );
                        let count: i64 = kernel.parts.store.connection().query_row(
                            "SELECT count(*) FROM objects WHERE id IN ('owner','file','binary')", [],
                            |row| row.get(0)).unwrap();
                        assert_eq!(count, 0, "Legacy ownership retired on cycle {cycle}");
                        let count: i64 = kernel
                            .parts
                            .store
                            .connection()
                            .query_row("SELECT count(*) FROM app_assets", [], |row| row.get(0))
                            .unwrap();
                        assert_eq!(count, 0);
                    }
                    let before = files(&actual_root);
                    kernel.close().unwrap();
                    if temporary {
                        assert!(
                            !actual_root.exists(),
                            "{name}: temporary UI removed after storage close"
                        );
                    } else {
                        assert_eq!(
                            files(&actual_root),
                            before,
                            "{name}: caller-owned UI retained"
                        );
                    }
                    if filename.is_file() {
                        assert!(
                            !super::worker_startup::locked(&filename),
                            "{name}: close released storage"
                        );
                    }
                    if name == "ready-root-file" {
                        assert_eq!(fs::read(&actual_root).unwrap(), b"root-file");
                    }
                }
            }
        }
        if let Some(alias) = alias {
            assert_eq!(
                fs::read(alias).unwrap(),
                b"unrelated native authority",
                "{name}"
            );
        }
        if let Some((path, target)) = link {
            assert_eq!(
                fs::read_link(path).unwrap(),
                target,
                "{name}: caller link retained"
            );
        }
    }
}
