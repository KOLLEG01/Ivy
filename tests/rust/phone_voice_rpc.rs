use ivy_contracts::{
    IvyError, IvyJson, IvyResult, Outcome,
    json::{parse, stringify},
};
use ivy_host_runtime::codex_rpc::{Events, RequestOptions};
use ivy_phone_bridge::voice_rpc::{VoiceRpc, VoiceSchemas, call_timeout_ms};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

fn value(text: &str) -> IvyJson {
    parse(text, 32 * 1024 * 1024).unwrap()
}
fn params() -> IvyJson {
    value(r#"{"threadId":"synthetic","role":"user","text":"cue"}"#)
}
const METHOD: &str = "thread/realtime/appendText";
const ID: &str = "11234567-89ab-cdef-0123-456789abcdef";

#[test]
fn model_selection_preserves_keypad_aliases_and_advertised_future_efforts() {
    use ivy_phone_bridge::voice_selection::{VoiceSelection, selected_keypad_model};
    assert_eq!(
        VoiceSelection::default().to_wire(),
        value(r#"{"model":"gpt-6.1-sol","reasoningEffort":"high"}"#)
    );
    for (alias, model) in [
        ("luna", "gpt-6-luna"),
        ("sol", "gpt-6-sol"),
        ("astra", "gpt-6-astra"),
    ] {
        for effort in ["high", "ultra", "future-effort"] {
            let result = selected_keypad_model(Some(&alias.into()), Some(&effort.into())).unwrap();
            assert_eq!(result.model, model.into());
            assert_eq!(result.reasoning_effort, effort.into());
        }
    }
    for alias in [
        None,
        Some(""),
        Some("__proto__"),
        Some("toString"),
        Some("Luna"),
        Some("future-model"),
    ] {
        assert_eq!(
            selected_keypad_model(alias.map(Into::into).as_ref(), Some(&"high".into()))
                .unwrap_err()
                .code,
            "phone_voice_selection_invalid"
        );
    }
    for effort in [None, Some("")] {
        assert!(
            selected_keypad_model(Some(&"sol".into()), effort.map(Into::into).as_ref()).is_err()
        );
    }
    let selection = VoiceSelection {
        model: "future-model".into(),
        reasoning_effort: "future-effort".into(),
    };
    selection.validate(&value(r#"[{"model":"future-model","supportedReasoningEfforts":[{"reasoningEffort":"future-effort"}]}]"#)).unwrap();
    for models in [
        r#"[]"#,
        r#"[{"model":"future-model","supportedReasoningEfforts":[]}]"#,
    ] {
        assert_eq!(
            selection.validate(&value(models)).unwrap_err().code,
            "phone_voice_selection_invalid"
        );
    }
}

// Instrument the required validator boundary, not a substitute production compiler.
struct Schemas {
    reject: bool,
    calls: Mutex<Vec<(IvyJson, IvyJson, usize)>>,
}
impl VoiceSchemas for Schemas {
    fn validate(&self, schema: &IvyJson, params: &IvyJson, max: usize) -> IvyResult<()> {
        self.calls
            .lock()
            .unwrap()
            .push((schema.clone(), params.clone(), max));
        if self.reject {
            Err(IvyError::new("invalid_arguments", "fixture rejection"))
        } else {
            Ok(())
        }
    }
}
struct Peer {
    input: BufReader<DuplexStream>,
    output: DuplexStream,
}
impl Peer {
    async fn sent(&mut self) -> IvyJson {
        let mut text = String::new();
        tokio::time::timeout(Duration::from_secs(5), self.input.read_line(&mut text))
            .await
            .unwrap()
            .unwrap();
        value(&text)
    }
    async fn emit(&mut self, frame: IvyJson) {
        self.output
            .write_all(format!("{}\n", stringify(&frame, 1024).unwrap()).as_bytes())
            .await
            .unwrap();
    }
    async fn reply(&mut self, id: IvyJson) {
        self.emit(IvyJson::object([("id", id), ("result", value("{}"))]))
            .await;
    }
}
fn fixture(reject: bool) -> (VoiceRpc, Peer, Arc<Schemas>) {
    let catalog = value(include_str!(
        "../../specs/native/codex-0.154.0/catalog.json"
    ));
    let (input, sent) = tokio::io::duplex(65536);
    let (emit, output) = tokio::io::duplex(65536);
    let schemas = Arc::new(Schemas {
        reject,
        calls: Mutex::new(vec![]),
    });
    let voice =
        VoiceRpc::start(catalog, input, output, Events::default(), schemas.clone()).unwrap();
    (
        voice,
        Peer {
            input: BufReader::new(sent),
            output: emit,
        },
        schemas,
    )
}
fn send(
    voice: &VoiceRpc,
    mut options: RequestOptions,
    timeout: Option<u64>,
) -> tokio::task::JoinHandle<IvyResult<IvyJson>> {
    let voice = voice.clone();
    options.request_id = Some(ID.into());
    tokio::spawn(async move { voice.call(METHOD, params(), options, timeout).await })
}
async fn joined(task: tokio::task::JoinHandle<IvyResult<IvyJson>>) -> IvyResult<IvyJson> {
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
}

#[test]
fn voice_timeout_policy_keeps_catalog_realtime_and_control_deadlines() {
    for (method, expected) in [
        ("model/list", 15000),
        (METHOD, 30000),
        ("thread/realtime/start", 30000),
        ("thread/read", 120000),
    ] {
        assert_eq!(call_timeout_ms(method), expected, "{method}");
    }
}

#[tokio::test]
async fn schema_error_raw_outcomes_are_preserved_before_voice_dispatch() {
    use tokio::io::AsyncReadExt;

    struct RejectRaw(IvyError);
    impl VoiceSchemas for RejectRaw {
        fn validate(&self, _: &IvyJson, _: &IvyJson, _: usize) -> IvyResult<()> {
            Err(self.0.clone())
        }
    }

    let mut rows = vec![];
    for raw in [r#"["not_executed"]"#, r#"["unknown"]"#, r#"["completed"]"#] {
        for (kind, details) in [
            ("absent", None),
            ("null", Some(IvyJson::Null)),
            ("object", Some(value(r#"{"context":"schema"}"#))),
        ] {
            let mut expected = IvyError::new_js(
                value(r#""schema-\ud800""#).as_js_string().unwrap().clone(),
                value(r#""message-\udfff""#).as_js_string().unwrap().clone(),
            )
            .with_provider_outcome(value(raw));
            expected.details = details;
            let catalog = value(include_str!(
                "../../specs/native/codex-0.154.0/catalog.json"
            ));
            let (input, mut requests) = tokio::io::duplex(65536);
            let (_peer, output) = tokio::io::duplex(65536);
            let voice = VoiceRpc::start(
                catalog,
                input,
                output,
                Events::default(),
                Arc::new(RejectRaw(expected.clone())),
            )
            .unwrap();
            let actual = voice
                .call(METHOD, params(), RequestOptions::default(), None)
                .await
                .unwrap_err();
            assert_eq!(actual, expected);
            assert_eq!(actual.to_wire(), expected.to_wire());
            assert!(
                tokio::time::timeout(Duration::from_millis(20), requests.read_u8())
                    .await
                    .is_err(),
                "Schema rejection must not dispatch a native request"
            );
            rows.push(IvyJson::object([
                ("details", kind.into()),
                ("rawOutcome", value(raw)),
                ("wire", actual.to_wire()),
            ]));
            voice.transport().close("fixture_closed").await;
        }
    }
    if let Ok(trace) = std::env::var("IVY_PHONE_RAW_OUTCOME_TRACE") {
        std::fs::write(trace, stringify(&IvyJson::Array(rows), 64 * 1024).unwrap()).unwrap();
    }
}

#[tokio::test]
async fn validation_and_connection_rejections_precede_dispatch() {
    let (voice, mut peer, schemas) = fixture(true);
    assert_eq!(
        voice
            .call("missing", params(), RequestOptions::default(), None)
            .await
            .unwrap_err()
            .code,
        "native_method_unavailable"
    );
    assert!(schemas.calls.lock().unwrap().is_empty());
    assert_eq!(
        joined(send(&voice, RequestOptions::default(), None))
            .await
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    let calls = schemas.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, params());
    assert_eq!(calls[0].2, 6 * 1024 * 1024);
    let catalog = value(include_str!(
        "../../specs/native/codex-0.154.0/catalog.json"
    ));
    let definition = catalog
        .get("clientRequests")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item.get("method") == Some(&METHOD.into()))
        .unwrap();
    assert_eq!(calls[0].0, *definition.get("inputSchema").unwrap());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), peer.sent())
            .await
            .is_err()
    );
    voice.transport().close("fixture_closed").await;
    voice.transport().closed().await;
    assert_eq!(
        joined(send(&voice, RequestOptions::default(), None))
            .await
            .unwrap_err()
            .code,
        "phone_voice_unavailable"
    );
    assert_eq!(schemas.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn original_id_and_error_details_are_preserved_after_receipt() {
    let (voice, mut peer, _) = fixture(false);
    let observed = Arc::new(Mutex::new(vec![]));
    let task = send(
        &voice,
        RequestOptions {
            before_resolve: Some(Arc::new({
                let observed = observed.clone();
                move |id, reply| {
                    let observed = observed.clone();
                    Box::pin(async move {
                        observed.lock().unwrap().push((id, reply));
                        Ok(())
                    })
                }
            })),
            ..Default::default()
        },
        None,
    );
    let frame = peer.sent().await;
    assert_eq!(frame.get("id"), Some(&ID.into()));
    assert_eq!(frame.get("method"), Some(&METHOD.into()));
    assert_eq!(frame.get("params"), Some(&params()));
    let details = value(r#"{"code":-32602,"message":"fixture","data":{"unknown":true}}"#);
    peer.emit(IvyJson::object([
        ("id", ID.into()),
        ("error", details.clone()),
    ]))
    .await;
    let error = joined(task).await.unwrap_err();
    assert_eq!(error.code, "phone_codex_request_failed");
    assert_eq!(error.outcome, Outcome::NotExecuted);
    assert_eq!(error.details, Some(details));
    assert_eq!(observed.lock().unwrap().len(), 1);
    voice.transport().close("fixture_closed").await;
}

#[tokio::test]
async fn dropped_waiter_keeps_original_receipt_hook() {
    let (voice, mut peer, _) = fixture(false);
    let committed = Arc::new(tokio::sync::Notify::new());
    let task = send(
        &voice,
        RequestOptions {
            before_resolve: Some(Arc::new({
                let committed = committed.clone();
                move |id, reply| {
                    let committed = committed.clone();
                    Box::pin(async move {
                        assert_eq!(id, ID.into());
                        assert!(reply.get("result").is_some());
                        committed.notify_one();
                        Ok(())
                    })
                }
            })),
            ..Default::default()
        },
        None,
    );
    peer.sent().await;
    task.abort();
    let _ = task.await;
    peer.reply(ID.into()).await;
    tokio::time::timeout(Duration::from_secs(5), committed.notified())
        .await
        .unwrap();
    assert!(voice.transport().connected());
    voice.transport().close("fixture_closed").await;
}

#[tokio::test]
async fn explicitly_detached_timeout_does_not_commit_a_late_reply_or_disconnect() {
    let (voice, mut peer, _) = fixture(false);
    let committed = Arc::new(Mutex::new(0));
    let task = send(
        &voice,
        RequestOptions {
            detach_on_timeout: true,
            before_resolve: Some(Arc::new({
                let committed = committed.clone();
                move |_, _| {
                    let committed = committed.clone();
                    Box::pin(async move {
                        *committed.lock().unwrap() += 1;
                        Ok(())
                    })
                }
            })),
            ..Default::default()
        },
        Some(30),
    );
    peer.sent().await;
    assert_eq!(
        joined(task).await.unwrap_err().code,
        "native_observation_deadline"
    );
    peer.reply(ID.into()).await;
    // A successful subsequent response fences processing of the late frame.
    let next = {
        let voice = voice.clone();
        tokio::spawn(async move {
            voice
                .call(METHOD, params(), RequestOptions::default(), None)
                .await
        })
    };
    let frame = peer.sent().await;
    peer.reply(frame.get("id").unwrap().clone()).await;
    assert_eq!(joined(next).await.unwrap(), value("{}"));
    assert_eq!(*committed.lock().unwrap(), 0);
    assert!(voice.transport().connected());
    voice.transport().close("fixture_closed").await;
}
