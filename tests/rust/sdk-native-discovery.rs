use ivy_contracts::{
    Outcome,
    json::{parse, stringify},
};
use ivy_sdk::{IvyError, IvyJson, native::*, rpc::*, tools::ToolClients};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
fn json(s: &str) -> IvyJson {
    parse(s, usize::MAX).unwrap()
}
struct Client {
    input: IvyJson,
    trace: Mutex<Vec<IvyJson>>,
    calls: Mutex<usize>,
    options: RequestOptions,
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let mut trace = IvyJson::object([("method", method.into()), ("params", params)]);
            if method == "tools.call" {
                trace.insert(
                    "timeout",
                    IvyJson::Number(options.timeout.as_millis() as f64),
                );
            }
            self.trace.lock().unwrap().push(trace);
            if method == "tools.list" {
                if self.input.get("failList") == Some(&IvyJson::Bool(true)) {
                    return Err(IvyError::new("fixture_discovery", "Discovery failed."));
                }
                let items = ["status", "catalog"].into_iter().map(|name| {
                    let mut definition = json(
                        r#"{"namespace":"agent","interfaceVersion":"1.0.0","annotations":{"readOnlyHint":true},"inputSchema":{},"outputSchema":{}}"#,
                    );
                    definition.insert("name", name.into());
                    let mut binding = IvyJson::object([("definition", definition)]);
                    binding.insert("qualifiedName", format!("agent.{name}").into());
                    binding.insert("definitionHash", format!("sha256:{}", "a".repeat(64)).into());
                    binding.insert("catalogHash", format!("sha256:{}", "b".repeat(64)).into());
                    binding
                }).collect();
                return Ok(IvyJson::object([
                    (
                        "provider",
                        json(r#"{"node":{"serviceNodeId":"native-agent"}}"#),
                    ),
                    ("items", IvyJson::Array(items)),
                    ("nextCursor", IvyJson::Null),
                ]));
            }
            assert_eq!(method, "tools.call");
            let mut calls = self.calls.lock().unwrap();
            let step = &self.input.get("steps").unwrap().as_array().unwrap()[*calls];
            *calls += 1;
            if self.input.get("cancelAfter") == Some(&IvyJson::Number(*calls as f64)) {
                self.options.cancellation.cancel();
            }
            if let Some(e) = step.get("error") {
                let outcome = match e.get("outcome").unwrap().as_str().unwrap().as_str() {
                    "unknown" => Outcome::Unknown,
                    "completed" => Outcome::Completed,
                    _ => Outcome::NotExecuted,
                };
                return Err(IvyError::new(
                    e.get("code").unwrap().as_str().unwrap(),
                    e.get("message").unwrap().as_str().unwrap(),
                )
                .with_outcome(outcome));
            }
            Ok(step.get("value").unwrap().clone())
        })
    }
}
fn target_json(target: &NativeTarget) -> IvyJson {
    let mut value = IvyJson::object([(
        "serviceNodeId",
        IvyJson::String(target.service_node_id.clone()),
    )]);
    if let Some(host) = &target.host_id {
        value.insert("hostId", IvyJson::String(host.clone()));
    }
    value.insert(
        "nativeVersion",
        IvyJson::String(target.native_version.clone()),
    );
    value.insert(
        "nativeExecutableHash",
        IvyJson::String(target.native_executable_hash.clone()),
    );
    value.insert("catalogHash", IvyJson::String(target.catalog_hash.clone()));
    value
}
#[tokio::test]
async fn selected_owner_discovery_matches_original_typescript() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/native-discovery-oracle.mjs",
        ])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let rows = rows.as_array().unwrap();
    assert!(rows.len() > 45);
    for row in rows {
        let input = json(row["input"].as_str().unwrap());
        let t = input.get("target").unwrap();
        let field = |name| t.get(name).unwrap().as_js_string().unwrap().clone();
        let target = NativeTarget {
            service_node_id: field("serviceNodeId"),
            host_id: t.get("hostId").and_then(IvyJson::as_js_string).cloned(),
            native_version: field("nativeVersion"),
            native_executable_hash: field("nativeExecutableHash"),
            catalog_hash: field("catalogHash"),
        };
        let options = RequestOptions {
            timeout: Duration::from_millis(1234),
            cancel_error: IvyError::new("fixture_cancelled", "Fixture cancellation."),
            ..Default::default()
        };
        if input.get("cancelBefore") == Some(&IvyJson::Bool(true)) {
            options.cancellation.cancel();
        }
        let client = Arc::new(Client {
            input,
            trace: Mutex::new(vec![]),
            calls: Mutex::new(0),
            options: options.clone(),
        });
        let tools = ToolClients::new(client.clone());
        let result = read_native_contract(&tools, &target, options).await;
        let actual = match result {
            Ok(r) => IvyJson::object([
                ("ok", true.into()),
                (
                    "value",
                    IvyJson::object([
                        ("target", target_json(&r.target)),
                        ("epoch", IvyJson::String(r.epoch)),
                        ("catalogHash", r.contract.catalog_hash().into()),
                        (
                            "definitions",
                            IvyJson::Array(
                                r.contract
                                    .definitions()
                                    .into_iter()
                                    .map(|(name, _)| IvyJson::String(name))
                                    .collect(),
                            ),
                        ),
                    ]),
                ),
            ]),
            Err(e) => IvyJson::object([("ok", false.into()), ("error", e.to_wire())]),
        };
        assert_eq!(
            actual,
            json(row["expected"].as_str().unwrap()),
            "{}",
            row["label"]
        );
        assert_eq!(
            stringify(
                &IvyJson::Array(client.trace.lock().unwrap().clone()),
                usize::MAX
            )
            .unwrap(),
            row["trace"].as_str().unwrap(),
            "{}",
            row["label"]
        );
    }
}
