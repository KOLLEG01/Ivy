use super::domains::{follow, ok, value};
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};

fn wiki_setup() -> Vec<Value> {
    let mut s = setup();
    for version in ["1.0.0", "2.0.0"] {
        s.push(json!({"op":"register","owner":{"kind":"agent"},"definition":{"key":"wiki/page","version":version,"owner":{"kind":"agent"},"mediaType":"text/markdown","retention":{"objects":{"mode":"retain"},"revisions":{"mode":"all"}},"specMarkdown":"Synthetic Wiki page fixture"}}));
    }
    s
}
fn wiki_create(title: &str, nonce: &str) -> Value {
    request(
        "wiki.create",
        json!({"title":title,"markdown":"Synthetic page α 😀","mutationId":format!("epoch:100000:{nonce}")}),
    )
}
fn object_bound(mut step: Value, from: usize) -> Value {
    bind(
        &mut step,
        "/request/params/objectId",
        from,
        "/value/value/object/id",
    );
    step
}
#[test]
fn kernel_wiki_navigation_content_references_receipts_and_reopen_use_canonical_object_execution() {
    let mut s = wiki_setup();
    s[0]["options"] = json!({"validateMessages":true});
    let parent = ok(&mut s, wiki_create("Parent", "parent"));
    let other = ok(&mut s, wiki_create("Other", "other"));
    let mut child = wiki_create("ChildAlpha", "child");
    child["request"]["params"]["parentId"] = "bound".into();
    bind(
        &mut child,
        "/request/params/parentId",
        parent,
        "/value/value/object/id",
    );
    let child = ok(&mut s, child);
    ok(
        &mut s,
        object_bound(
            request(
                "objects.write",
                json!({"objectId":"bound","expectedRevision":1,"contractVersion":"1.0.0","references":{"anchor":{"objectId":"json","revision":1}},"content":{"encoding":"text","value":"References retained"},"mutationId":"epoch:100000:references"}),
            ),
            child,
        ),
    );
    let update = object_bound(
        request(
            "wiki.update",
            json!({"objectId":"bound","expectedRevision":2,"markdown":"Changed page α 😀","mutationId":"epoch:100000:wiki-update"}),
        ),
        child,
    );
    let updated = ok(&mut s, update.clone());
    let replay = ok(&mut s, update);
    let read = ok(
        &mut s,
        object_bound(request("wiki.read", json!({"objectId":"bound"})), child),
    );
    let mut children = request("wiki.list", json!({"parentId":"bound","limit":1}));
    bind(
        &mut children,
        "/request/params/parentId",
        parent,
        "/value/value/object/id",
    );
    let children = ok(&mut s, children);
    let roots = ok(
        &mut s,
        request("wiki.list", json!({"parentId":null,"limit":1})),
    );
    ok(
        &mut s,
        follow(
            request("wiki.list", json!({"parentId":null,"limit":1})),
            roots,
        ),
    );
    let mut search = request(
        "wiki.search",
        json!({"rootId":"bound","text":"Changed α","limit":10}),
    );
    bind(
        &mut search,
        "/request/params/rootId",
        parent,
        "/value/value/object/id",
    );
    let searched = ok(&mut s, search);
    let history = ok(
        &mut s,
        object_bound(
            request("wiki.history", json!({"objectId":"bound","limit":1})),
            child,
        ),
    );
    ok(
        &mut s,
        follow(
            object_bound(
                request("wiki.history", json!({"objectId":"bound","limit":1})),
                child,
            ),
            history,
        ),
    );
    let mut movement = object_bound(
        request(
            "wiki.move",
            json!({"objectId":"bound","parentId":"bound","name":"Moved","mutationId":"epoch:100000:move"}),
        ),
        child,
    );
    bind(
        &mut movement,
        "/request/params/parentId",
        other,
        "/value/value/object/id",
    );
    ok(&mut s, movement);
    ok(
        &mut s,
        object_bound(
            request(
                "wiki.archive",
                json!({"objectId":"bound","archived":true,"mutationId":"epoch:100000:archive"}),
            ),
            child,
        ),
    );
    s.push(op("reopen"));
    let reopened = ok(&mut s, request("wiki.read", json!({"path":"/Other/Moved"})));
    let result = compare(&s);
    assert_eq!(value(&result, updated), value(&result, replay));
    assert_eq!(
        value(&result, read)["revision"]["references"]["anchor"]["objectId"],
        "json"
    );
    assert_eq!(value(&result, read)["revision"]["revision"], 3);
    assert_eq!(
        value(&result, children)["items"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        value(&result, searched)["items"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        value(&result, reopened)["content"]["value"],
        "Changed page α 😀"
    );
    assert_eq!(
        value(&result, reopened)["object"]["effectivelyArchived"],
        true
    );
}

#[test]
fn kernel_wiki_nested_output_guards_sql_rollback_version_mcp_and_reset_keep_original_order() {
    let mut s = wiki_setup();
    let family = push(&mut s, request("wiki.read", json!({"path":"/json"})));
    let mut strict = request("wiki.read", json!({"path":"/json"}));
    strict["options"] = json!({"validateMessages":true});
    let completed = push(&mut s, strict);
    let mut clear = request("system.status", json!({}));
    clear["options"] = json!({"validateMessages":false});
    ok(&mut s, clear);
    let invalid_parent = push(
        &mut s,
        request(
            "wiki.create",
            json!({"title":"Invalid","parentId":"json","markdown":"x","mutationId":"epoch:100000:invalid-parent"}),
        ),
    );
    let page = ok(&mut s, wiki_create("Page", "page"));
    let update = object_bound(
        request(
            "wiki.update",
            json!({"objectId":"bound","expectedRevision":1,"markdown":"Changed","mutationId":"epoch:100000:update"}),
        ),
        page,
    );
    s.push(sql("CREATE TRIGGER kernel_wiki_failure BEFORE INSERT ON revisions WHEN NEW.revision=2 BEGIN SELECT RAISE(ABORT,'fixture failure'); END;"));
    let failed = push(&mut s, update.clone());
    s.push(sql("DROP TRIGGER kernel_wiki_failure;"));
    ok(&mut s, update);
    ok(
        &mut s,
        object_bound(
            request(
                "objects.write",
                json!({"objectId":"bound","expectedRevision":2,"contractVersion":"2.0.0","references":{},"content":{"encoding":"text","value":"Upgraded"},"mutationId":"epoch:100000:upgrade"}),
            ),
            page,
        ),
    );
    let version = push(
        &mut s,
        object_bound(
            request(
                "wiki.update",
                json!({"objectId":"bound","expectedRevision":3,"markdown":"Unsupported","mutationId":"epoch:100000:version"}),
            ),
            page,
        ),
    );
    let mut mcp = object_bound(request("wiki.read", json!({"objectId":"bound"})), page);
    mcp["context"] = context("mcp");
    ok(&mut s, mcp.clone());
    let mut held = mcp;
    held["context"] = context("http");
    held["options"] = json!({"resetBootstrapCredentialDigest":format!("sha256:{}","a".repeat(64))});
    let held = push(&mut s, held);
    let mut clear = request("system.status", json!({}));
    clear["options"] = json!({"resetBootstrapCredentialDigest":""});
    ok(&mut s, clear);
    let invalid = push(
        &mut s,
        request(
            "wiki.create",
            json!({"title":"Incomplete","mutationId":"epoch:100000:incomplete"}),
        ),
    );
    let result = compare(&s);
    for index in [family, invalid_parent, invalid] {
        error(&result, index, "invalid_arguments");
    }
    for index in [completed, failed] {
        error(&result, index, "internal_error");
    }
    assert_eq!(
        result["rows"][completed]["error"]["data"]["outcome"],
        "completed"
    );
    assert_eq!(
        result["rows"][failed]["state"]["revisions"],
        result["rows"][failed - 1]["state"]["revisions"]
    );
    error(&result, version, "contract_version_conflict");
    error(&result, held, "maintenance_active");
}

#[test]
fn kernel_contract_summaries_preserve_ecmascript_whitespace_utf16_slice_latest_and_cursor_semantics()
 {
    let mut s = setup();
    for (version, spec) in [
        (
            "1.0.0",
            "\u{feff} first\u{a0}\t second\u{85}third \r\n".to_owned(),
        ),
        ("2.0.0", "\u{feff}\t\r\n\u{2009}".to_owned()),
        ("3.0.0", format!("{}😀 tail", "x".repeat(511))),
    ] {
        ok(
            &mut s,
            request(
                "contracts.register",
                json!({"mutationId":format!("epoch:100000:summary-{version}"),"definition":{"key":"fixture/summary","version":version,"owner":{"kind":"agent"},"mediaType":"text/plain","retention":{"objects":{"mode":"retain"},"revisions":{"mode":"all"}},"specMarkdown":spec}}),
            ),
        );
    }
    let first = ok(
        &mut s,
        request(
            "contracts.summaries",
            json!({"key":"fixture/summary","allVersions":true,"limit":1}),
        ),
    );
    let second = ok(
        &mut s,
        follow(
            request(
                "contracts.summaries",
                json!({"key":"fixture/summary","allVersions":true,"limit":1}),
            ),
            first,
        ),
    );
    let changed = push(
        &mut s,
        follow(
            request(
                "contracts.summaries",
                json!({"key":"fixture/summary","allVersions":false,"limit":1}),
            ),
            first,
        ),
    );
    let schema = ok(
        &mut s,
        request("contracts.summaries", json!({"key":"fixture/json"})),
    );
    let mut latest = request("contracts.summaries", json!({"key":"fixture/summary"}));
    latest["lossless"] = true.into();
    let latest = ok(&mut s, latest);
    s.push(sql(
        "DROP TRIGGER contracts_immutable_update; UPDATE contracts SET definition='[' WHERE key='fixture/summary' AND version='1.0.0';",
    ));
    let corrupt = push(
        &mut s,
        request(
            "contracts.summaries",
            json!({"key":"fixture/summary","cursor":"invalid"}),
        ),
    );
    // Catalog cursors are now admitted before reading any selected bodies.
    // Retain the original trace; only this corrupt-storage error order changes.
    let result = compare_with_error_codes(&s, &[(corrupt, "invalid_cursor")]);
    assert_eq!(
        value(&result, first)["items"][0]["description"],
        "first second\u{85}third"
    );
    assert_eq!(value(&result, first)["items"][0]["hasJsonSchema"], false);
    assert_eq!(
        value(&result, second)["items"][0]["description"],
        "fixture/summary"
    );
    assert_eq!(value(&result, schema)["items"][0]["version"], "10.0.0");
    assert_eq!(value(&result, schema)["items"][0]["hasJsonSchema"], true);
    let encoded = result["rows"][latest]["value"]["encoded"].as_str().unwrap();
    assert!(encoded.contains("\\ud83d"));
    assert!(encoded.contains("3.0.0"));
    assert!(!encoded.contains("tail"));
    error(&result, changed, "invalid_cursor");
    error(&result, corrupt, "invalid_cursor");
}

fn asset(bytes: &str) -> Value {
    json!({"path":"index.html","mediaType":"text/html","contentHash":ivy_contracts::hash::digest(bytes.as_bytes()),"byteLength":bytes.len()})
}
fn metadata(ui: &str) -> Value {
    json!({"uiId":ui,"slug":ui,"displayName":ui,"description":"Synthetic UI.","iconKey":"ui"})
}
fn release(id: &str, bytes: &str) -> Value {
    json!({"releaseId":id,"entryPath":"index.html","requirements":{"hiveProtocol":1,"contracts":[],"services":[]},"assets":[asset(bytes)]})
}
pub(super) fn stage(ui: &str, id: &str, bytes: &str, nonce: &str) -> Value {
    request(
        "uis.stageAsset",
        json!({"uiId":ui,"releaseId":id,"asset":asset(bytes),"base64":STANDARD.encode(bytes),"mutationId":format!("epoch:100000:{nonce}")}),
    )
}
pub(super) fn deploy(ui: &str, id: &str, bytes: &str, expected: Value, nonce: &str) -> Value {
    request(
        "uis.deploy",
        json!({"metadata":metadata(ui),"release":release(id,bytes),"expectedReleaseId":expected,"mutationId":format!("epoch:100000:{nonce}")}),
    )
}
fn quote(value: &Value) -> String {
    format!("'{}'", value.to_string().replace('\'', "''"))
}
fn retain(steps: &mut Vec<Value>, ui: &str, id: &str, bytes: &str) {
    steps.push(sql(format!(
        "INSERT INTO app_releases VALUES('{}','{}',{});",
        ui,
        id,
        quote(&release(id, bytes)),
    )));
    let mut read = request("uis.get", json!({"uiId":ui}));
    read["files"] = json!([{"kind":"write","path":format!("private/{ui}/{id}/index.html"),"data":STANDARD.encode(bytes)}]);
    ok(steps, read);
}
#[test]
fn kernel_ui_publication_catalogs_inspection_rollback_receipts_and_files_survive_reopen() {
    let mut s = setup();
    s[0]["options"] = json!({"validateMessages":true});
    let first_deploy = deploy("sample-ui", "r1", "<h1>one</h1>", Value::Null, "deploy-r1");
    let first_stage = stage("sample-ui", "r1", "<h1>one</h1>", "stage-r1");
    ok(&mut s, first_stage.clone());
    ok(&mut s, first_stage);
    let first = ok(&mut s, first_deploy.clone());
    ok(&mut s, stage("sample-ui", "r2", "<h1>two</h1>", "stage-r2"));
    ok(
        &mut s,
        deploy("sample-ui", "r2", "<h1>two</h1>", json!("r1"), "deploy-r2"),
    );
    ok(
        &mut s,
        stage("sample-ui", "r3", "<h1>three</h1>", "stage-r3"),
    );
    ok(
        &mut s,
        deploy(
            "sample-ui",
            "r3",
            "<h1>three</h1>",
            json!("r2"),
            "deploy-r3",
        ),
    );
    let retired = push(
        &mut s,
        request(
            "uis.rollback",
            json!({"uiId":"sample-ui","releaseId":"r2","expectedReleaseId":"r3","mutationId":"epoch:100000:retired"}),
        ),
    );
    // Current publication retires old releases; legacy retained releases remain selectable.
    retain(&mut s, "sample-ui", "r2", "<h1>two</h1>");
    let rollback = ok(
        &mut s,
        request(
            "uis.rollback",
            json!({"uiId":"sample-ui","releaseId":"r2","expectedReleaseId":"r3","mutationId":"epoch:100000:rollback"}),
        ),
    );
    retain(&mut s, "sample-ui", "r3", "<h1>three</h1>");
    ok(
        &mut s,
        stage("second-ui", "r1", "<h1>second</h1>", "stage-second"),
    );
    let mut second = deploy(
        "second-ui",
        "r1",
        "<h1>second</h1>",
        Value::Null,
        "deploy-second",
    );
    second["request"]["params"]["metadata"]["priority"] = 10.into();
    ok(&mut s, second);
    let listed = ok(&mut s, request("uis.list", json!({"limit":1})));
    ok(
        &mut s,
        follow(request("uis.list", json!({"limit":1})), listed),
    );
    let catalog = ok(&mut s, request("uis.catalog", json!({"limit":1})));
    ok(
        &mut s,
        follow(request("uis.catalog", json!({"limit":1})), catalog),
    );
    let releases = ok(
        &mut s,
        request("uis.releases", json!({"uiId":"sample-ui","limit":1})),
    );
    ok(
        &mut s,
        follow(
            request("uis.releases", json!({"uiId":"sample-ui","limit":1})),
            releases,
        ),
    );
    let inspected = ok(&mut s, request("uis.inspect", json!({"uiId":"sample-ui"})));
    s.push(op("reopen"));
    let get = ok(&mut s, request("uis.get", json!({"uiId":"sample-ui"})));
    let replay = ok(&mut s, first_deploy);
    let result = compare(&s);
    error(&result, retired, "not_found");
    assert_eq!(value(&result, first), value(&result, replay));
    assert_eq!(value(&result, rollback)["releaseId"], "r2");
    assert_eq!(
        value(&result, listed)["items"][0]["metadata"]["uiId"],
        "sample-ui"
    );
    assert_eq!(
        value(&result, catalog)["items"][0]["metadata"]["uiId"],
        "second-ui"
    );
    assert_eq!(value(&result, inspected)["status"], "ready");
    assert_eq!(value(&result, get)["currentReleaseId"], "r2");
    assert_eq!(value(&result, get)["releases"].as_array().unwrap().len(), 2);
    assert!(
        result["rows"][get]["fileTree"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["path"] == "private/sample-ui/r2/index.html")
    );
}

#[test]
fn kernel_ui_sql_failure_preserves_promoted_files_retry_replay_and_reset_bootstrap_guards() {
    let mut s = setup();
    let staged = stage("sample-ui", "r1", "x", "stage");
    let mut invalid = staged.clone();
    invalid["request"]["params"]["base64"] = "eA".into();
    let bad_base64 = push(&mut s, invalid);
    ok(&mut s, staged.clone());
    let conflict = push(
        &mut s,
        deploy("sample-ui", "r1", "x", json!("missing"), "pointer"),
    );
    s.push(sql("CREATE TRIGGER kernel_ui_failure BEFORE INSERT ON app_releases BEGIN SELECT RAISE(ABORT,'fixture failure'); END;"));
    let deployment = deploy("sample-ui", "r1", "x", Value::Null, "deploy");
    let failed = push(&mut s, deployment.clone());
    s.push(sql("DROP TRIGGER kernel_ui_failure;"));
    ok(&mut s, deployment);
    let mut replay = staged.clone();
    replay["files"] = json!([{"kind":"remove","path":"private/sample-ui/r1"}]);
    let replay = ok(&mut s, replay);
    let mut conflicting = staged;
    conflicting["request"]["params"]["base64"] = "eQ==".into();
    let mutation = push(&mut s, conflicting);
    let mut held = request("uis.inspect", json!({"uiId":"sample-ui"}));
    held["options"] = json!({"resetBootstrapCredentialDigest":format!("sha256:{}","a".repeat(64))});
    let invalid_files = ok(&mut s, held);
    let blocked_get = push(&mut s, request("uis.get", json!({"uiId":"sample-ui"})));
    ok(&mut s, stage("sample-ui", "r2", "y", "stage-held"));
    ok(
        &mut s,
        deploy("sample-ui", "r2", "y", json!("r1"), "deploy-held"),
    );
    let blocked_rollback = push(
        &mut s,
        request(
            "uis.rollback",
            json!({"uiId":"sample-ui","releaseId":"r1","expectedReleaseId":"r2","mutationId":"epoch:100000:rollback-held"}),
        ),
    );
    let mut clear = request("system.status", json!({}));
    clear["options"] = json!({"resetBootstrapCredentialDigest":""});
    ok(&mut s, clear);
    let ready = ok(&mut s, request("uis.inspect", json!({"uiId":"sample-ui"})));
    let result = compare(&s);
    error(&result, bad_base64, "invalid_arguments");
    error(&result, conflict, "release_conflict");
    error(&result, failed, "internal_error");
    assert!(
        result["rows"][failed]["state"]["apps"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        result["rows"][failed]["fileTree"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["path"] == "private/sample-ui/r1/index.html")
    );
    assert!(
        !result["rows"][replay]["fileTree"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["path"] == "private/sample-ui/r1/index.html")
    );
    error(&result, mutation, "mutation_conflict");
    for index in [blocked_get, blocked_rollback] {
        error(&result, index, "maintenance_active");
    }
    assert_eq!(value(&result, invalid_files)["status"], "invalid");
    assert_eq!(value(&result, ready)["status"], "ready");
}

#[test]
fn kernel_ui_requirements_share_live_registry_and_retained_metadata_diagnostics() {
    let mut s = setup();
    ok(&mut s, stage("sample-ui", "r1", "<h1>UI</h1>", "stage"));
    let mut deployment = deploy("sample-ui", "r1", "<h1>UI</h1>", Value::Null, "deploy");
    deployment["request"]["params"]["release"]["requirements"]["services"] =
        json!([{"serviceName":"fixture","namespace":"fixture","interfaceVersion":"1.0.0"}]);
    ok(&mut s, deployment);
    let absent = ok(&mut s, request("uis.inspect", json!({"uiId":"sample-ui"})));
    domains::ready(&mut s, "node", "host", "fixture");
    let ready = ok(&mut s, request("uis.inspect", json!({"uiId":"sample-ui"})));
    ok(
        &mut s,
        service_request(
            "service.heartbeat",
            "node",
            json!({"ready":false,"diagnostics":[]}),
        ),
    );
    let unready = ok(&mut s, request("uis.inspect", json!({"uiId":"sample-ui"})));
    let mut changed = domains::catalog();
    for tool in changed["namespaces"][0]["tools"].as_array_mut().unwrap() {
        tool["interfaceVersion"] = "2.0.0".into();
    }
    ok(&mut s, service_request("registry.sync", "node", changed));
    ok(
        &mut s,
        service_request(
            "service.heartbeat",
            "node",
            json!({"ready":true,"diagnostics":[]}),
        ),
    );
    let incompatible = ok(&mut s, request("uis.inspect", json!({"uiId":"sample-ui"})));
    ok(
        &mut s,
        service_request("registry.sync", "node", domains::catalog()),
    );
    ok(
        &mut s,
        service_request(
            "service.heartbeat",
            "node",
            json!({"ready":true,"diagnostics":[]}),
        ),
    );
    let recovered = ok(&mut s, request("uis.inspect", json!({"uiId":"sample-ui"})));
    s.push(sql(
        "UPDATE apps SET metadata='[' WHERE app_id='sample-ui';",
    ));
    let list = ok(&mut s, request("uis.list", json!({})));
    let catalog = ok(&mut s, request("uis.catalog", json!({})));
    let get = push(&mut s, request("uis.get", json!({"uiId":"sample-ui"})));
    let cursor = push(&mut s, request("uis.catalog", json!({"cursor":"invalid"})));
    s.push(sql(format!(
        "UPDATE apps SET metadata={} WHERE app_id='sample-ui';",
        quote(&metadata("sample-ui"))
    )));
    let mut missing = request("uis.inspect", json!({"uiId":"sample-ui"}));
    missing["files"] = json!([{"kind":"write","path":"private/sample-ui/r1/index.html","data":STANDARD.encode("bad")}]);
    let missing = ok(&mut s, missing);
    let result = compare(&s);
    for index in [absent, unready] {
        assert_eq!(value(&result, index)["status"], "unavailable");
    }
    assert_eq!(value(&result, missing)["status"], "invalid");
    for index in [ready, recovered] {
        assert_eq!(value(&result, index)["status"], "ready");
    }
    assert_eq!(value(&result, incompatible)["status"], "incompatible");
    assert!(value(&result, list)["items"].as_array().unwrap().is_empty());
    assert!(
        value(&result, catalog)["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    error(&result, get, "not_found");
    error(&result, cursor, "invalid_cursor");
}
