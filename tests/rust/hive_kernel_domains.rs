use super::*;

pub(super) fn ok(steps: &mut Vec<Value>, mut step: Value) -> usize {
    step["requireSuccess"] = true.into();
    push(steps, step)
}
pub(super) fn value(result: &Value, index: usize) -> &Value {
    &result["rows"][index]["value"]["value"]
}
pub(super) fn follow(mut step: Value, from: usize) -> Value {
    step["request"]["params"]["cursor"] = "bound".into();
    bind(
        &mut step,
        "/request/params/cursor",
        from,
        "/value/value/nextCursor",
    );
    step
}
pub(super) fn catalog() -> Value {
    let tools = ["alpha","beta"].map(|name|json!({"namespace":"fixture","name":name,"nativeMethod":name,"nativeSchemaIdentity":"synthetic","interfaceVersion":"1.0.0","description":name,"inputSchema":{"type":"object"},"outputSchema":{"type":"object"}}));
    json!({"namespaces":[{
        "namespace":"fixture","description":"Fixture namespace","guideMarkdown":"Fixture guide.",
        "tools":tools,
        "topics":[{"topic":"fixture.changed","version":"1.0.0","title":"Changed","description":"Fixture changes","payloadSchema":{"type":"object","properties":{"n":{"type":"integer"}},"required":["n"],"additionalProperties":false},"eventKinds":[]}],
        "inventoryKinds":[{"kind":"item","version":"1.0.0","summarySchema":{"type":"object","properties":{"name":{"type":["string","null"]},"archived":{"type":["boolean","null"]},"recency":{"type":["number","string","boolean","null"]}},"required":["name"],"additionalProperties":false},"searchPointers":["/name"],"archivedPointer":"/archived","recencyPointer":"/recency"}]
    }],"contracts":[],"requiredContracts":[
        {"key":"fixture/json","readVersions":["1.0.0"],"writeVersions":[]},
        {"key":"fixture/text","readVersions":["1.0.0"],"writeVersions":[]}
    ]})
}
pub(super) fn ready(steps: &mut Vec<Value>, id: &str, host: &str, service: &str) {
    let mut connection = connect(id, Some(1.0));
    connection["request"]["params"]["hostId"] = host.into();
    connection["request"]["params"]["serviceName"] = service.into();
    ok(steps, connection);
    ok(steps, service_request("registry.sync", id, catalog()));
    ok(
        steps,
        service_request(
            "service.heartbeat",
            id,
            json!({"ready":true,"diagnostics":[]}),
        ),
    );
}

#[test]
fn kernel_registry_catalogs_keep_version_order_cursor_identity_and_readiness() {
    let mut s = setup();
    let latest = ok(
        &mut s,
        request("contracts.list", json!({"key":"fixture/hive"})),
    );
    let first = ok(
        &mut s,
        request(
            "contracts.list",
            json!({"key":"fixture/hive","allVersions":true,"limit":1}),
        ),
    );
    let next = ok(
        &mut s,
        follow(
            request(
                "contracts.list",
                json!({"key":"fixture/hive","allVersions":true,"limit":1}),
            ),
            first,
        ),
    );
    let changed_query = push(
        &mut s,
        follow(
            request(
                "contracts.list",
                json!({"key":"fixture/hive","allVersions":false,"limit":1}),
            ),
            first,
        ),
    );
    ok(&mut s, connect("node", Some(1.0)));
    let absent = ok(
        &mut s,
        request("serviceNodes.contracts", json!({"serviceNodeId":"node"})),
    );
    ok(&mut s, service_request("registry.sync", "node", catalog()));
    let first_requirement = ok(
        &mut s,
        request(
            "serviceNodes.contracts",
            json!({"serviceNodeId":"node","limit":1}),
        ),
    );
    let second_requirement = ok(
        &mut s,
        follow(
            request(
                "serviceNodes.contracts",
                json!({"serviceNodeId":"node","limit":1}),
            ),
            first_requirement,
        ),
    );
    let mut resync = service_request("registry.sync", "node", catalog());
    resync["now"] = 101000.into();
    ok(&mut s, resync);
    let stale = push(
        &mut s,
        follow(
            request(
                "serviceNodes.contracts",
                json!({"serviceNodeId":"node","limit":1}),
            ),
            first_requirement,
        ),
    );
    ok(
        &mut s,
        service_request(
            "service.heartbeat",
            "node",
            json!({"ready":true,"diagnostics":[]}),
        ),
    );
    let nodes = ok(
        &mut s,
        request(
            "serviceNodes.list",
            json!({"hostId":"host","serviceName":"fixture"}),
        ),
    );
    let hosts = ok(&mut s, request("hosts.list", json!({})));
    ok(&mut s, request("hosts.observations", json!({})));
    let invalid = push(&mut s, service_request("registry.sync", "node", json!({})));
    let diagnostics = ok(
        &mut s,
        request("system.diagnostics", json!({"status":"current"})),
    );
    let unready = ok(
        &mut s,
        request("serviceNodes.get", json!({"serviceNodeId":"node"})),
    );
    s.push(sql(
        "UPDATE registries SET registry_json='null' WHERE node_id='node';",
    ));
    let null_catalog = ok(
        &mut s,
        request("serviceNodes.contracts", json!({"serviceNodeId":"node"})),
    );
    let result = compare(&s);
    assert_eq!(value(&result, latest)["items"][0]["version"], "10.0.0");
    assert_eq!(value(&result, first)["items"][0]["version"], "1.0.0");
    assert_eq!(value(&result, next)["items"][0]["version"], "10.0.0");
    error(&result, changed_query, "invalid_cursor");
    assert_eq!(value(&result, absent)["hasCatalog"], false);
    assert_eq!(value(&result, null_catalog)["hasCatalog"], false);
    assert_eq!(
        value(&result, first_requirement)["items"][0]["key"],
        "fixture/json"
    );
    assert_eq!(
        value(&result, second_requirement)["items"][0]["key"],
        "fixture/text"
    );
    error(&result, stale, "invalid_cursor");
    assert_eq!(value(&result, nodes)["items"][0]["ready"], true);
    assert_eq!(
        value(&result, hosts)["items"][0]["serviceNodeIds"],
        json!(["node"])
    );
    error(&result, invalid, "invalid_arguments");
    assert!(
        !value(&result, diagnostics)["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(value(&result, unready)["synced"], false);
}

#[test]
fn kernel_namespace_tool_and_topic_catalogs_bind_providers_and_snapshot_cursors() {
    let mut s = setup();
    ready(&mut s, "node", "host", "fixture");
    let namespaces = ok(&mut s, request("namespaces.list", json!({})));
    ok(
        &mut s,
        request(
            "namespaces.get",
            json!({"namespace":"fixture","serviceNodeId":"node"}),
        ),
    );
    let tools = ok(
        &mut s,
        request(
            "tools.list",
            json!({"namespace":"fixture","serviceNodeId":"node","limit":1}),
        ),
    );
    let next = ok(
        &mut s,
        follow(
            request(
                "tools.list",
                json!({"namespace":"fixture","serviceNodeId":"node","limit":1}),
            ),
            tools,
        ),
    );
    let prefix = ok(
        &mut s,
        request(
            "tools.list",
            json!({"namespace":"fixture","serviceNodeId":"node","namePrefix":"b"}),
        ),
    );
    let all_topics = ok(&mut s, request("topics.list", json!({})));
    let scoped = ok(
        &mut s,
        request(
            "topics.list",
            json!({"namespace":"fixture","serviceNodeId":"node"}),
        ),
    );
    let mut changed = catalog();
    changed["namespaces"][0]["guideMarkdown"] = "Changed guide.".into();
    ok(&mut s, service_request("registry.sync", "node", changed));
    let stale = push(
        &mut s,
        follow(
            request(
                "tools.list",
                json!({"namespace":"fixture","serviceNodeId":"node","limit":1}),
            ),
            tools,
        ),
    );
    let resource =
        json!({"serviceNodeId":"node","namespace":"fixture","kind":"item","nativeId":"a"});
    let conflict = push(
        &mut s,
        request(
            "topics.list",
            json!({"serviceNodeId":"missing","namespace":"other","resourceRef":resource,"cursor":"invalid"}),
        ),
    );
    let namespace_conflict = push(
        &mut s,
        request(
            "topics.list",
            json!({"namespace":"other","resourceRef":resource}),
        ),
    );
    let owner_conflict = push(
        &mut s,
        request(
            "topics.list",
            json!({"serviceNodeId":"node","hostId":"other"}),
        ),
    );
    ready(&mut s, "second", "host2", "fixture");
    ok(
        &mut s,
        service_request(
            "service.heartbeat",
            "node",
            json!({"ready":true,"diagnostics":[]}),
        ),
    );
    let ambiguous = push(
        &mut s,
        request("namespaces.get", json!({"namespace":"fixture"})),
    );
    s.push(sql(
        "UPDATE registries SET registry_json='[' WHERE node_id='second';",
    ));
    let corrupt = push(&mut s, request("topics.list", json!({"cursor":"invalid"})));
    let result = compare(&s);
    assert_eq!(
        value(&result, namespaces)["items"][0]["providers"][0]["eligible"],
        true
    );
    assert_eq!(
        value(&result, tools)["items"][0]["qualifiedName"],
        "fixture.alpha"
    );
    assert_eq!(
        value(&result, next)["items"][0]["qualifiedName"],
        "fixture.beta"
    );
    assert_eq!(value(&result, prefix)["items"].as_array().unwrap().len(), 1);
    assert!(
        value(&result, all_topics)["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["provider"].is_null() && v["namespace"] == "hive")
    );
    assert_eq!(value(&result, scoped)["items"].as_array().unwrap().len(), 1);
    error(&result, stale, "invalid_cursor");
    for index in [conflict, namespace_conflict, owner_conflict] {
        error(&result, index, "target_conflict");
    }
    error(&result, ambiguous, "ambiguous_service_node");
    error(&result, corrupt, "internal_error");
}

fn entry(id: &str, name: &str, recency: i64) -> Value {
    json!({"nativeId":id,"summary":{"name":name,"archived":false,"recency":recency},"observedAt":"1970-01-01T00:01:40Z"})
}
fn inventory(revision: u64, entries: Vec<Value>) -> Value {
    service_request(
        "inventory.sync",
        "node",
        json!({"namespace":"fixture","kind":"item","schemaVersion":"1.0.0","mode":"snapshot","snapshotRevision":revision,"entries":entries}),
    )
}

fn measured_inventory_pages(steps: &[Value]) {
    use rusqlite::{Connection, ffi};
    use std::{
        ffi::{CStr, c_int, c_void},
        ptr,
    };

    #[derive(Clone, Debug)]
    struct Statement {
        address: usize,
        sql: String,
        expanded: String,
        rows: usize,
    }
    struct Trace {
        db: *mut ffi::sqlite3,
        statements: Box<Vec<Statement>>,
    }
    unsafe extern "C" fn observe(
        kind: u32,
        context: *mut c_void,
        statement: *mut c_void,
        _: *mut c_void,
    ) -> c_int {
        // SQLite invokes this synchronously on this connection's test thread.
        // The boxed records stay alive until Drop unregisters the callback.
        let records = unsafe { &mut *context.cast::<Vec<Statement>>() };
        let address = statement as usize;
        if kind == ffi::SQLITE_TRACE_STMT as u32 {
            let sql = unsafe { ffi::sqlite3_sql(statement.cast()) };
            if !sql.is_null() {
                let sql = unsafe { CStr::from_ptr(sql) }
                    .to_string_lossy()
                    .into_owned();
                if sql.starts_with("SELECT") {
                    let expanded = unsafe { ffi::sqlite3_expanded_sql(statement.cast()) };
                    let text = if expanded.is_null() {
                        String::new()
                    } else {
                        let text = unsafe { CStr::from_ptr(expanded) }
                            .to_string_lossy()
                            .into_owned();
                        unsafe { ffi::sqlite3_free(expanded.cast()) };
                        text
                    };
                    records.push(Statement {
                        address,
                        sql,
                        expanded: text,
                        rows: 0,
                    });
                }
            }
        } else if kind == ffi::SQLITE_TRACE_ROW as u32 {
            if let Some(record) = records
                .iter_mut()
                .rev()
                .find(|record| record.address == address)
            {
                record.rows += 1;
            }
        }
        0
    }
    impl Trace {
        fn new(db: &Connection) -> Self {
            let mut trace = Self {
                db: unsafe { db.handle() },
                statements: Box::default(),
            };
            assert_eq!(
                unsafe {
                    ffi::sqlite3_trace_v2(
                        trace.db,
                        (ffi::SQLITE_TRACE_STMT | ffi::SQLITE_TRACE_ROW) as u32,
                        Some(observe),
                        trace.statements.as_mut() as *mut Vec<Statement> as *mut c_void,
                    )
                },
                ffi::SQLITE_OK
            );
            trace
        }
    }
    impl Drop for Trace {
        fn drop(&mut self) {
            unsafe {
                ffi::sqlite3_trace_v2(self.db, 0, None, ptr::null_mut());
            }
        }
    }
    fn execute(kernel: &mut Kernel, step: Value) -> IvyJson {
        let result = kernel
            .execute(
                &IvyJson::from_serde(&step["context"]).unwrap(),
                &IvyJson::from_serde(&step["request"]).unwrap(),
            )
            .unwrap();
        assert_eq!(result.get("kind"), Some(&IvyJson::from("result")));
        result.required("value").unwrap().clone()
    }
    fn measure(kernel: &mut Kernel) -> (IvyJson, Vec<Statement>) {
        // Observe rows returned by the real statements rather than allocating
        // another query implementation or a production instrumentation hook.
        // inventory.list keeps this connection alive; unregister before return
        // or unwinding, while the caller still owns the Kernel.
        let trace = Trace::new(kernel.parts.store.connection());
        let result = kernel.execute(&IvyJson::from_serde(&context("http")).unwrap(), &IvyJson::from_serde(&request("inventory.list", json!({"serviceNodeId":"node","namespace":"fixture","kind":"item","limit":7}))["request"]).unwrap()).unwrap();
        assert_eq!(result.get("kind"), Some(&IvyJson::from("result")));
        let records = trace.statements.as_ref().clone();
        drop(trace);
        (result.required("value").unwrap().clone(), records)
    }
    let root = Root::new();
    let path = root.0.join("measured.sqlite");
    let original = reference(&root.0.join("unused.sqlite"), steps);
    let profile = &original["registryRuntime"];
    native(&path, &steps[..setup().len()], profile);
    let mut kernel = compose(&path, profile, Rc::new(Cell::new(100000.0)));
    execute(&mut kernel, connect("node", Some(1.0)));
    execute(
        &mut kernel,
        service_request("registry.sync", "node", catalog()),
    );
    execute(
        &mut kernel,
        service_request(
            "service.heartbeat",
            "node",
            json!({"ready":true,"diagnostics":[]}),
        ),
    );
    let seed = |revision, count| {
        inventory(
            revision,
            (0..count)
                .map(|index| entry(&format!("item-{index:05}"), &format!("Item {index}"), index))
                .collect(),
        )
    };
    execute(&mut kernel, seed(1, 10));
    let (small_page, small) = measure(&mut kernel);
    execute(&mut kernel, seed(2, 1200));
    let (large_page, large) = measure(&mut kernel);
    assert_eq!(
        small_page
            .required("items")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        7
    );
    assert_eq!(
        large_page
            .required("items")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        7
    );
    assert!(
        large_page
            .get("nextCursor")
            .and_then(IvyJson::as_js_string)
            .is_some()
    );
    let counts = |records: &[Statement]| {
        (
            records.len(),
            records.iter().map(|record| record.rows).sum::<usize>(),
            records.iter().map(|record| record.rows).max().unwrap(),
        )
    };
    assert_eq!(counts(&large), counts(&small));
    assert!(counts(&large).2 <= 8);
    let query = large
        .iter()
        .find(|record| {
            record
                .sql
                .starts_with("SELECT i.*,s.revision AS snapshot_revision")
        })
        .unwrap();
    assert_eq!(query.rows, 8);
    assert!(query.sql.contains("LIMIT ?"));
    let plan = kernel
        .parts
        .store
        .connection()
        .prepare(&format!("EXPLAIN QUERY PLAN {}", query.expanded))
        .unwrap()
        .query_map([], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(
        plan.iter()
            .any(|detail| detail.to_lowercase().contains("inventory"))
    );
    assert!(
        plan.iter()
            .any(|detail| detail.to_lowercase().contains("index"))
    );

    execute(
        &mut kernel,
        inventory(
            3,
            (0..3)
                .map(|index| entry(&format!("large-{index}"), &"x".repeat(750_000), index))
                .collect(),
        ),
    );
    let first = execute(
        &mut kernel,
        request(
            "inventory.list",
            json!({"serviceNodeId":"node","namespace":"fixture","kind":"item","limit":10}),
        ),
    );
    assert_eq!(
        first.required("items").unwrap().as_array().unwrap().len(),
        2
    );
    let cursor = first.required("nextCursor").unwrap();
    let second = execute(
        &mut kernel,
        request(
            "inventory.list",
            json!({"serviceNodeId":"node","namespace":"fixture","kind":"item","limit":10,"cursor":cursor.to_serde().unwrap()}),
        ),
    );
    let ids = first
        .required("items")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .chain(second.required("items").unwrap().as_array().unwrap())
        .map(|item| {
            item.required("resourceRef")
                .unwrap()
                .required("nativeId")
                .unwrap()
                .as_str()
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(ids, ["large-0", "large-1", "large-2"]);
    assert!(second.get("nextCursor").is_none_or(IvyJson::is_null));
    execute(
        &mut kernel,
        inventory(4, vec![entry("oversized", "Oversized", 0)]),
    );
    kernel.parts.store.connection().execute("UPDATE inventory SET summary=? WHERE node_id='node' AND namespace='fixture' AND kind='item' AND native_id='oversized'", [json!({"name":"x".repeat(2_200_000)}).to_string()]).unwrap();
    let failure = kernel
        .execute(
            &IvyJson::from_serde(&context("http")).unwrap(),
            &IvyJson::from_serde(
                &request(
                    "inventory.list",
                    json!({"serviceNodeId":"node","namespace":"fixture","kind":"item","limit":10}),
                )["request"],
            )
            .unwrap(),
        )
        .unwrap_err();
    assert_eq!(failure.code, "result_too_large");
}

#[test]
fn kernel_inventory_sync_search_paging_and_retry_preserve_actual_sql_and_reopen() {
    let mut s = setup();
    ok(&mut s, connect("node", Some(1.0)));
    let premature = push(&mut s, inventory(1, vec![]));
    ok(&mut s, service_request("registry.sync", "node", catalog()));
    let entries = vec![entry("b", "Beta", 2), entry("a", "Alpha", 1)];
    let initial = ok(&mut s, inventory(4, entries.clone()));
    let replay = ok(&mut s, inventory(4, entries.into_iter().rev().collect()));
    let first = ok(
        &mut s,
        request(
            "inventory.list",
            json!({"serviceNodeId":"node","namespace":"fixture","kind":"item","limit":1}),
        ),
    );
    let next = ok(
        &mut s,
        follow(
            request(
                "inventory.list",
                json!({"serviceNodeId":"node","namespace":"fixture","kind":"item","limit":1}),
            ),
            first,
        ),
    );
    let search = ok(
        &mut s,
        request(
            "inventory.list",
            json!({"serviceNodeId":"node","searchTerm":"ALP","sort":"recency-desc","archived":false}),
        ),
    );
    let resource =
        json!({"serviceNodeId":"node","namespace":"fixture","kind":"item","nativeId":"a"});
    ok(
        &mut s,
        request("inventory.get", json!({"resourceRef":resource})),
    );
    let delta = service_request(
        "inventory.sync",
        "node",
        json!({"namespace":"fixture","kind":"item","schemaVersion":"1.0.0","mode":"delta","snapshotRevision":5,"expectedRevision":4,"entries":[entry("c","Gamma",3)],"removedNativeIds":["a"]}),
    );
    s.push(sql("CREATE TRIGGER kernel_inventory_failure BEFORE INSERT ON inventory WHEN NEW.native_id='c' BEGIN SELECT RAISE(ABORT,'fixture failure'); END;"));
    let failed = push(&mut s, delta.clone());
    s.push(sql("DROP TRIGGER kernel_inventory_failure;"));
    let retry = ok(&mut s, delta.clone());
    let stale = push(
        &mut s,
        follow(
            request(
                "inventory.list",
                json!({"serviceNodeId":"node","namespace":"fixture","kind":"item","limit":1}),
            ),
            first,
        ),
    );
    let repeated_delta = push(&mut s, delta);
    let removed = push(
        &mut s,
        request("inventory.get", json!({"resourceRef":resource})),
    );
    let forbidden = push(
        &mut s,
        request(
            "inventory.sync",
            json!({"namespace":"fixture","kind":"item","schemaVersion":"1.0.0","mode":"snapshot","snapshotRevision":6,"entries":[]}),
        ),
    );
    s.push(op("reopen"));
    let reopened = ok(
        &mut s,
        request(
            "inventory.list",
            json!({"serviceNodeId":"node","sort":"recency-desc"}),
        ),
    );
    let result = compare(&s);
    error(&result, premature, "service_not_ready");
    assert_eq!(value(&result, initial), value(&result, replay));
    assert_eq!(
        value(&result, first)["items"][0]["resourceRef"]["nativeId"],
        "a"
    );
    assert_eq!(
        value(&result, next)["items"][0]["resourceRef"]["nativeId"],
        "b"
    );
    assert_eq!(value(&result, search)["items"].as_array().unwrap().len(), 1);
    error(&result, failed, "internal_error");
    assert_eq!(
        result["rows"][failed]["state"]["inventorySets"],
        result["rows"][failed - 1]["state"]["inventorySets"]
    );
    assert_eq!(value(&result, retry)["snapshotRevision"], 5);
    error(&result, stale, "invalid_cursor");
    error(&result, repeated_delta, "revision_conflict");
    error(&result, removed, "not_found");
    error(&result, forbidden, "stale_generation");
    assert_eq!(
        value(&result, reopened)["items"].as_array().unwrap().len(),
        2
    );
    measured_inventory_pages(&s);
}

#[test]
fn kernel_event_publication_receipts_subscription_boundaries_and_durable_access_survive_reopen() {
    let mut s = setup();
    ready(&mut s, "node", "host", "fixture");
    let publication = service_request(
        "events.publish",
        "node",
        json!({"topic":"fixture.changed","topicVersion":"1.0.0","payload":{"n":1},"mutationId":"epoch:100000:publish"}),
    );
    let published = ok(&mut s, publication.clone());
    let replay = ok(&mut s, publication.clone());
    let forbidden = push(
        &mut s,
        request("events.publish", publication["request"]["params"].clone()),
    );
    let invalid_payload = push(
        &mut s,
        service_request(
            "events.publish",
            "node",
            json!({"topic":"fixture.changed","topicVersion":"1.0.0","payload":{"n":"bad"},"mutationId":"epoch:100000:bad"}),
        ),
    );
    let subscription = service_request(
        "events.subscribe",
        "node",
        json!({"name":"watch","filter":{"topics":["fixture.changed"]},"limit":1}),
    );
    let delivered = ok(&mut s, subscription.clone());
    let held = ok(&mut s, subscription.clone());
    let invalid_ack = push(
        &mut s,
        service_request(
            "events.ack",
            "node",
            json!({"name":"watch","throughSequence":999}),
        ),
    );
    let mut ack = service_request(
        "events.ack",
        "node",
        json!({"name":"watch","throughSequence":0}),
    );
    bind(
        &mut ack,
        "/request/params/throughSequence",
        delivered,
        "/value/value/throughSequence",
    );
    ok(&mut s, ack);
    let empty = ok(&mut s, subscription);
    let durable = push(
        &mut s,
        service_request(
            "events.subscribe",
            "node",
            json!({"name":"durable","filter":{},"durable":true}),
        ),
    );
    let mut secretary = connect("secretary", Some(1.0));
    secretary["request"]["params"]["serviceName"] = "secretary".into();
    ok(&mut s, secretary);
    ok(
        &mut s,
        service_request(
            "events.subscribe",
            "secretary",
            json!({"name":"durable","filter":{},"durable":true}),
        ),
    );
    let removal = service_request(
        "events.unsubscribe",
        "node",
        json!({"name":"watch","mutationId":"epoch:100000:unsubscribe"}),
    );
    let removed = ok(&mut s, removal.clone());
    let removed_replay = ok(&mut s, removal);
    let mut retry = publication;
    retry["request"]["params"]["mutationId"] = "epoch:100000:retry".into();
    retry["request"]["params"]["payload"]["n"] = 2.into();
    s.push(sql("CREATE TRIGGER kernel_event_failure BEFORE INSERT ON events WHEN NEW.topic='fixture.changed' BEGIN SELECT RAISE(ABORT,'fixture failure'); END;"));
    let failed = push(&mut s, retry.clone());
    s.push(sql("DROP TRIGGER kernel_event_failure;"));
    ok(&mut s, retry);
    s.push(op("reopen"));
    let reopened = ok(
        &mut s,
        request(
            "events.read",
            json!({"afterSequence":0,"filter":{"topics":["fixture.changed"]},"limit":10}),
        ),
    );
    let result = compare(&s);
    assert_eq!(value(&result, published), value(&result, replay));
    assert_eq!(value(&result, delivered), value(&result, held));
    error(&result, forbidden, "stale_generation");
    error(&result, invalid_payload, "invalid_arguments");
    error(&result, invalid_ack, "invalid_ack");
    assert!(
        value(&result, empty)["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    error(&result, durable, "forbidden");
    assert_eq!(value(&result, removed)["removed"], true);
    assert_eq!(value(&result, removed), value(&result, removed_replay));
    error(&result, failed, "internal_error");
    assert_eq!(
        value(&result, reopened)["items"].as_array().unwrap().len(),
        2
    );
}

fn deployment(id: &str, phase: &str, updated: &str) -> Value {
    json!({"deploymentId":id,"hostId":"host","instanceId":"worker","componentId":"fixture-worker","requestHash":format!("sha256:{}","a".repeat(64)),"previousBuild":null,"targetBuild":if phase=="preparing" {Value::Null}else{json!("target")},"observedBuild":null,"phase":phase,"createdAt":"1970-01-01T00:00:01Z","updatedAt":updated,"readiness":{"state":"not_run","message":"Fixture"}})
}
fn report(records: Vec<Value>) -> Value {
    service_request("deployments.report", "node", json!({"records":records}))
}
#[test]
fn kernel_deployment_reports_preserve_batch_rollback_terminal_identity_and_update_time_order() {
    let mut s = setup();
    ok(&mut s, connect("node", Some(1.0)));
    let preparing = deployment("deploy", "preparing", "1970-01-01T00:00:02Z");
    ok(&mut s, report(vec![preparing.clone()]));
    let prepared = deployment("deploy", "prepared", "1970-01-01T00:00:03Z");
    ok(&mut s, report(vec![prepared.clone()]));
    let mut changed_identity = prepared.clone();
    changed_identity["componentId"] = "changed".into();
    let identity = push(&mut s, report(vec![changed_identity]));
    let mut changed_target = prepared.clone();
    changed_target["targetBuild"] = "changed".into();
    let target = push(&mut s, report(vec![changed_target]));
    let mut regressed = preparing;
    regressed["targetBuild"] = "target".into();
    regressed["updatedAt"] = "1970-01-01T00:00:04Z".into();
    let phase = push(&mut s, report(vec![regressed]));
    let older = push(
        &mut s,
        report(vec![deployment(
            "deploy",
            "checking",
            "1970-01-01T00:00:02Z",
        )]),
    );
    let terminal = deployment("deploy", "succeeded", "1970-01-01T00:00:05Z");
    ok(&mut s, report(vec![terminal.clone()]));
    let mut repeat = report(vec![terminal.clone()]);
    repeat["now"] = 200000.into();
    ok(&mut s, repeat);
    let mut rewritten = terminal;
    rewritten["readiness"]["message"] = "Changed terminal".into();
    let replaced = push(&mut s, report(vec![rewritten]));
    let mut foreign = deployment("foreign", "preparing", "1970-01-01T00:00:08Z");
    foreign["hostId"] = "other".into();
    let rolled_back = push(
        &mut s,
        report(vec![
            deployment("batch", "preparing", "1970-01-01T00:00:07Z"),
            foreign.clone(),
        ]),
    );
    foreign["updatedAt"] = "2024-02-30T00:00:00Z".into();
    let date_before_owner = push(&mut s, report(vec![foreign]));
    s.push(sql("CREATE TRIGGER kernel_deployment_failure BEFORE INSERT ON deployment_reports WHEN NEW.deployment_id='retry' BEGIN SELECT RAISE(ABORT,'fixture failure'); END;"));
    let retry = report(vec![deployment(
        "retry",
        "preparing",
        "1970-01-01T00:00:10Z",
    )]);
    let failed = push(&mut s, retry.clone());
    s.push(sql("DROP TRIGGER kernel_deployment_failure;"));
    ok(&mut s, retry);
    // The original compares admitted timestamp strings for progression.
    let lexical = push(
        &mut s,
        report(vec![deployment(
            "retry",
            "prepared",
            "1970-01-01T00:00:10.100Z",
        )]),
    );
    let first = ok(&mut s, request("deployments.list", json!({"limit":1})));
    let next = ok(
        &mut s,
        follow(request("deployments.list", json!({"limit":1})), first),
    );
    let get = ok(
        &mut s,
        request(
            "deployments.get",
            json!({"hostId":"host","deploymentId":"deploy"}),
        ),
    );
    let absent = push(
        &mut s,
        request(
            "deployments.get",
            json!({"hostId":"host","deploymentId":"batch"}),
        ),
    );
    s.push(op("reopen"));
    let stale = ok(
        &mut s,
        request(
            "deployments.get",
            json!({"hostId":"host","deploymentId":"deploy"}),
        ),
    );
    s.push(sql(
        "UPDATE deployment_reports SET record_json='[' WHERE deployment_id='retry';",
    ));
    let corrupt_list = push(
        &mut s,
        request(
            "deployments.list",
            json!({"hostId":"unrelated","cursor":"invalid"}),
        ),
    );
    let corrupt_get = push(
        &mut s,
        request(
            "deployments.get",
            json!({"hostId":"unrelated","deploymentId":"missing"}),
        ),
    );
    let result = compare(&s);
    for index in [identity, target, rolled_back] {
        error(&result, index, "target_conflict");
    }
    for index in [phase, older, replaced, lexical] {
        error(&result, index, "revision_conflict");
    }
    error(&result, date_before_owner, "invalid_arguments");
    for index in [failed, corrupt_list, corrupt_get] {
        error(&result, index, "internal_error");
    }
    assert_eq!(
        result["rows"][rolled_back]["state"]["deploymentReports"],
        result["rows"][rolled_back - 1]["state"]["deploymentReports"]
    );
    assert_eq!(
        value(&result, first)["items"][0]["record"]["deploymentId"],
        "retry"
    );
    assert_eq!(
        value(&result, next)["items"][0]["record"]["deploymentId"],
        "deploy"
    );
    assert_eq!(value(&result, get)["stale"], false);
    assert_eq!(value(&result, stale)["stale"], true);
    error(&result, absent, "not_found");
}

#[test]
fn kernel_retention_bindings_and_fallible_catalog_keys_keep_cursor_error_precedence() {
    let mut s = setup();
    let preview = ok(&mut s, request("retention.preview", json!({})));
    let status = ok(&mut s, request("retention.status", json!({})));
    let mut pager = op("pageError");
    pager["params"] = json!({"limit":1,"cursor":"invalid"});
    let invalid = push(&mut s, pager.clone());
    pager["params"].as_object_mut().unwrap().remove("cursor");
    let key_error = push(&mut s, pager);
    ok(
        &mut s,
        request("contracts.list", json!({"key":"fixture/hive","limit":1})),
    );
    let result = compare(&s);
    assert_eq!(
        value(&result, preview)["checkedAt"],
        value(&result, status)["checkedAt"]
    );
    error(&result, invalid, "invalid_cursor");
    error(&result, key_error, "fixture_failure");
}
