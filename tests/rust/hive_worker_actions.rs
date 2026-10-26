use super::*;

fn credential(value: &str) -> String {
    format!("sha256:{}", value.repeat(64))
}
fn setup() -> Vec<Value> {
    let mut constructor = json!({"op":"worker","method":"constructor","now":100000,"credentials":[
        {"principalId":"alpha","digest":credential("a")}, {"principalId":"beta","digest":credential("b")}
    ]});
    constructor["requireSuccess"] = true.into();
    let mut steps = vec![constructor];
    steps.extend(object_write::setup());
    steps
}
fn action(payload: Value) -> Value {
    json!({"op":"worker","method":"dispatch","now":100000,"payload":payload})
}
fn execute(step: Value) -> Value {
    let mut result =
        action(json!({"action":"execute","context":step["context"],"request":step["request"]}));
    for key in ["requireSuccess", "options", "files", "identityBindings"] {
        if let Some(value) = step.get(key) {
            result[key] = value.clone();
        }
    }
    result
}
fn ok(steps: &mut Vec<Value>, mut step: Value) -> usize {
    step["requireSuccess"] = true.into();
    push(steps, step)
}
fn compare(steps: &[Value]) -> Value {
    super::compare(steps)
}
fn notifications(result: &Value, index: usize) -> &[Value] {
    result["rows"][index]["notifications"].as_array().unwrap()
}

#[test]
fn worker_retention_keeps_cleanup_order_partial_commits_and_failure_journal() {
    let mut s = setup();
    let collect = || {
        let mut step = action(json!({"action":"retention.collect"}));
        step["now"] = 1_000_000_000.into();
        step
    };
    let old_file = |release: &str| {
        json!([
            {"kind":"write","path":format!("private/fixture/{release}/entry"),"data":"eA=="},
            {"kind":"mtime","path":format!("private/fixture/{release}"),"ms":0}
        ])
    };
    ok(
        &mut s,
        action(json!({"action":"diagnostic","value":{
        "code":"fixture","resource":{"serviceNodeId":"fixture"},"source":"hive.fixture","severity":"warning","status":"resolved",
            "firstObservedAt":"1970-01-01T00:00:00.000Z","lastObservedAt":"1970-01-01T00:00:00.000Z",
            "message":"Old resolved diagnostic."
        }})),
    );
    s.push(sql("INSERT INTO apps VALUES('fixture','{}','keep',NULL); INSERT INTO app_releases VALUES('fixture','keep','{}'),('fixture','old','{}'); INSERT INTO metadata VALUES('oauth_refresh:expired','{\"expiresAt\":1}'),('oauth_refresh:revoked','{\"expiresAt\":2000000000,\"revoked\":true}'),('oauth_refresh:live','{\"expiresAt\":2000000000,\"credentialReference\":\"unconfigured\"}');"));
    let mut step = collect();
    step["files"] = old_file("old");
    let success = ok(&mut s, step);

    let message = format!("{}🙂tail", "x".repeat(511));
    s.push(sql(format!("INSERT INTO app_releases VALUES('fixture','pending','{{}}'); INSERT INTO metadata VALUES('oauth_refresh:pending','{{\"expiresAt\":1}}'); CREATE TRIGGER retention_failure BEFORE UPDATE ON metadata WHEN NEW.key='retention_last_success' BEGIN SELECT RAISE(ABORT,'{message}'); END;")));
    let mut step = collect();
    step["files"] = old_file("pending");
    let collection_failed = push(&mut s, step);
    s.push(sql("DROP TRIGGER retention_failure;"));
    let mut step = collect();
    step["files"] = json!([{"kind":"mkdir","path":"private/!invalid"}]);
    let ui_failed = push(&mut s, step);

    s.push(sql(
        "INSERT INTO metadata VALUES('oauth_refresh:malformed','[');",
    ));
    let mut step = collect();
    let mut files = old_file("before-oauth").as_array().unwrap().clone();
    files.insert(0, json!({"kind":"remove","path":"private/!invalid"}));
    step["files"] = files.into();
    let oauth_failed = push(&mut s, step);
    s.push(sql("CREATE TRIGGER journal_failure BEFORE UPDATE ON metadata WHEN NEW.key='retention_last_failure' AND NEW.value<>'' BEGIN SELECT RAISE(ABORT,'failure journal unavailable'); END;"));
    let journal_failed = push(&mut s, collect());
    s.push(sql(
        "DROP TRIGGER journal_failure; DELETE FROM metadata WHERE key='oauth_refresh:malformed';",
    ));
    let recovered = ok(&mut s, collect());

    let actual = compare(&s);
    let metadata = |index: usize, key: &str| {
        actual["rows"][index]["state"]["metadata"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["key"] == key)
            .map(|row| row["value"].clone())
    };
    let has_file = |index: usize, release: &str| {
        actual["rows"][index]["fileTree"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["path"] == format!("private/fixture/{release}/entry"))
    };
    let has_release = |index: usize, release: &str| {
        actual["rows"][index]["state"]["releases"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["release_id"] == release)
    };
    assert_eq!(metadata(success, "oauth_refresh:expired"), None);
    assert_eq!(metadata(success, "oauth_refresh:revoked"), None);
    assert!(metadata(success, "oauth_refresh:live").is_some());
    assert!(!has_release(success, "old"));
    assert!(!has_file(success, "old"));
    assert_eq!(actual["rows"][success]["state"]["diagnostics"], json!([]));
    assert!(notifications(&actual, success).iter().any(|notice| {
        notice["changes"]
            .as_array()
            .is_some_and(|changes| changes.contains(&json!("system/retention")))
    }));
    error(&actual, collection_failed, "internal_error");
    assert!(has_release(collection_failed, "pending"));
    assert!(has_file(collection_failed, "pending"));
    let journal = |index| {
        serde_json::from_str::<Value>(
            metadata(index, "retention_last_failure")
                .unwrap()
                .as_str()
                .unwrap(),
        )
        .unwrap()
    };
    let recorded = metadata(collection_failed, "retention_last_failure").unwrap();
    let truncated = JsString(message.encode_utf16().take(512).collect());
    assert_eq!(
        recorded.as_str().unwrap(),
        ivy_contracts::json::canonical(
            &IvyJson::object([
                ("at", "1970-01-12T13:46:40.000Z".into()),
                ("message", IvyJson::String(truncated))
            ]),
            usize::MAX
        )
        .unwrap()
    );
    error(&actual, ui_failed, "invalid_arguments");
    assert!(!has_release(ui_failed, "pending"));
    assert!(has_file(ui_failed, "pending"));
    assert_eq!(journal(ui_failed)["message"], "UI id is invalid.");
    error(&actual, oauth_failed, "storage_invalid");
    assert!(!has_file(oauth_failed, "pending"));
    assert!(!has_file(oauth_failed, "before-oauth"));
    for index in [collection_failed, ui_failed, oauth_failed, journal_failed] {
        assert!(metadata(index, "oauth_refresh:pending").is_some());
    }
    assert_eq!(
        journal(oauth_failed)["message"],
        "Retained OAuth family state is invalid."
    );
    error(&actual, journal_failed, "internal_error");
    assert_eq!(
        metadata(journal_failed, "retention_last_failure"),
        Some(json!(""))
    );
    assert_eq!(metadata(recovered, "oauth_refresh:pending"), None);
    assert_eq!(
        metadata(recovered, "retention_last_failure"),
        Some(json!(""))
    );
}

#[test]
fn worker_actions_publish_live_routes_changes_and_events_after_success_and_failure() {
    let mut s = setup();
    ok(&mut s, action(json!({"action":"health"})));
    let connected = ok(&mut s, execute(connect("node", Some(1.0))));
    let synced = ok(
        &mut s,
        execute(service_request("registry.sync", "node", domains::catalog())),
    );
    ok(
        &mut s,
        execute(service_request(
            "service.heartbeat",
            "node",
            json!({"ready":true,"diagnostics":[]}),
        )),
    );
    let catalog = ok(&mut s, action(json!({"action":"mcp.catalog"})));
    let failed = push(
        &mut s,
        execute(service_request("registry.sync", "node", json!({}))),
    );
    let silent = ok(&mut s, action(json!({"action":"health"})));
    s.push(sql("CREATE TRIGGER worker_failure BEFORE INSERT ON service_connections BEGIN SELECT RAISE(ABORT,'fixture'); END;"));
    let rollback = push(&mut s, execute(connect("rolled-back", Some(1.0))));
    s.push(sql("DROP TRIGGER worker_failure;"));
    let mut write = request(
        "objects.write",
        json!({"objectId":"text","expectedRevision":1,"contractVersion":"1.0.0","references":{},"content":{"encoding":"text","value":"worker"},"mutationId":"epoch:100000:worker"}),
    );
    let changed = ok(&mut s, execute(write.clone()));
    let replayed = ok(&mut s, execute(write.clone()));
    write["request"]["params"]["content"]["value"] = "conflict".into();
    let conflict = push(&mut s, execute(write));
    let disconnected = ok(
        &mut s,
        action(json!({"action":"disconnect","serviceNodeId":"node"})),
    );
    let result = compare(&s);
    assert_eq!(
        notifications(&result, connected)[0]["routing"]["generation"],
        1
    );
    assert!(notifications(&result, synced)[0]["routing"]["catalog"].is_object());
    assert!(result["rows"][catalog]["value"].as_array().unwrap().len() > 10);
    error(&result, failed, "invalid_arguments");
    assert_eq!(
        notifications(&result, failed)[0]["routing"]["node"]["synced"],
        false
    );
    assert!(
        notifications(&result, failed)[0]["routing"]
            .get("catalog")
            .is_none()
    );
    assert!(notifications(&result, silent).is_empty());
    error(&result, rollback, "internal_error");
    assert!(notifications(&result, rollback).is_empty());
    let notices = notifications(&result, changed);
    assert!(notices[0].get("changes").is_some());
    assert_eq!(notices[1]["eventsAvailable"]["throughSequence"], 1);
    assert!(notifications(&result, replayed).is_empty());
    error(&result, conflict, "mutation_conflict");
    assert!(notifications(&result, conflict).is_empty());
    assert_eq!(
        notifications(&result, disconnected)[0]["routing"]["node"]["connected"],
        false
    );
}

#[test]
fn worker_actions_keep_identity_cursor_guard_order_and_post_commit_cleanup_failure() {
    let mut s = setup();
    ok(&mut s, action(json!({"action":"oauth.initialize"})));
    let session = ok(
        &mut s,
        action(json!({"action":"session.create","credentialDigest":credential("a")})),
    );
    let mut authenticated = action(
        json!({"action":"authenticate","context":{"credentialDigest":credential("a"),"sessionDigest":"bound"}}),
    );
    bind(
        &mut authenticated,
        "/payload/context/sessionDigest",
        session,
        "/value/sessionToken",
    );
    authenticated["identityBindings"][0]["digest"] = true.into();
    ok(&mut s, authenticated);
    let mut resolve = action(json!({"action":"session.resolve","sessionDigest":"bound"}));
    bind(
        &mut resolve,
        "/payload/sessionDigest",
        session,
        "/value/sessionToken",
    );
    resolve["identityBindings"][0]["digest"] = true.into();
    ok(&mut s, resolve.clone());
    let mut end = action(
        json!({"action":"session.end","context":{"principalId":"alpha","credentialDigest":credential("a"),"sessionDigest":"bound"}}),
    );
    bind(
        &mut end,
        "/payload/context/sessionDigest",
        session,
        "/value/sessionToken",
    );
    end["identityBindings"][0]["digest"] = true.into();
    ok(&mut s, end);
    let ended = push(&mut s, resolve);
    let mut expired = action(json!({"action":"execute","context":context("http"),"request":null}));
    expired["payload"]["context"]["expiresAt"] = 100000.into();
    let expired = push(&mut s, expired);
    let invalid = push(
        &mut s,
        action(json!({"action":"execute","context":context("http"),"request":null})),
    );
    let cursor = ok(
        &mut s,
        action(json!({"action":"mcp.cursor.create","identity":{"surface":"ivy"},"offset":3})),
    );
    let mut read =
        action(json!({"action":"mcp.cursor.read","identity":{"surface":"ivy"},"cursor":"bound"}));
    bind(&mut read, "/payload/cursor", cursor, "/value/cursor");
    let offset = ok(&mut s, read.clone());
    read["payload"]["identity"]["surface"] = "ivy_dev".into();
    let boundary = push(&mut s, read);
    let negative_cursor = ok(
        &mut s,
        action(json!({"action":"mcp.cursor.create","identity":{},"offset":-1})),
    );
    let mut negative = action(json!({"action":"mcp.cursor.read","identity":{},"cursor":"bound"}));
    bind(
        &mut negative,
        "/payload/cursor",
        negative_cursor,
        "/value/cursor",
    );
    let negative = push(&mut s, negative);
    ok(
        &mut s,
        action(
            json!({"action":"oauth.family.create","activeCredentialReferences":["ref"],"state":{
                "familyDigest":"worker-family","credentialReference":"ref","clientId":"client","resource":"https://example.invalid/mcp","scope":"hive","generation":1,"currentTokenDigest":"token-1","expiresAt":200000,"revoked":false
            }}),
        ),
    );
    let rotation = json!({"action":"oauth.family.rotate","rotation":{
        "familyDigest":"worker-family","credentialReference":"ref","clientId":"client","resource":"https://example.invalid/mcp","scope":"hive","presentedGeneration":1,"presentedTokenDigest":"token-1","nextGeneration":2,"nextTokenDigest":"token-2","nextExpiresAt":300000
    }});
    ok(&mut s, action(rotation.clone()));
    let reused = push(&mut s, action(rotation));
    s.push(sql(
        "INSERT INTO metadata VALUES('oauth_refresh:malformed','[');",
    ));
    let cleanup = push(
        &mut s,
        action(
            json!({"action":"credentials.configure","credentials":[{"principalId":"beta","digest":credential("b")}]}),
        ),
    );
    let revoked = push(
        &mut s,
        action(json!({"action":"authenticate","context":{"credentialDigest":credential("a")}})),
    );
    ok(
        &mut s,
        action(json!({"action":"authenticate","context":{"credentialDigest":credential("b")}})),
    );
    let result = compare(&s);
    error(&result, ended, "unauthenticated");
    error(&result, expired, "unauthenticated");
    error(&result, invalid, "invalid_frame");
    error(&result, boundary, "invalid_cursor");
    error(&result, negative, "invalid_cursor");
    error(&result, reused, "unauthenticated");
    error(&result, cleanup, "storage_invalid");
    error(&result, revoked, "unauthenticated");
    assert_eq!(result["rows"][offset]["value"], json!({"offset":3}));
}

#[test]
fn worker_ui_actions_read_real_publications_and_preserve_optional_pointer_and_asset_guards() {
    let mut s = setup();
    ok(
        &mut s,
        execute(content::stage(
            "sample-ui",
            "r1",
            "<h1>Worker</h1>",
            "worker-stage",
        )),
    );
    ok(
        &mut s,
        execute(content::deploy(
            "sample-ui",
            "r1",
            "<h1>Worker</h1>",
            Value::Null,
            "worker-deploy",
        )),
    );
    let pointers = ok(&mut s, action(json!({"action":"ui.pointers"})));
    let absent = ok(
        &mut s,
        action(json!({"action":"ui.pointers","uiId":"absent"})),
    );
    let slug = ok(
        &mut s,
        action(json!({"action":"ui.resolveSlug","slug":"sample-ui"})),
    );
    let no_slug = ok(
        &mut s,
        action(json!({"action":"ui.resolveSlug","slug":"absent"})),
    );
    ok(
        &mut s,
        action(json!({"action":"ui.entry","uiId":"sample-ui"})),
    );
    let current = ok(
        &mut s,
        action(json!({"action":"ui.currentAsset","uiId":"sample-ui"})),
    );
    ok(
        &mut s,
        action(json!({"action":"ui.currentAsset","uiId":"sample-ui","path":""})),
    );
    ok(
        &mut s,
        action(json!({"action":"ui.currentAsset","uiId":"sample-ui","path":"index.html"})),
    );
    ok(
        &mut s,
        action(json!({"action":"asset","uiId":"sample-ui","releaseId":"r1","path":"index.html"})),
    );
    let invalid = push(
        &mut s,
        action(
            json!({"action":"asset","uiId":"absent","releaseId":"absent","path":"../index.html"}),
        ),
    );
    let missing = push(&mut s, action(json!({"action":"ui.entry","uiId":"absent"})));
    let result = compare(&s);
    assert_eq!(
        result["rows"][pointers]["value"],
        json!([{"app_id":"sample-ui","current_release_id":"r1","previous_release_id":null}])
    );
    assert_eq!(result["rows"][absent]["value"], json!([]));
    assert_eq!(result["rows"][slug]["value"], "sample-ui");
    assert!(result["rows"][no_slug]["value"].is_null());
    assert_eq!(
        result["rows"][current]["value"]["asset"]["contentHash"],
        ivy_contracts::hash::digest("<h1>Worker</h1>")
    );
    error(&result, invalid, "invalid_arguments");
    error(&result, missing, "not_found");
}

#[test]
fn worker_package_actions_preserve_constructor_principal_and_protocol_diagnostic_filters() {
    let mut s = setup();
    ok(
        &mut s,
        action(
            json!({"action":"credentials.configure","credentials":[{"principalId":"beta","digest":credential("b")}]}),
        ),
    );
    let seeded = ok(
        &mut s,
        action(
            json!({"action":"package.seed","catalog":{"schemaVersion":1,"revision":0,"packages":[]}}),
        ),
    );
    let catalog = ok(
        &mut s,
        action(json!({"action":"package.catalog","after":0,"includeUis":true})),
    );
    ok(&mut s, action(json!({"action":"package.prune"})));
    let upload = super::super::package_catalog::authorization("worker-package", "1.0.0", "service");
    let published = ok(
        &mut s,
        action(json!({"action":"package.publish","input":upload,"publisherPrincipalId":"beta"})),
    );
    let replayed = ok(
        &mut s,
        action(json!({"action":"package.publish","input":upload,"publisherPrincipalId":"alpha"})),
    );
    let mut altered = upload;
    altered["archiveHash"] = credential("c").into();
    let conflict = push(
        &mut s,
        action(json!({"action":"package.publish","input":altered,"publisherPrincipalId":"beta"})),
    );
    let invalid = [Value::Null, json!([]), json!({}), json!({"manifest":null})].map(|input| {
        push(
            &mut s,
            action(json!({"action":"package.publish","input":input,"publisherPrincipalId":"beta"})),
        )
    });
    ok(
        &mut s,
        action(
            json!({"action":"protocol.diagnostic","context":{},"code":"invalid_frame","message":"Rejected frame"}),
        ),
    );
    ok(
        &mut s,
        action(
            json!({"action":"diagnostic","value":{"code":"fixture","resource":{},"source":"hive.tool-bindings","severity":"warning","status":"current","message":"Binding notice","firstObservedAt":"1970-01-01T00:01:40.000Z","lastObservedAt":"1970-01-01T00:01:40.000Z"}}),
        ),
    );
    let diagnostics = ok(&mut s, action(json!({"action":"binding.diagnostics"})));
    let result = compare(&s);
    assert_eq!(
        result["rows"][published]["value"]["publisherPrincipalId"],
        "beta"
    );
    assert_eq!(
        result["rows"][published]["value"],
        result["rows"][replayed]["value"]
    );
    error(&result, conflict, "version_conflict");
    assert_eq!(
        result["rows"][seeded]["value"]["objectId"],
        result["rows"][catalog]["value"]["objectId"]
    );
    assert_eq!(
        result["rows"][diagnostics]["value"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    for index in invalid {
        error(&result, index, "invalid_arguments");
    }
    let mut empty = setup();
    empty[0]["credentials"] = json!([]);
    let denied = push(
        &mut empty,
        action(json!({"action":"package.seed","catalog":null})),
    );
    error(&compare(&empty), denied, "service_unavailable");
}
