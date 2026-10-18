use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, Outcome, json::parse};
use ivy_sdk::{
    native_evidence::NativeOwnerIdentity, native_operation::NativeOperationCall, native_owner::*,
    rpc::*, tools::ToolClients,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
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
                        json(r#"{"node":{"serviceNodeId":"native-agent"}}"#),
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
    for row in rows {
        let input = json(row["input"].as_str().unwrap());
        let t = field(&input, "target");
        let target = NativeOwnerIdentity {
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
                "dispatch" => owner
                    .dispatch(&call, &guard)
                    .await?
                    .unwrap_or(IvyJson::Null),
                "interact" => owner.interact(&call, Some(&guard)).await?,
                "interaction" => owner.interaction(&"original".into()).await?,
                "operation" => owner.operation(&call).await?,
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
