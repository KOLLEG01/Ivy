use ivy_contracts::{
    IvyError, IvyJson as J, Outcome,
    json::{parse, stringify},
};
use ivy_dashboards::store::DashboardStore;
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::sync::{Arc, Mutex};
struct Client {
    row: J,
    calls: Mutex<Vec<J>>,
}
impl RpcClient for Client {
    fn request<'a>(&'a self, method: &'a str, params: J, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(J::object([
                ("method", method.into()),
                ("params", params.clone()),
            ]));
            match method {
                "objects.query" => Ok(self.row.required("page")?.clone()),
                "objects.read" => self
                    .row
                    .required("objects")?
                    .get_key(params.required("objectId")?.as_js_string().unwrap())
                    .cloned()
                    .ok_or_else(|| IvyError::new("not_found", "missing")),
                "objects.write" => {
                    if let Some(e) = self.row.get("writeError") {
                        return Err(IvyError::new(
                            e.required("code")?.as_str()?,
                            e.required("message")?.as_str()?,
                        )
                        .with_outcome(
                            if e.required("outcome")?.as_str()? == "unknown" {
                                Outcome::Unknown
                            } else {
                                Outcome::NotExecuted
                            },
                        ));
                    }
                    Ok(J::object([
                        (
                            "object",
                            J::object([(
                                "id",
                                self.row.get("savedId").cloned().unwrap_or("saved".into()),
                            )]),
                        ),
                        ("revision", J::object([("revision", J::Number(8.0))])),
                    ]))
                }
                "objects.delete" => Ok(J::Object(vec![])),
                _ => panic!("unexpected {method}"),
            }
        })
    }
}
#[tokio::test]
async fn original_store_values_errors_and_rpc_transcripts() {
    let rows = parse(
        include_str!("../fixtures/dashboards/store.json"),
        usize::MAX,
    )
    .unwrap();
    for row in rows.as_array().unwrap() {
        let client = Arc::new(Client {
            row: row.clone(),
            calls: Mutex::new(vec![]),
        });
        let store = DashboardStore::new(
            client.clone(),
            row.required("root").unwrap().as_js_string().cloned(),
            &row.required("base").unwrap().as_str().unwrap(),
            row.required("node")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
        )
        .unwrap();
        let args = row.required("args").unwrap().as_array().unwrap();
        let text = |i: usize| args[i].as_js_string().unwrap().clone();
        let result = match row.required("action").unwrap().as_str().unwrap().as_str() {
            "read" => store.read(text(0)).await,
            "image" => store.image(text(0), &text(1)).await,
            "save" => {
                store
                    .save(
                        args[0].clone(),
                        args[1].as_js_string().cloned(),
                        args[2].as_f64(),
                        text(3),
                    )
                    .await
            }
            "remove" => {
                store
                    .remove(text(0), args[1].as_f64().unwrap(), text(2))
                    .await
            }
            "list" => {
                store
                    .list(
                        args[0].as_js_string().cloned(),
                        args[1].as_f64().unwrap() as u32,
                    )
                    .await
            }
            _ => panic!(),
        };
        let actual = match result {
            Ok(value) => J::object([("value", value)]),
            Err(e) => J::object([(
                "error",
                J::object([
                    ("code", J::String(e.code_js())),
                    ("message", J::String(e.message_js())),
                    ("outcome", e.outcome.as_str().into()),
                ]),
            )]),
        };
        let name = row.required("name").unwrap().as_str().unwrap();
        assert_eq!(
            stringify(&actual, usize::MAX).unwrap(),
            stringify(row.required("expected").unwrap(), usize::MAX).unwrap(),
            "{name}"
        );
        assert_eq!(
            *client.calls.lock().unwrap(),
            row.required("calls").unwrap().as_array().unwrap(),
            "{name}"
        );
    }
}

#[tokio::test]
async fn list_reads_sixteen_concurrently_before_starting_next_batch() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Batched {
        starts: AtomicUsize,
        active: AtomicUsize,
        peak: AtomicUsize,
        barrier: tokio::sync::Barrier,
    }
    impl RpcClient for Batched {
        fn request<'a>(
            &'a self,
            method: &'a str,
            params: J,
            _: RequestOptions,
        ) -> RequestFuture<'a> {
            Box::pin(async move {
                if method == "objects.query" {
                    return Ok(J::object([
                        (
                            "items",
                            J::Array(
                                (0..33)
                                    .map(|i| J::object([("objectId", format!("id{i}").into())]))
                                    .collect(),
                            ),
                        ),
                        ("nextCursor", J::Null),
                    ]));
                }
                let n = self.starts.fetch_add(1, Ordering::SeqCst);
                let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
                self.peak.fetch_max(active, Ordering::SeqCst);
                if n < 32 {
                    self.barrier.wait().await;
                }
                tokio::task::yield_now().await;
                self.active.fetch_sub(1, Ordering::SeqCst);
                let id = params.required("objectId")?.clone();
                Ok(J::object([
                    (
                        "object",
                        J::object([
                            ("contractKey", "dashboards/dashboard".into()),
                            ("parentId", J::Null),
                            ("effectivelyArchived", J::Bool(false)),
                        ]),
                    ),
                    (
                        "revision",
                        J::object([
                            ("contractVersion", "1.0.0".into()),
                            ("revision", J::Number(1.0)),
                        ]),
                    ),
                    (
                        "content",
                        J::object([
                            ("encoding", "json".into()),
                            (
                                "value",
                                J::object([
                                    ("title", id),
                                    ("html", "x".into()),
                                    ("sources", J::Object(vec![])),
                                ]),
                            ),
                        ]),
                    ),
                ]))
            })
        }
    }
    let client = Arc::new(Batched {
        starts: AtomicUsize::new(0),
        active: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
        barrier: tokio::sync::Barrier::new(16),
    });
    let store =
        DashboardStore::new(client.clone(), None, "https://example.test/", "node".into()).unwrap();
    let value = tokio::time::timeout(std::time::Duration::from_secs(2), store.list(None, 50))
        .await
        .unwrap()
        .unwrap();
    let rows = value.required("items").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 33);
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(row.required("title").unwrap(), &J::from(format!("id{i}")));
    }
    assert_eq!(client.starts.load(Ordering::SeqCst), 33);
    assert_eq!(client.peak.load(Ordering::SeqCst), 16);
}
