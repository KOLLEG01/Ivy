//! The connected Rust call path over an inert native pipe and Voice endpoint.
//! No SIP transport, account, physical device or speech provider is contacted.
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::{digest, hash_json},
    json::{canonical, parse},
};
use ivy_phone_bridge::{
    admission::PhoneAdmission,
    call_commands::PhoneCallCommands,
    journal::{PhoneJournal, PhoneJournalLimits, PhoneJournalOptions, PhoneJournalOwner},
    native_client::PhoneNativeClient,
    service_bridge::PhoneBridge,
    service_flow::{FlowFuture, PhoneFlow, PhoneFlowVoice},
    service_speech::synthesize_announcement,
    voice_rpc::{VoiceAsyncGuard, VoiceSelectionCommand},
    voice_selection::VoiceSelection,
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::Semaphore,
    task::JoinHandle,
};
fn object<const N: usize>(items: [(&str, IvyJson); N]) -> IvyJson {
    IvyJson::object(items)
}
fn text(value: &IvyJson, key: &str) -> JsString {
    value.get(key).unwrap().as_js_string().unwrap().clone()
}
fn equal(actual: &IvyJson, expected: &IvyJson) {
    assert_eq!(
        canonical(actual, 65536).unwrap(),
        canonical(expected, 65536).unwrap()
    );
}
fn uuid() -> JsString {
    uuid::Uuid::new_v4().to_string().into()
}
async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(15), future)
        .await
        .expect("Rust call path did not settle")
}
async fn until(mut condition: impl FnMut() -> bool) {
    bounded(async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
}

#[derive(Clone)]
struct Voice {
    journal: PhoneJournal,
    prompts: Arc<Mutex<Vec<JsString>>>,
    gate: Option<Arc<Semaphore>>,
    preparing: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
}
impl Voice {
    async fn finish(&self, call: &IvyJson, method: &str, mut args: IvyJson) -> IvyResult<()> {
        args.insert("epoch", call.get("epoch").unwrap().clone());
        args.insert("callId", call.get("callId").unwrap().clone());
        args.insert("method", method.into());
        if args.get("operationId").is_none() {
            args.insert("operationId", uuid().into());
        }
        if args.get("requestHash").is_none() {
            args.insert("requestHash", format!("sha256:{}", "0".repeat(64)).into());
        }
        self.journal.submit(args.clone()).await?;
        if method == "call.bindVoice" {
            self.journal.finish_voice_binding(args, None).await?;
        } else {
            self.journal
                .finish_voice_prompt(args, "sent".into())
                .await?;
        }
        Ok(())
    }
}
impl PhoneFlowVoice for Voice {
    fn models(&self) -> FlowFuture<IvyJson> {
        Box::pin(async {
            Ok(IvyJson::Array(vec![object([
                ("model", "gpt-6.1-sol".into()),
                (
                    "supportedReasoningEfforts",
                    IvyJson::Array(vec![object([("reasoningEffort", "high".into())])]),
                ),
            ])]))
        })
    }
    fn speaker_voices(&self) -> FlowFuture<IvyJson> {
        Box::pin(async {
            Ok(object([
                ("protocol", "v3".into()),
                ("defaultVoiceId", "cove".into()),
                (
                    "voices",
                    IvyJson::Array(vec![object([
                        ("id", "cove".into()),
                        ("displayName", "Cove".into()),
                    ])]),
                ),
            ]))
        })
    }
    fn speaker_voice(&self, _: &JsString) -> Option<JsString> {
        Some("cove".into())
    }
    fn failed(&self, _: &JsString) -> bool {
        false
    }
    fn inputs(&self, _: &JsString) -> IvyResult<Vec<IvyJson>> {
        Ok(vec![])
    }
    fn answer(&self, _: JsString, _: IvyJson, _: IvyJson, _: JsString) -> FlowFuture<()> {
        Box::pin(async {
            Err(IvyError::new(
                "phone_voice_input_missing",
                "No input in this fixture.",
            ))
        })
    }
    fn prewarm(&self, _: JsString, _: VoiceSelection) -> FlowFuture<()> {
        Box::pin(async { Ok(()) })
    }
    fn prepare(&self, call: IvyJson, generation: u32, _: VoiceSelection) -> FlowFuture<JsString> {
        let voice = self.clone();
        Box::pin(async move {
            voice.preparing.store(true, Ordering::SeqCst);
            if let Some(gate) = &voice.gate {
                gate.acquire().await.unwrap().forget();
            }
            let thread = uuid();
            voice
                .finish(
                    &call,
                    "call.bindVoice",
                    object([
                        ("voiceGeneration", f64::from(generation).into()),
                        ("threadId", thread.clone().into()),
                    ]),
                )
                .await?;
            Ok(thread)
        })
    }
    fn start(
        &self,
        call: IvyJson,
        generation: u32,
        thread: JsString,
        prompt: JsString,
        _: JsString,
        guard: VoiceAsyncGuard,
    ) -> FlowFuture<JsString> {
        let voice = self.clone();
        Box::pin(async move {
            guard().await?;
            voice
                .finish(
                    &call,
                    "call.promptVoice",
                    object([
                        ("voiceGeneration", f64::from(generation).into()),
                        ("threadId", thread.into()),
                        ("prompt", prompt.clone().into()),
                    ]),
                )
                .await?;
            voice.prompts.lock().unwrap().push(prompt);
            Ok("fixture answer".into())
        })
    }
    fn connected(&self, _: JsString, guard: VoiceAsyncGuard) -> FlowFuture<()> {
        Box::pin(async move { guard().await })
    }
    fn select(
        &self,
        _: IvyJson,
        _: JsString,
        _: VoiceSelectionCommand,
        _: VoiceSelection,
        _: VoiceAsyncGuard,
    ) -> FlowFuture<()> {
        Box::pin(async {
            Err(IvyError::new(
                "phone_voice_unavailable",
                "Selection is outside this fixture.",
            ))
        })
    }
    fn prompt(
        &self,
        call: IvyJson,
        thread: JsString,
        prompt: JsString,
        operation: JsString,
        hash: JsString,
        guard: VoiceAsyncGuard,
    ) -> FlowFuture<()> {
        let voice = self.clone();
        Box::pin(async move {
            guard().await?;
            voice
                .finish(
                    &call,
                    "call.forwardVoice",
                    object([
                        ("operationId", operation.into()),
                        ("threadId", thread.into()),
                        ("prompt", prompt.clone().into()),
                        ("requestHash", hash.into()),
                    ]),
                )
                .await?;
            voice.prompts.lock().unwrap().push(prompt);
            Ok(())
        })
    }
    fn stop(&self, _: JsString, _: Option<JsString>) -> FlowFuture<()> {
        Box::pin(async { Ok(()) })
    }
    fn close(&self) -> FlowFuture<()> {
        self.closed.store(true, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}
struct Fixture {
    root: PathBuf,
    epoch: JsString,
    incoming: JsString,
    journal: PhoneJournal,
    native: PhoneNativeClient,
    flow: PhoneFlow,
    bridge: PhoneBridge,
    voice: Voice,
    requests: Arc<Mutex<Vec<IvyJson>>>,
    errors: Arc<Mutex<Vec<String>>>,
    server: JoinHandle<()>,
}
fn features() -> IvyJson {
    object([
        ("access", "trusted".into()),
        ("disableCodecUpgrade", false.into()),
        ("screening", "none".into()),
        ("command", IvyJson::Null),
        ("commandSequence", 0.0.into()),
        ("commands", IvyJson::Array(vec![])),
        ("commandOverflow", false.into()),
    ])
}
fn sip_call(id: JsString, direction: &str, state: &str, incoming: IvyJson) -> IvyJson {
    object([
        ("id", id.into()),
        ("direction", direction.into()),
        ("state", state.into()),
        ("sipCallId", "fixture-wire-call".into()),
        ("incoming", incoming),
        ("error", IvyJson::Null),
        ("features", features()),
    ])
}
fn media() -> IvyJson {
    object([
        ("closed", false.into()),
        ("failed", false.into()),
        ("sentPackets", 0.0.into()),
        ("receivedPackets", 0.0.into()),
        ("audio", IvyJson::Null),
        (
            "receive",
            object([
                ("queuedPackets", 0.0.into()),
                ("maximumPackets", 8.0.into()),
                ("reorderMs", 0.0.into()),
                ("duplicatePackets", 0.0.into()),
                ("latePackets", 0.0.into()),
                ("overflowPackets", 0.0.into()),
                ("foreignPackets", 0.0.into()),
                ("gatedPackets", 0.0.into()),
                ("missingPackets", 0.0.into()),
                ("maximumResidenceMs", 0.0.into()),
            ]),
        ),
    ])
}
impl Fixture {
    async fn new(route: &str, is_incoming: bool, gated: bool) -> Self {
        let root =
            std::env::temp_dir().join(format!("ivy-phone-reference-{}", uuid::Uuid::new_v4()));
        let journal = PhoneJournal::open(PhoneJournalOptions {
            data_root: root.clone(),
            owner: PhoneJournalOwner {
                host_id: "fixture".into(),
                service_node_id: "phone".into(),
            },
            limits: PhoneJournalLimits::default(),
            max_concurrent_calls: 1,
        })
        .await
        .unwrap();
        let epoch = uuid();
        journal.begin_epoch(epoch.clone()).await.unwrap();
        let incoming = uuid();
        let context = object([
            ("sipCallId", "fixture-wire-call".into()),
            ("fromUri", "sip:known@fixture".into()),
            ("peerAddress", "127.0.0.1".into()),
            ("peerPort", 5060.0.into()),
            ("transport", "udp".into()),
        ]);
        let policy = PhoneAdmission::new(object([
            (
                "recipients",
                IvyJson::Array(vec![object([
                    ("id", "recipient".into()),
                    (
                        "destination",
                        if is_incoming {
                            "sip:known@fixture".into()
                        } else {
                            "sip:recipient@127.0.0.1".into()
                        },
                    ),
                ])]),
            ),
            (
                "incoming",
                IvyJson::Array(vec![object([
                    ("peerAddress", "127.0.0.1".into()),
                    ("transport", "udp".into()),
                    ("fromUri", "sip:known@fixture".into()),
                ])]),
            ),
        ]))
        .unwrap();
        let call = if is_incoming {
            sip_call(incoming.clone(), "incoming", "ringing", context)
        } else {
            IvyJson::Null
        };
        let mut status = object([
            ("configured", true.into()),
            ("endpoint", "127.0.0.1:5060".into()),
            ("registration", IvyJson::Null),
            ("call", call),
            ("media", if is_incoming { media() } else { IvyJson::Null }),
        ]);
        let (writer, reader) = tokio::io::duplex(65536);
        let (mut reply, output) = tokio::io::duplex(65536);
        let native =
            PhoneNativeClient::start(epoch.clone(), writer, output, Arc::new(journal.clone()))
                .unwrap();
        let requests = Arc::new(Mutex::new(vec![]));
        let seen = requests.clone();
        let native_epoch = epoch.clone();
        let server = tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let request = parse(&line, 65536).unwrap();
                seen.lock().unwrap().push(request.clone());
                let method = text(&request, "method").to_utf8().unwrap();
                let params = request.get("params").unwrap();
                let id = params.get("callId").cloned().unwrap_or(IvyJson::Null);
                let result = match method.as_str() {
                    "status" => status.clone(),
                    "call.prepare" => {
                        let call = sip_call(
                            id.as_js_string().unwrap().clone(),
                            "outgoing",
                            "prepared",
                            IvyJson::Null,
                        );
                        status.insert("call", call.clone());
                        status.insert("media", media());
                        call
                    }
                    "call.claim" => status.get("call").unwrap().clone(),
                    "call.dial" | "call.answer" | "call.hangup" => {
                        let mut call = status.get("call").unwrap().clone();
                        call.insert(
                            "state",
                            if method == "call.hangup" {
                                "local_ended".into()
                            } else {
                                "connected".into()
                            },
                        );
                        status.insert("call", call.clone());
                        call
                    }
                    "call.features" => features(),
                    "call.windows.connect" => object([("callId", id), ("connected", true.into())]),
                    "call.realtime.prepare" => object([
                        ("callId", id),
                        ("generation", params.get("generation").unwrap().clone()),
                        ("sdp", "fixture offer".into()),
                    ]),
                    "call.realtime.answer" => object([("callId", id), ("connected", true.into())]),
                    "call.realtime.stop" => object([("callId", id), ("stopped", true.into())]),
                    "call.release" => {
                        status.insert("call", IvyJson::Null);
                        status.insert("media", IvyJson::Null);
                        object([("callId", id), ("released", true.into())])
                    }
                    _ => panic!("Unexpected native command {method}"),
                };
                let value = object([
                    ("version", 1.0.into()),
                    ("epoch", native_epoch.clone().into()),
                    ("requestId", request.get("requestId").unwrap().clone()),
                    ("ok", true.into()),
                    ("result", result),
                    ("error", IvyJson::Null),
                ]);
                reply
                    .write_all((canonical(&value, 65536).unwrap() + "\n").as_bytes())
                    .await
                    .unwrap();
            }
        });
        let voice = Voice {
            journal: journal.clone(),
            prompts: Arc::new(Mutex::new(vec![])),
            gate: gated.then(|| Arc::new(Semaphore::new(0))),
            preparing: Arc::new(AtomicBool::new(false)),
            closed: Arc::new(AtomicBool::new(false)),
        };
        let settings = object([
            ("incomingRoute", route.into()),
            ("outgoingRoute", route.into()),
            ("ringSeconds", 30.0.into()),
            (
                "codexVoice",
                if route == "voice" {
                    object([])
                } else {
                    IvyJson::Null
                },
            ),
            (
                "audio",
                object([
                    ("captureEndpointId", "fixture-capture".into()),
                    ("renderEndpointId", "fixture-render".into()),
                    ("sourceMode", "system_excluding_runtime".into()),
                    ("captureBufferMs", 20.0.into()),
                    ("renderLatencyMs", 20.0.into()),
                    ("queueMs", 40.0.into()),
                ]),
            ),
        ]);
        let errors = Arc::new(Mutex::new(vec![]));
        let sink = errors.clone();
        let calls = PhoneCallCommands::new(policy, journal.clone(), native.clone());
        let flow = PhoneFlow::new(
            calls,
            native.clone(),
            settings,
            Some(Arc::new(voice.clone())),
            None,
            Arc::new(move |_, code| sink.lock().unwrap().push(code.into())),
            Arc::new(|_| {}),
        )
        .unwrap();
        let bridge = PhoneBridge::new(flow.clone(), native.clone());
        bounded(native.observe(None)).await.unwrap();
        Self {
            root,
            epoch,
            incoming,
            journal,
            native,
            bridge,
            flow,
            voice,
            requests,
            errors,
            server,
        }
    }
    fn args(&self, operation: JsString, prompt: Option<&str>) -> IvyJson {
        let mut args = object([
            ("principalId", "trusted".into()),
            ("operationId", operation.into()),
            ("recipientId", "recipient".into()),
        ]);
        if let Some(prompt) = prompt {
            args.insert("voicePrompt", prompt.into());
        }
        args
    }
    async fn tool(&self, name: &str, args: IvyJson) -> IvyResult<IvyJson> {
        let handlers = self.bridge.handlers();
        handlers
            .get(&JsString::from(format!("phone.{name}")))
            .unwrap()(
            args,
            ivy_sdk::service::InvocationContext::detached(
                "trusted".into(),
                None,
                1,
                tokio_util::sync::CancellationToken::new(),
                IvyError::new("service_unavailable", "Service connection is stopping."),
            ),
        )
        .await
    }
    fn sent(&self, method: &str) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.get("method") == Some(&method.into()))
            .count()
    }
    async fn command_done(&self, id: JsString, method: &str) {
        bounded(async {
            loop {
                if self
                    .journal
                    .call_command(id.clone(), method.into(), 0)
                    .await
                    .unwrap()
                    .is_some_and(|v| v.get("phase") == Some(&"result".into()))
                {
                    break;
                }
                let errors = self.errors.lock().unwrap().clone();
                assert!(errors.is_empty(), "{errors:?}");
                tokio::task::yield_now().await;
            }
        })
        .await;
    }
    async fn close(self) {
        bounded(self.flow.close().wait()).await.unwrap();
        self.native.close("phone_unavailable");
        self.server.abort();
        let _ = self.server.await;
        self.journal.close().wait().await.unwrap();
        assert!(self.root.starts_with(std::env::temp_dir()));
        std::fs::remove_dir_all(self.root).unwrap();
    }
}

#[tokio::test]
async fn windows_route_connects_retries_without_redial_and_releases() {
    let fixture = Fixture::new("windows", false, false).await;
    let args = object([
        ("operationId", uuid().into()),
        ("recipientId", "recipient".into()),
    ]);
    let mut forged = args.clone();
    forged.insert("principalId", "untrusted-override".into());
    assert!(fixture.tool("request", forged).await.is_err());
    let cancelled = tokio_util::sync::CancellationToken::new();
    cancelled.cancel();
    assert_eq!(
        fixture
            .bridge
            .invoke("request", args.clone(), "trusted".into(), &cancelled)
            .await
            .unwrap_err()
            .code,
        "request_cancelled"
    );
    assert_eq!(fixture.sent("call.dial"), 0);
    let call = bounded(fixture.tool("request", args.clone()))
        .await
        .unwrap();
    assert_eq!(text(&call, "principalId"), JsString::from("trusted"));
    let id = text(&call, "callId");
    fixture
        .command_done(id.clone(), "call.windows.connect")
        .await;
    let retry = fixture.tool("request", args.clone()).await.unwrap();
    assert_eq!(text(&retry, "callId"), id);
    assert_eq!(fixture.sent("call.dial"), 1);
    let mut changed = args;
    changed.insert("destination", "sip:different@127.0.0.1".into());
    assert!(fixture.tool("request", changed).await.is_err());
    let query = object([("callId", id.clone().into())]);
    let view = fixture.tool("call", query.clone()).await.unwrap();
    assert_eq!(view.get("current"), Some(&true.into()));
    assert_eq!(
        view.get("observation").and_then(|v| v.get("state")),
        Some(&"connected".into())
    );
    let ended = fixture.tool("hangup", query).await.unwrap();
    assert_eq!(ended.get("current"), Some(&false.into()));
    equal(ended.get("call").unwrap(), &call);
    let history = fixture.tool("history", object([])).await.unwrap();
    assert_eq!(
        history
            .get("calls")
            .and_then(IvyJson::as_array)
            .unwrap()
            .len(),
        1
    );
    assert!(fixture.journal.current_calls().await.unwrap().is_empty());
    assert_eq!(fixture.sent("call.hangup"), 1);
    assert_eq!(fixture.sent("call.release"), 1);
    fixture.close().await;
}
#[tokio::test]
async fn voice_prepares_connects_and_forwards_once_to_the_existing_task() {
    let fixture = Fixture::new("voice", false, false).await;
    let call = fixture
        .tool(
            "voiceCall",
            object([
                ("operationId", uuid().into()),
                ("recipientId", "recipient".into()),
                ("initialPrompt", "Initial prompt".into()),
            ]),
        )
        .await
        .unwrap();
    let id = text(&call, "callId");
    fixture
        .command_done(id.clone(), "call.realtime.answer")
        .await;
    let forwarded = object([
        ("operationId", uuid().into()),
        ("recipientId", "recipient".into()),
        ("initialPrompt", "Follow-up prompt".into()),
    ]);
    let same = fixture.tool("voiceCall", forwarded.clone()).await.unwrap();
    assert_eq!(text(&same, "callId"), id);
    fixture.tool("voiceCall", forwarded.clone()).await.unwrap();
    assert_eq!(fixture.sent("call.dial"), 1);
    assert_eq!(
        fixture.voice.prompts.lock().unwrap().as_slice(),
        &[
            JsString::from("Initial prompt"),
            JsString::from("Follow-up prompt")
        ]
    );
    let mut changed = forwarded;
    changed.insert("initialPrompt", "Changed request".into());
    assert_eq!(
        fixture.tool("voiceCall", changed).await.unwrap_err().code,
        "mutation_conflict"
    );
    fixture.close().await;
}
#[tokio::test]
async fn close_revokes_sip_before_a_blocked_voice_preparation_drains() {
    let fixture = Fixture::new("voice", true, true).await;
    let accepted = fixture
        .flow
        .accept("trusted".into(), uuid(), fixture.incoming.clone());
    drop(accepted);
    until(|| fixture.voice.preparing.load(Ordering::SeqCst)).await;
    // This admitted request can wait for the first Voice preparation. Closing
    // must revoke SIP before it drains either of these retained operations.
    let forwarded = fixture
        .flow
        .request(fixture.args(uuid(), Some("Pending follow-up")));
    let close = fixture.flow.close();
    bounded(async {
        while !fixture.journal.current_calls().await.unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert_eq!(fixture.sent("call.hangup"), 1);
    assert!(!fixture.voice.closed.load(Ordering::SeqCst));
    assert_eq!(fixture.sent("call.realtime.answer"), 0);
    fixture.voice.gate.as_ref().unwrap().add_permits(1);
    bounded(close.wait()).await.unwrap();
    assert!(bounded(forwarded.wait()).await.is_err());
    assert!(fixture.voice.closed.load(Ordering::SeqCst));
    assert_eq!(fixture.sent("call.realtime.answer"), 0);
    fixture.close().await;
}
#[tokio::test]
async fn speech_replay_uses_its_receipt_without_reading_a_credential_or_contacting_a_provider() {
    let fixture = Fixture::new("windows", false, false).await;
    let call = fixture
        .flow
        .request(fixture.args(uuid(), None))
        .wait()
        .await
        .unwrap();
    let id = text(&call, "callId");
    fixture
        .command_done(id.clone(), "call.windows.connect")
        .await;
    let settings = object([
        (
            "credentialsPath",
            fixture
                .root
                .join("missing-protected.json")
                .to_str()
                .unwrap()
                .into(),
        ),
        ("endpoint", "https://speech.invalid/v1/speech".into()),
        ("model", "fixture".into()),
        ("voice", "fixture".into()),
    ]);
    let hash = hash_json(&object([
        ("text", "Announcement".into()),
        ("settings", settings.clone()),
    ]))
    .unwrap();
    let intent = object([
        ("epoch", fixture.epoch.clone().into()),
        ("callId", id.into()),
        ("operationId", uuid().into()),
        ("method", "call.screening.synthesize".into()),
        ("requestHash", hash.into()),
    ]);
    let expected = object([
        (
            "wavPath",
            fixture
                .root
                .join("already-produced.wav")
                .to_str()
                .unwrap()
                .into(),
        ),
        ("sha256", format!("sha256:{}", "0".repeat(64)).into()),
    ]);
    fixture.journal.submit(intent.clone()).await.unwrap();
    fixture
        .journal
        .finish(
            intent,
            object([
                ("version", 1.0.into()),
                ("epoch", fixture.epoch.clone().into()),
                ("requestId", 1.0.into()),
                ("ok", true.into()),
                ("result", expected.clone()),
                ("error", IvyJson::Null),
            ]),
        )
        .await
        .unwrap();
    equal(
        &synthesize_announcement(&fixture.journal, &call, "Announcement".into(), &settings)
            .await
            .unwrap(),
        &expected,
    );
    assert_eq!(
        synthesize_announcement(&fixture.journal, &call, "Changed".into(), &settings)
            .await
            .unwrap_err()
            .code,
        "mutation_conflict"
    );
    let mut insecure = settings;
    insecure.insert("endpoint", "http://speech.invalid".into());
    assert_eq!(
        synthesize_announcement(&fixture.journal, &call, "Announcement".into(), &insecure)
            .await
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    fixture.close().await;
}

#[tokio::test]
async fn journal_requires_typed_cache_fields_and_integer_counters() {
    let fixture = Fixture::new("windows", false, false).await;
    let fingerprint = digest("reference-runtime");
    let cache = object([
        ("fingerprint", fingerprint.clone().into()),
        ("threadId", uuid().into()),
        ("creationOperationId", IvyJson::Null),
    ]);
    fixture
        .journal
        .retain_task("prepared-phone-voice", cache.clone())
        .await
        .unwrap();
    let mut invalid = cache.clone();
    invalid.insert(
        "fingerprint",
        IvyJson::Array(vec![fingerprint.clone().into()]),
    );
    assert!(
        fixture
            .journal
            .retain_task("prepared-phone-voice", invalid)
            .await
            .is_err()
    );
    let mut invalid = cache.clone();
    invalid.insert("preparationTurnId", false.into());
    assert!(
        fixture
            .journal
            .retain_task("prepared-phone-voice", invalid)
            .await
            .is_err()
    );
    equal(
        &fixture
            .journal
            .task_cache("prepared-phone-voice", &fingerprint)
            .await
            .unwrap()
            .unwrap(),
        &cache,
    );

    bounded(fixture.flow.close().wait()).await.unwrap();
    fixture.native.close("phone_unavailable");
    fixture.journal.close().wait().await.unwrap();
    // Corrupt the owned scratch file between owners; the live journal has an
    // exclusive SQLite lease. epochOperations is retained across owner restarts.
    for value in ["", "NaN", "0x10", "-1", "1.5", "0"] {
        {
            let database =
                ivy_store::rusqlite::Connection::open(fixture.root.join("phone-commands.sqlite"))
                    .unwrap();
            database.execute("INSERT INTO meta(key,value) VALUES('epochOperations',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [value]).unwrap();
        }
        let reopened = PhoneJournal::open(PhoneJournalOptions {
            data_root: fixture.root.clone(),
            owner: PhoneJournalOwner {
                host_id: "fixture".into(),
                service_node_id: "phone".into(),
            },
            limits: PhoneJournalLimits::default(),
            max_concurrent_calls: 1,
        })
        .await
        .unwrap();
        if value == "0" {
            assert_eq!(reopened.epoch_operations().await.unwrap(), 0.0);
        } else {
            assert_eq!(
                reopened.epoch_operations().await.unwrap_err().code,
                "phone_storage_invalid"
            );
        }
        reopened.close().wait().await.unwrap();
    }
    fixture.close().await;
}
