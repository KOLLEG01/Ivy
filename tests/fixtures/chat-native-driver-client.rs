use ivy_contracts::{IvyError, IvyJson as Json, Outcome, json::parse};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::sync::Mutex;

pub(crate) struct Client {
    pub(crate) row: Json,
    pub(crate) calls: Mutex<Vec<Json>>,
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
