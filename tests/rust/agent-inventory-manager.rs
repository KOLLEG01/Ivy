#[path = "agent-inventory-manager/events.rs"]
mod events;
#[path = "agent-inventory-manager/paging.rs"]
mod paging;
#[path = "agent-inventory-manager/policy.rs"]
mod policy;
#[path = "agent-inventory-results.rs"]
mod result_owner_cases;
#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use ivy_agent_manager::notice_policy::NativeNoticePolicy;
use ivy_agent_manager::{
    inventory::{InventorySignal, NativeInventory, publish_native_projects},
    inventory_manager::{InventoryConnection, InventoryManager},
    native_journal::{Limits, NativeJournal, Owner},
    project_locations::ProjectLocations,
    project_model::{NativeProjects, ProjectMembership, ProjectSnapshot},
    project_policy::ProjectPolicy,
    project_repository::WorkspaceGit,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, Outcome, json::parse};
use ivy_host_runtime::node_path;
use ivy_sdk::{
    rpc::{RequestFuture, RequestOptions, RpcClient},
    service::InvocationContext,
};
use std::sync::{Arc, Mutex, OnceLock, atomic::AtomicBool};
fn field<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson {
    value.get(key).unwrap()
}
fn text(value: &IvyJson, key: &str) -> String {
    field(value, key).as_str().unwrap()
}
fn array<'a>(value: &'a IvyJson, key: &str) -> &'a [IvyJson] {
    field(value, key).as_array().unwrap()
}
fn data() -> &'static IvyJson {
    static DATA: OnceLock<IvyJson> = OnceLock::new();
    DATA.get_or_init(|| {
        let result = reference_data::read(include_bytes!(
            "../fixtures/agent-inventory-manager-cases.json.gz"
        ));
        parse(std::str::from_utf8(&result).unwrap(), 8 * 1024 * 1024).unwrap()
    })
}
fn outcome(value: IvyResult<IvyJson>) -> IvyJson {
    match value {
        Ok(value) => IvyJson::object([("result", value)]),
        Err(error) => IvyJson::object([("error", error.to_wire())]),
    }
}
fn sorted(value: IvyJson) -> IvyJson {
    if value.as_object().is_some() {
        let mut pairs = value.into_object().unwrap();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        IvyJson::Object(pairs.into_iter().map(|(k, v)| (k, sorted(v))).collect())
    } else if value.as_array().is_some() {
        IvyJson::Array(
            value
                .into_array()
                .unwrap()
                .into_iter()
                .map(sorted)
                .collect(),
        )
    } else {
        value
    }
}
fn same(actual: IvyJson, expected: &IvyJson, name: &str, index: usize) {
    assert_eq!(
        sorted(actual),
        sorted(expected.clone()),
        "{name} step {index}"
    );
}
fn policy_snapshot(policy: &NativeNoticePolicy) -> IvyResult<IvyJson> {
    let mut values = Vec::new();
    for id in array(data(), "checks") {
        let key = id.as_js_string().unwrap().clone();
        values.push((
            key.clone(),
            policy
                .is_internal(&key)?
                .map(IvyJson::Bool)
                .unwrap_or(IvyJson::Null),
        ));
    }
    Ok(IvyJson::object([
        ("revision", IvyJson::Number(policy.revision())),
        ("internal", IvyJson::Object(values)),
    ]))
}

fn project(id: &str) -> IvyJson {
    IvyJson::object([
        ("nativeId", id.into()),
        ("source", "native".into()),
        ("name", "IvyInternal".into()),
        ("paths", IvyJson::Array(vec!["/work".into()])),
        ("position", IvyJson::Number(9_007_199_254_740_991.0)),
        ("recencyAt", IvyJson::Null),
    ])
}

#[test]
fn shared_projects_preserve_opaque_text_integer_bounds_and_original_membership() {
    let mut value = project("native");
    value.insert("name", IvyJson::String(JsString(vec![0xd800])));
    value.insert(
        "paths",
        IvyJson::Array(vec![IvyJson::String(JsString(vec![47, 0xd800]))]),
    );
    let projects = NativeProjects::from_summaries(&[value.clone()]).unwrap();
    assert_eq!(projects.entries()[0].position, 9_007_199_254_740_991);
    assert_eq!(projects.entries()[0].to_json(), value);
    let thread = IvyJson::object([("cwd", IvyJson::String(JsString(vec![47, 0xd800, 47, 120])))]);
    assert_eq!(
        projects
            .for_thread(&ProjectMembership::from_thread(&thread))
            .unwrap()
            .native_id,
        JsString::from("native")
    );
    for id in [IvyJson::Null, "".into(), "missing".into()] {
        let mut explicit = thread.clone();
        explicit.insert("projectId", id);
        assert!(
            projects
                .for_thread(&ProjectMembership::from_thread(&explicit))
                .is_none()
        );
    }
    value.insert("position", IvyJson::Number(9_007_199_254_740_992.0));
    assert!(NativeProjects::from_summaries(&[value]).is_err());
}

#[test]
fn project_heap_budget_rejects_a_set_that_fits_the_wire_budget() {
    let mut values = Vec::new();
    for i in 0..256 {
        let mut value = project(&format!("p{i}"));
        value.insert(
            "paths",
            IvyJson::Array(vec![IvyJson::from("a".repeat(2048)); 10]),
        );
        values.push(value);
    }
    assert!(
        ivy_contracts::json::canonical(&IvyJson::Array(values.clone()), 9 * 1024 * 1024).is_ok()
    );
    assert_eq!(
        NativeProjects::from_summaries(&values).unwrap_err().code,
        "native_inventory_capacity"
    );
}

#[test]
fn typed_workspace_selection_keeps_exact_roots_longest_path_ties_and_explicit_ids() {
    let root = std::env::temp_dir().join("ivy-project-policy");
    let child = root.join("project");
    let internal = root.join("internal");
    let policy = ProjectPolicy::new(
        node_path::from_native(&root.join("data")),
        &IvyJson::object([(
            "internalProjectRoot",
            IvyJson::String(node_path::from_native(&internal)),
        )]),
        &[],
        node_path::Context::current().unwrap(),
        &node_path::from_native(&root),
    )
    .unwrap();
    let mut first = project("root");
    first.insert(
        "paths",
        IvyJson::Array(vec![
            IvyJson::String(node_path::from_native(&root)),
            IvyJson::String(node_path::from_native(&internal)),
        ]),
    );
    let mut second = project("child");
    second.insert(
        "paths",
        IvyJson::Array(vec![IvyJson::String(node_path::from_native(&child))]),
    );
    let mut tied = second.clone();
    tied.insert("nativeId", "tie".into());
    let projects = NativeProjects::from_summaries(&[first.clone(), second.clone(), tied]).unwrap();
    let cwd = node_path::from_native(&child.join("nested"));
    assert_eq!(
        policy
            .workspace_project_id(projects.entries(), &cwd, None)
            .unwrap(),
        Some("child".into())
    );
    assert_eq!(
        policy
            .workspace_project_id(projects.entries(), &cwd, Some(&"explicit".into()))
            .unwrap(),
        Some("explicit".into())
    );
    assert_eq!(
        policy
            .workspace_project_id(projects.entries(), &cwd, Some(&"".into()))
            .unwrap(),
        Some("".into())
    );
    assert_eq!(
        policy
            .existing_project(projects.entries(), &node_path::from_native(&child), None)
            .unwrap(),
        Some(second)
    );
    assert_eq!(
        policy
            .existing_project(
                projects.entries(),
                &node_path::from_native(&child),
                Some(&"root".into())
            )
            .unwrap_err()
            .code,
        "target_conflict"
    );
    assert_eq!(
        policy.internal_project(projects.entries(), None).unwrap(),
        first
    );
    assert_eq!(
        policy
            .internal_project(projects.entries(), Some(&"child".into()))
            .unwrap_err()
            .code,
        "native_internal_project_missing"
    );
}

struct ConflictPublication {
    calls: Mutex<Vec<IvyJson>>,
    mode: &'static str,
}
impl RpcClient for ConflictPublication {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        assert_eq!(method, "inventory.sync");
        let mut calls = self.calls.lock().unwrap();
        let first = calls.is_empty();
        let details = IvyJson::object([
            (
                "serviceNodeId",
                if self.mode == "wrong_owner" {
                    "another.agent"
                } else {
                    "fixture.agent"
                }
                .into(),
            ),
            ("namespace", params.required("namespace").unwrap().clone()),
            ("kind", "project".into()),
            (
                "currentRevision",
                IvyJson::Number(
                    params
                        .required("snapshotRevision")
                        .unwrap()
                        .as_f64()
                        .unwrap()
                        + 10.0,
                ),
            ),
        ]);
        calls.push(params);
        let mode = self.mode;
        Box::pin(async move {
            if first {
                Err(IvyError::new("revision_conflict", "fixture")
                    .with_details(details)
                    .with_outcome(if mode == "unknown" {
                        Outcome::Unknown
                    } else {
                        Outcome::NotExecuted
                    }))
            } else {
                Ok(IvyJson::Null)
            }
        })
    }
}

#[tokio::test]
async fn typed_empty_project_publication_keeps_source_time_across_verified_retry() {
    let snapshot = ProjectSnapshot {
        observed_at: "2026-01-01T00:00:00.000Z".into(),
        projects: NativeProjects::from_summaries(&[]).unwrap(),
    };
    let client = ConflictPublication {
        calls: Mutex::new(vec![]),
        mode: "owner",
    };
    publish_native_projects(&client, &journal(), &snapshot, None)
        .await
        .unwrap();
    let calls = client.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    for call in calls.iter() {
        ivy_contracts::core_schema::validate_input("inventory.sync", call).unwrap();
        assert_eq!(
            call.get("observedAt").and_then(IvyJson::as_js_string),
            Some(&snapshot.observed_at)
        );
        assert!(
            call.required("entries")
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    assert!(
        calls[1].get("snapshotRevision").unwrap().as_f64().unwrap()
            > calls[0].get("snapshotRevision").unwrap().as_f64().unwrap()
    );
}

#[tokio::test]
async fn typed_project_publication_retries_only_a_verified_not_executed_conflict() {
    let snapshot = ProjectSnapshot {
        observed_at: "2026-01-01T00:00:00.000Z".into(),
        projects: NativeProjects::from_summaries(&[project("p")]).unwrap(),
    };
    for mode in ["owner", "wrong_owner", "unknown"] {
        let client = ConflictPublication {
            calls: Mutex::new(vec![]),
            mode,
        };
        let result = publish_native_projects(&client, &journal(), &snapshot, None).await;
        let calls = client.calls.lock().unwrap();
        if mode == "owner" {
            result.unwrap();
            assert_eq!(calls.len(), 2);
            assert_eq!(
                calls[0].required("entries").unwrap(),
                calls[1].required("entries").unwrap()
            );
            assert_eq!(
                calls[0].required("namespace").unwrap(),
                calls[1].required("namespace").unwrap()
            );
            assert_eq!(
                calls[0].required("schemaVersion").unwrap(),
                calls[1].required("schemaVersion").unwrap()
            );
            assert!(
                calls[1]
                    .required("snapshotRevision")
                    .unwrap()
                    .as_f64()
                    .unwrap()
                    > calls[0]
                        .required("snapshotRevision")
                        .unwrap()
                        .as_f64()
                        .unwrap()
                        + 10.0
            );
            let entry = &calls[0].required("entries").unwrap().as_array().unwrap()[0];
            assert_eq!(entry.required("summary").unwrap(), &project("p"));
            assert_eq!(
                entry.required("observedAt").unwrap().as_js_string(),
                Some(&snapshot.observed_at)
            );
        } else {
            assert_eq!(
                calls.len(),
                1,
                "unknown or another owner's result never authorizes retry"
            );
            assert_eq!(
                result.unwrap_err().code,
                if mode == "wrong_owner" {
                    "target_conflict"
                } else {
                    "revision_conflict"
                }
            );
        }
    }
    let client = Published::default();
    let context = InvocationContext::detached(
        "fixture".into(),
        None,
        1,
        Default::default(),
        IvyError::new("fixture_generation_retired", ""),
    );
    context.signal.cancel();
    let signal = InventorySignal::from_context(context);
    assert_eq!(
        publish_native_projects(&client, &journal(), &snapshot, Some(&signal))
            .await
            .unwrap_err()
            .code,
        "fixture_generation_retired"
    );
    assert!(client.0.lock().unwrap().is_empty());
}

#[derive(Default)]
struct Published(Mutex<Vec<IvyJson>>);
impl RpcClient for Published {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        assert_eq!(method, "inventory.sync");
        self.0.lock().unwrap().push(params);
        Box::pin(async { Ok(IvyJson::object([] as [(&str, IvyJson); 0])) })
    }
}
fn connection(client: Arc<Published>) -> InventoryConnection {
    InventoryConnection {
        client,
        signal: InventorySignal::from_context(InvocationContext::detached(
            "fixture".into(),
            None,
            1,
            Default::default(),
            IvyError::internal(),
        )),
    }
}
fn catalog() -> IvyJson {
    parse(r#"{"clientRequests":[{"method":"thread/list","inputSchema":{"properties":{"sourceKinds":{"items":{"enum":["cli"]}}}}},{"method":"project/list"}]}"#, 4096).unwrap()
}
fn journal() -> Arc<Mutex<NativeJournal>> {
    Arc::new(Mutex::new(
        NativeJournal::with_clock(
            Owner {
                service_node_id: "fixture.agent".into(),
                native_version: "1".into(),
                native_executable_hash: format!("sha256:{}", "0".repeat(64)).into(),
            },
            Limits {
                max_operations: 8,
                max_journal_bytes: 64 * 1024 * 1024,
                max_pending_inputs: 8,
            },
            Arc::new(|| "2026-01-01T00:00:00.000Z".into()),
        )
        .unwrap(),
    ))
}

#[tokio::test]
async fn paged_thread_collection_retries_ownership_changes_and_preserves_summary_semantics() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let loaded = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let inventory = NativeInventory::with_reader(
        &catalog(),
        "epoch".into(),
        Arc::new({
            let calls = calls.clone();
            let loaded = loaded.clone();
            move |method, params, _| {
                let (entries, cursor) = if method.eq_str("thread/loaded/list") {
                    let index = loaded.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    (
                        vec![IvyJson::from(if index == 0 { "a" } else { "b" })],
                        IvyJson::Null,
                    )
                } else {
                    assert!(method.eq_str("thread/list"));
                    assert_eq!(params.get("limit"), Some(&IvyJson::Number(100.0)));
                    let archived = params.get("archived") == Some(&IvyJson::Bool(true));
                    let next = !archived && params.get("cursor").is_none();
                    let id = if archived {
                        "history"
                    } else if next {
                        "a"
                    } else {
                        "b"
                    };
                    (
                        vec![IvyJson::object([
                            ("id", id.into()),
                            ("cwd", "/work".into()),
                            ("preview", "p".repeat(4096).into()),
                            ("name", IvyJson::Null),
                            ("status", IvyJson::object([("type", "idle".into())])),
                            ("source", "cli".into()),
                            ("updatedAt", IvyJson::Number(1.0)),
                            ("ephemeral", false.into()),
                            ("canAcceptDirectInput", true.into()),
                            ("threadSource", "cli".into()),
                        ])],
                        if next { "next".into() } else { IvyJson::Null },
                    )
                };
                calls.lock().unwrap().push((method, params));
                Box::pin(async move {
                    Ok(IvyJson::object([(
                        "result",
                        IvyJson::object([
                            ("data", IvyJson::Array(entries)),
                            ("nextCursor", cursor),
                        ]),
                    )]))
                })
            }
        }),
        Arc::new(|| 0.0),
        Arc::new(|| "2026-01-01T00:00:00.000Z".into()),
    )
    .unwrap();
    let snapshot = inventory.collect_threads(None).await.unwrap();
    let threads = snapshot.required("threads").unwrap().as_array().unwrap();
    assert_eq!(threads.len(), 3);
    for entry in threads {
        let summary = entry.required("summary").unwrap();
        let attached = entry.get("nativeId") == Some(&"b".into());
        assert_eq!(
            summary.get("owner"),
            Some(&IvyJson::from(if attached {
                "this-native-connection"
            } else {
                "historical-unattached"
            }))
        );
        assert_eq!(
            summary.get("epoch"),
            Some(&if attached {
                "epoch".into()
            } else {
                IvyJson::Null
            })
        );
        assert_eq!(
            summary
                .required("preview")
                .unwrap()
                .as_js_string()
                .unwrap()
                .0
                .len(),
            2048
        );
        assert_eq!(
            summary.get("archived"),
            Some(&IvyJson::Bool(
                entry.get("nativeId") == Some(&"history".into())
            ))
        );
        assert_eq!(summary.get("projectId"), Some(&IvyJson::Null));
    }
    assert_eq!(loaded.load(std::sync::atomic::Ordering::SeqCst), 4);
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _)| method.eq_str("thread/list"))
            .count(),
        6
    );
    let malformed = NativeInventory::with_reader(
        &catalog(),
        "epoch".into(),
        Arc::new(|_, _, _| Box::pin(async { Ok(IvyJson::Null) })),
        Arc::new(|| 0.0),
        Arc::new(|| "2026-01-01T00:00:00.000Z".into()),
    )
    .unwrap();
    assert_eq!(
        malformed.collect_threads(None).await.unwrap_err().code,
        "native_inventory_invalid"
    );
}

#[tokio::test]
async fn all_project_consumers_share_one_set_and_failed_refresh_retains_it() {
    let native = IvyJson::object([
        ("id", "p".into()),
        ("name", "IvyInternal".into()),
        (
            "roots",
            IvyJson::Array(vec![IvyJson::object([("path", "/work".into())])]),
        ),
        ("position", IvyJson::Number(0.0)),
        ("recencyAt", IvyJson::Null),
    ]);
    let rows = Arc::new(Mutex::new(vec![native.clone()]));
    let inventory = Arc::new(
        NativeInventory::with_reader(
            &catalog(),
            "epoch".into(),
            Arc::new({
                let rows = rows.clone();
                move |method, _, _| {
                    assert!(method.eq_str("project/list"));
                    let data = rows.lock().unwrap().clone();
                    Box::pin(async move {
                        Ok(IvyJson::object([(
                            "result",
                            IvyJson::object([
                                ("data", IvyJson::Array(data)),
                                ("nextCursor", IvyJson::Null),
                            ]),
                        )]))
                    })
                }
            }),
            Arc::new(|| 0.0),
            Arc::new(|| "2026-01-01T00:00:00.000Z".into()),
        )
        .unwrap(),
    );
    let policy = Arc::new(Mutex::new(NativeNoticePolicy::default()));
    policy.lock().unwrap().set_thread(
        "t".into(),
        &IvyJson::object([("projectId", "p".into())]),
        None,
    );
    let root = std::env::temp_dir();
    let locations = ProjectLocations::new(
        ProjectPolicy::new(
            node_path::from_native(&root),
            &IvyJson::object([] as [(&str, IvyJson); 0]),
            &[],
            node_path::Context::current().unwrap(),
            &node_path::from_native(&root),
        )
        .unwrap(),
        None,
        None,
        Arc::new(WorkspaceGit::new(root, None)),
    );
    let manager = InventoryManager::new(
        inventory.clone(),
        journal(),
        policy.clone(),
        Arc::new({
            let locations = locations.clone();
            move |projects| locations.set_native_projects(projects)
        }),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    let client = Arc::new(Published::default());
    manager.bind(connection(client.clone())).unwrap();
    manager
        .refresh_projects(connection(client.clone()))
        .unwrap()
        .wait()
        .await
        .unwrap();
    let shared = inventory
        .current_project_snapshot()
        .unwrap()
        .unwrap()
        .projects;
    assert!(Arc::ptr_eq(
        &shared,
        &manager.project_model().unwrap().projects
    ));
    assert_eq!(
        Arc::strong_count(&shared),
        5,
        "inventory, manager, policy, locations and this handle"
    );
    assert_eq!(
        policy.lock().unwrap().is_internal(&"t".into()).unwrap(),
        Some(true)
    );
    assert_eq!(client.0.lock().unwrap().len(), 1);
    let mut response = manager.project_snapshot().unwrap();
    response.insert("projects", IvyJson::Null);
    assert_eq!(manager.project_model().unwrap().projects.entries().len(), 1);
    *rows.lock().unwrap() = vec![native.clone(), native];
    assert_eq!(
        manager
            .refresh_projects(connection(client.clone()))
            .unwrap()
            .wait()
            .await
            .unwrap_err()
            .code,
        "native_inventory_invalid"
    );
    assert!(Arc::ptr_eq(
        &shared,
        &inventory
            .current_project_snapshot()
            .unwrap()
            .unwrap()
            .projects
    ));
    assert!(Arc::ptr_eq(
        &shared,
        &manager.project_model().unwrap().projects
    ));
    assert_eq!(
        policy.lock().unwrap().is_internal(&"t".into()).unwrap(),
        Some(true)
    );
    assert_eq!(
        client.0.lock().unwrap().len(),
        1,
        "invalid partial discovery is never published"
    );
    manager.bind(connection(client.clone())).unwrap();
    manager
        .ensure_projects(connection(client.clone()))
        .await
        .unwrap();
    assert!(Arc::ptr_eq(
        &shared,
        &manager.project_model().unwrap().projects
    ));
    assert_eq!(client.0.lock().unwrap().len(), 1);
    manager.drain().await.unwrap();
}

#[test]
fn notice_policy_matches_original_membership_revisions_and_push_events() {
    let rows = array(data(), "policyRows");
    assert_eq!(rows.len(), 21);
    for row in rows {
        let mut owner = NativeNoticePolicy::default();
        for (index, step) in array(row, "commands").iter().enumerate() {
            let result = (|| {
                match text(step, "action").as_str() {
                    "setProjects" => {
                        owner.set_projects(NativeProjects::from_summaries(array(step, "projects"))?)
                    }
                    "setThread" => owner.set_thread(
                        field(step, "id").as_js_string().unwrap().clone(),
                        field(step, "value"),
                        step.get("revision").and_then(IvyJson::as_f64),
                    ),
                    "setThreads" => owner.set_threads(
                        array(step, "entries"),
                        field(step, "revision").as_f64().unwrap(),
                    )?,
                    "observe" => {
                        let event = field(step, "event");
                        let method = event
                            .get("method")
                            .and_then(IvyJson::as_js_string)
                            .cloned()
                            .unwrap_or_default();
                        owner.observe(&method, event.get("params").unwrap_or(&IvyJson::Null));
                    }
                    "snapshot" => return policy_snapshot(&owner),
                    action => panic!("{action}"),
                }
                Ok(IvyJson::Null)
            })();
            same(
                outcome(result),
                &array(row, "expected")[index],
                &text(row, "name"),
                index,
            );
        }
    }
}

#[tokio::test]
async fn slow_project_observation_and_rebinding_keep_the_original_cached_time() {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use tokio::sync::{Notify, Semaphore};
    let ticks = Arc::new(AtomicU64::new(0));
    let reads = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Semaphore::new(0));
    let empty = Arc::new(AtomicBool::new(false));
    let instant = |ticks: u64| -> JsString {
        chrono::DateTime::from_timestamp_millis(1_767_225_600_000 + ticks as i64)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            .into()
    };
    let inventory = Arc::new(
        NativeInventory::with_reader(
            &catalog(),
            "original-epoch".into(),
            Arc::new({
                let reads = reads.clone();
                let entered = entered.clone();
                let release = release.clone();
                let empty = empty.clone();
                move |method, _, _| {
                    assert!(method.eq_str("project/list"));
                    reads.fetch_add(1, Ordering::SeqCst);
                    let entered = entered.clone();
                    let release = release.clone();
                    let empty = empty.load(Ordering::SeqCst);
                    Box::pin(async move {
                        entered.notify_one();
                        release.acquire().await.unwrap().forget();
                        Ok(IvyJson::object([(
                            "result",
                            IvyJson::object([
                                (
                                    "data",
                                    IvyJson::Array(if empty {
                                        vec![]
                                    } else {
                                        vec![IvyJson::object([
                                            ("id", "original-project".into()),
                                            ("name", "Original".into()),
                                            (
                                                "roots",
                                                IvyJson::Array(vec![IvyJson::object([(
                                                    "path",
                                                    "/work".into(),
                                                )])]),
                                            ),
                                            ("position", IvyJson::Number(0.0)),
                                            ("recencyAt", IvyJson::Null),
                                        ])]
                                    }),
                                ),
                                ("nextCursor", IvyJson::Null),
                            ]),
                        )]))
                    })
                }
            }),
            Arc::new({
                let ticks = ticks.clone();
                move || ticks.load(Ordering::SeqCst) as f64
            }),
            Arc::new({
                let ticks = ticks.clone();
                move || instant(ticks.load(Ordering::SeqCst))
            }),
        )
        .unwrap(),
    );
    let manager = InventoryManager::with_clock(
        inventory.clone(),
        journal(),
        Arc::new(Mutex::new(NativeNoticePolicy::default())),
        Arc::new(|_| Ok(())),
        Arc::new(AtomicBool::new(false)),
        Arc::new({
            let ticks = ticks.clone();
            move || ticks.load(Ordering::SeqCst) as f64
        }),
    )
    .unwrap();
    assert!(!manager.projects_observed().unwrap());
    assert!(inventory.current_project_snapshot().unwrap().is_none());
    assert_eq!(
        manager.project_model().err().unwrap().code,
        "native_inventory_unavailable"
    );
    let client = Arc::new(Published::default());
    manager.bind(connection(client.clone())).unwrap();
    ticks.store(10_000, Ordering::SeqCst);
    let work = manager
        .refresh_projects(connection(client.clone()))
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    ticks.store(70_000, Ordering::SeqCst);
    release.add_permits(1);
    work.wait().await.unwrap();
    let original = manager.project_model().unwrap();
    assert_eq!(
        original.observed_at,
        instant(10_000),
        "A slow project scan stamped cached source data with its finish time"
    );
    assert!(manager.projects_observed().unwrap());
    assert_eq!(
        client.0.lock().unwrap()[0]
            .required("entries")
            .unwrap()
            .as_array()
            .unwrap()[0]
            .get("observedAt")
            .and_then(IvyJson::as_js_string),
        Some(&original.observed_at)
    );
    ticks.store(90_000, Ordering::SeqCst);
    manager.bind(connection(client.clone())).unwrap();
    manager
        .ensure_projects(connection(client.clone()))
        .await
        .unwrap();
    let rebound = manager.project_model().unwrap();
    assert_eq!(
        rebound.observed_at, original.observed_at,
        "Binding relabelled the same cached source as newly observed"
    );
    assert!(Arc::ptr_eq(&rebound.projects, &original.projects));
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(client.0.lock().unwrap().len(), 1);

    let old = manager
        .refresh_projects(connection(client.clone()))
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let next = Arc::new(Published::default());
    manager.bind(connection(next.clone())).unwrap();
    let _queued = manager.refresh_projects(connection(next.clone())).unwrap();
    empty.store(true, Ordering::SeqCst);
    ticks.store(110_000, Ordering::SeqCst);
    release.add_permits(2);
    assert_eq!(
        old.wait().await.unwrap_err().code,
        "native_inventory_changed"
    );
    manager.drain().await.unwrap();
    assert_eq!(
        client.0.lock().unwrap().len(),
        1,
        "A retired project scan published after its original binding was replaced"
    );
    assert_eq!(next.0.lock().unwrap().len(), 1);
    assert_eq!(reads.load(Ordering::SeqCst), 3);
    let empty = manager.project_model().unwrap();
    assert!(
        manager.projects_observed().unwrap(),
        "Verified empty projects became an unknown observation"
    );
    assert!(empty.projects.entries().is_empty());
    assert_eq!(empty.observed_at, instant(110_000));
    let cached = inventory.current_project_snapshot().unwrap().unwrap();
    assert!(Arc::ptr_eq(&empty.projects, &cached.projects));
    assert_eq!(cached.observed_at, empty.observed_at);
}
