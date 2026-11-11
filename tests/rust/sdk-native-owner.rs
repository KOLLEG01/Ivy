use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, Outcome, json::parse};
use ivy_sdk::{
    native_evidence::NativeOwnerIdentity, native_operation::NativeOperationCall, native_owner::*,
    rpc::*, tools::ToolClients,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
#[path = "sdk-native-callback-errors.rs"]
mod callback_error_bridge;
fn json(text: &str) -> IvyJson {
    parse(text, usize::MAX).unwrap()
}
fn field<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson {
    value.get(key).unwrap()
}
fn string(value: &IvyJson, key: &str) -> JsString {
    field(value, key).as_js_string().unwrap().clone()
}
fn error(value: &IvyJson) -> IvyError {
    let outcome = match field(value, "outcome").as_str().unwrap().as_str() {
        "unknown" => Outcome::Unknown,
        "completed" => Outcome::Completed,
        _ => Outcome::NotExecuted,
    };
    let mut e = IvyError::new(
        field(value, "code").as_str().unwrap(),
        field(value, "message").as_str().unwrap(),
    )
    .with_outcome(outcome);
    e.details = value.get("details").cloned();
    e
}
struct Client {
    input: IvyJson,
    trace: Mutex<Vec<IvyJson>>,
    calls: Mutex<usize>,
    lists: Mutex<HashMap<String, usize>>,
    options: RequestOptions,
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        _options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            self.trace.lock().unwrap().push(IvyJson::object([
                ("method", method.into()),
                ("params", params.clone()),
            ]));
            if method == "tools.list" {
                let namespace = field(&params, "namespace").as_str()?;
                let prefix = field(&params, "namePrefix").as_str()?;
                let name = format!("{namespace}.{prefix}");
                let index = {
                    let mut lists = self.lists.lock().unwrap();
                    let index = lists.entry(name.clone()).or_default();
                    let prior = *index;
                    *index += 1;
                    prior
                };
                if let Some(e) = self
                    .input
                    .get("listErrors")
                    .and_then(|v| v.get(&name))
                    .and_then(IvyJson::as_array)
                    .and_then(|v| v.get(index))
                    .filter(|v| !v.is_null())
                {
                    return Err(error(e));
                }
                let default = format!(
                    "sha256:{}",
                    if namespace == "agent" { "a" } else { "b" }.repeat(64)
                );
                let hash = self
                    .input
                    .get("hashes")
                    .and_then(|v| v.get(&name))
                    .and_then(IvyJson::as_array)
                    .and_then(|v| v.get(index))
                    .cloned()
                    .unwrap_or(default.into());
                let definition = IvyJson::object([
                    ("namespace", namespace.clone().into()),
                    ("name", prefix.into()),
                    (
                        "interfaceVersion",
                        if namespace == "agent" {
                            "1.0.0".into()
                        } else {
                            field(field(&self.input, "target"), "nativeVersion").clone()
                        },
                    ),
                    ("inputSchema", json("{}")),
                    ("outputSchema", json("{}")),
                ]);
                return Ok(IvyJson::object([
                    (
                        "provider",
                        IvyJson::object([(
                            "node",
                            IvyJson::object([(
                                "serviceNodeId",
                                self.input
                                    .get("target")
                                    .and_then(|target| target.get("serviceNodeId"))
                                    .cloned()
                                    .unwrap_or_else(|| "native-agent".into()),
                            )]),
                        )]),
                    ),
                    (
                        "items",
                        IvyJson::Array(vec![IvyJson::object([
                            ("qualifiedName", name.into()),
                            ("definition", definition),
                            ("definitionHash", hash),
                            ("catalogHash", format!("sha256:{}", "c".repeat(64)).into()),
                        ])]),
                    ),
                    ("nextCursor", IvyJson::Null),
                ]));
            }
            assert_eq!(method, "tools.call");
            let mut calls = self.calls.lock().unwrap();
            let step = field(&self.input, "steps")
                .as_array()
                .unwrap()
                .get(*calls)
                .expect("unexpected call")
                .clone();
            *calls += 1;
            if self.input.get("cancelAfter") == Some(&IvyJson::Number(*calls as f64)) {
                self.options.cancellation.cancel();
            }
            if let Some(e) = step.get("error") {
                return Err(error(e));
            }
            Ok(field(&step, "value").clone())
        })
    }
}
struct Guard {
    client: Arc<Client>,
    calls: Mutex<usize>,
}

#[tokio::test]
async fn managed_release_keeps_original_logical_receipt_account_and_unsubscribe_binding() {
    use ivy_contracts::{hash::hash_json, native_backend::BackendIdentity};
    let backend = BackendIdentity::from_json(&json(
        r#"{"backendId":"codex","provider":"codex","accountId":"default"}"#,
    ))
    .unwrap();
    let target = NativeOwnerIdentity {
        backend: Some(backend.clone()),
        service_node_id: "native-agent".into(),
        host_id: Some("host".into()),
        native_version: "0.163.0".into(),
        native_executable_hash: format!("sha256:{}", "d".repeat(64)).into(),
        catalog_hash: format!("sha256:{}", "c".repeat(64)).into(),
    };
    let call = NativeOperationCall {
        operation_id: "original-release".into(),
        method: "agent.release".into(),
        params: json(
            r#"{"threadId":"thread","expectedEpoch":"epoch","expectedOperationId":"last-turn"}"#,
        ),
        definition_hash: format!("sha256:{}", "b".repeat(64)).into(),
    };
    let receipt = IvyJson::object([
        ("schemaVersion", 1.0.into()),
        ("backend", backend.json()),
        ("operationId", IvyJson::String(call.operation_id.clone())),
        ("callerPrincipalId", "caller".into()),
        (
            "serviceNodeId",
            IvyJson::String(target.service_node_id.clone()),
        ),
        (
            "nativeVersion",
            IvyJson::String(target.native_version.clone()),
        ),
        (
            "nativeExecutableHash",
            IvyJson::String(target.native_executable_hash.clone()),
        ),
        ("method", IvyJson::String(call.method.clone())),
        ("params", call.params.clone()),
        (
            "requestHash",
            hash_json(&IvyJson::object([
                ("method", "agent.release".into()),
                ("params", call.params.clone()),
            ]))
            .unwrap()
            .into(),
        ),
        ("phase", "succeeded".into()),
        ("epoch", "epoch".into()),
        ("requestId", IvyJson::Null),
        ("reply", IvyJson::Null),
        ("code", IvyJson::Null),
        ("createdAt", "2026-01-02T03:04:05.000Z".into()),
        ("updatedAt", "2026-01-02T03:04:05.000Z".into()),
        (
            "release",
            json(r#"{"kind":"logical","otherSubscribers":true}"#),
        ),
    ]);
    let client = Arc::new(Client {
        input: IvyJson::object([
            ("target", target.to_json()),
            (
                "steps",
                IvyJson::Array(vec![
                    IvyJson::object([("value", receipt.clone())]),
                    IvyJson::object([("value", receipt.clone())]),
                ]),
            ),
        ]),
        trace: Mutex::new(vec![]),
        calls: Mutex::new(0),
        lists: Mutex::new(HashMap::new()),
        options: RequestOptions::default(),
    });
    let owner = NativeOwner::new(
        Arc::new(ToolClients::new(client.clone())),
        "caller".into(),
        target,
        RequestOptions::default(),
    );
    let observed = owner
        .dispatch_local(&call, || async { Ok(()) })
        .await
        .unwrap()
        .unwrap();
    owner.check_operation(&observed, &call).unwrap();
    assert_eq!(observed, receipt);
    assert_eq!(owner.operation(&call).await.unwrap(), receipt);
    let trace = client.trace.lock().unwrap();
    let sends: Vec<_> = trace
        .iter()
        .filter(|v| v.get("method").is_some_and(|m| m.is_str("tools.call")))
        .collect();
    assert_eq!(sends.len(), 2);
    let params = field(sends[0], "params");
    assert_eq!(
        field(params, "qualifiedName"),
        &IvyJson::from("agent.release")
    );
    let args = field(params, "arguments");
    assert_eq!(args.get("backend"), Some(&backend.json()));
    assert_eq!(
        args.get("expectedDefinitionHash"),
        Some(&IvyJson::String(call.definition_hash))
    );
    assert_eq!(
        args.get("expectedOperationId"),
        Some(&IvyJson::from("last-turn"))
    );
    assert!(args.get("method").is_none() && args.get("params").is_none());
    assert_eq!(
        field(field(sends[1], "params"), "qualifiedName"),
        &IvyJson::from("agent.operation")
    );
    assert_eq!(
        client.lists.lock().unwrap().get("codex.thread/unsubscribe"),
        Some(&1)
    );
}

#[tokio::test]
async fn native_binding_cache_separates_legacy_backend_and_account_identities() {
    use ivy_contracts::native_backend::BackendIdentity;

    let name = JsString::from("codex.config/read");
    let hashes: Vec<JsString> = ['a', 'b', 'c']
        .into_iter()
        .map(|value| format!("sha256:{}", value.to_string().repeat(64)).into())
        .collect();
    let client = Arc::new(Client {
        input: IvyJson::object([
            (
                "target",
                json(r#"{"serviceNodeId":"native-agent","nativeVersion":"0.163.0"}"#),
            ),
            (
                "hashes",
                IvyJson::object([(
                    "codex.config/read",
                    IvyJson::Array(hashes.iter().cloned().map(IvyJson::String).collect()),
                )]),
            ),
            ("steps", IvyJson::Array(vec![])),
        ]),
        trace: Mutex::new(vec![]),
        calls: Mutex::new(0),
        lists: Mutex::new(HashMap::new()),
        options: RequestOptions::default(),
    });
    let tools = ToolClients::new(client.clone());
    let legacy = tools.native_service_tools("native-agent".into()).unwrap();
    legacy
        .binding(&name, Some(&IvyJson::String(hashes[0].clone())))
        .await
        .unwrap();

    for (account, expected) in [("default", &hashes[1]), ("other", &hashes[2])] {
        let backend = BackendIdentity::from_json(&IvyJson::object([
            ("backendId", "codex".into()),
            ("provider", "codex".into()),
            ("accountId", account.into()),
        ]))
        .unwrap();
        for _ in 0..2 {
            let scoped = tools
                .native_backend_tools("native-agent".into(), Some(&backend))
                .unwrap();
            scoped
                .binding(&name, Some(&IvyJson::String(expected.clone())))
                .await
                .unwrap();
        }
    }
    // Selecting an explicit account must not overwrite the retained legacy binding.
    legacy
        .binding(&name, Some(&IvyJson::String(hashes[0].clone())))
        .await
        .unwrap();
    assert_eq!(
        client.lists.lock().unwrap().get("codex.config/read"),
        Some(&3)
    );
    assert_eq!(*client.calls.lock().unwrap(), 0);
}

impl NativeDispatchGuard for Guard {
    fn check(&self) -> GuardFuture<'_> {
        Box::pin(async move {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            self.client
                .trace
                .lock()
                .unwrap()
                .push(IvyJson::object([("guard", IvyJson::Number(*calls as f64))]));
            if self.client.input.get("cancelGuard") == Some(&IvyJson::Number(*calls as f64)) {
                self.client.options.cancellation.cancel();
            }
            if let Some(e) = self
                .client
                .input
                .get("guardErrors")
                .and_then(IvyJson::as_array)
                .and_then(|v| v.get(*calls - 1))
                .filter(|v| !v.is_null())
            {
                return Err(error(e));
            }
            Ok(())
        })
    }
}
#[tokio::test]
async fn native_owner_calls_guards_recovery_and_evidence_match_original_typescript() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/native-owner-oracle.mjs",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let rows = rows.as_array().unwrap();
    assert!(rows.len() >= 80);
    for (row, local) in rows.iter().flat_map(|row| [(row, false), (row, true)]) {
        let input = json(row["input"].as_str().unwrap());
        if local && input.get("action") != Some(&IvyJson::from("dispatch")) {
            continue;
        }
        let t = field(&input, "target");
        let target = NativeOwnerIdentity {
            backend: ivy_contracts::native_backend::BackendIdentity::optional(t.get("backend"))
                .unwrap(),
            service_node_id: string(t, "serviceNodeId"),
            host_id: t.get("hostId").and_then(IvyJson::as_js_string).cloned(),
            native_version: string(t, "nativeVersion"),
            native_executable_hash: string(t, "nativeExecutableHash"),
            catalog_hash: string(t, "catalogHash"),
        };
        let c = field(&input, "call");
        let call = NativeOperationCall {
            operation_id: string(c, "operationId"),
            method: string(c, "method"),
            params: field(c, "params").clone(),
            definition_hash: string(c, "definitionHash"),
        };
        let options = RequestOptions {
            cancel_error: IvyError::new("fixture_cancelled", "Fixture cancellation."),
            timeout: std::time::Duration::from_millis(1234),
            ..Default::default()
        };
        if input.get("cancelBefore") == Some(&IvyJson::Bool(true)) {
            options.cancellation.cancel();
        }
        let client = Arc::new(Client {
            input,
            trace: Mutex::new(vec![]),
            calls: Mutex::new(0),
            lists: Mutex::new(HashMap::new()),
            options: options.clone(),
        });
        let tools = Arc::new(ToolClients::new(client.clone()));
        let owner = NativeOwner::new(tools.clone(), "caller".into(), target.clone(), options);
        let guard = Guard {
            client: client.clone(),
            calls: Mutex::new(0),
        };
        let result: IvyResult<IvyJson> = async {
            Ok(match field(&client.input, "action").as_str()?.as_str() {
                "management" => {
                    owner
                        .management("custom", json(r#"{"x":"\ud800"}"#), Some("original".into()))
                        .await?
                }
                "managementConflict" => {
                    owner
                        .management(
                            "custom",
                            IvyJson::object([(
                                "backend",
                                field(&client.input, "otherBackend").clone(),
                            )]),
                            Some("original".into()),
                        )
                        .await?
                }
                "dispatch" => {
                    let result = if local {
                        let state = std::rc::Rc::new(std::cell::RefCell::new(0));
                        owner
                            .dispatch_local(&call, || {
                                let state = state.clone();
                                let guard = &guard;
                                async move {
                                    tokio::task::yield_now().await;
                                    *state.borrow_mut() += 1;
                                    guard.check().await
                                }
                            })
                            .await
                    } else {
                        fn send<F: std::future::Future + Send>(future: F) -> F {
                            future
                        }
                        send(owner.dispatch(&call, &guard)).await
                    };
                    result?.unwrap_or(IvyJson::Null)
                }
                "interact" => owner.interact(&call, Some(&guard)).await?,
                "interaction" => owner.interaction(&"original".into()).await?,
                "operation" => owner.operation(&call).await?,
                "originalOperation" => {
                    let selected = field(&client.input, "originalOwner");
                    let original = NativeOwnerIdentity {
                        backend: ivy_contracts::native_backend::BackendIdentity::optional(
                            selected.get("backend"),
                        )?,
                        service_node_id: string(selected, "serviceNodeId"),
                        host_id: selected
                            .get("hostId")
                            .and_then(IvyJson::as_js_string)
                            .cloned(),
                        native_version: string(selected, "nativeVersion"),
                        native_executable_hash: string(selected, "nativeExecutableHash"),
                        catalog_hash: string(selected, "catalogHash"),
                    };
                    let retained = owner.original_operations(original);
                    // Exercise the same receipt-only trait used by reconciliation.
                    ivy_sdk::native_operation::NativeOperationOwner::operation(&retained, &call)
                        .await?
                }
                "status" => owner.status().await?,
                "frame" => owner.frame_limits().await?,
                "read" => {
                    owner
                        .read(
                            &call.method,
                            call.params.clone(),
                            client.input.get("epoch").and_then(IvyJson::as_js_string),
                            client.input.get("allowError") == Some(&IvyJson::Bool(true)),
                        )
                        .await?
                }
                "project" => owner.thread_start_params(&call.params).await?,
                "contract" => {
                    let first = owner.selected_contract().await?;
                    let second = NativeOwner::new(
                        tools,
                        "other-caller".into(),
                        target,
                        RequestOptions::default(),
                    )
                    .selected_contract()
                    .await?;
                    assert!(Arc::ptr_eq(&first, &second));
                    IvyJson::object([("catalogHash", first.catalog_hash().into())])
                }
                "contractRetry" => {
                    let _ = owner.selected_contract().await;
                    IvyJson::object([(
                        "catalogHash",
                        owner.selected_contract().await?.catalog_hash().into(),
                    )])
                }
                "contractConcurrent" => {
                    let other = NativeOwner::new(
                        tools,
                        "other-caller".into(),
                        target,
                        RequestOptions::default(),
                    );
                    let (first, second) =
                        tokio::join!(owner.selected_contract(), other.selected_contract());
                    let first = first?;
                    let second = second?;
                    assert!(Arc::ptr_eq(&first, &second));
                    IvyJson::object([("catalogHash", first.catalog_hash().into())])
                }
                "contractCache" => {
                    for index in (0..16).chain([0, 16, 0]) {
                        let mut selected = target.clone();
                        selected.host_id = Some(format!("host{index}").into());
                        NativeOwner::new(
                            tools.clone(),
                            "caller".into(),
                            selected,
                            RequestOptions::default(),
                        )
                        .selected_contract()
                        .await?;
                    }
                    IvyJson::Null
                }
                "contractAccounts" => {
                    let first = owner.selected_contract().await?;
                    let mut other = target.clone();
                    other.backend = Some(
                        ivy_contracts::native_backend::BackendIdentity::from_json(field(
                            &client.input,
                            "otherBackend",
                        ))
                        .unwrap(),
                    );
                    let second =
                        NativeOwner::new(tools, "caller".into(), other, RequestOptions::default())
                            .selected_contract()
                            .await?;
                    assert!(!Arc::ptr_eq(&first, &second));
                    assert!(std::ptr::eq(first.catalog(), second.catalog()));
                    let method = field(
                        field(first.catalog(), "clientRequests")
                            .as_array()
                            .unwrap()
                            .first()
                            .unwrap(),
                        "method",
                    )
                    .as_js_string()
                    .unwrap();
                    assert!(Arc::ptr_eq(
                        &first.input_schema(method, &[])?,
                        &second.input_schema(method, &[])?,
                    ));
                    IvyJson::object([
                        (
                            "sameCatalog",
                            (first.catalog_hash() == second.catalog_hash()).into(),
                        ),
                        ("firstDefinitionHash", first.definition_hash(method)?.into()),
                        (
                            "secondDefinitionHash",
                            second.definition_hash(method)?.into(),
                        ),
                    ])
                }
                _ => unreachable!(),
            })
        }
        .await;
        let actual = match result {
            Ok(value) => IvyJson::object([("ok", true.into()), ("value", value)]),
            Err(e) => IvyJson::object([("ok", false.into()), ("error", e.to_wire())]),
        };
        assert_eq!(
            actual,
            json(row["expected"].as_str().unwrap()),
            "{}",
            row["label"]
        );
        assert_eq!(
            IvyJson::Array(client.trace.lock().unwrap().clone()),
            json(row["trace"].as_str().unwrap()),
            "trace {}",
            row["label"]
        );
    }
}
