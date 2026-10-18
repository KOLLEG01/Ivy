use ivy_chat_bridge::store::{Destination, Document, Pin, Reference, hive::HiveStore};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, Outcome,
    json::{canonical, parse},
    operation::parse_operation_id,
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
};
fn text(v: &Json, key: &str) -> String {
    v.required(key).unwrap().as_str().unwrap()
}
struct Client {
    case: Json,
    calls: Mutex<Vec<Json>>,
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
                ("params", params),
            ]));
            if let Some(error) = self
                .case
                .get("error")
                .filter(|e| e.get("method") == Some(&Json::from(method)))
            {
                return Err(IvyError::new(text(error, "code"), text(error, "message"))
                    .with_outcome(if text(error, "outcome") == "unknown" {
                        Outcome::Unknown
                    } else {
                        Outcome::NotExecuted
                    }));
            }
            Ok(match method {
                "system.status" => Json::object([("runtimeEpoch", "chat".into())]),
                "objects.read" => self.case.required("record")?.clone(),
                "objects.query" => self.case.required("page")?.clone(),
                "objects.write" => self.case.required("writeResult")?.clone(),
                _ => panic!("Unexpected RPC {method}"),
            })
        })
    }
}
fn document(doc: Document) -> Json {
    Json::object([
        ("key", doc.key.into()),
        ("pin", doc.pin.json()),
        ("value", doc.value),
        ("metadata", doc.metadata),
        ("revision", doc.revision),
    ])
}
#[tokio::test]
async fn public_hive_persistence_matches_original_store_calls_and_errors() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = Command::new("node")
        .current_dir(root)
        .arg("tests/rust/fixtures/chat-store-hive-oracle.mjs")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cases = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    for case in cases.as_array().unwrap() {
        let client = Arc::new(Client {
            case: case.clone(),
            calls: Mutex::new(vec![]),
        });
        let store = HiveStore::new(client.clone());
        let key = text(case, "key");
        let name = text(case, "name");
        let parent = case
            .get("parent")
            .filter(|p| !p.is_null())
            .map(|p| p.as_str().unwrap());
        let pin = Pin {
            object_id: "object".into(),
            revision: 1,
        };
        let result: IvyResult<Json> = async {
            let mut results = vec![];
            for _ in 0..case.required("repeat")?.as_f64().unwrap() as usize {
                results.push(match text(case, "action").as_str() {
                    "read" => document(
                        store
                            .read(
                                &key,
                                if case.required("pinned")?.as_bool().unwrap() {
                                    Reference::Pinned(&pin)
                                } else {
                                    Reference::Current("object")
                                },
                                parent.as_deref(),
                            )
                            .await?,
                    ),
                    "named" => store
                        .named(&key, &name, parent.as_deref())
                        .await?
                        .map(document)
                        .unwrap_or(Json::Null),
                    "available" => {
                        store.require_available(&pin).await?;
                        Json::Null
                    }
                    "write" => {
                        let receipt = store
                            .write(
                                &key,
                                case.required("value")?,
                                "fixture",
                                if case.required("create")?.as_bool().unwrap() {
                                    Destination::Create {
                                        parent_id: parent.as_deref(),
                                        name: &name,
                                    }
                                } else {
                                    Destination::Update {
                                        object_id: "object",
                                        expected_revision: 1,
                                    }
                                },
                            )
                            .await?;
                        Json::object([
                            ("pin", receipt.pin.json()),
                            ("replayed", receipt.replayed.into()),
                        ])
                    }
                    a => panic!("Unexpected action {a}"),
                });
            }
            Ok(Json::Array(results))
        }
        .await;
        let result = match result {
            Ok(v) => v,
            Err(e) => Json::object([(
                "error",
                Json::object([
                    ("code", e.code.into()),
                    ("message", e.message.into()),
                    ("outcome", e.outcome.as_str().into()),
                ]),
            )]),
        };
        let mut calls = client.calls.lock().unwrap().clone();
        let mut scoped = None;
        for call in &mut calls {
            if call.get("method") == Some(&Json::from("objects.write")) {
                let mut params = call.remove("params").unwrap();
                let mutation = text(&params, "mutationId");
                assert_eq!(parse_operation_id(&mutation).unwrap().runtime_epoch, "chat");
                if let Some(previous) = &scoped {
                    assert_eq!(previous, &mutation);
                } else {
                    scoped = Some(mutation);
                }
                params.insert("mutationId", "<scoped>".into());
                call.insert("params", params);
            }
        }
        let actual = Json::object([("result", result), ("calls", Json::Array(calls))]);
        assert_eq!(
            canonical(&actual, usize::MAX).unwrap(),
            canonical(case.required("expected").unwrap(), usize::MAX).unwrap(),
            "{}",
            text(case, "label")
        );
    }
}
