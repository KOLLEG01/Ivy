use super::{Root, native, oracle};
use serde_json::{Value, json};

fn sql(text: impl Into<String>) -> Value {
    json!({"op":"sql","sql":text.into()})
}
fn digest(user: &str) -> String {
    format!(
        "sha256:{}",
        if user == "alpha" { "a" } else { "b" }.repeat(64)
    )
}
pub(super) fn setup() -> Vec<Value> {
    let mut text = super::query::seed(false);
    text.push_str("BEGIN IMMEDIATE; UPDATE metadata SET value='epoch' WHERE key='runtime_epoch';");
    super::query::insert(
        &mut text,
        "grand",
        "fixture/value",
        "1.0.0",
        "'id20'",
        false,
        "json",
        "{}",
        1,
    );
    text.push_str("UPDATE objects SET path='/id01/id20/grand',depth=3 WHERE id='grand';");
    // This synthetic identity tests only Hive's owning-service guard, not the
    // unfinished TaskBoard record bundle or its native archive workflow.
    for (key, mode, owner) in [
        (
            "task-board/task",
            "retain",
            json!({"kind":"service","serviceName":"task-board"}),
        ),
        ("fixture/owned", "owned", json!({"kind":"agent"})),
    ] {
        let definition=json!({"key":key,"version":"1.0.0","owner":owner,"mediaType":"application/json","jsonSchema":{"type":"object"},"retention":{"objects":{"mode":mode},"revisions":{"mode":"all"}},"specMarkdown":"synthetic storage guard fixture"}).to_string().replace('\'',"''");
        text.push_str(&format!("INSERT INTO contract_families VALUES('{key}','fixture','application/json','{mode}'); INSERT INTO contracts VALUES('{key}','1.0.0','{definition}');"));
    }
    super::query::insert(
        &mut text,
        "ticket",
        "task-board/task",
        "1.0.0",
        "NULL",
        false,
        "json",
        "{}",
        1,
    );
    text.push_str("INSERT INTO objects VALUES(NULL,'owned',NULL,'id01','owned','/owned',0,NULL,1,'fixture/owned',1,NULL,0,'2026-01-01','2026-01-02'); INSERT INTO revisions VALUES('owned',1,'fixture/owned','1.0.0','json','{}','sha256:fixture',2,'2026-01-02','{}'); COMMIT;");
    vec![
        sql(text),
        json!({"op":"configure","credentials":[{"principalId":"alpha","digest":digest("alpha")},{"principalId":"beta","digest":digest("beta")}]}),
        sql(
            "INSERT INTO service_nodes VALUES('task-node','alpha','fixture-host','task-board',1,'{}'),('other-principal','beta','fixture-host','task-board',1,'{}'),('other-service','alpha','fixture-host','fixture-service',1,'{}');",
        ),
    ]
}
pub(super) fn action(op: &str, nonce: &str, mut parameters: Value) -> Value {
    parameters["mutationId"] = json!(format!("epoch:1000:{nonce}"));
    let mut value =
        json!({"op":op,"params":parameters,"context":{"credentialDigest":digest("alpha")}});
    clock(&mut value, 100_000);
    value
}
fn clock(step: &mut Value, at: u64) {
    step["now"] = json!(at);
    step["nowText"] = json!(format!(
        "1970-01-01T00:{:02}:{:02}.000Z",
        at / 60_000,
        (at / 1000) % 60
    ));
}
fn archive(nonce: &str, id: &str, archived: bool) -> Value {
    action(
        "archive",
        nonce,
        json!({"objectId":id,"archived":archived,"expectedRevision":1}),
    )
}
fn reorder(nonce: &str, id: &str, before: Option<&str>) -> Value {
    action(
        "reorder",
        nonce,
        json!({"objectId":id,"beforeObjectId":before}),
    )
}
pub(super) fn compare(steps: &[Value]) -> Value {
    let root = Root::new();
    let expected = oracle(&root.0.join("reference.sqlite"), steps);
    let actual = native(&root.0.join("native.sqlite"), steps);
    assert_eq!(actual["rows"].as_array().unwrap().len(), steps.len());
    for (index, (left, right)) in actual["rows"]
        .as_array()
        .unwrap()
        .iter()
        .zip(expected["rows"].as_array().unwrap())
        .enumerate()
    {
        assert_eq!(
            left, right,
            "Mutation trace step {index} ({})",
            steps[index]["op"]
        );
        if matches!(steps[index]["op"].as_str(), Some("sql" | "configure")) {
            assert!(
                right["error"].is_null(),
                "Fixture setup failed at step {index}"
            );
        }
    }
    assert_eq!(actual["definitions"], expected["definitions"]);
    assert_eq!(actual["metadata"], expected["metadata"]);
    expected
}
fn object<'a>(rows: &'a Value, index: usize, id: &str) -> &'a Value {
    rows["rows"][index]["state"]["objects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .unwrap()
}
fn code(rows: &Value, index: usize) -> &Value {
    &rows["rows"][index]["error"]["data"]["code"]
}

#[test]
fn reorder_receipts_keep_dense_siblings_exact_errors_and_saved_retry_results() {
    let mut steps = setup();
    let first = steps.len();
    let ordered = reorder("first", "id39", Some("id01"));
    steps.push(ordered.clone());
    steps.push(reorder("end", "id00", None));
    let replay = steps.len();
    let mut retry = ordered.clone();
    clock(&mut retry, 300_000);
    steps.push(retry);
    let conflict = steps.len();
    let mut changed = ordered.clone();
    changed["params"]["beforeObjectId"] = Value::Null;
    steps.push(changed);
    steps.push(reorder("self", "id01", Some("id01")));
    steps.push(reorder("parent", "id01", Some("id20")));
    steps.push(reorder("missing-before", "id01", Some("missing")));
    steps.push(reorder("missing-source", "missing", None));
    steps.push(reorder("only-child", "grand", None));
    steps.push(json!({"op":"reopen"}));
    steps.push(ordered.clone());
    let mut other = ordered;
    other["context"]["credentialDigest"] = json!(digest("beta"));
    steps.push(other);
    let rows = compare(&steps);
    assert!(rows["rows"][first]["error"].is_null());
    assert_eq!(rows["rows"][first]["value"], rows["rows"][replay]["value"]);
    assert_ne!(
        rows["rows"][replay]["value"]["position"],
        object(&rows, replay, "id39")["position"]
    );
    assert_eq!(code(&rows, conflict), "mutation_conflict");
    assert_eq!(code(&rows, conflict + 1), "invalid_arguments");
    assert_eq!(code(&rows, conflict + 2), "invalid_arguments");
    let roots = rows["rows"][first]["state"]["objects"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["parent_id"].is_null())
        .collect::<Vec<_>>();
    let mut positions = roots
        .iter()
        .map(|r| r["position"].as_u64().unwrap())
        .collect::<Vec<_>>();
    positions.sort();
    assert_eq!(positions, (0..roots.len() as u64).collect::<Vec<_>>());
    assert_ne!(
        rows["rows"][steps.len() - 1]["state"]["mutations"],
        rows["rows"][steps.len() - 2]["state"]["mutations"]
    );
}

#[test]
fn archive_restore_keeps_explicit_descendants_owners_revision_guards_and_atomic_replay() {
    let mut steps = setup();
    let first = steps.len();
    let original = archive("root", "id01", true);
    steps.push(original.clone());
    let child = steps.len();
    let mut own = archive("child", "id20", true);
    clock(&mut own, 101_000);
    steps.push(own);
    let restored = steps.len();
    let mut restore = archive("restore", "id01", false);
    clock(&mut restore, 102_000);
    steps.push(restore);
    let repeat = steps.len();
    let mut twice = archive("again", "id20", true);
    clock(&mut twice, 103_000);
    steps.push(twice);
    steps.push(archive("child-restore", "id20", false));
    let replay = steps.len();
    steps.push(original.clone());
    let conflict = steps.len();
    let mut changed = original;
    changed["params"]["archived"] = json!(false);
    steps.push(changed);
    let revision = steps.len();
    let mut stale = archive("stale", "id01", true);
    stale["params"]["expectedRevision"] = json!(2);
    steps.push(stale);
    steps.push(archive("missing", "missing", true));
    let mut unguarded = archive("no-revision", "id01", true);
    unguarded["params"]
        .as_object_mut()
        .unwrap()
        .remove("expectedRevision");
    steps.push(unguarded);
    steps.push(json!({"op":"reopen"}));
    steps.push(archive("after-reopen", "id01", false));
    let rows = compare(&steps);
    assert_eq!(object(&rows, first, "grand")["effective_archive"], 1);
    assert_eq!(object(&rows, first, "owned")["effective_archive"], 0);
    assert_eq!(object(&rows, restored, "id01")["effective_archive"], 0);
    assert_eq!(object(&rows, restored, "id20")["effective_archive"], 1);
    assert_eq!(
        object(&rows, child, "id20")["archived_at"],
        object(&rows, repeat, "id20")["archived_at"]
    );
    assert_eq!(rows["rows"][first]["value"], rows["rows"][replay]["value"]);
    assert_eq!(object(&rows, replay, "id01")["effective_archive"], 0);
    assert_eq!(object(&rows, first, "grand")["updated_at"], "2026-01-02");
    assert_eq!(code(&rows, conflict), "mutation_conflict");
    assert_eq!(code(&rows, revision), "revision_conflict");
}

#[test]
fn task_ticket_archive_uses_admitted_service_binding_and_replays_before_guard_changes() {
    let mut steps = setup();
    let denied = steps.len();
    for (nonce, node) in [
        ("direct", None),
        ("wrong", Some("other-service")),
        ("wrong-principal", Some("other-principal")),
        ("unknown", Some("absent")),
        ("empty", Some("")),
    ] {
        let mut step = archive(nonce, "ticket", true);
        step["params"]["expectedRevision"] = json!(2);
        if let Some(node) = node {
            step["context"]["serviceNodeId"] = json!(node);
        }
        steps.push(step);
    }
    let accepted = steps.len();
    let mut owned = archive("owned", "ticket", true);
    owned["context"]["serviceNodeId"] = json!("task-node");
    steps.push(owned.clone());
    steps.push(sql(
        "UPDATE service_nodes SET service_name='fixture-service' WHERE id='task-node'",
    ));
    let replay = steps.len();
    let mut retry = owned.clone();
    retry["context"]
        .as_object_mut()
        .unwrap()
        .remove("serviceNodeId");
    steps.push(retry);
    let mut fresh = owned;
    fresh["params"]["mutationId"] = json!("epoch:1000:fresh");
    steps.push(fresh);
    steps.push(sql(
        "UPDATE credentials SET revoked=1 WHERE principal_id='alpha'",
    ));
    let revoked = steps.len();
    steps.push(archive("revoked", "ticket", false));
    let rows = compare(&steps);
    for index in denied..accepted {
        assert_eq!(code(&rows, index), "task_board_workflow_required");
    }
    assert!(rows["rows"][accepted]["error"].is_null());
    assert_eq!(
        rows["rows"][accepted]["value"],
        rows["rows"][replay]["value"]
    );
    assert_eq!(code(&rows, replay + 1), "task_board_workflow_required");
    assert_eq!(code(&rows, revoked), "unauthenticated");
}

#[test]
fn receipt_event_update_and_budget_failures_roll_back_all_navigation_effects() {
    let mut steps = setup();
    let triggers = [
        (
            "CREATE TRIGGER fixture_failure BEFORE INSERT ON mutations BEGIN SELECT RAISE(ABORT,'fixture_receipt_failure'); END;",
            archive("receipt", "id01", true),
        ),
        (
            "CREATE TRIGGER fixture_failure BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'fixture_event_failure'); END;",
            reorder("event", "id39", Some("id01")),
        ),
        (
            "CREATE TRIGGER fixture_failure BEFORE UPDATE OF position ON objects WHEN NEW.id='id03' BEGIN SELECT RAISE(ABORT,'fixture_position_failure'); END;",
            reorder("position", "id39", Some("id01")),
        ),
    ];
    let mut failures = vec![];
    for (trigger, step) in triggers {
        steps.push(sql(trigger));
        let failure = steps.len();
        steps.push(step.clone());
        failures.push(failure);
        steps.push(sql("DROP TRIGGER fixture_failure"));
        steps.push(step);
    }
    let budget = steps.len();
    let mut expired = archive("budget", "id01", false);
    expired["overdue"] = json!(true);
    steps.push(expired.clone());
    expired["overdue"] = json!(false);
    steps.push(expired);
    let future = steps.len();
    let mut fresh = archive("future", "id01", true);
    fresh["params"]["mutationId"] = json!("epoch:160001:future");
    steps.push(fresh);
    steps.push(sql(
        "UPDATE metadata SET value='2000' WHERE key='mutations_expired_before'",
    ));
    let old = steps.len();
    steps.push(archive("old", "id01", true));
    let replay = steps.len();
    steps.push(archive("receipt", "id01", true));
    let rows = compare(&steps);
    for index in failures {
        assert_eq!(code(&rows, index), "internal_error");
        assert_eq!(
            rows["rows"][index]["state"],
            rows["rows"][index - 1]["state"]
        );
        assert!(rows["rows"][index + 2]["error"].is_null());
    }
    assert_eq!(code(&rows, budget), "limit_exceeded");
    assert_eq!(
        rows["rows"][budget]["state"],
        rows["rows"][budget - 1]["state"]
    );
    assert!(rows["rows"][budget + 1]["error"].is_null());
    assert_eq!(code(&rows, future), "mutation_expired");
    assert_eq!(code(&rows, old), "mutation_expired");
    assert!(rows["rows"][replay]["error"].is_null());
}
