//! Call-scoped Voice controls, observation and cleanup. The original journal
//! owns SIP authority; Voice owns only its task/session on the same connection.
use crate::{
    call_commands::PhoneCallCommands,
    call_logs::PhoneCallLogs,
    native_client::{PhoneNativeClient, validate_phone},
    voice_rpc::{PhoneCodexVoice, VoiceAsyncGuard, VoiceSelectionCommand},
    voice_selection::{VoiceSelection, selected_keypad_model},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, Outcome, hash::hash_json};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::watch;
mod connect;

pub type FlowFuture<T> = Pin<Box<dyn Future<Output = IvyResult<T>> + Send>>;

/// The existing Voice-port boundary, implemented by the configured production
/// owner. It also permits deterministic tests without any account or inference.
pub trait PhoneFlowVoice: Send + Sync {
    fn models(&self) -> FlowFuture<IvyJson>;
    fn speaker_voices(&self) -> FlowFuture<IvyJson>;
    fn speaker_voice(&self, call_id: &JsString) -> Option<JsString>;
    fn failed(&self, call_id: &JsString) -> bool;
    fn inputs(&self, call_id: &JsString) -> IvyResult<Vec<IvyJson>>;
    fn answer(
        &self,
        call_id: JsString,
        id: IvyJson,
        reply: IvyJson,
        input_token: JsString,
    ) -> FlowFuture<()>;
    fn work(&self, _call_id: JsString) -> FlowFuture<Option<IvyJson>> {
        Box::pin(async { Ok(None) })
    }
    fn has_work(&self) -> bool {
        false
    }
    fn disconnected(&self, _call_id: JsString, _ended: Option<i64>) -> FlowFuture<()> {
        Box::pin(async { Ok(()) })
    }
    fn detach(&self, call_id: JsString, _ended: Option<i64>) -> FlowFuture<()> {
        self.stop(call_id, None)
    }
    fn prewarm(&self, principal: JsString, selection: VoiceSelection) -> FlowFuture<()>;
    fn prepare(
        &self,
        call: IvyJson,
        generation: u32,
        selection: VoiceSelection,
    ) -> FlowFuture<JsString>;
    fn start(
        &self,
        call: IvyJson,
        generation: u32,
        thread_id: JsString,
        prompt: JsString,
        sdp: JsString,
        guard: VoiceAsyncGuard,
    ) -> FlowFuture<JsString>;
    fn connected(&self, call_id: JsString, guard: VoiceAsyncGuard) -> FlowFuture<()>;
    fn select(
        &self,
        call: IvyJson,
        thread_id: JsString,
        command: VoiceSelectionCommand,
        selection: VoiceSelection,
        guard: VoiceAsyncGuard,
    ) -> FlowFuture<()>;
    fn prompt(
        &self,
        call: IvyJson,
        thread_id: JsString,
        prompt: JsString,
        operation_id: JsString,
        request_hash: JsString,
        guard: VoiceAsyncGuard,
    ) -> FlowFuture<()>;
    fn stop(&self, call_id: JsString, keep_thread: Option<JsString>) -> FlowFuture<()>;
    fn close(&self) -> FlowFuture<()>;
}
impl PhoneFlowVoice for PhoneCodexVoice {
    fn prompt(
        &self,
        call: IvyJson,
        thread_id: JsString,
        prompt: JsString,
        operation_id: JsString,
        request_hash: JsString,
        guard: VoiceAsyncGuard,
    ) -> FlowFuture<()> {
        let work = PhoneCodexVoice::prompt(
            self,
            call,
            thread_id,
            prompt,
            operation_id,
            request_hash,
            guard,
        );
        Box::pin(async move { work.wait().await })
    }
    fn models(&self) -> FlowFuture<IvyJson> {
        let owner = self.clone();
        Box::pin(async move { PhoneCodexVoice::models(&owner).await })
    }
    fn speaker_voices(&self) -> FlowFuture<IvyJson> {
        let owner = self.clone();
        Box::pin(async move { PhoneCodexVoice::speaker_voices(&owner).await })
    }
    fn speaker_voice(&self, call_id: &JsString) -> Option<JsString> {
        PhoneCodexVoice::speaker_voice(self, call_id)
    }
    fn failed(&self, call_id: &JsString) -> bool {
        PhoneCodexVoice::failed(self, call_id)
    }
    fn inputs(&self, call_id: &JsString) -> IvyResult<Vec<IvyJson>> {
        PhoneCodexVoice::inputs(self, call_id)
    }
    fn answer(
        &self,
        call_id: JsString,
        id: IvyJson,
        reply: IvyJson,
        input_token: JsString,
    ) -> FlowFuture<()> {
        let work = PhoneCodexVoice::answer(self, call_id, id, reply, input_token);
        Box::pin(async move { work?.wait().await })
    }
    fn work(&self, call_id: JsString) -> FlowFuture<Option<IvyJson>> {
        let owner = self.clone();
        Box::pin(async move { PhoneCodexVoice::work(&owner, &call_id).await })
    }
    fn has_work(&self) -> bool {
        PhoneCodexVoice::has_work(self)
    }
    fn disconnected(&self, call_id: JsString, ended: Option<i64>) -> FlowFuture<()> {
        let owner = self.clone();
        Box::pin(async move { PhoneCodexVoice::disconnected(&owner, &call_id, ended).await })
    }
    fn detach(&self, call_id: JsString, ended: Option<i64>) -> FlowFuture<()> {
        let work = PhoneCodexVoice::detach(self, call_id, ended);
        Box::pin(async move { work.wait().await })
    }
    fn prewarm(&self, principal: JsString, selection: VoiceSelection) -> FlowFuture<()> {
        let work = PhoneCodexVoice::prewarm(self, principal, selection);
        Box::pin(async move { work.wait().await })
    }
    fn prepare(
        &self,
        call: IvyJson,
        generation: u32,
        selection: VoiceSelection,
    ) -> FlowFuture<JsString> {
        let work = PhoneCodexVoice::prepare(self, call, generation, selection);
        Box::pin(async move { work.wait().await })
    }
    fn start(
        &self,
        call: IvyJson,
        generation: u32,
        thread_id: JsString,
        prompt: JsString,
        sdp: JsString,
        guard: VoiceAsyncGuard,
    ) -> FlowFuture<JsString> {
        let work = self.start_checked(call, generation, thread_id, prompt, sdp, guard);
        Box::pin(async move { work?.wait().await })
    }
    fn connected(&self, call_id: JsString, guard: VoiceAsyncGuard) -> FlowFuture<()> {
        let work = PhoneCodexVoice::connected(self, call_id, guard);
        Box::pin(async move { work.wait().await })
    }
    fn select(
        &self,
        call: IvyJson,
        thread_id: JsString,
        command: VoiceSelectionCommand,
        selection: VoiceSelection,
        guard: VoiceAsyncGuard,
    ) -> FlowFuture<()> {
        let work = PhoneCodexVoice::select(self, call, thread_id, command, selection, guard);
        Box::pin(async move { work.wait().await })
    }
    fn stop(&self, call_id: JsString, keep_thread: Option<JsString>) -> FlowFuture<()> {
        let work = PhoneCodexVoice::stop(self, call_id, keep_thread);
        Box::pin(async move { work.wait().await })
    }
    fn close(&self) -> FlowFuture<()> {
        let work = PhoneCodexVoice::close(self);
        Box::pin(async move { work.wait().await })
    }
}

#[derive(Clone)]
pub struct PhoneFlowWork(watch::Receiver<Option<IvyResult<IvyJson>>>);
impl PhoneFlowWork {
    pub async fn wait(&self) -> IvyResult<IvyJson> {
        let mut receive = self.0.clone();
        loop {
            if let Some(result) = receive.borrow_and_update().clone() {
                return result;
            }
            receive
                .changed()
                .await
                .map_err(|_| IvyError::internal().with_outcome(Outcome::Unknown))?;
        }
    }
}
fn work(action: impl Future<Output = IvyResult<IvyJson>> + Send + 'static) -> PhoneFlowWork {
    let (done, receiver) = watch::channel(None);
    tokio::spawn(async move {
        let result = tokio::spawn(action)
            .await
            .unwrap_or_else(|_| Err(IvyError::internal().with_outcome(Outcome::Unknown)));
        done.send_replace(Some(result));
    });
    PhoneFlowWork(receiver)
}
#[derive(Clone)]
pub enum PhoneVoiceControl {
    Select(VoiceSelection),
    Update {
        selection: VoiceSelection,
        voice_id: JsString,
    },
    Restart,
}
#[derive(Default)]
struct State {
    admissions: BTreeMap<JsString, PhoneFlowWork>,
    work: BTreeMap<JsString, PhoneFlowWork>,
    cleanups: BTreeMap<JsString, PhoneFlowWork>,
    command_sequences: BTreeMap<JsString, u32>,
    ended: BTreeSet<JsString>,
    abandoned: BTreeSet<JsString>,
    selections: BTreeMap<JsString, VoiceSelection>,
    models: Vec<IvyJson>,
    closed: bool,
    closing: Option<PhoneFlowWork>,
}
struct Inner {
    calls: PhoneCallCommands,
    native: PhoneNativeClient,
    settings: IvyJson,
    voice: Option<Arc<dyn PhoneFlowVoice>>,
    logs: Option<PhoneCallLogs>,
    on_error: Arc<dyn Fn(&JsString, &str) + Send + Sync>,
    on_connected: Arc<dyn Fn(&JsString) + Send + Sync>,
    cleanup_observer: Mutex<Option<crate::voice_rpc::WorkObserver>>,
    commands: tokio::sync::Mutex<()>,
    state: Mutex<State>,
}
struct Reservation {
    flow: PhoneFlow,
    id: JsString,
    cleanup: bool,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        let mut state = self.flow.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if self.cleanup {
            state.cleanups.remove(&self.id);
        } else {
            state.work.remove(&self.id);
        }
    }
}
#[derive(Clone)]
pub struct PhoneFlow(Arc<Inner>);
fn field<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a IvyJson> {
    value.get(key).ok_or_else(IvyError::internal)
}
fn text(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    field(value, key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn operation_result(operation: &IvyJson) -> IvyResult<IvyJson> {
    let result = operation
        .get("receipt")
        .filter(|r| r.get("ok") == Some(&true.into()))
        .and_then(|r| r.get("result"));
    if operation.get("phase") == Some(&"result".into())
        && result.is_some_and(|r| r.as_object().is_some())
    {
        return Ok(result.unwrap().clone());
    }
    Err(IvyError::new(
        "phone_command_incomplete",
        "Original Phone command has no confirmed result. Read its retained outcome.",
    )
    .with_outcome(Outcome::Unknown))
}
fn failed_result(operation: &IvyJson) -> bool {
    operation.get("phase") == Some(&"result".into())
        && operation.get("receipt").and_then(|r| r.get("ok")) == Some(&false.into())
}
fn route(settings: &IvyJson, call: &IvyJson) -> IvyJson {
    call.get("route")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| {
            let incoming = call.get("direction") == Some(&"incoming".into());
            settings
                .get(if incoming {
                    "incomingRoute"
                } else {
                    "outgoingRoute"
                })
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or_else(|| {
                    if incoming {
                        "voice".into()
                    } else {
                        "windows".into()
                    }
                })
        })
}
impl PhoneFlow {
    pub fn set_cleanup_observer(&self, observer: crate::voice_rpc::WorkObserver) {
        *self.0.cleanup_observer.lock().unwrap() = Some(observer);
    }
    pub fn new(
        calls: PhoneCallCommands,
        native: PhoneNativeClient,
        settings: IvyJson,
        voice: Option<Arc<dyn PhoneFlowVoice>>,
        logs: Option<PhoneCallLogs>,
        on_error: Arc<dyn Fn(&JsString, &str) + Send + Sync>,
        on_connected: Arc<dyn Fn(&JsString) + Send + Sync>,
    ) -> IvyResult<Self> {
        if let Some(audio) = settings.get("audio").filter(|v| !v.is_null()) {
            validate_phone("AudioSettings", audio)?;
        }
        let ring = settings
            .get("ringSeconds")
            .and_then(IvyJson::as_f64)
            .unwrap_or(f64::NAN);
        require(
            ring.is_finite() && ring.fract() == 0.0 && (1.0..=120.0).contains(&ring),
            "invalid_arguments",
            "Phone ringing must be bounded.",
        )?;
        Ok(Self(Arc::new(Inner {
            calls,
            native,
            settings,
            voice,
            logs,
            on_error,
            on_connected,
            cleanup_observer: Mutex::new(None),
            commands: tokio::sync::Mutex::new(()),
            state: Mutex::new(State::default()),
        })))
    }
    pub fn calls(&self) -> &PhoneCallCommands {
        &self.0.calls
    }
    pub fn logs(&self) -> Option<&PhoneCallLogs> {
        self.0.logs.as_ref()
    }
    pub fn call_route(&self, call: &IvyJson) -> IvyJson {
        route(&self.0.settings, call)
    }
    fn codex(&self) -> Option<&IvyJson> {
        self.0.settings.get("codexVoice").filter(|v| !v.is_null())
    }
    fn voice(&self) -> IvyResult<&Arc<dyn PhoneFlowVoice>> {
        self.0.voice.as_ref().ok_or_else(|| {
            IvyError::new(
                "phone_voice_unconfigured",
                "Voice requires its configured Codex runtime.",
            )
        })
    }
    fn report(&self, id: &JsString, error: &IvyError) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (self.0.on_error)(id, &error.code)
        }));
    }
    pub async fn voice_model_catalog(&self) -> IvyResult<IvyJson> {
        let models = self.voice()?.models().await?;
        self.0.state.lock().unwrap().models =
            models.as_array().ok_or_else(IvyError::internal)?.to_vec();
        Ok(models)
    }
    pub async fn speaker_voice_catalog(&self) -> IvyResult<IvyJson> {
        let voice = self.0.voice.as_ref().ok_or_else(|| {
            IvyError::new(
                "phone_voice_unavailable",
                "Configured Voice cannot discover speaker voices.",
            )
        })?;
        voice.speaker_voices().await
    }
    async fn default_selection(&self) -> IvyResult<VoiceSelection> {
        let mut selection = match self.0.settings.get("voiceDefault").filter(|v| !v.is_null()) {
            Some(v) => VoiceSelection {
                model: text(v, "model")?,
                reasoning_effort: text(v, "reasoningEffort")?,
            },
            None => VoiceSelection::default(),
        };
        if self.codex().and_then(|v| v.get("rememberReasoning")) == Some(&true.into()) {
            if let Some(effort) = self.calls().journal().remembered_voice_reasoning().await? {
                let candidate = VoiceSelection {
                    model: selection.model.clone(),
                    reasoning_effort: effort,
                };
                if candidate
                    .validate(&IvyJson::Array(self.0.state.lock().unwrap().models.clone()))
                    .is_ok()
                {
                    selection = candidate;
                }
            }
        }
        Ok(selection)
    }
    pub async fn voice_selection(&self, id: &JsString) -> IvyResult<VoiceSelection> {
        let selection = self.0.state.lock().unwrap().selections.get(id).cloned();
        match selection {
            Some(v) => Ok(v),
            None => self.default_selection().await,
        }
    }
    pub async fn prewarm_voice(&self, principal: JsString) -> IvyResult<()> {
        let Some(voice) = self.0.voice.as_ref() else {
            return Ok(());
        };
        // Discover voices while the task warms, so the first call can reuse the
        // same connection-scoped catalog without delaying task preparation.
        let task = async {
            let models = self.voice_model_catalog().await?;
            let selection = self.default_selection().await?;
            selection.validate(&models)?;
            voice.prewarm(principal, selection).await
        };
        let (task, speakers) = tokio::join!(task, voice.speaker_voices());
        task?;
        speakers?;
        Ok(())
    }
    pub fn audio_profiles(&self) -> IvyJson {
        match self.0.settings.get("audio").filter(|v| !v.is_null()) {
            Some(audio) => IvyJson::Array(vec![IvyJson::object([
                ("route", "windows".into()),
                ("settings", audio.clone()),
            ])]),
            None => IvyJson::Array(vec![]),
        }
    }
    fn windows_audio(&self) -> IvyResult<IvyJson> {
        self.0
            .settings
            .get("audio")
            .filter(|v| !v.is_null())
            .cloned()
            .ok_or_else(|| {
                IvyError::new(
                    "phone_audio_unconfigured",
                    "Windows calls require explicit Windows audio settings.",
                )
            })
    }
    pub async fn configuration(&self) -> IvyResult<IvyJson> {
        let policy = self.calls().admission().definition();
        let codex = self.codex();
        let queue = codex
            .and_then(|v| v.get("queueMs"))
            .filter(|v| !v.is_null())
            .cloned();
        let prebuffer = codex
            .map(|v| {
                v.get("playbackPrebufferMs")
                    .filter(|v| !v.is_null())
                    .cloned()
                    .unwrap_or_else(|| {
                        60.0_f64
                            .min(queue.as_ref().and_then(IvyJson::as_f64).unwrap_or(80.0))
                            .into()
                    })
            })
            .unwrap_or(IvyJson::Null);
        Ok(IvyJson::object([
            (
                "incomingRoute",
                self.0
                    .settings
                    .get("incomingRoute")
                    .filter(|v| !v.is_null())
                    .cloned()
                    .unwrap_or_else(|| "voice".into()),
            ),
            (
                "outgoingRoute",
                self.0
                    .settings
                    .get("outgoingRoute")
                    .filter(|v| !v.is_null())
                    .cloned()
                    .unwrap_or_else(|| "windows".into()),
            ),
            (
                "screeningConfigured",
                self.0
                    .settings
                    .get("speech")
                    .is_some_and(|v| !v.is_null())
                    .into(),
            ),
            (
                "accessChallengeConfigured",
                (self
                    .0
                    .settings
                    .get("accessCodePath")
                    .and_then(IvyJson::as_js_string)
                    .is_some_and(|v| !v.0.is_empty())
                    && policy
                        .get("challengedIncoming")
                        .and_then(IvyJson::as_array)
                        .is_some_and(|v| !v.is_empty()))
                .into(),
            ),
            (
                "directDialConfigured",
                (policy.get("allowDirectDial") == Some(&true.into())).into(),
            ),
            (
                "voiceDefault",
                self.0
                    .settings
                    .get("voiceDefault")
                    .filter(|v| !v.is_null())
                    .cloned()
                    .unwrap_or_else(|| VoiceSelection::default().to_wire()),
            ),
            (
                "voiceEffectiveDefault",
                self.default_selection().await?.to_wire(),
            ),
            ("voicePlaybackPrebufferMs", prebuffer),
            ("voiceQueueMs", queue.unwrap_or(IvyJson::Null)),
            (
                "rememberVoiceReasoning",
                codex
                    .and_then(|v| v.get("rememberReasoning"))
                    .cloned()
                    .unwrap_or(false.into()),
            ),
            (
                "resumeIncomingConversation",
                codex
                    .and_then(|v| v.get("resumeIncomingConversation"))
                    .cloned()
                    .unwrap_or(false.into()),
            ),
            (
                "resetAfterOutgoingCall",
                codex
                    .and_then(|v| v.get("resetAfterOutgoingCall"))
                    .cloned()
                    .unwrap_or(true.into()),
            ),
        ]))
    }
    async fn original(
        &self,
        principal: JsString,
        id: JsString,
        action: &'static str,
    ) -> IvyResult<IvyJson> {
        let call = self.calls().journal().call(id).await?.ok_or_else(|| {
            IvyError::new(
                "phone_call_missing",
                "Original Phone admission is unavailable.",
            )
        })?;
        self.calls()
            .admission()
            .authorize(&principal, &call, action)?;
        Ok(call)
    }
    async fn continuing(&self, call: &IvyJson) -> IvyResult<()> {
        let id = text(call, "callId")?;
        let local = || {
            let state = self.0.state.lock().unwrap();
            require(
                !state.closed && !state.ended.contains(&id),
                "phone_call_cancelled",
                "The original call is no longer admitted for positive work.",
            )
        };
        local()?;
        self.calls()
            .journal()
            .flow_continuing(
                self.calls().admission().clone(),
                call.clone(),
                self.0.native.epoch().clone(),
            )
            .await?;
        local()
    }
    fn connected_guard(&self, call: IvyJson) -> VoiceAsyncGuard {
        let flow = self.clone();
        Arc::new(move || {
            let flow = flow.clone();
            let call = call.clone();
            Box::pin(async move { flow.confirm_connected(&call).await })
        })
    }
    fn continuing_guard(&self, call: IvyJson) -> VoiceAsyncGuard {
        let flow = self.clone();
        Arc::new(move || {
            let flow = flow.clone();
            let call = call.clone();
            Box::pin(async move { flow.continuing(&call).await })
        })
    }
    async fn confirm_connected(&self, call: &IvyJson) -> IvyResult<()> {
        let id = text(call, "callId")?;
        for attempt in 0..3 {
            self.continuing(call).await?;
            let observed = match self.0.native.observe(Some(id.clone())).await {
                Ok(v) => v,
                Err(cause) if cause.code == "invalid_arguments" => {
                    if attempt == 2 {
                        return Err(IvyError::new(
                            "phone_voice_status_invalid",
                            "Native call status remained invalid after SIP answer.",
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(100 * (attempt + 1))).await;
                    continue;
                }
                Err(cause) => return Err(cause),
            };
            self.continuing(call).await?;
            require(
                observed.get("call").is_some_and(|v| {
                    v.get("id") == Some(&id.clone().into())
                        && v.get("state") == Some(&"connected".into())
                }),
                "phone_call_cancelled",
                "Original call ended before Voice submission.",
            )?;
            return Ok(());
        }
        Err(IvyError::internal())
    }
    fn reserve(&self, id: JsString, action: FlowFuture<IvyJson>) -> IvyResult<PhoneFlowWork> {
        let mut state = self.0.state.lock().unwrap();
        require(
            !state.work.contains_key(&id),
            "phone_call_busy",
            "Wait for the current call action before changing Voice.",
        )?;
        let flow = self.clone();
        let key = id.clone();
        let pending = work(async move {
            let _reservation = Reservation {
                flow,
                id: key,
                cleanup: false,
            };
            action.await
        });
        state.work.insert(id, pending.clone());
        Ok(pending)
    }
    pub async fn speaker_voice_state(&self, id: JsString, catalog: &IvyJson) -> IvyResult<IvyJson> {
        let selected = self
            .calls()
            .journal()
            .selected_speaker_voice(id.clone())
            .await?
            .unwrap_or(text(catalog, "defaultVoiceId")?);
        let effective = self.0.voice.as_ref().and_then(|v| v.speaker_voice(&id));
        Ok(IvyJson::object([
            ("callId", id.into()),
            ("selectedVoiceId", selected.clone().into()),
            (
                "effectiveVoiceId",
                effective.clone().map(Into::into).unwrap_or(IvyJson::Null),
            ),
            (
                "restartRequired",
                effective.is_some_and(|v| v != selected).into(),
            ),
            ("appliesOn", "next_voice_session".into()),
        ]))
    }
    pub fn select_speaker_voice(
        &self,
        principal: JsString,
        id: JsString,
        operation_id: JsString,
        voice_id: JsString,
    ) -> PhoneFlowWork {
        let flow = self.clone();
        work(async move {
            let gate = flow.0.commands.lock().await;
            let call = flow.original(principal, id.clone(), "read").await?;
            if let Some(prior) = flow
                .calls()
                .journal()
                .operation(operation_id.clone())
                .await?
            {
                let intent = field(&prior, "intent")?;
                require(
                    intent.get("callId") == Some(&id.into())
                        && intent.get("method") == Some(&"call.selectSpeakerVoice".into())
                        && intent.get("voiceId") == Some(&voice_id.into()),
                    "mutation_conflict",
                    "Speaker operation belongs to another call or selection.",
                )?;
                return Ok(prior);
            }
            flow.continuing(&call).await?;
            require(
                flow.call_route(&call) == "voice".into(),
                "phone_voice_task_missing",
                "Speaker selection requires a Voice call.",
            )?;
            require(
                !flow.0.state.lock().unwrap().work.contains_key(&id),
                "phone_call_busy",
                "Wait for the current call action before selecting a speaker.",
            )?;
            let action_flow = flow.clone();
            let action_id = id.clone();
            let pending = flow.reserve(id, Box::pin(async move {
                let catalog = action_flow.speaker_voice_catalog().await?;
                let advertised = field(&catalog, "voices")?
                    .as_array().ok_or_else(IvyError::internal)?
                    .iter().any(|v| v.get("id") == Some(&voice_id.clone().into()));
                require(advertised, "phone_speaker_voice_invalid",
                    "Select a speaker voice advertised by phone_bridge_speaker_voices for native V3.")?;
                action_flow.confirm_connected(&call).await?;
                let intent = IvyJson::object([
                    ("epoch", field(&call,"epoch")?.clone()),
                    ("callId", action_id.clone().into()),
                    ("operationId", operation_id.into()),
                    ("method", "call.selectSpeakerVoice".into()),
                    ("voiceId", voice_id.clone().into()),
                    ("requestHash", hash_json(&IvyJson::object([("voiceId",voice_id.clone().into())]))?.into()),
                ]);
                let effective = action_flow.0.voice.as_ref().and_then(|v|v.speaker_voice(&action_id));
                let result = IvyJson::object([
                    ("callId", action_id.into()),
                    ("selectedVoiceId", voice_id.clone().into()),
                    ("effectiveVoiceId", effective.clone().map(Into::into).unwrap_or(IvyJson::Null)),
                    ("restartRequired", effective.is_some_and(|v|v!=voice_id).into()),
                    ("appliesOn", "next_voice_session".into()),
                ]);
                action_flow.calls().journal().retain_speaker_voice_selection(intent,result).await
            }))?;
            drop(gate);
            pending.wait().await
        })
    }
    pub async fn voice_inputs(&self, principal: JsString, id: JsString) -> IvyResult<Vec<IvyJson>> {
        self.original(principal, id.clone(), "read").await?;
        self.0
            .voice
            .as_ref()
            .map(|v| v.inputs(&id))
            .unwrap_or_else(|| Ok(vec![]))
    }
    pub async fn voice_work(
        &self,
        principal: JsString,
        id: JsString,
    ) -> IvyResult<Option<IvyJson>> {
        self.original(principal, id.clone(), "read").await?;
        if let Some(voice) = &self.0.voice {
            voice.work(id).await
        } else {
            Ok(None)
        }
    }
    pub fn has_voice_work(&self) -> bool {
        self.0.voice.as_ref().is_some_and(|v| v.has_work())
    }
    pub fn answer_voice_input(
        &self,
        principal: JsString,
        id: JsString,
        input: IvyJson,
        reply: IvyJson,
        input_token: JsString,
    ) -> PhoneFlowWork {
        let flow = self.clone();
        work(async move {
            let call = flow.original(principal, id.clone(), "connect").await?;
            let voice = flow.0.voice.as_ref().ok_or_else(|| {
                IvyError::new(
                    "phone_voice_unavailable",
                    "Configured Voice cannot receive task inputs.",
                )
            })?;
            if voice.work(id.clone()).await?.is_none() {
                flow.continuing(&call).await?;
            }
            voice.answer(id, input, reply, input_token).await?;
            Ok(IvyJson::Null)
        })
    }
    pub fn control_voice(
        &self,
        principal: JsString,
        id: JsString,
        operation_id: JsString,
        control: PhoneVoiceControl,
    ) -> PhoneFlowWork {
        let flow = self.clone();
        work(async move {
            let gate = flow.0.commands.lock().await;
            let call = flow.original(principal, id.clone(), "read").await?;
            if let Some(prior) = flow
                .calls()
                .journal()
                .operation(operation_id.clone())
                .await?
            {
                let intent = field(&prior, "intent")?;
                let matches = match &control {
                    PhoneVoiceControl::Restart => {
                        intent.get("method") == Some(&"call.restartVoice".into())
                    }
                    PhoneVoiceControl::Select(selection)
                    | PhoneVoiceControl::Update { selection, .. } => {
                        intent.get("method") == Some(&"call.selectVoice".into())
                            && intent.get("model") == Some(&selection.model.clone().into())
                            && intent.get("reasoningEffort")
                                == Some(&selection.reasoning_effort.clone().into())
                            && match &control {
                                PhoneVoiceControl::Update { voice_id, .. } => {
                                    intent.get("voiceId") == Some(&voice_id.clone().into())
                                }
                                _ => intent.get("voiceId").is_none(),
                            }
                    }
                };
                require(
                    intent.get("callId") == Some(&id.into()) && matches,
                    "mutation_conflict",
                    "Voice operation belongs to another command or selection.",
                )?;
                return Ok(prior);
            }
            flow.continuing(&call).await?;
            require(
                flow.call_route(&call) == "voice".into(),
                "phone_voice_task_missing",
                "Voice controls require a Voice call.",
            )?;
            let action_flow = flow.clone();
            let action_operation = operation_id.clone();
            let pending = flow.reserve(
                id,
                Box::pin(async move {
                    match control {
                        PhoneVoiceControl::Select(selection) => {
                            action_flow
                                .apply_selection(
                                    &call,
                                    VoiceSelectionCommand::Operation(action_operation),
                                    selection,
                                )
                                .await?
                        }
                        PhoneVoiceControl::Update {
                            selection,
                            voice_id,
                        } => {
                            action_flow
                                .apply_selection(
                                    &call,
                                    VoiceSelectionCommand::Update {
                                        operation_id: action_operation,
                                        voice_id,
                                    },
                                    selection,
                                )
                                .await?
                        }
                        PhoneVoiceControl::Restart => {
                            action_flow.recycle(&call, Some(action_operation)).await?
                        }
                    };
                    Ok(IvyJson::Null)
                }),
            )?;
            drop(gate);
            pending.wait().await?;
            flow.calls()
                .journal()
                .operation(operation_id)
                .await?
                .ok_or_else(|| {
                    IvyError::new(
                        "phone_operation_missing",
                        "Voice command has no retained outcome.",
                    )
                })
        })
    }
    async fn apply_selection(
        &self,
        call: &IvyJson,
        command: VoiceSelectionCommand,
        selection: VoiceSelection,
    ) -> IvyResult<()> {
        selection.validate(&self.voice_model_catalog().await?)?;
        if let VoiceSelectionCommand::Update { voice_id, .. } = &command {
            let catalog = self.speaker_voice_catalog().await?;
            require(
                catalog.required("voices")?.as_array().is_some_and(|vs| {
                    vs.iter()
                        .any(|v| v.get("id") == Some(&voice_id.clone().into()))
                }),
                "phone_speaker_voice_invalid",
                "Select a speaker voice advertised by phone_bridge_speaker_voices.",
            )?;
        }
        self.confirm_connected(call).await?;
        let id = text(call, "callId")?;
        let generation = self
            .calls()
            .journal()
            .latest_voice_generation(id.clone())
            .await?;
        let thread = match generation {
            Some(g) => self.calls().journal().voice_task(id.clone(), g).await?,
            None => None,
        };
        let voice = self.0.voice.as_ref();
        require(
            voice.is_some() && thread.is_some(),
            "phone_voice_task_missing",
            "Current Voice task is unavailable.",
        )?;
        voice
            .unwrap()
            .select(
                call.clone(),
                thread.unwrap(),
                command,
                selection.clone(),
                self.connected_guard(call.clone()),
            )
            .await?;
        if self.codex().and_then(|v| v.get("rememberReasoning")) == Some(&true.into()) {
            self.calls()
                .journal()
                .remember_voice_reasoning(selection.reasoning_effort.clone())
                .await?;
        }
        self.continuing(call).await?;
        self.0
            .state
            .lock()
            .unwrap()
            .selections
            .insert(id, selection);
        Ok(())
    }
    async fn start_voice(
        &self,
        call: &IvyJson,
        thread: JsString,
        sdp: JsString,
        generation: u32,
    ) -> IvyResult<()> {
        let checked = std::time::Instant::now();
        self.confirm_connected(call).await?;
        let id = text(call, "callId")?;
        crate::diagnostics::voice_start_timing(&id, "connected_check", checked);
        let voice = self.voice()?;
        let greeting = if call.get("direction") == Some(&"incoming".into()) {
            self.0
                .settings
                .get("incomingInitialPrompt")
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or_else(|| "Greet the user with \"Hi\"".into())
        } else {
            "Greet the person you called.".into()
        };
        let prompt = call
            .get("voicePrompt")
            .filter(|v| !v.is_null())
            .unwrap_or(&greeting)
            .as_js_string()
            .cloned()
            .ok_or_else(IvyError::internal)?;
        let answer = voice
            .start(
                call.clone(),
                generation,
                thread,
                prompt,
                sdp,
                self.continuing_guard(call.clone()),
            )
            .await?;
        self.continuing(call).await?;
        let connecting = std::time::Instant::now();
        let connected = operation_result(
            &self
                .calls()
                .answer_realtime(text(call, "principalId")?, id.clone(), generation, answer)
                .wait()
                .await?,
        )?;
        require(
            connected.get("callId") == Some(&id.clone().into())
                && connected.get("connected") == Some(&true.into()),
            "phone_voice_not_ready",
            "Native Voice media did not connect to the original call.",
        )?;
        crate::diagnostics::voice_start_timing(&id, "rtc_connect", connecting);
        self.continuing(call).await?;
        let opening = std::time::Instant::now();
        voice
            .connected(id.clone(), self.connected_guard(call.clone()))
            .await?;
        crate::diagnostics::voice_start_timing(&id, "opening_cue", opening);
        Ok(())
    }
    async fn recycle(&self, call: &IvyJson, operation_id: Option<JsString>) -> IvyResult<()> {
        self.confirm_connected(call).await?;
        let id = text(call, "callId")?;
        let generation = self
            .calls()
            .journal()
            .latest_voice_generation(id.clone())
            .await?;
        require(
            self.0.voice.is_some() && generation.is_some_and(|g| g < 128),
            "phone_voice_generation_exhausted",
            "Current Voice generation cannot be restarted.",
        )?;
        let generation = generation.unwrap();
        let next = generation + 1;
        let selection = self.voice_selection(&id).await?;
        let intent = match operation_id {
            Some(operation_id) => {
                let mut args = selection.to_wire();
                args.insert("voiceGeneration", f64::from(next).into());
                let mut intent = args.clone();
                intent.insert("epoch", field(call, "epoch")?.clone());
                intent.insert("callId", id.clone().into());
                intent.insert("operationId", operation_id.into());
                intent.insert("method", "call.restartVoice".into());
                intent.insert("requestHash", hash_json(&args)?.into());
                self.calls().journal().submit(intent.clone()).await?;
                Some(intent)
            }
            None => None,
        };
        let result = async {
            let voice = self.voice()?;
            let thread = voice.prepare(call.clone(), next, selection).await?;
            self.continuing(call).await?;
            voice.stop(id.clone(), Some(thread.clone())).await?;
            operation_result(
                &self
                    .calls()
                    .stop_realtime(text(call, "principalId")?, id.clone(), generation)
                    .wait()
                    .await?,
            )?;
            self.continuing(call).await?;
            let queue = self
                .codex()
                .and_then(|v| v.get("queueMs"))
                .and_then(IvyJson::as_f64)
                .unwrap_or(80.0) as u32;
            let prebuffer = self
                .codex()
                .and_then(|v| v.get("playbackPrebufferMs"))
                .and_then(IvyJson::as_f64)
                .map(|v| v as u32);
            let offer = operation_result(
                &self
                    .calls()
                    .prepare_realtime(
                        text(call, "principalId")?,
                        id.clone(),
                        next,
                        Some(queue),
                        prebuffer,
                    )
                    .wait()
                    .await?,
            )?;
            let sdp = offer
                .get("sdp")
                .and_then(IvyJson::as_js_string)
                .cloned()
                .ok_or_else(|| {
                    IvyError::new(
                        "phone_voice_sdp_invalid",
                        "Native restart has no SDP offer.",
                    )
                })?;
            self.start_voice(call, thread.clone(), sdp, next).await?;
            if let Some(intent) = &intent {
                self.calls()
                    .journal()
                    .finish_voice_restart(intent.clone(), Some(thread))
                    .await?;
            }
            Ok(())
        }
        .await;
        if let Err(cause) = result {
            if let Some(intent) = intent {
                if self.calls().journal().epoch().await?.as_ref()
                    == call.get("epoch").and_then(IvyJson::as_js_string)
                    && self
                        .calls()
                        .journal()
                        .operation(text(&intent, "operationId")?)
                        .await?
                        .is_some_and(|v| v.get("phase") == Some(&"submitted".into()))
                {
                    self.calls()
                        .journal()
                        .finish_voice_restart(intent, None)
                        .await?;
                }
            }
            let _ = self.cleanup_when_observed(call, None).await;
            return Err(cause);
        }
        Ok(())
    }
    pub fn bridge_screening(&self, principal: JsString, id: JsString) -> PhoneFlowWork {
        let flow = self.clone();
        work(async move {
            let call = flow
                .original(principal.clone(), id.clone(), "connect")
                .await?;
            flow.continuing(&call).await?;
            require(
                call.get("screening").is_some_and(|v| !v.is_null()),
                "phone_screening_missing",
                "Original call is not a screening session.",
            )?;
            let status = flow.0.native.observe(Some(id.clone())).await?;
            flow.continuing(&call).await?;
            let observed = status.get("call");
            require(
                observed.is_some_and(|v| {
                    v.get("id") == Some(&id.clone().into())
                        && v.get("state") == Some(&"connected".into())
                        && v.get("features")
                            .and_then(|v| v.get("screening"))
                            .is_some_and(|v| v == &"accepted".into() || v == &"bridged".into())
                }),
                "phone_screening_not_accepted",
                "The original screening recipient has not accepted.",
            )?;
            let result = async {
                operation_result(
                    &flow
                        .calls()
                        .bridge_screening(principal.clone(), id.clone(), flow.windows_audio()?)
                        .wait()
                        .await?,
                )
            }
            .await;
            if let Err(cause) = result {
                flow.cleanup(principal, id).wait().await?;
                return Err(cause);
            }
            Ok(call)
        })
    }
    fn call_result(&self, call: &IvyJson, operation: &IvyJson, states: &[&str]) -> IvyResult<()> {
        let observed = operation_result(operation)?;
        validate_phone("SipCallObservation", &observed)?;
        if observed.get("id") == call.get("callId")
            && observed.get("direction") == call.get("direction")
            && observed
                .get("state")
                .and_then(|value| value.as_str().ok())
                .is_some_and(|state| states.contains(&state.as_str()))
        {
            return Ok(());
        }
        Err(IvyError::new(
            "phone_call_unconfirmed",
            "The original SIP call did not confirm the required state.",
        )
        .with_outcome(Outcome::Unknown))
    }
    pub fn hangup(&self, principal: JsString, id: JsString) -> PhoneFlowWork {
        self.cleanup(principal, id)
    }
    fn cleanup(&self, principal: JsString, id: JsString) -> PhoneFlowWork {
        let flow = self.clone();
        work(async move {
            flow.original(principal.clone(), id.clone(), "cleanup")
                .await?;
            let pending = {
                let mut state = flow.0.state.lock().unwrap();
                if let Some(prior) = state.cleanups.get(&id) {
                    prior.clone()
                } else {
                    state.ended.insert(id.clone());
                    let action_flow = flow.clone();
                    let action_id = id.clone();
                    let pending = work(async move {
                        let _reservation = Reservation {
                            flow: action_flow.clone(),
                            id: action_id.clone(),
                            cleanup: true,
                        };
                        let result = action_flow.cleanup_core(principal, action_id.clone()).await;
                        let observer = action_flow.0.cleanup_observer.lock().unwrap().clone();
                        if let Some(observer) = observer { observer(&action_id, result.as_ref().err().map(|e| e.code.as_str())); }
                        result.map(|_| IvyJson::Null)
                    });
                    state.cleanups.insert(id, pending.clone());
                    pending
                }
            };
            pending.wait().await
        })
    }
    async fn cleanup_core(&self, principal: JsString, id: JsString) -> IvyResult<()> {
        let call = self
            .original(principal.clone(), id.clone(), "cleanup")
            .await?;
        if call.get("epoch") != Some(&self.0.native.epoch().clone().into())
            || self
                .calls()
                .journal()
                .current_call(Some(id.clone()))
                .await?
                .as_ref()
                .and_then(|v| v.get("callId"))
                != Some(&id.clone().into())
        {
            return Ok(());
        }
        let created = self
            .calls()
            .journal()
            .call_command(
                id.clone(),
                if call.get("direction") == Some(&"incoming".into()) {
                    "call.claim".into()
                } else {
                    "call.prepare".into()
                },
                0,
            )
            .await?;
        if created.as_ref().is_none_or(failed_result) {
            self.calls().cancel_unprepared(principal, id).wait().await?;
            return Ok(());
        }
        let hangup = self
            .calls()
            .hangup(principal.clone(), id.clone())
            .wait()
            .await?;
        self.call_result(&call, &hangup, &["local_ended", "outcome_unknown"])?;
        if let Some(voice) = &self.0.voice {
            let ended = hangup
                .get("receipt")
                .and_then(|r| r.get("result"))
                .and_then(|r| r.get("call"))
                .filter(|c| {
                    c.get("id") == Some(&id.clone().into())
                        && c.get("state") == Some(&"local_ended".into())
                })
                .and_then(|c| c.get("endedAt"))
                .and_then(IvyJson::as_js_string)
                .map(ivy_sdk::clock::parse_date)
                .filter(|n| n.is_finite())
                .map(|n| n as i64);
            voice.detach(id.clone(), ended).await?;
        }
        for generation in (0..=128).rev() {
            if let Some(prepared) = self
                .calls()
                .journal()
                .call_command(id.clone(), "call.realtime.prepare".into(), generation)
                .await?
            {
                if !failed_result(&prepared) {
                    operation_result(
                        &self
                            .calls()
                            .stop_realtime(principal.clone(), id.clone(), generation)
                            .wait()
                            .await?,
                    )?;
                }
                break;
            }
        }
        operation_result(
            &self
                .calls()
                .release(principal.clone(), id.clone())
                .wait()
                .await?,
        )?;
        {
            let mut state = self.0.state.lock().unwrap();
            state.command_sequences.remove(&id);
            state.ended.remove(&id);
            state.abandoned.remove(&id);
            state.selections.remove(&id);
        }
        if !self.0.state.lock().unwrap().closed && self.call_route(&call) == "voice".into() {
            let flow = self.clone();
            tokio::spawn(async move {
                if let Err(error) = flow.prewarm_voice(principal).await {
                    flow.report(&id, &error);
                }
            });
        }
        Ok(())
    }
    async fn cleanup_when_observed(
        &self,
        call: &IvyJson,
        observed: Option<IvyJson>,
    ) -> IvyResult<()> {
        let id = text(call, "callId")?;
        if self
            .calls()
            .journal()
            .current_call(Some(id.clone()))
            .await?
            .is_none()
        {
            self.0.state.lock().unwrap().abandoned.remove(&id);
            return Ok(());
        }
        let status = match observed {
            Some(v) => v,
            None => self.0.native.observe(Some(id.clone())).await?,
        };
        if status.get("call").and_then(|v| v.get("id")) != Some(&id.clone().into()) {
            let created = self
                .calls()
                .journal()
                .call_command(
                    id.clone(),
                    if call.get("direction") == Some(&"incoming".into()) {
                        "call.claim".into()
                    } else {
                        "call.prepare".into()
                    },
                    0,
                )
                .await?;
            if created.as_ref().is_none_or(failed_result) {
                self.calls()
                    .cancel_unprepared(text(call, "principalId")?, id.clone())
                    .wait()
                    .await?;
                self.0.state.lock().unwrap().abandoned.remove(&id);
            }
            return Ok(());
        }
        self.cleanup(text(call, "principalId")?, id.clone())
            .wait()
            .await?;
        self.0.state.lock().unwrap().abandoned.remove(&id);
        Ok(())
    }
    async fn command_feedback(&self, call: &IvyJson, sequence: u32, success: bool) {
        let result = async {
            self.continuing(call).await?;
            operation_result(
                &self
                    .calls()
                    .feedback(
                        text(call, "principalId")?,
                        text(call, "callId")?,
                        sequence,
                        success,
                    )
                    .wait()
                    .await?,
            )?;
            Ok(())
        }
        .await;
        if let Err(cause) = result {
            if let Ok(id) = text(call, "callId") {
                self.report(&id, &cause);
            }
        }
    }
    fn schedule_control(
        &self,
        call: IvyJson,
        sequence: u32,
        control: PhoneVoiceControl,
    ) -> IvyResult<()> {
        let id = text(&call, "callId")?;
        let flow = self.clone();
        self.reserve(
            id.clone(),
            Box::pin(async move {
                let result = match control {
                    PhoneVoiceControl::Restart => flow.recycle(&call, None).await,
                    PhoneVoiceControl::Select(selection) => {
                        flow.apply_selection(
                            &call,
                            VoiceSelectionCommand::Sequence(sequence),
                            selection,
                        )
                        .await
                    }
                    PhoneVoiceControl::Update { .. } => Err(IvyError::internal()),
                };
                if let Err(cause) = &result {
                    flow.report(&id, cause);
                }
                flow.command_feedback(&call, sequence, result.is_ok()).await;
                Ok(IvyJson::Null)
            }),
        )?;
        Ok(())
    }
    pub async fn observe(&self, observed: Option<IvyJson>) -> IvyResult<()> {
        for call in self.calls().journal().current_calls().await? {
            if call.get("epoch") != Some(&self.0.native.epoch().clone().into()) {
                continue;
            }
            let id = text(&call, "callId")?;
            let result = async {
                let status = match observed
                    .as_ref()
                    .filter(|v| v.get("call").and_then(|v| v.get("id")) == Some(&id.clone().into()))
                {
                    Some(v) => v.clone(),
                    None => self.0.native.observe(Some(id.clone())).await?,
                };
                if let Some(logs) = &self.0.logs {
                    let _ = logs.record(&call, &status);
                }
                let original = status
                    .get("call")
                    .filter(|v| v.get("id") == Some(&id.clone().into()));
                let media = status.get("media");
                let ended = original.is_some()
                    && (original.and_then(|v| v.get("state")).is_some_and(|v| {
                        v == &"local_ended".into() || v == &"outcome_unknown".into()
                    }) || media
                        .and_then(|v| v.get("audio"))
                        .and_then(|v| v.get("state"))
                        == Some(&"revoked".into())
                        || media.and_then(|v| v.get("failed")) == Some(&true.into())
                        || self.0.voice.as_ref().is_some_and(|v| v.failed(&id)));
                let (busy, abandoned) = {
                    let mut state = self.0.state.lock().unwrap();
                    if ended {
                        state.ended.insert(id.clone());
                    }
                    (state.work.contains_key(&id), state.abandoned.contains(&id))
                };
                if ended && let Some(voice) = &self.0.voice {
                    let ended = original
                        .filter(|c| c.get("state") == Some(&"local_ended".into()))
                        .and_then(|c| c.get("endedAt"))
                        .and_then(IvyJson::as_js_string)
                        .map(ivy_sdk::clock::parse_date)
                        .filter(|n| n.is_finite())
                        .map(|n| n as i64);
                    voice.disconnected(id.clone(), ended).await?;
                }
                if busy {
                    return Ok(());
                }
                if ended || abandoned {
                    self.cleanup_when_observed(&call, Some(status)).await?;
                    return Ok(());
                }
                let Some(original) =
                    original.filter(|v| v.get("state") == Some(&"connected".into()))
                else {
                    return Ok(());
                };
                let gate = self.0.commands.lock().await;
                // Recheck the per-call slot after waiting for an API control's
                // journal reads; never consume a keypad command behind it.
                if self.0.state.lock().unwrap().work.contains_key(&id) {
                    return Ok(());
                }
                let previous = self
                    .0
                    .state
                    .lock()
                    .unwrap()
                    .command_sequences
                    .get(&id)
                    .copied()
                    .unwrap_or(0);
                let next = original
                    .get("features")
                    .and_then(|v| v.get("commands"))
                    .and_then(IvyJson::as_array)
                    .and_then(|items| {
                        items.iter().find(|v| {
                            v.get("sequence")
                                .and_then(IvyJson::as_f64)
                                .is_some_and(|s| s > f64::from(previous))
                        })
                    });
                let Some(next) = next else {
                    return Ok(());
                };
                let sequence = field(next, "sequence")?
                    .as_f64()
                    .ok_or_else(IvyError::internal)? as u32;
                if sequence != previous + 1 {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        (self.0.on_error)(&id, "phone_command_overflow")
                    }))
                    .map_err(|_| IvyError::internal())?;
                }
                self.0
                    .state
                    .lock()
                    .unwrap()
                    .command_sequences
                    .insert(id.clone(), sequence);
                let control = if next.get("command") == Some(&"new_voice".into()) {
                    PhoneVoiceControl::Restart
                } else {
                    let model = next.get("model").and_then(IvyJson::as_js_string);
                    let effort = next.get("reasoningEffort").and_then(IvyJson::as_js_string);
                    require(
                        model.is_some_and(|v| !v.0.is_empty())
                            && effort.is_some_and(|v| !v.0.is_empty()),
                        "phone_voice_selection_invalid",
                        "Native Voice selection is incomplete.",
                    )?;
                    PhoneVoiceControl::Select(selected_keypad_model(model, effort)?)
                };
                self.schedule_control(call.clone(), sequence, control)?;
                drop(gate);
                Ok(())
            }
            .await;
            if let Err(cause) = result {
                self.report(&id, &cause);
            }
        }
        Ok(())
    }
    pub fn close(&self) -> PhoneFlowWork {
        let mut state = self.0.state.lock().unwrap();
        if let Some(prior) = &state.closing {
            return prior.clone();
        }
        state.closed = true;
        let flow = self.clone();
        let pending = work(async move {
            let cleanup = async {
                let calls = flow.calls().journal().current_calls().await?;
                let mut pending = Vec::new();
                for call in calls {
                    pending.push(flow.cleanup(text(&call, "principalId")?, text(&call, "callId")?));
                }
                let mut failure = None;
                for pending in pending {
                    if let Err(cause) = pending.wait().await {
                        if failure.is_none() {
                            failure = Some(cause);
                        }
                    }
                }
                failure.map_or(Ok(()), Err)
            }
            .await;
            // Revoke connected calls before draining requests that may be waiting
            // for Voice preparation. An admission completing after this snapshot
            // observes closed and performs its own cleanup before it settles.
            let admissions = flow
                .0
                .state
                .lock()
                .unwrap()
                .admissions
                .values()
                .cloned()
                .collect::<Vec<_>>();
            for admission in admissions {
                let _ = admission.wait().await;
            }
            let pending = flow
                .0
                .state
                .lock()
                .unwrap()
                .work
                .values()
                .cloned()
                .collect::<Vec<_>>();
            for pending in pending {
                let _ = pending.wait().await;
            }
            // The original finally block lets a Voice close failure replace a
            // cleanup failure, after all admitted work has drained.
            if let Some(voice) = &flow.0.voice {
                voice.close().await?;
            }
            cleanup?;
            Ok(IvyJson::Null)
        });
        state.closing = Some(pending.clone());
        pending
    }
}

#[cfg(test)]
#[path = "../../../../tests/rust/phone_service_flow.rs"]
mod tests;
