use ivy_chat_bridge::{
    admission::ChatAdmission,
    operations::ChatOperations,
    store::{ChatStore, Destination, Document, Reference},
};
use ivy_contracts::{
    IvyJson as Json, IvyResult,
    json::{canonical, parse},
};
use ivy_sdk::{
    clock::Clock,
    rpc::{RequestFuture, RequestOptions, RpcClient},
};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    time::Instant,
};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("ivy-chat-operations-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Time(AtomicI64);
impl Clock for Time {
    fn utc_millis(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
struct Client;
impl RpcClient for Client {
    fn request<'a>(&'a self, method: &'a str, _: Json, _: RequestOptions) -> RequestFuture<'a> {
        panic!("Unexpected Hive RPC {method}")
    }
}
fn text(v: &Json, key: &str) -> String {
    v.required(key).unwrap().as_str().unwrap()
}
fn js<'a>(v: &'a Json, key: &str) -> &'a ivy_contracts::JsString {
    v.get(key).unwrap().as_js_string().unwrap()
}
fn result(value: IvyResult<Json>) -> Json {
    match value {
        Ok(value) => Json::object([("value", value)]),
        Err(e) => Json::object([(
            "error",
            Json::object([
                ("code", e.code.into()),
                ("message", e.message.into()),
                ("outcome", e.outcome.to_json()),
            ]),
        )]),
    }
}
fn document(doc: Option<Document>) -> Json {
    doc.map(|d| {
        Json::object([
            ("value", d.value),
            ("revision", Json::Number(d.pin.revision as f64)),
        ])
    })
    .unwrap_or(Json::Null)
}
#[tokio::test]
async fn admitted_operation_identity_replay_and_recovery_match_original() {
    let output = Command::new("node")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .arg("tests/rust/fixtures/chat-operations-oracle.mjs")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let fixture = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    let now = fixture.required("now").unwrap().as_f64().unwrap() as i64;
    for case in fixture.required("cases").unwrap().as_array().unwrap() {
        let path = Fixture::new();
        let time = Arc::new(Time(AtomicI64::new(now)));
        let client = Arc::new(Client);
        let root = case
            .get("root")
            .filter(|v| !v.is_null())
            .map(|v| v.as_str().unwrap());
        let mut store = ChatStore::with_clock(
            client.clone(),
            root.clone(),
            Some(path.0.clone()),
            time.clone(),
        )
        .await
        .unwrap();
        let admission = ChatAdmission::new(case.required("definition").unwrap()).unwrap();
        let actual = if text(case, "kind") != "run" {
            result((|| {
                let ops = ChatOperations::new(&store, &admission)?;
                if text(case, "kind") == "name" {
                    return Ok(ops
                        .name(js(case, "caller"), js(case, "operationId"))?
                        .into());
                }
                ops.check(case.required("value")?)?;
                Ok(Json::Null)
            })())
        } else {
            let mut selected: Option<Document> = None;
            let mut results = vec![];
            for step in case.required("steps").unwrap().as_array().unwrap() {
                time.0.store(
                    step.get("at")
                        .and_then(Json::as_f64)
                        .map(|n| n as i64)
                        .unwrap_or(now),
                    Ordering::Relaxed,
                );
                if text(step, "action") == "reopen" {
                    store.close().await.unwrap();
                    store = ChatStore::with_clock(
                        client.clone(),
                        root.clone(),
                        Some(path.0.clone()),
                        time.clone(),
                    )
                    .await
                    .unwrap();
                    results.push(result(Ok(Json::Null)));
                    continue;
                }
                let ops = ChatOperations::new(&store, &admission).unwrap();
                let output: IvyResult<Json> = async {
                    match text(step, "action").as_str() {
                        "begin" => {
                            let doc = ops
                                .begin(js(step, "caller"), step.required("request")?)
                                .await?;
                            let result = document(Some(Document {
                                key: doc.key.clone(),
                                pin: doc.pin.clone(),
                                value: doc.value.clone(),
                                metadata: doc.metadata.clone(),
                                revision: doc.revision.clone(),
                            }));
                            selected = Some(doc);
                            Ok(result)
                        }
                        "finish" | "refuse" => {
                            let id = &selected.as_ref().unwrap().pin.object_id;
                            let doc = if text(step, "action") == "finish" {
                                ops.finish(id, step.required("outcome")?).await?
                            } else {
                                ops.reject_unapplied(
                                    id,
                                    &text(step, "code"),
                                    &text(step, "message"),
                                )
                                .await?
                            };
                            let result = Json::object([
                                ("value", doc.value.clone()),
                                ("revision", Json::Number(doc.pin.revision as f64)),
                            ]);
                            selected = Some(doc);
                            Ok(result)
                        }
                        "find" => Ok(document(
                            ops.find(js(step, "caller"), js(step, "operationId"))
                                .await?,
                        )),
                        "patch" => {
                            let old = selected.as_ref().unwrap();
                            let mut value = old.value.clone();
                            for (k, v) in step.required("patch")?.as_object().unwrap() {
                                value.insert(k.clone(), v.clone());
                            }
                            let pin = store
                                .write(
                                    "chat-bridge/operation",
                                    &value,
                                    "patch",
                                    Destination::Update {
                                        object_id: &old.pin.object_id,
                                        expected_revision: old.pin.revision,
                                    },
                                )
                                .await
                                .map_err(|e| e.into_ivy())?;
                            let doc = store
                                .read("chat-bridge/operation", Reference::Pinned(&pin))
                                .await
                                .map_err(|e| e.into_ivy())?;
                            let result = Json::object([
                                ("value", doc.value.clone()),
                                ("revision", Json::Number(doc.pin.revision as f64)),
                            ]);
                            selected = Some(doc);
                            Ok(result)
                        }
                        a => panic!("Unknown action {a}"),
                    }
                }
                .await;
                results.push(result(output));
            }
            Json::Array(results)
        };
        let saved_outcome = case.required("value").unwrap().get("outcome");
        if text(case, "kind") == "check"
            && saved_outcome.is_some_and(|value| !value.is_null() && value.as_object().is_none())
        {
            assert_eq!(
                actual.required("error").unwrap().get("code"),
                Some(&"chat_identity_conflict".into())
            );
            assert_eq!(
                actual.required("error").unwrap().get("outcome"),
                Some(&"not_executed".into())
            );
        } else if text(case, "kind") == "run" {
            assert_eq!(
                actual.as_array().unwrap().len(),
                case.required("steps").unwrap().as_array().unwrap().len()
            );
            assert_eq!(
                actual.as_array().unwrap().len(),
                case.required("expected").unwrap().as_array().unwrap().len()
            );
            for ((step, actual), expected) in case
                .required("steps")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .zip(actual.as_array().unwrap())
                .zip(case.required("expected").unwrap().as_array().unwrap())
            {
                if step.get("action") == Some(&Json::from("begin"))
                    && step.get("request").is_some_and(|request| {
                        request.as_object().is_none() || request.get("expectedBridge").is_none()
                    })
                {
                    let error = actual.required("error").unwrap();
                    assert_eq!(error.get("code"), Some(&"invalid_arguments".into()));
                    assert_eq!(error.get("outcome"), Some(&"not_executed".into()));
                } else {
                    assert_eq!(
                        canonical(actual, usize::MAX).unwrap(),
                        canonical(expected, usize::MAX).unwrap(),
                        "{}",
                        text(case, "label")
                    );
                }
            }
        } else {
            assert_eq!(
                canonical(&actual, usize::MAX).unwrap(),
                canonical(case.required("expected").unwrap(), usize::MAX).unwrap(),
                "{}",
                text(case, "label")
            );
        }
        store.close().await.unwrap();
    }
    let admission = ChatAdmission::new(fixture.required("definition").unwrap()).unwrap();
    let caller = "alice".into();
    let request = fixture.required("request").unwrap();
    let outcome = fixture.required("outcome").unwrap();
    for race in fixture.required("races").unwrap().as_array().unwrap() {
        let store = ChatStore::with_clock(
            Arc::new(Client),
            None,
            None,
            Arc::new(Time(AtomicI64::new(now))),
        )
        .await
        .unwrap();
        let ops = ChatOperations::new(&store, &admission).unwrap();
        let kind = text(race, "kind");
        let (left, right) = if kind.starts_with("begin") {
            let mut other = request.clone();
            if kind == "begin-different" {
                other.insert("replyIds", Json::Array(vec!["other".into()]));
            }
            tokio::join!(ops.begin(&caller, request), ops.begin(&caller, &other))
        } else {
            let selected = ops.begin(&caller, request).await.unwrap();
            let id = &selected.pin.object_id;
            let mut other = outcome.clone();
            if kind == "finish-different" {
                let mut ack = other.remove("acknowledgement").unwrap();
                let mut data = ack.remove("data").unwrap();
                data.insert("replyIds", Json::Array(vec!["other".into()]));
                ack.insert("data", data);
                other.insert("acknowledgement", ack);
            }
            if kind == "finish-refuse" {
                tokio::join!(
                    ops.finish(id, outcome),
                    ops.reject_unapplied(id, "synthetic_refusal", "Proven unapplied.")
                )
            } else {
                tokio::join!(ops.finish(id, outcome), ops.finish(id, &other))
            }
        };
        let successes = usize::from(left.is_ok()) + usize::from(right.is_ok());
        let mut errors = [left, right]
            .into_iter()
            .filter_map(|r| r.err().map(|e| e.code))
            .collect::<Vec<_>>();
        errors.sort();
        let saved = ops
            .find(&caller, js(request, "operationId"))
            .await
            .unwrap()
            .unwrap();
        let actual = Json::object([
            ("successes", Json::Number(successes as f64)),
            (
                "errors",
                Json::Array(errors.into_iter().map(Json::from).collect()),
            ),
            ("phase", saved.value.required("phase").unwrap().clone()),
            ("revision", Json::Number(saved.pin.revision as f64)),
            ("changes", Json::Number(store.change_version() as f64)),
        ]);
        let actual = canonical(&actual, usize::MAX).unwrap();
        assert!(
            race.required("expected")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .any(|v| canonical(v, usize::MAX).unwrap() == actual),
            "{kind}: {actual}"
        );
        store.close().await.unwrap();
    }
}
