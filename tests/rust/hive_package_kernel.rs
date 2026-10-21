use super::*;
use ivy_contracts::IvyResult;
use ivy_hive::{
    package_kernel::{self, CatalogRequest, KernelCatalog},
    registry::Registry,
    retention::Runtime,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

#[derive(Default)]
pub(super) struct Trace {
    catalog: Option<KernelCatalog>,
    registry: Option<Rc<RefCell<Registry>>>,
    runtime: Option<Rc<dyn Runtime>>,
    clock: Rc<Cell<f64>>,
    build: JsString,
}
impl Trace {
    pub(super) fn reset(&mut self) {
        self.catalog = None;
        self.registry = None;
        self.runtime = None;
    }
    pub(super) fn execute(
        &mut self,
        store: &mut Store,
        step: &Value,
        profile: &Value,
    ) -> IvyResult<IvyJson> {
        self.clock.set(step["now"].as_f64().unwrap());
        if self.catalog.is_none() || step["method"] == "constructor" {
            let dates = profile["dates"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| {
                    json!([
                        p[0].as_str().unwrap().encode_utf16().collect::<Vec<_>>(),
                        p[1]
                    ])
                })
                .collect::<Vec<_>>();
            let runtime = Rc::new(registry::ObservedRuntime::new(
                &json!({"dates":dates,"iso":profile["iso"],"localeOrder":[]}),
            )) as Rc<dyn Runtime>;
            let registry = Rc::new(RefCell::new(Registry::new(
                store,
                runtime.as_ref(),
                self.clock.get(),
            )?));
            self.build = step["buildId"].as_str().unwrap_or("kernel-build").into();
            let clock = self.clock.clone();
            let catalog = package_kernel::catalog(
                store,
                registry.clone(),
                self.build.clone(),
                runtime.clone(),
                Rc::new(move || clock.get()),
            )?;
            self.catalog = Some(catalog);
            self.registry = Some(registry);
            self.runtime = Some(runtime);
        }
        let catalog = self.catalog.as_ref().unwrap();
        let registry = self.registry.as_ref().unwrap();
        let runtime = self.runtime.as_ref().unwrap();
        let params = IvyJson::from_serde(&step["params"]).unwrap();
        match step["method"].as_str().unwrap() {
            "constructor" => Ok(IvyJson::Null),
            "protected" => {
                let mut builds = package_kernel::protected_builds(
                    store,
                    &registry.borrow(),
                    &self.build,
                    runtime.as_ref(),
                    &|| self.clock.get(),
                )?
                .into_iter()
                .collect::<Vec<_>>();
                builds.sort();
                Ok(IvyJson::Array(
                    builds.into_iter().map(IvyJson::String).collect(),
                ))
            }
            "save" => {
                registry.borrow_mut().save(
                    store,
                    params.get("node").unwrap(),
                    params
                        .get("notify")
                        .and_then(IvyJson::as_bool)
                        .unwrap_or(true),
                )?;
                Ok(IvyJson::Null)
            }
            "worker" => {
                let message = params.get("message").unwrap();
                let null = IvyJson::Null;
                let request = match message.get("action").unwrap().as_str()?.as_str() {
                    "package.catalog" => CatalogRequest::Catalog {
                        after: message.get("after").unwrap_or(&null),
                        include_uis: message
                            .get("includeUis")
                            .and_then(IvyJson::as_bool)
                            .unwrap_or(false),
                    },
                    "package.seed" => CatalogRequest::Seed {
                        catalog: message.get("catalog").unwrap_or(&null),
                    },
                    "package.prune" => CatalogRequest::Prune,
                    "package.publish" => CatalogRequest::Publish {
                        input: message.get("input").unwrap_or(&null),
                        publisher_principal_id: message
                            .get("publisherPrincipalId")
                            .unwrap()
                            .as_js_string()
                            .unwrap(),
                    },
                    _ => panic!("Unexpected test package action"),
                };
                let principals = params
                    .get("principals")
                    .and_then(IvyJson::as_array)
                    .unwrap_or(&[])
                    .iter()
                    .map(|p| p.as_js_string().unwrap().clone())
                    .collect::<Vec<_>>();
                package_kernel::dispatch(
                    catalog,
                    store,
                    request,
                    &principals,
                    self.clock.get() as u64,
                    &step["nowText"].as_str().unwrap().into(),
                )
            }
            _ => panic!("Unexpected package kernel fixture method"),
        }
    }
}
fn op(method: &str, params: Value) -> Value {
    json!({"op":"packageKernel","method":method,"params":params,"now":100000,"nowText":"1970-01-01T00:01:40.000Z"})
}
fn worker(action: &str, params: Value, principals: Vec<&str>) -> Value {
    let mut message = params;
    message["action"] = action.into();
    op("worker", json!({"message":message,"principals":principals}))
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
    vec![
        sql(
            "UPDATE metadata SET value='epoch' WHERE key='runtime_epoch'; INSERT INTO principals VALUES('publisher'),('other');",
        ),
        op("constructor", json!({})),
    ]
}
fn authorization(component: &str, version: &str, kind: &str) -> Value {
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
    v["publisherPrincipalId"] = "publisher".into();
    v
}
fn seed(entries: Vec<Value>) -> Value {
    worker(
        "package.seed",
        json!({"catalog":{"schemaVersion":1,"revision":entries.len(),"packages":entries}}),
        vec!["publisher"],
    )
}
fn compare(steps: &[Value]) -> Value {
    let root = Root::new();
    let observed = oracle(&root.0.join("observed.sqlite"), steps);
    let actual = native_with_runtime(
        &root.0.join("native.sqlite"),
        steps,
        Some(&observed["retentionRuntime"]),
    );
    let mut original = steps.to_vec();
    let mut previous = 0;
    for (i, step) in original.iter_mut().enumerate() {
        let mutations = actual["rows"][i]["catalogMutations"].as_array().unwrap();
        if step["op"] == "packageKernel" {
            let mut ids = vec![];
            if mutations.len() > previous {
                assert_eq!(mutations.len(), previous + 1);
                ids.push(
                    mutations.last().unwrap()["mutation_id"]
                        .as_str()
                        .unwrap()
                        .rsplit(':')
                        .next()
                        .unwrap()
                        .to_owned(),
                );
                if step["params"]["message"]["action"] == "package.seed" {
                    ids.push(
                        actual["rows"][i]["value"]["objectId"]
                            .as_str()
                            .unwrap()
                            .to_owned(),
                    );
                }
            } else {
                ids.push(format!("00000000-0000-4000-8000-{i:012}"));
                ids.push(format!("00000000-0001-4000-8000-{i:012}"));
            }
            for id in &ids {
                assert_eq!(uuid::Uuid::parse_str(id).unwrap().get_version_num(), 4);
            }
            step["uuids"] = json!(ids);
        }
        previous = mutations.len();
    }
    let expected = oracle(&root.0.join("reference.sqlite"), &original);
    for (i, (a, b)) in actual["rows"]
        .as_array()
        .unwrap()
        .iter()
        .zip(expected["rows"].as_array().unwrap())
        .enumerate()
    {
        assert_eq!(a, b, "Package kernel step{i}: {}", steps[i]);
        if steps[i]["op"] == "sql" || steps[i]["requireSuccess"] == true {
            assert!(a["error"].is_null(), "Fixture{i}: {}", a["error"]);
        }
    }
    for key in ["definitions", "metadata", "topics"] {
        assert_eq!(actual[key], expected[key]);
    }
    expected
}
fn error(r: &Value, index: usize, code: &str) {
    assert_eq!(r["rows"][index]["error"]["data"]["code"], code);
}
fn service_node(id: &str, build: &str, connected: bool) -> Value {
    json!({"serviceNodeId":id,"serviceName":"fixture","hostId":"host","buildId":build,"connected":connected,"ready":false,"lastContactAt":"1970-01-01T00:00:00.000Z"})
}
fn insert_node(node: Value) -> Value {
    let id = node["serviceNodeId"].as_str().unwrap();
    sql(format!(
        "INSERT INTO service_nodes VALUES('{}','publisher','host','fixture',1,'{}');",
        id,
        node.to_string().replace('\'', "''")
    ))
}
fn observation(host: &str, node: &str, status: Value) -> Value {
    sql(format!(
        "INSERT INTO host_reports VALUES ('{host}','{node}',1,'1970-01-01T00:00:00.000Z','{}');",
        json!({"sequence":1,"status":status})
            .to_string()
            .replace('\'', "''")
    ))
}

#[test]
fn package_kernel_worker_actions_keep_current_first_principal_guard_order_and_publisher_identity() {
    let mut s = setup();
    let missing = push(
        &mut s,
        worker("package.seed", json!({"catalog":null}), vec![]),
    );
    let empty = push(
        &mut s,
        worker("package.prune", json!({}), vec!["", "publisher"]),
    );
    let no_catalog = push(
        &mut s,
        worker(
            "package.catalog",
            json!({"after":0,"includeUis":true}),
            vec![],
        ),
    );
    let seeded = push(
        &mut s,
        seed(vec![
            entry("service", "1.0.0", "service", 1),
            entry("ui", "1.0.0", "app", 2),
        ]),
    );
    let filtered = push(
        &mut s,
        worker(
            "package.catalog",
            json!({"after":0,"includeUis":false}),
            vec![],
        ),
    );
    let guard = push(&mut s, worker("package.prune", json!({}), vec![]));
    let published = push(
        &mut s,
        worker(
            "package.publish",
            json!({"input":authorization("service","2.0.0","service"),"publisherPrincipalId":"other"}),
            vec![],
        ),
    );
    let pruned = push(&mut s, worker("package.prune", json!({}), vec!["other"]));
    s.push(json!({"op":"reopen"}));
    let reopened = push(
        &mut s,
        worker(
            "package.catalog",
            json!({"after":2,"includeUis":true}),
            vec![],
        ),
    );
    let r = compare(&s);
    error(&r, missing, "service_unavailable");
    error(&r, empty, "service_unavailable");
    error(&r, no_catalog, "service_unavailable");
    error(&r, guard, "service_unavailable");
    assert!(r["rows"][seeded]["error"].is_null());
    assert_eq!(
        r["rows"][filtered]["value"]["catalog"]["packages"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        r["rows"][published]["value"]["publisherPrincipalId"],
        "other"
    );
    assert!(r["rows"][pruned]["error"].is_null());
    assert_eq!(
        r["rows"][reopened]["value"]["catalog"]["packages"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn package_kernel_live_registry_and_offline_host_observations_protect_each_build_source() {
    let builds = (1..=10)
        .map(|v| ivy_contracts::hash::digest(format!("service/{v}.0.0")))
        .collect::<Vec<_>>();
    let mut s = setup();
    s[1]["buildId"] = builds[0].clone().into();
    s.push(insert_node(service_node("live", &builds[1], true)));
    s.push(insert_node(service_node("offline", &builds[2], false)));
    s.push(observation("host","offline",json!({"executor":{"buildId":builds[3]},"instances":[{"installedBuild":builds[4],"observedBuild":builds[5]}],"unfinished":[{"previousBuild":builds[6],"targetBuild":builds[7],"observedBuild":builds[0]}]})));
    let protected = push(&mut s, op("protected", json!({})));
    s.push(seed(
        (1..=10)
            .map(|v| entry("service", &format!("{v}.0.0"), "service", v))
            .collect(),
    ));
    let retained = push(
        &mut s,
        worker("package.prune", json!({}), vec!["publisher"]),
    );
    s.push(sql(
        "DELETE FROM host_reports; DELETE FROM service_nodes WHERE id='offline';",
    ));
    s.push(op(
        "save",
        json!({"node":service_node("live",&builds[8],false)}),
    ));
    let changed = push(&mut s, op("protected", json!({})));
    let removed = push(
        &mut s,
        worker("package.prune", json!({}), vec!["publisher"]),
    );
    let catalog = push(
        &mut s,
        worker(
            "package.catalog",
            json!({"after":0,"includeUis":true}),
            vec![],
        ),
    );
    let r = compare(&s);
    assert_eq!(r["rows"][protected]["value"].as_array().unwrap().len(), 8);
    assert_eq!(r["rows"][retained]["value"], json!([]));
    assert_eq!(r["rows"][changed]["value"].as_array().unwrap().len(), 2);
    assert_eq!(r["rows"][removed]["value"].as_array().unwrap().len(), 7);
    assert_eq!(
        r["rows"][catalog]["value"]["catalog"]["packages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["version"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["1.0.0", "9.0.0", "10.0.0"]
    );
}
#[test]
fn package_kernel_dangling_host_and_corrupt_live_registry_fail_before_catalog_write_and_recover() {
    let mut s = setup();
    s.push(seed(
        (1..=3)
            .map(|v| entry("service", &format!("{v}.0.0"), "service", v))
            .collect(),
    ));
    let mut dangling = observation("missing", "gone", json!({"instances":[],"unfinished":[]}));
    dangling["sql"] = format!(
        "PRAGMA foreign_keys=OFF;{} PRAGMA foreign_keys=ON;",
        dangling["sql"].as_str().unwrap()
    )
    .into();
    s.push(dangling);
    let failed = push(
        &mut s,
        worker("package.prune", json!({}), vec!["publisher"]),
    );
    s.push(sql("DELETE FROM host_reports;"));
    s.push(insert_node(service_node("bad", "build", false)));
    s.push(sql(
        "UPDATE service_nodes SET status_json='null' WHERE id='bad';",
    ));
    let corrupt = push(
        &mut s,
        worker(
            "package.publish",
            json!({"input":authorization("service","4.0.0","service"),"publisherPrincipalId":"publisher"}),
            vec![],
        ),
    );
    s.push(sql("DELETE FROM service_nodes;"));
    let retry = push(
        &mut s,
        worker(
            "package.publish",
            json!({"input":authorization("service","4.0.0","service"),"publisherPrincipalId":"publisher"}),
            vec![],
        ),
    );
    let r = compare(&s);
    error(&r, failed, "not_found");
    error(&r, corrupt, "internal_error");
    assert!(r["rows"][retry]["error"].is_null());
    assert_eq!(
        r["rows"][failed]["catalogMutations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        r["rows"][corrupt]["catalogMutations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn package_kernel_retained_host_iteration_and_optional_builds_keep_original_failure_boundaries() {
    let mut s = setup();
    s.push(insert_node(service_node("offline", "node-build", false)));
    s.push(observation(
        "host",
        "offline",
        json!({"executor":null,"instances":[],"unfinished":[]}),
    ));
    let healthy = push(&mut s, op("protected", json!({})));
    let mut failed = vec![];
    for status in [
        json!({"instances":null,"unfinished":[]}),
        json!({"instances":[null],"unfinished":[]}),
        json!({"instances":[],"unfinished":[null]}),
        json!({"instances":7,"unfinished":[]}),
    ] {
        let snapshot = json!({"sequence":1,"status":status});
        s.push(sql(format!(
            "UPDATE host_reports SET snapshot_json='{}';",
            snapshot.to_string().replace('\'', "''")
        )));
        failed.push(push(&mut s, op("protected", json!({}))));
    }
    s.push(sql(format!("UPDATE host_reports SET snapshot_json='{}';",json!({"sequence":1,"status":{"executor":{"buildId":""},"instances":"characters","unfinished":"later"}}).to_string().replace('\'',"''"))));
    let strings = push(&mut s, op("protected", json!({})));
    let r = compare(&s);
    for i in failed {
        error(&r, i, "internal_error");
    }
    assert_eq!(r["rows"][healthy]["value"], r["rows"][strings]["value"]);
    assert_eq!(r["rows"][strings]["value"].as_array().unwrap().len(), 2);
}
