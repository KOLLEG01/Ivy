use super::*;
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};

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
    let root = Root::new();
    let observed = oracle(&root.0.join("observed.sqlite"), steps);
    // The reference supplies only finite Date/Intl/string primitive observations.
    let actual = native(
        &root.0.join("native.sqlite"),
        steps,
        &observed["registryRuntime"],
    );
    let mut bound = steps.to_vec();
    let mut prior = 0;
    for (index, step) in bound.iter_mut().enumerate() {
        if step["payload"]["action"] == "session.create" {
            if let Some(token) = actual["rows"][index]["value"]["sessionToken"].as_str() {
                let bytes = URL_SAFE_NO_PAD.decode(token).unwrap();
                assert_eq!(bytes.len(), 32);
                assert_eq!(token.len(), 43);
                step["sessionBytes"] = STANDARD.encode(bytes).into();
            }
        }
        let mutations = actual["rows"][index]["catalogMutations"]
            .as_array()
            .unwrap();
        if matches!(
            step["payload"]["action"].as_str(),
            Some("package.seed" | "package.prune" | "package.publish")
        ) {
            let mut ids = vec![];
            if mutations.len() > prior {
                assert_eq!(mutations.len(), prior + 1);
                let nonce = mutations.last().unwrap()["mutation_id"]
                    .as_str()
                    .unwrap()
                    .rsplit(':')
                    .next()
                    .unwrap();
                assert_eq!(uuid::Uuid::parse_str(nonce).unwrap().get_version_num(), 4);
                ids.push(nonce.to_owned());
                if step["payload"]["action"] == "package.seed" {
                    let id = actual["rows"][index]["value"]["objectId"].as_str().unwrap();
                    assert_eq!(uuid::Uuid::parse_str(id).unwrap().get_version_num(), 4);
                    ids.push(id.to_owned());
                }
            } else {
                ids.push(format!("00000000-0000-4000-8000-{index:012}"));
                ids.push(format!("00000000-0001-4000-8000-{index:012}"));
            }
            step["uuids"] = json!(ids);
        }
        prior = mutations.len();
    }
    let expected = oracle(&root.0.join("reference.sqlite"), &bound);
    for (i, (a, b)) in actual["rows"]
        .as_array()
        .unwrap()
        .iter()
        .zip(expected["rows"].as_array().unwrap())
        .enumerate()
    {
        assert_eq!(a, b, "Worker step {i}: {}", steps[i]);
        if matches!(
            steps[i]["op"].as_str(),
            Some("sql" | "configure" | "register")
        ) || steps[i]["requireSuccess"] == true
        {
            assert!(a["error"].is_null(), "Fixture step {i}: {}", a["error"]);
        }
    }
    for key in ["definitions", "metadata", "topics"] {
        assert_eq!(actual[key], expected[key]);
    }
    actual
}
fn notifications(result: &Value, index: usize) -> &[Value] {
    result["rows"][index]["notifications"].as_array().unwrap()
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
