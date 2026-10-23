//! One call's Voice thread and capture authority. Input submission is never
//! proof that Voice started; only the original exact capture can bind audio.
use super::{
    ActionResult, DesktopActionError, DesktopCaptureCorrelation, DesktopCaptureIdentity,
    NativeVoiceInput, VoiceDispatchResult, VoiceInputPlan,
};
use crate::{
    media::{AudioDesktopIdentity, CallAudioPort},
    micro::{MicroGestureResult, MicroVoiceObservation},
};
use ivy_contracts::{IvyJson, json::JsString};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Notify;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceLifetimeResult {
    pub state: &'static str,
    pub dispatch: Option<VoiceDispatchResult>,
    pub reason: Option<&'static str>,
}
impl VoiceLifetimeResult {
    fn new(state: &'static str, dispatch: Option<VoiceDispatchResult>) -> Self {
        Self {
            state,
            dispatch,
            reason: None,
        }
    }
    pub fn to_json(&self) -> IvyJson {
        IvyJson::object([
            ("state", self.state.into()),
            (
                "dispatch",
                self.dispatch
                    .as_ref()
                    .map_or(IvyJson::Null, VoiceDispatchResult::to_json),
            ),
            ("reason", self.reason.map_or(IvyJson::Null, Into::into)),
        ])
    }
}
#[derive(Clone, Debug)]
pub struct VoiceBaselineObservation {
    pub cleanup: bool,
    pub controls_state: &'static str,
    pub capture_state: Option<&'static str>,
    pub controls_ms: f64,
    pub capture_ms: f64,
    pub age_ms: f64,
    pub reason: Option<&'static str>,
}
struct Completion<T> {
    result: Mutex<Option<ActionResult<T>>>,
    changed: Notify,
}
pub struct VoiceOperation<T>(Arc<Completion<T>>);
impl<T> Clone for VoiceOperation<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<T: Clone> VoiceOperation<T> {
    fn new() -> Self {
        Self(Arc::new(Completion {
            result: Mutex::new(None),
            changed: Notify::new(),
        }))
    }
    fn complete(&self, value: ActionResult<T>) {
        let mut result = self.0.result.lock().unwrap_or_else(|e| e.into_inner());
        if result.is_none() {
            *result = Some(value);
            self.0.changed.notify_waiters();
        }
    }
    pub fn result(&self) -> Option<ActionResult<T>> {
        self.0
            .result
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub async fn wait(&self) -> ActionResult<T> {
        loop {
            let changed = self.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(result) = self.result() {
                return result;
            }
            changed.await;
        }
    }
}
#[derive(Default)]
pub struct VoiceStop {
    set: AtomicBool,
    sync: Mutex<()>,
    changed: Condvar,
}
impl VoiceStop {
    pub fn is_set(&self) -> bool {
        self.set.load(Ordering::Acquire)
    }
    fn set(&self) {
        let _guard = self.sync.lock().unwrap_or_else(|e| e.into_inner());
        self.set.store(true, Ordering::Release);
        self.changed.notify_all();
    }
    pub fn wait(&self, duration: Duration) -> bool {
        let guard = self.sync.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = self
            .changed
            .wait_timeout_while(guard, duration, |_| !self.is_set())
            .unwrap_or_else(|e| e.into_inner());
        self.is_set()
    }
}
pub trait VoiceTime: Send + Sync {
    fn now(&self) -> Duration;
    fn wait(&self, stop: &VoiceStop, duration: Duration) -> ActionResult<bool> {
        Ok(stop.wait(duration))
    }
    fn sleep(&self, duration: Duration) -> ActionResult<()> {
        std::thread::sleep(duration);
        Ok(())
    }
}
struct SystemVoiceTime(Instant);
impl VoiceTime for SystemVoiceTime {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}

/// OS/device primitives remain with the supplied runtime. All call policy,
/// retries, freshness, fallback and cleanup ordering stay in this owner.
pub trait CallVoiceRuntime: Send + Sync {
    fn is_current(&self, desktop: &AudioDesktopIdentity) -> ActionResult<bool>;
    fn has_exited(&self, desktop: &AudioDesktopIdentity) -> ActionResult<bool>;
    fn capture(
        &self,
        desktop: &AudioDesktopIdentity,
        endpoint: &JsString,
    ) -> ActionResult<Option<DesktopCaptureCorrelation>>;
    fn select(
        &self,
        input: &NativeVoiceInput,
        wait: bool,
        original: &dyn Fn() -> ActionResult<bool>,
    ) -> ActionResult<VoiceInputPlan>;
    fn prepare_focus(
        &self,
        plan: &VoiceInputPlan,
        input: &NativeVoiceInput,
        desktop: &AudioDesktopIdentity,
        original: &dyn Fn() -> ActionResult<bool>,
    ) -> ActionResult<bool>;
    fn dispatch(
        &self,
        plan: &VoiceInputPlan,
        input: &NativeVoiceInput,
        desktop: &AudioDesktopIdentity,
        cleanup: bool,
        before: &dyn Fn() -> ActionResult<bool>,
        release: &dyn Fn() -> ActionResult<bool>,
        prior_micro: Option<MicroGestureResult>,
        was_dictation: bool,
    ) -> ActionResult<VoiceDispatchResult>;
}
pub trait CallVoiceAudio: Send + Sync {
    fn voice_target(&self) -> ActionResult<(AudioDesktopIdentity, JsString)>;
    fn bind(&self, endpoint: &JsString) -> ActionResult<()>;
    fn grant(&self) -> ActionResult<()>;
    fn clear(&self) -> ActionResult<()>;
    fn release(&self) -> ActionResult<()>;
    fn arm_recovery(&self) -> ActionResult<()>;
    fn revoke(&self);
    fn finish(&self, preserve: bool) -> ActionResult<()> {
        if preserve {
            self.clear()
        } else {
            self.revoke();
            Ok(())
        }
    }
}
impl CallVoiceAudio for CallAudioPort {
    fn finish(&self, preserve: bool) -> ActionResult<()> {
        self.finish_voice_audio(preserve)
            .map_err(|_| DesktopActionError::Unavailable)
    }
    fn voice_target(&self) -> ActionResult<(AudioDesktopIdentity, JsString)> {
        self.voice_target()
            .map(|(desktop, endpoint)| (desktop, endpoint.into()))
            .map_err(|_| DesktopActionError::Unavailable)
    }
    fn bind(&self, endpoint: &JsString) -> ActionResult<()> {
        self.bind_authority(
            &endpoint
                .to_utf8()
                .map_err(|_| DesktopActionError::Unavailable)?,
        )
        .map_err(|_| DesktopActionError::Unavailable)
    }
    fn grant(&self) -> ActionResult<()> {
        self.grant_authority()
            .map_err(|_| DesktopActionError::Unavailable)
    }
    fn clear(&self) -> ActionResult<()> {
        self.clear_authority()
            .map_err(|_| DesktopActionError::Unavailable)
    }
    fn release(&self) -> ActionResult<()> {
        self.release_voice_authority()
            .map_err(|_| DesktopActionError::Unavailable)
    }
    fn arm_recovery(&self) -> ActionResult<()> {
        self.arm_voice_recovery()
            .map_err(|_| DesktopActionError::Unavailable)
    }
    fn revoke(&self) {
        self.revoke();
    }
}
struct State {
    launched: bool,
    disposed: bool,
    preserve_audio: bool,
    final_audio_failure: Option<DesktopActionError>,
    closing: Option<VoiceOperation<()>>,
}
struct Inner {
    port: Arc<dyn CallVoiceAudio>,
    desktop: AudioDesktopIdentity,
    endpoint: JsString,
    input: NativeVoiceInput,
    runtime: Arc<dyn CallVoiceRuntime>,
    current: Arc<dyn Fn(bool) -> ActionResult<bool> + Send + Sync>,
    time: Arc<dyn VoiceTime>,
    baseline: Option<Arc<dyn Fn(VoiceBaselineObservation) + Send + Sync>>,
    stop: VoiceStop,
    started: VoiceOperation<VoiceLifetimeResult>,
    finished: VoiceOperation<VoiceLifetimeResult>,
    state: Mutex<State>,
    confirmed: Mutex<Option<DesktopCaptureIdentity>>,
}
#[derive(Clone)]
pub struct CallVoiceSession(Arc<Inner>);
impl CallVoiceSession {
    pub(super) fn final_audio_failure(&self) -> Option<DesktopActionError> {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .final_audio_failure
            .clone()
    }
    pub(super) fn revoke_for_owner(&self) -> ActionResult<()> {
        let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(error) = &state.final_audio_failure {
            return Err(error.clone());
        }
        self.0.port.revoke();
        Ok(())
    }
    pub fn new(
        port: CallAudioPort,
        desktop: AudioDesktopIdentity,
        input: NativeVoiceInput,
        runtime: Arc<dyn CallVoiceRuntime>,
        current: Arc<dyn Fn(bool) -> ActionResult<bool> + Send + Sync>,
    ) -> ActionResult<Self> {
        Self::with_time(
            Arc::new(port),
            desktop,
            input,
            runtime,
            current,
            Arc::new(SystemVoiceTime(Instant::now())),
            None,
        )
    }
    pub fn with_time(
        port: Arc<dyn CallVoiceAudio>,
        desktop: AudioDesktopIdentity,
        input: NativeVoiceInput,
        runtime: Arc<dyn CallVoiceRuntime>,
        current: Arc<dyn Fn(bool) -> ActionResult<bool> + Send + Sync>,
        time: Arc<dyn VoiceTime>,
        baseline: Option<Arc<dyn Fn(VoiceBaselineObservation) + Send + Sync>>,
    ) -> ActionResult<Self> {
        let (target, endpoint) = port.voice_target()?;
        if !desktop.same(&target) {
            return Err(DesktopActionError::Argument(
                "Voice must use the audio call's Desktop.",
            ));
        }
        Ok(Self(Arc::new(Inner {
            port,
            desktop,
            endpoint,
            input,
            runtime,
            current,
            time,
            baseline,
            stop: VoiceStop::default(),
            started: VoiceOperation::new(),
            finished: VoiceOperation::new(),
            state: Mutex::new(State {
                launched: false,
                disposed: false,
                preserve_audio: false,
                final_audio_failure: None,
                closing: None,
            }),
            confirmed: Mutex::new(None),
        })))
    }
    pub fn start(&self) -> ActionResult<VoiceOperation<VoiceLifetimeResult>> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.launched || state.disposed || self.0.stop.is_set() {
            return Err(DesktopActionError::Unavailable);
        }
        state.launched = true;
        let owner = self.clone();
        if std::thread::Builder::new()
            .name("Ivy call Voice".into())
            .spawn(move || {
                let apartment = VoiceApartment::enter();
                match apartment {
                    Ok(_apartment) => owner.run(),
                    Err(error) => {
                        owner.0.started.complete(Err(error.clone()));
                        owner.0.finished.complete(Err(error));
                    }
                }
            })
            .is_err()
        {
            state.launched = false;
            state.disposed = true;
            return Err(DesktopActionError::Unavailable);
        }
        Ok(self.0.started.clone())
    }
    pub fn stop(&self) -> VoiceOperation<VoiceLifetimeResult> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.disposed || state.final_audio_failure.is_some() {
            return self.0.finished.clone();
        }
        state.preserve_audio = false;
        self.0.port.revoke();
        self.0.stop.set();
        if !state.launched {
            let value = Ok(VoiceLifetimeResult::new("not_started", None));
            self.0.started.complete(value.clone());
            self.0.finished.complete(value);
        }
        self.0.finished.clone()
    }
    pub fn pause_for_transition(&self) -> ActionResult<VoiceOperation<VoiceLifetimeResult>> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(error) = &state.final_audio_failure {
            return Err(error.clone());
        }
        if state.disposed
            || self.0.stop.is_set()
            || !matches!(self.0.started.result(),Some(Ok(value)) if value.state=="active")
        {
            return Err(DesktopActionError::Unavailable);
        }
        state.preserve_audio = true;
        self.0.port.clear()?;
        self.0.stop.set();
        Ok(self.0.finished.clone())
    }
    pub fn retire_for_transition(&self) -> VoiceOperation<()> {
        let complete = VoiceOperation::new();
        if let Some(finished) = self.0.finished.result() {
            complete.complete(finished.and_then(|result| self.finish_retirement(result)));
        } else {
            let owner = self.clone();
            let result = complete.clone();
            tokio::spawn(async move {
                let outcome = match owner.0.finished.wait().await {
                    Ok(finished) => owner.finish_retirement(finished),
                    Err(error) => Err(error),
                };
                result.complete(outcome);
            });
        }
        complete
    }
    fn finish_retirement(&self, result: VoiceLifetimeResult) -> ActionResult<()> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.disposed || !state.preserve_audio || result.state != "stopped" {
            return Err(DesktopActionError::Unavailable);
        }
        self.0.port.release()?;
        state.disposed = true;
        let complete = VoiceOperation::new();
        complete.complete(Ok(()));
        state.closing = Some(complete);
        Ok(())
    }
    pub fn has_original_capture(&self) -> bool {
        guarded(|| {
            if self.0.stop.is_set()
                || !matches!(self.0.started.result(),Some(Ok(value)) if value.state=="active")
                || self.0.finished.result().is_some()
                || !self.current(false)?
            {
                return Ok(false);
            }
            let observed = self.capture()?;
            Ok(self.matched(&observed)
                && observed.and_then(|v| v.identity)
                    == *self.0.confirmed.lock().unwrap_or_else(|e| e.into_inner()))
        })
        .unwrap_or(false)
    }
    pub fn close(&self) -> VoiceOperation<()> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(closing) = &state.closing {
            return closing.clone();
        }
        let complete = VoiceOperation::new();
        state.closing = Some(complete.clone());
        let owner = self.clone();
        let result = complete.clone();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            let outcome = owner.dispose_core().await;
            result.complete(outcome);
        });
        complete
    }
    async fn dispose_core(&self) -> ActionResult<()> {
        let result = self.stop().wait().await?;
        if result.state == "outcome_unknown" {
            let owner = self.clone();
            let absent = tokio::task::spawn_blocking(move || -> ActionResult<bool> {
                for _ in 0..20 {
                    if owner.0.runtime.has_exited(&owner.0.desktop)?
                        || owner.current(true)? && owner.absent(&owner.capture()?)
                    {
                        return Ok(true);
                    }
                    owner.0.time.sleep(Duration::from_millis(100))?;
                }
                Ok(false)
            })
            .await
            .map_err(|_| DesktopActionError::Unavailable)??;
            if !absent {
                return Err(DesktopActionError::Unavailable);
            }
        }
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .disposed = true;
        Ok(())
    }
    fn current(&self, cleanup: bool) -> ActionResult<bool> {
        Ok((self.0.current)(cleanup)? && self.0.runtime.is_current(&self.0.desktop)?)
    }
    fn capture(&self) -> ActionResult<Option<DesktopCaptureCorrelation>> {
        self.0.runtime.capture(&self.0.desktop, &self.0.endpoint)
    }
    fn absent(&self, value: &Option<DesktopCaptureCorrelation>) -> bool {
        value.as_ref().is_some_and(|v| {
            v.endpoint_id == self.0.endpoint && v.state == "none" && v.identity.is_none()
        })
    }
    fn matched(&self, value: &Option<DesktopCaptureCorrelation>) -> bool {
        value.as_ref().is_some_and(|v| {
            v.endpoint_id == self.0.endpoint && v.state == "matched" && v.identity.is_some()
        })
    }
    fn elapsed(&self, start: Duration) -> Duration {
        self.0.time.now().saturating_sub(start)
    }
    fn dispatch_settings(
        &self,
        cleanup: bool,
        plan: &VoiceInputPlan,
        settings: &NativeVoiceInput,
        prior: Option<MicroGestureResult>,
        dictation: bool,
    ) -> ActionResult<Option<VoiceDispatchResult>> {
        if !self
            .0
            .runtime
            .prepare_focus(plan, settings, &self.0.desktop, &|| self.current(cleanup))?
        {
            return Ok(None);
        }
        self.0
            .runtime
            .dispatch(
                plan,
                settings,
                &self.0.desktop,
                cleanup,
                &|| Ok((cleanup || !self.0.stop.is_set()) && (self.0.current)(cleanup)?),
                &|| (self.0.current)(true),
                prior,
                dictation,
            )
            .map(Some)
    }
    fn dispatch(
        &self,
        cleanup: bool,
        plan: &mut VoiceInputPlan,
    ) -> ActionResult<Option<VoiceDispatchResult>> {
        let mut refused = None;
        for attempt in 0..2 {
            let result = self.dispatch_settings(cleanup, plan, &self.0.input, refused, false)?;
            if !result.as_ref().is_some_and(|r| r.phase == "not_submitted")
                || plan.method() != "micro"
                || self.0.input.hotkey_fallback().is_none()
                || attempt != 0
            {
                return Ok(result);
            }
            refused = result.and_then(|r| r.micro);
            *plan = VoiceInputPlan::Hotkey;
        }
        Ok(None)
    }
    fn select(
        &self,
        input: &NativeVoiceInput,
        wait: bool,
        cleanup: bool,
    ) -> ActionResult<VoiceInputPlan> {
        self.0.runtime.select(input, wait, &|| {
            if cleanup {
                self.current(true)
            } else {
                Ok(!self.0.stop.is_set() && (self.0.current)(false)?)
            }
        })
    }
    fn run(&self) {
        let mut work = Work {
            dispatch: None,
            binding: None,
            plan: None,
            micro_baseline: None,
            possible_start: false,
            reason: Some("voice_cancelled"),
        };
        let run = guarded(|| self.observe(&mut work));
        if run.is_err() {
            work.reason = Some("voice_observation_failed");
        }
        // The agreed exception to the original abrupt final-audio failure keeps
        // ownership fenced. Publish it under the same gate as stop/pause so no
        // later caller can repeat audio or input cleanup. Completion is one-shot:
        // an already active start remains active, but finished carries failure.
        let final_audio = {
            let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            let result = guarded(|| self.0.port.finish(state.preserve_audio));
            if let Err(error) = &result {
                state.final_audio_failure = Some(error.clone());
                self.0.started.complete(Err(error.clone()));
                self.0.finished.complete(Err(error.clone()));
            }
            result
        };
        if final_audio.is_err() {
            return;
        }
        let ending = if work.possible_start {
            guarded(|| self.cleanup(work.binding.as_ref(), &mut work.plan))
                .unwrap_or_else(|_| VoiceLifetimeResult::new("outcome_unknown", None))
        } else {
            VoiceLifetimeResult::new("not_started", None)
        };
        self.0.started.complete(Ok(VoiceLifetimeResult {
            state: if work.possible_start {
                "outcome_unknown"
            } else {
                "not_started"
            },
            dispatch: work.dispatch,
            reason: work.reason,
        }));
        self.0.finished.complete(Ok(ending));
    }
    fn observe(&self, work: &mut Work) -> ActionResult<()> {
        if self.0.stop.is_set() || !self.current(false)? {
            return Ok(());
        }
        let began = self.0.time.now();
        let mut baseline = self.capture()?;
        for _ in 0..20 {
            if !baseline.as_ref().is_some_and(|b| b.state == "unavailable")
                || self.0.stop.is_set()
                || !self.current(false)?
            {
                break;
            }
            self.0.time.wait(&self.0.stop, Duration::from_millis(50))?;
            baseline = self.capture()?;
        }
        work.reason = if self.absent(&baseline) {
            None
        } else {
            Some("capture_not_absent")
        };
        if let Some(observe) = &self.0.baseline {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                observe(VoiceBaselineObservation {
                    cleanup: false,
                    controls_state: "not_required",
                    capture_state: baseline.as_ref().map(|v| v.state),
                    controls_ms: 0.0,
                    capture_ms: self.elapsed(began).as_secs_f64() * 1000.0,
                    age_ms: self.elapsed(began).as_secs_f64() * 1000.0,
                    reason: work.reason,
                })
            }));
        }
        if self.0.input.adopt_existing() && self.matched(&baseline) {
            work.binding = baseline.and_then(|v| v.identity);
            work.possible_start = true;
        } else {
            if !self.absent(&baseline) {
                return Ok(());
            }
            if !self.current(false)? || self.0.stop.is_set() {
                work.reason = Some("voice_cancelled");
                return Ok(());
            }
            let mut plan;
            if self.0.input.micro().is_some() {
                let micro_only = NativeVoiceInput::new(
                    self.0.input.micro().cloned(),
                    None,
                    self.0.input.adopt_existing(),
                );
                plan = self.select(&micro_only, true, false)?;
                if let VoiceInputPlan::Micro {
                    generation: Some(g),
                    device: Some(device),
                } = &plan
                {
                    work.micro_baseline = Some(
                        device
                            .observe_voice(*g)
                            .map_err(|_| DesktopActionError::Unavailable)?,
                    );
                }
                work.plan = Some(plan.clone());
                work.dispatch = self.dispatch_settings(false, &plan, &micro_only, None, false)?;
                if work
                    .dispatch
                    .as_ref()
                    .is_some_and(|d| d.phase == "not_submitted")
                    && self.0.input.hotkey_fallback().is_some()
                {
                    plan = VoiceInputPlan::Hotkey;
                    work.plan = Some(plan.clone());
                    work.dispatch = self.dispatch(false, &mut plan)?;
                }
            } else {
                plan = self.select(&self.0.input, true, false)?;
                work.plan = Some(plan.clone());
                work.dispatch = self.dispatch(false, &mut plan)?;
            }
            work.plan = Some(plan);
            work.possible_start = work
                .dispatch
                .as_ref()
                .is_some_and(|d| matches!(d.phase, "submitted" | "outcome_unknown"));
            if !work
                .dispatch
                .as_ref()
                .is_some_and(|d| d.phase == "submitted")
            {
                work.reason = Some(if work.possible_start {
                    "input_outcome_unknown"
                } else {
                    "input_not_submitted"
                });
                return Ok(());
            }
        }
        work.reason = Some("capture_unconfirmed");
        let mut began = self.0.time.now();
        let mut candidate = None;
        let mut matched_since = None;
        let mut dictation_seen = None;
        while work.binding.is_none()
            && !self.0.stop.is_set()
            && self.current(false)?
            && self.elapsed(began) < Duration::from_secs(30)
        {
            let capture = self.capture()?;
            if let (
                Some(VoiceInputPlan::Micro {
                    generation: Some(g),
                    device: Some(device),
                }),
                Some(baseline),
            ) = (&work.plan, &work.micro_baseline)
            {
                let observed = device
                    .observe_voice(*g)
                    .map_err(|_| DesktopActionError::Unavailable)?;
                if observed.generation == baseline.generation
                    && observed.dictation_sequence > baseline.dictation_sequence
                {
                    dictation_seen.get_or_insert_with(|| self.0.time.now());
                }
            }
            if let Some(dictation_at) = dictation_seen {
                work.reason = Some("micro_dictation_detected");
                if self.0.input.hotkey_fallback().is_none() {
                    work.possible_start = false;
                    return Ok(());
                }
                if self.absent(&capture) {
                    let prior = work.dispatch.as_ref().and_then(|d| d.micro.clone());
                    work.possible_start = false;
                    work.plan = Some(VoiceInputPlan::Hotkey);
                    work.dispatch = self.dispatch_settings(
                        false,
                        work.plan.as_ref().unwrap(),
                        &self.0.input,
                        prior,
                        true,
                    )?;
                    work.possible_start = work
                        .dispatch
                        .as_ref()
                        .is_some_and(|d| matches!(d.phase, "submitted" | "outcome_unknown"));
                    if !work
                        .dispatch
                        .as_ref()
                        .is_some_and(|d| d.phase == "submitted")
                    {
                        work.reason = Some(if work.possible_start {
                            "input_outcome_unknown"
                        } else {
                            "input_not_submitted"
                        });
                        return Ok(());
                    }
                    work.reason = Some("capture_unconfirmed");
                    began = self.0.time.now();
                    work.micro_baseline = None;
                    dictation_seen = None;
                    candidate = None;
                    matched_since = None;
                    continue;
                }
                if self.elapsed(dictation_at) >= Duration::from_secs(5) {
                    work.possible_start = false;
                    return Ok(());
                }
                self.0.time.wait(&self.0.stop, Duration::from_millis(50))?;
                continue;
            }
            if self.matched(&capture) {
                let identity = capture.and_then(|v| v.identity);
                if work.plan.as_ref().is_none_or(|p| p.method() != "micro") {
                    work.binding = identity;
                } else {
                    if candidate != identity {
                        candidate = identity.clone();
                        matched_since = Some(self.0.time.now());
                    }
                    if matched_since
                        .is_some_and(|since| self.elapsed(since) >= Duration::from_millis(500))
                        && self.elapsed(began) >= Duration::from_millis(750)
                    {
                        work.binding = identity;
                    }
                }
            } else {
                candidate = None;
                matched_since = None;
                if capture.as_ref().is_some_and(|c| c.state == "ambiguous") {
                    break;
                }
            }
            if work.binding.is_none() {
                self.0.time.wait(&self.0.stop, Duration::from_millis(50))?;
            }
        }
        if work.binding.is_none() || self.0.stop.is_set() || !self.current(false)? {
            return Ok(());
        }
        self.0.port.bind(&self.0.endpoint)?;
        self.0.port.grant()?;
        *self.0.confirmed.lock().unwrap_or_else(|e| e.into_inner()) = work.binding.clone();
        self.0.port.arm_recovery()?;
        self.0.started.complete(Ok(VoiceLifetimeResult::new(
            "active",
            work.dispatch.clone(),
        )));
        let mut missing_since = None;
        let mut uncertain_since = None;
        let mut suspended = false;
        while !self.0.time.wait(&self.0.stop, Duration::from_secs(1))? && self.current(false)? {
            let capture = self.capture()?;
            if self.matched(&capture)
                && capture.as_ref().and_then(|v| v.identity.as_ref()) == work.binding.as_ref()
            {
                missing_since = None;
                uncertain_since = None;
                if suspended {
                    self.0.port.grant()?;
                    suspended = false;
                }
                continue;
            }
            if !suspended {
                self.0.port.clear()?;
                suspended = true;
            }
            if capture.as_ref().is_some_and(|c| c.state == "none") {
                let since = *missing_since.get_or_insert_with(|| self.0.time.now());
                uncertain_since = None;
                if self.elapsed(since) >= Duration::from_secs(10) {
                    work.reason = Some("voice_capture_ended");
                    break;
                }
            } else {
                let since = *uncertain_since.get_or_insert_with(|| self.0.time.now());
                missing_since = None;
                if self.elapsed(since) >= Duration::from_secs(5) {
                    work.reason = Some("capture_owner_changed");
                    break;
                }
            }
        }
        Ok(())
    }
    fn cleanup(
        &self,
        binding: Option<&DesktopCaptureIdentity>,
        plan: &mut Option<VoiceInputPlan>,
    ) -> ActionResult<VoiceLifetimeResult> {
        if !self.0.runtime.is_current(&self.0.desktop)? {
            return Ok(VoiceLifetimeResult::new(
                if self.0.runtime.has_exited(&self.0.desktop)? {
                    "stopped"
                } else {
                    "outcome_unknown"
                },
                None,
            ));
        }
        if !(self.0.current)(true)? {
            return Ok(VoiceLifetimeResult::new("outcome_unknown", None));
        }
        let capture = self.capture()?;
        let replaced = self.matched(&capture)
            && binding.is_some()
            && capture.as_ref().and_then(|c| c.identity.as_ref()) != binding;
        if plan.as_ref().is_some_and(|p| p.method() == "micro") {
            if replaced || capture.as_ref().is_some_and(|c| c.state == "ambiguous") {
                return Ok(VoiceLifetimeResult::new("outcome_unknown", None));
            }
            if let Some(VoiceInputPlan::Micro { generation, device }) = plan.as_ref() {
                let actual = device
                    .as_ref()
                    .map(|d| d.status())
                    .transpose()
                    .map_err(|_| DesktopActionError::Unavailable)?
                    .and_then(|s| s.generation);
                if actual != *generation {
                    let settings = NativeVoiceInput::new(
                        self.0.input.micro().cloned(),
                        None,
                        self.0.input.adopt_existing(),
                    );
                    *plan = Some(self.select(&settings, false, true)?);
                }
            }
            let settings = NativeVoiceInput::new(
                self.0.input.micro().cloned(),
                None,
                self.0.input.adopt_existing(),
            );
            let invoked =
                self.dispatch_settings(true, plan.as_ref().unwrap(), &settings, None, false)?;
            if !submitted(&invoked) {
                return Ok(VoiceLifetimeResult::new("outcome_unknown", invoked));
            }
            if self.wait_absent()? {
                return Ok(VoiceLifetimeResult::new("stopped", invoked));
            }
            let capture = self.capture()?;
            if self.0.input.hotkey_fallback().is_none()
                || !self.current(true)?
                || !self.matched(&capture)
                || binding.is_none()
                || capture.as_ref().and_then(|v| v.identity.as_ref()) != binding
            {
                return Ok(VoiceLifetimeResult::new("outcome_unknown", invoked));
            }
            *plan = Some(VoiceInputPlan::Hotkey);
            let fallback =
                self.dispatch_settings(true, plan.as_ref().unwrap(), &self.0.input, None, false)?;
            if !submitted(&fallback) {
                return Ok(VoiceLifetimeResult::new("outcome_unknown", fallback));
            }
            return Ok(VoiceLifetimeResult::new(
                if self.wait_absent()? {
                    "stopped"
                } else {
                    "outcome_unknown"
                },
                fallback,
            ));
        }
        if plan.is_none() && self.absent(&capture) {
            return Ok(VoiceLifetimeResult::new("stopped", None));
        }
        if replaced
            || capture.as_ref().is_some_and(|c| c.state == "ambiguous")
            || plan.is_none() && !self.matched(&capture)
        {
            return Ok(VoiceLifetimeResult::new("outcome_unknown", None));
        }
        if plan.is_none() {
            *plan = Some(self.select(&self.0.input, false, true)?);
        }
        let result = self.dispatch(true, plan.as_mut().unwrap())?;
        if !submitted(&result) {
            return Ok(VoiceLifetimeResult::new("outcome_unknown", result));
        }
        Ok(VoiceLifetimeResult::new(
            if self.wait_absent()? {
                "stopped"
            } else {
                "outcome_unknown"
            },
            result,
        ))
    }
    fn wait_absent(&self) -> ActionResult<bool> {
        let began = self.0.time.now();
        while self.elapsed(began) < Duration::from_secs(5) {
            if self.0.runtime.has_exited(&self.0.desktop)?
                || self.current(true)? && self.absent(&self.capture()?)
            {
                return Ok(true);
            }
            self.0.time.sleep(Duration::from_millis(100))?;
        }
        Ok(false)
    }
}
struct Work {
    dispatch: Option<VoiceDispatchResult>,
    binding: Option<DesktopCaptureIdentity>,
    plan: Option<VoiceInputPlan>,
    micro_baseline: Option<MicroVoiceObservation>,
    possible_start: bool,
    reason: Option<&'static str>,
}
fn submitted(result: &Option<VoiceDispatchResult>) -> bool {
    result.as_ref().is_some_and(|r| r.phase == "submitted")
}
// A new Voice thread enters the original MTA for its complete resource lifetime.
struct VoiceApartment;
impl VoiceApartment {
    fn enter() -> ActionResult<Self> {
        #[cfg(windows)]
        {
            #[link(name = "ole32")]
            unsafe extern "system" {
                fn CoInitializeEx(reserved: *mut std::ffi::c_void, mode: u32) -> i32;
            }
            // This owner runs on its newly created thread; mode0 is MTA.
            if unsafe { CoInitializeEx(std::ptr::null_mut(), 0) } < 0 {
                return Err(DesktopActionError::Unavailable);
            }
        }
        Ok(Self)
    }
}
impl Drop for VoiceApartment {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            #[link(name = "ole32")]
            unsafe extern "system" {
                fn CoUninitialize();
            }
            unsafe {
                CoUninitialize();
            }
        }
    }
}
fn guarded<T>(work: impl FnOnce() -> ActionResult<T>) -> ActionResult<T> {
    catch_unwind(AssertUnwindSafe(work)).map_err(|_| DesktopActionError::Unavailable)?
}

#[cfg(all(test, windows))]
#[path = "../../../../../tests/rust/phone_call_voice.rs"]
mod tests;
