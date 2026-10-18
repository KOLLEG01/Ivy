use ivy_contracts::{IvyError, IvyJson, Outcome, json::parse};
use ivy_sdk::{rpc::*, tools::ToolClients};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

#[path = "browser_notice_cases.rs"]
mod browser_notice_cases;
#[path = "inbox_evidence_cases.rs"]
mod inbox_evidence_cases;
#[path = "store_cases.rs"]
mod store_cases;
#[path = "workflow_cases.rs"]
mod workflow_cases;

struct Scripted {
    steps: Mutex<VecDeque<IvyJson>>,
    trace: Mutex<Vec<IvyJson>>,
}
impl RpcClient for Scripted {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            self.trace.lock().unwrap().push(IvyJson::object([
                ("method", method.into()),
                ("params", params),
                (
                    "timeout",
                    IvyJson::Number(options.timeout.as_millis() as f64),
                ),
            ]));
            let step = self
                .steps
                .lock()
                .unwrap()
                .pop_front()
                .expect("Unexpected RPC");
            assert_eq!(step.required("method")?, &IvyJson::from(method));
            if let Some(error) = step.get("error") {
                let outcome = match error.required("outcome")?.as_str()?.as_str() {
                    "unknown" => Outcome::Unknown,
                    "completed" => Outcome::Completed,
                    _ => Outcome::NotExecuted,
                };
                return Err(IvyError::new(
                    error.required("code")?.as_str()?,
                    error.required("message")?.as_str()?,
                )
                .with_outcome(outcome));
            }
            Ok(step.required("value")?.clone())
        })
    }
}

#[tokio::test]
async fn original_voice_discovery_and_message_unread_rpc_traces() {
    use ivy_secretary::{assignment_message::message_conversation_unread, voice_target::*};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut command = std::process::Command::new("node");
    command
        .arg("tests/secretary-rust/delivery-oracle.mjs")
        .current_dir(root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().expect("Original Node oracle required");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let corpus = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        4 * 1024 * 1024,
    )
    .unwrap();
    for row in corpus.required("dates").unwrap().as_array().unwrap() {
        let pair = row.as_array().unwrap();
        let actual = ivy_sdk::clock::parse_date(pair[0].as_js_string().unwrap());
        let expected = pair[1].as_f64().unwrap_or(f64::NAN);
        assert!(
            actual == expected || (actual.is_nan() && expected.is_nan()),
            "date {:?}: {actual} != {expected}",
            pair[0]
        );
    }
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert!(rows.len() >= 70);
    for (index, row) in rows.iter().enumerate() {
        let input = row.required("input").unwrap();
        let client = Arc::new(Scripted {
            steps: Mutex::new(
                input
                    .get("steps")
                    .and_then(IvyJson::as_array)
                    .unwrap_or(&[])
                    .iter()
                    .cloned()
                    .collect(),
            ),
            trace: Mutex::new(Vec::new()),
        });
        let tools = ToolClients::new(client.clone());
        let mut results = Vec::new();
        for _ in 0..input
            .get("repeats")
            .and_then(IvyJson::as_f64)
            .unwrap_or(1.0) as usize
        {
            let result = match input.required("kind").unwrap().as_str().unwrap().as_str() {
                "voice" => discover_voice_target(client.as_ref(), &tools)
                    .await
                    .map(|value| value.unwrap_or(IvyJson::Null)),
                "unique" => Ok(unique_voice_target(
                    input.required("candidates").unwrap().as_array().unwrap(),
                )
                .unwrap_or(IvyJson::Null)),
                _ => message_conversation_unread(
                    client.as_ref(),
                    &tools,
                    input.required("root").unwrap().as_js_string().unwrap(),
                    input.required("batch").unwrap(),
                )
                .await
                .map(IvyJson::Bool),
            };
            results.push(match result {
                Ok(value) => IvyJson::object([("value", value)]),
                Err(error) => IvyJson::object([
                    ("code", error.code.into()),
                    ("message", error.message.into()),
                    ("outcome", error.outcome.as_str().into()),
                ]),
            });
        }
        let actual = IvyJson::object([
            ("results", IvyJson::Array(results)),
            (
                "trace",
                IvyJson::Array(client.trace.lock().unwrap().clone()),
            ),
            (
                "remaining",
                IvyJson::Number(client.steps.lock().unwrap().len() as f64),
            ),
        ]);
        assert_eq!(&actual, row.required("expected").unwrap(), "case {index}");
    }
}
