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
                    ("outcome", e.outcome.to_json()),
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
async fn list_uses_one_projection_without_loading_html_bodies() {
    struct Projection;
    impl RpcClient for Projection {
        fn request<'a>(
            &'a self,
            method: &'a str,
            params: J,
            _: RequestOptions,
        ) -> RequestFuture<'a> {
            Box::pin(async move {
                assert_eq!(
                    method, "objects.query",
                    "listing must not read complete definitions"
                );
                assert_eq!(
                    params.required("select")?,
                    &J::Array(vec!["data:/title".into(), "data:/imageToken".into()])
                );
                Ok(J::object([
                    (
                        "items",
                        J::Array(
                            (0..33)
                                .map(|i| {
                                    J::object([
                                        ("objectId", format!("id{i}").into()),
                                        ("revision", J::Number(1.0)),
                                        (
                                            "values",
                                            J::object([
                                                ("data:/title", format!("id{i}").into()),
                                                ("data:/imageToken", J::Null),
                                            ]),
                                        ),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                    ("nextCursor", J::Null),
                ]))
            })
        }
    }
    let store = DashboardStore::new(
        Arc::new(Projection),
        None,
        "https://example.test/",
        "node".into(),
    )
    .unwrap();
    let value = store.list(None, 50).await.unwrap();
    let rows = value.required("items").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 33);
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(row.required("title").unwrap(), &J::from(format!("id{i}")));
    }
}

#[tokio::test]
async fn typed_rpc_failures_keep_their_exact_outcome_through_store_operations() {
    struct Failing {
        method: &'static str,
        error: IvyError,
        calls: Mutex<Vec<String>>,
    }
    impl RpcClient for Failing {
        fn request<'a>(&'a self, method: &'a str, _: J, _: RequestOptions) -> RequestFuture<'a> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(method.into());
                if self.method == "objects.delete" && method == "objects.read" {
                    return Err(IvyError::new("not_found", "missing"));
                }
                assert_eq!(method, self.method);
                Err(self.error.clone())
            })
        }
    }
    // This is the already typed RpcClient seam. It does not broaden the SDK
    // response parser's separate actual-string outcome admission.
    let outcomes = parse(
        r#"["not_executed","unknown","completed",["unknown"],["completed"],[["not_executed"]]]"#,
        usize::MAX,
    )
    .unwrap();
    for raw in outcomes.as_array().unwrap() {
        for (action, method) in [
            ("read", "objects.read"),
            ("image", "objects.read"),
            ("list", "objects.query"),
            ("save", "objects.write"),
            ("remove-read", "objects.read"),
            ("remove-delete", "objects.delete"),
        ] {
            let expected = IvyError::new_js(
                "typed_provider_error".into(),
                ivy_contracts::JsString(vec![0x65, 0xd800, 0x72]),
            )
            .with_provider_outcome(raw.clone())
            .with_details(J::Null);
            let client = Arc::new(Failing {
                method,
                error: expected.clone(),
                calls: Mutex::new(vec![]),
            });
            let store =
                DashboardStore::new(client.clone(), None, "https://example.test/", "node".into())
                    .unwrap();
            let result = match action {
                "read" => store.read("id".into()).await,
                "image" => store.image("id".into(), &"token".into()).await,
                "list" => store.list(None, 50).await,
                "save" => store.save(J::Null, None, None, "operation".into()).await,
                _ => store.remove("id".into(), 1.0, "operation".into()).await,
            };
            let actual = result.expect_err(action);
            assert_eq!(actual, expected, "{action}");
            assert_eq!(
                actual
                    .to_wire()
                    .required("data")
                    .unwrap()
                    .required("outcome")
                    .unwrap(),
                raw,
                "{action}"
            );
            let calls = client.calls.lock().unwrap();
            let expected_calls = if action == "remove-delete" {
                vec!["objects.read", "objects.delete"]
            } else {
                vec![method]
            };
            assert_eq!(*calls, expected_calls, "{action}");
        }
    }
}
