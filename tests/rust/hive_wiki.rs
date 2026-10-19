use super::{
    AuthContext, AuthenticatedContext, IvyError, IvyJson, JsString, Root, Store, native,
    object_write, oracle,
};
use ivy_contracts::{
    IvyResult,
    core_schema::{validate_input, validate_output},
};
use ivy_hive::{
    objects::{self, Location},
    query::query_objects,
    wiki::{self, Action, Requests},
};
use serde_json::{Value, json};
use std::time::Duration;

struct Backend<'a> {
    store: &'a mut Store,
    context: AuthenticatedContext,
    step: &'a Value,
    calls: Vec<IvyJson>,
}
fn field<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a IvyJson> {
    value.get(key).ok_or_else(IvyError::internal)
}
fn text<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a JsString> {
    field(value, key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn location(value: &IvyJson) -> IvyResult<Location<'_>> {
    if let Some(id) = value.get("objectId").and_then(IvyJson::as_js_string) {
        Ok(Location::Id(id))
    } else {
        Ok(Location::Path(text(value, "path")?))
    }
}
impl Backend<'_> {
    fn called(&mut self, method: &str, params: &IvyJson) -> IvyResult<()> {
        self.calls.push(IvyJson::object([
            ("method", method.into()),
            ("params", params.clone()),
        ]));
        if self.step["failAt"].as_u64() == Some(self.calls.len() as u64) {
            return Err(IvyError::new(
                "fixture_failure",
                "Requested Wiki dependency failure.",
            ));
        }
        Ok(())
    }
    fn now(&self) -> JsString {
        self.step["nowText"].as_str().unwrap().into()
    }
}
impl Requests for Backend<'_> {
    fn request(&mut self, method: &'static str, params: &IvyJson) -> IvyResult<IvyJson> {
        self.called(method, params)?;
        validate_input(method, params)?;
        let now = self.now();
        let clock = self.step["now"].as_u64().unwrap();
        let result = match method {
            "objects.stat" => objects::stat(self.store, location(params)?),
            "objects.read" => objects::read(
                self.store,
                location(params)?,
                params
                    .get("revision")
                    .and_then(IvyJson::as_f64)
                    .map(|v| v as u64),
            ),
            "objects.query" => query_objects(self.store, params),
            "objects.write" => {
                objects::write(self.store, &self.context, params, false, clock, &now)
            }
            "objects.history" => objects::history(self.store, params),
            "objects.move" => objects::move_object(self.store, &self.context, params, clock, &now),
            "objects.archive" => {
                objects::archive(self.store, &self.context, None, params, clock, &now)
            }
            // Search routing is a callback-boundary check, not search backend qualification.
            "objects.search" => IvyJson::from_serde(
                self.step
                    .get("searchResponse")
                    .expect("Explicit search callback response"),
            ),
            _ => panic!("Unexpected Wiki request"),
        }?;
        validate_output(method, &result)?;
        Ok(result)
    }
    fn update(&mut self, params: &IvyJson) -> IvyResult<IvyJson> {
        self.called("wiki.updateContent", params)?;
        let value = IvyJson::object([
            ("mutationId", field(params, "mutationId")?.clone()),
            ("objectId", field(params, "objectId")?.clone()),
            (
                "expectedRevision",
                field(params, "expectedRevision")?.clone(),
            ),
            ("contractVersion", "1.0.0".into()),
            (
                "references",
                IvyJson::object(std::iter::empty::<(&str, IvyJson)>()),
            ),
            (
                "content",
                IvyJson::object([
                    ("encoding", "text".into()),
                    ("value", field(params, "markdown")?.clone()),
                ]),
            ),
        ]);
        let now = self.now();
        objects::write(
            self.store,
            &self.context,
            &value,
            true,
            self.step["now"].as_u64().unwrap(),
            &now,
        )
    }
}
pub(super) fn execute(store: &mut Store, step: &Value) -> (IvyResult<IvyJson>, Vec<IvyJson>) {
    let mut calls = vec![];
    let result = (|| {
        let method = step["method"].as_str().unwrap();
        let mut params = IvyJson::from_serde(&step["params"])?;
        object_write::apply_units(&mut params, step);
        validate_input(method, &params)?;
        let principal = store.authenticate(&AuthContext {
            credential_digest: step["context"]["credentialDigest"].as_str().unwrap().into(),
            session_digest: None,
        })?;
        let context = AuthenticatedContext {
            principal_id: principal.principal_id,
            credential_digest: principal.credential_digest,
        };
        let action = match method {
            "wiki.search" => Action::Search,
            "wiki.list" => Action::List,
            "wiki.read" => Action::Read,
            "wiki.create" => Action::Create,
            "wiki.update" => Action::Update,
            "wiki.history" => Action::History,
            "wiki.move" => Action::Move,
            "wiki.archive" => Action::Archive,
            _ => panic!("Known Wiki action"),
        };
        let result = store.with_budget_mut(
            if step["overdue"] == true {
                Duration::ZERO
            } else {
                Duration::from_secs(10)
            },
            |store| {
                let mut inner = Backend {
                    store,
                    context,
                    step,
                    calls: vec![],
                };
                let value = wiki::execute(&mut inner, action, &params);
                calls = inner.calls;
                value
            },
        );
        let result = result?;
        validate_output(method, &result)?;
        Ok(result)
    })();
    (result, calls)
}
fn sql(value: &str) -> Value {
    json!({"op":"sql","sql":value})
}
fn setup() -> Vec<Value> {
    let mut steps = vec![
        sql(
            "UPDATE metadata SET value='epoch' WHERE key='runtime_epoch';UPDATE metadata SET value='0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' WHERE key='cursor_secret';",
        ),
        json!({"op":"reopen"}),
        json!({"op":"configure","credentials":[{"principalId":"alpha","digest":format!("sha256:{}","a".repeat(64))}]}),
    ];
    for (key, version) in [
        ("wiki/page", "1.0.0"),
        ("wiki/page", "2.0.0"),
        ("fixture/page", "1.0.0"),
    ] {
        let definition = json!({"key":key,"version":version,"owner":{"kind":"agent"},"mediaType":"text/markdown","retention":{"objects":{"mode":"retain"},"revisions":{"mode":"all"}},"specMarkdown":"Synthetic immutable page fixture"});
        steps.push(json!({"op":"register","owner":{"kind":"agent"},"definition":definition}));
    }
    steps
}
fn operation(method: &str, params: Value) -> Value {
    json!({"op":"wiki","method":format!("wiki.{method}"),"params":params,"now":100000,"nowText":"1970-01-01T00:01:40.000Z","context":{"credentialDigest":format!("sha256:{}","a".repeat(64))}})
}
fn mutation(method: &str, nonce: &str, mut params: Value) -> Value {
    params["mutationId"] = format!("epoch:100000:{nonce}").into();
    operation(method, params)
}
fn create(nonce: &str, title: &str) -> Value {
    mutation(
        "create",
        nonce,
        json!({"title":title,"markdown":"Synthetic **markdown** α 😀"}),
    )
}
fn push(steps: &mut Vec<Value>, step: Value) -> usize {
    let index = steps.len();
    steps.push(step);
    index
}
fn bind(step: &mut Value, from: usize, at: &str) {
    let bindings = step
        .as_object_mut()
        .unwrap()
        .entry("identityBindings")
        .or_insert(json!([]))
        .as_array_mut()
        .unwrap();
    bindings.push(json!({"from":from,"select":"/value/object/id","at":at}));
}
fn raw_create(nonce: &str, name: &str, key: &str, version: &str) -> Value {
    json!({"op":"write","params":{"mutationId":format!("epoch:100000:{nonce}"),"contractVersion":version,"references":{},"create":{"contractKey":key,"parentId":null,"ownerObjectId":null,"name":name},"content":{"encoding":"text","value":"Synthetic reference target"}},"context":{"credentialDigest":format!("sha256:{}","a".repeat(64))},"now":100000,"nowText":"1970-01-01T00:01:40.000Z"})
}
fn compare(steps: &[Value]) -> Value {
    let root = Root::new();
    let actual = native(&root.0.join("native.sqlite"), steps);
    let mut reference = steps.to_vec();
    for (index, step) in reference.iter_mut().enumerate() {
        if step["op"] == "write" || step["method"] == "wiki.create" {
            if let Some(id) = actual["rows"][index]["value"]["object"]["id"].as_str() {
                assert_eq!(uuid::Uuid::parse_str(id).unwrap().get_version_num(), 4);
                step["createdId"] = id.into();
            }
        }
    }
    let expected = oracle(&root.0.join("original.sqlite"), &reference);
    assert_eq!(actual["rows"].as_array().unwrap().len(), steps.len());
    for (index, (a, b)) in actual["rows"]
        .as_array()
        .unwrap()
        .iter()
        .zip(expected["rows"].as_array().unwrap())
        .enumerate()
    {
        assert_eq!(
            a, b,
            "Wiki trace {index} ({}/{})",
            steps[index]["op"], steps[index]["method"]
        );
        if matches!(
            steps[index]["op"].as_str(),
            Some("sql" | "register" | "configure")
        ) || steps[index]["requireSuccess"] == true
        {
            assert!(b["error"].is_null(), "Wiki setup {index}: {}", b["error"]);
        }
    }
    for key in ["definitions", "metadata", "topics"] {
        assert_eq!(actual[key], expected[key]);
    }
    expected
}
fn value(result: &Value, index: usize) -> &Value {
    &result["rows"][index]["value"]
}
fn calls(result: &Value, index: usize) -> &Value {
    &result["rows"][index]["wikiCalls"]
}
fn code(result: &Value, index: usize) -> &Value {
    &result["rows"][index]["error"]["data"]["code"]
}
fn success(result: &Value, index: usize) {
    assert!(
        result["rows"][index]["error"].is_null(),
        "Wiki step {index}: {}",
        result["rows"][index]["error"]
    );
}

#[test]
fn wiki_navigation_history_archive_and_query_paging_use_real_objects_match_original() {
    let mut s = setup();
    let root = push(&mut s, create("root", "Root"));
    let mut first = create("first", "Zulu");
    first["params"]["parentId"] = Value::Null;
    bind(&mut first, root, "/params/parentId");
    let first = push(&mut s, first);
    let mut second = create("second", "Alpha");
    second["params"]["parentId"] = Value::Null;
    bind(&mut second, root, "/params/parentId");
    let second = push(&mut s, second);
    let roots = push(&mut s, operation("list", json!({})));
    let mut list = operation("list", json!({"parentId":null,"limit":1}));
    bind(&mut list, root, "/params/parentId");
    let listed = push(&mut s, list.clone());
    list["params"]["cursor"] = Value::Null;
    list["identityBindings"]
        .as_array_mut()
        .unwrap()
        .push(json!({"from":listed,"select":"/value/nextCursor","at":"/params/cursor"}));
    let next = push(&mut s, list);
    let path = push(&mut s, operation("read", json!({"path":"/Root/Alpha"})));
    let mut history = operation("history", json!({"objectId":null}));
    bind(&mut history, second, "/params/objectId");
    let history = push(&mut s, history);
    let mut movement = mutation(
        "move",
        "move",
        json!({"objectId":null,"parentId":null,"name":"Moved"}),
    );
    bind(&mut movement, first, "/params/objectId");
    let moved = push(&mut s, movement.clone());
    let replay = push(&mut s, movement);
    let mut archive = mutation(
        "archive",
        "archive",
        json!({"objectId":null,"archived":true,"expectedRevision":1}),
    );
    bind(&mut archive, root, "/params/objectId");
    let archive = push(&mut s, archive);
    let mut hidden = operation("list", json!({"parentId":null}));
    bind(&mut hidden, root, "/params/parentId");
    let hidden = push(&mut s, hidden.clone());
    let mut visible = s[hidden].clone();
    visible["params"]["includeArchived"] = true.into();
    let visible = push(&mut s, visible);
    let r = compare(&s);
    for i in [
        root, first, second, roots, listed, next, path, history, moved, replay, archive, hidden,
        visible,
    ] {
        success(&r, i);
    }
    assert_eq!(value(&r, roots)["items"].as_array().unwrap().len(), 1);
    assert!(value(&r, listed)["nextCursor"].is_string());
    assert_eq!(value(&r, next)["nextCursor"], Value::Null);
    assert_eq!(value(&r, hidden)["items"], json!([]));
    assert_eq!(value(&r, visible)["items"].as_array().unwrap().len(), 1);
    assert_eq!(calls(&r, listed)[0]["method"], "objects.stat");
    assert_eq!(calls(&r, listed)[1]["params"]["where"]["op"], "eq");
    assert_eq!(calls(&r, roots)[0]["params"]["where"]["op"], "isNull");
}

#[test]
fn wiki_update_retains_exact_references_replays_prior_revision_and_uses_cas_match_original() {
    let mut s = setup();
    let target = push(
        &mut s,
        raw_create("target", "Target", "fixture/page", "1.0.0"),
    );
    let mut page = raw_create("page", "Page", "wiki/page", "1.0.0");
    page["params"]["references"] = json!({"anchor":{"objectId":null,"revision":1}});
    bind(&mut page, target, "/params/references/anchor/objectId");
    let page = push(&mut s, page);
    let mut update = mutation(
        "update",
        "update",
        json!({"objectId":null,"expectedRevision":1,"markdown":"Changed page"}),
    );
    bind(&mut update, page, "/params/objectId");
    let updated = push(&mut s, update.clone());
    let mut read = operation("read", json!({"objectId":null}));
    bind(&mut read, page, "/params/objectId");
    let read = push(&mut s, read.clone());
    let mut later = update.clone();
    later["params"]["mutationId"] = "epoch:100000:later".into();
    later["params"]["expectedRevision"] = 2.into();
    later["params"]["markdown"] = "Later".into();
    let later = push(&mut s, later);
    let replay = push(&mut s, update.clone());
    update["params"]["mutationId"] = "epoch:100000:stale".into();
    let stale = push(&mut s, update);
    let mut history = operation("history", json!({"objectId":null,"limit":1}));
    bind(&mut history, page, "/params/objectId");
    let history = push(&mut s, history.clone());
    let mut next = s[history].clone();
    next["params"]["cursor"] = Value::Null;
    next["identityBindings"]
        .as_array_mut()
        .unwrap()
        .push(json!({"from":history,"select":"/value/nextCursor","at":"/params/cursor"}));
    let next = push(&mut s, next);
    let mut historical = s[read].clone();
    historical["params"]["revision"] = 1.into();
    let historical = push(&mut s, historical);
    let r = compare(&s);
    for i in [
        target, page, updated, read, later, replay, history, next, historical,
    ] {
        success(&r, i);
    }
    assert_eq!(value(&r, updated)["revision"]["revision"], 2);
    assert_eq!(value(&r, replay)["revision"]["revision"], 2);
    assert_eq!(
        value(&r, read)["revision"]["references"]["anchor"]["objectId"],
        value(&r, target)["object"]["id"]
    );
    assert_eq!(code(&r, stale), "revision_conflict");
    assert_eq!(calls(&r, updated)[1]["method"], "wiki.updateContent");
    assert_eq!(
        value(&r, historical)["content"]["value"],
        "Synthetic reference target"
    );
}

#[test]
fn wiki_page_parent_version_and_dependency_failure_guards_and_search_routing_match_original() {
    let mut s = setup();
    let foreign = push(
        &mut s,
        raw_create("foreign", "Foreign", "fixture/page", "1.0.0"),
    );
    let page = push(&mut s, create("page", "Page"));
    let future = push(&mut s, raw_create("future", "Future", "wiki/page", "2.0.0"));
    let mut rejected = vec![];
    for (method, params) in [
        ("read", json!({"objectId":null})),
        ("history", json!({"objectId":null})),
        (
            "update",
            json!({"objectId":null,"expectedRevision":1,"mutationId":"epoch:100000:bad-update","markdown":"Bad"}),
        ),
        (
            "archive",
            json!({"objectId":null,"archived":true,"mutationId":"epoch:100000:bad-archive"}),
        ),
        (
            "move",
            json!({"objectId":null,"parentId":null,"name":"No","mutationId":"epoch:100000:bad-move"}),
        ),
    ] {
        let mut op = operation(method, params);
        bind(&mut op, foreign, "/params/objectId");
        rejected.push(push(&mut s, op));
    }
    for (method, key, params) in [
        ("list", "parentId", json!({"parentId":null})),
        ("search", "rootId", json!({"rootId":null,"text":"query"})),
        (
            "create",
            "parentId",
            json!({"parentId":null,"title":"Child","markdown":"No","mutationId":"epoch:100000:bad-child"}),
        ),
    ] {
        let mut op = operation(method, params);
        bind(&mut op, foreign, &format!("/params/{key}"));
        rejected.push(push(&mut s, op));
    }
    let mut wrong_parent = mutation(
        "move",
        "bad-parent",
        json!({"objectId":null,"parentId":null,"name":"Bad"}),
    );
    bind(&mut wrong_parent, page, "/params/objectId");
    bind(&mut wrong_parent, foreign, "/params/parentId");
    let wrong_parent = push(&mut s, wrong_parent);
    let mut version = mutation(
        "update",
        "future-update",
        json!({"objectId":null,"expectedRevision":1,"markdown":"No"}),
    );
    bind(&mut version, future, "/params/objectId");
    let version = push(&mut s, version);
    let missing = push(&mut s, operation("read", json!({"objectId":"missing"})));
    let mut failures = vec![];
    for fail_at in [1, 2] {
        let mut op = operation("read", json!({"objectId":null}));
        bind(&mut op, page, "/params/objectId");
        op["failAt"] = fail_at.into();
        failures.push(push(&mut s, op));
    }
    let mut search = operation(
        "search",
        json!({"text":"exact query","rootId":null,"includeArchived":true,"limit":7,"cursor":"opaque"}),
    );
    bind(&mut search, page, "/params/rootId");
    search["searchResponse"] = json!({"items":[],"nextCursor":null});
    let search = push(&mut s, search);
    let mut global = operation("search", json!({"text":"global"}));
    global["searchResponse"] = json!({"items":[],"nextCursor":null});
    let global = push(&mut s, global);
    let r = compare(&s);
    for i in rejected {
        assert_eq!(code(&r, i), "invalid_arguments");
        assert_eq!(calls(&r, i).as_array().unwrap().len(), 1);
        assert_eq!(r["rows"][i]["state"], r["rows"][i - 1]["state"]);
    }
    assert_eq!(code(&r, wrong_parent), "invalid_arguments");
    assert_eq!(calls(&r, wrong_parent).as_array().unwrap().len(), 2);
    assert_eq!(code(&r, version), "contract_version_conflict");
    assert_eq!(code(&r, missing), "not_found");
    for (index, i) in failures.into_iter().enumerate() {
        assert_eq!(code(&r, i), "fixture_failure");
        assert_eq!(calls(&r, i).as_array().unwrap().len(), index + 1);
    }
    for i in [search, global] {
        success(&r, i);
    }
    assert_eq!(calls(&r, search)[1]["params"]["contractKey"], "wiki/page");
    assert_eq!(calls(&r, search)[1]["params"]["cursor"], "opaque");
    assert_eq!(calls(&r, global).as_array().unwrap().len(), 1);
}

#[test]
fn wiki_object_write_failures_roll_back_and_retry_original_identity_match_original() {
    for (index, table) in ["objects", "revisions", "mutations"]
        .into_iter()
        .enumerate()
    {
        let mut s = setup();
        push(
            &mut s,
            sql(&format!(
                "CREATE TRIGGER wiki_fault BEFORE INSERT ON {table} BEGIN SELECT RAISE(FAIL,'synthetic');END;"
            )),
        );
        let write = create(&format!("fault-{index}"), "Page");
        let failed = push(&mut s, write.clone());
        push(&mut s, sql("DROP TRIGGER wiki_fault;"));
        let retried = push(&mut s, write.clone());
        let replay = push(&mut s, write);
        let r = compare(&s);
        assert_eq!(code(&r, failed), "internal_error");
        assert_eq!(r["rows"][failed]["state"], r["rows"][failed - 1]["state"]);
        for i in [retried, replay] {
            success(&r, i);
            assert_eq!(value(&r, i)["revision"]["revision"], 1);
        }
    }
}
