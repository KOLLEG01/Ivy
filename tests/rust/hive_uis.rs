use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_hive::{ui_files::UiFiles, uis::Uis};
use std::path::Path;

fn text<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a JsString> {
    value
        .get(key)
        .and_then(IvyJson::as_js_string)
        .ok_or_else(IvyError::internal)
}
pub(super) fn execute(
    registry: &Registry,
    store: &mut Store,
    runtime: &ObservedRuntime,
    parameters: &IvyJson,
    step: &Value,
    database: &Path,
) -> IvyResult<IvyJson> {
    let files = UiFiles::new(database.with_extension("files").join("private"));
    let owner = Uis::new(registry, &files);
    let context = step
        .get("context")
        .map(|v| {
            store.authenticate(&AuthContext {
                credential_digest: v["credentialDigest"].as_str().unwrap().into(),
                session_digest: v["sessionDigest"].as_str().map(JsString::from),
            })
        })
        .transpose()?
        .map(|p| AuthenticatedContext {
            principal_id: p.principal_id,
            credential_digest: p.credential_digest,
        });
    let now = step["now"].as_u64().unwrap();
    let clock = || now as f64;
    store.with_budget_mut(
        if step["overdue"] == true {
            Duration::ZERO
        } else {
            Duration::from_secs(10)
        },
        |store| match step["method"].as_str().unwrap() {
            "constructor" => Ok(IvyJson::Null),
            "list" => owner.list(store).map(IvyJson::Array),
            "get" => owner.get(store, text(parameters, "uiId")?),
            "catalog" => owner.catalog(store, parameters),
            "releases" => owner.releases(store, parameters),
            "resolveSlug" => owner
                .resolve_slug(store, text(parameters, "slug")?)
                .map(|s| s.map(IvyJson::String).unwrap_or(IvyJson::Null)),
            "inspect" => owner.inspect(store, parameters, runtime, &clock),
            "entry" => owner.entry(store, text(parameters, "uiId")?, runtime, &clock),
            "asset" => owner.asset(
                store,
                text(parameters, "uiId")?,
                text(parameters, "releaseId")?,
                text(parameters, "path")?,
            ),
            "currentAsset" => owner.current_asset(
                store,
                text(parameters, "uiId")?,
                parameters.get("path").and_then(IvyJson::as_js_string),
                runtime,
                &clock,
            ),
            "stageAsset" => owner.stage_asset(store, context.as_ref().unwrap(), parameters, now),
            "deploy" => owner.deploy(store, context.as_ref().unwrap(), parameters, now),
            "rollback" => owner.rollback(
                store,
                context.as_ref().unwrap(),
                parameters,
                now,
                runtime,
                &clock,
            ),
            "collectFiles" => owner.collect_files(store, &clock).map(|_| IvyJson::Null),
            "cursor" => store
                .cursor(
                    parameters.get("identity").unwrap(),
                    parameters.get("boundary").unwrap(),
                )
                .map(IvyJson::from),
            _ => Err(IvyError::internal()),
        },
    )
}
fn operation(method: &str, params: Value) -> Value {
    json!({"op":"uis","method":method,"params":params,"now":100000})
}
fn mutation(method: &str, mut params: Value, nonce: &str) -> Value {
    params["mutationId"] = format!("epoch:100000:{nonce}").into();
    let mut step = operation(method, params);
    step["context"] = json!({"credentialDigest":digest(true)});
    step
}
fn setup() -> Vec<Value> {
    let mut s = super::setup();
    s.push(sql("UPDATE metadata SET value='0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' WHERE key='cursor_secret';"));
    s.push(json!({"op":"reopen"}));
    s
}
fn metadata(ui: &str) -> Value {
    json!({"uiId":ui,"slug":ui,"displayName":ui,"description":"Synthetic UI.","iconKey":"ui"})
}
fn asset(path: &str, mime: &str, bytes: impl AsRef<[u8]>) -> Value {
    json!({"path":path,"mediaType":mime,"contentHash":ivy_contracts::hash::digest(bytes.as_ref()),"byteLength":bytes.as_ref().len()})
}
fn release(id: &str, bytes: &str) -> Value {
    json!({"releaseId":id,"entryPath":"index.html","requirements":{"hiveProtocol":1,"contracts":[],"services":[]},"assets":[asset("index.html","text/html",bytes)]})
}
fn stage(id: &str, bytes: &str, nonce: &str) -> Value {
    mutation(
        "stageAsset",
        json!({"uiId":"sample-ui","releaseId":id,"asset":asset("index.html","text/html",bytes),"base64":STANDARD.encode(bytes)}),
        nonce,
    )
}
fn deploy(id: &str, bytes: &str, expected: Value, nonce: &str) -> Value {
    mutation(
        "deploy",
        json!({"metadata":metadata("sample-ui"),"release":release(id,bytes),"expectedReleaseId":expected}),
        nonce,
    )
}
fn rollback(id: &str, expected: &str, nonce: &str) -> Value {
    mutation(
        "rollback",
        json!({"uiId":"sample-ui","releaseId":id,"expectedReleaseId":expected}),
        nonce,
    )
}
fn inspect() -> Value {
    operation("inspect", json!({"uiId":"sample-ui"}))
}
fn seed_ui(ui: &str, meta: Value, current: Value, release_rows: Vec<(&str, Value)>) -> Value {
    let current = current.as_str().map(quote).unwrap_or_else(|| "NULL".into());
    let mut s = format!(
        "INSERT INTO apps VALUES({},{},{current},NULL);",
        quote(ui),
        quote(&meta.to_string())
    );
    for (id, value) in release_rows {
        s.push_str(&format!(
            "INSERT INTO app_releases VALUES({},{},{});",
            quote(ui),
            quote(id),
            quote(&value.to_string())
        ));
    }
    sql(s)
}
fn fixture(actions: Vec<Value>) -> Value {
    json!({"op":"uiFiles","method":"fixture","params":{"actions":actions},"now":100000,"requireSuccess":true})
}
fn write(path: &str, bytes: &str) -> Value {
    json!({"kind":"write","path":path,"data":STANDARD.encode(bytes)})
}
fn mtime(path: &str, ms: u64) -> Value {
    json!({"kind":"mtime","path":path,"ms":ms})
}
fn file(method: &str, id: &str, bytes: &str) -> Value {
    json!({"op":"uiFiles","method":method,"params":{"uiId":"sample-ui","releaseId":id,"asset":asset("index.html","text/html",bytes)},"now":100000})
}
fn follow(mut step: Value, from: usize) -> Value {
    step["params"]["cursor"] = Value::Null;
    step["identityBindings"] =
        json!([{"from":from,"select":"/value/nextCursor","at":"/params/cursor"}]);
    step
}
fn token(s: &mut Vec<Value>, method: &str, params: Value, boundary: Value) -> usize {
    let mut identity = params;
    identity["method"] = method.into();
    push(
        s,
        operation("cursor", json!({"identity":identity,"boundary":boundary})),
    )
}
fn from_token(mut step: Value, from: usize) -> Value {
    step["params"]["cursor"] = Value::Null;
    step["identityBindings"] = json!([{"from":from,"select":"/value","at":"/params/cursor"}]);
    step
}
fn compare(steps: &[Value]) -> Value {
    let root = Root::new();
    let expected = oracle(&root.0.join("reference.sqlite"), steps);
    let actual = native_with_runtime(
        &root.0.join("native.sqlite"),
        steps,
        Some(&expected["registryRuntime"]),
    );
    assert_eq!(actual["rows"].as_array().unwrap().len(), steps.len());
    for (i, (a, b)) in actual["rows"]
        .as_array()
        .unwrap()
        .iter()
        .zip(expected["rows"].as_array().unwrap())
        .enumerate()
    {
        assert_eq!(
            a, b,
            "Uis trace {i} ({}/{})",
            steps[i]["op"], steps[i]["method"]
        );
        if matches!(
            steps[i]["op"].as_str(),
            Some("sql" | "configure" | "register")
        ) || steps[i]["requireSuccess"] == true
        {
            assert!(b["error"].is_null(), "Fixture {i}: {}", b["error"]);
        }
    }
    for key in ["definitions", "metadata", "topics"] {
        assert_eq!(actual[key], expected[key]);
    }
    expected
}
fn value(r: &Value, i: usize) -> &Value {
    &r["rows"][i]["value"]
}
fn success(r: &Value, i: usize) {
    assert!(r["rows"][i]["error"].is_null(), "{}", r["rows"][i]["error"]);
}
fn error(r: &Value, i: usize, code: &str) {
    assert_eq!(r["rows"][i]["error"]["data"]["code"], code);
}
fn has_file(r: &Value, i: usize, path: &str) -> bool {
    r["rows"][i]["fileTree"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["path"] == path)
}

#[test]
fn uis_catalog_filters_metadata_fills_pages_and_preserves_priority_utf8_cursor_and_restart() {
    let mut s = setup();
    for (id, priority, valid) in [
        ("high", 100, true),
        ("bad-high", 99, false),
        ("also-bad", 98, false),
        ("a", 0, true),
        ("z", 0, true),
        ("é", 0, true),
        ("low", -1, true),
    ] {
        let mut meta = metadata(id);
        meta["priority"] = priority.into();
        if id == "é" {
            meta.as_object_mut().unwrap().remove("slug");
        }
        if !valid {
            meta["uiId"] = "other".into();
        }
        s.push(seed_ui(id, meta, Value::Null, vec![]));
    }
    s.push(sql("INSERT INTO apps VALUES('malformed','{',NULL,NULL);"));
    let first = push(&mut s, operation("catalog", json!({"limit":2})));
    s.push(json!({"op":"reopen"}));
    let second = push(
        &mut s,
        follow(operation("catalog", json!({"limit":2})), first),
    );
    let third = push(
        &mut s,
        follow(operation("catalog", json!({"limit":2})), second),
    );
    let list = push(&mut s, operation("list", json!({})));
    let bad = push(&mut s, operation("get", json!({"uiId":"bad-high"})));
    let slug = push(&mut s, operation("resolveSlug", json!({"slug":"a"})));
    let mut mismatch = follow(operation("catalog", json!({"limit":3})), first);
    let mismatch = push(&mut s, mismatch.take());
    let t = token(
        &mut s,
        "uis.catalog",
        json!({"limit":2}),
        json!(["1000000:a"]),
    );
    let coerced = push(
        &mut s,
        from_token(operation("catalog", json!({"limit":2})), t),
    );
    let t = token(&mut s, "uis.catalog", json!({}), json!({"toString":5}));
    let coercion = push(&mut s, from_token(operation("catalog", json!({})), t));
    let mut overdue = operation("catalog", json!({}));
    overdue["overdue"] = true.into();
    let overdue = push(&mut s, overdue);
    let r = compare(&s);
    for i in [first, second, third, list, slug, coerced] {
        success(&r, i);
    }
    assert_eq!(
        value(&r, first)["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["metadata"]["uiId"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["high", "a"]
    );
    assert_eq!(value(&r, second)["items"][0]["metadata"]["uiId"], "z");
    assert_eq!(value(&r, third)["items"][0]["metadata"]["uiId"], "low");
    assert_eq!(value(&r, list).as_array().unwrap().len(), 5);
    assert_eq!(value(&r, slug), "a");
    error(&r, bad, "not_found");
    error(&r, mismatch, "invalid_cursor");
    error(&r, coercion, "internal_error");
    error(&r, overdue, "limit_exceeded");
}

#[test]
fn uis_release_pages_use_raw_sql_summaries_guard_metadata_and_enforce_actual_byte_budget() {
    let mut s = setup();
    s.push(seed_ui(
        "sample-ui",
        metadata("sample-ui"),
        json!("r1"),
        vec![
            ("r1", release("r1", "a")),
            ("r2", json!({"entryPath":null,"assets":null})),
            ("r3", release("different-key", "c")),
        ],
    ));
    let get = push(&mut s, operation("get", json!({"uiId":"sample-ui"})));
    let first = push(
        &mut s,
        operation("releases", json!({"uiId":"sample-ui","limit":1})),
    );
    let next = push(
        &mut s,
        follow(
            operation("releases", json!({"uiId":"sample-ui","limit":1})),
            first,
        ),
    );
    s.push(sql("INSERT INTO app_releases VALUES('sample-ui','z','{');"));
    let malformed = push(&mut s, operation("releases", json!({"uiId":"sample-ui"})));
    let missing = push(
        &mut s,
        operation("releases", json!({"uiId":"absent","cursor":"forged"})),
    );
    s.push(sql("DELETE FROM app_releases;"));
    let big = "x".repeat(1_100_000);
    s.push(sql(format!(
        "INSERT INTO app_releases VALUES('sample-ui','a',{}),('sample-ui','b',{});",
        quote(&json!({"entryPath":big,"assets":[]}).to_string()),
        quote(&json!({"entryPath":big,"assets":[]}).to_string())
    )));
    let page = push(
        &mut s,
        operation("releases", json!({"uiId":"sample-ui","limit":2})),
    );
    let last = push(
        &mut s,
        follow(
            operation("releases", json!({"uiId":"sample-ui","limit":2})),
            page,
        ),
    );
    s.push(sql(format!(
        "DELETE FROM app_releases;INSERT INTO app_releases VALUES('sample-ui','a',{});",
        quote(&json!({"entryPath":"x".repeat(2_100_000),"assets":[]}).to_string())
    )));
    let oversized = push(&mut s, operation("releases", json!({"uiId":"sample-ui"})));
    let r = compare(&s);
    for i in [get, first, next, page, last] {
        success(&r, i);
    }
    assert_eq!(value(&r, get)["releases"].as_array().unwrap().len(), 2);
    assert_eq!(
        value(&r, next)["items"][0],
        json!({"releaseId":"r2","entryPath":"null","assetCount":0})
    );
    error(&r, malformed, "internal_error");
    error(&r, missing, "not_found");
    assert_eq!(value(&r, page)["items"].as_array().unwrap().len(), 1);
    assert!(value(&r, page)["nextCursor"].is_string());
    assert!(value(&r, last)["nextCursor"].is_null());
    error(&r, oversized, "result_too_large");
}

#[test]
fn uis_stage_deploy_receipts_cas_slug_immutability_pruning_and_reopen_match_original() {
    let mut s = setup();
    let initial = deploy("r1", "a", Value::Null, "deploy-r1");
    let staged = stage("r1", "a", "stage-r1");
    s.push(staged.clone());
    let first = push(&mut s, initial.clone());
    s.push(stage("r2", "b", "stage-r2"));
    let second = push(&mut s, deploy("r2", "b", json!("r1"), "deploy-r2"));
    s.push(stage("r3", "c", "stage-r3"));
    let third = push(&mut s, deploy("r3", "c", json!("r2"), "deploy-r3"));
    let stale = push(&mut s, deploy("none", "x", Value::Null, "stale"));
    let mut slug = deploy("missing", "x", Value::Null, "slug");
    slug["params"]["metadata"]["uiId"] = "other-ui".into();
    let slug = push(&mut s, slug);
    let mut immutable = deploy("r3", "c", json!("r3"), "immutable");
    immutable["params"]["release"]["requirements"]["services"] =
        json!([{"serviceName":"missing","namespace":"missing","interfaceVersion":"1.0.0"}]);
    let immutable = push(&mut s, immutable);
    s.push(json!({"op":"reopen"}));
    let replay = push(&mut s, initial);
    s.push(fixture(vec![
        json!({"kind":"remove","path":"private/sample-ui/r1"}),
    ]));
    let replay_stage = push(&mut s, staged.clone());
    let mut conflict = staged;
    conflict["params"]["base64"] = "Yg==".into();
    let conflict = push(&mut s, conflict);
    let mut invalid = stage("new", "x", "base64-invalid");
    invalid["params"]["base64"] = "eA".into();
    let invalid = push(&mut s, invalid);
    let r = compare(&s);
    for i in [first, second, third, replay, replay_stage] {
        success(&r, i);
    }
    assert_eq!(value(&r, replay), value(&r, first));
    assert_eq!(
        r["rows"][third]["state"]["releases"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        r["rows"][replay]["state"]["apps"][0]["current_release_id"],
        "r3"
    );
    assert!(!has_file(&r, replay_stage, "private/.staging/sample-ui/r1"));
    error(&r, stale, "release_conflict");
    error(&r, slug, "invalid_arguments");
    error(&r, immutable, "release_conflict");
    error(&r, conflict, "mutation_conflict");
    error(&r, invalid, "invalid_arguments");
}

#[test]
fn uis_deploy_preserves_promoted_files_after_policy_and_sql_failures_and_retries_exact_identity() {
    let mut s = setup();
    s.push(stage("r1", "a", "stage-r1"));
    let request = deploy("r1", "a", Value::Null, "sql-retry");
    s.push(sql("CREATE TRIGGER fail_ui BEFORE INSERT ON app_releases BEGIN SELECT RAISE(ABORT,'fixture');END;"));
    let failed = push(&mut s, request.clone());
    s.push(sql("DROP TRIGGER fail_ui;"));
    let retry = push(&mut s, request);
    for (id, kind) in [
        ("bad-entry", "path"),
        ("duplicate", "duplicate"),
        ("bad-mime", "mime"),
        ("not-html", "html"),
    ] {
        let mut staged = stage(id, "x", &format!("stage-{id}"));
        let mut deployed = deploy(id, "x", json!("r1"), &format!("deploy-{id}"));
        match kind {
            "path" => deployed["params"]["release"]["entryPath"] = "../index.html".into(),
            "duplicate" => {
                let item = deployed["params"]["release"]["assets"][0].clone();
                deployed["params"]["release"]["assets"]
                    .as_array_mut()
                    .unwrap()
                    .push(item);
            }
            "mime" => {
                staged["params"]["asset"]["mediaType"] = "application/x-unknown".into();
                deployed["params"]["release"]["assets"][0]["mediaType"] =
                    "application/x-unknown".into();
            }
            "html" => {
                staged["params"]["asset"]["mediaType"] = "text/plain".into();
                deployed["params"]["release"]["assets"][0]["mediaType"] = "text/plain".into();
            }
            _ => unreachable!(),
        }
        s.push(staged);
        s.push(deployed);
    }
    let r = compare(&s);
    error(&r, failed, "internal_error");
    assert!(has_file(&r, failed, "private/sample-ui/r1/index.html"));
    assert_eq!(r["rows"][failed]["state"]["apps"], json!([]));
    success(&r, retry);
    for index in (retry + 2..s.len()).step_by(2) {
        error(&r, index, "invalid_arguments");
        let id = s[index]["params"]["release"]["releaseId"].as_str().unwrap();
        assert!(has_file(
            &r,
            index,
            &format!("private/sample-ui/{id}/index.html")
        ));
        assert_eq!(
            r["rows"][index]["state"]["apps"][0]["current_release_id"],
            "r1"
        );
    }
}

#[test]
fn uis_inspection_keeps_pointer_metadata_independence_manifest_guards_issue_rank_order_and_cap() {
    let mut s = setup();
    s.push(seed_ui("sample-ui", json!({}), Value::Null, vec![]));
    let none = push(&mut s, inspect());
    let missing = push(
        &mut s,
        operation("inspect", json!({"uiId":"sample-ui","releaseId":"missing"})),
    );
    s.push(sql("INSERT INTO app_releases VALUES('sample-ui','broken','{');UPDATE apps SET current_release_id='broken';"));
    let broken = push(&mut s, inspect());
    s.push(sql(format!(
        "UPDATE app_releases SET release_json={};",
        quote(&release("other-id", "x").to_string())
    )));
    let identity = push(&mut s, inspect());
    let mut manifest = release("broken", "x");
    manifest["assets"] = json!(
        (0..40)
            .map(|_| asset("missing.txt", "text/plain", "x"))
            .collect::<Vec<_>>()
    );
    manifest["requirements"]["contracts"] =
        json!([{"key":"absent","readVersions":["1.0.0"],"writeVersions":["1.0.0"]}]);
    manifest["requirements"]["services"] =
        json!([{"serviceName":"absent","namespace":"fixture","interfaceVersion":"1.0.0"}]);
    s.push(sql(format!(
        "UPDATE app_releases SET release_json={};",
        quote(&manifest.to_string())
    )));
    let capped = push(&mut s, inspect());
    let mut manifest = release("broken", "x");
    manifest["assets"][0]["path"] = "e\u{301}.txt".into();
    s.push(sql(format!(
        "UPDATE app_releases SET release_json={};",
        quote(&manifest.to_string())
    )));
    let path = push(&mut s, inspect());
    let get = push(&mut s, operation("get", json!({"uiId":"sample-ui"})));
    let mut raw = release("broken", "x");
    raw["assets"][0]["path"] = "raw-surrogate".into();
    let encoded = raw
        .to_string()
        .replace("raw-surrogate", &format!("{}ud800", char::from(92)));
    s.push(sql(format!(
        "UPDATE app_releases SET release_json={};",
        quote(&encoded)
    )));
    let mut raw_inspect = inspect();
    raw_inspect["lossless"] = true.into();
    let raw_inspect = push(&mut s, raw_inspect);
    let r = compare(&s);
    assert_eq!(value(&r, none)["status"], "unselected");
    assert_eq!(value(&r, missing)["status"], "missing");
    for i in [broken, identity, path, capped] {
        assert_eq!(value(&r, i)["status"], "invalid");
    }
    assert_eq!(value(&r, capped)["issues"].as_array().unwrap().len(), 32);
    assert_eq!(value(&r, capped)["issuesTruncated"], true);
    assert_eq!(
        value(&r, capped)["issues"][0]["message"],
        "The private UI file is missing or changed."
    );
    assert_eq!(
        value(&r, capped)["issues"][1]["message"],
        "An asset path occurs more than once."
    );
    assert_eq!(
        value(&r, path)["issues"][0]["message"],
        "An asset path is invalid."
    );
    error(&r, get, "not_found");
    success(&r, raw_inspect);
    assert!(
        value(&r, raw_inspect)["encoded"]
            .as_str()
            .unwrap()
            .contains(&format!("{}ud800", char::from(92)))
    );
}

fn contract() -> Value {
    json!({"op":"register","owner":{"kind":"agent"},"definition":{"key":"fixture/ui","version":"1.0.0","owner":{"kind":"agent"},"mediaType":"text/plain","retention":{"objects":{"mode":"retain"},"revisions":{"mode":"all"}},"specMarkdown":"Synthetic required contract."}})
}
#[test]
fn uis_requirements_use_actual_registry_catalog_versions_readiness_contracts_and_stale_restart() {
    let mut s = setup();
    let mut manifest = release("r1", "x");
    manifest["requirements"]["contracts"] =
        json!([{"key":"fixture/ui","readVersions":["1.0.0"],"writeVersions":["1.0.0"]}]);
    manifest["requirements"]["services"] =
        json!([{"serviceName":"sample","namespace":"fixture","interfaceVersion":"1.0.0"}]);
    s.push(stage("r1", "x", "stage"));
    let request = mutation(
        "deploy",
        json!({"metadata":metadata("sample-ui"),"release":manifest,"expectedReleaseId":null}),
        "deploy",
    );
    let deployed = push(&mut s, request);
    let absent = push(&mut s, inspect());
    let entry = push(&mut s, operation("entry", json!({"uiId":"sample-ui"})));
    s.push(connect("node", "sample", "host", 1));
    let no_catalog = push(&mut s, inspect());
    let no_entry = push(&mut s, operation("entry", json!({"uiId":"sample-ui"})));
    let mut wrong = catalog("fixture");
    wrong["namespaces"][0]["tools"][0]["interfaceVersion"] = "2.0.0".into();
    s.push(action("sync", Some("node"), wrong));
    let incompatible = push(&mut s, inspect());
    let mut mixed = catalog("fixture");
    let mut tool = mixed["namespaces"][0]["tools"][0].clone();
    tool["name"] = "other/ReadCase".into();
    tool["nativeMethod"] = "other/ReadCase".into();
    tool["interfaceVersion"] = "2.0.0".into();
    mixed["namespaces"][0]["tools"]
        .as_array_mut()
        .unwrap()
        .push(tool);
    s.push(action("sync", Some("node"), mixed));
    let mixed = push(&mut s, inspect());
    s.push(action("sync", Some("node"), catalog("fixture")));
    let waiting = push(&mut s, inspect());
    s.push(heartbeat("node", true, vec![]));
    let contract_missing = push(&mut s, inspect());
    let ready_entry = push(&mut s, operation("entry", json!({"uiId":"sample-ui"})));
    s.push(contract());
    let ready = push(&mut s, inspect());
    let mut second_contract = contract();
    second_contract["definition"]["version"] = "2.0.0".into();
    s.push(second_contract);
    let mut seed = String::from("BEGIN IMMEDIATE;");
    super::super::query::insert(
        &mut seed,
        "required-object",
        "fixture/ui",
        "2.0.0",
        "NULL",
        false,
        "text",
        "retained",
        1,
    );
    seed.push_str("COMMIT;");
    s.push(sql(seed));
    let unreadable = push(&mut s, inspect());
    let mut attempted = deploy("r2", "y", json!("r1"), "contract-first");
    attempted["params"]["release"]["requirements"]["contracts"] =
        json!([{"key":"fixture/ui","readVersions":["1.0.0"],"writeVersions":["1.0.0"]}]);
    let deploy_conflict = push(&mut s, attempted);
    s.push(sql("UPDATE app_releases SET release_json=json_set(release_json,'$.requirements.contracts[0].readVersions',json('[\"1.0.0\",\"2.0.0\"]'));"));
    let readable = push(&mut s, inspect());
    s.push(sql("DROP TRIGGER contracts_immutable_update;UPDATE contracts SET definition='{' WHERE key='fixture/ui' AND version='1.0.0';"));
    let unexpected = push(&mut s, inspect());
    s.push(sql(format!(
        "UPDATE contracts SET definition={} WHERE key='fixture/ui' AND version='1.0.0';",
        quote(&contract()["definition"].to_string())
    )));
    let stale = push(&mut s, at(inspect(), 999999));
    let stale_node = push(&mut s, at(action("nodes", None, json!({})), 999999));
    s.push(json!({"op":"reopen"}));
    let reopened = push(&mut s, inspect());
    let r = compare(&s);
    success(&r, deployed);
    error(&r, entry, "service_unavailable");
    error(&r, no_entry, "service_interface_changed");
    success(&r, ready_entry);
    assert_eq!(value(&r, absent)["status"], "incompatible");
    assert_eq!(
        value(&r, no_catalog)["issues"][1]["code"],
        "service_not_ready"
    );
    assert_eq!(
        value(&r, incompatible)["issues"][1]["code"],
        "service_interface_changed"
    );
    assert_eq!(value(&r, waiting)["issues"][1]["code"], "service_not_ready");
    assert_eq!(
        value(&r, mixed)["issues"][1]["code"],
        "service_interface_changed"
    );
    assert_eq!(
        value(&r, contract_missing)["issues"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(value(&r, ready)["status"], "ready");
    assert_eq!(value(&r, unreadable)["status"], "incompatible");
    assert_eq!(
        value(&r, unreadable)["issues"][0]["code"],
        "contract_version_conflict"
    );
    error(&r, deploy_conflict, "contract_version_conflict");
    assert_eq!(value(&r, readable)["status"], "ready");
    error(&r, unexpected, "internal_error");
    assert_eq!(value(&r, stale)["status"], "ready");
    assert_eq!(value(&r, stale_node)[0]["stale"], true);
    assert_eq!(value(&r, reopened)["status"], "unavailable");
}

#[test]
fn uis_rollback_uses_availability_and_saved_receipts_before_later_state_and_rejects_missing_releases()
 {
    let mut s = setup();
    s.push(stage("r1", "a", "s1"));
    s.push(deploy("r1", "a", Value::Null, "d1"));
    s.push(stage("r2", "b", "s2"));
    s.push(deploy("r2", "b", json!("r1"), "d2"));
    s.push(fixture(vec![write("private/sample-ui/r1/index.html", "z")]));
    let req = rollback("r1", "r2", "rollback");
    let rolled = push(&mut s, req.clone());
    let check = push(&mut s, file("check", "r1", "a"));
    let read = push(&mut s, file("read", "r1", "a"));
    let missing = push(&mut s, rollback("unknown", "r1", "missing"));
    let stale = push(&mut s, rollback("r2", "wrong", "stale"));
    s.push(stage("r3", "c", "s3"));
    s.push(deploy("r3", "c", json!("r1"), "d3"));
    s.push(json!({"op":"reopen"}));
    let replay = push(&mut s, req);
    s.push(fixture(vec![
        json!({"kind":"remove","path":"private/sample-ui/r1/index.html"}),
    ]));
    let unavailable = push(&mut s, rollback("r1", "r3", "unavailable"));
    let r = compare(&s);
    success(&r, rolled);
    success(&r, replay);
    assert_eq!(value(&r, rolled), value(&r, replay));
    assert_eq!(value(&r, check), false);
    error(&r, read, "ui_release_invalid");
    error(&r, missing, "not_found");
    error(&r, stale, "release_conflict");
    error(&r, unavailable, "ui_release_invalid");
    assert_eq!(
        r["rows"][replay]["state"]["apps"][0]["current_release_id"],
        "r3"
    );
}

#[test]
fn uis_entry_current_assets_and_collection_preserve_unchecked_manifest_and_guard_boundaries() {
    let mut s = setup();
    let mut manifest = release("r1", "x");
    manifest["requirements"]["hiveProtocol"] = 2.into();
    s.push(seed_ui(
        "sample-ui",
        metadata("sample-ui"),
        json!("r1"),
        vec![("r1", manifest)],
    ));
    let entry = push(&mut s, operation("entry", json!({"uiId":"sample-ui"})));
    let current = push(
        &mut s,
        operation(
            "currentAsset",
            json!({"uiId":"sample-ui","path":"index.html"}),
        ),
    );
    let omitted = push(
        &mut s,
        operation("currentAsset", json!({"uiId":"sample-ui"})),
    );
    let path = push(
        &mut s,
        operation(
            "asset",
            json!({"uiId":"absent","releaseId":"none","path":"../escape"}),
        ),
    );
    let pointer = push(
        &mut s,
        operation("currentAsset", json!({"uiId":"absent","path":"../escape"})),
    );
    let mut manifest = release("r1", "x");
    manifest.as_object_mut().unwrap().remove("entryPath");
    s.push(sql(format!(
        "UPDATE app_releases SET release_json={};",
        quote(&manifest.to_string())
    )));
    let no_path = push(&mut s, operation("entry", json!({"uiId":"sample-ui"})));
    let empty_path = push(
        &mut s,
        operation("currentAsset", json!({"uiId":"sample-ui","path":""})),
    );
    s.push(fixture(vec![
        write("private/sample-ui/r1/index.html", "x"),
        write("private/sample-ui/old/index.html", "x"),
        write("private/orphan-ui/r1/index.html", "x"),
        mtime("private/sample-ui/r1", 0),
        mtime("private/sample-ui/old", 0),
        mtime("private/orphan-ui/r1", 0),
    ]));
    let collect = push(&mut s, at(operation("collectFiles", json!({})), 172800000));
    let r = compare(&s);
    error(&r, entry, "unsupported_protocol");
    success(&r, current);
    error(&r, omitted, "unsupported_protocol");
    error(&r, path, "invalid_arguments");
    error(&r, pointer, "not_found");
    assert_eq!(value(&r, no_path), &json!({"releaseId":"r1"}));
    error(&r, empty_path, "internal_error");
    success(&r, collect);
    assert!(has_file(&r, collect, "private/sample-ui/r1/index.html"));
    assert!(!has_file(&r, collect, "private/sample-ui/old"));
    assert!(!has_file(&r, collect, "private/orphan-ui"));
}
