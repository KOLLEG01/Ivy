use super::{AuthContext, AuthenticatedContext, IvyError, IvyJson, JsString, Store};
use ivy_contracts::IvyResult;
use ivy_hive::objects;
use serde_json::{Value, json};
use std::time::Duration;
#[path = "hive_object_create.rs"]
mod creation;

pub(super) fn bind(step: &mut Value, rows: &[Value]) {
    for binding in step["bindings"].as_array().unwrap().clone() {
        let id = rows[binding["from"].as_u64().unwrap() as usize]["value"]["object"]["id"].clone();
        assert!(
            id.is_string(),
            "Binding must refer to a successful Object write"
        );
        *step
            .pointer_mut(binding["at"].as_str().unwrap())
            .expect("Existing binding destination") = id;
    }
}

pub(super) fn native_write(store: &mut Store, step: &Value) -> IvyResult<IvyJson> {
    let principal = store.authenticate(&AuthContext {
        credential_digest: step["context"]["credentialDigest"].as_str().unwrap().into(),
        session_digest: step["context"]["sessionDigest"]
            .as_str()
            .map(JsString::from),
    })?;
    let context = AuthenticatedContext {
        principal_id: principal.principal_id,
        credential_digest: principal.credential_digest,
    };
    let parameters = IvyJson::from_serde(&step["params"]).unwrap();
    let mode = step["mode"].as_str().unwrap_or("ordinary");
    let method = match mode {
        "hive" | "prepared" => step["method"].as_str().unwrap(),
        "preserve" => "objects.updateContent",
        _ => "objects.write",
    };
    let now: JsString = step["nowText"].as_str().unwrap().into();
    let mut request = parameters.clone();
    // Ordinary writes include the original content in their receipt hash.
    if mode != "prepared" {
        apply_units(&mut request, step);
    }
    let changes = store.connection().total_changes();
    let result = store.with_budget_mut(
        if step["overdue"] == true {
            Duration::ZERO
        } else {
            Duration::from_secs(10)
        },
        |store| {
            if mode == "prepared" {
                objects::write_hive_prepared(
                    store,
                    &context,
                    &method.into(),
                    &request,
                    step["now"].as_u64().unwrap(),
                    &now,
                    |store| {
                        store
                            .connection()
                            .execute_batch(step["prepareSql"].as_str().unwrap_or(""))
                            .map_err(|e| store.query_error(e))?;
                        if step["prepareFail"] == true {
                            return Err(IvyError::new(
                                "fixture_failure",
                                "Preparation requested failure.",
                            ));
                        }
                        let mut prepared = IvyJson::from_serde(&step["preparedParams"]).unwrap();
                        apply_units(&mut prepared, step);
                        Ok(prepared)
                    },
                )
            } else if mode == "hive" {
                objects::write_hive(
                    store,
                    &context,
                    &method.into(),
                    &request,
                    step["now"].as_u64().unwrap(),
                    &now,
                )
            } else {
                objects::write(
                    store,
                    &context,
                    &request,
                    mode == "preserve",
                    step["now"].as_u64().unwrap(),
                    &now,
                )
            }
        },
    )?;
    Ok(if step["watchChanges"] == true {
        IvyJson::object([
            ("result", result),
            (
                "changes",
                IvyJson::Number((store.connection().total_changes() - changes) as f64),
            ),
        ])
    } else {
        result
    })
}
pub(super) fn apply_units(parameters: &mut IvyJson, step: &Value) {
    fn set(value: &mut IvyJson, fields: &[&str], next: IvyJson) {
        if fields.len() == 1 {
            value.insert(fields[0], next);
        } else {
            let mut child = value.get(fields[0]).unwrap().clone();
            set(&mut child, &fields[1..], next);
            value.insert(fields[0], child);
        }
    }
    for (pointer, units) in step["parameterUnits"].as_object().into_iter().flatten() {
        set(
            parameters,
            &pointer.split('/').skip(1).collect::<Vec<_>>(),
            IvyJson::String(JsString(
                units
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_u64().unwrap() as u16)
                    .collect(),
            )),
        );
    }
    if let Some(units) = step["contentUnits"].as_array() {
        let mut content = parameters.get("content").unwrap().clone();
        content.insert(
            "value",
            IvyJson::String(JsString(
                units.iter().map(|v| v.as_u64().unwrap() as u16).collect(),
            )),
        );
        parameters.insert("content", content);
    }
}
fn sql(text: impl Into<String>) -> Value {
    json!({"op":"sql","sql":text.into()})
}
fn definition(key: &str, version: &str, media: &str, hive: bool) -> Value {
    let mut value = json!({"key":key,"version":version,"owner":{"kind":if hive {"hive"} else {"agent"}},"mediaType":media,"retention":{"objects":{"mode":"retain"},"revisions":{"mode":"all"}},"specMarkdown":"synthetic immutable content fixture"});
    if media == "application/json" {
        value["jsonSchema"] = json!({"type":"object","required":["n"],"properties":{"n":{"type":"integer"}},"additionalProperties":false});
    }
    json!({"op":"register","owner":value["owner"],"definition":value})
}
pub(super) fn setup() -> Vec<Value> {
    let mut steps = vec![
        sql("UPDATE metadata SET value='epoch' WHERE key='runtime_epoch';"),
        json!({"op":"configure","credentials":[{"principalId":"alpha","digest":format!("sha256:{}","a".repeat(64))},{"principalId":"beta","digest":format!("sha256:{}","b".repeat(64))}]}),
    ];
    for (key, media, hive) in [
        ("fixture/json", "application/json", false),
        ("fixture/text", "text/plain", false),
        ("fixture/blob", "application/octet-stream", false),
        ("fixture/hive", "text/plain", true),
    ] {
        for version in ["1.0.0", "2.0.0", "10.0.0"] {
            steps.push(definition(key, version, media, hive));
        }
    }
    let mut seed = "BEGIN IMMEDIATE;".to_owned();
    for (id, key, encoding, data, length) in [
        ("json", "fixture/json", "json", "'{\"n\":0}'", 7),
        ("text", "fixture/text", "text", "'before'", 6),
        ("blob", "fixture/blob", "base64", "X'0001FEFF'", 4),
        ("hive", "fixture/hive", "text", "'internal'", 8),
    ] {
        seed.push_str(&format!("INSERT INTO objects VALUES(NULL,'{id}',NULL,NULL,'{id}','/{id}',0,NULL,1,'{key}',1,NULL,0,'2026-01-01','2026-01-02'); INSERT INTO revisions VALUES('{id}',1,'{key}','1.0.0','{encoding}',{data},'sha256:fixture',{length},'2026-01-02','{{}}'); INSERT INTO object_fts(rowid,object_id,name,text) SELECT search_id,id,name,{} FROM objects WHERE id='{id}';", if encoding == "text" { data } else { "''" }));
        seed.push_str(&format!("INSERT INTO retention_families VALUES('{key}','fixture-policy',0,'{{\"objectCount\":1,\"revisionCount\":1,\"byteLength\":{length}}}');"));
    }
    seed.push_str("COMMIT;");
    steps.push(sql(seed));
    steps
}
fn write(
    nonce: &str,
    id: &str,
    expected: u64,
    encoding: &str,
    content: Value,
    references: Value,
) -> Value {
    super::object_mutations::action(
        "write",
        nonce,
        json!({"objectId":id,"expectedRevision":expected,"contractVersion":"1.0.0","content":{"encoding":encoding,"value":content},"references":references}),
    )
}
fn compare(steps: &[Value]) -> Value {
    super::object_mutations::compare(steps)
}
fn code(result: &Value, index: usize) -> &Value {
    &result["rows"][index]["error"]["data"]["code"]
}
fn read(id: &str, revision: u64) -> Value {
    json!({"op":"readObject","objectId":id,"revision":revision})
}

#[test]
fn immutable_writes_keep_content_types_exact_links_fts_identity_and_saved_receipts() {
    let mut steps = setup();
    let first = steps.len();
    let json_write = write(
        "json",
        "json",
        1,
        "json",
        json!({"n":1}),
        json!({"text":{"objectId":"text","revision":1},"blob":{"objectId":"blob","revision":1}}),
    );
    steps.push(json_write.clone());
    let mut next = write("new-version", "json", 2, "json", json!({"n":2}), json!({}));
    next["params"]["contractVersion"] = "10.0.0".into();
    steps.push(next);
    let replay = steps.len();
    steps.push(json_write.clone());
    let mut conflict = json_write.clone();
    conflict["params"]["content"]["value"] = json!({"n":10});
    steps.push(conflict);
    steps.push(write(
        "text",
        "text",
        1,
        "text",
        json!("after café 🐝\nsecond line"),
        json!({"json":{"objectId":"json","revision":2}}),
    ));
    steps.push(write(
        "blob",
        "blob",
        1,
        "base64",
        json!("AAECA//+"),
        json!({}),
    ));
    let history = steps.len();
    steps.extend([
        read("json", 1),
        read("json", 2),
        read("text", 2),
        read("blob", 2),
    ]);
    steps.push(json!({"op":"reopen"}));
    let restart = steps.len();
    steps.push(json_write);
    let result = compare(&steps);
    assert!(result["rows"][first]["error"].is_null());
    assert_eq!(
        result["rows"][replay]["value"],
        result["rows"][first]["value"]
    );
    assert_eq!(
        result["rows"][restart]["value"],
        result["rows"][first]["value"]
    );
    assert_eq!(code(&result, replay + 1), "mutation_conflict");
    assert_eq!(
        result["rows"][history]["value"]["content"]["value"],
        json!({"n":0})
    );
    assert_eq!(
        result["rows"][history + 1]["value"]["revision"]["references"]["text"]["revision"],
        1
    );
    assert_eq!(
        result["rows"][history + 3]["value"]["content"]["value"],
        "AAECA//+"
    );
    let before = &result["rows"][first - 1]["state"]["fts"];
    let after = &result["rows"][restart]["state"]["fts"];
    for row in before.as_array().unwrap() {
        assert_eq!(
            after
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["object_id"] == row["object_id"])
                .unwrap()["rowid"],
            row["rowid"]
        );
    }
    let retention = result["rows"][restart]["state"]["retention"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["contract_key"] == "fixture/json")
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(retention["summary_json"].as_str().unwrap()).unwrap(),
        json!({"objectCount":1,"revisionCount":3,"byteLength":21})
    );
}

#[test]
fn write_guards_preserve_cas_archive_contract_owner_version_and_content_error_order() {
    let mut steps = setup();
    let start = steps.len();
    let mut absent = write("absent", "absent", 9, "text", json!("bad"), json!({}));
    absent["params"]["contractVersion"] = "8.0.0".into();
    steps.push(absent);
    let mut stale = write("stale", "json", 9, "text", json!("bad"), json!({}));
    stale["params"]["expectedArchived"] = true.into();
    stale["params"]["contractVersion"] = "8.0.0".into();
    steps.push(stale);
    let mut archive = write("archive", "text", 1, "text", json!("changed"), json!({}));
    archive["params"]["expectedArchived"] = true.into();
    archive["params"]["contractVersion"] = "8.0.0".into();
    steps.push(archive);
    let mut unknown = write("unknown", "text", 1, "base64", json!("bad"), json!({}));
    unknown["params"]["contractVersion"] = "8.0.0".into();
    steps.push(unknown);
    steps.push(write(
        "hive-forbidden",
        "hive",
        1,
        "base64",
        json!("bad"),
        json!({}),
    ));
    let mut wrong_owner = write("wrong-owner", "text", 1, "base64", json!("bad"), json!({}));
    wrong_owner["mode"] = "hive".into();
    wrong_owner["method"] = "fixture.internal".into();
    steps.push(wrong_owner);
    let mut upgrade = write("upgrade", "json", 1, "json", json!({"n":2}), json!({}));
    upgrade["params"]["contractVersion"] = "10.0.0".into();
    steps.push(upgrade);
    let mut downgrade = write("downgrade", "json", 2, "text", json!("bad"), json!({}));
    downgrade["params"]["contractVersion"] = "2.0.0".into();
    steps.push(downgrade);
    let mut schema = write(
        "schema",
        "json",
        2,
        "json",
        json!({"n":"bad"}),
        json!({"bad name":{"objectId":"absent","revision":1}}),
    );
    schema["params"]["contractVersion"] = "10.0.0".into();
    steps.push(schema);
    steps.push(write("encoding", "text", 1, "json", json!({}), Value::Null));
    let mut unicode = write("unicode", "text", 1, "text", json!("replaced"), json!({}));
    unicode["contentUnits"] = json!([0xd800]);
    steps.push(unicode);
    steps.push(sql(
        "UPDATE objects SET archived_at='archived',effective_archive=1 WHERE id='text';",
    ));
    let mut archived = write(
        "archived",
        "text",
        1,
        "text",
        json!("archived update"),
        json!({}),
    );
    archived["params"]["expectedArchived"] = true.into();
    steps.push(archived);
    let result = compare(&steps);
    for (offset, expected) in [
        (0, "not_found"),
        (1, "revision_conflict"),
        (2, "revision_conflict"),
        (3, "contract_not_found"),
        (4, "forbidden"),
        (5, "contract_owner_mismatch"),
        (7, "contract_version_conflict"),
        (8, "invalid_arguments"),
        (9, "invalid_arguments"),
        (10, "invalid_arguments"),
    ] {
        assert_eq!(code(&result, start + offset), expected, "guard {offset}");
    }
    assert_eq!(
        result["rows"][start + 2]["error"]["message"],
        "Object archive state changed; read the current state."
    );
    assert_eq!(
        result["rows"][start + 8]["error"]["message"],
        "Content does not satisfy its exact contract."
    );
    assert!(result["rows"].as_array().unwrap().last().unwrap()["error"].is_null());
    for offset in 0..11 {
        if offset != 6 {
            assert_eq!(
                result["rows"][start + offset]["state"],
                result["rows"][start + offset - 1]["state"],
                "Failed guard must roll back"
            );
        }
    }
}

#[test]
fn exact_reference_boundaries_validate_all_targets_and_roll_back_the_object_head() {
    let mut steps = setup();
    let start = steps.len();
    let valid = json!({"objectId":"blob","revision":1});
    let mut many = serde_json::Map::new();
    for i in 0..257 {
        many.insert(format!("r{i}"), valid.clone());
    }
    let cases = vec![
        (Value::Null, "internal_error"),
        (json!([]), "limit_exceeded"),
        (Value::Object(many), "limit_exceeded"),
        (json!({"0":valid}), "invalid_arguments"),
        (json!({"bad/name":valid}), "invalid_arguments"),
        (
            json!({"long":{"objectId":"x".repeat(257),"revision":1}}),
            "invalid_arguments",
        ),
        (
            json!({"self":{"objectId":"text","revision":1}}),
            "invalid_arguments",
        ),
        (
            json!({"missing":{"objectId":"absent","revision":1}}),
            "not_found",
        ),
        (
            json!({"future":{"objectId":"blob","revision":2}}),
            "not_found",
        ),
        (
            json!({"extra":{"objectId":"blob","revision":1,"extra":0}}),
            "invalid_arguments",
        ),
        (
            json!({"zero":{"objectId":"blob","revision":0}}),
            "invalid_arguments",
        ),
        (
            json!({"fraction":{"objectId":"blob","revision":1.5}}),
            "invalid_arguments",
        ),
        (
            json!({"unsafe":{"objectId":"blob","revision":9007199254740992u64}}),
            "invalid_arguments",
        ),
        (json!({"array":["blob",1]}), "invalid_arguments"),
    ];
    for (index, (refs, _)) in cases.iter().enumerate() {
        steps.push(write(
            &format!("ref-{index}"),
            "text",
            1,
            "text",
            json!("after"),
            refs.clone(),
        ));
    }
    let mut exact = serde_json::Map::new();
    for i in 0..256 {
        exact.insert(format!("r{i}"), valid.clone());
    }
    steps.push(write(
        "exact256",
        "text",
        1,
        "text",
        json!("after"),
        Value::Object(exact),
    ));
    // Large, valid references hit the separate canonical byte ceiling only
    // after every exact revision has been checked.
    let target = "🐝".repeat(128);
    steps.push(sql(format!("BEGIN IMMEDIATE; INSERT INTO objects VALUES(NULL,'{target}',NULL,NULL,'long','/long',0,NULL,1,'fixture/blob',1,NULL,0,'now','now'); INSERT INTO revisions VALUES('{target}',1,'fixture/blob','1.0.0','base64',X'','sha256:fixture',0,'now','{{}}'); COMMIT;")));
    let mut huge = serde_json::Map::new();
    for i in 0..256 {
        huge.insert(
            format!("r{:0>127}", i),
            json!({"objectId":target,"revision":1}),
        );
    }
    let large = steps.len();
    steps.push(write(
        "large-refs",
        "text",
        2,
        "text",
        json!("huge"),
        Value::Object(huge),
    ));
    let receipt_target = "x".repeat(128);
    steps.push(sql(format!("BEGIN IMMEDIATE; INSERT INTO objects VALUES(NULL,'{receipt_target}',NULL,NULL,'receipt-target','/receipt-target',0,NULL,1,'fixture/blob',1,NULL,0,'now','now'); INSERT INTO revisions VALUES('{receipt_target}',1,'fixture/blob','1.0.0','base64',X'','sha256:fixture',0,'now','{{}}'); COMMIT;")));
    let mut receipt_refs = serde_json::Map::new();
    for i in 0..256 {
        receipt_refs.insert(
            format!("r{:0>127}", i),
            json!({"objectId":receipt_target,"revision":1}),
        );
    }
    let receipt_limit = steps.len();
    steps.push(write(
        "receipt-limit",
        "text",
        2,
        "text",
        json!("receipt"),
        Value::Object(receipt_refs),
    ));
    let mut max = write(
        "max-revision",
        "blob",
        9007199254740991,
        "base64",
        json!(""),
        json!({"self":{"objectId":"blob","revision":1}}),
    );
    // Persist an immutable maximum-number revision and select it as the head;
    // the original revision remains pinned by the existing reference graph.
    steps.push(sql("BEGIN IMMEDIATE; INSERT INTO revisions SELECT object_id,9007199254740991,contract_key,contract_version,encoding,content,content_hash,byte_length,created_at,references_json FROM revisions WHERE object_id='blob' AND revision=1; UPDATE objects SET current_revision=9007199254740991 WHERE id='blob'; COMMIT;"));
    max["params"]["expectedArchived"] = false.into();
    let maximum = steps.len();
    steps.push(max);
    let result = compare(&steps);
    for (offset, (_, expected)) in cases.iter().enumerate() {
        assert_eq!(
            code(&result, start + offset),
            *expected,
            "reference case {offset}"
        );
        assert_eq!(
            result["rows"][start + offset]["state"],
            result["rows"][start - 1]["state"]
        );
    }
    assert!(result["rows"][start + cases.len()]["error"].is_null());
    assert_eq!(code(&result, large), "content_too_large");
    assert_eq!(code(&result, receipt_limit), "content_too_large");
    for index in [large, receipt_limit] {
        assert_eq!(
            result["rows"][index]["state"],
            result["rows"][index - 1]["state"]
        );
    }
    assert_eq!(code(&result, maximum), "limit_exceeded");
    assert_eq!(
        result["rows"][maximum]["error"]["message"],
        "Revision limit reached."
    );
}

#[test]
fn preserved_links_and_prepared_hive_inputs_are_resolved_only_after_receipt_replay() {
    let mut steps = setup();
    let first = write(
        "linked",
        "text",
        1,
        "text",
        json!("first"),
        json!({"blob":{"objectId":"blob","revision":1}}),
    );
    steps.push(first);
    let mut preserve = write(
        "preserve",
        "text",
        2,
        "text",
        json!("markdown edit"),
        json!(["ignored"]),
    );
    preserve["mode"] = "preserve".into();
    let saved = steps.len();
    steps.push(preserve.clone());
    steps.push(write(
        "change-links",
        "text",
        3,
        "text",
        json!("later"),
        json!({"json":{"objectId":"json","revision":1}}),
    ));
    steps.push(sql("DELETE FROM revision_references WHERE source_object_id='text' AND source_revision IN (2,3); DELETE FROM revisions WHERE object_id='text' AND revision IN (2,3);"));
    steps.push(json!({"op":"reopen"}));
    let replay = steps.len();
    steps.push(preserve);
    let mut prepared =
        super::object_mutations::action("write", "prepared", json!({"action":"set-content"}));
    prepared["mode"] = "prepared".into();
    prepared["method"] = "fixture.hiveWrite".into();
    prepared["preparedParams"] = write(
        "prepared",
        "hive",
        1,
        "text",
        json!("prepared content"),
        json!({}),
    )["params"]
        .clone();
    prepared["prepareSql"] = "INSERT INTO metadata VALUES('fixture_prepared','one');".into();
    let prepared_saved = steps.len();
    steps.push(prepared.clone());
    steps.push(sql(
        "DELETE FROM revisions WHERE object_id='hive' AND revision=1;",
    ));
    steps.push(json!({"op":"reopen"}));
    prepared["prepareFail"] = true.into();
    prepared["preparedParams"]["expectedRevision"] = 999.into();
    let prepared_replay = steps.len();
    steps.push(prepared);
    let mut failed =
        super::object_mutations::action("write", "prepare-fail", json!({"action":"failure"}));
    failed["mode"] = "prepared".into();
    failed["method"] = "fixture.hiveWrite".into();
    failed["prepareFail"] = true.into();
    failed["prepareSql"] =
        "UPDATE metadata SET value='failed' WHERE key='fixture_prepared';".into();
    let preparation_failure = steps.len();
    steps.push(failed);
    // An ordinary write also retains its snapshot after the object is removed.
    let deleted = write("deleted", "blob", 1, "base64", json!("AA=="), json!({}));
    let before_delete = steps.len();
    steps.push(deleted.clone());
    steps.push(sql("BEGIN IMMEDIATE; DELETE FROM revision_references WHERE target_object_id='blob' OR source_object_id='blob'; DELETE FROM revisions WHERE object_id='blob'; DELETE FROM objects WHERE id='blob'; DELETE FROM object_fts WHERE object_id='blob'; COMMIT;"));
    steps.push(json!({"op":"reopen"}));
    let after_delete = steps.len();
    steps.push(deleted);
    let result = compare(&steps);
    assert_eq!(
        result["rows"][saved]["value"]["revision"]["references"],
        json!({"blob":{"objectId":"blob","revision":1}})
    );
    assert_eq!(
        result["rows"][saved]["value"],
        result["rows"][replay]["value"]
    );
    assert_eq!(
        result["rows"][prepared_saved]["value"],
        result["rows"][prepared_replay]["value"]
    );
    assert_eq!(code(&result, preparation_failure), "fixture_failure");
    assert_eq!(
        result["rows"][preparation_failure]["state"],
        result["rows"][preparation_failure - 1]["state"]
    );
    assert_eq!(
        result["rows"][before_delete]["value"],
        result["rows"][after_delete]["value"]
    );
}

#[test]
fn content_reference_retention_event_and_receipt_failures_roll_back_the_whole_write() {
    let mut steps = setup();
    let mut failed_indices = vec![];
    let mut success_indices = vec![];
    let mut revision = 1;
    for (index, (table, event)) in [
        ("objects", "UPDATE OF current_revision"),
        ("revisions", "INSERT"),
        ("retention_families", "UPDATE"),
        ("revision_references", "INSERT"),
        ("events", "INSERT"),
        ("mutations", "INSERT"),
    ]
    .iter()
    .enumerate()
    {
        steps.push(sql(format!("CREATE TRIGGER fixture_abort BEFORE {event} ON {table} BEGIN SELECT RAISE(ABORT,'fixture rollback'); END;")));
        let action = write(
            &format!("rollback-{index}"),
            "text",
            revision,
            "text",
            json!(format!("stage {index}")),
            json!({"blob":{"objectId":"blob","revision":1}}),
        );
        failed_indices.push(steps.len());
        steps.push(action.clone());
        steps.push(sql("DROP TRIGGER fixture_abort;"));
        success_indices.push(steps.len());
        steps.push(action);
        revision += 1;
    }
    // A deferred constraint can fail COMMIT after the work and receipt inserts.
    steps.push(sql("CREATE TRIGGER fixture_commit AFTER INSERT ON mutations BEGIN UPDATE objects SET current_revision=current_revision+1 WHERE id='text'; END;"));
    let commit = write(
        "commit-fail",
        "text",
        revision,
        "text",
        json!("commit"),
        json!({}),
    );
    let commit_failure = steps.len();
    steps.push(commit.clone());
    steps.push(sql("DROP TRIGGER fixture_commit;"));
    let commit_retry = steps.len();
    steps.push(commit);
    let mut budget = write(
        "budget",
        "text",
        revision + 1,
        "text",
        json!("deadline"),
        json!({}),
    );
    steps.push(sql(
        "CREATE TRIGGER fixture_budget BEFORE INSERT ON revisions BEGIN SELECT ivy_budget(); END;",
    ));
    budget["overdue"] = true.into();
    let deadline = steps.len();
    steps.push(budget);
    let result = compare(&steps);
    for i in failed_indices.into_iter().chain([commit_failure, deadline]) {
        assert!(!result["rows"][i]["error"].is_null());
        assert_eq!(
            result["rows"][i]["state"],
            result["rows"][i - 1]["state"],
            "write failure {i}"
        );
    }
    for i in success_indices.into_iter().chain([commit_retry]) {
        assert!(result["rows"][i]["error"].is_null(), "retry {i}");
    }
    assert_eq!(code(&result, deadline), "limit_exceeded");
}

#[test]
fn unchanged_text_avoids_extra_fts_changes_and_missing_rows_keep_the_search_identity() {
    let mut steps = setup();
    let unchanged = steps.len();
    let mut same = write("same-text", "text", 1, "text", json!("before"), json!({}));
    same["watchChanges"] = true.into();
    steps.push(same);
    let mut different = write(
        "different-text",
        "text",
        2,
        "text",
        json!("new searchable value"),
        json!({}),
    );
    different["watchChanges"] = true.into();
    let changed = steps.len();
    steps.push(different);
    steps.push(sql("DELETE FROM object_fts WHERE object_id='text';"));
    let restored = steps.len();
    steps.push(write(
        "restore-fts",
        "text",
        3,
        "text",
        json!("restored value"),
        json!({}),
    ));
    let result = compare(&steps);
    for index in [unchanged, changed, restored] {
        assert!(result["rows"][index]["error"].is_null());
    }
    assert!(
        result["rows"][changed]["value"]["changes"]
            .as_u64()
            .unwrap()
            > result["rows"][unchanged]["value"]["changes"]
                .as_u64()
                .unwrap()
    );
    let initial = result["rows"][unchanged]["state"]["fts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["object_id"] == "text")
        .unwrap();
    let final_row = result["rows"][restored]["state"]["fts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["object_id"] == "text")
        .unwrap();
    assert_eq!(initial["rowid"], final_row["rowid"]);
    assert_eq!(final_row["text"], "restored value");
}
