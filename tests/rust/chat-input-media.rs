use ivy_chat_bridge::{admission::ChatAdmission, input_media::admitted_native_input};
use ivy_contracts::{IvyError, IvyJson as Json, Outcome, json::parse};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{process::Command, sync::Mutex};

struct Reader {
    response: Json,
    failure: bool,
    calls: Mutex<Vec<Json>>,
}
impl RpcClient for Reader {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(Json::object([
                ("method", method.into()),
                ("params", params),
            ]));
            if self.failure {
                Err(
                    IvyError::new("service_unavailable", "Synthetic read outage.")
                        .with_outcome(Outcome::Unknown),
                )
            } else {
                Ok(self.response.clone())
            }
        })
    }
}
#[tokio::test]
async fn original_image_pins_bytes_and_container_admission_match_typescript() {
    let output = Command::new("node")
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .args(["tests/rust/fixtures/chat-admission-oracle.mjs", "media"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    for (index, row) in rows.as_array().unwrap().iter().enumerate() {
        let admission = ChatAdmission::new(row.get("definition").unwrap()).unwrap();
        let client = Reader {
            response: row.get("response").unwrap().clone(),
            failure: row.get("failure") == Some(&Json::Bool(true)),
            calls: Mutex::new(Vec::new()),
        };
        let result = admitted_native_input(
            &admission,
            &client,
            row.get("caller").unwrap().as_js_string().unwrap(),
            row.get("request").unwrap(),
        )
        .await;
        let actual = match result {
            Ok(value) => Json::object([(
                "value",
                Json::object([
                    ("identity", value.admission.identity),
                    ("requestHash", value.admission.request_hash.into()),
                    ("input", Json::Array(value.input)),
                ]),
            )]),
            Err(e) => Json::object([(
                "error",
                Json::object([
                    ("code", e.code.into()),
                    ("message", e.message.into()),
                    ("outcome", e.outcome.to_json()),
                ]),
            )]),
        };
        assert_eq!(&actual, row.get("result").unwrap(), "oracle row {index}");
        assert_eq!(
            &Json::Array(client.calls.into_inner().unwrap()),
            row.get("calls").unwrap(),
            "reads in row {index}"
        );
    }
    assert!(rows.as_array().unwrap().len() >= 25);
}
