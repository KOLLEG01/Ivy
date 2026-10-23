use ivy_chat_bridge::{
    admission::ChatAdmission, operations::ChatOperations, queries::ChatQueries, store::ChatStore,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, Outcome,
    json::{canonical, parse},
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
};
struct Client {
    case: Json,
    state: Mutex<(usize, Vec<Json>)>,
}
fn text(value: &Json, key: &str) -> String {
    value.required(key).unwrap().as_str().unwrap()
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state.1.push(Json::object([
                ("method", method.into()),
                ("params", params),
            ]));
            let index = state.0;
            if method == "objects.query" {
                state.0 += 1;
            }
            if let Some(fail) = self.case.get("fail") {
                if text(fail, "method") == method
                    && (method != "objects.query"
                        || fail.get("query").and_then(Json::as_f64) == Some(index as f64))
                {
                    return Err(IvyError::new("fixture_unavailable", "Fixture read failed.")
                        .with_outcome(Outcome::Unknown));
                }
            }
            match method {
                "objects.query" if self.case.required("match")?.as_f64() == Some(index as f64) => {
                    Ok(self.case.required("page")?.clone())
                }
                "objects.query" => Ok(Json::object([
                    ("items", Json::Array(vec![])),
                    ("nextCursor", Json::Null),
                ])),
                "objects.read" => Ok(self.case.required("read")?.clone()),
                _ => panic!("Unexpected RPC {method}"),
            }
        })
    }
}
fn nodes(predicate: &Json) -> usize {
    1 + predicate
        .get("args")
        .and_then(Json::as_array)
        .unwrap_or(&[])
        .iter()
        .map(nodes)
        .sum::<usize>()
}
#[tokio::test]
async fn exact_input_lookup_filters_channels_before_reading_pinned_content() {
    let output = Command::new("node")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .arg("tests/rust/fixtures/chat-queries-oracle.mjs")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cases = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    for case in cases.as_array().unwrap() {
        let definition = case.required("definition").unwrap();
        let admission = ChatAdmission::new(definition).unwrap();
        let client = Arc::new(Client {
            case: case.clone(),
            state: Mutex::new((0, vec![])),
        });
        let root = definition
            .get("rootObjectId")
            .filter(|v| !v.is_null())
            .map(|v| v.as_str().unwrap());
        let store = ChatStore::open(client.clone(), root, None).await.unwrap();
        let operations = ChatOperations::new(&store, &admission).unwrap();
        let queries = ChatQueries::new(&operations);
        let actual = match queries
            .input(
                case.required("caller").unwrap().as_js_string().unwrap(),
                case.required("request").unwrap(),
            )
            .await
        {
            Ok(value) => Json::object([("value", value)]),
            Err(error) => Json::object([(
                "error",
                Json::object([
                    ("code", error.code.into()),
                    ("message", error.message.into()),
                    ("outcome", error.outcome.to_json()),
                ]),
            )]),
        };
        let extra_page = case
            .get("page")
            .and_then(|page| page.get("nextCursor"))
            .is_some_and(|cursor| !cursor.is_null());
        let malformed_channel = case
            .get("read")
            .and_then(|read| read.get("content"))
            .and_then(|content| content.get("value"))
            .and_then(|input| input.get("identity"))
            .and_then(|identity| identity.get("channel"))
            .is_some_and(|channel| !channel.is_null() && channel.as_object().is_none());
        if case.required("request").unwrap().as_object().is_none() {
            let error = actual.required("error").unwrap();
            assert_eq!(error.get("code"), Some(&Json::from("invalid_arguments")));
            assert_eq!(error.get("outcome"), Some(&Json::from("not_executed")));
            assert_eq!(
                error.get("message"),
                Some(&Json::from("A request object is required."))
            );
        } else if extra_page {
            assert_eq!(
                actual.required("error").unwrap().get("code"),
                Some(&Json::from("chat_identity_conflict"))
            );
        } else if malformed_channel {
            assert_eq!(
                actual.required("error").unwrap().get("code"),
                Some(&Json::from("invalid_arguments"))
            );
        } else {
            assert_eq!(
                canonical(&actual, usize::MAX).unwrap(),
                canonical(case.required("expected").unwrap(), usize::MAX).unwrap(),
                "{}",
                text(case, "label")
            );
        }
        let calls = client.state.lock().unwrap().1.clone();
        if extra_page {
            assert_eq!(
                calls.len(),
                1,
                "An ambiguous lookup must not read pinned content"
            );
            assert_eq!(
                calls[0],
                case.required("calls").unwrap().as_array().unwrap()[0]
            );
        } else {
            assert_eq!(
                canonical(&Json::Array(calls.clone()), usize::MAX).unwrap(),
                canonical(case.required("calls").unwrap(), usize::MAX).unwrap(),
                "{} RPC trace",
                text(case, "label")
            );
        }
        let mut reads = 0;
        for call in &calls {
            let method = text(call, "method");
            if method == "objects.read" {
                reads += 1;
                continue;
            }
            assert_eq!(method, "objects.query");
            assert_eq!(reads, 0, "Content was read before lookup finished");
            let params = call.required("params").unwrap();
            assert!(nodes(params.required("where").unwrap()) <= 64);
            assert_eq!(params.get("limit"), Some(&Json::Number(1.0)));
        }
        assert!(reads <= 1);
        assert!(
            calls
                .iter()
                .filter(|v| text(v, "method") == "objects.query")
                .count()
                <= 1
        );
        assert_eq!(
            store.change_version(),
            0,
            "Read-only lookup mutated persistence"
        );
        store.close().await.unwrap();
    }
}
