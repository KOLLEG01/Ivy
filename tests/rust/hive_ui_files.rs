use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::IvyResult;
use ivy_hive::{ui_files::UiFiles, ui_security, uis};
use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
};
#[path = "hive_ui_collection_causes.rs"]
mod collection_causes;
#[cfg(windows)]
#[path = "hive_filesystem.rs"]
mod filesystem;

fn string<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a JsString> {
    value
        .get(key)
        .and_then(IvyJson::as_js_string)
        .ok_or_else(IvyError::internal)
}
fn array<'a>(value: &'a IvyJson, key: &str) -> &'a [IvyJson] {
    value.get(key).and_then(IvyJson::as_array).unwrap()
}
pub(super) fn fixture(root: &Path, actions: &Value) {
    let mut child =
        Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()))
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../tests/fixtures/hive-ui-files-fixture.mjs"),
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
        .write_all(
            json!({"root":root,"actions":actions})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "OS fixture: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}
pub(super) fn tree(root: &Path) -> Value {
    fn walk(root: &Path, directory: &Path, result: &mut Vec<Value>) {
        let native_directory = ivy_host_runtime::node_path::node_filesystem_path(
            &ivy_host_runtime::node_path::from_native(directory),
        )
        .unwrap();
        if !native_directory.exists() {
            return;
        }
        for entry in fs::read_dir(native_directory).unwrap() {
            let entry = entry.unwrap();
            let path = directory.join(entry.file_name());
            let native_path = ivy_host_runtime::node_path::node_filesystem_path(
                &ivy_host_runtime::node_path::from_native(&path),
            )
            .unwrap();
            let kind = fs::symlink_metadata(&native_path).unwrap().file_type();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .unwrap()
                .replace('\\', "/");
            if kind.is_symlink() {
                result.push(json!({"path":relative,"kind":"link"}));
            } else if kind.is_dir() {
                result.push(json!({"path":relative,"kind":"directory"}));
                walk(root, &path, result);
            } else if kind.is_file() {
                result.push(json!({"path":relative,"kind":"file","data":STANDARD.encode(fs::read(native_path).unwrap())}));
            } else {
                panic!("Unexpected fixture file type");
            }
        }
    }
    let mut result = vec![];
    walk(root, root, &mut result);
    result.sort_by(|a, b| {
        a["path"]
            .as_str()
            .unwrap()
            .as_bytes()
            .cmp(b["path"].as_str().unwrap().as_bytes())
    });
    json!(result)
}
pub(super) fn execute(store: &mut Store, step: &Value, database: &Path) -> IvyResult<IvyJson> {
    let root = database.with_extension("files");
    let files = UiFiles::new(root.join("private"));
    let mut params = IvyJson::from_serde(&step["params"]).unwrap();
    object_write::apply_units(&mut params, step);
    if let Some(sql) = step["prepareSql"].as_str() {
        store
            .connection()
            .execute_batch(sql)
            .map_err(|e| store.query_error(e))?;
    }
    let ui = || string(&params, "uiId");
    let release = || string(&params, "releaseId");
    let asset = || params.get("asset").unwrap();
    let path_result = |path: PathBuf| -> IvyResult<IvyJson> {
        Ok(path
            .strip_prefix(&root)
            .unwrap()
            .to_str()
            .unwrap()
            .replace('\\', "/")
            .into())
    };
    let mut value = match step["method"].as_str().unwrap() {
        "fixture" => {
            fixture(&root, &step["params"]["actions"]);
            IvyJson::Null
        }
        "assetPath" => IvyJson::String(uis::asset_path(string(&params, "path")?)?.clone()),
        "contentPolicy" => ui_security::content_policy(string(&params, "path")?).into(),
        "directory" => path_result(files.directory(ui()?, release()?)?)?,
        "path" => path_result(files.path(ui()?, release()?, string(&params, "path")?)?)?,
        "available" => files.available(ui()?, release()?, asset()).into(),
        "check" => files.check(ui()?, release()?, asset())?.into(),
        "stage" => {
            let bytes = STANDARD
                .decode(String::from_utf16_lossy(&string(&params, "data")?.0))
                .unwrap();
            files.stage(ui()?, release()?, asset(), &bytes)?;
            IvyJson::Null
        }
        "promote" => {
            files.promote(ui()?, release()?, array(&params, "assets"))?;
            IvyJson::Null
        }
        "read" => STANDARD
            .encode(
                tokio::runtime::Builder::new_current_thread()
                    .build()
                    .unwrap()
                    .block_on(files.read(ui()?, release()?, asset()))?,
            )
            .into(),
        "install" => {
            tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
                .block_on(files.install(
                    &root.join("bundle"),
                    ui()?,
                    release()?,
                    array(&params, "assets"),
                ))?;
            IvyJson::Null
        }
        "collect" => {
            let retained: HashSet<_> = params
                .get("retained")
                .and_then(IvyJson::as_array)
                .unwrap_or(&[])
                .iter()
                .map(|v| v.as_js_string().unwrap().clone())
                .collect();
            files.collect(ui()?, &retained, step["now"].as_f64().unwrap())?;
            IvyJson::Null
        }
        "collectTemporary" => {
            files.collect_temporary(step["now"].as_f64().unwrap())?;
            IvyJson::Null
        }
        "collectAll" => {
            let retained: HashMap<_, _> = params
                .get("retained")
                .and_then(IvyJson::as_object)
                .unwrap_or(&[])
                .iter()
                .map(|(id, ids)| {
                    (
                        id.clone(),
                        ids.as_array()
                            .unwrap()
                            .iter()
                            .map(|v| v.as_js_string().unwrap().clone())
                            .collect(),
                    )
                })
                .collect();
            let clock = Cell::new(0);
            files.collect_all(&retained, &|| {
                let n = clock.get();
                clock.set(n + 1);
                step["clock"]
                    .get(n)
                    .and_then(Value::as_f64)
                    .unwrap_or_else(|| step["now"].as_f64().unwrap())
            })?;
            IvyJson::Null
        }
        "migrateLegacy" => {
            files.migrate_legacy(store)?;
            IvyJson::Null
        }
        _ => panic!("Unknown UI file method"),
    };
    if step["lossless"] == true {
        value = IvyJson::object([("encoded", stringify(&value, usize::MAX)?.into())]);
    }
    Ok(value)
}
fn action(method: &str, params: Value) -> Value {
    json!({"op":"uiFiles","method":method,"params":params,"now":172800000})
}
fn fixture_step(actions: Vec<Value>) -> Value {
    let mut step = action("fixture", json!({"actions":actions}));
    step["requireSuccess"] = true.into();
    step
}
fn write(path: &str, bytes: impl AsRef<[u8]>) -> Value {
    json!({"kind":"write","path":path,"data":STANDARD.encode(bytes)})
}
fn mkdir(path: &str) -> Value {
    json!({"kind":"mkdir","path":path})
}
fn mtime(path: &str, ms: f64) -> Value {
    json!({"kind":"mtime","path":path,"ms":ms})
}
fn remove(path: &str) -> Value {
    json!({"kind":"remove","path":path})
}
fn asset(path: &str, mime: &str, bytes: impl AsRef<[u8]>) -> Value {
    json!({"path":path,"mediaType":mime,"contentHash":ivy_contracts::hash::digest(bytes.as_ref()),"byteLength":bytes.as_ref().len()})
}
fn file_action(method: &str, asset: Value) -> Value {
    action(
        method,
        json!({"uiId":"sample-ui","releaseId":"r1","asset":asset}),
    )
}
fn stage(asset: Value, bytes: impl AsRef<[u8]>) -> Value {
    action(
        "stage",
        json!({"uiId":"sample-ui","releaseId":"r1","asset":asset,"data":STANDARD.encode(bytes)}),
    )
}
fn promote(assets: Vec<Value>) -> Value {
    action(
        "promote",
        json!({"uiId":"sample-ui","releaseId":"r1","assets":assets}),
    )
}
fn sql(text: impl Into<String>) -> Value {
    json!({"op":"sql","sql":text.into(),"requireSuccess":true})
}
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}
fn push(steps: &mut Vec<Value>, step: Value) -> usize {
    let index = steps.len();
    steps.push(step);
    index
}
fn compare(steps: &[Value]) -> Value {
    let root = Root::new();
    compare_at(
        &root.0.join("native.sqlite"),
        &root.0.join("reference.sqlite"),
        steps,
    )
}
fn compare_at(native_path: &Path, reference_path: &Path, steps: &[Value]) -> Value {
    let actual = native(native_path, steps);
    let expected = oracle(reference_path, steps);
    for (index, (a, b)) in actual["rows"]
        .as_array()
        .unwrap()
        .iter()
        .zip(expected["rows"].as_array().unwrap())
        .enumerate()
    {
        assert_eq!(
            a, b,
            "UI files trace {index} ({}/{})",
            steps[index]["op"], steps[index]["method"]
        );
        if steps[index]["requireSuccess"] == true {
            assert!(b["error"].is_null(), "Fixture {index}: {}", b["error"]);
        }
    }
    for key in ["definitions", "metadata", "topics"] {
        assert_eq!(actual[key], expected[key]);
    }
    expected
}
fn success(result: &Value, index: usize) {
    assert!(
        result["rows"][index]["error"].is_null(),
        "{}",
        result["rows"][index]["error"]
    );
}
fn error(result: &Value, index: usize, code: &str) {
    assert_eq!(result["rows"][index]["error"]["data"]["code"], code);
}
fn has_file(result: &Value, index: usize, path: &str) -> bool {
    result["rows"][index]["fileTree"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["path"] == path)
}

#[test]
fn ui_asset_paths_preserve_utf16_length_well_formed_nfc_and_relative_guards() {
    let mut steps = vec![];
    for path in [
        "index.html",
        "assets/é.txt",
        "assets/💡.js",
        "a".repeat(1024).as_str(),
    ] {
        steps.push(action("assetPath", json!({"path":path})));
    }
    for path in [
        "",
        "/a",
        "a/",
        "a//b",
        ".",
        "..",
        "a/../b",
        "a/./b",
        "a\\b",
        "a%b",
        "a?b",
        "a#b",
        "a:b",
        "a\u{0}b",
        "a\u{1f}b",
        "a\u{7f}b",
        "e\u{301}.txt",
        "a".repeat(1025).as_str(),
    ] {
        steps.push(action("assetPath", json!({"path":path})));
    }
    for units in [vec![0xd800], vec![0xdc00], vec![0xd800, 0x61]] {
        let mut step = action("assetPath", json!({"path":"placeholder"}));
        step["parameterUnits"] = json!({"/path":units});
        steps.push(step);
    }
    let result = compare(&steps);
    for i in 0..4 {
        success(&result, i);
    }
    for i in 4..steps.len() {
        error(&result, i, "invalid_arguments");
    }
}

#[test]
fn ui_content_policy_distinguishes_root_published_and_case_sensitive_sandbox_suffix() {
    let steps = [
        "",
        "index.html",
        "note.sandbox.html",
        ".sandbox.html",
        "note.SANDBOX.html",
        "note.sandbox.html/x",
        "nested/é.sandbox.html",
    ]
    .map(|path| action("contentPolicy", json!({"path":path})));
    let result = compare(&steps);
    let rows = &result["rows"];
    assert!(!rows[0]["value"].as_str().unwrap().contains("frame-src"));
    assert!(
        rows[1]["value"]
            .as_str()
            .unwrap()
            .contains("frame-src 'self'")
    );
    for index in [2, 3, 6] {
        assert!(
            rows[index]["value"]
                .as_str()
                .unwrap()
                .starts_with("sandbox allow-scripts;")
        );
    }
    assert_eq!(rows[1]["value"], rows[4]["value"]);
    assert_eq!(rows[1]["value"], rows[5]["value"]);
}

#[test]
fn ui_file_ids_and_empty_release_short_circuits_match_original() {
    let mut steps = vec![];
    for (ui, release) in [
        ("sample-ui", "_r-1"),
        ("A", "r1"),
        ("bad_id", "r1"),
        ("", "r1"),
        ("sample-ui", ""),
        ("sample-ui", "a/b"),
    ] {
        steps.push(action("directory", json!({"uiId":ui,"releaseId":release})));
    }
    let empty = push(
        &mut steps,
        action(
            "promote",
            json!({"uiId":"INVALID","releaseId":"/","assets":[]}),
        ),
    );
    let install = push(
        &mut steps,
        action(
            "install",
            json!({"uiId":"INVALID","releaseId":"/","assets":[]}),
        ),
    );
    let mut invalid = stage(json!({}), "x");
    invalid["params"]["uiId"] = "INVALID".into();
    let stage = push(&mut steps, invalid);
    let null_stage = push(&mut steps, self::stage(Value::Null, "x"));
    let null_available = push(&mut steps, file_action("available", Value::Null));
    let result = compare(&steps);
    success(&result, 0);
    for index in 1..6 {
        error(&result, index, "invalid_arguments");
    }
    success(&result, empty);
    error(&result, install, "invalid_arguments");
    error(&result, stage, "artifact_changed");
    error(&result, null_stage, "internal_error");
    assert_eq!(result["rows"][null_available]["value"], false);
}

#[test]
fn ui_staging_promotion_immutable_retries_and_changed_bytes_match_original() {
    let html = asset("index.html", "text/html", "<p>ok</p>");
    let css = asset("assets/style.css", "text/css", "body{}");
    let mut steps = vec![
        file_action("available", html.clone()),
        file_action("check", html.clone()),
        stage(html.clone(), "<p>ok</p>"),
        stage(html.clone(), "<p>ok</p>"),
    ];
    let incomplete = push(&mut steps, promote(vec![html.clone(), css.clone()]));
    steps.push(stage(css.clone(), "body{}"));
    let published = push(&mut steps, promote(vec![html.clone(), css.clone()]));
    steps.push(json!({"op":"reopen"}));
    steps.push(promote(vec![html.clone(), css.clone()]));
    let read = push(&mut steps, file_action("read", html.clone()));
    steps.push(fixture_step(vec![write(
        "private/sample-ui/r1/index.html",
        "<p>NO</p>",
    )]));
    let available = push(&mut steps, file_action("available", html.clone()));
    let check = push(&mut steps, file_action("check", html.clone()));
    let changed = push(&mut steps, file_action("read", html.clone()));
    steps.push(stage(html.clone(), "<p>ok</p>"));
    let conflict = push(&mut steps, promote(vec![html.clone()]));
    let wrong = push(&mut steps, stage(html.clone(), "<p>NO</p>"));
    let result = compare(&steps);
    error(&result, incomplete, "ui_release_invalid");
    success(&result, published);
    assert!(has_file(
        &result,
        published,
        "private/sample-ui/r1/index.html"
    ));
    assert!(!has_file(
        &result,
        published,
        "private/.staging/sample-ui/r1"
    ));
    assert_eq!(result["rows"][read]["value"], STANDARD.encode("<p>ok</p>"));
    assert_eq!(result["rows"][available]["value"], true);
    assert_eq!(result["rows"][check]["value"], false);
    error(&result, changed, "ui_release_invalid");
    error(&result, conflict, "ui_release_invalid");
    error(&result, wrong, "artifact_changed");
}

#[test]
fn ui_staging_checks_existing_content_and_keeps_complete_release_fast_path() {
    let html = asset("index.html", "text/html", "a");
    let other = asset("index.html", "text/html", "b");
    let mut steps = vec![stage(html.clone(), "a")];
    let conflict = push(&mut steps, stage(other, "b"));
    steps.push(promote(vec![html.clone()]));
    steps.push(fixture_step(vec![write(
        "private/.staging/sample-ui/r1/index.html",
        "corrupt staging",
    )]));
    let fast = push(&mut steps, promote(vec![html.clone()]));
    let result = compare(&steps);
    error(&result, conflict, "mutation_conflict");
    success(&result, fast);
    assert!(has_file(
        &result,
        fast,
        "private/.staging/sample-ui/r1/index.html"
    ));
}

#[test]
fn ui_install_canonicalizes_json_and_cleans_staging_on_failure_and_retry() {
    let html = asset("index.html", "text/html", "<p>ok</p>");
    let json = asset("data/value.json", "application/json", "{\"a\":1,\"z\":2}");
    let install = action(
        "install",
        json!({"uiId":"sample-ui","releaseId":"r1","assets":[html,json]}),
    );
    let mut steps = vec![
        fixture_step(vec![
            write("bundle/index.html", "<p>ok</p>"),
            write("bundle/data/value.json", "{\"z\":2, \"a\":1}"),
            write("bundle/plain.json", "{  \"x\":1}"),
        ]),
        install.clone(),
    ];
    steps.push(file_action("read", json.clone()));
    steps.push(fixture_step(vec![remove("bundle")]));
    let fast = push(&mut steps, install.clone());
    let mut r2 = install.clone();
    r2["params"]["releaseId"] = "r2".into();
    let missing = push(&mut steps, r2.clone());
    steps.push(fixture_step(vec![
        write("bundle/index.html", "<p>ok</p>"),
        write("bundle/data/value.json", "{"),
    ]));
    let syntax = push(&mut steps, r2.clone());
    steps.push(fixture_step(vec![write(
        "bundle/data/value.json",
        "{\"a\":1,\"z\":3}",
    )]));
    let changed = push(&mut steps, r2.clone());
    steps.push(fixture_step(vec![write(
        "bundle/data/value.json",
        "{\"z\":2,\"a\":1}",
    )]));
    let retried = push(&mut steps, r2);
    let result = compare(&steps);
    success(&result, 1);
    success(&result, fast);
    error(&result, missing, "internal_error");
    error(&result, syntax, "internal_error");
    error(&result, changed, "artifact_changed");
    success(&result, retried);
    assert_eq!(
        result["rows"][2]["value"],
        STANDARD.encode("{\"a\":1,\"z\":2}")
    );
    for index in [missing, syntax, changed, retried] {
        assert!(
            !result["rows"][index]["fileTree"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["path"]
                    .as_str()
                    .unwrap()
                    .starts_with("private/.incoming/sample-ui-"))
        );
    }
}

#[test]
fn ui_private_reads_reject_escaping_directory_junction_but_keep_release_and_hardlink_semantics() {
    let descriptor = asset("nested/index.txt", "text/plain", "x");
    let top = asset("index.txt", "text/plain", "x");
    let mut steps = vec![fixture_step(vec![
        write("outside/index.txt", "x"),
        mkdir("private/sample-ui/r1"),
        json!({"kind":"junction","path":"private/sample-ui/r1/nested","target":"outside"}),
        json!({"kind":"hardlink","path":"private/sample-ui/r1/index.txt","target":"outside/index.txt"}),
    ])];
    let escaped = push(&mut steps, file_action("available", descriptor.clone()));
    let read = push(&mut steps, file_action("read", descriptor));
    let hardlink = push(&mut steps, file_action("check", top.clone()));
    steps.push(fixture_step(vec![
        json!({"kind":"junction","path":"private/sample-ui/r2","target":"outside"}),
    ]));
    let mut release = file_action("check", top.clone());
    release["params"]["releaseId"] = "r2".into();
    let release = push(&mut steps, release);
    let result = compare(&steps);
    assert_eq!(result["rows"][escaped]["value"], false);
    error(&result, read, "ui_release_invalid");
    assert_eq!(result["rows"][hardlink]["value"], true);
    assert_eq!(result["rows"][release]["value"], true);
}

#[test]
fn ui_collection_preserves_retained_releases_uses_inclusive_cutoff_and_validates_unretained_ids() {
    let mut steps = vec![fixture_step(vec![
        write("private/sample-ui/old/a", "x"),
        write("private/sample-ui/boundary/a", "x"),
        write("private/sample-ui/fresh/a", "x"),
        write("private/sample-ui/kept/a", "x"),
        write("private/sample-ui/not-a-directory", "x"),
        mtime("private/sample-ui/old", 86400000.0 - 1.0),
        mtime("private/sample-ui/boundary", 86400000.0),
        mtime("private/sample-ui/fresh", 86400001.0),
        mtime("private/sample-ui/kept", 0.0),
    ])];
    let collected = push(
        &mut steps,
        action("collect", json!({"uiId":"sample-ui","retained":["kept"]})),
    );
    steps.push(fixture_step(vec![mkdir("private/sample-ui/bad release")]));
    let invalid = push(
        &mut steps,
        action("collect", json!({"uiId":"sample-ui","retained":[]})),
    );
    let retained = push(
        &mut steps,
        action(
            "collect",
            json!({"uiId":"sample-ui","retained":["bad release","kept"]}),
        ),
    );
    steps.push(fixture_step(vec![remove("private/sample-ui")]));
    let missing = push(
        &mut steps,
        action("collect", json!({"uiId":"sample-ui","retained":[]})),
    );
    let result = compare(&steps);
    success(&result, collected);
    assert!(!has_file(&result, collected, "private/sample-ui/boundary"));
    assert!(!has_file(&result, collected, "private/sample-ui/old"));
    assert!(has_file(&result, collected, "private/sample-ui/fresh/a"));
    assert!(has_file(&result, collected, "private/sample-ui/kept/a"));
    error(&result, invalid, "invalid_arguments");
    success(&result, retained);
    success(&result, missing);
}

#[test]
fn ui_collection_temporary_depth_strict_cutoff_and_each_ui_clock_match_original() {
    let mut steps = vec![fixture_step(vec![
        write("private/.incoming/old/a", "x"),
        write("private/.incoming/boundary/a", "x"),
        write("private/.staging/a/old/a", "x"),
        mkdir("private/.staging/empty"),
        write("private/a/r1/a", "x"),
        write("private/b/r1/a", "x"),
        mtime("private/.incoming/old", 86399999.0),
        mtime("private/.incoming/boundary", 86400000.0),
        mtime("private/.staging/a/old", 86399999.0),
        mtime("private/a/r1", 86400000.0),
        mtime("private/b/r1", 86400000.0),
    ])];
    let temp = push(&mut steps, action("collectTemporary", json!({})));
    let mut collect = action("collectAll", json!({"retained":{}}));
    collect["clock"] = json!([172799999.0, 172800000.0, 172800001.0]);
    let all = push(&mut steps, collect);
    let result = compare(&steps);
    success(&result, temp);
    assert!(!has_file(&result, temp, "private/.incoming/old"));
    assert!(has_file(&result, temp, "private/.incoming/boundary/a"));
    assert!(!has_file(&result, temp, "private/.staging/a"));
    assert!(!has_file(&result, temp, "private/.staging/empty"));
    success(&result, all);
    assert!(has_file(&result, all, "private/a/r1/a"));
    assert!(!has_file(&result, all, "private/b"));
    assert!(!has_file(&result, all, "private/.incoming/boundary"));
}

fn legacy_seed() -> Vec<Value> {
    let mut steps = vec![];
    for (key, mime) in [
        ("ui/asset-owner", "application/json"),
        ("ui/asset-file", "text/html"),
        ("ui/asset-binary", "image/png"),
    ] {
        let mut definition = json!({"key":key,"version":"1.0.0","owner":{"kind":"agent"},"mediaType":mime,"specMarkdown":"Synthetic legacy file.","retention":{"objects":{"mode":"retain"},"revisions":{"mode":"all"}}});
        if mime == "application/json" {
            definition["jsonSchema"] = true.into();
        }
        steps.push(json!({"op":"register","owner":definition["owner"],"definition":definition,"requireSuccess":true}));
    }
    let mut sql = String::from("BEGIN IMMEDIATE;");
    for (id, key, owner, encoding, content, bytes) in [
        (
            "owner",
            "ui/asset-owner",
            "NULL",
            "json",
            "'{}'",
            b"{}".as_slice(),
        ),
        (
            "file",
            "ui/asset-file",
            "'owner'",
            "text",
            "'<p>legacy</p>'",
            b"<p>legacy</p>".as_slice(),
        ),
        (
            "binary",
            "ui/asset-binary",
            "'owner'",
            "base64",
            "X'0001FF'",
            [0, 1, 255].as_slice(),
        ),
    ] {
        sql.push_str(&format!("INSERT INTO objects VALUES(NULL,'{id}',NULL,{owner},'{id}','/{id}',0,NULL,1,'{key}',1,NULL,0,'2026-01-01','2026-01-01'); INSERT INTO revisions VALUES('{id}',1,'{key}','1.0.0','{encoding}',{content},{}, {},'2026-01-01','{{}}'); INSERT INTO object_fts(rowid,object_id,name,text) SELECT search_id,id,name,'' FROM objects WHERE id='{id}';INSERT INTO retention_families VALUES('{key}','fixture',1, '{{\"objectCount\":1,\"revisionCount\":1,\"byteLength\":{}}}');",quote(&ivy_contracts::hash::digest(bytes)),bytes.len(),bytes.len()));
    }
    let mut html = asset("index.html", "text/html", "<p>legacy</p>");
    html["objectId"] = "file".into();
    html["revision"] = 1.into();
    html["byteLength"] = 99999.into();
    let mut binary = asset("image.png", "image/png", [0, 1, 255]);
    binary["objectId"] = "binary".into();
    binary["revision"] = 1.into();
    let release = json!({"releaseId":"r1","entryPath":"index.html","requirements":{"hiveProtocol":1,"contracts":[],"services":[]},"assets":[html,binary],"preserved":{"legacy":true}});
    sql.push_str(&format!("INSERT INTO apps VALUES('sample-ui','{{}}','r1',NULL);INSERT INTO app_releases VALUES('sample-ui','r1',{});INSERT INTO app_assets VALUES('sample-ui','r1','index.html','file',1,'text/html',{});COMMIT;",quote(&release.to_string()),quote(&ivy_contracts::hash::digest("<p>legacy</p>"))));
    steps.push(self::sql(sql));
    steps
}

#[test]
fn ui_legacy_migration_retires_owned_objects_updates_usage_cleans_only_stale_partials_and_reopens()
{
    let mut steps = legacy_seed();
    steps.push(fixture_step(vec![
        write(
            "private/sample-ui/r1/index.html.00000000-0000-4000-8000-000000000001.partial",
            "interrupted",
        ),
        write(
            "private/sample-ui/r1/index.html.A0000000-0000-4000-8000-000000000001.partial",
            "keep uppercase",
        ),
        write(
            "private/sample-ui/r1/index.html.unrelated.partial",
            "keep unrelated",
        ),
    ]));
    let migrated = push(&mut steps, action("migrateLegacy", json!({})));
    steps.push(json!({"op":"reopen"}));
    let repeated = push(&mut steps, action("migrateLegacy", json!({})));
    let read = push(
        &mut steps,
        file_action("read", asset("image.png", "image/png", [0, 1, 255])),
    );
    let result = compare(&steps);
    success(&result, migrated);
    success(&result, repeated);
    assert_eq!(result["rows"][read]["value"], STANDARD.encode([0, 1, 255]));
    let state = &result["rows"][migrated]["state"];
    assert_eq!(state["objects"], json!([]));
    assert_eq!(state["fts"], json!([]));
    assert_eq!(state["appAssets"], json!([]));
    for family in state["retention"].as_array().unwrap() {
        let summary: Value =
            serde_json::from_str(family["summary_json"].as_str().unwrap()).unwrap();
        assert_eq!(
            summary,
            json!({"objectCount":0,"revisionCount":0,"byteLength":0})
        );
    }
    let release: Value =
        serde_json::from_str(state["releases"][0]["release_json"].as_str().unwrap()).unwrap();
    assert_eq!(release["preserved"]["legacy"], true);
    assert_eq!(release["assets"][0]["byteLength"], 13);
    assert!(release["assets"][0].get("objectId").is_none());
    assert!(!has_file(
        &result,
        migrated,
        "private/sample-ui/r1/index.html.00000000-0000-4000-8000-000000000001.partial"
    ));
    assert!(has_file(
        &result,
        migrated,
        "private/sample-ui/r1/index.html.A0000000-0000-4000-8000-000000000001.partial"
    ));
}

#[test]
fn ui_legacy_sql_rollback_keeps_written_files_and_retry_commits_without_rewriting_them() {
    let mut steps = legacy_seed();
    steps.push(sql("CREATE TRIGGER fail_migration BEFORE UPDATE ON app_releases BEGIN SELECT RAISE(ABORT,'fixture failure');END;"));
    let failed = push(&mut steps, action("migrateLegacy", json!({})));
    steps.push(sql("DROP TRIGGER fail_migration;"));
    steps.push(json!({"op":"reopen"}));
    let retried = push(&mut steps, action("migrateLegacy", json!({})));
    let result = compare(&steps);
    error(&result, failed, "internal_error");
    assert!(has_file(&result, failed, "private/sample-ui/r1/index.html"));
    assert!(has_file(&result, failed, "private/sample-ui/r1/image.png"));
    assert_eq!(
        result["rows"][failed]["state"]["objects"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        result["rows"][failed]["state"]["appAssets"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    success(&result, retried);
    assert_eq!(result["rows"][retried]["state"]["objects"], json!([]));
}

#[test]
fn ui_legacy_migration_uses_nodes_utf8_replacement_for_sql_text_before_checksum() {
    let mut steps = legacy_seed();
    let mut migration = action("migrateLegacy", json!({}));
    let hash = quote(&ivy_contracts::hash::digest("\u{fffd}"));
    // Deliberately corrupt TEXT at the SQLite byte boundary, not through JSON or
    // a policy replacement. Observe only after migration retires the source rows.
    migration["prepareSql"]=format!("DROP TRIGGER revisions_immutable_update;UPDATE revisions SET content=CAST(X'FF' AS TEXT),content_hash={hash},byte_length=3 WHERE object_id='file';UPDATE app_releases SET release_json=json_set(release_json,'$.assets[0].contentHash',{hash});UPDATE retention_families SET summary_json=json_set(summary_json,'$.byteLength',3) WHERE contract_key='ui/asset-file';").into();
    migration["requireSuccess"] = true.into();
    let migrated = push(&mut steps, migration);
    let read = push(
        &mut steps,
        file_action("read", asset("index.html", "text/html", "\u{fffd}")),
    );
    let result = compare(&steps);
    success(&result, migrated);
    success(&result, read);
    assert_eq!(result["rows"][read]["value"], STANDARD.encode("\u{fffd}"));
    assert_eq!(result["rows"][migrated]["state"]["objects"], json!([]));
}

#[test]
fn ui_legacy_invalid_assets_missing_revisions_and_existing_changed_files_fail_before_sql_cleanup() {
    let mut steps = legacy_seed();
    steps.push(fixture_step(vec![write(
        "private/sample-ui/r1/index.html",
        "wrong",
    )]));
    let changed = push(&mut steps, action("migrateLegacy", json!({})));
    steps.push(fixture_step(vec![remove("private")]));
    steps.push(sql(
        "UPDATE app_releases SET release_json=json_set(release_json,'$.assets[0].revision',0);",
    ));
    let missing = push(&mut steps, action("migrateLegacy", json!({})));
    steps.push(sql(
        "UPDATE app_releases SET release_json=json_set(release_json,'$.assets[0].revision',1.5);",
    ));
    let invalid = push(&mut steps, action("migrateLegacy", json!({})));
    steps.push(sql("UPDATE app_releases SET release_json=json_set(release_json,'$.assets[0].revision',1,'$.assets[0].path','../escape');"));
    let path = push(&mut steps, action("migrateLegacy", json!({})));
    steps.push(sql("UPDATE app_releases SET release_json=json_set(release_json,'$.assets[0].path','index.html');DROP TRIGGER revisions_immutable_update;UPDATE revisions SET content='corrupt bytes' WHERE object_id='file';"));
    let checksum = push(&mut steps, action("migrateLegacy", json!({})));
    steps.push(sql("UPDATE revisions SET content='<p>legacy</p>' WHERE object_id='file';UPDATE app_releases SET release_json=json_set(release_json,'$.assets',json('[null]'));"));
    let null_asset = push(&mut steps, action("migrateLegacy", json!({})));
    steps.push(sql("UPDATE app_releases SET release_json='null';"));
    let null = push(&mut steps, action("migrateLegacy", json!({})));
    let result = compare(&steps);
    error(&result, changed, "storage_invalid");
    error(&result, missing, "storage_invalid");
    error(&result, invalid, "storage_invalid");
    error(&result, path, "invalid_arguments");
    error(&result, checksum, "storage_invalid");
    error(&result, null_asset, "internal_error");
    success(&result, null);
    assert_eq!(result["rows"][null]["state"]["objects"], json!([]));
}
