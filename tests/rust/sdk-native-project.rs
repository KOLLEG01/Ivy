use ivy_contracts::json::{parse, stringify};
use ivy_sdk::{
    IvyError, IvyJson, JsString, native_project::*, native_project_membership::*, rpc::*,
    tools::ToolClients,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

fn json(text: &str) -> IvyJson {
    parse(text, 32 * 1024 * 1024).unwrap()
}
fn capture(value: ivy_sdk::IvyResult<IvyJson>) -> IvyJson {
    match value {
        Ok(value) => IvyJson::object([("ok", true.into()), ("value", value)]),
        Err(error) => IvyJson::object([
            ("ok", false.into()),
            ("code", IvyJson::String(error.code_js())),
            ("message", IvyJson::String(error.message_js())),
        ]),
    }
}
struct InventoryClient {
    inventory: IvyJson,
    trace: Mutex<Vec<IvyJson>>,
}
impl RpcClient for InventoryClient {
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
                let mut binding = json(
                    r#"{"qualifiedName":"agent.projects","definition":{"namespace":"agent","name":"projects","interfaceVersion":"1.0.0","annotations":{"readOnlyHint":true},"inputSchema":{},"outputSchema":{}}}"#,
                );
                binding.insert(
                    "definitionHash",
                    format!("sha256:{}", "a".repeat(64)).into(),
                );
                binding.insert("catalogHash", format!("sha256:{}", "b".repeat(64)).into());
                Ok(IvyJson::object([
                    (
                        "provider",
                        json(r#"{"node":{"serviceNodeId":"native-agent"}}"#),
                    ),
                    ("items", IvyJson::Array(vec![binding])),
                    ("nextCursor", IvyJson::Null),
                ]))
            } else if method == "tools.call" {
                Ok(self.inventory.clone())
            } else {
                Err(IvyError::internal())
            }
        })
    }
}

#[tokio::test]
async fn native_project_paths_membership_defaults_and_transport_match_typescript() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/native-project-oracle.mjs",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["unicode"], "17.0");
    let rows = document["rows"].as_array().unwrap();
    assert!(rows.len() > 4000);
    for row in rows {
        let input = json(row["input"].as_str().unwrap());
        let expected = json(row["expected"].as_str().unwrap());
        let result = match row["kind"].as_str().unwrap() {
            "normalize" => Ok(IvyJson::String(normalized_project_path(
                input.get("path").unwrap().as_js_string().unwrap(),
            ))),
            "membership" => native_project_for_thread(
                input.get("projects").unwrap().as_array().unwrap(),
                input.get("thread").unwrap(),
            )
            .map(|v| v.unwrap_or(IvyJson::Null)),
            "internal" => Ok(is_internal_project(input.get("project").unwrap()).into()),
            "default" => default_native_thread_project(
                input.get("params").unwrap(),
                input.get("inventory").unwrap(),
            ),
            "rpc" => {
                let client = Arc::new(InventoryClient {
                    inventory: input.get("inventory").unwrap().clone(),
                    trace: Mutex::new(vec![]),
                });
                let tools = ToolClients::new(client.clone());
                let result = native_thread_project(
                    &tools,
                    &"native-agent".into(),
                    input.get("params").unwrap(),
                    RequestOptions {
                        timeout: Duration::from_millis(1234),
                        ..Default::default()
                    },
                )
                .await;
                assert_eq!(
                    stringify(
                        &IvyJson::Array(client.trace.lock().unwrap().clone()),
                        1_000_000
                    )
                    .unwrap(),
                    row["trace"].as_str().unwrap(),
                    "{row}"
                );
                result
            }
            other => panic!("Unknown row {other}"),
        };
        assert_eq!(capture(result), expected, "{row}");
    }
    // Guard the host-language Unicode version too: identity mappings must remain identity.
    let lowercase: HashMap<u32, JsString> = document["lowercase"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row[0].as_u64().unwrap() as u32,
                JsString::from(row[1].as_str().unwrap()),
            )
        })
        .collect();
    for cp in 0..=0x10ffff {
        let Some(c) = char::from_u32(cp) else {
            continue;
        };
        let mut input = JsString::from("C:/");
        input.0.extend(c.encode_utf16(&mut [0; 2]).iter().copied());
        input.0.extend("/z".encode_utf16());
        let mut expected = JsString::from("c:/");
        expected.0.extend(
            lowercase
                .get(&cp)
                .cloned()
                .unwrap_or_else(|| JsString::from(c.to_string()))
                .0
                .into_iter()
                .map(|c| if c == 92 { 47 } else { c }),
        );
        expected.0.extend("/z".encode_utf16());
        assert_eq!(
            normalized_project_path(&input),
            expected,
            "Unicode U+{cp:04X}"
        );
    }
}

#[tokio::test]
async fn project_short_circuits_do_not_dispatch_and_inventory_must_match_contract() {
    let client = Arc::new(InventoryClient {
        inventory: json(r#"{"projects":[]}"#),
        trace: Mutex::new(vec![]),
    });
    let tools = ToolClients::new(client.clone());
    let cancellation = tokio_util::sync::CancellationToken::new();
    cancellation.cancel();
    for input in [
        r#"{"projectId":""}"#,
        r#"{"projectId":"explicit"}"#,
        r#"{"ephemeral":true}"#,
    ] {
        native_thread_project(
            &tools,
            &"native-agent".into(),
            &json(input),
            RequestOptions {
                cancellation: cancellation.clone(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    assert!(client.trace.lock().unwrap().is_empty());
    let error = native_thread_project(
        &tools,
        &"native-agent".into(),
        &json(r#"{"cwd":"/work"}"#),
        RequestOptions {
            cancellation,
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "cancelled");
    assert!(client.trace.lock().unwrap().is_empty());
    let error = native_thread_project(
        &tools,
        &"native-agent".into(),
        &json(r#"{"cwd":"/work"}"#),
        RequestOptions::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
}
