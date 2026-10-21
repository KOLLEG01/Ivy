use super::object_mutations::{action, compare};
use serde_json::{Value, json};

fn sql(text: impl Into<String>) -> Value {
    json!({"op":"sql","sql":text.into()})
}
fn moving(nonce: &str, id: &str, parent: Option<&str>, name: &str) -> Value {
    action(
        "move",
        nonce,
        json!({"objectId":id,"parentId":parent,"name":name}),
    )
}
fn setup() -> Vec<Value> {
    let mut steps = super::object_write::setup();
    steps.push(
        json!({"op":"register","owner":{"kind":"agent"},"definition":{
            "key":"fixture/owned","version":"1.0.0","owner":{"kind":"agent"},
            "mediaType":"application/json","jsonSchema":{"type":"object"},
            "retention":{"objects":{"mode":"owned"},"revisions":{"mode":"all"}},
            "specMarkdown":"synthetic move ownership fixture"
        }}),
    );
    steps.push(sql("BEGIN IMMEDIATE; UPDATE objects SET icon='kept',position=4 WHERE id='json'; UPDATE objects SET parent_id='json',path='/json/text',depth=2,position=3 WHERE id='text'; UPDATE objects SET parent_id='text',path='/json/text/hive',depth=3,position=7 WHERE id='hive'; UPDATE objects SET position=9 WHERE id='blob'; INSERT INTO objects VALUES(NULL,'owned',NULL,'json','owned','/owned',12,NULL,1,'fixture/owned',1,NULL,0,'2026-01-01','2026-01-02'); INSERT INTO revisions VALUES('owned',1,'fixture/owned','1.0.0','json','{}','sha256:fixture',2,'2026-01-02','{}'); INSERT INTO object_fts(rowid,object_id,name,text) SELECT search_id,id,name,'' FROM objects WHERE id='owned'; COMMIT;"));
    steps
}
fn state(rows: &Value, index: usize) -> &Value {
    &rows["rows"][index]["state"]
}
fn object<'a>(rows: &'a Value, index: usize, id: &str) -> &'a Value {
    state(rows, index)["objects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .unwrap()
}
fn code(rows: &Value, index: usize) -> &Value {
    &rows["rows"][index]["error"]["data"]["code"]
}
fn success(rows: &Value, index: usize) {
    assert!(
        rows["rows"][index]["error"].is_null(),
        "Move failed at {index}: {}",
        rows["rows"][index]["error"]
    );
}
fn no_effect(rows: &Value, index: usize, expected: &str) {
    assert_eq!(code(rows, index), expected);
    assert_eq!(state(rows, index), state(rows, index - 1));
}

#[test]
fn moves_refresh_cross_contract_subtrees_archive_and_positions_without_changing_history_or_owners()
{
    let mut steps = setup();
    steps.push(action("write", "linked-text", json!({"objectId":"text","expectedRevision":1,"contractVersion":"1.0.0","content":{"encoding":"text","value":"search café"},"references":{"blob":{"objectId":"blob","revision":1}}})));
    steps.push(action(
        "archive",
        "archive-parent",
        json!({"objectId":"blob","expectedRevision":1,"archived":true}),
    ));
    let first = steps.len();
    let mut moved = moving("nested", "json", Some("blob"), "Cafe\u{301}");
    moved["params"]["icon"] = json!("\u{feff} e\u{301} \u{2029}");
    steps.push(moved);
    let restored = steps.len();
    steps.push(moving("root", "json", None, "Café"));
    let noop = steps.len();
    steps.push(moving("noop", "json", None, "Café"));
    let cleared = steps.len();
    let mut clear = moving("clear-icon", "json", None, "Café");
    clear["params"]["icon"] = Value::Null;
    steps.push(clear);
    let owned = steps.len();
    steps.push(moving("owned", "owned", Some("json"), "artifact"));
    let text = steps.len();
    steps.push(moving("text-rename", "text", Some("json"), "Renamed"));
    let history = steps.len();
    steps.push(json!({"op":"readObject","objectId":"text","revision":2}));
    let rows = compare(&steps);
    for index in first..=history {
        success(&rows, index);
    }
    assert_eq!(rows["rows"][first]["value"]["path"], "/blob/Café");
    assert_eq!(rows["rows"][first]["value"]["icon"], "é");
    for (id, suffix) in [("json", ""), ("text", "/text"), ("hive", "/text/hive")] {
        assert_eq!(
            object(&rows, first, id)["path"],
            format!("/blob/Café{suffix}")
        );
        assert_eq!(object(&rows, first, id)["effective_archive"], 1);
        assert!(object(&rows, first, id)["archived_at"].is_null());
        assert_eq!(object(&rows, restored, id)["effective_archive"], 0);
        assert_eq!(
            object(&rows, restored, id)["path"],
            format!("/Café{suffix}")
        );
    }
    for id in ["text", "hive"] {
        assert_eq!(
            object(&rows, first, id)["updated_at"],
            object(&rows, first - 1, id)["updated_at"]
        );
        assert_eq!(
            object(&rows, first, id)["search_id"],
            object(&rows, first - 1, id)["search_id"]
        );
    }
    assert_eq!(object(&rows, first, "blob")["position"], 0);
    assert_eq!(object(&rows, first, "owned")["position"], 1);
    assert_eq!(object(&rows, restored, "json")["position"], 2);
    assert_eq!(object(&rows, restored, "json")["icon"], "é");
    assert!(object(&rows, cleared, "json")["icon"].is_null());
    assert_eq!(object(&rows, owned, "owned")["owner_object_id"], "json");
    assert_eq!(object(&rows, owned, "owned")["position"], 4);
    assert_eq!(object(&rows, text, "hive")["path"], "/Café/Renamed/hive");
    for key in ["objects", "fts", "revisions", "references", "retention"] {
        assert_eq!(state(&rows, noop)[key], state(&rows, noop - 1)[key]);
    }
    assert_ne!(
        state(&rows, noop)["events"],
        state(&rows, noop - 1)["events"]
    );
    for key in ["revisions", "references", "retention"] {
        assert_eq!(state(&rows, text)[key], state(&rows, first - 1)[key]);
    }
    let fts = state(&rows, text)["fts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["object_id"] == "text")
        .unwrap();
    assert_eq!(fts["name"], "Renamed");
    assert_eq!(fts["text"], "search café");
    assert_eq!(fts["rowid"], object(&rows, text, "text")["search_id"]);
    assert_eq!(
        rows["rows"][history]["value"]["revision"]["references"],
        json!({"blob":{"objectId":"blob","revision":1}})
    );
    assert_eq!(
        rows["rows"][history]["value"]["content"]["value"],
        "search café"
    );
}

#[test]
fn move_guard_order_preserves_parent_name_cycle_subtree_depth_sibling_position_and_icon_errors() {
    let mut steps = setup();
    let mut errors = vec![];
    let cases = [
        (
            "missing",
            "missing",
            Some("missing"),
            "/",
            "not_found",
            "Object not found.",
        ),
        (
            "parent",
            "json",
            Some("missing"),
            "/",
            "not_found",
            "Object not found.",
        ),
        (
            "name",
            "json",
            Some("owned"),
            "/",
            "invalid_arguments",
            "Invalid Object name.",
        ),
        (
            "owned-parent",
            "json",
            Some("owned"),
            "x",
            "invalid_arguments",
            "Owned artifacts cannot be navigation parents.",
        ),
        (
            "self",
            "json",
            Some("json"),
            "x",
            "invalid_arguments",
            "An Object cannot be moved below itself.",
        ),
        (
            "descendant",
            "json",
            Some("hive"),
            "x",
            "invalid_arguments",
            "An Object cannot be moved below itself.",
        ),
        (
            "sibling",
            "json",
            None,
            "blob",
            "revision_conflict",
            "An Object already uses that sibling name.",
        ),
    ];
    for (nonce, id, parent, name, code, message) in cases {
        errors.push((steps.len(), code, message));
        let mut step = moving(nonce, id, parent, name);
        step["params"]["icon"] = json!("\n");
        if nonce == "missing" {
            step["overdue"] = true.into();
        }
        steps.push(step);
    }
    steps.push(sql("UPDATE objects SET depth=64 WHERE id='blob';"));
    errors.push((
        steps.len(),
        "limit_exceeded",
        "Object hierarchy exceeds its maximum depth.",
    ));
    steps.push(moving("depth-parent", "json", Some("blob"), "x"));
    steps.push(sql("UPDATE objects SET depth=62 WHERE id='blob';"));
    errors.push((
        steps.len(),
        "limit_exceeded",
        "Move would exceed maximum hierarchy depth.",
    ));
    steps.push(moving("depth-subtree", "json", Some("blob"), "x"));
    steps.push(sql("UPDATE objects SET depth=1 WHERE id='blob'; UPDATE objects SET position=9007199254740991 WHERE id='hive';"));
    let mut full = moving("full", "owned", Some("text"), "artifact");
    full["params"]["icon"] = json!("\n");
    errors.push((
        steps.len(),
        "limit_exceeded",
        "Object sibling position limit reached.",
    ));
    steps.push(full);
    let maximum = steps.len();
    steps.push(moving("same-parent-max", "hive", Some("text"), "hive"));
    let mut invalid_icon = moving("icon", "hive", Some("text"), "hive");
    invalid_icon["params"]["icon"] = json!("\n");
    errors.push((
        steps.len(),
        "invalid_arguments",
        "Object icon must be a short single-line value.",
    ));
    steps.push(invalid_icon);
    let mut overdue = moving("overdue", "json", Some("missing"), "/");
    overdue["overdue"] = true.into();
    errors.push((
        steps.len(),
        "limit_exceeded",
        "Database operation exceeded its work budget.",
    ));
    steps.push(overdue);
    let rows = compare(&steps);
    for (index, code, message) in errors {
        no_effect(&rows, index, code);
        assert_eq!(rows["rows"][index]["error"]["message"], message);
    }
    success(&rows, maximum);
    assert_eq!(
        object(&rows, maximum, "hive")["position"],
        9007199254740991u64
    );
}

#[test]
fn move_receipts_replay_before_current_revision_archive_budget_and_deleted_source_reads() {
    let mut steps = super::object_write::setup();
    let original = moving("saved", "text", Some("json"), "Saved");
    let first = steps.len();
    steps.push(original.clone());
    steps.push(action("write", "later-content", json!({"objectId":"text","expectedRevision":1,"contractVersion":"2.0.0","content":{"encoding":"text","value":"changed"},"references":{}})));
    let latest = steps.len();
    steps.push(moving("later-move", "text", Some("blob"), "Later"));
    steps.push(action(
        "archive",
        "archive-blob",
        json!({"objectId":"blob","expectedRevision":1,"archived":true}),
    ));
    let replay = steps.len();
    let mut retry = original.clone();
    retry["overdue"] = true.into();
    steps.push(retry.clone());
    steps.push(sql("BEGIN IMMEDIATE; DELETE FROM revisions WHERE object_id='text'; DELETE FROM objects WHERE id='text'; DELETE FROM object_fts WHERE object_id='text'; COMMIT;"));
    steps.push(json!({"op":"reopen"}));
    let deleted = steps.len();
    steps.push(retry);
    let conflict = steps.len();
    let mut changed = original.clone();
    changed["params"]["name"] = "Changed".into();
    steps.push(changed);
    let other = steps.len();
    let mut beta = original;
    beta["context"]["credentialDigest"] = json!(format!("sha256:{}", "b".repeat(64)));
    steps.push(beta);
    let rows = compare(&steps);
    for index in [first, latest, replay, deleted] {
        success(&rows, index);
    }
    assert_eq!(rows["rows"][latest]["value"]["currentRevision"], 2);
    assert_eq!(rows["rows"][latest]["value"]["contractVersion"], "2.0.0");
    for index in [replay, deleted] {
        assert_eq!(rows["rows"][index]["value"], rows["rows"][first]["value"]);
        assert_eq!(state(&rows, index), state(&rows, index - 1));
    }
    no_effect(&rows, conflict, "mutation_conflict");
    no_effect(&rows, other, "not_found");
}

#[test]
fn move_unicode_normalization_and_raw_utf16_sql_aliases_match_original_identity_checks() {
    let mut steps = setup();
    let mut errors = vec![];
    for (nonce, field, message) in [
        ("bad-name", "/name", "Object name must be valid Unicode."),
        ("bad-icon", "/icon", "Object icon must be valid Unicode."),
    ] {
        let mut step = moving(nonce, "json", None, "json");
        step["parameterUnits"] = json!({field:[0xd800]});
        errors.push((steps.len(), "invalid_arguments", message));
        steps.push(step);
    }
    let normalized = steps.len();
    let mut nfc = moving("normalized", "json", None, "\u{1100}\u{1161}\u{11a8}");
    nfc["params"]["icon"] = json!("\u{2003}A\u{30a}\u{301}\u{2028}");
    steps.push(nfc);
    let mut sibling = moving(
        "normalized-sibling",
        "blob",
        None,
        "\u{1100}\u{1161}\u{11a8}",
    );
    sibling["params"]["icon"] = json!("\n");
    errors.push((
        steps.len(),
        "revision_conflict",
        "An Object already uses that sibling name.",
    ));
    steps.push(sibling);
    steps.push(sql("BEGIN IMMEDIATE; INSERT INTO objects VALUES(NULL,'�',NULL,NULL,'alias','/alias',15,NULL,1,'fixture/json',1,NULL,0,'now','now'); INSERT INTO revisions VALUES('�',1,'fixture/json','1.0.0','json','{\"n\":0}','sha256:fixture',7,'now','{}'); COMMIT;"));
    let mut alias = moving("alias-same-name", "unused", None, "alias");
    alias["parameterUnits"] = json!({"/objectId":[0xd800]});
    errors.push((
        steps.len(),
        "revision_conflict",
        "An Object already uses that sibling name.",
    ));
    steps.push(alias.clone());
    alias["params"]["mutationId"] = "epoch:1000:alias-rename".into();
    alias["params"]["name"] = "renamed".into();
    let renamed = steps.len();
    steps.push(alias.clone());
    alias["params"]["mutationId"] = "epoch:1000:alias-cycle".into();
    alias["params"]["parentId"] = "�".into();
    errors.push((
        steps.len(),
        "invalid_arguments",
        "An Object cannot be moved below itself.",
    ));
    steps.push(alias);
    let parent = steps.len();
    steps.push(moving("canonical-parent", "owned", Some("�"), "artifact"));
    let raw_parent = steps.len();
    let mut raw = moving("raw-parent", "owned", Some("unused"), "artifact");
    raw["parameterUnits"] = json!({"/parentId":[0xd800]});
    steps.push(raw);
    let rows = compare(&steps);
    for (index, code, message) in errors {
        no_effect(&rows, index, code);
        assert_eq!(rows["rows"][index]["error"]["message"], message);
    }
    for index in [normalized, renamed, parent, raw_parent] {
        success(&rows, index);
    }
    assert_eq!(rows["rows"][normalized]["value"]["name"], "각");
    assert_eq!(rows["rows"][normalized]["value"]["icon"], "Ǻ");
    assert_eq!(rows["rows"][renamed]["value"]["id"], "�");
    assert_eq!(rows["rows"][raw_parent]["value"]["parentId"], "�");
    assert_eq!(rows["rows"][raw_parent]["value"]["position"], 0);
    assert_eq!(rows["rows"][raw_parent]["value"]["ownerObjectId"], "json");
}

#[test]
fn move_rolls_back_source_siblings_descendants_events_receipts_and_deferred_commit_then_retries() {
    for (nonce, trigger) in [
        (
            "source",
            "CREATE TRIGGER reject_move BEFORE UPDATE OF parent_id ON objects WHEN OLD.id='json' BEGIN SELECT RAISE(ABORT,'fixture_move'); END;",
        ),
        (
            "siblings",
            "CREATE TRIGGER reject_move BEFORE UPDATE OF position ON objects WHEN OLD.id='blob' BEGIN SELECT RAISE(ABORT,'fixture_move'); END;",
        ),
        (
            "navigation",
            "CREATE TRIGGER reject_move BEFORE UPDATE OF path ON objects WHEN OLD.id='text' BEGIN SELECT RAISE(ABORT,'fixture_move'); END;",
        ),
        (
            "event",
            "CREATE TRIGGER reject_move BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'fixture_move'); END;",
        ),
        (
            "receipt",
            "CREATE TRIGGER reject_move BEFORE INSERT ON mutations BEGIN SELECT RAISE(ABORT,'fixture_move'); END;",
        ),
        (
            "commit",
            "CREATE TRIGGER reject_move AFTER INSERT ON mutations BEGIN UPDATE objects SET current_revision=999 WHERE id='json'; END;",
        ),
    ] {
        let mut steps = setup();
        steps.push(sql(trigger));
        let failed = steps.len();
        let pending = moving(nonce, "json", Some("blob"), "Moved");
        steps.push(pending.clone());
        steps.push(sql("DROP TRIGGER reject_move;"));
        let retried = steps.len();
        steps.push(pending.clone());
        steps.push(json!({"op":"reopen"}));
        let replay = steps.len();
        steps.push(pending);
        let rows = compare(&steps);
        no_effect(&rows, failed, "internal_error");
        success(&rows, retried);
        success(&rows, replay);
        assert_eq!(
            rows["rows"][retried]["value"],
            rows["rows"][replay]["value"]
        );
        assert_eq!(state(&rows, replay), state(&rows, replay - 1));
    }
    // A corrupt persisted current revision fails during readback after the
    // navigation UPDATE, and must roll back that whole mutation too.
    let mut steps = setup();
    steps.push(sql("BEGIN IMMEDIATE; INSERT INTO revisions SELECT object_id,2,contract_key,contract_version,'json','{','sha256:fixture',1,created_at,references_json FROM revisions WHERE object_id='json' AND revision=1; UPDATE objects SET current_revision=2 WHERE id='json'; COMMIT;"));
    let failed = steps.len();
    steps.push(moving("bad-content", "json", Some("blob"), "Moved"));
    let rows = compare(&steps);
    no_effect(&rows, failed, "internal_error");
}
