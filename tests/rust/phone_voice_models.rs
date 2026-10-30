use super::*;
use ivy_contracts::json::{parse, stringify};
use ivy_host_runtime::codex_rpc::Events;
use std::{
    sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

fn value(text: &str) -> IvyJson {
    parse(text, 4 * 1024 * 1024).unwrap()
}
fn model(id: IvyJson, hidden: bool) -> IvyJson {
    IvyJson::object([
        ("model", id),
        ("displayName", "Provider model".into()),
        ("description", "Native description".into()),
        ("defaultReasoningEffort", "future-effort".into()),
        (
            "supportedReasoningEfforts",
            value(r#"[{"reasoningEffort":"future-effort","description":"Native effort"}]"#),
        ),
        ("isDefault", false.into()),
        ("hidden", hidden.into()),
        ("extra", "omitted".into()),
    ])
}
fn page(data: Vec<IvyJson>, next: Option<IvyJson>) -> IvyJson {
    let mut fields = vec![("data".into(), IvyJson::Array(data))];
    if let Some(next) = next {
        fields.push(("nextCursor".into(), next));
    }
    IvyJson::Object(fields)
}
#[derive(Default)]
struct Schemas {
    calls: Mutex<Vec<(IvyJson, usize)>>,
    reject_output: AtomicBool,
}
// An instrumented REQUIRED compiler boundary, not a substitute implementation or
// a claim that installed native schema compilation has been qualified here.
impl super::super::VoiceSchemas for Schemas {
    fn validate(&self, schema: &IvyJson, input: &IvyJson, max: usize) -> IvyResult<()> {
        self.calls.lock().unwrap().push((schema.clone(), max));
        stringify(input, max)?;
        if max == 256 * 1024 && self.reject_output.load(Ordering::SeqCst) {
            return Err(IvyError::new(
                "fixture_schema_rejected",
                "Malformed installed-model page.",
            ));
        }
        Ok(())
    }
}
struct Control {
    current: Mutex<VoiceRpc>,
    opens: AtomicUsize,
    stopping: AtomicBool,
}
impl VoiceModelControl for Control {
    fn open(&self) -> Pin<Box<dyn Future<Output = IvyResult<VoiceRpc>> + Send + '_>> {
        Box::pin(async {
            self.opens.fetch_add(1, Ordering::SeqCst);
            if self.stopping.load(Ordering::SeqCst) {
                Err(IvyError::new(
                    "service_not_ready",
                    "Codex Voice is stopping.",
                ))
            } else {
                Ok(self.current.lock().unwrap().clone())
            }
        })
    }
    fn current(&self) -> Option<VoiceRpc> {
        Some(self.current.lock().unwrap().clone())
    }
}
struct Peer {
    input: BufReader<DuplexStream>,
    output: DuplexStream,
    sent: Vec<IvyJson>,
}
impl Peer {
    async fn request(&mut self) -> IvyJson {
        let mut text = String::new();
        assert_ne!(
            tokio::time::timeout(Duration::from_secs(5), self.input.read_line(&mut text))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        let request = value(&text);
        assert_eq!(
            request.get("method"),
            Some(&"model/list".into()),
            "Discovery must not prepare a task or turn"
        );
        self.sent.push(request.clone());
        request
    }
    async fn frame(&mut self, frame: IvyJson) {
        self.output
            .write_all(format!("{}\n", stringify(&frame, 4 * 1024 * 1024).unwrap()).as_bytes())
            .await
            .unwrap();
    }
    async fn reply(&mut self, request: &IvyJson, result: IvyJson) {
        self.frame(IvyJson::object([
            ("id", request.get("id").unwrap().clone()),
            ("result", result),
        ]))
        .await;
    }
}
fn connection(catalog: Option<IvyJson>) -> (VoiceRpc, Peer, Arc<Schemas>) {
    let catalog = catalog.unwrap_or_else(|| {
        value(include_str!(
            "../../specs/native/codex-0.154.0/catalog.json"
        ))
    });
    let (input, sent) = tokio::io::duplex(65536);
    let (emit, output) = tokio::io::duplex(65536);
    let schemas = Arc::new(Schemas::default());
    let rpc = VoiceRpc::start(catalog, input, output, Events::default(), schemas.clone()).unwrap();
    (
        rpc,
        Peer {
            input: BufReader::new(sent),
            output: emit,
            sent: vec![],
        },
        schemas,
    )
}
fn fixture() -> (
    VoiceModelCatalog,
    Arc<Control>,
    Peer,
    Arc<Schemas>,
    Arc<AtomicI64>,
) {
    let (rpc, peer, schemas) = connection(None);
    let control = Arc::new(Control {
        current: Mutex::new(rpc),
        opens: AtomicUsize::new(0),
        stopping: AtomicBool::new(false),
    });
    let now = Arc::new(AtomicI64::new(1000));
    let clock = now.clone();
    let models = VoiceModelCatalog::with_clock(
        control.clone(),
        Arc::new(move || clock.load(Ordering::SeqCst) as i128),
    );
    (models, control, peer, schemas, now)
}
fn read(models: &VoiceModelCatalog) -> tokio::task::JoinHandle<IvyResult<IvyJson>> {
    let models = models.clone();
    tokio::spawn(async move { models.models().await })
}
async fn joined(task: tokio::task::JoinHandle<IvyResult<IvyJson>>) -> IvyResult<IvyJson> {
    tokio::time::timeout(Duration::from_secs(20), task)
        .await
        .unwrap()
        .unwrap()
}
async fn observers(control: &Control, count: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while control.opens.load(Ordering::SeqCst) < count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn pages_coalesce_retain_cancelled_observers_and_copy_utf16_results() {
    let (models, control, mut peer, schemas, _) = fixture();
    let first = read(&models);
    let request = peer.request().await;
    let second = read(&models);
    // Await entry into the second observer before releasing the first page.
    observers(&control, 2).await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    peer.reply(
        &request,
        page(
            vec![
                model("visible".into(), true),
                model("visible".into(), false),
            ],
            Some(value(r#""\ud800""#)),
        ),
    )
    .await;
    let request = peer.request().await;
    assert_eq!(
        request.get("params"),
        Some(&value(
            r#"{"limit":100,"includeHidden":false,"cursor":"\ud800"}"#
        ))
    );
    peer.reply(
        &request,
        page(vec![model(value(r#""\udfff""#), false)], Some("".into())),
    )
    .await;
    let expected = joined(second).await.unwrap();
    assert_eq!(expected.as_array().unwrap().len(), 2);
    assert_eq!(
        expected.as_array().unwrap()[1].get("model"),
        Some(&value(r#""\udfff""#))
    );
    assert!(expected.as_array().unwrap()[0].get("hidden").is_none());
    assert!(expected.as_array().unwrap()[0].get("extra").is_none());
    let mut changed = models.models().await.unwrap();
    if let IvyJson::Array(items) = &mut changed {
        items.clear();
    }
    assert_eq!(models.models().await.unwrap(), expected);
    assert_eq!(peer.sent.len(), 2);
    assert_eq!(
        peer.sent[0].get("params"),
        Some(&value(r#"{"limit":100,"includeHidden":false}"#))
    );
    let calls = schemas.calls.lock().unwrap();
    assert_eq!(
        calls.iter().map(|(_, max)| *max).collect::<Vec<_>>(),
        vec![6 * 1024 * 1024, 256 * 1024, 6 * 1024 * 1024, 256 * 1024]
    );
    let definition = control
        .current
        .lock()
        .unwrap()
        .rpc
        .catalog()
        .get("clientRequests")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v.get("method") == Some(&"model/list".into()))
        .unwrap()
        .clone();
    assert_eq!(calls[0].0, *definition.get("inputSchema").unwrap());
    assert_eq!(calls[1].0, *definition.get("outputSchema").unwrap());
}

#[tokio::test]
async fn cache_expires_from_completion_at_exact_boundary_and_open_always_runs() {
    let (models, control, mut peer, _, now) = fixture();
    let task = read(&models);
    let request = peer.request().await;
    now.store(5000, Ordering::SeqCst);
    peer.reply(&request, page(vec![model("first".into(), false)], None))
        .await;
    let first = joined(task).await.unwrap();
    for time in [1000, 64_999] {
        now.store(time, Ordering::SeqCst);
        assert_eq!(models.models().await.unwrap(), first);
    }
    control.stopping.store(true, Ordering::SeqCst);
    assert_eq!(models.models().await.unwrap_err().code, "service_not_ready");
    control.stopping.store(false, Ordering::SeqCst);
    now.store(65_000, Ordering::SeqCst);
    let task = read(&models);
    let request = peer.request().await;
    peer.reply(
        &request,
        page(vec![model("new".into(), false)], Some(IvyJson::Null)),
    )
    .await;
    assert_ne!(joined(task).await.unwrap(), first);
    assert_eq!(peer.sent.len(), 2);
}

#[tokio::test]
async fn reconnect_fences_inflight_load_uses_new_page_connection_and_retries_without_fallback() {
    let (models, control, mut first_peer, _, _) = fixture();
    let first = read(&models);
    let request = first_peer.request().await;
    let (next_rpc, mut next_peer, next_schemas) = connection(None);
    *control.current.lock().unwrap() = next_rpc;
    // Reconnect does not discard the old in-flight promise or start a second one.
    let second = read(&models);
    observers(&control, 2).await;
    first_peer
        .reply(&request, page(vec![], Some("second".into())))
        .await;
    let request = next_peer.request().await;
    next_peer
        .reply(
            &request,
            page(vec![model("lost-generation".into(), false)], None),
        )
        .await;
    for task in [first, second] {
        assert_eq!(
            joined(task).await.unwrap_err().code,
            "phone_voice_unavailable"
        );
    }
    // Captured output schema owner validates BOTH pages; the new owner validates input only.
    assert_eq!(next_schemas.calls.lock().unwrap().len(), 1);
    let retry = read(&models);
    let request = next_peer.request().await;
    next_peer.frame(IvyJson::object([("id",request.get("id").unwrap().clone()),("error",value(r#"{"code":-32000,"message":"native catalog unavailable","data":{"raw":"\ud800"}}"#))])).await;
    let error = joined(retry).await.unwrap_err();
    assert_eq!(error.code, "phone_codex_request_failed");
    assert_eq!(
        error.details.unwrap().get("data"),
        Some(&value(r#"{"raw":"\ud800"}"#))
    );
    let retry = read(&models);
    let request = next_peer.request().await;
    next_peer
        .reply(
            &request,
            page(vec![model("after-reconnect".into(), false)], None),
        )
        .await;
    let expected = joined(retry).await.unwrap();
    let previous = Arc::downgrade(&control.current.lock().unwrap().rpc);
    // A cached result is also invalidated by a replacement identity with identical catalog bytes.
    let (rpc, mut third_peer, _) = connection(None);
    *control.current.lock().unwrap() = rpc;
    assert!(
        previous.upgrade().is_none(),
        "Cached identity must not retain the retired RPC actor/catalog"
    );
    let task = read(&models);
    let request = third_peer.request().await;
    third_peer.reply(&request, page(vec![], None)).await;
    assert_ne!(joined(task).await.unwrap(), expected);
}

#[tokio::test]
async fn native_schema_failure_size_limit_and_missing_catalog_are_not_cached() {
    let (models, _, mut peer, schemas, _) = fixture();
    schemas.reject_output.store(true, Ordering::SeqCst);
    let task = read(&models);
    let request = peer.request().await;
    peer.reply(&request, value(r#"{"data":[{"model":"incomplete"}]}"#))
        .await;
    assert_eq!(
        joined(task).await.unwrap_err().code,
        "fixture_schema_rejected"
    );
    schemas.reject_output.store(false, Ordering::SeqCst);
    let task = read(&models);
    let request = peer.request().await;
    let oversized = page(vec![model("x".repeat(256 * 1024).into(), false)], None);
    let expected = stringify(&oversized, 256 * 1024).unwrap_err();
    peer.reply(&request, oversized).await;
    assert_eq!(joined(task).await.unwrap_err(), expected);
    let task = read(&models);
    let request = peer.request().await;
    peer.reply(&request, page(vec![], None)).await;
    assert_eq!(joined(task).await.unwrap(), value("[]"));

    let mut catalog = value(include_str!(
        "../../specs/native/codex-0.154.0/catalog.json"
    ));
    if let IvyJson::Object(fields) = &mut catalog {
        if let IvyJson::Array(items) = &mut fields
            .iter_mut()
            .find(|(key, _)| key == &JsString::from("clientRequests"))
            .unwrap()
            .1
        {
            items.retain(|item| item.get("method") != Some(&"model/list".into()));
        }
    }
    let (rpc, _, schemas) = connection(Some(catalog));
    let control = Arc::new(Control {
        current: Mutex::new(rpc),
        opens: AtomicUsize::new(0),
        stopping: AtomicBool::new(false),
    });
    assert_eq!(
        VoiceModelCatalog::new(control)
            .models()
            .await
            .unwrap_err()
            .code,
        "native_method_unavailable"
    );
    assert!(schemas.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn visible_limit_hidden_duplicates_and_utf16_duplicates_match_original() {
    for (rows, valid) in [
        (
            (0..256)
                .map(|id| model(format!("m{id}").into(), false))
                .collect::<Vec<_>>(),
            true,
        ),
        (
            (0..257)
                .map(|id| model(format!("m{id}").into(), false))
                .collect(),
            false,
        ),
        (
            vec![
                model(value(r#""\ud800""#), false),
                model(value(r#""\ud800""#), false),
            ],
            false,
        ),
        (
            vec![
                model(value(r#""\ud800""#), false),
                model(value(r#""\ud801""#), false),
            ],
            true,
        ),
        (
            (0..300).map(|_| model("hidden".into(), true)).collect(),
            true,
        ),
    ] {
        let (models, _, mut peer, _, _) = fixture();
        let task = read(&models);
        let request = peer.request().await;
        peer.reply(&request, page(rows, None)).await;
        let result = joined(task).await;
        assert_eq!(result.is_ok(), valid);
        if !valid {
            assert_eq!(result.unwrap_err().code, "phone_voice_catalog_invalid");
        }
    }
}

#[tokio::test]
async fn cursor_limit_allows_terminal_257th_page_but_rejects_another_cursor_and_repeats() {
    for terminal in [true, false] {
        let (models, _, mut peer, _, _) = fixture();
        let task = read(&models);
        for index in 0..=256 {
            let request = peer.request().await;
            if index != 0 {
                assert_eq!(
                    request.get("params").unwrap().get("cursor"),
                    Some(&format!("c{}", index - 1).into())
                );
            }
            peer.reply(
                &request,
                page(
                    vec![],
                    if index == 256 && terminal {
                        None
                    } else {
                        Some(format!("c{index}").into())
                    },
                ),
            )
            .await;
        }
        let result = joined(task).await;
        assert_eq!(result.is_ok(), terminal);
        if !terminal {
            assert_eq!(result.unwrap_err().code, "phone_voice_catalog_invalid");
        }
    }
    let (models, _, mut peer, _, _) = fixture();
    let task = read(&models);
    for _ in 0..2 {
        let request = peer.request().await;
        peer.reply(&request, page(vec![], Some(value(r#""\ud800""#))))
            .await;
    }
    assert_eq!(
        joined(task).await.unwrap_err().code,
        "phone_voice_catalog_invalid"
    );
}

#[tokio::test]
async fn detached_catalog_deadline_preserves_connection_for_next_voice_operation() {
    let (models, control, mut peer, _, _) = fixture();
    let task = read(&models);
    let request = peer.request().await;
    assert_eq!(
        joined(task).await.unwrap_err().code,
        "native_observation_deadline"
    );
    let rpc = control.current.lock().unwrap().clone();
    assert!(rpc.transport().connected());
    assert_eq!(control.opens.load(Ordering::SeqCst), 1);
    // A late reply is observed by the shared actor but cannot fill the expired catalog.
    peer.reply(&request, page(vec![model("too-late".into(), false)], None))
        .await;
    let retry = read(&models);
    let request = peer.request().await;
    peer.reply(&request, page(vec![model("fresh".into(), false)], None))
        .await;
    assert_eq!(
        joined(retry).await.unwrap().as_array().unwrap()[0].get("model"),
        Some(&"fresh".into())
    );
    assert!(rpc.transport().connected());
}

#[tokio::test]
async fn discovery_bounds_duplicates_and_pagination_have_explicit_results() {
    let cases = vec![
        vec![page(vec![], None)],
        vec![
            page(
                vec![
                    model("custom-provider".into(), false),
                    model("custom-provider".into(), true),
                ],
                Some(value(r#""\ud800""#)),
            ),
            page(vec![model(value(r#""\udfff""#), false)], Some("".into())),
        ],
        vec![page(
            vec![
                model("duplicate".into(), false),
                model("duplicate".into(), false),
            ],
            None,
        )],
        vec![page(
            (0..256)
                .map(|id| model(format!("m{id}").into(), false))
                .collect(),
            Some(IvyJson::Null),
        )],
        vec![page(
            (0..257)
                .map(|id| model(format!("m{id}").into(), false))
                .collect(),
            None,
        )],
        vec![
            page(vec![], Some("again".into())),
            page(vec![], Some("again".into())),
        ],
        (0..=256)
            .map(|index| {
                page(
                    vec![],
                    if index == 256 {
                        None
                    } else {
                        Some(format!("c{index}").into())
                    },
                )
            })
            .collect(),
        (0..=256)
            .map(|index| page(vec![], Some(format!("c{index}").into())))
            .collect(),
    ];
    let counts = [1, 2, 1, 1, 1, 2, 257, 257];
    for (index, pages) in cases.iter().enumerate() {
        let (models, _, mut peer, schemas, _) = fixture();
        let task = read(&models);
        for (page_index, page) in pages.iter().take(counts[index]).enumerate() {
            let request = peer.request().await;
            let params = request.get("params").unwrap();
            assert_eq!(params.get("limit"), Some(&100.0.into()));
            assert_eq!(params.get("includeHidden"), Some(&false.into()));
            assert_eq!(
                params.get("cursor"),
                if page_index == 0 {
                    None
                } else {
                    pages[page_index - 1].get("nextCursor")
                }
            );
            peer.reply(&request, page.clone()).await;
        }
        let result = joined(task).await;
        match index {
            0 | 6 => assert!(result.unwrap().as_array().unwrap().is_empty()),
            1 => assert_eq!(result.unwrap().as_array().unwrap().len(), 2),
            3 => assert_eq!(result.unwrap().as_array().unwrap().len(), 256),
            _ => assert_eq!(result.unwrap_err().code, "phone_voice_catalog_invalid"),
        }
        assert_eq!(peer.sent.len(), counts[index]);
        assert_eq!(
            schemas
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, bound)| *bound == 256 * 1024)
                .count(),
            counts[index]
        );
    }
}
