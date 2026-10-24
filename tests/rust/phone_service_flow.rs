use super::*;
use crate::{
    admission::PhoneAdmission,
    journal::{PhoneJournal, PhoneJournalLimits, PhoneJournalOptions, PhoneJournalOwner},
    service_test_support::{equal, fixture, observed, timed, until},
};
use ivy_contracts::json::{canonical, parse};
use std::{
    path::PathBuf,
    sync::{
        OnceLock,
        atomic::{AtomicU32, Ordering},
    },
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream},
    task::JoinHandle,
};
fn id(n: u32) -> JsString {
    format!("71000000-0000-4000-8000-{n:012}").into()
}
fn cases() -> &'static IvyJson {
    static CASES: OnceLock<IvyJson> = OnceLock::new();
    CASES.get_or_init(|| fixture("flow-controls"))
}
fn object<const N: usize>(items: [(&str, IvyJson); N]) -> IvyJson {
    IvyJson::object(items)
}
fn normalize(value: &IvyJson, key: &JsString) -> IvyJson {
    match value {
        IvyJson::String(s)
            if key == &JsString::from("createdAt") || key == &JsString::from("updatedAt") =>
        {
            let time = s.to_utf8().unwrap();
            assert!(chrono::DateTime::parse_from_rfc3339(&time).is_ok());
            "$time".into()
        }
        IvyJson::Array(items) => {
            IvyJson::Array(items.iter().map(|v| normalize(v, &"".into())).collect())
        }
        IvyJson::Object(items) => {
            IvyJson::object(items.iter().map(|(k, v)| (k.clone(), normalize(v, k))))
        }
        _ => value.clone(),
    }
}
struct Voice {
    journal: PhoneJournal,
    events: Arc<Mutex<Vec<String>>>,
    effective: Mutex<JsString>,
    serial: AtomicU32,
    fail_prepare: bool,
    models_gate: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
}
impl Voice {
    async fn finish(&self, method: &str, mut extra: IvyJson) -> IvyResult<()> {
        if extra.get("operationId").is_none() {
            extra.insert(
                "operationId",
                id(self.serial.fetch_add(1, Ordering::SeqCst)).into(),
            );
        }
        extra.insert("epoch", id(1).into());
        extra.insert("callId", id(3).into());
        extra.insert("method", method.into());
        if extra.get("requestHash").is_none() {
            extra.insert("requestHash", format!("sha256:{}", "0".repeat(64)).into());
        }
        self.journal.submit(extra.clone()).await?;
        match method {
            "call.bindVoice" => {
                self.journal.finish_voice_binding(extra, None).await?;
            }
            "call.selectVoice" => {
                self.journal
                    .finish_voice_selection(extra, "sent".into())
                    .await?;
            }
            _ => {
                self.journal
                    .finish_voice_prompt(extra, "sent".into())
                    .await?;
            }
        }
        Ok(())
    }
    fn event(&self, text: String) {
        self.events.lock().unwrap().push(text);
    }
}
impl PhoneFlowVoice for Arc<Voice> {
    fn prompt(
        &self,
        _call: IvyJson,
        thread_id: JsString,
        prompt: JsString,
        operation_id: JsString,
        request_hash: JsString,
        guard: VoiceAsyncGuard,
    ) -> FlowFuture<()> {
        let voice = self.clone();
        Box::pin(async move {
            guard().await?;
            voice.event(format!("prompt:{}", prompt.to_utf8()?));
            voice
                .finish(
                    "call.forwardVoice",
                    object([
                        ("threadId", thread_id.into()),
                        ("prompt", prompt.into()),
                        ("operationId", operation_id.into()),
                        ("requestHash", request_hash.into()),
                    ]),
                )
                .await
        })
    }
    fn models(&self) -> FlowFuture<IvyJson> {
        let voice = self.clone();
        let gate = self.models_gate.lock().unwrap().clone();
        Box::pin(async move {
            if let Some(gate) = gate {
                voice.event("models_wait".into());
                gate.acquire().await.unwrap().forget();
            }
            Ok(cases().get("models").unwrap().clone())
        })
    }
    fn speaker_voices(&self) -> FlowFuture<IvyJson> {
        Box::pin(async { Ok(cases().get("speakers").unwrap().clone()) })
    }
    fn speaker_voice(&self, _: &JsString) -> Option<JsString> {
        Some(self.effective.lock().unwrap().clone())
    }
    fn failed(&self, _: &JsString) -> bool {
        false
    }
    fn inputs(&self, _: &JsString) -> IvyResult<Vec<IvyJson>> {
        Ok(vec![])
    }
    fn answer(&self, _: JsString, _: IvyJson, _: IvyJson, _: JsString) -> FlowFuture<()> {
        self.event("answer".into());
        Box::pin(async { Ok(()) })
    }
    fn prewarm(&self, _: JsString, _: VoiceSelection) -> FlowFuture<()> {
        self.event("prewarm".into());
        Box::pin(async { Ok(()) })
    }
    fn prepare(&self, _: IvyJson, generation: u32, _: VoiceSelection) -> FlowFuture<JsString> {
        let voice = self.clone();
        Box::pin(async move {
            voice.event(format!("prepare:{generation}"));
            if voice.fail_prepare {
                return Err(IvyError::new("phone_voice_unavailable", "Fixture failure"));
            }
            let thread = id(10 + generation);
            voice
                .finish(
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
        _: IvyJson,
        generation: u32,
        thread: JsString,
        prompt: JsString,
        _: JsString,
        guard: VoiceAsyncGuard,
    ) -> FlowFuture<JsString> {
        let voice = self.clone();
        Box::pin(async move {
            guard().await?;
            voice.event(format!("start:{generation}:{}", prompt.to_utf8()?));
            let selected = voice
                .journal
                .selected_speaker_voice(id(3))
                .await?
                .unwrap_or("cove".into());
            *voice.effective.lock().unwrap() = selected;
            voice
                .finish(
                    "call.promptVoice",
                    object([
                        ("voiceGeneration", f64::from(generation).into()),
                        ("threadId", thread.into()),
                        ("prompt", prompt.into()),
                    ]),
                )
                .await?;
            Ok("fixture answer".into())
        })
    }
    fn connected(&self, _: JsString, guard: VoiceAsyncGuard) -> FlowFuture<()> {
        let voice = self.clone();
        Box::pin(async move {
            guard().await?;
            voice.event("connected".into());
            Ok(())
        })
    }
    fn select(
        &self,
        _: IvyJson,
        thread: JsString,
        command: VoiceSelectionCommand,
        selection: VoiceSelection,
        guard: VoiceAsyncGuard,
    ) -> FlowFuture<()> {
        let voice = self.clone();
        Box::pin(async move {
            guard().await?;
            voice.event(format!(
                "select:{}:{}",
                selection.model.to_utf8()?,
                selection.reasoning_effort.to_utf8()?
            ));
            let mut args = selection.to_wire();
            args.insert("threadId", thread.into());
            args.insert("prompt", "Voice task model selection".into());
            if let VoiceSelectionCommand::Sequence(sequence) = &command {
                args.insert("commandSequence", f64::from(*sequence).into());
            }
            if let VoiceSelectionCommand::Update { voice_id, .. } = &command {
                args.insert("voiceId", voice_id.clone().into());
            }
            let hash = hash_json(&args)?;
            args.insert("requestHash", hash.into());
            match command {
                VoiceSelectionCommand::Operation(operation)
                | VoiceSelectionCommand::Update { operation_id: operation, .. } => { args.insert("operationId", operation.into()); }
                _ => {}
            }
            voice.finish("call.selectVoice", args).await
        })
    }
    fn stop(&self, _: JsString, keep: Option<JsString>) -> FlowFuture<()> {
        self.event(format!(
            "stop:{}",
            keep.map(|s| s.to_utf8().unwrap()).unwrap_or_default()
        ));
        Box::pin(async { Ok(()) })
    }
    fn close(&self) -> FlowFuture<()> {
        self.event("close".into());
        Box::pin(async { Ok(()) })
    }
}
struct NativeState {
    status: IvyJson,
    errors: u32,
}
struct Fixture {
    root: PathBuf,
    journal: PhoneJournal,
    native: PhoneNativeClient,
    flow: PhoneFlow,
    voice: Arc<Voice>,
    status: Arc<Mutex<NativeState>>,
    requests: Arc<Mutex<Vec<IvyJson>>>,
    errors: Arc<Mutex<Vec<String>>>,
    reader: JoinHandle<()>,
    output: Arc<tokio::sync::Mutex<DuplexStream>>,
}
impl Fixture {
    async fn new(row: &IvyJson) -> Self {
        let root =
            std::env::temp_dir().join(format!("ivy-phone-controls-{}", uuid::Uuid::new_v4()));
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
        journal.begin_epoch(id(1)).await.unwrap();
        let admission = PhoneAdmission::new(cases().get("policy").unwrap().clone()).unwrap();
        let incoming = object([
            ("sipCallId", "observed-wire-call".into()),
            ("fromUri", "sip:known@fixture".into()),
            ("peerAddress", "127.0.0.1".into()),
            ("peerPort", 5060.0.into()),
            ("transport", "udp".into()),
        ]);
        let mut observation = object([
            ("id", id(3).into()),
            ("direction", "incoming".into()),
            ("state", "ringing".into()),
            ("sipCallId", "observed-wire-call".into()),
            ("error", IvyJson::Null),
            ("incoming", incoming),
        ]);
        let call = admission
            .incoming(id(1).into(), id(2).into(), "main".into(), &observation)
            .unwrap();
        let mut call = call;
        call.insert("route", "voice".into());
        journal.admit_call(call).await.unwrap();
        observation.insert(
            "state",
            if row.get("ended") == Some(&true.into()) {
                "local_ended".into()
            } else {
                "connected".into()
            },
        );
        observation.insert(
            "features",
            object([
                ("access", "trusted".into()),
                ("disableCodecUpgrade", false.into()),
                ("screening", "none".into()),
                ("command", IvyJson::Null),
                ("commandSequence", 0.0.into()),
                ("commands", IvyJson::Array(vec![])),
                ("commandOverflow", false.into()),
            ]),
        );
        // The media observation is copied from the existing pure incoming fixture.
        let media=parse(&format!("{{\"closed\":false,\"failed\":false,\"sentPackets\":0,\"receivedPackets\":0,\"receive\":{{\"queuedPackets\":0,\"maximumPackets\":8,\"reorderMs\":20,\"duplicatePackets\":0,\"latePackets\":0,\"overflowPackets\":0,\"foreignPackets\":0,\"gatedPackets\":0,\"missingPackets\":0,\"maximumResidenceMs\":0}},\"audio\":{{\"callId\":\"{}\",\"state\":\"unprepared\",\"route\":null}}}}",id(3).to_utf8().unwrap()),65536).unwrap();
        let mut status = cases().get("idle").unwrap().clone();
        status.insert("call", observation);
        status.insert("media", media);
        let status = Arc::new(Mutex::new(NativeState { status, errors: 0 }));
        let (input, peer) = tokio::io::duplex(65536);
        let (output, read) = tokio::io::duplex(65536);
        let output = Arc::new(tokio::sync::Mutex::new(output));
        let native =
            PhoneNativeClient::start(id(1), input, read, Arc::new(journal.clone())).unwrap();
        let requests = Arc::new(Mutex::new(vec![]));
        let captured = requests.clone();
        let native_state = status.clone();
        let sender = output.clone();
        let reader = tokio::spawn(async move {
            let mut lines = BufReader::new(peer).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let request = parse(&line, 65536).unwrap();
                let method = text(&request, "method").unwrap().to_utf8().unwrap();
                let params = field(&request, "params").unwrap();
                captured.lock().unwrap().push(object([
                    ("method", method.clone().into()),
                    ("params", params.clone()),
                ]));
                let result = {
                    let mut state = native_state.lock().unwrap();
                    match method.as_str() {
                        "status" => {
                            let mut status = state.status.clone();
                            if state.errors > 0 {
                                state.errors -= 1;
                                status.insert("configured", "invalid".into());
                            }
                            status
                        }
                        "call.claim" => state.status.get("call").unwrap().clone(),
                        "call.hangup" => {
                            let mut call = state.status.get("call").unwrap().clone();
                            call.insert("state", "local_ended".into());
                            state.status.insert("call", call.clone());
                            call
                        }
                        "call.release" => {
                            object([("callId", id(3).into()), ("released", true.into())])
                        }
                        "call.realtime.prepare" => object([
                            ("callId", id(3).into()),
                            ("generation", field(params, "generation").unwrap().clone()),
                            ("sdp", "fixture offer".into()),
                        ]),
                        "call.realtime.stop" => {
                            object([("callId", id(3).into()), ("stopped", true.into())])
                        }
                        "call.realtime.answer" => {
                            object([("callId", id(3).into()), ("connected", true.into())])
                        }
                        "call.command.feedback" => object([
                            ("callId", id(3).into()),
                            (
                                "commandSequence",
                                field(params, "commandSequence").unwrap().clone(),
                            ),
                            ("success", field(params, "success").unwrap().clone()),
                        ]),
                        _ => panic!("Unexpected native method {method}"),
                    }
                };
                let reply = object([
                    ("version", 1.0.into()),
                    ("epoch", id(1).into()),
                    ("requestId", field(&request, "requestId").unwrap().clone()),
                    ("ok", true.into()),
                    ("result", result),
                    ("error", IvyJson::Null),
                ]);
                sender
                    .lock()
                    .await
                    .write_all((canonical(&reply, 65536).unwrap() + "\n").as_bytes())
                    .await
                    .unwrap();
            }
        });
        let original_calls =
            PhoneCallCommands::new(admission.clone(), journal.clone(), native.clone());
        let calls = if row.get("changed") == Some(&true.into()) {
            PhoneCallCommands::new(
                PhoneAdmission::new(object([
                    ("recipients", IvyJson::Array(vec![])),
                    ("incoming", IvyJson::Array(vec![])),
                ]))
                .unwrap(),
                journal.clone(),
                native.clone(),
            )
        } else {
            original_calls.clone()
        };
        let errors = Arc::new(Mutex::new(vec![]));
        let error_sink = errors.clone();
        let voice = Arc::new(Voice {
            journal: journal.clone(),
            events: Arc::new(Mutex::new(vec![])),
            effective: Mutex::new("cove".into()),
            serial: AtomicU32::new(100),
            fail_prepare: row.get("failPrepare") == Some(&true.into()),
            models_gate: Mutex::new(None),
        });
        let flow = PhoneFlow::new(
            calls,
            native.clone(),
            cases().get("settings").unwrap().clone(),
            Some(Arc::new(voice.clone())),
            None,
            Arc::new(move |_, error| error_sink.lock().unwrap().push(error.into())),
            Arc::new(|_| {}),
        )
        .unwrap();
        if row.get("unprepared") != Some(&true.into()) {
            original_calls
                .claim("main".into(), id(3))
                .wait()
                .await
                .unwrap();
            voice
                .finish(
                    "call.bindVoice",
                    object([("voiceGeneration", 0.0.into()), ("threadId", id(10).into())]),
                )
                .await
                .unwrap();
        }
        status.lock().unwrap().errors = row
            .get("statusErrors")
            .and_then(IvyJson::as_f64)
            .unwrap_or(0.0) as u32;
        Self {
            root,
            journal,
            native,
            flow,
            voice,
            status,
            requests,
            errors,
            reader,
            output,
        }
    }
    async fn step(&self, step: &IvyJson) -> IvyResult<IvyJson> {
        let action = field(step, "action")?.as_str().unwrap();
        let operation = || id(field(step, "operation").unwrap().as_f64().unwrap() as u32);
        match action.as_str() {
            "config" => return self.flow.configuration().await,
            "catalog" => return self.flow.voice_model_catalog().await,
            "remember" => {
                self.journal
                    .remember_voice_reasoning(text(step, "effort")?)
                    .await?
            }
            "prewarm" => self.flow.prewarm_voice("main".into()).await?,
            "speaker-state" => {
                return self
                    .flow
                    .speaker_voice_state(id(3), cases().get("speakers").unwrap())
                    .await;
            }
            "speaker" => {
                return self
                    .flow
                    .select_speaker_voice(
                        "other".into(),
                        id(3),
                        operation(),
                        text(step, "voiceId")?,
                    )
                    .wait()
                    .await;
            }
            "select" => {
                return self
                    .flow
                    .control_voice(
                        "other".into(),
                        id(3),
                        operation(),
                        PhoneVoiceControl::Select(VoiceSelection {
                            model: text(step, "model")?,
                            reasoning_effort: text(step, "effort")?,
                        }),
                    )
                    .wait()
                    .await;
            }
            "restart" => {
                return self
                    .flow
                    .control_voice(
                        "other".into(),
                        id(3),
                        operation(),
                        PhoneVoiceControl::Restart,
                    )
                    .wait()
                    .await;
            }
            "selection" => {
                return Ok(object([
                    (
                        "selection",
                        self.flow.voice_selection(&id(3)).await?.to_wire(),
                    ),
                    (
                        "remembered",
                        self.journal
                            .remembered_voice_reasoning()
                            .await?
                            .map(Into::into)
                            .unwrap_or(IvyJson::Null),
                    ),
                ]));
            }
            "inputs" => {
                return self
                    .flow
                    .voice_inputs("other".into(), id(3))
                    .await
                    .map(IvyJson::Array);
            }
            "prepare-realtime" => {
                self.flow
                    .calls()
                    .prepare_realtime(
                        "main".into(),
                        id(3),
                        field(step, "generation")?.as_f64().unwrap() as u32,
                        Some(80),
                        None,
                    )
                    .wait()
                    .await?;
            }
            "hangup" => return self.flow.hangup("other".into(), id(3)).wait().await,
            "current" => {
                return self.journal.current_calls().await.map(|v| {
                    IvyJson::Array(
                        v.iter()
                            .map(|v| field(v, "callId").unwrap().clone())
                            .collect(),
                    )
                });
            }
            "operation" => {
                return self
                    .journal
                    .operation(operation())
                    .await
                    .map(|v| v.unwrap_or(IvyJson::Null));
            }
            "commands" => {
                let mut state = self.status.lock().unwrap();
                let mut call = field(&state.status, "call")?.clone();
                let mut features = field(&call, "features")?.clone();
                features.insert("commands", field(step, "commands")?.clone());
                call.insert("features", features);
                state.status.insert("call", call);
            }
            "observe" => {
                let status = self.status.lock().unwrap().status.clone();
                self.flow.observe(Some(status)).await?;
            }
            "settle" => until(|| self.flow.0.state.lock().unwrap().work.is_empty()).await,
            _ => panic!("Unknown flow action"),
        }
        Ok(IvyJson::Null)
    }
    async fn close(self) {
        let _ = timed(self.flow.close().wait()).await;
        self.native.close("phone_unavailable");
        drop(self.output);
        self.reader.abort();
        self.journal.close().wait().await.unwrap();
        assert!(self.root.starts_with(std::env::temp_dir()));
        std::fs::remove_dir_all(self.root).unwrap();
    }
}
#[tokio::test]
async fn original_voice_controls_restart_feedback_and_cleanup() {
    for row in cases().get("cases").unwrap().as_array().unwrap() {
        let fixture = Fixture::new(row).await;
        let mut observations = Vec::new();
        for step in row.get("steps").unwrap().as_array().unwrap() {
            let result = timed(fixture.step(step)).await;
            observations.push(object([
                ("action", field(step, "action").unwrap().clone()),
                ("result", observed(result)),
            ]));
        }
        // Detached prewarm reads the same catalog/journal; wait for its bounded
        // completion before comparing, without changing public flow timing.
        if fixture.journal.current_calls().await.unwrap().is_empty()
            && row.get("unprepared") != Some(&true.into())
        {
            until(|| {
                fixture
                    .voice
                    .events
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|v| v == "prewarm")
            })
            .await;
        }
        let actual = normalize(
            &object([
                ("observations", IvyJson::Array(observations)),
                (
                    "requests",
                    IvyJson::Array(fixture.requests.lock().unwrap().clone()),
                ),
                (
                    "events",
                    IvyJson::Array(
                        fixture
                            .voice
                            .events
                            .lock()
                            .unwrap()
                            .iter()
                            .cloned()
                            .map(Into::into)
                            .collect(),
                    ),
                ),
                (
                    "errors",
                    IvyJson::Array(
                        fixture
                            .errors
                            .lock()
                            .unwrap()
                            .iter()
                            .cloned()
                            .map(Into::into)
                            .collect(),
                    ),
                ),
            ]),
            &"".into(),
        );
        equal(
            &actual,
            row.get("expected").unwrap(),
            &row.get("name").unwrap().as_str().unwrap(),
        );
        fixture.close().await;
    }
}
#[tokio::test]
async fn partial_voice_update_replays_its_original_defaults_after_a_newer_selection() {
    let fixture = Fixture::new(&object([])).await;
    let bridge = crate::service_bridge::PhoneBridge::new(fixture.flow.clone(), fixture.native.clone());
    let signal = tokio_util::sync::CancellationToken::new();
    let original = object([("operationId", id(30).into()), ("callId", id(3).into()),
        ("reasoningEffort", "ultra".into()), ("voiceId", "ember".into())]);
    let first = bridge.invoke("update", original.clone(), "other".into(), &signal).await.unwrap();
    assert_eq!(first.required("intent").unwrap().get("model"), Some(&"gpt-6.1-sol".into()));
    assert_eq!(fixture.journal.selected_speaker_voice(id(3)).await.unwrap(), Some("ember".into()));
    bridge.invoke("update", object([("operationId", id(31).into()), ("callId", id(3).into()),
        ("model", "gpt-6-sol".into())]), "other".into(), &signal).await.unwrap();
    let events = fixture.voice.events.lock().unwrap().clone();
    let replay = bridge.invoke("update", original.clone(), "other".into(), &signal).await.unwrap();
    equal(&replay, &first, "original partial update");
    assert_eq!(*fixture.voice.events.lock().unwrap(), events);
    let selected = fixture.flow.voice_selection(&id(3)).await.unwrap();
    assert_eq!(selected.model, JsString::from("gpt-6-sol"));
    assert_eq!(selected.reasoning_effort, JsString::from("ultra"));
    assert!(events.iter().all(|event| !event.starts_with("start:") && !event.starts_with("prepare:")));
    let mut conflict = original; conflict.insert("voiceId", "cove".into());
    assert_eq!(bridge.invoke("update", conflict, "other".into(), &signal).await.unwrap_err().code, "mutation_conflict");
    fixture.close().await;
}

#[tokio::test]
async fn control_observer_loss_retains_selection_and_close_revokes_before_voice_drains() {
    let fixture = Fixture::new(&object([])).await;
    let gate = fixture.flow.0.commands.lock().await;
    let pending = fixture
        .flow
        .select_speaker_voice("other".into(), id(3), id(20), "ember".into());
    drop(pending);
    drop(gate);
    timed(async {
        loop {
            if fixture
                .journal
                .operation(id(20))
                .await
                .unwrap()
                .is_some_and(|v| v.get("phase") == Some(&"result".into()))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    let count = fixture.requests.lock().unwrap().len();
    assert!(
        fixture
            .flow
            .select_speaker_voice("other".into(), id(3), id(20), "ember".into())
            .wait()
            .await
            .is_ok()
    );
    assert_eq!(fixture.requests.lock().unwrap().len(), count);
    let models = Arc::new(tokio::sync::Semaphore::new(0));
    *fixture.voice.models_gate.lock().unwrap() = Some(models.clone());
    let selection = fixture.flow.control_voice(
        "other".into(),
        id(3),
        id(21),
        PhoneVoiceControl::Select(VoiceSelection::default()),
    );
    until(|| {
        fixture
            .voice
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|v| v == "models_wait")
    })
    .await;
    let close = fixture.flow.close();
    drop(close);
    timed(async {
        loop {
            if fixture.journal.current_calls().await.unwrap().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        !fixture
            .voice
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|v| v == "close")
    );
    models.add_permits(1);
    assert_eq!(
        selection.wait().await.unwrap_err().code,
        "phone_call_cancelled"
    );
    assert!(
        fixture
            .flow
            .control_voice("other".into(), id(3), id(22), PhoneVoiceControl::Restart)
            .wait()
            .await
            .is_err()
    );
    fixture.flow.close().wait().await.unwrap();
    let events = fixture.voice.events.lock().unwrap().clone();
    assert_eq!(events.iter().filter(|v| v.as_str() == "close").count(), 1);
    fixture.close().await;
}
