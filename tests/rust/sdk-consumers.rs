use ivy_contracts::{
    json::{parse, stringify},
    operation::parse_operation_id_js,
};
use ivy_sdk::{
    IvyError, IvyJson, JsString, Outcome, browser_notice::*, main_chat::*,
    project_inventory::host_project, rpc::*,
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

fn field<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson {
    value.get(key).unwrap()
}
fn text(value: &IvyJson, key: &str) -> JsString {
    field(value, key).as_js_string().unwrap().clone()
}
fn success(value: IvyJson) -> IvyJson {
    IvyJson::object([("ok", true.into()), ("value", value)])
}
fn failure(error: IvyError) -> IvyJson {
    IvyJson::object([
        ("ok", false.into()),
        (
            "error",
            IvyJson::object([
                ("code", IvyJson::String(error.code_js())),
                ("message", IvyJson::String(error.message_js())),
                ("outcome", error.outcome.as_str().into()),
            ]),
        ),
    ])
}

#[test]
fn query_projection_and_utc_date_match_typescript() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/sdk-query-date-oracle.mjs",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(rows.len() > 4_000);
    for row in rows {
        let input = parse(row["input"].as_str().unwrap(), 1_000_000).unwrap();
        let expected = parse(row["result"].as_str().unwrap(), 1_000_000).unwrap();
        let value = field(&input, "value");
        let actual = if field(&input, "kind").as_str().unwrap() == "query" {
            match query_document(value) {
                Ok(document) => {
                    assert!(std::ptr::eq(document, field(value, "document")));
                    success(document.clone())
                }
                Err(error) => failure(error),
            }
        } else {
            let millis = value
                .as_f64()
                .unwrap_or_else(|| match value.as_str().unwrap().as_str() {
                    "NaN" => f64::NAN,
                    "Infinity" => f64::INFINITY,
                    "-Infinity" => f64::NEG_INFINITY,
                    _ => panic!("unknown numeric fixture"),
                });
            match ivy_sdk::clock::date_to_iso(millis) {
                Ok(iso) => success(IvyJson::String(iso)),
                Err(error) => {
                    // JS throws RangeError; the Rust API exposes an IvyResult without
                    // inventing a wire RangeError code. Compare its native message.
                    assert_eq!(error.code, "invalid_arguments");
                    assert_eq!(error.outcome, Outcome::NotExecuted);
                    IvyJson::object([
                        ("ok", false.into()),
                        (
                            "error",
                            IvyJson::object([
                                ("code", IvyJson::Null),
                                ("message", IvyJson::String(error.message_js())),
                                ("outcome", IvyJson::Null),
                            ]),
                        ),
                    ])
                }
            }
        };
        assert_eq!(actual, expected, "input={}", row["input"]);
    }
    assert_eq!(
        ivy_sdk::clock::date_to_iso(-0.0).unwrap(),
        ivy_sdk::clock::date_to_iso(0.0).unwrap()
    );
}

#[derive(Default)]
struct Scripted {
    steps: Mutex<VecDeque<IvyJson>>,
    trace: Mutex<Vec<IvyJson>>,
    ids: Mutex<Vec<JsString>>,
}
impl RpcClient for Scripted {
    fn request<'a>(
        &'a self,
        method: &'a str,
        mut params: IvyJson,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            if method == "events.publish" {
                let id = text(&params, "mutationId");
                let parsed = parse_operation_id_js(&id).unwrap();
                assert_eq!(parsed.runtime_epoch, JsString::from("fixture-epoch"));
                assert!(uuid::Uuid::parse_str(&parsed.nonce.to_utf8().unwrap()).is_ok());
                let mut ids = self.ids.lock().unwrap();
                let index = if let Some(index) = ids.iter().position(|value| value == &id) {
                    index
                } else {
                    ids.push(id);
                    ids.len() - 1
                };
                params.insert("mutationId", format!("generated-{index}").into());
            }
            let mut event = IvyJson::object([("method", method.into()), ("params", params)]);
            if method == "inventory.list" {
                event.insert(
                    "timeout",
                    IvyJson::Number(options.timeout.as_millis() as f64),
                );
            }
            self.trace.lock().unwrap().push(event);
            let step = self
                .steps
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected request");
            assert_eq!(field(&step, "method"), &IvyJson::from(method));
            if let Some(error) = step.get("error") {
                let outcome = match field(error, "outcome").as_str()?.as_str() {
                    "unknown" => Outcome::Unknown,
                    "completed" => Outcome::Completed,
                    _ => Outcome::NotExecuted,
                };
                let mut result = IvyError::new_js(text(error, "code"), text(error, "message"))
                    .with_outcome(outcome);
                if let Some(details) = error.get("details") {
                    result = result.with_details(details.clone());
                }
                return Err(result);
            }
            Ok(field(&step, "value").clone())
        })
    }
}

#[tokio::test]
async fn shared_consumer_helpers_match_typescript() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/sdk-consumers-oracle.mjs",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(rows.len() >= 40);
    for row in rows {
        let input = parse(row["input"].as_str().unwrap(), 1_000_000).unwrap();
        let expected = parse(row["result"].as_str().unwrap(), 1_000_000).unwrap();
        let client = Arc::new(Scripted {
            steps: Mutex::new(
                input
                    .get("steps")
                    .and_then(IvyJson::as_array)
                    .map(|steps| steps.iter().cloned().collect())
                    .unwrap_or_default(),
            ),
            ..Default::default()
        });
        let result = match field(&input, "kind").as_str().unwrap().as_str() {
            "main-checked" => match main_chat_checked(client.as_ref()).await {
                Ok(value) => success(value.to_json()),
                Err(error) => {
                    let origin = match &error {
                        MainChatFailure::Ivy(_) => "ivy",
                        MainChatFailure::Boundary => "boundary",
                    };
                    let error = error.into_error();
                    let mut normalized = field(&failure(error.clone()), "error").clone();
                    if let Some(details) = error.details {
                        normalized.insert("details", details);
                    }
                    IvyJson::object([
                        ("ok", false.into()),
                        ("origin", origin.into()),
                        ("error", normalized),
                    ])
                }
            },
            "main" => match main_chat(client.as_ref()).await {
                Ok(value) => success(value.to_json()),
                Err(error) => failure(error),
            },
            "project" => match host_project(
                client.as_ref(),
                input.get("hostId").and_then(IvyJson::as_js_string),
                &text(&input, "projectId"),
                RequestOptions {
                    timeout: Duration::from_millis(1234),
                    ..Default::default()
                },
            )
            .await
            {
                Ok(value) => success(value.unwrap_or(IvyJson::Null)),
                Err(error) => failure(error),
            },
            "notification" => success(main_notification(field(&input, "value"))),
            "notification-checked" => match main_notification_checked(field(&input, "value"))
                .and_then(|notification| {
                    ivy_contracts::hash::hash_json(&notification).map(|hash| {
                        IvyJson::object([("notification", notification), ("hash", hash.into())])
                    })
                }) {
                Ok(value) => success(value),
                Err(error) => failure(error),
            },
            "topic" => success(browser_notice_topic(&text(&input, "namespace"))),
            "publish" => {
                let mut results = Vec::new();
                for publication in field(&input, "publications").as_array().unwrap() {
                    let original = field(publication, "notice");
                    let before = original.clone();
                    results.push(
                        match publish_browser_notice(
                            client.clone(),
                            &text(publication, "namespace"),
                            original,
                        )
                        .await
                        {
                            Ok(()) => IvyJson::object([("ok", true.into())]),
                            Err(error) => failure(error),
                        },
                    );
                    assert_eq!(original, &before);
                }
                success(IvyJson::Array(results))
            }
            _ => panic!("unknown fixture"),
        };
        let actual = IvyJson::object([
            (
                "trace",
                IvyJson::Array(client.trace.lock().unwrap().clone()),
            ),
            ("result", result),
        ]);
        assert_eq!(
            actual,
            expected,
            "input={}\nactual={}\nexpected={}",
            row["input"],
            stringify(&actual, 1_000_000).unwrap(),
            row["result"]
        );
    }
}
