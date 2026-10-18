use ivy_chat_bridge::{
    result_text::{chat_reply_parts, read_chat_result_text},
    store::ChatStore,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    json::{canonical, parse},
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{
    path::Path,
    process::Command,
    sync::{Arc, Mutex, OnceLock},
};

fn array<'a>(value: &'a Json, key: &str) -> &'a [Json] {
    value.required(key).unwrap().as_array().unwrap()
}
fn text(value: &Json, key: &str) -> String {
    value.required(key).unwrap().as_str().unwrap()
}
fn result(value: IvyResult<Json>) -> Json {
    match value {
        Ok(value) => Json::object([("value", value)]),
        Err(error) => Json::object([(
            "error",
            Json::object([
                ("code", Json::String(error.code_js())),
                ("message", Json::String(error.message_js())),
                ("outcome", error.outcome.as_str().into()),
            ]),
        )]),
    }
}
fn equal(actual: &Json, expected: &Json, label: &str) {
    assert_eq!(
        canonical(actual, usize::MAX).unwrap(),
        canonical(expected, usize::MAX).unwrap(),
        "{label}"
    );
}
fn oracle() -> &'static Json {
    static ORACLE: OnceLock<Json> = OnceLock::new();
    ORACLE.get_or_init(|| {
        let output = Command::new("node")
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .arg("tests/rust/fixtures/chat-result-text-oracle.mjs")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if let Some(path) = std::env::var_os("IVY_CHAT_RESULT_TEXT_ORACLE_CAPTURE") {
            std::fs::write(path, &output.stdout).unwrap();
        }
        parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
    })
}

struct Reader {
    row: Json,
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
            let ordinal = {
                let mut calls = self.calls.lock().unwrap();
                calls.push(Json::object([
                    ("method", method.into()),
                    ("params", params.clone()),
                ]));
                calls.len()
            };
            if self.row.get("failureAt") == Some(&Json::Number(ordinal as f64)) {
                return Err(
                    IvyError::new("fixture_outage", "Original text read unavailable.")
                        .with_outcome(Outcome::Unknown),
                );
            }
            let saved = array(&self.row, "records")
                .iter()
                .find(|record| {
                    record
                        .required("value")
                        .unwrap()
                        .required("object")
                        .unwrap()
                        .get("id")
                        == params.get("objectId")
                })
                .ok_or_else(|| {
                    IvyError::new("revision_pruned", "Original text revision is unavailable.")
                })?;
            let mut value = saved.required("value")?.clone();
            if let Some(repeat) = saved.get("repeat") {
                let unit = repeat.required("unit")?.as_js_string().unwrap();
                let count = repeat.required("count")?.as_f64().unwrap() as usize;
                let mut content = value.required("content")?.clone();
                content.insert("value", Json::String(JsString(unit.0.repeat(count))));
                value.insert("content", content);
            }
            Ok(value)
        })
    }
}

#[tokio::test]
async fn original_text_revisions_hashes_and_read_order_match_typescript() {
    let rows = array(oracle(), "reads");
    for row in rows {
        let client = Arc::new(Reader {
            row: row.clone(),
            calls: Mutex::new(Vec::new()),
        });
        let store = ChatStore::open(client.clone(), None, None).await.unwrap();
        let actual = read_chat_result_text(
            &store,
            row.required("inputId").unwrap().as_js_string().unwrap(),
            row.required("result").unwrap(),
        )
        .await;
        equal(
            &result(actual.map(Json::String)),
            row.required("expected").unwrap(),
            &text(row, "label"),
        );
        equal(
            &Json::Array(client.calls.lock().unwrap().clone()),
            row.required("calls").unwrap(),
            &format!("read order: {}", text(row, "label")),
        );
    }
    assert!(rows.len() >= 30);
}

#[test]
fn presentation_preserves_unicode_and_original_artifact_order() {
    let rows = array(oracle(), "presentations");
    for row in rows {
        let actual = chat_reply_parts(
            row.required("result").unwrap(),
            row.required("text").unwrap().as_js_string().unwrap(),
        );
        equal(
            &result(actual.map(Json::Array)),
            row.required("expected").unwrap(),
            &text(row, "label"),
        );
    }
    assert!(rows.len() >= 40);
}
