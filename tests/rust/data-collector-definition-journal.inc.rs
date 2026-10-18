use super::{Database, field, json, text};
use ivy_contracts::{
    IvyError, IvyJson, JsString, Outcome,
    builtin_schema::json_equal,
    json::{parse, stringify},
};
use ivy_data_collector::{definition_journal::DefinitionJournal, store_owner::StoreOwner};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::sync::{Arc, Mutex};

fn normalize(value: &mut IvyJson) {
    if let IvyJson::Object(pairs) = value {
        for (key, value) in pairs {
            if key == &JsString::from("mutationId") {
                if value != &IvyJson::from("old-operation") {
                    *value = "<fresh>".into();
                }
            } else {
                normalize(value);
            }
        }
    } else if let IvyJson::Array(items) = value {
        for value in items {
            normalize(value);
        }
    }
}
fn normalized(mut value: IvyJson) -> IvyJson {
    normalize(&mut value);
    value
}
struct Replay {
    expected: Mutex<std::collections::VecDeque<IvyJson>>,
    replies: Mutex<std::collections::VecDeque<IvyJson>>,
    store: StoreOwner,
    key: String,
}
impl RpcClient for Replay {
    fn request(&self, method: &str, params: IvyJson, _: RequestOptions) -> RequestFuture {
        if method == "system.status" {
            return Box::pin(async { Ok(json(r#"{"runtimeEpoch":"new-epoch"}"#)) });
        }
        let expected = self
            .expected
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected RPC");
        assert_eq!(method, text(&expected, "method"));
        assert!(
            json_equal(
                &normalized(params),
                &normalized(field(&expected, "params").clone())
            ),
            "parameter difference: {expected:?}"
        );
        let reply = self.replies.lock().unwrap().pop_front().unwrap();
        let store = self.store.clone();
        let key = self.key.clone();
        Box::pin(async move {
            let retained = store.call(move |s| s.metadata(&key)).await?;
            let actual = retained
                .map(|s| normalized(parse(&s, usize::MAX).unwrap()))
                .unwrap_or(IvyJson::Null);
            let expected = field(&expected, "retained");
            let expected = if expected.is_null() {
                IvyJson::Null
            } else {
                normalized(parse(&expected.as_str()?, usize::MAX)?)
            };
            assert!(
                json_equal(&actual, &expected),
                "write request must be durable before RPC: actual={actual:?}; expected={expected:?}"
            );
            if let Some(error) = reply.get("error") {
                return Err(IvyError::new(text(error, "code"), text(error, "message"))
                    .with_outcome(Outcome::NotExecuted));
            }
            Ok(reply)
        })
    }
}
#[tokio::test]
async fn definition_write_reconciliation_matches_original_pinned_journal() {
    let root = Database::new();
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/data-collector-definition-oracle.mjs",
            "runtime",
        ])
        .arg(root.0.join("oracle"))
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cases = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    for item in cases.as_array().unwrap() {
        let database = Database::new();
        let store = database.open();
        let case = field(item, "case");
        let operation = field(item, "operation").as_js_string().unwrap().clone();
        let key = format!(
            "definition-write:{}:step",
            String::from_utf16_lossy(&operation.0)
        );
        if let Some(retained) = case.get("retained") {
            let key = key.clone();
            let retained = stringify(retained, usize::MAX).unwrap();
            store
                .call(move |s| s.set_metadata(&key, &retained))
                .await
                .unwrap();
        }
        let client = Arc::new(Replay {
            expected: Mutex::new(
                field(item, "calls")
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect(),
            ),
            replies: Mutex::new(
                field(case, "replies")
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect(),
            ),
            store: store.clone(),
            key: key.clone(),
        });
        let mut journal =
            DefinitionJournal::new(store.clone(), client.clone(), Arc::new(|a, b| a.cmp(b)));
        let kind = case
            .get("kind")
            .map(|v| v.as_str().unwrap())
            .unwrap_or("definition".into());
        let result = journal
            .write(
                &kind,
                field(item, "value").clone(),
                &operation,
                "step",
                case.get("previous"),
                json(r#"{"source":{"objectId":"source","revision":1}}"#),
            )
            .await;
        let (result, error) = match result {
            Ok(value) => (value, IvyJson::Null),
            Err(e) => (
                IvyJson::Null,
                IvyJson::object([
                    ("code", e.code.into()),
                    ("message", e.message.into()),
                    ("outcome", e.outcome.as_str().into()),
                ]),
            ),
        };
        assert!(
            json_equal(&result, field(item, "result")),
            "{} result",
            text(case, "name")
        );
        assert!(
            json_equal(&error, field(item, "error")),
            "{} error: {error:?}",
            text(case, "name")
        );
        assert!(client.expected.lock().unwrap().is_empty());
        let retained = store.call(move |s| s.metadata(&key)).await.unwrap();
        let actual = retained
            .map(|s| normalized(parse(&s, usize::MAX).unwrap()))
            .unwrap_or(IvyJson::Null);
        let expected = field(item, "retained");
        let expected = if expected.is_null() {
            IvyJson::Null
        } else {
            normalized(parse(&expected.as_str().unwrap(), usize::MAX).unwrap())
        };
        assert!(json_equal(&actual, &expected));
    }
}

#[tokio::test]
async fn definition_request_identity_is_retained_before_work_and_receipts_replay() {
    let db = Database::new();
    let store = db.open();
    let client = Arc::new(Replay {
        expected: Mutex::new(Default::default()),
        replies: Mutex::new(Default::default()),
        store: store.clone(),
        key: "unused".into(),
    });
    let journal = DefinitionJournal::new(store.clone(), client, Arc::new(|a, b| a.cmp(b)));
    let operation = JsString::from("request");
    let request = r#"{"caller":"caller","input":{"action":"save"}}"#;
    assert!(
        journal
            .begin(&operation, request, 1000)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        journal
            .begin(&operation, request, 2000)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        journal
            .begin(&operation, "changed", 2000)
            .await
            .unwrap_err()
            .code,
        "operation_conflict"
    );
    assert_eq!(
        journal
            .metadata("definition-expiry:request".into())
            .await
            .unwrap()
            .unwrap(),
        "604801000"
    );
    let value = json(r#"{"jobId":"job","revision":1}"#);
    assert_eq!(
        journal
            .finish(&operation, request, value.clone(), 3000)
            .await
            .unwrap(),
        value
    );
    assert_eq!(
        journal.begin(&operation, request, 4000).await.unwrap(),
        Some(value)
    );
    assert_eq!(
        journal
            .begin(&operation, "changed", 4000)
            .await
            .unwrap_err()
            .code,
        "operation_conflict"
    );
}
