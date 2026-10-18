use ivy_chat_bridge::store::{ChatStore, Destination, Reference, local::Limits};
use ivy_contracts::{
    IvyError, IvyJson as Json,
    json::{canonical, parse},
};
use ivy_sdk::{
    clock::Clock,
    rpc::{RequestFuture, RequestOptions, RpcClient},
};
use std::{path::Path, process::Command, sync::Arc, time::Instant};
struct Fixed(i64);
impl Clock for Fixed {
    fn utc_millis(&self) -> i64 {
        self.0
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
struct Client;
impl RpcClient for Client {
    fn request<'a>(&'a self, method: &'a str, _: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            Ok(match method {
                "system.status" => Json::object([("runtimeEpoch", "chat".into())]),
                "objects.write" => Json::object([
                    ("object", Json::object([("id", "input".into())])),
                    ("revision", Json::object([("revision", Json::Number(2.0))])),
                ]),
                _ => panic!("Unexpected RPC {method}"),
            })
        })
    }
}
fn json(s: &str) -> Json {
    parse(s, usize::MAX).unwrap()
}
fn error(e: IvyError) -> Json {
    Json::object([(
        "error",
        Json::object([
            ("code", e.code.into()),
            ("message", e.message.into()),
            ("outcome", e.outcome.as_str().into()),
        ]),
    )])
}
#[tokio::test]
async fn facade_dates_and_terminal_input_cleanup_match_original() {
    let output = Command::new("node")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .arg("tests/rust/fixtures/chat-store-integration-oracle.mjs")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let fixture = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    let path = super::Fixture::new();
    for case in fixture.required("dates").unwrap().as_array().unwrap() {
        let time = case.required("time").unwrap().as_f64().unwrap() as i64;
        let store = ChatStore::with_clock(
            Arc::new(Client),
            Some("root".into()),
            Some(path.0.clone()),
            Arc::new(Fixed(time)),
        )
        .await
        .unwrap();
        let actual = match store
            .write(
                "chat-bridge/main",
                &json("{\"sequence\":1}"),
                "main",
                Destination::Create {
                    parent_id: Some("root"),
                    name: &time.to_string(),
                },
            )
            .await
        {
            Err(e) => error(e.into_ivy()),
            Ok(pin) => {
                let changes = store.change_version();
                store.close().await.unwrap();
                let store = ChatStore::with_clock(
                    Arc::new(Client),
                    Some("root".into()),
                    Some(path.0.clone()),
                    Arc::new(Fixed(time + 1)),
                )
                .await
                .unwrap();
                let doc = store
                    .read("chat-bridge/main", Reference::Pinned(&pin))
                    .await
                    .unwrap();
                let result = Json::object([
                    (
                        "createdAt",
                        doc.metadata.required("createdAt").unwrap().clone(),
                    ),
                    (
                        "updatedAt",
                        doc.metadata.required("updatedAt").unwrap().clone(),
                    ),
                    (
                        "revisionAt",
                        doc.revision.required("createdAt").unwrap().clone(),
                    ),
                    ("changeVersion", Json::Number(changes as f64)),
                ]);
                // Reopening resets only the volatile change counter.
                assert_eq!(store.change_version(), 0);
                store.close().await.unwrap();
                assert_eq!(
                    canonical(&result, usize::MAX).unwrap(),
                    canonical(case.required("expected").unwrap(), usize::MAX).unwrap()
                );
                continue;
            }
        };
        store.close().await.unwrap();
        assert_eq!(
            canonical(&actual, usize::MAX).unwrap(),
            canonical(case.required("expected").unwrap(), usize::MAX).unwrap()
        );
    }
    let now = fixture.required("now").unwrap().as_f64().unwrap() as i64;
    for case in fixture.required("workflows").unwrap().as_array().unwrap() {
        let store = ChatStore::with_clock(
            Arc::new(Client),
            Some("root".into()),
            None,
            Arc::new(Fixed(now)),
        )
        .await
        .unwrap();
        let main = store
            .write(
                "chat-bridge/main",
                &json("{\"queue\":[{\"inputId\":\"input\"}]}"),
                "main",
                Destination::Create {
                    parent_id: Some("root"),
                    name: "main",
                },
            )
            .await
            .unwrap();
        let child = store
            .write(
                "chat-bridge/collection",
                &json("{}"),
                "child",
                Destination::Create {
                    parent_id: Some("input"),
                    name: "child",
                },
            )
            .await
            .unwrap();
        let blob = store
            .save_local_blob(
                "chat-bridge/evidence-chunk",
                "blob",
                &child.object_id,
                &[0, 1, 255],
            )
            .await
            .unwrap();
        let mut trace = vec![Json::object([
            ("stage", "created".into()),
            ("changeVersion", Json::Number(store.change_version() as f64)),
            (
                "blob",
                Json::Array(
                    store
                        .local_blob("chat-bridge/evidence-chunk", &blob, &child.object_id)
                        .await
                        .unwrap()
                        .into_iter()
                        .map(|b| Json::Number(b as f64))
                        .collect(),
                ),
            ),
        ])];
        store
            .set_limits(Limits {
                count: 2,
                bytes: 256 * 1024 * 1024,
            })
            .await
            .unwrap();
        let input = Json::object([
            ("payload", json("{\"images\":[]}")),
            ("state", case.required("state").unwrap().clone()),
            ("finishedAt", "2026-09-21T14:13:20.123Z".into()),
        ]);
        store
            .write(
                "chat-bridge/input",
                &input,
                "finished",
                Destination::Update {
                    object_id: "input",
                    expected_revision: 1,
                },
            )
            .await
            .unwrap();
        trace.push(Json::object([
            ("stage", "queued".into()),
            (
                "retained",
                store
                    .named_in("chat-bridge/collection", "child", Some("input"))
                    .await
                    .unwrap()
                    .is_some()
                    .into(),
            ),
            ("changeVersion", Json::Number(store.change_version() as f64)),
        ]));
        store
            .write(
                "chat-bridge/main",
                &json("{\"queue\":[]}"),
                "drained",
                Destination::Update {
                    object_id: &main.object_id,
                    expected_revision: 1,
                },
            )
            .await
            .unwrap();
        store.require_available(&main).await.unwrap();
        let blob_error = store
            .local_blob("chat-bridge/evidence-chunk", &blob, &child.object_id)
            .await
            .unwrap_err()
            .into_ivy()
            .code;
        trace.push(Json::object([
            ("stage", "drained".into()),
            (
                "retained",
                store
                    .named_in("chat-bridge/collection", "child", Some("input"))
                    .await
                    .unwrap()
                    .is_some()
                    .into(),
            ),
            ("changeVersion", Json::Number(store.change_version() as f64)),
            (
                "mainRevision",
                Json::Number(
                    store
                        .read("chat-bridge/main", Reference::Pinned(&main))
                        .await
                        .unwrap()
                        .pin
                        .revision as f64,
                ),
            ),
            ("blobError", blob_error.into()),
        ]));
        assert_eq!(
            canonical(&Json::Array(trace), usize::MAX).unwrap(),
            canonical(case.required("expected").unwrap(), usize::MAX).unwrap(),
            "{:?}",
            case.get("state")
        );
        store.close().await.unwrap();
    }
}

#[tokio::test]
async fn committed_local_write_advances_version_after_waiter_cancellation() {
    struct PausedClock {
        entered: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl Clock for PausedClock {
        fn utc_millis(&self) -> i64 {
            if let Some(entered) = self.entered.lock().unwrap().take() {
                entered.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
            }
            1790000000123
        }
        fn monotonic(&self) -> Instant {
            Instant::now()
        }
    }
    let (entered, wait) = tokio::sync::oneshot::channel();
    let (release, receive) = std::sync::mpsc::channel();
    let clock = Arc::new(PausedClock {
        entered: std::sync::Mutex::new(Some(entered)),
        release: std::sync::Mutex::new(receive),
    });
    let store = Arc::new(
        ChatStore::with_clock(Arc::new(Client), Some("root".into()), None, clock)
            .await
            .unwrap(),
    );
    let owner = store.clone();
    let write = tokio::spawn(async move {
        owner
            .write(
                "chat-bridge/main",
                &json("{\"queue\":[]}"),
                "main",
                Destination::Create {
                    parent_id: Some("root"),
                    name: "main",
                },
            )
            .await
    });
    wait.await.unwrap();
    write.abort();
    assert!(write.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    assert!(
        store
            .named("chat-bridge/main", "main")
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(store.change_version(), 1);
    Arc::try_unwrap(store).ok().unwrap().close().await.unwrap();
}
