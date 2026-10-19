use ivy_chat_bridge::{
    admission::ChatAdmission, native_driver::ChatNativeDriver, store::ChatStore,
};
#[path = "chat-native-driver-cases.rs"]
mod durable;
#[path = "chat-generation-cases.rs"]
mod generation;
#[path = "chat-service-runtime-cases.rs"]
mod runtime;
use ivy_contracts::{
    IvyError, IvyJson as Json, Outcome, checked_native_contract::CheckedNativeContracts,
    json::parse,
};
use ivy_sdk::{
    rpc::{RequestFuture, RequestOptions, RpcClient},
    tools::ToolClients,
};
use std::{
    process::Command,
    sync::{Arc, Mutex},
};

struct Client {
    row: Json,
    calls: Mutex<Vec<Json>>,
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(Json::object([
                ("method", method.into()),
                ("params", params.clone()),
            ]));
            if self.row.get("failure") == Some(&Json::from(method)) {
                return Err(
                    IvyError::new("service_unavailable", "Synthetic owner outage.")
                        .with_outcome(Outcome::Unknown),
                );
            }
            match method {
                "serviceNodes.get" => Ok(self.row.required("node")?.clone()),
                "tools.call" => Ok(self.row.required("status")?.clone()),
                "tools.list" => Ok(Json::object([
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
                            (
                                "qualifiedName",
                                format!("agent.{}", params.required("namePrefix")?.as_str()?)
                                    .into(),
                            ),
                            (
                                "definition",
                                Json::object([
                                    ("namespace", "agent".into()),
                                    ("name", params.required("namePrefix")?.clone()),
                                    ("interfaceVersion", "1.0.0".into()),
                                    ("inputSchema", parse("{}", 100)?),
                                    ("outputSchema", parse("{}", 100)?),
                                ]),
                            ),
                            (
                                "definitionHash",
                                format!("sha256:{}", "a".repeat(64)).into(),
                            ),
                            ("catalogHash", format!("sha256:{}", "b".repeat(64)).into()),
                        ])]),
                    ),
                    ("nextCursor", Json::Null),
                ])),
                _ => panic!("unexpected RPC: {method}"),
            }
        })
    }
}

#[tokio::test]
async fn configured_owner_readiness_and_rpc_order_match_original_driver() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = Command::new("node")
        .current_dir(&root)
        .arg("tests/rust/fixtures/chat-native-driver-oracle.mjs")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    let catalogs = CheckedNativeContracts::new(root);
    for row in rows.as_array().unwrap() {
        let client = Arc::new(Client {
            row: row.clone(),
            calls: Mutex::new(vec![]),
        });
        let tools = Arc::new(ToolClients::new(client.clone()));
        let admission = ChatAdmission::new(row.get("definition").unwrap()).unwrap();
        let store = ChatStore::open(client.clone(), None, None).await.unwrap();
        let instructions = "".into();
        let driver = ChatNativeDriver::new(
            &store,
            tools,
            &admission,
            &catalogs,
            "chat-provider".into(),
            &instructions,
        )
        .unwrap();
        let result = async {
            let value = match row.required("action")?.as_str()?.as_str() {
                "identity" => {
                    let owner = driver.native_owner()?;
                    let target = owner.target();
                    Json::object([
                        (
                            "target",
                            Json::object([
                                (
                                    "serviceNodeId",
                                    Json::String(target.service_node_id.clone()),
                                ),
                                ("nativeVersion", Json::String(target.native_version.clone())),
                                (
                                    "nativeExecutableHash",
                                    Json::String(target.native_executable_hash.clone()),
                                ),
                                ("catalogHash", Json::String(target.catalog_hash.clone())),
                            ]),
                        ),
                        ("caller", Json::String(owner.caller().clone())),
                    ])
                }
                "verify" => {
                    driver.verify_owner().await?;
                    Json::Null
                }
                "management" => {
                    driver
                        .management("custom", parse(r#"{"original":"\ud800"}"#, 100)?)
                        .await?
                }
                "repeat" => {
                    driver.ready().await?;
                    driver.ready().await?
                }
                _ => driver.ready().await?,
            };
            Ok::<_, IvyError>(value)
        }
        .await;
        let actual = match result {
            Ok(value) => Json::object([("value", value)]),
            Err(e) => Json::object([(
                "error",
                Json::object([
                    ("code", e.code.into()),
                    ("message", e.message.into()),
                    ("outcome", e.outcome.as_str().into()),
                ]),
            )]),
        };
        assert_eq!(
            &actual,
            row.get("result").unwrap(),
            "result {:?}",
            row.get("label")
        );
        assert_eq!(
            &Json::Array(client.calls.lock().unwrap().clone()),
            row.get("calls").unwrap(),
            "calls {:?}",
            row.get("label")
        );
    }
    assert!(rows.as_array().unwrap().len() >= 45);
}
