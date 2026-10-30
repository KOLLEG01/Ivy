use super::super::{VoiceRpc, VoiceSchemas};
use super::*;
use ivy_contracts::json::{parse, stringify};
use ivy_host_runtime::codex_rpc::Events;
use std::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

const LIST: &str = "thread/realtime/listVoices";
fn value(text: &str) -> IvyJson {
    parse(text, 4 * 1024 * 1024).unwrap()
}
fn voices() -> IvyJson {
    value(
        r#"{"voices":{"v1":["cove","ember"],"defaultV1":"cove","v2":["marin"],"defaultV2":"marin"}}"#,
    )
}
#[derive(Default)]
struct Schemas {
    calls: Mutex<Vec<(IvyJson, usize)>>,
    reject_output: AtomicBool,
}
// Instrument the REQUIRED installed compiler seam; this is not that compiler.
impl VoiceSchemas for Schemas {
    fn validate(&self, schema: &IvyJson, input: &IvyJson, max: usize) -> IvyResult<()> {
        if input.get("version").is_some() {return ivy_contracts::schema::SchemaValidators::new().validate(schema,input,max);}
        self.calls.lock().unwrap().push((schema.clone(), max));
        stringify(input, max)?;
        if max == 64 * 1024 && self.reject_output.load(Ordering::SeqCst) {
            return Err(IvyError::new(
                "fixture_schema_rejected",
                "Malformed native speaker page.",
            ));
        }
        Ok(())
    }
}
struct Control {
    current: Mutex<VoiceRpc>,
    settings: Mutex<VoiceSpeakerSettings>,
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
impl VoiceSpeakerControl for Control {
    fn speaker_settings(&self) -> VoiceSpeakerSettings {
        self.settings.lock().unwrap().clone()
    }
}
struct Peer {
    input: BufReader<DuplexStream>,
    output: DuplexStream,
    sent: Vec<IvyJson>,
}
impl Peer {
    async fn request(&mut self, method: &str) -> IvyJson {
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
            Some(&method.into()),
            "Discovery must not prepare a task or start a Voice session"
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
fn connection(generation: usize, remove_method: bool) -> (VoiceRpc, Peer, Arc<Schemas>) {
    let mut catalog = value(include_str!(
        "../../specs/native/codex-0.159.2/catalog.json"
    ));
    if let IvyJson::Object(fields) = &mut catalog {
        if let IvyJson::Array(methods) = &mut fields
            .iter_mut()
            .find(|(key, _)| key == &JsString::from("clientRequests"))
            .unwrap()
            .1
        {
            if remove_method {
                methods.retain(|row| row.get("method") != Some(&LIST.into()));
            } else {
                let definition = methods
                    .iter_mut()
                    .find(|row| row.get("method") == Some(&LIST.into()))
                    .unwrap();
                if let IvyJson::Object(fields) = definition {
                    fields
                        .iter_mut()
                        .find(|(key, _)| key == &JsString::from("outputSchema"))
                        .unwrap()
                        .1
                        .insert("x-fixture-generation", IvyJson::Number(generation as f64));
                }
            }
        }
    }
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
    VoiceSpeakerCatalog,
    Arc<Control>,
    Peer,
    Arc<Schemas>,
    Arc<AtomicI64>,
) {
    let (rpc, peer, schemas) = connection(1, false);
    let control = Arc::new(Control {
        current: Mutex::new(rpc),
        settings: Mutex::new(VoiceSpeakerSettings {
            cwd: "synthetic-work".into(),
            voice: None,
        }),
        opens: AtomicUsize::new(0),
        stopping: AtomicBool::new(false),
    });
    let now = Arc::new(AtomicI64::new(1000));
    let clock = now.clone();
    let speakers = VoiceSpeakerCatalog::with_clock(
        control.clone(),
        Arc::new(move || clock.load(Ordering::SeqCst) as i128),
    );
    (speakers, control, peer, schemas, now)
}
fn read(speakers: &VoiceSpeakerCatalog) -> tokio::task::JoinHandle<IvyResult<IvyJson>> {
    let speakers = speakers.clone();
    tokio::spawn(async move { speakers.speaker_voices().await })
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
async fn reply_catalog(peer: &mut Peer, configuration: IvyJson) {
    let request = peer.request(LIST).await;
    peer.reply(&request, voices()).await;
    let request = peer.request("config/read").await;
    peer.reply(&request, configuration).await;
}
#[tokio::test]
async fn speaker_catalog_defaults_overrides_and_errors_are_explicit() {
    let mut cases = vec![];
    for (voice, config) in [
        (None, value("{}")),
        (None, value(r#"{"realtime":{"voice":"ember"}}"#)),
        (
            Some("cove".into()),
            value(r#"{"realtime":{"voice":"ember"}}"#),
        ),
        (None, value(r#"{"realtime":{"voice":null}}"#)),
        (None, value(r#"{"realtime":null}"#)),
        (None, value(r#"{"realtime":{"voice":7}}"#)),
        (None, value(r#"{"realtime":{"voice":"marin"}}"#)),
        (Some("marin".into()), value("{}")),
        (
            Some("ember".into()),
            value(r#"{"realtime":{"voice":false}}"#),
        ),
        (None, value(r#"{"realtime":{"voice":""}}"#)),
        (None, value(r#"{"realtime":false}"#)),
    ] {
        let mut row = IvyJson::object([("result", voices()), ("config", config)]);
        if let Some(voice) = voice {
            row.insert("voice", voice);
        }
        cases.push(row);
    }
    for result in [
        value(r#"{"voices":{"v1":[],"defaultV1":"cove"}}"#),
        value(r#"{"voices":{"v1":["cove","cove"],"defaultV1":"cove"}}"#),
        value(r#"{"voices":{"v1":["cove"],"defaultV1":"missing"}}"#),
        value(r#"{"voices":{"v1":["","ßeta","iDot"],"defaultV1":""}}"#),
    ] {
        cases.push(IvyJson::object([("result", result)]));
    }
    let selected = [
        Some("cove"),
        Some("ember"),
        Some("cove"),
        Some("cove"),
        Some("cove"),
        None,
        None,
        None,
        Some("ember"),
        None,
        Some("cove"),
        None,
        None,
        None,
        None,
    ];
    for (index, row) in cases.iter().enumerate() {
        let (speakers, control, mut peer, schemas, _) = fixture();
        control.settings.lock().unwrap().voice =
            row.get("voice").and_then(IvyJson::as_js_string).cloned();
        let task = read(&speakers);
        let request = peer.request(LIST).await;
        assert_eq!(request.get("params"), Some(&value("{}")));
        peer.reply(&request, row.get("result").unwrap().clone())
            .await;
        let invalid_catalog = (11..=14).contains(&index);
        {
            let request = peer.request("config/read").await;
            assert_eq!(
                request.get("params"),
                Some(&value(r#"{"cwd":"synthetic-work","includeLayers":false}"#))
            );
            peer.reply(
                &request,
                IvyJson::object([(
                    "config",
                    row.get("config").cloned().unwrap_or_else(|| value("{}")),
                )]),
            )
            .await;
        }
        let result = joined(task).await;
        if let Some(expected) = selected[index] {
            let catalog = result.unwrap();
            assert_eq!(catalog.get("protocol"), Some(&"v3".into()));
            assert_eq!(catalog.get("catalogVersion"), Some(&"v1".into()));
            assert_eq!(catalog.get("defaultVoiceId"), Some(&expected.into()));
            let voices = catalog.get("voices").unwrap().as_array().unwrap();
            assert_eq!(voices.len(), 2);
        } else {
            assert_eq!(
                result.unwrap_err().code,
                if invalid_catalog {
                    "phone_speaker_catalog_invalid"
                } else {
                    "phone_speaker_voice_invalid"
                }
            );
        }
        assert_eq!(peer.sent.len(), 2);
        assert_eq!(
            schemas
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, bound)| *bound == 64 * 1024)
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn coalesced_cancelled_observer_retains_load_schema_boundaries_and_caller_copies() {
    let (speakers, control, mut peer, schemas, _) = fixture();
    let first = read(&speakers);
    let request = peer.request(LIST).await;
    let second = read(&speakers);
    observers(&control, 2).await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    peer.reply(&request, voices()).await;
    let request = peer.request("config/read").await;
    peer.reply(&request, value(r#"{"config":{}}"#)).await;
    let expected = joined(second).await.unwrap();
    let mut copy = speakers.speaker_voices().await.unwrap();
    copy.insert("voices", value("[]"));
    assert_eq!(speakers.speaker_voices().await.unwrap(), expected);
    assert_eq!(peer.sent.len(), 2);
    assert_eq!(peer.sent[0].get("params"), Some(&value("{}")));
    assert_eq!(
        peer.sent[1].get("params"),
        Some(&value(r#"{"cwd":"synthetic-work","includeLayers":false}"#))
    );
    let calls = schemas.calls.lock().unwrap();
    assert_eq!(
        calls.iter().map(|(_, max)| *max).collect::<Vec<_>>(),
        vec![6 * 1024 * 1024, 64 * 1024, 6 * 1024 * 1024]
    );
    assert_eq!(
        calls[1].0.get("x-fixture-generation"),
        Some(&IvyJson::Number(1.0))
    );
}

#[tokio::test]
async fn cache_expiry_is_strict_completion_time_open_stop_policy_and_clock_rollback() {
    let (speakers, control, mut peer, _, now) = fixture();
    let task = read(&speakers);
    let request = peer.request(LIST).await;
    peer.reply(&request, voices()).await;
    let request = peer.request("config/read").await;
    now.store(9000, Ordering::SeqCst);
    peer.reply(&request, value(r#"{"config":{}}"#)).await;
    let first = joined(task).await.unwrap();
    for time in [1000, 68999] {
        now.store(time, Ordering::SeqCst);
        assert_eq!(speakers.speaker_voices().await.unwrap(), first);
    }
    control.stopping.store(true, Ordering::SeqCst);
    assert_eq!(
        speakers.speaker_voices().await.unwrap_err().code,
        "service_not_ready"
    );
    control.stopping.store(false, Ordering::SeqCst);
    now.store(69000, Ordering::SeqCst);
    let task = read(&speakers);
    reply_catalog(
        &mut peer,
        value(r#"{"config":{"realtime":{"voice":"ember"}}}"#),
    )
    .await;
    assert_ne!(joined(task).await.unwrap(), first);
    assert_eq!(peer.sent.len(), 4);
}

#[tokio::test]
async fn reconnect_uses_current_output_schema_and_preserves_original_unfenced_completion() {
    let (speakers, control, mut first_peer, first_schemas, _) = fixture();
    // Validate the next peer's complete catalog before starting a timed request.
    let (rpc, mut next_peer, next_schemas) = connection(2, false);
    let old = Arc::downgrade(&control.current.lock().unwrap().rpc);
    let first = read(&speakers);
    let request = first_peer.request(LIST).await;
    *control.current.lock().unwrap() = rpc;
    let second = read(&speakers);
    observers(&control, 2).await;
    first_peer.reply(&request, voices()).await;
    let request = next_peer.request("config/read").await;
    next_peer
        .reply(
            &request,
            value(r#"{"config":{"realtime":{"voice":"ember"}}}"#),
        )
        .await;
    let expected = joined(first).await.unwrap();
    assert_eq!(joined(second).await.unwrap(), expected);
    assert_eq!(expected.get("defaultVoiceId"), Some(&"ember".into()));
    assert_eq!(first_schemas.calls.lock().unwrap().len(), 1);
    let calls = next_schemas.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].1, 64 * 1024);
    assert_eq!(
        calls[0].0.get("x-fixture-generation"),
        Some(&IvyJson::Number(2.0))
    );
    drop(calls);
    assert_eq!(speakers.speaker_voices().await.unwrap(), expected);
    assert_eq!(next_peer.sent.len(), 1);
    assert!(old.upgrade().is_none());
    let retired = Arc::downgrade(&control.current.lock().unwrap().rpc);
    let (rpc, mut third_peer, _) = connection(3, false);
    *control.current.lock().unwrap() = rpc;
    assert!(
        retired.upgrade().is_none(),
        "The cache must not retain the retired RPC catalog"
    );
    let task = read(&speakers);
    reply_catalog(&mut third_peer, value(r#"{"config":{}}"#)).await;
    assert_ne!(joined(task).await.unwrap(), expected);
}

#[tokio::test]
async fn schema_size_native_and_missing_method_errors_are_not_cached_or_substituted() {
    let (speakers, _, mut peer, schemas, _) = fixture();
    schemas.reject_output.store(true, Ordering::SeqCst);
    let task = read(&speakers);
    let request = peer.request(LIST).await;
    peer.reply(&request, value(r#"{"voices":null}"#)).await;
    assert_eq!(
        joined(task).await.unwrap_err().code,
        "fixture_schema_rejected"
    );
    schemas.reject_output.store(false, Ordering::SeqCst);
    let oversized = IvyJson::object([(
        "voices",
        IvyJson::object([
            ("v1", IvyJson::Array(vec!["x".repeat(64 * 1024).into()])),
            ("defaultV1", "cove".into()),
        ]),
    )]);
    let expected = stringify(&oversized, 64 * 1024).unwrap_err();
    let task = read(&speakers);
    let request = peer.request(LIST).await;
    peer.reply(&request, oversized).await;
    assert_eq!(joined(task).await.unwrap_err(), expected);
    let task = read(&speakers);
    let request = peer.request(LIST).await;
    peer.reply(&request, voices()).await;
    let request = peer.request("config/read").await;
    peer.frame(IvyJson::object([("id",request.get("id").unwrap().clone()),("error",value(r#"{"code":-32000,"message":"native configuration failed","data":{"raw":"\ud800"}}"#))])).await;
    let error = joined(task).await.unwrap_err();
    assert_eq!(error.code, "phone_codex_request_failed");
    assert_eq!(
        error.details.unwrap().get("data"),
        Some(&value(r#"{"raw":"\ud800"}"#))
    );
    let task = read(&speakers);
    reply_catalog(&mut peer, value(r#"{"config":{}}"#)).await;
    assert!(joined(task).await.is_ok());
    let (rpc, _peer, schemas) = connection(1, true);
    let control = Arc::new(Control {
        current: Mutex::new(rpc),
        settings: Mutex::new(VoiceSpeakerSettings {
            cwd: "synthetic-work".into(),
            voice: None,
        }),
        opens: AtomicUsize::new(0),
        stopping: AtomicBool::new(false),
    });
    assert_eq!(
        VoiceSpeakerCatalog::new(control)
            .speaker_voices()
            .await
            .unwrap_err()
            .code,
        "native_method_unavailable"
    );
    assert!(schemas.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn both_detached_fifteen_second_deadlines_preserve_connection_for_a_fresh_read() {
    for phase in [LIST, "config/read"] {
        let (speakers, control, mut peer, _, _) = fixture();
        let task = read(&speakers);
        let mut request = peer.request(LIST).await;
        if phase == "config/read" {
            peer.reply(&request, voices()).await;
            request = peer.request("config/read").await;
        }
        assert_eq!(
            joined(task).await.unwrap_err().code,
            "native_observation_deadline"
        );
        assert!(control.current.lock().unwrap().transport().connected());
        assert_eq!(control.opens.load(Ordering::SeqCst), 1);
        peer.reply(
            &request,
            if phase == LIST {
                voices()
            } else {
                value(r#"{"config":{}}"#)
            },
        )
        .await;
        let task = read(&speakers);
        reply_catalog(&mut peer, value(r#"{"config":{}}"#)).await;
        assert!(joined(task).await.is_ok());
        assert!(control.current.lock().unwrap().transport().connected());
    }
}

#[tokio::test]
async fn current_cwd_and_voice_are_read_at_original_boundaries_then_retained_by_cache() {
    let (speakers, control, mut peer, _, _) = fixture();
    let task = read(&speakers);
    let request = peer.request(LIST).await;
    control.settings.lock().unwrap().cwd =
        value(r#""work-\ud800""#).as_js_string().unwrap().clone();
    peer.reply(&request, voices()).await;
    let request = peer.request("config/read").await;
    assert_eq!(
        request.get("params").unwrap().get("cwd"),
        Some(&value(r#""work-\ud800""#))
    );
    control.settings.lock().unwrap().voice = Some("ember".into());
    peer.reply(
        &request,
        value(r#"{"config":{"realtime":{"voice":"cove"}}}"#),
    )
    .await;
    let expected = joined(task).await.unwrap();
    assert_eq!(expected.get("defaultVoiceId"), Some(&"ember".into()));
    control.settings.lock().unwrap().voice = Some("cove".into());
    assert_eq!(speakers.speaker_voices().await.unwrap(), expected);
    assert_eq!(peer.sent.len(), 2);
}

#[test]
fn display_names_capitalize_unicode_text_and_reject_invalid_text() {
    for (id, label) in [
        ("cove", "Cove"),
        ("écho", "Écho"),
        ("ßound", "SSound"),
        ("𐐨voice", "𐐀voice"),
        ("", ""),
    ] {
        assert_eq!(display_name(&id.into()).unwrap(), JsString::from(label));
    }
    assert!(display_name(&JsString(vec![0xd800])).is_err());
}
