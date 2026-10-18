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
fn model_selection_matches_original_catalog_and_keypad_policy() {
    use ivy_phone_bridge::voice_selection::{VoiceSelection, selected_keypad_model};
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let mut rows = vec![value(r#"{"action":"default"}"#)];
    let mut actual = vec![VoiceSelection::default().to_wire()];
    let normalize = |result: IvyResult<IvyJson>| {
        result.unwrap_or_else(|error| {
            IvyJson::object([
                ("code", IvyJson::String(error.code_js())),
                ("message", IvyJson::String(error.message_js())),
            ])
        })
    };
    for model in [
        IvyJson::Null,
        "".into(),
        "luna".into(),
        "sol".into(),
        "astra".into(),
        "__proto__".into(),
        "toString".into(),
        "Luna".into(),
        "future-model".into(),
    ] {
        for effort in [
            IvyJson::Null,
            "".into(),
            "high".into(),
            "ultra".into(),
            "future-effort".into(),
            value(r#""\ud800""#),
        ] {
            rows.push(IvyJson::object([
                ("action", "keypad".into()),
                ("model", model.clone()),
                ("effort", effort.clone()),
            ]));
            actual.push(normalize(
                selected_keypad_model(model.as_js_string(), effort.as_js_string())
                    .map(|v| v.to_wire()),
            ));
        }
    }
    for models in [
        value("[]"),
        value(
            r#"[{"model":"future-model","supportedReasoningEfforts":[{"reasoningEffort":"future-effort"}]}]"#,
        ),
        value(
            r#"[{"model":"future-model","supportedReasoningEfforts":[]},{"model":"future-model","supportedReasoningEfforts":[{"reasoningEffort":"future-effort"}]}]"#,
        ),
    ] {
        for effort in ["high", "future-effort"] {
            let selection = VoiceSelection {
                model: "future-model".into(),
                reasoning_effort: effort.into(),
            };
            rows.push(IvyJson::object([
                ("action", "validate".into()),
                ("selection", selection.to_wire()),
                ("models", models.clone()),
            ]));
            actual.push(normalize(
                selection.validate(&models).map(|()| IvyJson::Null),
            ));
        }
    }
    let mut child = Command::new("node")
        .args([
            "--disable-warning=ExperimentalWarning",
            "--experimental-transform-types",
            "tests/fixtures/phone-voice-selection-oracle.mjs",
        ])
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            stringify(&IvyJson::Array(rows), 64 * 1024)
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        IvyJson::Array(actual),
        value(std::str::from_utf8(&output.stdout).unwrap())
    );
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
fn timeout_policy_matches_original_typescript() {
    let output = std::process::Command::new("node")
        .args([
            "--disable-warning=ExperimentalWarning",
            "--experimental-transform-types",
            "tests/fixtures/phone-voice-call-oracle.mjs",
        ])
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut expected = vec![];
    for method in ["model/list", METHOD, "thread/read"] {
        expected.push(IvyJson::object([
            ("method", method.into()),
            ("limit", IvyJson::Number((6 * 1024 * 1024) as f64)),
        ]));
        expected.push(IvyJson::object([
            ("method", method.into()),
            ("timeout", IvyJson::Number(call_timeout_ms(method) as f64)),
        ]));
    }
    assert_eq!(
        value(std::str::from_utf8(&output.stdout).unwrap()),
        IvyJson::Array(expected)
    );
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
