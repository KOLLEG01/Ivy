use super::super::{Root, native, oracle};
use super::{Value, json};

fn create(nonce: &str, name: &str, key: &str) -> Value {
    let mut step = super::write(
        nonce,
        "unused",
        1,
        if key == "fixture/hive" {
            "text"
        } else {
            "json"
        },
        if key == "fixture/hive" {
            json!("internal")
        } else {
            json!({"n":1})
        },
        json!({}),
    );
    step["params"].as_object_mut().unwrap().remove("objectId");
    step["params"]
        .as_object_mut()
        .unwrap()
        .remove("expectedRevision");
    step["params"]["create"] =
        json!({"name":name,"contractKey":key,"parentId":null,"ownerObjectId":null,"icon":null});
    step
}
fn bind(step: &mut Value, pointer: &str, from: usize) {
    if !step["bindings"].is_array() {
        step["bindings"] = json!([]);
    }
    step["bindings"]
        .as_array_mut()
        .unwrap()
        .push(json!({"at":pointer,"from":from}));
}
fn compare(steps: &[Value]) -> Value {
    let root = Root::new();
    // Execute the real Rust UUID generator. Feed each successful generated ID
    // into Node's original crypto binding so both stores see the same identity.
    let actual = native(&root.0.join("native.sqlite"), steps);
    let mut original = steps.to_vec();
    for (index, step) in original.iter_mut().enumerate() {
        if step["params"]["create"].is_object() || step["preparedParams"]["create"].is_object() {
            if let Some(id) = actual["rows"][index]["value"]["object"]["id"].as_str() {
                assert_eq!(uuid::Uuid::parse_str(id).unwrap().get_version_num(), 4);
                step["createdId"] = id.into();
            }
        }
    }
    let expected = oracle(&root.0.join("original.sqlite"), &original);
    for (index, (left, right)) in actual["rows"]
        .as_array()
        .unwrap()
        .iter()
        .zip(expected["rows"].as_array().unwrap())
        .enumerate()
    {
        assert_eq!(
            left, right,
            "Object creation/rename trace {index} ({})",
            steps[index]["op"]
        );
        if matches!(
            steps[index]["op"].as_str(),
            Some("sql" | "register" | "configure")
        ) {
            assert!(right["error"].is_null(), "Fixture setup failed at {index}");
        }
    }
    assert_eq!(actual["definitions"], expected["definitions"]);
    assert_eq!(actual["topics"], expected["topics"]);
    actual
}
fn owned() -> Value {
    let mut value = super::definition("fixture/owned", "1.0.0", "application/json", false);
    value["definition"]["retention"]["objects"]["mode"] = "owned".into();
    value
}
fn code(result: &Value, index: usize) -> &Value {
    super::code(result, index)
}
fn error(result: &Value, index: usize, message: &str) {
    assert_eq!(result["rows"][index]["error"]["message"], message);
    assert_eq!(
        result["rows"][index]["state"],
        result["rows"][index - 1]["state"],
        "Failed mutation must roll back"
    );
}
fn object<'a>(result: &'a Value, index: usize, id: &str) -> &'a Value {
    result["rows"][index]["state"]["objects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .unwrap()
}

#[test]
fn creation_uses_real_uuids_normalized_navigation_archive_inheritance_and_owned_lifetimes() {
    let mut steps = super::setup();
    steps.push(owned());
    let root = steps.len();
    let mut parent = create("parent", "Cafe\u{301}", "fixture/json");
    parent["params"]["create"]["icon"] = json!("\u{feff} e\u{301} \u{2029}");
    steps.push(parent.clone());
    let mut archive = super::super::object_mutations::action(
        "archive",
        "archive-parent",
        json!({"objectId":"bound","expectedRevision":1,"archived":true}),
    );
    bind(&mut archive, "/params/objectId", root);
    steps.push(archive);
    let child = steps.len();
    let mut nested = create("child", "\u{1100}\u{1161}\u{11a8}", "fixture/json");
    bind(&mut nested, "/params/create/parentId", root);
    nested["params"]["references"] = json!({"text":{"objectId":"text","revision":1}});
    steps.push(nested);
    let artifact = steps.len();
    let mut attachment = create("owned", "attached", "fixture/owned");
    bind(&mut attachment, "/params/create/ownerObjectId", child);
    steps.push(attachment);
    steps.push(json!({"op":"reopen"}));
    let replay = steps.len();
    steps.push(parent);
    let result = compare(&steps);
    for index in [root, child, artifact, replay] {
        assert!(result["rows"][index]["error"].is_null());
    }
    assert_eq!(result["rows"][root]["value"]["object"]["name"], "Café");
    assert_eq!(result["rows"][root]["value"]["object"]["icon"], "é");
    assert_eq!(result["rows"][child]["value"]["object"]["path"], "/Café/각");
    assert_eq!(
        result["rows"][child]["value"]["object"]["effectivelyArchived"],
        true
    );
    assert_eq!(
        result["rows"][child]["value"]["object"]["archivedAt"],
        Value::Null
    );
    assert_eq!(
        result["rows"][artifact]["value"]["object"]["ownerObjectId"],
        result["rows"][child]["value"]["object"]["id"]
    );
    assert_eq!(
        result["rows"][root]["value"],
        result["rows"][replay]["value"]
    );
    assert_ne!(
        result["rows"][root]["value"]["object"]["id"],
        result["rows"][child]["value"]["object"]["id"]
    );
    assert_eq!(
        result["rows"][child]["value"]["revision"]["references"],
        json!({"text":{"objectId":"text","revision":1}})
    );
    let summary = result["rows"][replay]["state"]["retention"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["contract_key"] == "fixture/json")
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(summary["summary_json"].as_str().unwrap()).unwrap(),
        json!({"objectCount":3,"revisionCount":3,"byteLength":21})
    );
}

#[test]
fn names_and_icons_apply_nfc_before_utf16_bounds_and_only_ecmascript_trim() {
    let mut steps = super::setup();
    let mut accepted = vec![];
    let mut refused = vec![];
    for (index, name) in [
        "A\u{30a}",
        "A\u{30a}\u{301}",
        "A\u{315}\u{300}",
        " ",
        "\u{2126}",
        "👩\u{200d}💻",
    ]
    .iter()
    .enumerate()
    {
        accepted.push(steps.len());
        steps.push(create(&format!("nfc-{index}"), name, "fixture/json"));
    }
    for (index, name) in ["e\u{301}".repeat(255), "🐝".repeat(127)]
        .iter()
        .enumerate()
    {
        accepted.push(steps.len());
        steps.push(create(&format!("bound-{index}"), name, "fixture/json"));
    }
    for (index, name) in [
        "".into(),
        ".".into(),
        "..".into(),
        "a/b".into(),
        "a\\b".into(),
        "a\0b".into(),
        "e\u{301}".repeat(256),
        "🐝".repeat(128),
    ]
    .iter()
    .enumerate()
    {
        refused.push((steps.len(), "Invalid Object name."));
        steps.push(create(&format!("bad-name-{index}"), name, "fixture/json"));
    }
    let mut lone = create("lone-name", "placeholder", "fixture/json");
    lone["parameterUnits"] = json!({"/create/name":[0xd800]});
    refused.push((steps.len(), "Object name must be valid Unicode."));
    steps.push(lone);
    for (index, (icon, good)) in [
        ("\u{feff} e\u{301} \u{2029}".into(), true),
        ("\r x \n".into(), true),
        ("\u{85}".into(), true),
        ("\u{180e}".into(), true),
        ("x".repeat(32), true),
        ("🐝".repeat(16), true),
        ("x".repeat(33), false),
        ("🐝".repeat(17), false),
        ("\u{feff} \u{2029}".into(), false),
        ("x\nx".into(), false),
        ("x\0x".into(), false),
    ]
    .iter()
    .enumerate()
    {
        let mut step = create(
            &format!("icon-{index}"),
            &format!("icon-{index}"),
            "fixture/json",
        );
        step["params"]["create"]["icon"] = json!(icon);
        if *good {
            accepted.push(steps.len());
        } else {
            refused.push((
                steps.len(),
                "Object icon must be a short single-line value.",
            ));
        }
        steps.push(step);
    }
    let mut lone = create("lone-icon", "lone-icon", "fixture/json");
    lone["parameterUnits"] = json!({"/create/icon":[0xdfff]});
    refused.push((steps.len(), "Object icon must be valid Unicode."));
    steps.push(lone);
    let result = compare(&steps);
    for index in accepted {
        assert!(result["rows"][index]["error"].is_null(), "accepted {index}");
    }
    for (index, message) in refused {
        assert_eq!(code(&result, index), "invalid_arguments");
        error(&result, index, message);
    }
}

#[test]
fn creation_keeps_contract_name_parent_depth_owner_position_and_icon_error_order() {
    let mut steps = super::setup();
    steps.push(owned());
    let mut failures = vec![];
    let owned_index = steps.len();
    let mut artifact = create("artifact", "owned", "fixture/owned");
    artifact["params"]["create"]["ownerObjectId"] = "text".into();
    steps.push(artifact);
    for (nonce, key, parent, owner, message) in [
        (
            "retained-owner",
            "fixture/json",
            Value::Null,
            json!("text"),
            "Domain Objects cannot have a lifecycle owner.",
        ),
        (
            "owned-null",
            "fixture/owned",
            Value::Null,
            Value::Null,
            "Owned Objects require an existing lifecycle owner.",
        ),
        (
            "owned-missing",
            "fixture/owned",
            Value::Null,
            json!("missing"),
            "Object not found.",
        ),
        (
            "parent-missing",
            "fixture/json",
            json!("missing"),
            json!("text"),
            "Object not found.",
        ),
    ] {
        let mut step = create(nonce, nonce, key);
        step["params"]["create"]["parentId"] = parent;
        step["params"]["create"]["ownerObjectId"] = owner;
        failures.push((steps.len(), message));
        steps.push(step);
    }
    let mut parent_owned = create("parent-owned", "child-of-owned", "fixture/json");
    bind(&mut parent_owned, "/params/create/parentId", owned_index);
    failures.push((steps.len(), "Owned artifacts cannot be navigation parents."));
    steps.push(parent_owned);
    let mut owner_owned = create("owner-owned", "owned-by-owned", "fixture/owned");
    bind(
        &mut owner_owned,
        "/params/create/ownerObjectId",
        owned_index,
    );
    failures.push((
        steps.len(),
        "The lifecycle owner must be a retained Object.",
    ));
    steps.push(owner_owned);
    let mut duplicate = create("duplicate", "text", "fixture/json");
    duplicate["params"]["create"]["ownerObjectId"] = "missing".into();
    failures.push((steps.len(), "An Object already uses that sibling name."));
    steps.push(duplicate);
    let mut bad_content = create("bad-content", "", "fixture/json");
    bad_content["params"]["content"]["value"] = json!({"n":"wrong"});
    failures.push((steps.len(), "Content does not satisfy its exact contract."));
    steps.push(bad_content);
    let forbidden = create("hive-forbidden", "", "fixture/hive");
    failures.push((
        steps.len(),
        "Hive-owned Objects can only be changed through their dedicated operation.",
    ));
    steps.push(forbidden);
    let mut internal = create("internal-owner", "", "fixture/json");
    internal["mode"] = "hive".into();
    internal["method"] = "fixture.internal".into();
    failures.push((
        steps.len(),
        "Internal Hive writes require a Hive-owned contract.",
    ));
    steps.push(internal);
    let mut chain = "BEGIN IMMEDIATE;".to_owned();
    let mut path = "/json".to_owned();
    let mut parent = "json".to_owned();
    for depth in 2..=64 {
        let id = format!("depth{depth}");
        path.push_str(&format!("/{id}"));
        chain.push_str(&format!("INSERT INTO objects VALUES(NULL,'{id}','{parent}',NULL,'{id}','{path}',0,NULL,{depth},'fixture/json',1,NULL,0,'now','now');INSERT INTO revisions VALUES('{id}',1,'fixture/json','1.0.0','json','{{\"n\":0}}','sha256:fixture',7,'now','{{}}');"));
        parent = id;
    }
    chain.push_str("COMMIT;");
    steps.push(super::sql(chain));
    let mut deep = create("too-deep", "deep", "fixture/json");
    deep["params"]["create"]["parentId"] = "depth64".into();
    deep["params"]["create"]["ownerObjectId"] = "missing".into();
    failures.push((steps.len(), "Object hierarchy exceeds its maximum depth."));
    steps.push(deep);
    steps.push(super::sql(
        "UPDATE objects SET position=9007199254740990 WHERE id='text';",
    ));
    let maximum = steps.len();
    steps.push(create("max-position", "last-position", "fixture/json"));
    let mut overflow = create("position-overflow", "overflow", "fixture/json");
    overflow["params"]["create"]["icon"] = "\n".into();
    failures.push((steps.len(), "Object sibling position limit reached."));
    steps.push(overflow);
    let result = compare(&steps);
    for (index, message) in failures {
        error(&result, index, message);
    }
    assert_eq!(
        result["rows"][maximum]["value"]["object"]["position"],
        json!(9007199254740991u64)
    );
}

#[test]
fn renaming_refreshes_cross_contract_descendants_and_replays_saved_paths_after_later_edits() {
    let mut steps = super::setup();
    let root = steps.len();
    steps.push(create("rename-root", "Cafe\u{301}", "fixture/json"));
    let child = steps.len();
    let mut nested = create("rename-child", "child", "fixture/json");
    bind(&mut nested, "/params/create/parentId", root);
    steps.push(nested);
    let grandchild = steps.len();
    let mut leaf = create("rename-leaf", "leaf", "fixture/hive");
    leaf["mode"] = "hive".into();
    leaf["method"] = "fixture.internal".into();
    bind(&mut leaf, "/params/create/parentId", child);
    steps.push(leaf);
    let mut same = super::write("same-name", "bound", 1, "json", json!({"n":2}), json!({}));
    same["params"]["name"] = "Cafe\u{301}".into();
    same["overdue"] = true.into();
    bind(&mut same, "/params/objectId", root);
    let equal = steps.len();
    steps.push(same.clone());
    let mut rename = super::write("rename", "bound", 2, "json", json!({"n":3}), json!({}));
    rename["params"]["name"] = "Ne\u{301}w".into();
    bind(&mut rename, "/params/objectId", root);
    let renamed = steps.len();
    steps.push(rename);
    steps.push(json!({"op":"reopen"}));
    let replay = steps.len();
    steps.push(same);
    let mut duplicate = super::write(
        "rename-conflict",
        "bound",
        3,
        "json",
        json!({"n":4}),
        json!({}),
    );
    duplicate["params"]["name"] = "text".into();
    bind(&mut duplicate, "/params/objectId", root);
    let conflict = steps.len();
    steps.push(duplicate);
    let mut timeout = super::write(
        "rename-timeout",
        "bound",
        3,
        "json",
        json!({"n":4}),
        json!({}),
    );
    timeout["params"]["name"] = "Timed".into();
    timeout["overdue"] = true.into();
    bind(&mut timeout, "/params/objectId", root);
    let deadline = steps.len();
    steps.push(timeout.clone());
    timeout["overdue"] = false.into();
    let retry = steps.len();
    steps.push(timeout);
    let result = compare(&steps);
    assert!(result["rows"][equal]["error"].is_null());
    assert!(result["rows"][renamed]["error"].is_null());
    let leaf_id = result["rows"][grandchild]["value"]["object"]["id"]
        .as_str()
        .unwrap();
    assert_eq!(object(&result, renamed, leaf_id)["path"], "/Néw/child/leaf");
    assert_eq!(object(&result, renamed, leaf_id)["current_revision"], 1);
    assert_eq!(
        object(&result, renamed, leaf_id)["updated_at"],
        object(&result, grandchild, leaf_id)["updated_at"]
    );
    assert_eq!(
        result["rows"][equal]["value"],
        result["rows"][replay]["value"]
    );
    error(
        &result,
        conflict,
        "An Object already uses that sibling name.",
    );
    assert_eq!(code(&result, deadline), "limit_exceeded");
    error(
        &result,
        deadline,
        "Database operation exceeded its work budget.",
    );
    assert!(result["rows"][retry]["error"].is_null());
}

#[test]
fn creation_and_rename_failures_roll_back_all_rows_receipts_events_and_navigation() {
    let mut steps = super::setup();
    let mut failed = vec![];
    let mut succeeded = vec![];
    for (index, (table, event)) in [
        ("objects", "INSERT"),
        ("revisions", "INSERT"),
        ("retention_families", "UPDATE"),
        ("revision_references", "INSERT"),
        ("events", "INSERT"),
        ("mutations", "INSERT"),
    ]
    .iter()
    .enumerate()
    {
        steps.push(super::sql(format!("CREATE TRIGGER create_abort BEFORE {event} ON {table} BEGIN SELECT RAISE(ABORT,'fixture create rollback'); END;")));
        let mut action = create(
            &format!("create-rollback-{index}"),
            &format!("rollback-{index}"),
            "fixture/json",
        );
        action["params"]["references"] = json!({"text":{"objectId":"text","revision":1}});
        failed.push(steps.len());
        steps.push(action.clone());
        steps.push(super::sql("DROP TRIGGER create_abort;"));
        succeeded.push(steps.len());
        steps.push(action);
    }
    steps.push(super::sql("CREATE TRIGGER create_commit AFTER INSERT ON mutations BEGIN UPDATE objects SET current_revision=2 WHERE id=json_extract(NEW.result_json,'$.object.id'); END;"));
    let action = create("create-commit", "commit", "fixture/json");
    failed.push(steps.len());
    steps.push(action.clone());
    steps.push(super::sql("DROP TRIGGER create_commit;"));
    let root = steps.len();
    succeeded.push(root);
    steps.push(action);
    let mut child = create("rename-failure-child", "child", "fixture/json");
    bind(&mut child, "/params/create/parentId", root);
    steps.push(child);
    steps.push(super::sql("CREATE TRIGGER nav_abort BEFORE UPDATE OF path ON objects WHEN NEW.parent_id IS NOT NULL BEGIN SELECT RAISE(ABORT,'fixture navigation rollback'); END;"));
    let mut rename = super::write(
        "rename-failure",
        "bound",
        1,
        "json",
        json!({"n":9}),
        json!({}),
    );
    rename["params"]["name"] = "rollback-rename".into();
    bind(&mut rename, "/params/objectId", root);
    failed.push(steps.len());
    steps.push(rename.clone());
    steps.push(super::sql("DROP TRIGGER nav_abort;"));
    succeeded.push(steps.len());
    steps.push(rename);
    let result = compare(&steps);
    for index in failed {
        assert_eq!(code(&result, index), "internal_error");
        assert_eq!(
            result["rows"][index]["state"],
            result["rows"][index - 1]["state"]
        );
    }
    for index in succeeded {
        assert!(result["rows"][index]["error"].is_null(), "retry {index}");
    }
}

#[test]
fn prepared_hive_creation_replay_precedes_preparation_and_contract_keys_keep_sql_utf16_replacement()
{
    let mut steps = super::setup();
    let mut prepared = super::super::object_mutations::action(
        "write",
        "prepared-create",
        json!({"action":"create"}),
    );
    prepared["mode"] = "prepared".into();
    prepared["method"] = "fixture.internal".into();
    prepared["preparedParams"] =
        create("prepared-create", "internal-created", "fixture/hive")["params"].clone();
    prepared["prepareSql"] = "INSERT INTO metadata VALUES('fixture_create_prepare','one');".into();
    let first = steps.len();
    steps.push(prepared.clone());
    let mut edit = super::write(
        "later-internal",
        "bound",
        1,
        "text",
        json!("later"),
        json!({}),
    );
    edit["mode"] = "hive".into();
    edit["method"] = "fixture.internal".into();
    bind(&mut edit, "/params/objectId", first);
    steps.push(edit);
    steps.push(json!({"op":"reopen"}));
    prepared["prepareFail"] = true.into();
    prepared["preparedParams"]["create"]["name"] = "changed prepared input".into();
    let replay = steps.len();
    steps.push(prepared);
    let mut definition = super::definition("placeholder", "1.0.0", "application/json", false);
    definition["definitionUnits"] = json!({"key":[0xd800]});
    definition["lossless"] = true.into();
    steps.push(definition);
    let mut raw = create("raw-key", "raw-key", "placeholder");
    raw["parameterUnits"] = json!({"/create/contractKey":[0xd800]});
    let raw_key = steps.len();
    steps.push(raw);
    let mut legacy_errors = vec![];
    for (index, member) in [
        "owner-missing",
        "owner-null",
        "retention-missing",
        "retention-null",
        "objects-missing",
        "objects-null",
    ]
    .iter()
    .enumerate()
    {
        let version = format!("{}.0.0", 20 + index);
        let mut definition = super::definition("fixture/json", &version, "application/json", false)
            ["definition"]
            .clone();
        match *member {
            "owner-missing" => {
                definition.as_object_mut().unwrap().remove("owner");
            }
            "owner-null" => definition["owner"] = Value::Null,
            "retention-missing" => {
                definition.as_object_mut().unwrap().remove("retention");
            }
            "retention-null" => definition["retention"] = Value::Null,
            "objects-missing" => {
                definition["retention"]
                    .as_object_mut()
                    .unwrap()
                    .remove("objects");
            }
            "objects-null" => definition["retention"]["objects"] = Value::Null,
            _ => unreachable!(),
        }
        steps.push(super::sql(format!(
            "INSERT INTO contracts VALUES('fixture/json','{version}','{}');",
            definition.to_string().replace('\'', "''")
        )));
        let mut step = create(member, member, "fixture/json");
        step["params"]["contractVersion"] = version.into();
        legacy_errors.push(steps.len());
        steps.push(step);
    }
    let result = compare(&steps);
    assert!(result["rows"][first]["error"].is_null());
    assert_eq!(
        result["rows"][first]["value"],
        result["rows"][replay]["value"]
    );
    assert!(result["rows"][raw_key]["error"].is_null());
    assert_eq!(
        result["rows"][raw_key]["value"]["object"]["contractKey"],
        "�"
    );
    for index in legacy_errors {
        assert_eq!(code(&result, index), "internal_error");
        assert_eq!(
            result["rows"][index]["state"],
            result["rows"][index - 1]["state"]
        );
    }
}
