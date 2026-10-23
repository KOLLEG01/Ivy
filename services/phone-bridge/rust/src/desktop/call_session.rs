//! One original call's Desktop actions, generations and retained resource drain.
use super::{
    ActionResult, CallVoiceAudio, CallVoiceRuntime, CallVoiceSession, DesktopActionError,
    NativeVoiceInput, PhoneDesktopRuntime, VoiceLifetimeResult, VoiceOperation,
    validate_application,
};
use crate::media::{AudioDesktopIdentity, CallAudioPort};
use ivy_contracts::IvyJson;
use std::{
    cell::RefCell,
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::watch;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallDesktopError {
    NotReady,
    Stopping,
    Conflict,
    Desktop(DesktopActionError),
}
impl From<DesktopActionError> for CallDesktopError {
    fn from(value: DesktopActionError) -> Self {
        Self::Desktop(value)
    }
}
impl CallDesktopError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotReady => "runtime_not_ready",
            Self::Stopping => "runtime_stopping",
            Self::Conflict => "operation_conflict",
            Self::Desktop(DesktopActionError::Argument(_)) => "invalid_request",
            _ => "operation_outcome_unknown",
        }
    }
}
type Result<T> = std::result::Result<T, CallDesktopError>;

#[derive(Clone)]
pub struct DesktopCallOperation<T: Clone>(watch::Receiver<Option<Result<T>>>);
impl<T: Clone> DesktopCallOperation<T> {
    fn completed(value: Result<T>) -> Self {
        Self(watch::channel(Some(value)).1)
    }
    pub fn result(&self) -> Option<Result<T>> {
        self.0.borrow().clone()
    }
    pub fn same_work(&self, other: &Self) -> bool {
        self.0.same_channel(&other.0)
    }
    pub async fn wait(mut self) -> Result<T> {
        loop {
            if let Some(value) = self.result() {
                return value;
            }
            if self.0.changed().await.is_err() {
                return Err(DesktopActionError::Unavailable.into());
            }
        }
    }
}
fn retained<T: Clone + Send + Sync + 'static>(
    work: impl Future<Output = Result<T>> + Send + 'static,
) -> DesktopCallOperation<T> {
    let (result, waiter) = watch::channel(None);
    tokio::spawn(async move {
        let value = tokio::spawn(work)
            .await
            .unwrap_or_else(|_| Err(DesktopActionError::Unavailable.into()));
        result.send_replace(Some(value));
    });
    DesktopCallOperation(waiter)
}

pub enum DesktopCallAction {
    Launch {
        application: String,
        start_if_missing: bool,
    },
    StartVoice {
        desktop: AudioDesktopIdentity,
        input: NativeVoiceInput,
    },
    StopVoice {
        desktop: AudioDesktopIdentity,
        input: NativeVoiceInput,
    },
    PauseVoice {
        desktop: AudioDesktopIdentity,
        input: NativeVoiceInput,
        generation: i32,
    },
    ResumeVoice {
        desktop: AudioDesktopIdentity,
        input: NativeVoiceInput,
        generation: i32,
    },
}
impl DesktopCallAction {
    fn name(&self) -> &'static str {
        match self {
            Self::Launch { .. } => "launch",
            Self::StartVoice { .. } => "startVoice",
            Self::StopVoice { .. } => "stopVoice",
            Self::PauseVoice { .. } => "pauseVoice",
            Self::ResumeVoice { .. } => "resumeVoice",
        }
    }
}
type Current = dyn Fn(bool) -> ActionResult<bool> + Send + Sync;
type VoiceFactory = dyn Fn(AudioDesktopIdentity, NativeVoiceInput, Arc<Current>) -> ActionResult<CallVoiceSession>
    + Send
    + Sync;
struct State {
    voice: Option<CallVoiceSession>,
    desktop: Option<AudioDesktopIdentity>,
    input: Option<NativeVoiceInput>,
    work: Vec<(String, DesktopCallOperation<IvyJson>)>,
    closing: Option<DesktopCallOperation<()>>,
    generation: i32,
    paused: bool,
}
struct Inner {
    id: String,
    runtime: PhoneDesktopRuntime,
    current: Arc<Current>,
    audio: Option<Arc<dyn CallVoiceAudio>>,
    voice_factory: Arc<VoiceFactory>,
    closed: Arc<AtomicBool>,
    gate: Mutex<()>,
    state: Mutex<State>,
}
#[derive(Clone)]
pub struct CallDesktopSession(Arc<Inner>);
thread_local! { static ENTERED: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) }; }
impl CallDesktopSession {
    pub fn new(
        id: String,
        runtime: PhoneDesktopRuntime,
        voice_runtime: Arc<dyn CallVoiceRuntime>,
        current: Arc<Current>,
        audio: Option<CallAudioPort>,
    ) -> Result<Self> {
        let voice_audio = audio.clone();
        Self::with_factory(
            id,
            runtime,
            current,
            audio.map(|p| Arc::new(p) as Arc<dyn CallVoiceAudio>),
            Arc::new(move |desktop, input, current| {
                CallVoiceSession::new(
                    voice_audio.clone().ok_or(DesktopActionError::Unavailable)?,
                    desktop,
                    input,
                    voice_runtime.clone(),
                    current,
                )
            }),
        )
    }
    fn with_factory(
        id: String,
        runtime: PhoneDesktopRuntime,
        current: Arc<Current>,
        audio: Option<Arc<dyn CallVoiceAudio>>,
        voice_factory: Arc<VoiceFactory>,
    ) -> Result<Self> {
        if !crate::native_rpc::guid_d(&id) {
            return Err(DesktopActionError::Argument("Original call required.").into());
        }
        Ok(Self(Arc::new(Inner {
            id,
            runtime,
            current,
            audio,
            voice_factory,
            closed: Arc::new(AtomicBool::new(false)),
            gate: Mutex::new(()),
            state: Mutex::new(State {
                voice: None,
                desktop: None,
                input: None,
                work: vec![],
                closing: None,
                generation: 0,
                paused: false,
            }),
        })))
    }
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn serialized<T>(&self, work: impl FnOnce() -> T) -> T {
        let key = Arc::as_ptr(&self.0) as usize;
        ENTERED.with(|entered| {
            if entered.borrow().contains(&key) {
                return work();
            }
            let _gate = self.0.gate.lock().unwrap_or_else(|e| e.into_inner());
            entered.borrow_mut().push(key);
            struct Reset<'a>(&'a RefCell<Vec<usize>>);
            impl Drop for Reset<'_> {
                fn drop(&mut self) {
                    self.0.borrow_mut().pop();
                }
            }
            let _reset = Reset(entered);
            work()
        })
    }
    fn current(&self, cleanup: bool) -> ActionResult<bool> {
        Ok(!self.0.closed.load(Ordering::Acquire) && (self.0.current)(cleanup)?)
    }
    fn voice_current(&self) -> Arc<Current> {
        // Retain just the original permit and closed flag, avoiding an owner /
        // Voice cycle while preserving the callback after observers disappear.
        let current = self.0.current.clone();
        let closed = self.0.closed.clone();
        Arc::new(move |cleanup| {
            Ok((cleanup || !closed.load(Ordering::Acquire)) && current(cleanup)?)
        })
    }
    /// Wire decoding is owned by the native command layer. These typed settings
    /// preserve the original constructor validation and immutable input snapshot.
    pub fn invoke(
        &self,
        call_id: Option<&str>,
        action: DesktopCallAction,
    ) -> Result<DesktopCallOperation<IvyJson>> {
        if let DesktopCallAction::Launch { application, .. } = &action {
            validate_application(application)?;
        }
        if matches!(
            action,
            DesktopCallAction::PauseVoice { .. } | DesktopCallAction::ResumeVoice { .. }
        ) {
            return self.serialized(|| self.transition(call_id, action));
        }
        if call_id != Some(self.0.id.as_str()) {
            return Err(CallDesktopError::NotReady);
        }
        self.serialized(|| {
            let name = action.name();
            let cleanup = matches!(action, DesktopCallAction::StopVoice { .. });
            if self.0.closed.load(Ordering::Acquire) {
                return Err(CallDesktopError::Stopping);
            }
            {
                let state = self.state();
                if state.work.iter().any(|(key, _)| key == name) {
                    return Err(CallDesktopError::Conflict);
                }
                if !cleanup && state.work.iter().any(|(_, work)| work.result().is_none()) {
                    return Err(CallDesktopError::NotReady);
                }
            }
            let pending = match action {
                DesktopCallAction::StartVoice { desktop, input } => {
                    if self.state().work.iter().any(|(key, _)| key == "stopVoice") {
                        return Err(CallDesktopError::Conflict);
                    }
                    if !self.current(false)? || self.0.audio.is_none() {
                        return Err(CallDesktopError::NotReady);
                    }
                    {
                        let mut state = self.state();
                        state.desktop = Some(desktop.clone());
                        state.input = Some(input.clone());
                    }
                    let voice = (self.0.voice_factory)(desktop, input, self.voice_current())?;
                    self.state().voice = Some(voice.clone());
                    self.voice_result(voice.start()?, name)
                }
                DesktopCallAction::StopVoice { desktop, input } => {
                    let voice = self.state().voice.clone();
                    if voice.is_some() && !self.original(&desktop, &input) {
                        return Err(DesktopActionError::Argument(
                            "Cleanup must retain the original Desktop and Voice input settings.",
                        )
                        .into());
                    }
                    self.revoke(voice.as_ref());
                    match voice {
                        Some(voice) => self.voice_result(voice.stop(), name),
                        None => DesktopCallOperation::completed(Ok(self.output(
                            name,
                            None,
                            VoiceLifetimeResult {
                                state: "not_started",
                                dispatch: None,
                                reason: None,
                            }
                            .to_json(),
                        ))),
                    }
                }
                DesktopCallAction::Launch {
                    application,
                    start_if_missing,
                } => {
                    let owner = self.clone();
                    retained(async move {
                        let output = tokio::task::spawn_blocking(move || {
                            if !owner.current(false)? {
                                return Err(CallDesktopError::NotReady);
                            }
                            let voice = owner.state().voice.clone();
                            if let Some(error) = voice
                                .as_ref()
                                .and_then(CallVoiceSession::final_audio_failure)
                            {
                                return Err(error.into());
                            }
                            let result = owner.0.runtime.launch(
                                Some(&application),
                                start_if_missing,
                                &|| owner.current(false),
                            )?;
                            Ok(owner.output(name, None, result.to_json()))
                        })
                        .await
                        .map_err(|_| CallDesktopError::from(DesktopActionError::Unavailable))?;
                        output
                    })
                }
                _ => unreachable!(),
            };
            self.state().work.push((name.into(), pending.clone()));
            Ok(pending)
        })
    }
    fn original(&self, desktop: &AudioDesktopIdentity, input: &NativeVoiceInput) -> bool {
        let state = self.state();
        state
            .desktop
            .as_ref()
            .is_some_and(|value| value.same(desktop))
            && state
                .input
                .as_ref()
                .is_some_and(|value| value.matches(Some(input)))
    }
    fn revoke(&self, voice: Option<&CallVoiceSession>) {
        // Only the agreed final-audio exception skips the owner's original
        // revoke. The Voice gate makes that check atomic with failure publication.
        if let Some(voice) = voice {
            let _ = voice.revoke_for_owner();
        } else if let Some(audio) = &self.0.audio {
            audio.revoke();
        }
    }
    fn output(&self, action: &str, generation: Option<i32>, result: IvyJson) -> IvyJson {
        let mut fields = vec![
            ("callId", self.0.id.as_str().into()),
            ("action", action.into()),
        ];
        if let Some(generation) = generation {
            fields.push(("generation", IvyJson::Number(generation.into())));
        }
        fields.push(("result", result));
        IvyJson::object(fields)
    }
    fn voice_result(
        &self,
        work: VoiceOperation<VoiceLifetimeResult>,
        action: &'static str,
    ) -> DesktopCallOperation<IvyJson> {
        if let Some(result) = work.result() {
            return DesktopCallOperation::completed(
                result
                    .map(|value| self.output(action, None, value.to_json()))
                    .map_err(Into::into),
            );
        }
        let owner = self.clone();
        retained(async move { Ok(owner.output(action, None, work.wait().await?.to_json())) })
    }
    fn transition(
        &self,
        call_id: Option<&str>,
        action: DesktopCallAction,
    ) -> Result<DesktopCallOperation<IvyJson>> {
        let name = action.name();
        let resume = matches!(action, DesktopCallAction::ResumeVoice { .. });
        let (desktop, input, generation) = match action {
            DesktopCallAction::PauseVoice {
                desktop,
                input,
                generation,
            }
            | DesktopCallAction::ResumeVoice {
                desktop,
                input,
                generation,
            } => (desktop, input, generation),
            _ => unreachable!(),
        };
        if self.0.closed.load(Ordering::Acquire) || !self.current(false)? {
            return Err(CallDesktopError::NotReady);
        }
        {
            let state = self.state();
            if state.voice.is_none()
                || state.work.iter().any(|(key, _)| key == "stopVoice")
                || call_id != Some(self.0.id.as_str())
            {
                return Err(CallDesktopError::NotReady);
            }
            let key = format!("{name}:{generation}");
            if !(0..=128).contains(&generation)
                || generation != state.generation + i32::from(resume)
                || state.work.iter().any(|(old, _)| old == &key)
                || state.work.iter().any(|(_, work)| work.result().is_none())
            {
                return Err(CallDesktopError::Conflict);
            }
        }
        if !self.original(&desktop, &input) {
            return Err(DesktopActionError::Argument(
                "Voice transitions retain the original Desktop and input configuration.",
            )
            .into());
        }
        let pending = if resume {
            let (desktop, input) = {
                let mut state = self.state();
                if !state.paused {
                    return Err(CallDesktopError::NotReady);
                }
                state.paused = false;
                state.generation = generation;
                (state.desktop.clone().unwrap(), state.input.clone().unwrap())
            };
            let input = NativeVoiceInput::new(
                input.micro().cloned(),
                input.hotkey_fallback().cloned(),
                false,
            );
            let voice = (self.0.voice_factory)(desktop, input, self.voice_current())?;
            self.state().voice = Some(voice.clone());
            self.voice_result(voice.start()?, name)
        } else {
            let voice = {
                let state = self.state();
                if state.paused {
                    return Err(CallDesktopError::Conflict);
                }
                state.voice.clone().unwrap()
            };
            // Like an async Task before its first await, begin pause now; retain
            // a synchronous failure as the generation's failed operation.
            match voice.pause_for_transition() {
                Err(error) => DesktopCallOperation::completed(Err(error.into())),
                Ok(work) => {
                    let owner = self.clone();
                    retained(async move {
                        tokio::task::yield_now().await;
                        let result = work.wait().await?;
                        if result.state == "stopped" {
                            owner.transition_current()?;
                            voice.retire_for_transition().wait().await?;
                            owner.serialized(|| {
                                owner.transition_current_core()?;
                                owner.state().paused = true;
                                Ok::<_, CallDesktopError>(())
                            })?;
                        }
                        Ok(owner.output(name, Some(generation), result.to_json()))
                    })
                }
            }
        };
        self.state()
            .work
            .push((format!("{name}:{generation}"), pending.clone()));
        Ok(pending)
    }
    fn transition_current(&self) -> Result<()> {
        self.serialized(|| self.transition_current_core())
    }
    fn transition_current_core(&self) -> Result<()> {
        let stopped = self.state().work.iter().any(|(key, _)| key == "stopVoice");
        if self.0.closed.load(Ordering::Acquire) || stopped || !self.current(false)? {
            Err(CallDesktopError::NotReady)
        } else {
            Ok(())
        }
    }
    pub(crate) fn same_owner(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn has_original_voice_capture(&self, desktop: &AudioDesktopIdentity) -> bool {
        let voice = self.serialized(|| {
            let state = self.state();
            if self.0.closed.load(Ordering::Acquire)
                || state.paused
                || !state
                    .desktop
                    .as_ref()
                    .is_some_and(|value| value.same(desktop))
            {
                None
            } else {
                state.voice.clone()
            }
        });
        voice.is_some_and(|voice| voice.has_original_capture())
    }
    pub fn close(&self) -> DesktopCallOperation<()> {
        self.serialized(|| {
            if let Some(closing) = &self.state().closing {
                return closing.clone();
            }
            self.0.closed.store(true, Ordering::Release);
            let (voice, work) = {
                let state = self.state();
                (
                    state.voice.clone(),
                    state
                        .work
                        .iter()
                        .map(|(_, work)| work.clone())
                        .collect::<Vec<_>>(),
                )
            };
            self.revoke(voice.as_ref());
            let voice = voice.map(|voice| voice.close());
            let ready = work.iter().all(|work| work.result().is_some());
            let immediate = if ready {
                match &voice {
                    None => Some(Ok(())),
                    Some(voice) => voice.result().map(|result| result.map_err(Into::into)),
                }
            } else {
                None
            };
            let complete = match immediate {
                Some(result) => DesktopCallOperation::completed(result),
                None => retained(async move {
                    // Both kinds of work already run independently. Drain every
                    // admitted action even if Voice disposal has failed first.
                    for work in work {
                        let _ = work.wait().await;
                    }
                    if let Some(voice) = voice {
                        voice.wait().await?;
                    }
                    Ok(())
                }),
            };
            self.state().closing = Some(complete.clone());
            complete
        })
    }
}

#[cfg(all(test, windows))]
#[path = "../../../../../tests/rust/phone_call_desktop.rs"]
mod tests;
