//! Real allocation and model discovery over one host's independent accounts.
use super::{Document, Memory, Stored};
use ivy_contracts::{
    IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts,
    native_backend::{BackendIdentity, NativeProvider},
    native_contract::NativeContract,
};
use ivy_sdk::{
    rpc::{RequestFuture, RequestOptions, RpcClient},
    tools::ToolClients,
};
use ivy_task_board::{
    allocation::{AllocationRuntime, AllocationStore, Allocator},
    configuration::{execution_with_defaults, supports_execution_options},
    native_intent::TaskPrompts,
    operations::Store,
};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

fn backend(id: &str, provider: NativeProvider, account: &str) -> BackendIdentity {
    BackendIdentity::new(id.into(), provider, account.into()).unwrap()
}
struct Client {
    sources: Vec<NativeContract>,
    trace: Mutex<Vec<Json>>,
    foreign: bool,
    unavailable: bool,
    node_pages: Option<Vec<Json>>,
}
impl Client {
    fn new(catalogs: &CheckedNativeContracts) -> Self {
        Self {
            sources: [
                (
                    "0.163.0",
                    backend("alpha", NativeProvider::Codex, "personal"),
                ),
                ("0.142.3", backend("beta", NativeProvider::Claude, "work")),
                ("0.142.3", backend("gamma", NativeProvider::Claude, "other")),
            ]
            .into_iter()
            .map(|(version, backend)| {
                catalogs
                    .checked_native_contract(&version.into(), None)
                    .unwrap()
                    .with_backend(Some(backend))
            })
            .collect(),
            trace: Mutex::default(),
            foreign: false,
            unavailable: false,
            node_pages: None,
        }
    }
    fn node(&self) -> Json {
        registered_host()
    }
    fn status(&self, source: &NativeContract) -> Json {
        let mut value = Json::object([
            ("hostId", "fixture".into()),
            ("serviceNodeId", "native-agent".into()),
            (
                "nativeVersion",
                source.catalog().required("version").unwrap().clone(),
            ),
            (
                "nativeExecutableHash",
                source
                    .catalog()
                    .required("nativeExecutableHash")
                    .unwrap()
                    .clone(),
            ),
            ("catalogHash", source.catalog_hash().into()),
            ("epoch", "same-epoch".into()),
            ("pid", 42.0.into()),
            (
                "state",
                if self.unavailable && source.backend().unwrap().id() == "beta" {
                    "failed"
                } else {
                    "ready"
                }
                .into(),
            ),
            ("observedAt", "2026-01-02T03:04:05.000Z".into()),
            ("code", Json::Null),
            ("initialized", Json::Object(vec![])),
            ("pendingInputs", 0.0.into()),
            ("observedMethods", Json::Array(vec![])),
            (
                "operations",
                Json::object([
                    ("retained", 0.0.into()),
                    ("maximum", 100.0.into()),
                    ("bytes", 0.0.into()),
                    ("maximumBytes", 1048576.0.into()),
                ]),
            ),
        ]);
        value.insert("backend", source.backend().unwrap().json());
        value
    }
}
pub(super) fn registered_host() -> Json {
    Json::object([
        ("hostId", "fixture".into()),
        ("serviceNodeId", "native-agent".into()),
        ("serviceName", "agent-manager".into()),
        ("instanceMode", "multiple".into()),
        ("principalId", "fixture-caller".into()),
        ("version", "0.4.33".into()),
        ("buildId", format!("sha256:{}", "a".repeat(64)).into()),
        ("hiveProtocol", 1.0.into()),
        // Hive normalizes the omitted host-level version to null.
        ("nativeVersion", Json::Null),
        ("connected", true.into()),
        ("synced", true.into()),
        ("ready", true.into()),
        ("stale", false.into()),
        ("desiredEnabled", true.into()),
        ("lastContactAt", "2026-01-02T03:04:05.000Z".into()),
        ("lastSuccessfulSyncAt", "2026-01-02T03:04:05.000Z".into()),
        ("lastFailedSyncAt", Json::Null),
        ("lastObservationAt", Json::Null),
        ("diagnostic", Json::Null),
    ])
}
impl RpcClient for Client {
    fn request<'a>(&'a self, method: &'a str, args: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            self.trace.lock().unwrap().push(Json::object([
                ("method", method.into()),
                ("args", args.clone()),
            ]));
            match method {
                "serviceNodes.list" => {
                    let page = if let Some(pages) = &self.node_pages {
                        let index = args.get("cursor").map_or(0, |cursor| {
                            cursor
                                .as_str()
                                .unwrap()
                                .strip_prefix("nodes-")
                                .unwrap()
                                .parse::<usize>()
                                .unwrap()
                        });
                        pages[index].clone()
                    } else {
                        node_page(vec![self.node()], None)
                    };
                    if page.is_null() {
                        return Err(ivy_contracts::IvyError::new(
                            "discovery_unavailable",
                            "Registry read failed.",
                        ));
                    }
                    ivy_contracts::core_schema::validate_output("serviceNodes.list", &page)?;
                    Ok(page)
                }
                "system.status" => Ok(Json::object([(
                    "runtimeEpoch",
                    "11111111-1111-4111-8111-111111111111".into(),
                )])),
                "tools.list" => {
                    let namespace = args.required("namespace")?.as_str()?;
                    let name = args.required("namePrefix")?.as_str()?;
                    let (definition, definition_hash) = if namespace == "agent" {
                        (
                            Json::object([
                                ("namespace", "agent".into()),
                                ("name", name.clone().into()),
                                ("interfaceVersion", "1.0.0".into()),
                                ("annotations", Json::object([("readOnlyHint", true.into())])),
                                ("inputSchema", Json::Object(vec![])),
                                ("outputSchema", Json::Object(vec![])),
                            ]),
                            format!("sha256:{}", "a".repeat(64)),
                        )
                    } else {
                        let source = self
                            .sources
                            .iter()
                            .find(|s| s.backend().unwrap().id() == namespace)
                            .unwrap();
                        (
                            source.definition(&name.clone().into())?,
                            source.definition_hash(&name.clone().into())?.to_owned(),
                        )
                    };
                    Ok(Json::object([
                        (
                            "provider",
                            Json::object([(
                                "node",
                                Json::object([("serviceNodeId", "native-agent".into())]),
                            )]),
                        ),
                        (
                            "items",
                            Json::Array(vec![Json::object([
                                ("qualifiedName", format!("{namespace}.{name}").into()),
                                ("definition", definition),
                                ("definitionHash", definition_hash.into()),
                                ("catalogHash", format!("sha256:{}", "b".repeat(64)).into()),
                            ])]),
                        ),
                        ("nextCursor", Json::Null),
                    ]))
                }
                "tools.call" => {
                    let name = args.required("qualifiedName")?.as_str()?;
                    let arguments = args.required("arguments")?;
                    if name == "agent.backends" {
                        let mut statuses: Vec<_> =
                            self.sources.iter().map(|s| self.status(s)).collect();
                        if self.foreign {
                            statuses[0].insert("hostId", "foreign".into());
                        }
                        return Ok(Json::object([("backends", Json::Array(statuses))]));
                    }
                    if let Some(id) = name.strip_suffix(".model/list") {
                        assert!(self.sources.iter().any(|s| s.backend().unwrap().id() == id));
                        return Ok(Json::object([
                            (
                                "data",
                                Json::Array(vec![Json::object([
                                    ("id", "shared".into()),
                                    ("model", "shared".into()),
                                    ("displayName", "Same model label".into()),
                                    ("description", "Fixture".into()),
                                    ("hidden", false.into()),
                                    ("isDefault", true.into()),
                                    ("defaultReasoningEffort", "high".into()),
                                    (
                                        "supportedReasoningEfforts",
                                        Json::Array(vec![Json::object([
                                            ("reasoningEffort", "high".into()),
                                            ("description", "High".into()),
                                        ])]),
                                    ),
                                ])]),
                            ),
                            ("nextCursor", Json::Null),
                        ]));
                    }
                    let selected = BackendIdentity::from_json(arguments.required("backend")?)?;
                    let source = self
                        .sources
                        .iter()
                        .find(|s| s.backend() == Some(&selected))
                        .expect("Exact account selector");
                    let value = match name.as_str() {
                        "agent.capabilities" => {
                            Json::object([("capabilities", Json::Array(vec![]))])
                        }
                        "agent.catalog" => return Ok(source.catalog().clone()),
                        "agent.resolveWorkspace" => Json::object([
                            ("hostId", "fixture".into()),
                            ("serviceNodeId", "native-agent".into()),
                            ("canonicalCwd", "/workspace".into()),
                            ("taskRoot", "/workspace".into()),
                            ("bootstrapPath", "/workspace".into()),
                            ("intendedPath", "/workspace".into()),
                            ("sourcePath", Json::Null),
                            ("project", Json::Null),
                            ("nativeProjectId", "project".into()),
                            ("useWorktree", false.into()),
                            ("repository", Json::Null),
                        ]),
                        _ => panic!("Unexpected native effect {name}"),
                    };
                    selected.arguments(value)
                }
                _ => panic!("Unexpected discovery RPC {method}"),
            }
        })
    }
}
struct Runtime {
    client: Arc<Client>,
    tools: Arc<ToolClients>,
    principal: JsString,
    node: JsString,
}
impl Runtime {
    fn new(client: Client) -> Self {
        let client = Arc::new(client);
        Self {
            tools: Arc::new(ToolClients::new(client.clone())),
            client,
            principal: "caller".into(),
            node: "task-board".into(),
        }
    }
}
impl TaskPrompts for Runtime {
    fn task_prompt(&self, _: &Json, _: &JsString, _: Option<&Json>) -> IvyResult<JsString> {
        Ok("Selected account prompt".into())
    }
    fn task_update_prompt(&self, _: &Json, _: &JsString, _: bool) -> IvyResult<JsString> {
        unreachable!()
    }
}
impl AllocationRuntime for Runtime {
    fn client(&self) -> Arc<dyn RpcClient> {
        self.client.clone()
    }
    fn tools(&self) -> &Arc<ToolClients> {
        &self.tools
    }
    fn principal_id(&self) -> &JsString {
        &self.principal
    }
    fn service_node_id(&self) -> &JsString {
        &self.node
    }
}
impl AllocationStore for Memory {
    async fn page(&self, _: &str, _: &Json) -> IvyResult<Json> {
        Ok(Json::object([
            ("items", Json::Array(vec![])),
            ("nextCursor", Json::Null),
        ]))
    }
    async fn workspace_owner(
        &self,
        _: &JsString,
        _: &JsString,
        _: &JsString,
    ) -> IvyResult<Option<JsString>> {
        Ok(None)
    }
    async fn named_plan(&self, name: &JsString) -> IvyResult<Option<Document>> {
        Store::named(self, "task-board/native-plan", &name.to_utf8()?, None).await
    }
}
fn options(backend: &BackendIdentity) -> Json {
    Json::object([
        ("backend", backend.json()),
        ("model", "shared".into()),
        ("reasoningEffort", "high".into()),
        ("serviceTier", "standard".into()),
    ])
}
fn task(backend: &BackendIdentity) -> Document {
    Document {
        pin: ivy_task_board::operations::ObjectPin::from_json(Json::object([
            ("objectId", "task".into()),
            ("revision", 1.0.into()),
        ]))
        .unwrap(),
        parent_id: None,
        content_hash: String::new(),
        value: Json::object([
            ("taskKey", "TASK-0001".into()),
            ("primaryResourceRef", Json::Null),
            ("lastRun", Json::Null),
            ("lastHistory", Json::Null),
            (
                "fields",
                Json::object([
                    ("nativeOptions", options(backend)),
                    ("executionRequirement", Json::Null),
                    ("requiredCapabilities", Json::Array(vec![])),
                    (
                        "workspaceRequirement",
                        Json::object([
                            ("kind", "new_project_path".into()),
                            ("path", "/workspace".into()),
                        ]),
                    ),
                ]),
            ),
        ]),
    }
}

fn node_page(items: Vec<Json>, cursor: Option<&str>) -> Json {
    Json::object([
        ("items", Json::Array(items)),
        ("nextCursor", cursor.map_or(Json::Null, Json::from)),
    ])
}

fn other_node(client: &Client, index: usize) -> Json {
    let mut node = client.node();
    node.insert("serviceNodeId", format!("other-{index:04}").into());
    node.insert("hostId", "other-host".into());
    node
}

#[tokio::test]
async fn registry_null_native_version_discovers_accounts_for_default_options() {
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let client = Arc::new(Client::new(&catalogs));
    assert!(
        supports_execution_options(client.clone(), &client.node(), &Json::Object(vec![]))
            .await
            .unwrap()
    );
    let trace = client.trace.lock().unwrap();
    assert!(trace.iter().any(|row| {
        row.required("args")
            .unwrap()
            .get("qualifiedName")
            .is_some_and(|name| name.is_str("agent.backends"))
    }));
    assert!(!trace.iter().any(|row| {
        row.required("args")
            .unwrap()
            .get("qualifiedName")
            .is_some_and(|name| name.is_str("agent.status"))
    }));
}

#[tokio::test]
async fn allocation_finds_the_selected_host_after_a_full_registry_page() {
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let mut client = Client::new(&catalogs);
    client.node_pages = Some(vec![
        node_page(
            (0..200).map(|index| other_node(&client, index)).collect(),
            Some("nodes-1"),
        ),
        node_page(vec![client.node()], None),
    ]);
    let runtime = Runtime::new(client);
    let selected = runtime.client.sources[1].backend().unwrap();
    let mut task = task(selected);
    task.value.get_mut("fields").unwrap().insert(
        "executionRequirement",
        Json::object([("kind", "host".into()), ("hostId", "fixture".into())]),
    );
    let store = Memory::default();
    let allocation = Allocator {
        store: &store,
        engine: &runtime,
        catalogs: &catalogs,
    }
    .allocate(&task, &"allocate".into(), None, false)
    .await
    .unwrap();
    assert!(BackendIdentity::matches(
        allocation.required("target").unwrap(),
        Some(selected)
    ));
    assert!(
        allocation
            .required("target")
            .unwrap()
            .required("hostId")
            .unwrap()
            .is_str("fixture")
    );
    let trace = runtime.client.trace.lock().unwrap();
    let pages: Vec<_> = trace
        .iter()
        .filter(|row| row.required("method").unwrap().is_str("serviceNodes.list"))
        .collect();
    assert_eq!(pages.len(), 2);
    assert!(pages[0].required("args").unwrap().get("cursor").is_none());
    assert!(
        pages[1]
            .required("args")
            .unwrap()
            .required("cursor")
            .unwrap()
            .is_str("nodes-1")
    );
}

#[tokio::test]
async fn complete_registry_remains_usable_at_the_node_and_page_limits() {
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    for count in [5, 20] {
        let mut client = Client::new(&catalogs);
        client.node_pages = Some(
            (0..count)
                .map(|index| {
                    let mut items = if count == 5 {
                        (index * 200..(index + 1) * 200)
                            .map(|n| other_node(&client, n))
                            .collect::<Vec<_>>()
                    } else {
                        vec![]
                    };
                    if index == 0 {
                        if items.is_empty() {
                            items.push(client.node());
                        } else {
                            items[0] = client.node();
                        }
                    }
                    let cursor = (index + 1 < count).then(|| format!("nodes-{}", index + 1));
                    node_page(items, cursor.as_deref())
                })
                .collect(),
        );
        let runtime = Runtime::new(client);
        let selected = runtime.client.sources[1].backend().unwrap();
        let mut task = task(selected);
        task.value.get_mut("fields").unwrap().insert(
            "executionRequirement",
            Json::object([("kind", "host".into()), ("hostId", "fixture".into())]),
        );
        let store = Memory::default();
        let allocation = Allocator {
            store: &store,
            engine: &runtime,
            catalogs: &catalogs,
        }
        .allocate(&task, &"allocate".into(), None, false)
        .await
        .unwrap();
        assert!(BackendIdentity::matches(
            allocation.required("target").unwrap(),
            Some(selected)
        ));
        assert_eq!(
            runtime
                .client
                .trace
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.required("method").unwrap().is_str("serviceNodes.list"))
                .count(),
            count
        );
    }
}

#[tokio::test]
async fn allocation_requires_a_complete_registry_before_native_reads_or_plan_writes() {
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    for case in [
        "repeated-cursor",
        "page-limit",
        "node-limit",
        "final-page-failure",
    ] {
        let mut client = Client::new(&catalogs);
        let count = if case == "page-limit" {
            21
        } else if case == "node-limit" {
            6
        } else {
            2
        };
        let mut pages = Vec::new();
        for index in 0..count {
            let mut items = Vec::new();
            if case == "node-limit" {
                items.extend(
                    (index * 200..((index + 1) * 200).min(1001)).map(|n| other_node(&client, n)),
                );
            }
            if index == 0 {
                if items.is_empty() {
                    items.push(client.node());
                } else {
                    items[0] = client.node();
                }
            }
            let cursor = if case == "repeated-cursor" || case == "final-page-failure" {
                Some("nodes-1".to_owned())
            } else if index + 1 == count {
                None
            } else {
                Some(format!("nodes-{}", index + 1))
            };
            pages.push(node_page(items, cursor.as_deref()));
        }
        if case == "final-page-failure" {
            // A read error after a usable first page must not authorize effects.
            pages[1] = Json::Null;
        }
        client.node_pages = Some(pages);
        let runtime = Runtime::new(client);
        let selected = runtime.client.sources[1].backend().unwrap();
        let store = Memory::default();
        let result = Allocator {
            store: &store,
            engine: &runtime,
            catalogs: &catalogs,
        }
        .allocate(&task(selected), &"allocate".into(), None, false)
        .await;
        let error = result.unwrap_err();
        assert_eq!(
            error.code,
            if case == "final-page-failure" {
                "discovery_unavailable"
            } else {
                "task_board_target_unavailable"
            },
            "{case}"
        );
        let trace = runtime.client.trace.lock().unwrap();
        assert_eq!(
            trace.len(),
            if case == "page-limit" { 20 } else { count },
            "{case}"
        );
        assert!(
            trace
                .iter()
                .all(|row| row.required("method").unwrap().is_str("serviceNodes.list")),
            "{case}"
        );
        assert!(store.documents.borrow().is_empty(), "{case}");
        assert_eq!(*store.effects.borrow(), 0, "{case}");
    }
}

#[tokio::test]
async fn allocation_retains_provider_account_and_scoped_plan_for_identical_model_labels() {
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    for index in 0..3 {
        let runtime = Runtime::new(Client::new(&catalogs));
        let selected = runtime.client.sources[index].backend().unwrap();
        let store = Memory::default();
        let allocation = Allocator {
            store: &store,
            engine: &runtime,
            catalogs: &catalogs,
        }
        .allocate(&task(selected), &"allocate".into(), None, false)
        .await
        .unwrap();
        assert!(BackendIdentity::matches(
            allocation.required("target").unwrap(),
            Some(selected)
        ));
        assert!(
            allocation
                .required("workspace")
                .unwrap()
                .get("backend")
                .is_none()
        );
        let plan = store
            .read(
                "task-board/native-plan",
                allocation.required("intent").unwrap(),
                true,
            )
            .await
            .unwrap();
        assert!(BackendIdentity::matches(&plan.value, Some(selected)));
        for (slot, method) in [
            ("threadStart", "thread/start"),
            ("threadResume", "thread/resume"),
            ("turnStart", "turn/start"),
            ("turnInterrupt", "turn/interrupt"),
        ] {
            assert_eq!(
                plan.value
                    .required("definitions")
                    .unwrap()
                    .required(slot)
                    .unwrap(),
                &Json::from(
                    runtime.client.sources[index]
                        .definition_hash(&method.into())
                        .unwrap()
                )
            );
        }
        let trace = runtime.client.trace.lock().unwrap();
        for row in trace
            .iter()
            .filter(|row| row.required("method").unwrap().is_str("tools.call"))
        {
            let args = row.required("args").unwrap();
            let name = args.required("qualifiedName").unwrap().as_str().unwrap();
            if name != "agent.backends" {
                assert!(
                    name == format!("{}.model/list", selected.id())
                        || BackendIdentity::matches(
                            args.required("arguments").unwrap(),
                            Some(selected)
                        )
                );
            }
        }
    }
}

#[tokio::test]
async fn discovery_rejects_foreign_owner_unavailable_account_and_changed_continuation() {
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let mut client = Client::new(&catalogs);
    let selected = client.sources[1].backend().unwrap().clone();
    client.foreign = true;
    assert_eq!(
        supports_execution_options(
            Arc::new(client),
            &Client::new(&catalogs).node(),
            &options(&selected)
        )
        .await
        .unwrap_err()
        .code,
        "task_board_target_mismatch"
    );
    let mut client = Client::new(&catalogs);
    client.unavailable = true;
    assert!(
        !supports_execution_options(
            Arc::new(client),
            &Client::new(&catalogs).node(),
            &options(&selected)
        )
        .await
        .unwrap()
    );
    let foreign_account = backend("beta", NativeProvider::Claude, "other");
    assert!(
        !supports_execution_options(
            Arc::new(Client::new(&catalogs)),
            &Client::new(&catalogs).node(),
            &options(&foreign_account)
        )
        .await
        .unwrap()
    );
    let mut fast = options(&selected);
    fast.insert("serviceTier", "fast".into());
    assert!(
        !supports_execution_options(
            Arc::new(Client::new(&catalogs)),
            &Client::new(&catalogs).node(),
            &fast
        )
        .await
        .unwrap()
    );
    let runtime = Runtime::new(Client::new(&catalogs));
    let store = Memory::default();
    let mut task = task(&selected);
    let old = runtime.client.sources[2].backend().unwrap();
    let primary = Json::object([
        ("serviceNodeId", "native-agent".into()),
        ("namespace", old.id().into()),
        ("kind", "thread".into()),
        ("nativeId", "same-thread".into()),
    ]);
    task.value.insert("primaryResourceRef", primary.clone());
    task.value.insert("primaryBackend", old.json());
    task.value.insert(
        "lastRun",
        Json::object([("objectId", "previous".into()), ("revision", 1.0.into())]),
    );
    let previous = Json::object([
        ("taskId", "task".into()),
        ("primaryResourceRef", primary),
        ("phase", "completed".into()),
        (
            "externalOutcome",
            Json::object([("state", "succeeded".into())]),
        ),
        (
            "target",
            old.arguments(Json::object([
                ("hostId", "fixture".into()),
                ("serviceNodeId", "native-agent".into()),
            ]))
            .unwrap(),
        ),
        ("plan", Json::object([("turnStart", Json::Object(vec![]))])),
    ]);
    store.documents.borrow_mut().push(Stored {
        key: "task-board/run".into(),
        name: "Previous".into(),
        document: Document {
            pin: ivy_task_board::operations::ObjectPin::from_json(Json::object([
                ("objectId", "previous".into()),
                ("revision", 1.0.into()),
            ]))
            .unwrap(),
            value: previous,
            parent_id: None,
            content_hash: String::new(),
        },
    });
    assert_eq!(
        Allocator {
            store: &store,
            engine: &runtime,
            catalogs: &catalogs
        }
        .allocate(&task, &"allocate".into(), None, true)
        .await
        .unwrap_err()
        .code,
        "task_board_workspace_mismatch"
    );
    assert!(!runtime.client.trace.lock().unwrap().iter().any(|row| {
        row.required("args")
            .unwrap()
            .get("qualifiedName")
            .is_some_and(|name| name.is_str("agent.read") || name.is_str("agent.resolveWorkspace"))
    }));
}

#[test]
fn execution_defaults_inherit_only_within_the_same_complete_account() {
    let first = backend("primary", NativeProvider::Codex, "one");
    let second = backend("primary", NativeProvider::Codex, "two");
    let defaults = Json::object([
        ("executionRequirement", Json::Null),
        ("nativeOptions", options(&first)),
    ]);
    for selected in [&first, &second] {
        let fields = Json::object([(
            "nativeOptions",
            Json::object([
                ("backend", selected.json()),
                ("model", Json::Null),
                ("reasoningEffort", Json::Null),
                ("serviceTier", Json::Null),
            ]),
        )]);
        let merged = execution_with_defaults(&fields, &defaults);
        let options = merged.required("nativeOptions").unwrap();
        assert!(BackendIdentity::matches(options, Some(selected)));
        assert_eq!(
            options.required("model").unwrap().is_null(),
            selected != &first
        );
        assert_eq!(
            options.required("reasoningEffort").unwrap().is_null(),
            selected != &first
        );
    }
}
