use super::*;
use ivy_hive::package_catalog_object::PackageCatalogObject;
use std::collections::HashSet;

type Protected = fn(&Store) -> ivy_contracts::IvyResult<HashSet<JsString>>;
#[derive(Default)]
pub(super) struct Trace {
    owner: Option<PackageCatalogObject<Protected>>,
}
fn protected(store: &Store) -> ivy_contracts::IvyResult<HashSet<JsString>> {
    store.connection().execute("UPDATE metadata SET value=CAST(value AS INTEGER)+1 WHERE key='fixture_protected_calls'", []).map_err(|e| store.query_error(e))?;
    if let Some(statement) = store.metadata("fixture_protected_sql")? {
        store
            .connection()
            .execute_batch(&statement)
            .map_err(|e| store.query_error(e))?;
    }
    if store.metadata("fixture_protected_failure")?.as_deref() == Some("yes") {
        return Err(IvyError::new(
            "fixture_failure",
            "Protected builds unavailable.",
        ));
    }
    let value = store
        .metadata("fixture_protected_builds")?
        .unwrap_or_else(|| "[]".into());
    let value = ivy_contracts::json::parse(&value, usize::MAX)?;
    Ok(value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_js_string().unwrap().clone())
        .collect())
}
impl Trace {
    pub(super) fn reset(&mut self) {
        self.owner = None;
    }
    pub(super) fn execute(
        &mut self,
        store: &mut Store,
        step: &Value,
    ) -> ivy_contracts::IvyResult<IvyJson> {
        let method = step["method"].as_str().unwrap();
        if self.owner.is_none() || method == "constructor" {
            self.owner = Some(PackageCatalogObject::new(store, protected as Protected)?);
        }
        let owner = self.owner.as_ref().unwrap();
        let mut parameters = IvyJson::from_serde(&step["params"]).unwrap();
        object_write::apply_units(&mut parameters, step);
        let principal: JsString = step["principal"].as_str().unwrap_or("publisher").into();
        let now = step["now"].as_u64().unwrap();
        let at: JsString = step["nowText"].as_str().unwrap().into();
        store.with_budget_mut(
            if step["overdue"] == true {
                std::time::Duration::ZERO
            } else {
                std::time::Duration::from_secs(10)
            },
            |store| match method {
                "constructor" => Ok(IvyJson::Null),
                "current" => owner
                    .current(store)
                    .map(|v| v.map(|v| v.into_json()).unwrap_or(IvyJson::Null)),
                "seed" => owner
                    .seed(
                        store,
                        parameters.get("catalog").unwrap_or(&IvyJson::Null),
                        &principal,
                        now,
                        &at,
                    )
                    .map(|v| v.into_json()),
                "list" => owner
                    .list(
                        store,
                        parameters.get("after").unwrap_or(&IvyJson::Null),
                        parameters
                            .get("includeUis")
                            .and_then(IvyJson::as_bool)
                            .unwrap_or(false),
                    )
                    .map(|v| v.into_json()),
                "publish" => owner.publish(
                    store,
                    parameters.get("authorization").unwrap_or(&IvyJson::Null),
                    &principal,
                    now,
                    &at,
                ),
                "prune" => owner.prune(store, &principal, now, &at).map(IvyJson::Array),
                _ => Err(IvyError::internal()),
            },
        )
    }
}
fn op(method: &str, params: Value) -> Value {
    json!({"op":"packageCatalog","method":method,"params":params,"now":100000,"nowText":"1970-01-01T00:01:40.000Z"})
}
fn sql(statement: impl Into<String>) -> Value {
    json!({"op":"sql","sql":statement.into()})
}
fn push(steps: &mut Vec<Value>, step: Value) -> usize {
    let index = steps.len();
    steps.push(step);
    index
}
fn setup() -> Vec<Value> {
    let mut steps = vec![
        sql(
            "UPDATE metadata SET value='epoch' WHERE key='runtime_epoch'; INSERT INTO principals VALUES ('publisher'),('another'),('original'); INSERT INTO metadata VALUES ('fixture_protected_calls','0'),('fixture_protected_builds','[]');",
        ),
        op("constructor", json!({})),
    ];
    steps[1]["requireSuccess"] = true.into();
    steps
}
pub(super) fn authorization(component: &str, version: &str, kind: &str) -> Value {
    let build = ivy_contracts::hash::digest(format!("{component}/{version}"));
    let mut manifest = json!({"schemaVersion":1,"componentId":component,"kind":kind,"version":version,"buildId":build,"connectsToHive":false,"requirements":{"node":">=24.18.0 <25.0.0","hiveProtocol":1,"contracts":[]}});
    if kind == "app" {
        manifest["app"] = json!({"appId":component,"dist":format!("dist/apps/{component}"),"entryPath":"index.html"});
    } else {
        manifest["entrypoint"] =
            json!({"executable":"node","args":["dist/main.js"],"timeoutMs":5000});
        manifest["readiness"] = json!({"timeoutMs":5000,"command":{"executable":"node","args":["dist/health.js"],"timeoutMs":5000}});
        manifest["shutdown"] = json!({"timeoutMs":1000});
        manifest["restart"] = json!({"policy":"always","minimumDelayMs":100,"maximumDelayMs":500});
    }
    json!({"componentId":component,"version":version,"buildId":build,"archiveHash":ivy_contracts::hash::digest(format!("archive/{component}/{version}")),"bytes":1,"manifest":manifest})
}
fn entry(component: &str, version: &str, kind: &str, revision: u64) -> Value {
    let mut v = authorization(component, version, kind);
    v["revision"] = revision.into();
    v["publishedAt"] = "1970-01-01T00:00:00.000Z".into();
    v["publisherPrincipalId"] = "original".into();
    v
}
fn catalog(revision: u64, packages: Vec<Value>) -> Value {
    json!({"schemaVersion":1,"revision":revision,"packages":packages})
}
fn seed(value: Value) -> Value {
    let mut step = op("seed", json!({"catalog":value}));
    step["requireSuccess"] = true.into();
    step
}
fn invalid_seed(value: Value) -> Value {
    op("seed", json!({"catalog":value}))
}
fn publish(component: &str, version: &str, kind: &str) -> Value {
    op(
        "publish",
        json!({"authorization":authorization(component,version,kind)}),
    )
}
fn compare(steps: &[Value]) -> Value {
    let root = Root::new();
    let observed = steps
        .iter()
        .any(|step| step["op"] == "retention")
        .then(|| oracle(&root.0.join("observed.sqlite"), steps));
    let actual = if let Some(observed) = observed.as_ref() {
        // Only the existing Date/locale primitive observations cross this seam.
        native_with_runtime(
            &root.0.join("native.sqlite"),
            steps,
            Some(&observed["retentionRuntime"]),
        )
    } else {
        native(&root.0.join("native.sqlite"), steps)
    };
    let mut original = steps.to_vec();
    let mut prior = 0;
    for (index, step) in original.iter_mut().enumerate() {
        let mutations = actual["rows"][index]["catalogMutations"]
            .as_array()
            .unwrap();
        if step["op"] == "packageCatalog" {
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
                if step["method"] == "seed" {
                    let id = actual["rows"][index]["value"]["objectId"].as_str().unwrap();
                    assert_eq!(uuid::Uuid::parse_str(id).unwrap().get_version_num(), 4);
                    ids.push(id.to_owned());
                }
            } else {
                // Entropy from a rolled-back operation has no durable identity.
                // Give the reference valid unique primitive values, not outcomes.
                ids.push(format!("00000000-0000-4000-8000-{index:012}"));
                ids.push(format!("00000000-0001-4000-8000-{index:012}"));
            }
            step["uuids"] = json!(ids);
        }
        prior = mutations.len();
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
            "Package catalog trace {index} ({}/{})",
            steps[index]["op"], steps[index]["method"]
        );
        if steps[index]["op"] == "sql" || steps[index]["requireSuccess"] == true {
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
fn value(r: &Value, i: usize) -> &Value {
    &r["rows"][i]["value"]
}
fn success(r: &Value, i: usize) {
    assert!(r["rows"][i]["error"].is_null(), "{}", r["rows"][i]["error"]);
}
fn error(r: &Value, i: usize, code: &str) {
    assert_eq!(r["rows"][i]["error"]["data"]["code"], code);
}
fn revision(r: &Value, i: usize) -> u64 {
    value(r, i)["objectRevision"].as_u64().unwrap()
}
fn calls(r: &Value, i: usize) -> u64 {
    r["rows"][i]["state"]["metadata"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["key"] == "fixture_protected_calls")
        .unwrap()["value"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn package_catalog_initialization_seed_guard_order_idempotence_filters_and_reopen_match_original() {
    let mut s = setup();
    let current = push(&mut s, op("current", json!({})));
    let invalid = push(&mut s, op("list", json!({"after":-1,"includeUis":true})));
    let missing = push(&mut s, op("list", json!({"after":0,"includeUis":true})));
    let no_prune = push(&mut s, op("prune", json!({})));
    let mut invalid_auth = publish("fixture", "1.0.0", "service");
    invalid_auth["params"]["authorization"]["bytes"] = 0.into();
    let guard = push(&mut s, invalid_auth);
    let no_publish = push(&mut s, publish("fixture", "1.0.0", "service"));
    let created = push(
        &mut s,
        seed(catalog(
            8,
            vec![
                entry("service", "1.0.0", "service", 3),
                entry("ui", "2.0.0", "app", 8),
            ],
        )),
    );
    let again = push(&mut s, seed(json!({"invalid":true})));
    let filtered = push(&mut s, op("list", json!({"after":0,"includeUis":false})));
    let after = push(&mut s, op("list", json!({"after":3,"includeUis":true})));
    let empty = push(&mut s, op("list", json!({"after":8,"includeUis":true})));
    s.push(json!({"op":"reopen"}));
    let reopened = push(&mut s, op("current", json!({})));
    let r = compare(&s);
    assert!(value(&r, current).is_null());
    error(&r, invalid, "invalid_arguments");
    error(&r, missing, "service_unavailable");
    error(&r, no_prune, "service_unavailable");
    error(&r, guard, "invalid_arguments");
    error(&r, no_publish, "service_unavailable");
    for i in [created, again, filtered, after, empty, reopened] {
        success(&r, i);
        assert_eq!(revision(&r, i), 1);
    }
    assert_eq!(value(&r, created), value(&r, again));
    assert_eq!(value(&r, created), value(&r, reopened));
    assert_eq!(
        value(&r, filtered)["catalog"]["packages"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        value(&r, after)["catalog"]["packages"][0]["componentId"],
        "ui"
    );
    assert_eq!(value(&r, empty)["catalog"]["packages"], json!([]));
    assert_eq!(calls(&r, reopened), 0);
}

#[test]
fn package_catalog_publication_latest_identity_repeat_bytes_and_pruning_match_original() {
    let mut s = setup();
    s.push(seed(catalog(0, vec![])));
    let first = push(&mut s, publish("service", "1.0.0", "service"));
    let second = push(&mut s, publish("service", "2.0.0", "service"));
    let mut same = publish("service", "2.0.0", "service");
    same["params"]["authorization"]["bytes"] = 2.into();
    same["principal"] = "another".into();
    same["now"] = 200000.into();
    same["nowText"] = "1970-01-01T00:03:20.000Z".into();
    let repeat = push(&mut s, same);
    let old = push(&mut s, publish("service", "1.0.0", "service"));
    let mut changed = publish("service", "2.0.0", "service");
    changed["params"]["authorization"]["archiveHash"] =
        ivy_contracts::hash::digest("changed").into();
    let conflict = push(&mut s, changed);
    let mut manifest = publish("service", "2.0.0", "service");
    manifest["params"]["authorization"]["manifest"]["entrypoint"]["args"] =
        json!(["dist/other.js"]);
    let manifest_conflict = push(&mut s, manifest);
    let third = push(&mut s, publish("service", "10.0.0", "service"));
    let current = push(&mut s, op("current", json!({})));
    let r = compare(&s);
    for i in [first, second, repeat, third, current] {
        success(&r, i);
    }
    for i in [old, conflict, manifest_conflict] {
        error(&r, i, "version_conflict");
    }
    assert_eq!(value(&r, second), value(&r, repeat));
    assert_eq!(calls(&r, repeat), 2);
    assert_eq!(value(&r, third)["revision"], 3);
    assert_eq!(revision(&r, current), 4);
    assert_eq!(
        value(&r, current)["catalog"]["packages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["version"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["10.0.0"]
    );
}

#[test]
fn package_catalog_retention_keeps_stable_duplicate_identity_and_protected_builds_in_input_order() {
    let mut s = setup();
    let mut packages = vec![
        entry("one", "2.0.0", "service", 1),
        entry("one", "10.0.0", "service", 2),
        entry("one", "3.0.0", "service", 3),
        entry("two", "1.0.0", "app", 4),
        entry("two", "1.0.0", "app", 5),
        entry("two", "1.0.0", "app", 6),
    ];
    for (i, p) in packages.iter_mut().enumerate() {
        p["tag"] = i.into();
    }
    let protected_build = packages[0]["buildId"].as_str().unwrap();
    s.push(sql(format!(
        "UPDATE metadata SET value='[\"{protected_build}\"]' WHERE key='fixture_protected_builds';"
    )));
    s.push(seed(catalog(10, packages)));
    let pruned = push(&mut s, op("prune", json!({})));
    let first = push(&mut s, op("current", json!({})));
    s.push(sql(
        "UPDATE metadata SET value='[]' WHERE key='fixture_protected_builds';",
    ));
    let again = push(&mut s, op("prune", json!({})));
    let second = push(&mut s, op("current", json!({})));
    let noop = push(&mut s, op("prune", json!({})));
    let r = compare(&s);
    for i in [pruned, first, again, second, noop] {
        success(&r, i);
    }
    assert_eq!(
        value(&r, pruned)
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["tag"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![2, 4, 5]
    );
    assert_eq!(value(&r, again)[0]["tag"], 0);
    assert_eq!(value(&r, first)["catalog"]["revision"], 10);
    assert_eq!(revision(&r, first), 2);
    assert_eq!(revision(&r, second), 3);
    assert_eq!(value(&r, noop), &json!([]));
    assert_eq!(calls(&r, noop), 3);
    assert_eq!(
        value(&r, second)["catalog"]["packages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["tag"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
}

#[test]
fn package_catalog_protected_callback_failure_precedes_write_and_noop_repeat_skips_it() {
    let mut s = setup();
    s.push(seed(catalog(0, vec![])));
    let first = push(&mut s, publish("one", "1.0.0", "service"));
    s.push(sql(
        "INSERT INTO metadata VALUES ('fixture_protected_failure','yes');",
    ));
    let repeat = push(&mut s, publish("one", "1.0.0", "service"));
    let fail = push(&mut s, publish("one", "2.0.0", "service"));
    let prune = push(&mut s, op("prune", json!({})));
    let current = push(&mut s, op("current", json!({})));
    s.push(sql(
        "DELETE FROM metadata WHERE key='fixture_protected_failure';",
    ));
    let retry = push(&mut s, publish("one", "2.0.0", "service"));
    let r = compare(&s);
    success(&r, repeat);
    assert_eq!(value(&r, first), value(&r, repeat));
    error(&r, fail, "fixture_failure");
    error(&r, prune, "fixture_failure");
    assert_eq!(revision(&r, current), 2);
    assert_eq!(calls(&r, current), 3);
    success(&r, retry);
    assert_eq!(value(&r, retry)["revision"], 2);
}

#[test]
fn package_catalog_sql_rollback_retains_catalog_and_retry_uses_fresh_internal_operation() {
    let mut s = setup();
    s.push(seed(catalog(0, vec![])));
    s.push(sql("CREATE TRIGGER refuse_package_revision BEFORE INSERT ON revisions BEGIN SELECT RAISE(ABORT,'package fixture'); END;"));
    let fail = push(&mut s, publish("one", "1.0.0", "service"));
    let unchanged = push(&mut s, op("current", json!({})));
    s.push(json!({"op":"reopen"}));
    s.push(sql("DROP TRIGGER refuse_package_revision;"));
    let retried = push(&mut s, publish("one", "1.0.0", "service"));
    let current = push(&mut s, op("current", json!({})));
    let r = compare(&s);
    error(&r, fail, "internal_error");
    assert_eq!(revision(&r, unchanged), 1);
    assert_eq!(value(&r, unchanged)["catalog"]["packages"], json!([]));
    success(&r, retried);
    assert_eq!(revision(&r, current), 2);
    assert_eq!(
        r["rows"][current]["catalogMutations"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn package_catalog_seed_enforces_object_limits_and_preview_before_history_collection() {
    let mut s = setup();
    let mut extra = catalog(0, vec![]);
    extra["extra"] = true.into();
    let invalid = push(&mut s, invalid_seed(extra));
    let mut large = entry("one", "1.0.0", "service", 1);
    large["padding"] = "x".repeat(1_048_576).into();
    let too_big = push(&mut s, invalid_seed(catalog(1, vec![large])));
    let seeded = push(&mut s, seed(catalog(0, vec![])));
    s.push(publish("one", "1.0.0", "service"));
    let current = push(&mut s, op("current", json!({})));
    let read_old = json!({"op":"readObject","objectId":"","revision":1,"identityBindings":[{"at":"/objectId","from":seeded,"select":"/value/objectId"}]});
    let old = push(&mut s, read_old.clone());
    let collected = push(
        &mut s,
        json!({"op":"retention","method":"collect","now":100001,"requireSuccess":true}),
    );
    let previewed = push(&mut s, read_old.clone());
    s.push(json!({"op":"reopen"}));
    let applied = push(
        &mut s,
        json!({"op":"retention","method":"collect","now":100002,"requireSuccess":true}),
    );
    let pruned = push(&mut s, read_old);
    let r = compare(&s);
    error(&r, invalid, "invalid_arguments");
    error(&r, too_big, "content_too_large");
    success(&r, seeded);
    success(&r, current);
    assert_eq!(revision(&r, current), 2);
    success(&r, old);
    assert_eq!(value(&r, old)["content"]["value"], catalog(0, vec![]));
    success(&r, collected);
    success(&r, previewed);
    assert_eq!(value(&r, previewed)["content"]["value"], catalog(0, vec![]));
    success(&r, applied);
    error(&r, pruned, "revision_pruned");
}

#[test]
fn package_catalog_current_legacy_extras_nonjson_and_malformed_content_preserve_guards() {
    let mut s = setup();
    s.push(seed(catalog(0, vec![])));
    s.push(sql("DROP TRIGGER revisions_immutable_update;"));
    s.push(sql("UPDATE revisions SET content='{\"schemaVersion\":1,\"revision\":0,\"packages\":[],\"legacy\":true}' WHERE contract_key='ivy/package-catalog';"));
    let legacy = push(&mut s, op("current", json!({})));
    let list = push(&mut s, op("list", json!({"after":0,"includeUis":true})));
    s.push(sql("UPDATE revisions SET encoding='text',content='legacy text' WHERE contract_key='ivy/package-catalog';"));
    let nonjson = push(&mut s, op("current", json!({})));
    s.push(sql("UPDATE revisions SET encoding='json',content='{}' WHERE contract_key='ivy/package-catalog';"));
    let invalid = push(&mut s, op("current", json!({})));
    s.push(sql(
        "UPDATE revisions SET content='{' WHERE contract_key='ivy/package-catalog';",
    ));
    let malformed = push(&mut s, op("current", json!({})));
    s.push(sql("BEGIN; INSERT INTO objects(id,parent_id,owner_object_id,name,path,position,icon,depth,contract_key,current_revision,archived_at,effective_archive,created_at,updated_at) SELECT 'duplicate',NULL,NULL,'Other catalog','/Other catalog',position+1,icon,depth,contract_key,current_revision,'1970-01-01T00:01:40.000Z',1,created_at,updated_at FROM objects WHERE contract_key='ivy/package-catalog' LIMIT 1; INSERT INTO revisions SELECT 'duplicate',revision,contract_key,contract_version,encoding,content,content_hash,byte_length,created_at,references_json FROM revisions WHERE contract_key='ivy/package-catalog' LIMIT 1; COMMIT;"));
    let duplicate = push(&mut s, op("current", json!({})));
    let duplicate_seed = push(&mut s, invalid_seed(json!(null)));
    let r = compare(&s);
    success(&r, legacy);
    success(&r, list);
    assert_eq!(value(&r, legacy)["catalog"]["legacy"], true);
    assert!(value(&r, list)["catalog"].get("legacy").is_none());
    error(&r, nonjson, "storage_invalid");
    error(&r, invalid, "invalid_arguments");
    error(&r, malformed, "internal_error");
    error(&r, duplicate, "storage_invalid");
    error(&r, duplicate_seed, "storage_invalid");
}

#[test]
fn package_catalog_rejects_invalid_after_and_revision_overflow_without_partial_publication() {
    let mut s = setup();
    s.push(seed(catalog(9_007_199_254_740_991, vec![])));
    let invalids = [
        json!(null),
        json!("0"),
        json!(true),
        json!(-1),
        json!(0.1),
        json!(9_007_199_254_740_992_u64),
    ]
    .into_iter()
    .map(|after| push(&mut s, op("list", json!({"after":after,"includeUis":true}))))
    .collect::<Vec<_>>();
    let overflow = push(&mut s, publish("one", "1.0.0", "service"));
    let current = push(&mut s, op("current", json!({})));
    let r = compare(&s);
    for i in invalids {
        error(&r, i, "invalid_arguments");
    }
    error(&r, overflow, "invalid_arguments");
    success(&r, current);
    assert_eq!(revision(&r, current), 1);
    assert_eq!(
        value(&r, current)["catalog"]["revision"],
        json!(9_007_199_254_740_991_u64)
    );
}
