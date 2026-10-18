use ivy_contracts::{IvyJson as J, JsString};
use ivy_dashboards::{handlers::Handlers, store::DashboardStore};
use ivy_sdk::{
    rpc::{RequestFuture, RequestOptions, RpcClient},
    service::InvocationContext,
};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

struct Client {
    calls: Mutex<Vec<J>>,
}
impl RpcClient for Client {
    fn request<'a>(&'a self, method: &'a str, params: J, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(J::object([
                ("method", method.into()),
                ("params", params.clone()),
            ]));
            if method == "objects.write" {
                return Ok(J::object([
                    ("object", J::object([("id", "saved".into())])),
                    ("revision", J::object([("revision", J::Number(1.0))])),
                ]));
            }
            let dashboard = params.get("objectId") == Some(&"id".into());
            Ok(J::object([
                (
                    "object",
                    J::object([
                        (
                            "contractKey",
                            if dashboard {
                                "dashboards/dashboard"
                            } else {
                                "data"
                            }
                            .into(),
                        ),
                        ("parentId", J::Null),
                        ("effectivelyArchived", J::Bool(false)),
                    ]),
                ),
                (
                    "revision",
                    J::object([
                        ("revision", J::Number(1.0)),
                        ("contractVersion", "1.1.0".into()),
                    ]),
                ),
                (
                    "content",
                    J::object([
                        ("encoding", "json".into()),
                        (
                            "value",
                            if dashboard {
                                J::object([
                                    ("title", "A".into()),
                                    ("html", "<p>x</p>".into()),
                                    (
                                        "sources",
                                        J::object([(
                                            "feed",
                                            J::object([("objectId", "source".into())]),
                                        )]),
                                    ),
                                    ("imageToken", "a".repeat(32).into()),
                                    ("metadata", J::object([("message", "metadata".into())])),
                                    ("refreshSeconds", J::Number(0.0)),
                                ])
                            } else {
                                "fresh".into()
                            },
                        ),
                    ]),
                ),
            ]))
        })
    }
}
fn context(op: Option<&str>) -> InvocationContext {
    InvocationContext {
        caller_principal_id: "caller😀".into(),
        operation_id: op.map(JsString::from),
        generation: 3,
        signal: CancellationToken::new(),
    }
}
fn fixture() -> (Handlers, Arc<Client>, Arc<Mutex<Vec<J>>>) {
    let client = Arc::new(Client {
        calls: Mutex::new(vec![]),
    });
    let renders = Arc::new(Mutex::new(vec![]));
    let sink = renders.clone();
    let handlers = Handlers::new(Arc::new(move |html, data, output, signal| {
        sink.lock().unwrap().push(J::object([
            ("html", J::String(html)),
            ("data", data),
            ("width", J::Number(output.width as f64)),
            ("cancelled", J::Bool(signal.is_cancelled())),
        ]));
        Box::pin(async { Ok(vec![1, 2, 3]) })
    }));
    handlers.set_store(Some(
        DashboardStore::new(client.clone(), None, "https://example.test/", "node".into()).unwrap(),
    ));
    (handlers, client, renders)
}
#[tokio::test]
async fn token_authorization_precedes_sources_and_rendering_and_preserves_metadata() {
    let (handlers, client, renders) = fixture();
    let mut input = J::object([("id", "id".into()), ("token", "wrong".into())]);
    let error = handlers
        .invoke("image", input.clone(), context(None))
        .await
        .unwrap_err();
    assert_eq!(error.code, "unauthenticated");
    assert_eq!(client.calls.lock().unwrap().len(), 1);
    assert!(renders.lock().unwrap().is_empty());
    input.insert("token", "a".repeat(32).into());
    let context = context(None);
    context.signal.cancel();
    let result = handlers.invoke("image", input, context).await.unwrap();
    assert_eq!(result.required("data").unwrap(), &"AQID".into());
    assert_eq!(
        result.required("metadata").unwrap(),
        &J::object([
            ("message", "metadata".into()),
            ("refreshSeconds", J::Number(0.0))
        ])
    );
    let renders = renders.lock().unwrap();
    assert_eq!(renders.len(), 1);
    assert_eq!(
        renders[0].required("data").unwrap(),
        &J::object([("feed", "fresh".into())])
    );
    assert_eq!(renders[0].required("width").unwrap(), &J::Number(800.0));
    assert_eq!(renders[0].required("cancelled").unwrap(), &J::Bool(true));
}
#[tokio::test]
async fn write_identity_is_caller_scoped_stable_and_required_before_rpc() {
    let (handlers, client, _) = fixture();
    let input = J::object([(
        "value",
        J::object([
            ("title", "A".into()),
            ("html", "x".into()),
            ("sources", J::Object(vec![])),
        ]),
    )]);
    assert_eq!(
        handlers
            .invoke("save", input.clone(), context(None))
            .await
            .unwrap_err()
            .message,
        "An operationId is required for writes."
    );
    assert!(client.calls.lock().unwrap().is_empty());
    for _ in 0..2 {
        handlers
            .invoke("save", input.clone(), context(Some("epoch:123:original")))
            .await
            .unwrap();
    }
    let calls = client.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], calls[1]);
    // Captured from the unchanged TypeScript deriveOperationId implementation.
    assert_eq!(
        calls[0]
            .required("params")
            .unwrap()
            .required("mutationId")
            .unwrap(),
        &"epoch:123:d182e078ea1fa562".into()
    );
}
#[tokio::test]
async fn offline_precedence_and_data_envelope_do_not_render() {
    let (handlers, client, renders) = fixture();
    assert_eq!(handlers.map().len(), 7);
    let result = handlers
        .invoke("data", J::object([("id", "id".into())]), context(None))
        .await
        .unwrap();
    assert_eq!(
        result.required("data").unwrap(),
        &J::object([("feed", "fresh".into())])
    );
    assert!(result.required("dashboard").unwrap().get("value").is_some());
    let time = result.required("observedAt").unwrap().as_str().unwrap();
    assert_eq!(time.len(), 24);
    chrono::DateTime::parse_from_rfc3339(&time).unwrap();
    assert!(renders.lock().unwrap().is_empty());
    handlers.set_store(None);
    assert_eq!(
        handlers
            .invoke("read", J::Null, context(None))
            .await
            .unwrap_err()
            .code,
        "service_not_ready"
    );
    assert_eq!(client.calls.lock().unwrap().len(), 2);
}
