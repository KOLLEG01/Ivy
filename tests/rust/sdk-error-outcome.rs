use ivy_contracts::json::parse;
use ivy_sdk::{IvyError, IvyJson, JsString, Outcome, rpc::*, tools::*};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

struct Scripted {
    steps: Mutex<VecDeque<(&'static str, Result<IvyJson, IvyError>)>>,
    trace: Mutex<Vec<String>>,
}
impl RpcClient for Scripted {
    fn request<'a>(
        &'a self,
        method: &'a str,
        _params: IvyJson,
        _options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            self.trace.lock().unwrap().push(method.to_owned());
            let (expected, result) = self
                .steps
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected replay");
            assert_eq!(method, expected);
            result
        })
    }
}
fn page(hash: &str) -> IvyJson {
    IvyJson::object([
        (
            "items",
            IvyJson::Array(vec![IvyJson::object([
                ("qualifiedName", "example.observe".into()),
                ("definitionHash", hash.into()),
                (
                    "definition",
                    IvyJson::object([
                        ("interfaceVersion", "1.0.0".into()),
                        (
                            "annotations",
                            IvyJson::object([("readOnlyHint", true.into())]),
                        ),
                    ]),
                ),
            ])]),
        ),
        (
            "provider",
            IvyJson::object([(
                "node",
                IvyJson::object([("serviceNodeId", "selected".into())]),
            )]),
        ),
        ("nextCursor", IvyJson::Null),
    ])
}
fn json(text: &str) -> IvyJson {
    parse(text, 100_000).unwrap()
}

fn main_client(connected: IvyJson, channels: IvyJson) -> Scripted {
    let binding = json(r#"{"items":[{"qualifiedName":"chat.workspace","definitionHash":"hash","definition":{"interfaceVersion":"1.2.0","annotations":{"readOnlyHint":true}}}],"provider":{"node":{"serviceNodeId":"main"}},"nextCursor":null}"#);
    let mut node = json(r#"{"serviceNodeId":"main","serviceName":"chat-bridge","principalId":"main-owner","connected":true,"synced":true,"ready":true}"#);
    node.insert("connected", connected);
    let mut workspace = json(r#"{"expectedBridge":{"principalId":"main-owner","callerPrincipalId":"secretary"},"nativeOwnerReady":true,"channels":[]}"#);
    workspace.insert("channels", channels);
    Scripted {
        steps: Mutex::new(VecDeque::from([
            ("tools.list", Ok(binding)),
            ("serviceNodes.get", Ok(node)),
            ("tools.call", Ok(workspace)),
        ])),
        trace: Mutex::new(Vec::new()),
    }
}

#[tokio::test]
async fn main_chat_uses_the_registered_public_workspace() {
    let channels = json(r#"[{"channel":{"adapter":"whatsapp","channelId":"main"}}]"#);
    let client = main_client(true.into(), channels);
    let selected = ivy_sdk::main_chat::main_chat(&client).await.unwrap();
    assert_eq!(selected.service_node_id, JsString::from("main"));
    assert_eq!(selected.channel.required("channelId").unwrap(), &IvyJson::from("main"));
    assert_eq!(*client.trace.lock().unwrap(), ["tools.list", "serviceNodes.get", "tools.call"]);
    assert!(client.steps.lock().unwrap().is_empty());
}

#[tokio::test]
async fn main_chat_requires_boolean_readiness_and_an_array_of_channels() {
    for connected in [IvyJson::from("true"), IvyJson::Number(1.0), IvyJson::Null] {
        let client = main_client(connected, json(r#"[{"channel":{"adapter":"whatsapp"}}]"#));
        assert_eq!(ivy_sdk::main_chat::main_chat(&client).await.unwrap_err().code, "chat_main_unavailable");
        assert_eq!(*client.trace.lock().unwrap(), ["tools.list", "serviceNodes.get"]);
    }
    let client = main_client(true.into(), json(r#"{"length":1,"0":{"channel":{"adapter":"whatsapp"}}}"#));
    assert!(ivy_sdk::main_chat::main_chat(&client).await.is_err());
}

#[tokio::test]
async fn actual_read_refresh_uses_only_the_literal_not_executed_outcome() {
    for value in [
        json(r#"["unknown"]"#),
        json(r#"["completed"]"#),
        json(r#"[["not_executed"]]"#),
        json(r#"["not_executed"]"#),
        "not_executed".into(),
        "unknown".into(),
        "completed".into(),
    ] {
        let refresh = value == IvyJson::from("not_executed");
        let failure =
            IvyError::new_js("tool_definition_changed".into(), JsString(vec![0xd800, 65]))
                .with_provider_outcome(value.clone())
                .with_details(IvyJson::Null);
        let mut steps = VecDeque::from([
            ("tools.list", Ok(page("old"))),
            ("tools.call", Err(failure.clone())),
        ]);
        if refresh {
            steps.extend([
                ("tools.list", Ok(page("new"))),
                ("tools.call", Ok("result".into())),
            ]);
        }
        let rpc = Arc::new(Scripted {
            steps: Mutex::new(steps),
            trace: Mutex::new(Vec::new()),
        });
        let selected = ToolClients::new(rpc.clone())
            .service_tools(
                "selected".into(),
                &[InterfaceRequirement {
                    namespace: "example".into(),
                    interface_version: "1.0.0".into(),
                }],
            )
            .unwrap();
        let name = "example.observe".into();
        let binding = selected.binding(&name, None).await.unwrap();
        let result = selected
            .call(&name, IvyJson::Null, None, ToolCallOptions::default())
            .await;
        if refresh {
            assert_eq!(result.unwrap(), IvyJson::from("result"));
        } else {
            let error = result.unwrap_err();
            assert_eq!(error, failure);
            assert_eq!(error.outcome_json(), value);
            assert!(Arc::ptr_eq(
                &binding,
                &selected.binding(&name, None).await.unwrap()
            ));
        }
        assert!(rpc.steps.lock().unwrap().is_empty());
        assert_eq!(
            *rpc.trace.lock().unwrap(),
            if refresh {
                vec!["tools.list", "tools.call", "tools.list", "tools.call"]
            } else {
                vec!["tools.list", "tools.call"]
            }
        );
    }
}

#[test]
fn the_actual_rpc_boundary_keeps_its_distinct_string_only_admission() {
    for value in [
        json(r#"["unknown"]"#),
        json(r#"["completed"]"#),
        json(r#"[["not_executed"]]"#),
        json(r#"["not_executed"]"#),
        json("null"),
        json("false"),
        json("0"),
        json("{}"),
        "not_executed".into(),
        "unknown".into(),
        "completed".into(),
    ] {
        let raw = IvyError::new("remote", "Retained.")
            .with_provider_outcome(value.clone())
            .to_wire();
        assert_eq!(raw.get("data").unwrap().get("outcome"), Some(&value));
        let frame = IvyJson::object([
            ("jsonrpc", "2.0".into()),
            ("id", "r".into()),
            ("error", raw),
        ]);
        assert_eq!(
            ivy_contracts::validation::validate_response_frame(&frame).is_ok(),
            value.as_js_string().is_some(),
            "{value:?}"
        );
        let error = response_value(&frame, "r").unwrap_err();
        if value.as_js_string().is_some() {
            assert_eq!(error.code, "remote");
            assert_eq!(error.outcome_json(), value);
        } else {
            assert_eq!(error.code, "invalid_frame");
            assert_eq!(error.message, "Hive returned an invalid error.");
            assert_eq!(error.outcome, Outcome::Unknown);
        }
    }
}
