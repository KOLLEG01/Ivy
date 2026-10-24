use super::object_mutations::{action, compare};
use serde_json::{Value, json};

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}
fn sql(text: impl Into<String>) -> Value {
    json!({"op":"sql","sql":text.into()})
}
fn object(
    text: &mut String,
    id: &str,
    key: &str,
    parent: Option<&str>,
    owner: Option<&str>,
    content: Value,
) {
    let parent = parent.map(quote).unwrap_or_else(|| "NULL".into());
    let owner = owner.map(quote).unwrap_or_else(|| "NULL".into());
    let body = content.to_string();
    text.push_str(&format!("INSERT INTO objects VALUES(NULL,{id},{parent},{owner},{id},COALESCE((SELECT path FROM objects WHERE id={parent}),'')||'/'||{id},0,NULL,COALESCE((SELECT depth FROM objects WHERE id={parent}),0)+1,{key},1,NULL,0,'2026-01-01','2026-01-02'); INSERT INTO revisions VALUES({id},1,{key},'1.0.0','json',{body},'sha256:fixture',{bytes},'2026-01-02','{{}}');",id=quote(id),key=quote(key),body=quote(&body),bytes=body.len()));
}
fn revision(id: &str, number: u64, content: Value) -> Value {
    let content = content.to_string();
    sql(format!(
        "BEGIN IMMEDIATE; INSERT INTO revisions SELECT object_id,{number},contract_key,contract_version,'json',{content},'sha256:fixture',{bytes},'2026-01-03','{{}}' FROM revisions WHERE object_id={id} AND revision=1; UPDATE objects SET current_revision={number} WHERE id={id}; COMMIT;",
        id = quote(id),
        bytes = content.len(),
        content = quote(&content)
    ))
}
fn remove(nonce: &str, id: &str, revision: u64) -> Value {
    action(
        "delete",
        nonce,
        json!({"objectId":id,"expectedRevision":revision}),
    )
}
fn setup() -> Vec<Value> {
    let mut steps = super::object_mutations::setup();
    let mut text = String::from("BEGIN IMMEDIATE;");
    // Synthetic retained rows isolate the original delete graph and SQL guards.
    // They do not qualify the current TaskBoard bundle or public write schemas.
    for key in [
        "wiki/page",
        "wiki/attachment",
        "dashboards/dashboard",
        "task-board/run",
        "task-board/result",
        "task-board/review",
        "task-board/history",
        "task-board/delivery",
        "task-board/comment-read-state",
        "task-board/task-key-allocation",
    ] {
        let definition = json!({"key":key,"version":"1.0.0","mediaType":"application/json","owner":{"kind":"agent"},"retention":{"objects":{"mode":"retain"},"revisions":{"mode":"all"}},"jsonSchema":{"type":"object"},"specMarkdown":"synthetic retained deletion fixture"});
        text.push_str(&format!("INSERT INTO contract_families VALUES({key},'fixture','application/json','retain'); INSERT INTO contracts VALUES({key},'1.0.0',{definition});",key=quote(key),definition=quote(&definition.to_string())));
    }
    for (id, key, parent, owner) in [
        ("page", "wiki/page", None, None),
        ("page-child", "fixture/value", Some("page"), None),
        ("leaf", "wiki/attachment", Some("page-child"), None),
        ("artifact", "fixture/owned", None, Some("page")),
        ("artifact-child", "fixture/owned", None, Some("page-child")),
        ("dashboard", "dashboards/dashboard", None, None),
        ("public-attachment", "wiki/attachment", None, None),
        ("workspace", "wiki/page", None, None),
    ] {
        object(&mut text, id, key, parent, owner, json!({"fixture":id}));
    }
    text.push_str("UPDATE objects SET archived_at='2026-01-02',effective_archive=1 WHERE id='leaf'; INSERT INTO revision_references VALUES('page-child',1,'inside','leaf',1),('leaf',1,'cycle','page',1); COMMIT;");
    steps.push(sql(text));
    steps.push(refresh());
    steps
}
fn refresh() -> Value {
    sql(
        "INSERT OR REPLACE INTO retention_families SELECT key,'fixture-policy',1,json_object('objectCount',(SELECT COUNT(*) FROM objects WHERE contract_key=key),'revisionCount',(SELECT COUNT(*) FROM revisions WHERE contract_key=key),'byteLength',(SELECT COALESCE(SUM(byte_length),0) FROM revisions WHERE contract_key=key),'untouched','keep') FROM contract_families; INSERT INTO object_fts(rowid,object_id,name,text) SELECT search_id,id,name,'needle' FROM objects WHERE search_id NOT IN (SELECT rowid FROM object_fts);",
    )
}
fn state(rows: &Value, step: usize) -> &Value {
    &rows["rows"][step]["state"]
}
fn code(rows: &Value, step: usize) -> &Value {
    &rows["rows"][step]["error"]["data"]["code"]
}
fn absent(rows: &Value, step: usize, id: &str) -> bool {
    state(rows, step)["objects"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["id"] != id)
}
fn no_effect(rows: &Value, step: usize, expected: &str) {
    assert_eq!(code(rows, step), expected);
    assert_eq!(state(rows, step), state(rows, step - 1));
}

#[test]
fn delete_preserves_navigation_owned_closure_retained_history_dense_positions_and_replay() {
    let mut steps = setup();
    steps.push(revision("page", 2, json!({"title":"old"})));
    steps.push(revision("page", 3, json!({"title":"current"})));
    steps.push(refresh());
    let invalid = steps.len();
    steps.push(remove("invalid", "id01", 99));
    let stale = steps.len();
    steps.push(remove("stale", "page", 2));
    let missing = steps.len();
    steps.push(remove("missing", "missing", 1));
    let deleted = steps.len();
    let original = remove("delete", "page", 3);
    steps.push(original.clone());
    let replay = steps.len();
    steps.push(original.clone());
    let conflict = steps.len();
    let mut changed = original.clone();
    changed["params"]["expectedRevision"] = json!(2);
    steps.push(changed);
    steps.push(json!({"op":"reopen"}));
    let restart = steps.len();
    steps.push(original.clone());
    let other = steps.len();
    let mut beta = original.clone();
    beta["context"]["credentialDigest"] = json!(format!("sha256:{}", "b".repeat(64)));
    steps.push(beta);
    steps.push(sql(
        "UPDATE metadata SET value='2000' WHERE key='mutations_expired_before'",
    ));
    steps.push(original);
    let rows = compare(&steps);
    no_effect(&rows, invalid, "invalid_arguments");
    no_effect(&rows, stale, "revision_conflict");
    no_effect(&rows, missing, "not_found");
    assert_eq!(rows["rows"][deleted]["value"], json!({"deleted":true}));
    for id in ["page", "page-child", "leaf", "artifact", "artifact-child"] {
        assert!(absent(&rows, deleted, id));
    }
    assert!(!absent(&rows, deleted, "owned"));
    assert!(
        state(&rows, deleted)["references"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        state(&rows, deleted)["fts"]
            .as_array()
            .unwrap()
            .iter()
            .all(
                |row| !row["object_id"].as_str().unwrap().starts_with("page")
                    && row["object_id"] != "leaf"
                    && row["object_id"] != "artifact"
                    && row["object_id"] != "artifact-child"
            )
    );
    let mut positions = state(&rows, deleted)["objects"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["parent_id"].is_null())
        .map(|row| row["position"].as_u64().unwrap())
        .collect::<Vec<_>>();
    positions.sort();
    assert_eq!(positions, (0..positions.len() as u64).collect::<Vec<_>>());
    assert_eq!(state(&rows, deleted), state(&rows, replay));
    assert_eq!(
        rows["rows"][deleted]["value"],
        rows["rows"][restart]["value"]
    );
    no_effect(&rows, conflict, "mutation_conflict");
    no_effect(&rows, other, "not_found");
    let summary = state(&rows, deleted)["retention"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["contract_key"] == "wiki/page")
        .unwrap();
    let summary: Value = serde_json::from_str(summary["summary_json"].as_str().unwrap()).unwrap();
    assert_eq!(summary["objectCount"], 1);
    assert_eq!(summary["untouched"], "keep");
}

#[test]
fn incoming_pinned_references_protect_the_whole_delete_closure_and_all_allowed_roots_work() {
    let mut steps = setup();
    steps.push(revision("leaf", 2, json!({"current":true})));
    steps.push(refresh());
    steps.push(sql("INSERT INTO revision_references VALUES('id00',1,'external','leaf',1); UPDATE retention_families SET summary_json='{\"objectCount\":\"stale\",\"revisionCount\":42,\"byteLength\":42}' WHERE contract_key='fixture/owned';"));
    let denied = steps.len();
    let pending = remove("protected", "page", 1);
    steps.push(pending.clone());
    steps.push(sql(
        "DELETE FROM revision_references WHERE source_object_id='id00'",
    ));
    let accepted = steps.len();
    steps.push(pending);
    steps.push(remove("dashboard", "dashboard", 1));
    steps.push(remove("attachment", "public-attachment", 1));
    let rows = compare(&steps);
    no_effect(&rows, denied, "target_conflict");
    assert!(rows["rows"][accepted]["error"].is_null());
    for id in ["page", "leaf", "dashboard", "public-attachment"] {
        assert!(absent(&rows, steps.len() - 1, id));
    }
    let owned = |index| {
        state(&rows, index)["retention"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["contract_key"] == "fixture/owned")
            .unwrap()
    };
    assert_eq!(owned(denied), owned(steps.len() - 1));
}

#[test]
fn task_delete_preserves_active_dependency_scope_and_associated_workflow_guards() {
    let mut steps = setup();
    let mut text = String::from("BEGIN IMMEDIATE;");
    object(
        &mut text,
        "task",
        "task-board/task",
        Some("workspace"),
        None,
        json!({"claim":{},"taskKey":"TK-1","fields":{"dependencies":[]}}),
    );
    object(
        &mut text,
        "dependent",
        "task-board/task",
        Some("workspace"),
        None,
        json!({"fields":{"dependencies":["task"]}}),
    );
    for (id, key) in [
        ("run", "task-board/run"),
        ("result", "task-board/result"),
        ("review", "task-board/review"),
        ("history-node", "task-board/history"),
        ("delivery", "task-board/delivery"),
        ("read-state", "task-board/comment-read-state"),
    ] {
        object(
            &mut text,
            id,
            key,
            Some("workspace"),
            None,
            json!({"taskId":"task"}),
        );
    }
    object(
        &mut text,
        "allocation",
        "task-board/task-key-allocation",
        Some("workspace"),
        None,
        json!({"taskKey":"TK-1"}),
    );
    object(
        &mut text,
        "other-allocation",
        "task-board/task-key-allocation",
        Some("workspace"),
        None,
        json!({"taskKey":"TK-2"}),
    );
    object(
        &mut text,
        "other-run",
        "task-board/run",
        Some("workspace"),
        None,
        json!({"taskId":"dependent"}),
    );
    object(
        &mut text,
        "external-run",
        "task-board/run",
        None,
        None,
        json!({"taskId":"task"}),
    );
    text.push_str("COMMIT;");
    steps.push(sql(text));
    steps.push(refresh());
    let active = steps.len();
    steps.push(remove("task", "task", 1));
    steps.push(revision(
        "task",
        2,
        json!({"claim":null,"publication":{},"taskKey":"TK-1"}),
    ));
    let publishing = steps.len();
    steps.push(remove("task", "task", 2));
    steps.push(revision(
        "task",
        3,
        json!({"claim":null,"publication":null,"taskKey":"TK-1"}),
    ));
    let dependency = steps.len();
    steps.push(remove("task", "task", 3));
    steps.push(revision(
        "dependent",
        2,
        json!({"fields":{"dependencies":[]}}),
    ));
    steps.push(refresh());
    let accepted = steps.len();
    let original = remove("task", "task", 3);
    steps.push(original.clone());
    let replay = steps.len();
    steps.push(original);
    let workspace = steps.len();
    steps.push(remove("workspace", "workspace", 1));
    let rows = compare(&steps);
    for index in [active, publishing, dependency] {
        no_effect(&rows, index, "target_conflict");
    }
    assert!(rows["rows"][accepted]["error"].is_null());
    assert_eq!(state(&rows, accepted), state(&rows, replay));
    for id in [
        "task",
        "run",
        "result",
        "review",
        "history-node",
        "delivery",
        "read-state",
        "allocation",
    ] {
        assert!(absent(&rows, accepted, id));
    }
    for id in [
        "workspace",
        "dependent",
        "other-run",
        "other-allocation",
        "external-run",
    ] {
        assert!(!absent(&rows, accepted, id));
    }
    for id in ["workspace", "dependent", "other-run", "other-allocation"] {
        assert!(absent(&rows, workspace, id));
    }
    assert!(!absent(&rows, workspace, "external-run"));
}

#[test]
fn deletion_failures_restore_fts_references_usage_positions_events_receipts_and_retry_identity() {
    for trigger in [
        "BEFORE UPDATE OF summary_json ON retention_families WHEN OLD.contract_key='wiki/page' BEGIN SELECT RAISE(ABORT,'fixture_usage_failure'); END;",
        "BEFORE DELETE ON revision_references BEGIN SELECT RAISE(ABORT,'fixture_reference_failure'); END;",
        "BEFORE DELETE ON revisions WHEN OLD.object_id='page' BEGIN SELECT RAISE(ABORT,'fixture_revision_failure'); END;",
        "BEFORE DELETE ON objects WHEN OLD.id='page' BEGIN SELECT RAISE(ABORT,'fixture_object_failure'); END;",
        "BEFORE UPDATE OF position ON objects WHEN NEW.id='id03' BEGIN SELECT RAISE(ABORT,'fixture_position_failure'); END;",
        "BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'fixture_event_failure'); END;",
        "BEFORE INSERT ON mutations BEGIN SELECT RAISE(ABORT,'fixture_receipt_failure'); END;",
        "AFTER INSERT ON mutations BEGIN INSERT INTO objects VALUES(NULL,'dangling',NULL,NULL,'dangling','/dangling',0,NULL,1,'fixture/value',1,NULL,0,'t','t'); END;",
    ] {
        let mut steps = setup();
        steps.push(sql(format!("CREATE TRIGGER fixture_failure {trigger}")));
        let failed = steps.len();
        let mutation = remove("failed", "page", 1);
        steps.push(mutation.clone());
        steps.push(sql("DROP TRIGGER fixture_failure"));
        let retried = steps.len();
        steps.push(mutation);
        let rows = compare(&steps);
        no_effect(&rows, failed, "internal_error");
        assert!(rows["rows"][retried]["error"].is_null());
        assert!(absent(&rows, retried, "page"));
    }
    let mut steps = setup();
    let failed = steps.len();
    let mut mutation = remove("overdue", "page", 1);
    mutation["overdue"] = json!(true);
    steps.push(mutation.clone());
    mutation["overdue"] = json!(false);
    steps.push(mutation);
    let rows = compare(&steps);
    no_effect(&rows, failed, "limit_exceeded");
    assert!(absent(&rows, failed + 1, "page"));
}

#[test]
fn disposable_deletion_requires_the_current_owner_and_an_entirely_disposable_subtree() {
    let mut steps = setup();
    let owner = json!({"kind":"service","serviceName":"task-board"}).to_string();
    let definition = json!({"key":"fixture/disposable","version":"1.0.0","mediaType":"application/json","owner":serde_json::from_str::<Value>(&owner).unwrap(),"retention":{"objects":{"mode":"owned"},"revisions":{"mode":"all"}},"jsonSchema":{"type":"object"},"specMarkdown":"Synthetic disposable service records."});
    let mut source = format!("BEGIN IMMEDIATE; INSERT INTO contract_families VALUES('fixture/disposable',{},'application/json','owned'); INSERT INTO contracts VALUES('fixture/disposable','1.0.0',{});", quote(&owner), quote(&definition.to_string()));
    object(&mut source, "disposable", "fixture/disposable", None, None, json!({}));
    source.push_str("COMMIT;");
    steps.push(sql(source));
    let denied = steps.len();
    for (nonce, node, generation) in [
        ("no-node", None, 1.0),
        ("wrong-service", Some("other-service"), 1.0),
        ("wrong-principal", Some("other-principal"), 1.0),
        ("no-generation", Some("task-node"), 0.0),
        ("fractional-generation", Some("task-node"), 1.5),
    ] {
        let mut step = remove(nonce, "disposable", 1);
        step["context"]["serviceNodeId"] = json!(node);
        step["context"]["generation"] = json!(generation);
        steps.push(step);
    }
    let mut retained = String::from("BEGIN IMMEDIATE;");
    object(&mut retained, "retained-child", "wiki/attachment", Some("disposable"), None, json!({}));
    retained.push_str("COMMIT;");
    steps.push(sql(retained));
    steps.push(refresh());
    let mut allowed = remove("owner-delete", "disposable", 1);
    allowed["context"]["serviceNodeId"] = json!("task-node");
    allowed["context"]["generation"] = json!(1);
    let blocked = steps.len();
    steps.push(allowed.clone());
    steps.push(sql("BEGIN IMMEDIATE; DELETE FROM object_fts WHERE object_id='retained-child'; DELETE FROM revisions WHERE object_id='retained-child'; DELETE FROM objects WHERE id='retained-child'; COMMIT;"));
    steps.push(refresh());
    let deleted = steps.len();
    steps.push(allowed.clone());
    steps.push(allowed);
    let rows = compare(&steps);
    for index in denied..denied + 5 { no_effect(&rows, index, "forbidden"); }
    no_effect(&rows, blocked, "target_conflict");
    assert_eq!(rows["rows"][deleted]["value"], json!({"deleted":true}));
    assert_eq!(rows["rows"][deleted + 1]["value"], rows["rows"][deleted]["value"]);
    assert!(absent(&rows, deleted, "disposable"));
}
