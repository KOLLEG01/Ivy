use ivy_contracts::{
    IvyError, IvyJson,
    json::{parse, stringify},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_dashboards::sources::{Source, snapshot};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::sync::{Arc, Mutex};

struct Client {
    fixture: IvyJson,
    calls: Mutex<Vec<IvyJson>>,
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(IvyJson::object([
                ("method", method.into()),
                ("params", params.clone()),
            ]));
            if method == "objects.query" {
                return Ok(self.fixture.required("page")?.clone());
            }
            let id = params.required("objectId")?.as_str()?;
            self.fixture
                .required("objects")?
                .get(&id)
                .cloned()
                .ok_or_else(|| IvyError::new("not_found", "missing"))
        })
    }
}

#[tokio::test]
async fn matches_original_source_reads_and_failures() {
    let fixtures = parse(
        include_str!("../fixtures/dashboards/sources.json"),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    for fixture in fixtures.as_array().unwrap() {
        let client = Arc::new(Client {
            fixture: fixture.clone(),
            calls: Mutex::new(vec![]),
        });
        let sources: Vec<_> = fixture
            .required("sources")
            .unwrap()
            .as_object()
            .unwrap()
            .iter()
            .map(|(name, value)| {
                let source = if let Some(id) = value.get("objectId") {
                    Source::Object(id.as_js_string().unwrap().clone())
                } else {
                    Source::Contract {
                        key: value
                            .required("contractKey")
                            .unwrap()
                            .as_js_string()
                            .unwrap()
                            .clone(),
                        limit: value.required("limit").unwrap().as_f64().unwrap() as u32,
                    }
                };
                (name.clone(), source)
            })
            .collect();
        let result = snapshot(client.clone(), &sources, RequestOptions::default()).await;
        let actual = match result {
            Ok(value) => IvyJson::object([("value", value)]),
            Err(error) => IvyJson::object([(
                "error",
                IvyJson::object([
                    ("code", error.code.clone().into()),
                    ("message", IvyJson::String(error.message_js())),
                ]),
            )]),
        };
        assert_eq!(
            &actual,
            fixture.required("expected").unwrap(),
            "{}",
            fixture.required("name").unwrap().as_str().unwrap()
        );
        let encode = |calls: &[IvyJson]| {
            let mut calls: Vec<_> = calls
                .iter()
                .map(|v| stringify(v, MANAGEMENT_FRAME_BYTES).unwrap())
                .collect();
            calls.sort();
            calls
        };
        assert_eq!(
            encode(&client.calls.lock().unwrap()),
            encode(fixture.required("calls").unwrap().as_array().unwrap())
        );
    }
}

#[tokio::test]
async fn concurrent_sources_keep_names_and_fetch_again_on_each_snapshot() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Concurrent {
        starts: AtomicUsize,
        ready: tokio::sync::Notify,
    }
    impl RpcClient for Concurrent {
        fn request<'a>(
            &'a self,
            _: &'a str,
            params: IvyJson,
            _: RequestOptions,
        ) -> RequestFuture<'a> {
            Box::pin(async move {
                let sequence = self.starts.fetch_add(1, Ordering::SeqCst) + 1;
                if sequence % 2 == 0 {
                    self.ready.notify_one();
                } else {
                    self.ready.notified().await;
                }
                Ok(IvyJson::object([
                    (
                        "object",
                        IvyJson::object([
                            ("contractKey", "data".into()),
                            ("effectivelyArchived", IvyJson::Bool(false)),
                        ]),
                    ),
                    (
                        "content",
                        IvyJson::object([
                            ("encoding", "json".into()),
                            ("value", params.required("objectId")?.clone()),
                        ]),
                    ),
                ]))
            })
        }
    }
    let client = Arc::new(Concurrent {
        starts: AtomicUsize::new(0),
        ready: tokio::sync::Notify::new(),
    });
    let sources = vec![
        ("first".into(), Source::Object("a".into())),
        ("second".into(), Source::Object("b".into())),
    ];
    for _ in 0..2 {
        let value = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            snapshot(client.clone(), &sources, RequestOptions::default()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            value,
            IvyJson::object([("first", "a".into()), ("second", "b".into())])
        );
    }
    assert_eq!(client.starts.load(Ordering::SeqCst), 4);
}
