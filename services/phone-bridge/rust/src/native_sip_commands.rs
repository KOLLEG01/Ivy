//! Native SIP lifecycle commands composed with process-local replay receipts.
//! Status uses the original admitted calls; the caller retains platform owners.
use crate::{
    call_features::CallAccessSettings,
    native_config::{NativeConfigError, NativeMicroSettings},
    native_media_settings::{NativeAudioDesktop, NativeAudioSettings, NativeScreeningSettings},
    native_pipe::{NativeRuntime, NativeShutdown, NativeShutdownError},
    native_registration::registration_json,
    native_rpc::{
        NativeHandler, NativeInvocation, NativeInvocationStart, NativeOutcome, NativeRequest,
    },
    native_sip::{NativeSipError, NativeSipOperation, NativeSipOperations, retained},
    native_sip_status::{NativeSipViews, call_json, status_json},
    sip::SipHostError,
};
use ivy_contracts::{IvyJson, json::stringify};
use std::{
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::{Arc, Mutex},
    task::Poll,
};
#[path = "native_realtime.rs"]
mod realtime;

pub type NativeMicroSetup =
    Pin<Box<dyn Future<Output = Result<(), SipHostError>> + Send + 'static>>;
type Setup = dyn Fn(Option<NativeMicroSettings>) -> NativeMicroSetup + Send + Sync;

/// Lifecycle and status compose the original SIP/media/feature handles. The
/// remaining runtime supplies platform commands and its required Micro/realtime
/// observations; it cannot replace the SIP call projection.
pub struct NativeSipCommands {
    owner: NativeSipOperations,
    remaining: Arc<dyn NativeRuntime>,
    setup: Arc<Setup>,
    views: NativeSipViews,
    shutdown: Mutex<Option<NativeSipOperation<()>>>,
}
impl NativeSipCommands {
    /// Bind the retained passive probe to this same SIP idle/stop fence.
    /// The supplied factory owns the platform recorder, not a microphone/player.
    pub fn with_probe(mut self, factory: Arc<dyn crate::loopback_probe::ProbeFactory>) -> Self {
        self.remaining = Arc::new(crate::native_probe::NativeProbeCommands::new(
            self.owner.clone(),
            self.remaining,
            factory,
        ));
        self
    }
    /// Add process-local diagnostics to this owner's existing command chain.
    /// Their readers share its call/stop fence and retain the original platform
    /// shutdown, including final input disposal. Construction starts no work.
    pub fn with_diagnostics(
        mut self,
        library: impl Fn() -> Result<
            Arc<crate::media::EvsLibrary>,
            crate::native_codec::CodecLibraryError,
        > + Send
        + Sync
        + 'static,
        inventory: impl Fn() -> Result<IvyJson, crate::native_inventory::InventoryError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        let codecs = crate::native_codec_commands::NativeCodecCommands::new(
            self.owner.clone(),
            self.remaining,
            library,
        );
        self.remaining = Arc::new(
            crate::native_inventory::NativeInventoryCommands::with_reader(
                self.owner.clone(),
                Arc::new(codecs),
                inventory,
            ),
        );
        self
    }
    /// Default Desktop/input and MMDevice composition. Media, realtime and the
    /// authenticated codec loader remain explicit dependencies of the factory.
    #[cfg(all(windows, target_pointer_width = "64"))]
    pub fn windows_with_diagnostics(
        media: Arc<dyn crate::native_sip::NativeSipMediaFactory>,
        remaining: Arc<dyn NativeRuntime>,
        realtime: impl Fn(&crate::native_sip::NativeSipCall) -> Result<String, NativeSipError>
        + Send
        + Sync
        + 'static,
        library: impl Fn() -> Result<
            Arc<crate::media::EvsLibrary>,
            crate::native_codec::CodecLibraryError,
        > + Send
        + Sync
        + 'static,
    ) -> Self {
        Self::windows(media, remaining, realtime).with_diagnostics(
            library,
            crate::native_inventory::NativeInventoryCommands::read_windows,
        )
    }
    /// Compose the default Desktop/Micro owners without selecting a device or
    /// constructing media. Audio/realtime and other native commands remain with
    /// the supplied owner until their original call admission.
    #[cfg(all(windows, target_pointer_width = "64"))]
    pub fn windows(
        media: Arc<dyn crate::native_sip::NativeSipMediaFactory>,
        remaining: Arc<dyn NativeRuntime>,
        realtime: impl Fn(&crate::native_sip::NativeSipCall) -> Result<String, NativeSipError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        let desktop = crate::desktop::PhoneDesktopRuntime::windows();
        let input = crate::desktop::VoiceInputOwner::windows();
        let voice = crate::desktop::DesktopVoiceRuntime::new(desktop.clone(), input.clone());
        let owner =
            NativeSipOperations::with_desktop_runtime(media, desktop.clone(), Arc::new(voice));
        let remaining = crate::native_desktop::NativeDesktopCommands::with_runtime(
            owner.clone(),
            desktop,
            remaining,
        );
        Self::with_voice_input(owner, Arc::new(remaining), input, realtime).with_probe(Arc::new(
            crate::loopback_probe::windows::WindowsProbeFactory::default(),
        ))
    }
    /// Share the native owner's input selection with configure/status and its
    /// final disposal. Callers pass the same input owner to DesktopVoiceRuntime.
    /// The remaining runtime still owns audio/realtime and other platform work.
    pub fn with_voice_input(
        owner: NativeSipOperations,
        remaining: Arc<dyn NativeRuntime>,
        input: crate::desktop::VoiceInputOwner,
        realtime: impl Fn(&crate::native_sip::NativeSipCall) -> Result<String, NativeSipError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        let setup = input.clone();
        let view = input.clone();
        Self::new(
            owner,
            Arc::new(VoiceInputRuntime {
                input,
                remaining,
                shutdown: Mutex::new(None),
            }),
            move |micro| {
                let setup = setup.clone();
                Box::pin(async move {
                    if let Some(micro) = micro {
                        setup
                            .prepare_micro(micro)
                            .map_err(|_| SipHostError::CleanupUnconfirmed)?;
                    }
                    Ok(())
                })
            },
            NativeSipViews::new(
                move || {
                    let status = view
                        .micro_status()
                        .map_err(|_| SipHostError::CleanupUnconfirmed)?;
                    crate::native_sip_status::micro_json(status.as_ref())
                },
                realtime,
            ),
        )
    }
    pub fn new(
        owner: NativeSipOperations,
        remaining: Arc<dyn NativeRuntime>,
        setup: impl Fn(Option<NativeMicroSettings>) -> NativeMicroSetup + Send + Sync + 'static,
        views: NativeSipViews,
    ) -> Self {
        Self {
            owner,
            remaining,
            setup: Arc::new(setup),
            views,
            shutdown: Mutex::new(None),
        }
    }
    fn selected(
        &self,
        request: &NativeRequest,
    ) -> Result<(crate::sip::SipHostObservation, Option<String>), NativeSipError> {
        let state = self.owner.observation();
        if state.stopping {
            return Err(SipHostError::Stopping.into());
        }
        // The original handler selects a call before method-specific DTO
        // validation. Null-conditional host lookup does not decode callId
        // until the host exists. GetString on another JSON kind throws a
        // generic native error, independently of exact-field validation.
        let selected = if state.endpoint.is_some() {
            request
                .params()
                .get("callId")
                .map(selected_id)
                .transpose()?
        } else {
            None
        };
        Ok((state, selected))
    }
    fn desktop_start(&self, request: NativeRequest) -> NativeInvocationStart {
        let selected = (|| {
            let (state, _) = self.selected(&request)?;
            if !state.configured || state.endpoint.is_none() {
                return Err(SipHostError::NotReady.into());
            }
            let id = request
                .params()
                .get("callId")
                .ok_or(NativeConfigError::OutcomeUnknown)?;
            let id = selected_id(id)?;
            if !crate::native_rpc::guid_d(&id) {
                return Err(SipHostError::InvalidRequest.into());
            }
            self.owner
                .find(&id)
                .ok_or_else(|| NativeSipError::from(SipHostError::NotReady))
        })();
        let owner = self.owner.clone();
        Box::pin(async move {
            let call = match selected {
                Ok(call) => call,
                Err(error) => return Box::pin(async move { failed(error) }) as NativeInvocation,
            };
            if let Err(error) = call.constructed().await {
                return Box::pin(async move { failed(error.into()) }) as NativeInvocation;
            }
            // The original DesktopAsync/Invoke prefix runs under NativeRpc's
            // admission gate. Only its retained operation leaves that gate.
            match owner.desktop(&call, &request) {
                Ok(work) => Box::pin(async move {
                    match work.wait().await {
                        Ok(value) => crate::native_desktop::native_result_json(&value)
                            .map(NativeOutcome::result)
                            .unwrap_or_else(|_| NativeOutcome::error("operation_outcome_unknown")),
                        Err(error) => NativeOutcome::error(error.code()),
                    }
                }) as NativeInvocation,
                Err(code) => {
                    Box::pin(async move { NativeOutcome::error(code) }) as NativeInvocation
                }
            }
        })
    }
    fn lifecycle(&self, request: NativeRequest) -> Result<NativeInvocation, NativeSipError> {
        let (state, selected) = self.selected(&request)?;
        if matches!(request.method(), "status" | "heartbeat") {
            let fields = request
                .params()
                .as_object()
                .expect("validated native params");
            if fields.iter().any(|(key, _)| key != &"callId".into())
                || request.method() == "heartbeat" && !fields.is_empty()
            {
                return Err(SipHostError::InvalidRequest.into());
            }
            let snapshot = self.owner.status_snapshot(selected.as_deref())?;
            let views = self.views.clone();
            return Ok(Box::pin(async move {
                if let Some(call) = &snapshot.call {
                    if let Err(e) = call.constructed().await {
                        return failed(e.into());
                    }
                }
                match status_json(snapshot, &views) {
                    Ok(json) => NativeOutcome::result(json),
                    Err(e) => failed(e),
                }
            }));
        }
        if request.method() == "configure" {
            let setup = self.setup.clone();
            let work = self.owner.configure(&request, move |micro| setup(micro))?;
            return Ok(Box::pin(async move {
                match work.wait().await {
                    Ok(endpoint) => result(IvyJson::object([
                        ("configured", true.into()),
                        ("endpoint", endpoint.to_string().into()),
                    ])),
                    Err(error) => failed(error.into()),
                }
            }));
        }
        // The original configured/host latches precede optional setup. A thrown
        // Micro/registration constructor does not undo an already created host.
        if !state.configured || state.endpoint.is_none() {
            return Err(SipHostError::NotReady.into());
        }
        if realtime::method(request.method()) {
            return self.realtime(&request, selected);
        }
        let fields = request
            .params()
            .as_object()
            .expect("validated native params");
        if request.method() == "registration.reconnect" {
            if !fields.is_empty() {
                return Err(SipHostError::InvalidRequest.into());
            }
            let work = self.owner.reconnect_registration()?;
            return Ok(Box::pin(async move {
                match work.wait().await {
                    Ok(observation) => match registration_json(&observation) {
                        Ok(json) => NativeOutcome::result(json),
                        Err(_) => NativeOutcome::error("operation_outcome_unknown"),
                    },
                    Err(error) => failed(error.into()),
                }
            }));
        }
        if matches!(request.method(), "call.audio.prepare" | "call.audio.rebind") {
            let prepare = request.method() == "call.audio.prepare";
            let names = if prepare {
                ["callId", "settings", "desktop"]
            } else {
                ["callId", "desktop", "threadId"]
            };
            if fields.len() != 3
                || names
                    .iter()
                    .any(|name| request.params().get(name).is_none())
            {
                return Err(SipHostError::InvalidRequest.into());
            }
            let raw = request.raw_params();
            let settings = if prepare {
                NativeAudioSettings::parse(
                    request.params().get("settings").unwrap(),
                    crate::native_desktop::raw_field(request.params(), raw, "settings"),
                )?
            } else {
                None
            };
            let desktop = NativeAudioDesktop::parse(
                request.params().get("desktop").unwrap(),
                crate::native_desktop::raw_field(request.params(), raw, "desktop"),
            )?;
            let thread = if prepare {
                None
            } else {
                crate::native_desktop::nullable_string(
                    request.params().get("threadId").unwrap(),
                    "invalid_request",
                )
                .map_err(|_| NativeConfigError::InvalidRequest)?
            };
            let call = selected
                .and_then(|id| self.owner.find(&id))
                .ok_or(SipHostError::NotReady)?;
            let work = if prepare {
                let settings = settings.ok_or(NativeConfigError::InvalidRequest)?;
                let desktop = desktop
                    .ok_or(NativeConfigError::InvalidRequest)?
                    .validated()?;
                self.owner.prepare_audio(&call, settings, desktop)?
            } else {
                if desktop.is_none()
                    || thread
                        .as_deref()
                        .is_none_or(|t| !crate::native_rpc::guid_d(t))
                {
                    return Err(SipHostError::NotReady.into());
                }
                self.owner
                    .rebind_audio(&call, desktop.unwrap().validated()?)?
            };
            return Ok(Box::pin(async move {
                match work.wait().await {
                    Ok(Ok(json)) => NativeOutcome::result(json),
                    Ok(Err(error)) => failed(error),
                    Err(error) => failed(error.into()),
                }
            }));
        }
        if matches!(
            request.method(),
            "call.screening.prepare" | "call.screening.bridge" | "call.windows.connect"
        ) {
            if fields.len() != 2
                || request.params().get("callId").is_none()
                || request.params().get("settings").is_none()
            {
                return Err(SipHostError::InvalidRequest.into());
            }
            let call = selected
                .and_then(|id| self.owner.find(&id))
                .ok_or(SipHostError::NotReady)?;
            let raw = crate::native_rpc::root_values(request.raw_params());
            let index = fields
                .iter()
                .position(|(key, _)| key == &"settings".into())
                .expect("exact settings");
            let settings = request.params().get("settings").unwrap();
            let owner = self.owner.clone();
            if request.method() == "call.screening.prepare" {
                let settings = NativeScreeningSettings::parse(settings, raw[index])?;
                return Ok(Box::pin(async move {
                    if let Err(error) = call.constructed().await {
                        return failed(error.into());
                    }
                    match owner.prepare_screening(&call, settings) {
                        Ok(snapshot) => NativeOutcome::result(snapshot),
                        Err(error) => failed(error),
                    }
                }));
            }
            let settings = NativeAudioSettings::parse(settings, raw[index])?;
            let bridge = request.method() == "call.screening.bridge";
            return Ok(Box::pin(async move {
                if let Err(error) = call.constructed().await {
                    return failed(error.into());
                }
                match owner.connect_audio(&call, settings, bridge) {
                    Ok(work) => match work.wait().await {
                        Ok(Ok(snapshot)) => NativeOutcome::result(snapshot),
                        Ok(Err(error)) => failed(error),
                        Err(error) => failed(error.into()),
                    },
                    Err(error) => failed(error),
                }
            }));
        }
        if request.method() == "call.features" {
            if fields.len() != 2
                || request.params().get("callId").is_none()
                || request.params().get("challenge").is_none()
            {
                return Err(SipHostError::InvalidRequest.into());
            }
            let call = selected
                .and_then(|id| self.owner.find(&id))
                .ok_or(SipHostError::NotReady)?;
            let raw = crate::native_rpc::root_values(request.raw_params());
            let index = fields
                .iter()
                .position(|(key, _)| key == &"challenge".into())
                .expect("exact challenge");
            let challenge =
                CallAccessSettings::parse(request.params().get("challenge").unwrap(), raw[index])?;
            let snapshot = self.owner.configure_features(&call, challenge)?.json();
            return Ok(Box::pin(async move { NativeOutcome::result(snapshot) }));
        }
        if request.method() == "call.command.feedback" {
            if fields.len() != 3
                || ["callId", "commandSequence", "success"]
                    .iter()
                    .any(|name| request.params().get(name).is_none())
            {
                return Err(SipHostError::InvalidRequest.into());
            }
            let raw = crate::native_rpc::root_values(request.raw_params());
            let index = fields
                .iter()
                .position(|(key, _)| key == &"commandSequence".into())
                .expect("exact commandSequence");
            // System.Text.Json Int64 rejects exponent/fraction spellings and
            // overflow even when the parsed JSON number could be rounded.
            let sequence = raw[index]
                .parse::<i64>()
                .map_err(|_| SipHostError::InvalidRequest)?;
            let success = request
                .params()
                .get("success")
                .and_then(IvyJson::as_bool)
                .ok_or(SipHostError::InvalidRequest)?;
            let call = selected
                .and_then(|id| self.owner.find(&id))
                .ok_or(SipHostError::NotReady)?;
            let owner = self.owner.clone();
            return Ok(Box::pin(async move {
                if let Err(error) = call.constructed().await {
                    return failed(error.into());
                }
                match owner.command_feedback(&call, sequence, success) {
                    Ok(snapshot) => NativeOutcome::result(snapshot),
                    Err(error) => failed(error),
                }
            }));
        }
        if request.method() == "call.dial" {
            let input = crate::native_dial::NativeDial::parse(&request)?;
            let call = selected
                .and_then(|id| self.owner.find(&id))
                .ok_or(SipHostError::NotReady)?;
            let work = self.owner.dial(&call, input)?;
            return Ok(Box::pin(async move {
                match work.wait().await {
                    Ok(json) => NativeOutcome::result(json),
                    Err(error) => failed(error.into()),
                }
            }));
        }
        if request.method() == "call.answer" {
            if fields.len() != 2 || request.params().get("callId").is_none() {
                return Err(SipHostError::InvalidRequest.into());
            }
            let waiting = request
                .params()
                .get("waiting")
                .and_then(IvyJson::as_bool)
                .ok_or(SipHostError::InvalidRequest)?;
            let call = selected
                .and_then(|id| self.owner.find(&id))
                .ok_or(SipHostError::NotReady)?;
            let owner = self.owner.clone();
            return Ok(Box::pin(async move {
                if let Err(error) = call.constructed().await {
                    return failed(error.into());
                }
                match owner.answer(&call, waiting) {
                    Ok(work) => match work.wait().await {
                        Ok(json) => NativeOutcome::result(json),
                        Err(error) => failed(error.into()),
                    },
                    Err(error) => failed(error),
                }
            }));
        }
        if fields.len() != 1 || request.params().get("callId").is_none() {
            return Err(SipHostError::InvalidRequest.into());
        }
        if request.method() == "call.prepare" {
            let id = selected.ok_or(SipHostError::InvalidRequest)?;
            // Reserve synchronously under NativeDispatcher admission, before
            // returning the observer-independent construction future.
            let call = self.owner.prepare(id)?;
            return Ok(Box::pin(async move {
                match call.prepared().await {
                    Ok(()) => match call_json(&call) {
                        Ok(value) => NativeOutcome::result(value),
                        Err(error) => failed(error),
                    },
                    Err(error) => failed(error.into()),
                }
            }));
        }
        let call = selected.and_then(|id| self.owner.find(&id)).ok_or(
            if request.method() == "call.claim" {
                NativeSipError::OfferExpired
            } else {
                SipHostError::NotReady.into()
            },
        )?;
        if request.method() == "call.codec.upgrade" {
            let owner = self.owner.clone();
            return Ok(Box::pin(async move {
                if let Err(error) = call.constructed().await {
                    return failed(error.into());
                }
                match owner.upgrade_codec(&call) {
                    Ok(work) => match work.wait().await {
                        Ok(snapshot) => NativeOutcome::result(snapshot),
                        Err(error) => failed(error.into()),
                    },
                    Err(error) => failed(error),
                }
            }));
        }
        if request.method() == "call.waiting.end" {
            let owner = self.owner.clone();
            return Ok(Box::pin(async move {
                if let Err(error) = call.constructed().await {
                    return failed(error.into());
                }
                match owner.end_waiting(&call) {
                    Ok(snapshot) => NativeOutcome::result(snapshot),
                    Err(error) => failed(error),
                }
            }));
        }
        if request.method() == "call.claim" {
            let owner = self.owner.clone();
            return Ok(Box::pin(async move {
                if let Err(error) = call.constructed().await {
                    return failed(error.into());
                }
                match owner.claim(&call).and_then(|()| call_json(&call)) {
                    Ok(snapshot) => NativeOutcome::result(snapshot),
                    Err(error) => failed(error),
                }
            }));
        }
        if request.method() == "call.hangup" {
            self.owner.hangup(&call)?;
            let outcome = match call_json(&call) {
                Ok(value) => NativeOutcome::result(value),
                Err(error) => failed(error),
            };
            return Ok(Box::pin(async move { outcome }));
        }
        let id = call.observation().id;
        let release = self.owner.release(&call)?;
        Ok(Box::pin(async move {
            match release.wait().await {
                Ok(()) => result(IvyJson::object([
                    ("callId", id.into()),
                    ("released", true.into()),
                ])),
                Err(error) => failed(error.into()),
            }
        }))
    }
}
fn selected_id(value: &IvyJson) -> Result<String, NativeSipError> {
    if value.is_null() {
        // GetString accepts null, but the original dictionary lookup rejects it.
        return Err(SipHostError::InvalidRequest.into());
    }
    value
        .as_str()
        .map_err(|_| crate::native_config::NativeConfigError::OutcomeUnknown.into())
}
fn result(value: IvyJson) -> NativeOutcome {
    match stringify(&value, crate::ipc::FRAME_BYTES) {
        Ok(value) => NativeOutcome::result(value),
        Err(_) => NativeOutcome::error("operation_outcome_unknown"),
    }
}
fn failed(error: NativeSipError) -> NativeOutcome {
    NativeOutcome::error(error.code())
}
fn lifecycle_method(method: &str) -> bool {
    matches!(
        method,
        "configure"
            | "status"
            | "heartbeat"
            | "call.prepare"
            | "call.dial"
            | "call.claim"
            | "call.answer"
            | "call.hangup"
            | "call.release"
            | "registration.reconnect"
            | "call.features"
            | "call.waiting.end"
            | "call.command.feedback"
            | "call.screening.prepare"
            | "call.screening.bridge"
            | "call.windows.connect"
            | "call.audio.prepare"
            | "call.audio.rebind"
            | "call.codec.upgrade"
            | "call.realtime.prepare"
            | "call.realtime.answer"
            | "call.realtime.stop"
            | "call.desktop.launch"
            | "call.desktop.startVoice"
            | "call.desktop.pauseVoice"
            | "call.desktop.resumeVoice"
            | "call.desktop.stopVoice"
    )
}
impl NativeHandler for NativeSipCommands {
    fn start(&self, request: NativeRequest) -> NativeInvocationStart {
        if desktop_method(request.method()) {
            return self.desktop_start(request);
        }
        if !lifecycle_method(request.method()) {
            return self.remaining.start(request);
        }
        let synchronous = matches!(
            request.method(),
            "configure"
                | "status"
                | "heartbeat"
                | "call.prepare"
                | "call.claim"
                | "call.features"
                | "call.waiting.end"
                | "call.command.feedback"
                | "call.hangup"
                | "call.screening.prepare"
        );
        let action = self.invoke(request);
        Box::pin(async move {
            if synchronous {
                // These original branches return their result before InvokeAsync
                // reaches an await. Preserve that invocation/admission boundary;
                // dial/answer/release and other asynchronous actions keep running
                // independently after their synchronous entry returns.
                let outcome = action.await;
                Box::pin(async move { outcome }) as NativeInvocation
            } else {
                action
            }
        })
    }
    fn cleanup_slots(&self) -> usize {
        (self.owner.observation().calls.len() * 4).max(4)
    }
    fn invoke(&self, request: NativeRequest) -> NativeInvocation {
        if desktop_method(request.method()) {
            let start = self.desktop_start(request);
            return Box::pin(async move { start.await.await });
        }
        if !lifecycle_method(request.method()) {
            return self.remaining.invoke(request);
        }
        match self.lifecycle(request) {
            Ok(work) => work,
            Err(error) => Box::pin(async move { failed(error) }),
        }
    }
}
fn desktop_method(method: &str) -> bool {
    matches!(
        method,
        "call.desktop.launch"
            | "call.desktop.startVoice"
            | "call.desktop.pauseVoice"
            | "call.desktop.resumeVoice"
            | "call.desktop.stopVoice"
    )
}
impl NativeRuntime for NativeSipCommands {
    fn shutdown(&self) -> NativeShutdown {
        let mut saved = self.shutdown.lock().unwrap();
        let original = saved
            .get_or_insert_with(|| {
                // Revoke SIP synchronously, even if the returned observer leaves.
                let sip = self.owner.close();
                let remaining = self.remaining.clone();
                retained(async move {
                    let sip_result = sip.wait().await;
                    // The original finally disposes Desktop/Micro even if SIP
                    // cleanup fails. Completion of either fence is never inferred.
                    let other_result = remaining.shutdown().await;
                    sip_result.and(other_result.map_err(|_| SipHostError::CleanupUnconfirmed))
                })
            })
            .clone();
        Box::pin(async move {
            original
                .wait()
                .await
                .map_err(|_| NativeShutdownError::OutcomeUnknown)
        })
    }
}

// NativeSipOperations.DisposeCoreAsync's last finally disposes the one input
// owner after the host and other platform work, even when earlier cleanup fails.
// Keep that disposal retained and memoized without changing per-call failures.
struct VoiceInputRuntime {
    input: crate::desktop::VoiceInputOwner,
    remaining: Arc<dyn NativeRuntime>,
    shutdown: Mutex<Option<NativeSipOperation<()>>>,
}
impl NativeHandler for VoiceInputRuntime {
    fn invoke(&self, request: NativeRequest) -> NativeInvocation {
        self.remaining.invoke(request)
    }
    fn start(&self, request: NativeRequest) -> NativeInvocationStart {
        self.remaining.start(request)
    }
    fn cleanup_slots(&self) -> usize {
        self.remaining.cleanup_slots()
    }
}
impl NativeRuntime for VoiceInputRuntime {
    fn shutdown(&self) -> NativeShutdown {
        let mut saved = self.shutdown.lock().unwrap();
        let original = saved
            .get_or_insert_with(|| {
                let input = self.input.clone();
                let remaining = self.remaining.clone();
                retained(async move {
                    let previous = match catch_unwind(AssertUnwindSafe(|| remaining.shutdown())) {
                        Ok(mut work) => {
                            std::future::poll_fn(|cx| {
                                match catch_unwind(AssertUnwindSafe(|| work.as_mut().poll(cx))) {
                                    Ok(value) => value,
                                    Err(_) => Poll::Ready(Err(NativeShutdownError::OutcomeUnknown)),
                                }
                            })
                            .await
                        }
                        Err(_) => Err(NativeShutdownError::OutcomeUnknown),
                    };
                    let input_result = match input.close() {
                        Ok(work) => work
                            .wait()
                            .await
                            .map_err(|_| SipHostError::CleanupUnconfirmed),
                        Err(_) => Err(SipHostError::CleanupUnconfirmed),
                    };
                    previous
                        .map_err(|_| SipHostError::CleanupUnconfirmed)
                        .and(input_result)
                })
            })
            .clone();
        Box::pin(async move {
            original
                .wait()
                .await
                .map_err(|_| NativeShutdownError::OutcomeUnknown)
        })
    }
}

#[cfg(all(test, windows))]
#[path = "../../../../tests/rust/phone_native_voice_input.rs"]
mod voice_input_tests;
