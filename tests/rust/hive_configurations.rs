use super::*;
use ivy_hive::{configurations::Configurations, objects};

fn text<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a JsString> {
    value
        .get(key)
        .and_then(IvyJson::as_js_string)
        .ok_or_else(IvyError::internal)
}
pub(super) fn execute(
    owner: &Configurations,
    store: &mut Store,
    runtime: &ObservedRuntime,
    params: &IvyJson,
    step: &Value,
) -> IvyResult<IvyJson> {
    let method = step["method"].as_str().unwrap();
    let revision = params
        .get("revision")
        .and_then(IvyJson::as_f64)
        .map(|v| v as u64);
    let context = step
        .get("context")
        .map(|value| {
            store.authenticate(&AuthContext {
                credential_digest: value["credentialDigest"].as_str().unwrap().into(),
                session_digest: value["sessionDigest"].as_str().map(JsString::from),
            })
        })
        .transpose()?
        .map(|p| AuthenticatedContext {
            principal_id: p.principal_id,
            credential_digest: p.credential_digest,
        });
    store.with_budget_mut(
        if step["overdue"] == true {
            Duration::ZERO
        } else {
            Duration::from_secs(10)
        },
        |store| match method {
            "constructor" => Ok(IvyJson::Null),
            "get" => owner.get(store, text(params, "hostId")?, revision),
            "list" => owner.list(store, runtime).map(IvyJson::Array),
            "put" => owner.put(
                store,
                context.as_ref().unwrap(),
                params,
                step["now"].as_u64().unwrap(),
                &step["nowText"].as_str().unwrap().into(),
            ),
            "edit" => owner.edit(store, text(params, "hostId")?, revision),
            "save" => owner.save(
                store,
                context.as_ref().unwrap(),
                params,
                step["now"].as_u64().unwrap(),
                &step["nowText"].as_str().unwrap().into(),
            ),
            "history" => owner.history(store, params),
            "projectRead" => {
                let owned;
                let read = if let Some(v) = params.get("read") {
                    v
                } else {
                    owned = objects::read(
                        store,
                        objects::Location::Id(text(params, "objectId")?),
                        revision,
                    )?;
                    &owned
                };
                owner.project_read(read)
            }
            "isConfigurationContract" => {
                Ok(Configurations::is_configuration_contract(text(params, "key")?).into())
            }
            "sql" => {
                let values = params
                    .get("bindings")
                    .and_then(IvyJson::as_array)
                    .unwrap_or(&[])
                    .iter()
                    .map(|v| match v {
                        IvyJson::Null => rusqlite::types::Value::Null,
                        IvyJson::String(v) => {
                            rusqlite::types::Value::Text(String::from_utf16_lossy(&v.0))
                        }
                        IvyJson::Number(v) => rusqlite::types::Value::Real(*v),
                        IvyJson::Bool(v) => rusqlite::types::Value::Integer(i64::from(*v)),
                        _ => panic!("Scalar SQL fixture binding"),
                    })
                    .collect::<Vec<_>>();
                store
                    .connection()
                    .execute(
                        &String::from_utf16_lossy(&text(params, "sql")?.0),
                        rusqlite::params_from_iter(values),
                    )
                    .map_err(|e| store.query_error(e))?;
                Ok(IvyJson::Null)
            }
            _ => Err(IvyError::internal()),
        },
    )
}
fn operation(method: &str, params: Value) -> Value {
    json!({"op":"configurations","method":method,"params":params,"now":100000,"nowText":"1970-01-01T00:01:40.000Z","context":{"credentialDigest":digest(true)}})
}
fn setup() -> Vec<Value> {
    let mut steps = super::setup();
    steps.push(sql("UPDATE metadata SET value='0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' WHERE key='cursor_secret';"));
    steps.push(json!({"op":"reopen"}));
    steps
}
fn config(host: &str) -> Value {
    json!({"schemaVersion":1,"hostId":host,"runtimeRoot":"C:/fixture/runtime","artifactRoot":"C:/fixture/artifacts","stagingRoot":"C:/fixture/staging","publicBaseUrl":"https://hive.invalid/ivy","executables":{"node":"node"},"instances":[{"instanceId":"worker","serviceNodeId":"worker","componentId":"fixture-worker","enabled":true,"engine":"process","credential":"synthetic-instance-secret","secretPaths":["/settings/token","/settings/whole"],"settings":{"value":1,"token":"synthetic-token","whole":[{"key":"synthetic-array-secret"}]}}]})
}
fn mutation(
    method: &str,
    host: &str,
    nonce: &str,
    configuration: Value,
    revision: Option<u64>,
) -> Value {
    let mut params = json!({"hostId":host,"mutationId":format!("epoch:100000:{nonce}"),"configuration":configuration});
    if let Some(revision) = revision {
        params["expectedRevision"] = json!(revision);
    }
    operation(method, params)
}
fn compare(steps: &[Value]) -> Value {
    let root = Root::new();
    // Obtain only language/date primitive observations in a warm-up store.
    // Native UUIDv4 IDs are then bound into the unchanged original crypto API.
    let observed = oracle(&root.0.join("observed.sqlite"), steps);
    let actual = native_with_runtime(
        &root.0.join("native.sqlite"),
        steps,
        Some(&observed["registryRuntime"]),
    );
    let mut original = steps.to_vec();
    for (index, step) in original.iter_mut().enumerate() {
        if step["op"] == "configurations" && matches!(step["method"].as_str(), Some("put" | "save"))
        {
            if let Some(id) = actual["rows"][index]["value"]["objectId"].as_str() {
                assert_eq!(uuid::Uuid::parse_str(id).unwrap().get_version_num(), 4);
                step["createdId"] = id.into();
            }
        }
    }
    let expected = oracle(&root.0.join("reference.sqlite"), &original);
    for (index, (a, b)) in actual["rows"]
        .as_array()
        .unwrap()
        .iter()
        .zip(expected["rows"].as_array().unwrap())
        .enumerate()
    {
        assert_eq!(
            a, b,
            "Configuration trace {index} ({}/{})",
            steps[index]["op"], steps[index]["method"]
        );
        if matches!(
            steps[index]["op"].as_str(),
            Some("sql" | "configure" | "register")
        ) || steps[index]["requireSuccess"] == true
            || (steps[index]["op"] == "configurations" && steps[index]["method"] == "sql")
        {
            assert!(
                b["error"].is_null(),
                "Fixture setup {index}: {}",
                b["error"]
            );
        }
    }
    for key in ["definitions", "metadata", "topics"] {
        assert_eq!(actual[key], expected[key]);
    }
    expected
}

#[test]
fn configurations_persist_current_and_historical_revisions_replay_cas_and_reopen_match_original() {
    let mut s = setup();
    let missing = push(&mut s, operation("get", json!({"hostId":"host"})));
    let initial = mutation("put", "host", "first", config("host"), None);
    let created = push(&mut s, initial.clone());
    let replay = push(&mut s, initial.clone());
    let no_cas = push(
        &mut s,
        mutation("put", "host", "no-cas", config("host"), None),
    );
    let stale = push(
        &mut s,
        mutation("put", "host", "stale", config("host"), Some(2)),
    );
    let mut changed = config("host");
    changed["instances"][0]["settings"]["value"] = json!(2);
    let updated = push(&mut s, mutation("put", "host", "second", changed, Some(1)));
    let old = push(
        &mut s,
        operation("get", json!({"hostId":"host","revision":1})),
    );
    let replay_old = push(&mut s, initial);
    push(&mut s, json!({"op":"reopen"}));
    let current = push(&mut s, operation("get", json!({"hostId":"host"})));
    let history = push(
        &mut s,
        operation("history", json!({"hostId":"host","limit":1})),
    );
    let mut next = operation("history", json!({"hostId":"host","limit":1,"cursor":null}));
    next["identityBindings"] =
        json!([{"from":history,"select":"/value/nextCursor","at":"/params/cursor"}]);
    let next = push(&mut s, next);
    let r = compare(&s);
    assert_eq!(code(&r, missing), "not_found");
    for i in [
        created, replay, updated, old, replay_old, current, history, next,
    ] {
        success(&r, i);
    }
    for i in [no_cas, stale] {
        assert_eq!(code(&r, i), "revision_conflict");
    }
    assert_eq!(value(&r, current)["revision"], 2);
    assert_eq!(
        value(&r, old)["configuration"]["instances"][0]["settings"]["value"],
        1
    );
    assert_eq!(value(&r, replay_old)["revision"], 1);
    assert!(value(&r, history)["nextCursor"].is_string());
}

#[test]
fn configurations_editor_redacts_preserves_rotated_secrets_and_protects_instance_paths_match_original()
 {
    let mut s = setup();
    let created = push(
        &mut s,
        mutation("put", "host", "first", config("host"), None),
    );
    let editor = push(&mut s, operation("edit", json!({"hostId":"host"})));
    let mut save = mutation("save", "host", "saved", Value::Null, Some(1));
    save["identityBindings"] =
        json!([{"from":editor,"select":"/value/configuration","at":"/params/configuration"}]);
    let saved = push(&mut s, save.clone());
    let replay = push(&mut s, save);
    let retained = push(&mut s, operation("get", json!({"hostId":"host"})));
    let mut rotated = config("host");
    rotated["instances"][0]["credential"] = json!("synthetic-rotated");
    let rotated = push(&mut s, mutation("put", "host", "rotate", rotated, Some(2)));
    let mut retain = mutation("save", "host", "retain", Value::Null, Some(3));
    retain["identityBindings"] =
        json!([{"from":editor,"select":"/value/configuration","at":"/params/configuration"}]);
    let retain = push(&mut s, retain);
    let final_get = push(&mut s, operation("get", json!({"hostId":"host"})));
    let mut read = operation("projectRead", json!({"objectId":null}));
    read["identityBindings"] =
        json!([{"from":created,"select":"/value/objectId","at":"/params/objectId"}]);
    let read = push(&mut s, read);
    let r = compare(&s);
    for i in [
        created, editor, saved, replay, retained, rotated, retain, final_get, read,
    ] {
        success(&r, i);
    }
    assert!(
        !serde_json::to_string(value(&r, editor))
            .unwrap()
            .contains("synthetic-token")
    );
    assert_eq!(
        value(&r, editor)["configuration"]["instances"][0]["credential"],
        json!({"$ivySecret":{"instanceId":"worker","path":"/credential"}})
    );
    assert_eq!(
        value(&r, final_get)["configuration"]["instances"][0]["credential"],
        "synthetic-rotated"
    );
    assert_eq!(value(&r, retained)["configuration"], config("host"));
    assert!(
        !serde_json::to_string(value(&r, read))
            .unwrap()
            .contains("synthetic-rotated")
    );
}

#[test]
fn configurations_editor_rejects_malformed_moved_undeclared_and_missing_markers_before_writing_match_original()
 {
    let mut s = setup();
    push(
        &mut s,
        mutation("put", "host", "first", config("host"), None),
    );
    let mut failures = vec![];
    for (i, candidate) in [
        Value::Null,
        json!([]),
        json!({"instances":{}}),
        json!({"instances":[null]}),
        json!({"instances":[{}]}),
        json!({"instances":[{"instanceId":"worker","secretPaths":null}]}),
        json!({"instances":[{"instanceId":"worker","secretPaths":["/settings/~2bad"]}]}),
    ]
    .into_iter()
    .enumerate()
    {
        failures.push(push(
            &mut s,
            mutation("save", "host", &format!("shape-{i}"), candidate, Some(1)),
        ));
    }
    for (i, (path, marker)) in [
        ("/credential", json!({"$ivySecret":{"instanceId":"worker"}})),
        (
            "/credential",
            json!({"$ivySecret":{"instanceId":"other","path":"/credential"}}),
        ),
        (
            "/credential",
            json!({"$ivySecret":{"instanceId":"worker","path":"/settings/token"}}),
        ),
        (
            "/settings/value",
            json!({"$ivySecret":{"instanceId":"worker","path":"/settings/value"}}),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut candidate = config("host");
        *candidate
            .pointer_mut(&format!("/instances/0{path}"))
            .unwrap() = marker;
        failures.push(push(
            &mut s,
            mutation("save", "host", &format!("marker-{i}"), candidate, Some(1)),
        ));
    }
    let mut missing = config("host");
    missing["instances"][0]["secretPaths"]
        .as_array_mut()
        .unwrap()
        .push(json!("/settings/missing"));
    missing["instances"][0]["settings"]["missing"] =
        json!({"$ivySecret":{"instanceId":"worker","path":"/settings/missing"}});
    failures.push(push(
        &mut s,
        mutation("save", "host", "missing-marker", missing, Some(1)),
    ));
    let r = compare(&s);
    for i in failures {
        assert_eq!(code(&r, i), "invalid_arguments");
        assert_eq!(state(&r, i), state(&r, i - 1));
    }
}

#[test]
fn configurations_admission_guard_order_identity_uniqueness_and_secret_path_rules_match_original() {
    let mut s = setup();
    let mut cases = vec![];
    for (i, change) in [
        json!({"hostId":"other"}),
        json!({"schemaVersion":2}),
        json!({"instances":[null]}),
    ]
    .into_iter()
    .enumerate()
    {
        let mut candidate = config("host");
        for (key, value) in change.as_object().unwrap() {
            candidate[key] = value.clone();
        }
        cases.push(push(
            &mut s,
            mutation("put", "host", &format!("bad-{i}"), candidate, Some(77)),
        ));
    }
    for (i, paths) in [
        json!(["/settings/token", "/settings/token"]),
        json!(["/credential"]),
        json!(["/settings/items/0/token"]),
        json!(["/settings/items/01"]),
        json!(["/settings/~2bad"]),
    ]
    .into_iter()
    .enumerate()
    {
        let mut candidate = config("host");
        candidate["instances"][0]["secretPaths"] = paths;
        cases.push(push(
            &mut s,
            mutation("put", "host", &format!("paths-{i}"), candidate, None),
        ));
    }
    let mut duplicate = config("host");
    let instance = duplicate["instances"][0].clone();
    duplicate["instances"]
        .as_array_mut()
        .unwrap()
        .push(instance);
    let duplicate = push(
        &mut s,
        mutation("put", "host", "duplicate", duplicate, None),
    );
    let fresh_cas = push(
        &mut s,
        mutation("put", "host", "fresh-cas", config("host"), Some(1)),
    );
    let mut long = operation(
        "isConfigurationContract",
        json!({"key":"ivy/host-configuration"}),
    );
    long["parameterUnits"] = json!({"/key":[55296]});
    let raw = push(&mut s, long);
    let yes = push(
        &mut s,
        operation(
            "isConfigurationContract",
            json!({"key":"ivy/host-configuration"}),
        ),
    );
    let r = compare(&s);
    assert_eq!(code(&r, cases[0]), "target_conflict");
    for i in cases.into_iter().skip(1).chain([duplicate]) {
        assert_eq!(code(&r, i), "invalid_arguments");
    }
    assert_eq!(code(&r, fresh_cas), "revision_conflict");
    assert_eq!(value(&r, raw), &json!(false));
    assert_eq!(value(&r, yes), &json!(true));
}

#[test]
fn configurations_editor_escaped_keys_overlapping_paths_whole_arrays_and_reordered_instances_match_original()
 {
    let mut s = setup();
    let mut cfg = config("host");
    cfg["instances"][0]["secretPaths"] = json!([
        "/settings/a~1b~0c",
        "/settings/",
        "/settings/group",
        "/settings/group/value",
        "/settings/whole"
    ]);
    cfg["instances"][0]["settings"] = json!({"a/b~c":"synthetic-escaped-secret","":"synthetic-empty-secret","group":{"value":"synthetic-overlap-secret"},"whole":[{"token":"synthetic-array-secret"}]});
    let mut second = cfg["instances"][0].clone();
    second["instanceId"] = json!("second");
    second["serviceNodeId"] = json!("second");
    second["credential"] = json!("synthetic-second");
    cfg["instances"].as_array_mut().unwrap().push(second);
    push(&mut s, mutation("put", "host", "first", cfg, None));
    let editor = push(&mut s, operation("edit", json!({"hostId":"host"})));
    let mut reordered = config("host");
    reordered["instances"] = json!([]);
    let mut save = mutation("save", "host", "reordered", reordered, Some(1));
    save["params"]["configuration"]["instances"] = json!([{}, {}]);
    save["identityBindings"] = json!([{"from":editor,"select":"/value/configuration/instances/1","at":"/params/configuration/instances/0"},{"from":editor,"select":"/value/configuration/instances/0","at":"/params/configuration/instances/1"}]);
    let save = push(&mut s, save);
    let get = push(&mut s, operation("get", json!({"hostId":"host"})));
    let r = compare(&s);
    for i in [editor, save, get] {
        success(&r, i);
    }
    let encoded = serde_json::to_string(value(&r, editor)).unwrap();
    assert!(!encoded.contains("synthetic-"));
    assert_eq!(
        value(&r, get)["configuration"]["instances"][0]["credential"],
        "synthetic-second"
    );
    assert_eq!(
        value(&r, get)["configuration"]["instances"][1]["settings"]["a/b~c"],
        "synthetic-escaped-secret"
    );
}

#[test]
fn configurations_each_sql_failure_and_deferred_commit_leave_identity_retryable_match_original() {
    for(index,sql_failure)in ["CREATE TRIGGER configuration_fault BEFORE INSERT ON objects BEGIN SELECT RAISE(FAIL,'synthetic');END;","CREATE TRIGGER configuration_fault BEFORE INSERT ON revisions BEGIN SELECT RAISE(FAIL,'synthetic');END;","CREATE TRIGGER configuration_fault BEFORE INSERT ON mutations BEGIN SELECT RAISE(FAIL,'synthetic');END;","CREATE TABLE configuration_parent(id INTEGER PRIMARY KEY);CREATE TABLE configuration_deferred(id INTEGER REFERENCES configuration_parent(id) DEFERRABLE INITIALLY DEFERRED);CREATE TRIGGER configuration_fault AFTER INSERT ON revisions BEGIN INSERT INTO configuration_deferred VALUES(1);END;"].into_iter().enumerate(){let mut s=setup();push(&mut s,operation("constructor",json!({})));push(&mut s,sql(sql_failure));let write=mutation("put","host",&format!("fault-{index}"),config("host"),None);let failed=push(&mut s,write.clone());push(&mut s,sql("DROP TRIGGER configuration_fault;"));let retried=push(&mut s,write.clone());let replay=push(&mut s,write);let r=compare(&s);assert_eq!(code(&r,failed),"internal_error");assert_eq!(state(&r,failed),state(&r,failed-1));for i in [retried,replay]{success(&r,i);assert_eq!(value(&r,i)["revision"],1);}}
}

#[test]
fn configurations_list_uses_original_default_locale_and_archived_current_selection_match_original()
{
    let mut s = setup();
    let mut creates = vec![];
    for (i, host) in ["z", "ä", "A", "a"].into_iter().enumerate() {
        creates.push(push(
            &mut s,
            mutation("put", host, &format!("host-{i}"), config(host), None),
        ));
    }
    let listed = push(&mut s, operation("list", json!({})));
    let mut archive = operation(
        "sql",
        json!({"sql":"UPDATE objects SET effective_archive=1 WHERE id=?","bindings":[null]}),
    );
    archive["identityBindings"] =
        json!([{"from":creates[0],"select":"/value/objectId","at":"/params/bindings/0"}]);
    push(&mut s, archive);
    let second = push(&mut s, operation("list", json!({})));
    let archived = push(&mut s, operation("get", json!({"hostId":"z"})));
    let r = compare(&s);
    assert_eq!(value(&r, listed).as_array().unwrap().len(), 4);
    assert_eq!(value(&r, second).as_array().unwrap().len(), 3);
    success(&r, archived);
    for item in value(&r, listed).as_array().unwrap() {
        assert!(item.get("configuration").is_none());
    }
}

#[test]
fn configurations_retained_duplicate_hosts_and_wrong_encoding_remain_visible_as_storage_failures_match_original()
 {
    let mut s = setup();
    let first = push(
        &mut s,
        mutation("put", "host", "first", config("host"), None),
    );
    let second = push(
        &mut s,
        mutation("put", "other", "second", config("other"), None),
    );
    // Construct damaged retained state explicitly; normal writes are immutable.
    push(&mut s, sql("DROP TRIGGER revisions_immutable_update;"));
    let mut duplicate = operation(
        "sql",
        json!({"sql":"UPDATE revisions SET content=? WHERE object_id=?","bindings":[serde_json::to_string(&config("host")).unwrap(),null]}),
    );
    duplicate["identityBindings"] =
        json!([{"from":second,"select":"/value/objectId","at":"/params/bindings/1"}]);
    push(&mut s, duplicate);
    let failed = push(&mut s, operation("get", json!({"hostId":"host"})));
    let mut remove = operation(
        "sql",
        json!({"sql":"UPDATE revisions SET content=? WHERE object_id=?","bindings":[serde_json::to_string(&config("other")).unwrap(),null]}),
    );
    remove["identityBindings"] =
        json!([{"from":second,"select":"/value/objectId","at":"/params/bindings/1"}]);
    push(&mut s, remove);
    let mut encoding = operation(
        "sql",
        json!({"sql":"UPDATE revisions SET encoding='text' WHERE object_id=?","bindings":[null]}),
    );
    encoding["identityBindings"] =
        json!([{"from":first,"select":"/value/objectId","at":"/params/bindings/0"}]);
    push(&mut s, encoding);
    let wrong = push(&mut s, operation("get", json!({"hostId":"host"})));
    let list = push(&mut s, operation("list", json!({})));
    let mut malformed = operation(
        "sql",
        json!({"sql":"UPDATE revisions SET content='malformed' WHERE object_id=?","bindings":[null]}),
    );
    malformed["identityBindings"] =
        json!([{"from":first,"select":"/value/objectId","at":"/params/bindings/0"}]);
    push(&mut s, malformed);
    let malformed = push(&mut s, operation("get", json!({"hostId":"host"})));
    let r = compare(&s);
    assert_eq!(code(&r, failed), "storage_invalid");
    assert_eq!(code(&r, wrong), "storage_invalid");
    assert_eq!(code(&r, list), "storage_invalid");
    assert_eq!(code(&r, malformed), "internal_error");
}
