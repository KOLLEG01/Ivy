use super::{Database, json};
use ivy_contracts::{
    IvyError, IvyJson, JsString, Outcome,
    builtin_schema::json_equal,
    hash::hash_json,
    json::{parse, stringify},
};
use ivy_data_collector::{mcp_bridge::TaskMcp, model, native_mcp::NativeMcpSession};
use ivy_sdk::{
    rpc::{RequestFuture, RequestOptions, RpcClient},
    tools::ToolClients,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;
struct Client {
    row: IvyJson,
    trace: Mutex<Vec<IvyJson>>,
    operations: Mutex<BTreeMap<String, IvyJson>>,
    polls: Mutex<usize>,
}
impl Client {
    fn kind(&self) -> String {
        model::text(&self.row, "kind").unwrap()
    }
    fn operation(&self, call: &IvyJson) -> IvyJson {
        let kind = self.kind();
        let method = model::text(call, "method").unwrap();
        let params = call.get("params").unwrap();
        let target = self.row.get("target").unwrap();
        let result = if method == "thread/start" {
            IvyJson::object([(
                "thread",
                IvyJson::object([
                    ("id", "thread".into()),
                    ("ephemeral", (kind != "bad-ephemeral").into()),
                ]),
            )])
        } else if method == "thread/unsubscribe" {
            IvyJson::Null
        } else if kind == "tool-error" {
            json(
                r#"{"isError":true,"structuredContent":{"error":{"code":-32000,"message":"private diagnostic","data":{"code":"provider_unavailable","outcome":"unknown"}}}}"#,
            )
        } else {
            IvyJson::object([(
                "structuredContent",
                IvyJson::object([
                    ("tool", params.get("tool").unwrap().clone()),
                    ("args", params.get("arguments").unwrap().clone()),
                ]),
            )])
        };
        IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("operationId", call.get("operationId").unwrap().clone()),
            (
                "callerPrincipalId",
                if kind == "wrong-caller" {
                    "other"
                } else {
                    "caller"
                }
                .into(),
            ),
            (
                "serviceNodeId",
                target.get("serviceNodeId").unwrap().clone(),
            ),
            (
                "nativeVersion",
                target.get("nativeVersion").unwrap().clone(),
            ),
            (
                "nativeExecutableHash",
                target.get("nativeExecutableHash").unwrap().clone(),
            ),
            ("method", method.clone().into()),
            ("params", params.clone()),
            (
                "requestHash",
                hash_json(&IvyJson::object([
                    ("method", method.clone().into()),
                    ("params", params.clone()),
                ]))
                .unwrap()
                .into(),
            ),
            ("createdAt", "2026-01-02T03:04:05.006Z".into()),
            ("updatedAt", "2026-01-02T03:04:05.006Z".into()),
            (
                "phase",
                if kind == "failed-start" && method == "thread/start" {
                    "failed"
                } else {
                    "succeeded"
                }
                .into(),
            ),
            (
                "epoch",
                if kind == "changed-epoch" {
                    "other"
                } else {
                    "epoch"
                }
                .into(),
            ),
            ("requestId", IvyJson::Number(1.0)),
            ("reply", IvyJson::object([("result", result)])),
            ("code", IvyJson::Null),
        ])
    }
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            self.trace.lock().unwrap().push(IvyJson::object([
                ("method", method.into()),
                ("params", params.clone()),
            ]));
            if method == "system.status" {
                return Ok(json(
                    r#"{"runtimeEpoch":"hive-epoch","callerPrincipalId":"caller"}"#,
                ));
            }
            let target = self.row.get("target").unwrap();
            if method == "tools.list" {
                let namespace = model::text(&params, "namespace")?;
                let name = model::text(&params, "namePrefix")?;
                return Ok(IvyJson::object([
                    (
                        "provider",
                        IvyJson::object([(
                            "node",
                            IvyJson::object([(
                                "serviceNodeId",
                                target.get("serviceNodeId").unwrap().clone(),
                            )]),
                        )]),
                    ),
                    (
                        "items",
                        IvyJson::Array(vec![IvyJson::object([
                            ("qualifiedName", format!("{namespace}.{name}").into()),
                            (
                                "definition",
                                IvyJson::object([
                                    ("namespace", namespace.clone().into()),
                                    ("name", name.into()),
                                    (
                                        "interfaceVersion",
                                        if namespace == "agent" {
                                            "1.0.0".into()
                                        } else {
                                            target.get("nativeVersion").unwrap().clone()
                                        },
                                    ),
                                    ("inputSchema", json("{}")),
                                    ("outputSchema", json("{}")),
                                ]),
                            ),
                            (
                                "definitionHash",
                                format!(
                                    "sha256:{}",
                                    if namespace == "agent" { "a" } else { "b" }.repeat(64)
                                )
                                .into(),
                            ),
                            ("catalogHash", format!("sha256:{}", "c".repeat(64)).into()),
                        ])]),
                    ),
                    ("nextCursor", IvyJson::Null),
                ]));
            }
            assert_eq!(method, "tools.call");
            let kind = self.kind();
            let args = params.get("arguments").unwrap();
            match model::text(&params, "qualifiedName")?.as_str() {
                "agent.status" => {
                    let mut status = self.row.get("status").unwrap().clone();
                    if kind == "not-ready" {
                        status.insert("state", "stopped".into());
                    }
                    Ok(status)
                }
                "agent.invoke" => {
                    let method = model::text(args, "method")?;
                    if method == "thread/unsubscribe" && kind == "cleanup-failure" {
                        return Err(IvyError::new("fixture_cleanup", "Failed."));
                    }
                    let mut op = self.operation(args);
                    self.operations
                        .lock()
                        .unwrap()
                        .insert(model::text(args, "operationId")?, op.clone());
                    if method == "thread/start" && kind == "uncertain" {
                        return Err(IvyError::new("fixture_transport", "Uncertain.")
                            .with_outcome(Outcome::Unknown));
                    }
                    if method == "thread/start"
                        && matches!(kind.as_str(), "pending" | "absent-once")
                    {
                        op.insert("phase", "accepted".into());
                        for key in ["epoch", "requestId", "reply"] {
                            op.insert(key, IvyJson::Null);
                        }
                    }
                    Ok(op)
                }
                "agent.operation" => {
                    let mut polls = self.polls.lock().unwrap();
                    let first = *polls == 0;
                    *polls += 1;
                    if kind == "absent-once" && first {
                        return Err(IvyError::new("not_found", "Not observed.").with_details(
                            IvyJson::object([
                                ("kind", "agent_operation_absent".into()),
                                ("operationId", args.get("operationId").unwrap().clone()),
                                (
                                    "serviceNodeId",
                                    target.get("serviceNodeId").unwrap().clone(),
                                ),
                                ("epoch", "epoch".into()),
                            ]),
                        ));
                    }
                    Ok(self
                        .operations
                        .lock()
                        .unwrap()
                        .get(&model::text(args, "operationId")?)
                        .unwrap()
                        .clone())
                }
                name => panic!("Unexpected tool {name}"),
            }
        })
    }
}
fn normalize(value: &mut IvyJson, ids: &mut BTreeMap<String, String>) {
    match value {
        IvyJson::Array(items) => {
            for item in items {
                normalize(item, ids);
            }
        }
        IvyJson::Object(pairs) => {
            for (key, item) in pairs {
                if key == &JsString::from("operationId") {
                    let id = item.as_str().unwrap();
                    let next = format!("operation-{}", ids.len() + 1);
                    *item = ids.entry(id).or_insert(next).clone().into();
                } else {
                    normalize(item, ids);
                }
            }
        }
        _ => {}
    }
}
#[tokio::test]
async fn native_mcp_matches_original_owner_calls_serialization_polling_and_cleanup() {
    let _directory = Database::new();
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/data-collector-native-oracle.mjs",
        ])
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cases = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    for row in cases.as_array().unwrap() {
        let client = Arc::new(Client {
            row: row.clone(),
            trace: Mutex::default(),
            operations: Mutex::default(),
            polls: Mutex::new(0),
        });
        let kind = client.kind();
        let signal = CancellationToken::new();
        if kind == "cancelled" {
            signal.cancel();
        }
        let session = NativeMcpSession::new(
            client.clone(),
            Arc::new(ToolClients::new(client.clone())),
            row.get("configuration").unwrap().clone(),
            signal,
        );
        let result = match session {
            Err(error) => Err(error),
            Ok(session) => {
                let result = if kind == "concurrent" {
                    let (one, two) = tokio::join!(
                        session.call("echo".into(), json(r#"{"n":1}"#)),
                        session.call("second".into(), json(r#"{"n":2}"#))
                    );
                    one.and_then(|one| two.map(|two| IvyJson::Array(vec![one, two])))
                } else {
                    session
                        .call(
                            if kind == "ungranted" {
                                "missing"
                            } else {
                                "echo"
                            }
                            .into(),
                            json(r#"{"n":1}"#),
                        )
                        .await
                };
                session.close().await.unwrap();
                session.close().await.unwrap();
                result
            }
        };
        let (value, error) = match result {
            Ok(value) => (value, IvyJson::Null),
            Err(e) => (
                IvyJson::Null,
                IvyJson::object([
                    ("code", IvyJson::String(e.code_js())),
                    ("message", IvyJson::String(e.message_js())),
                    ("outcome", e.outcome.as_str().into()),
                ]),
            ),
        };
        assert!(
            json_equal(&value, row.get("value").unwrap()),
            "{kind}: value mismatch"
        );
        assert!(
            json_equal(&error, row.get("error").unwrap()),
            "{kind}: {} expected {}",
            stringify(&error).unwrap(),
            stringify(row.get("error").unwrap()).unwrap()
        );
        let mut trace = IvyJson::Array(client.trace.lock().unwrap().clone());
        normalize(&mut trace, &mut BTreeMap::new());
        assert!(
            json_equal(&trace, row.get("trace").unwrap()),
            "{kind}: {} expected {}",
            stringify(&trace).unwrap(),
            stringify(row.get("trace").unwrap()).unwrap()
        );
    }
}
