//! Call-bound Desktop admission and the media/Voice disposal join.
use super::*;
use crate::{
    desktop::{
        CallDesktopSession, DesktopCallAction, DesktopCallOperation, NativeVoiceInput,
        validate_application,
    },
    native_desktop::{desktop_identity, exact, nullable_string, raw_field},
    sip::DelayedInviteMedia,
};
use ivy_contracts::IvyJson;

#[derive(Default)]
struct Resources {
    desktop: Option<CallDesktopSession>,
    desktop_closing: Option<DesktopCallOperation<()>>,
    closing: Option<NativeSipOperation<()>>,
    closed: bool,
}
#[derive(Default)]
pub(super) struct CallResources(Mutex<Resources>);
impl CallResources {
    pub(super) fn selected_desktop(&self) -> Option<CallDesktopSession> {
        self.0.lock().unwrap().desktop.clone()
    }
    pub(super) fn retains_desktop(&self, selected: &CallDesktopSession) -> bool {
        self.0
            .lock()
            .unwrap()
            .desktop
            .as_ref()
            .is_some_and(|d| d.same_owner(selected))
    }
    pub(super) fn has_desktop(&self) -> bool {
        self.0.lock().unwrap().desktop.is_some()
    }
    fn start_desktop_close(resources: &mut Resources) -> Option<DesktopCallOperation<()>> {
        if !resources.closed {
            resources.closed = true;
            resources.desktop_closing = resources.desktop.as_ref().map(CallDesktopSession::close);
        }
        resources.desktop_closing.clone()
    }
    pub(super) fn begin_desktop_close(&self) {
        Self::start_desktop_close(&mut self.0.lock().unwrap());
    }
    fn close(&self, media: SipMediaSession) -> NativeSipOperation<()> {
        let mut resources = self.0.lock().unwrap();
        if let Some(work) = &resources.closing {
            return work.clone();
        }
        let desktop = Self::start_desktop_close(&mut resources);
        let work = retained(async move {
            // Like Task.WhenAll in SipCall.DisposeCoreAsync, neither failure
            // abandons the other owner, and no host slot retires before both.
            let (media, desktop) = tokio::join!(media.close(), async move {
                match desktop {
                    Some(work) => work
                        .wait()
                        .await
                        .map_err(|_| SipHostError::CleanupUnconfirmed),
                    None => Ok(()),
                }
            });
            media
                .map_err(|_| SipHostError::CleanupUnconfirmed)
                .and(desktop)
        });
        resources.closing = Some(work.clone());
        work
    }
}
pub(super) struct CallMedia {
    pub(super) media: SipMediaSession,
    pub(super) resources: Arc<CallResources>,
}
impl DelayedInviteMedia for CallMedia {
    fn is_closed(&self) -> bool {
        self.media.is_closed()
    }
    fn failed(&self) -> bool {
        self.media.failed()
    }
    fn initial_offer(&self) -> Option<String> {
        self.media.initial_offer()
    }
    fn negotiate_offer(&self, offer: &str) -> Option<String> {
        self.media.negotiate_offer(offer)
    }
    fn accept_answer(&self, answer: &str) -> bool {
        self.media.accept_answer(answer)
    }
    fn rejected_answer(&self, offer: &str) -> Option<String> {
        self.media.rejected_answer(offer)
    }
    fn start(&self) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
        self.media.start()
    }
}
impl HostedSipMedia for CallMedia {
    fn request_close(&self) {
        self.media.request_close();
    }
    fn close(&self) -> Pin<Box<dyn Future<Output = Result<(), SipHostError>> + Send + '_>> {
        let work = self.resources.close(self.media.clone());
        Box::pin(async move { work.wait().await })
    }
}
impl NativeSipOperations {
    pub(crate) fn desktop(
        &self,
        call: &NativeSipCall,
        request: &NativeRequest,
    ) -> Result<DesktopCallOperation<IvyJson>, &'static str> {
        {
            let state = self.0.state.lock().unwrap();
            Self::original(&state, call).map_err(|_| "runtime_not_ready")?;
        }
        let mut resources = call.0.resources.0.lock().unwrap();
        if resources.closed || call.0.releasing.lock().unwrap().is_some() {
            return Err("runtime_stopping");
        }
        if resources.desktop.is_none() {
            let (runtime, voice) = self.0.desktop.as_ref().ok_or("runtime_not_ready")?;
            let original = Arc::downgrade(&call.0);
            let current = Arc::new(move |cleanup| {
                if cleanup {
                    return Ok(true);
                }
                let Some(call) = original.upgrade().map(NativeSipCall) else {
                    return Ok(false);
                };
                let observed = call.observation();
                Ok(call.0.releasing.lock().unwrap().is_none()
                    && !observed.ending
                    && observed.phase != SipHostCallPhase::OutcomeUnknown
                    && call.media().is_some_and(|m| !m.is_closed()))
            });
            resources.desktop = Some(
                CallDesktopSession::new(
                    call.observation().id,
                    runtime.clone(),
                    voice.clone(),
                    current,
                    call.media().map(|m| m.call_audio().clone()),
                )
                .map_err(|e| e.code())?,
            );
        }
        // Original SipCall creates/caches the Desktop owner before decoding the
        // action, so even malformed first actions retain that same owner.
        let action = decode(request)?;
        resources
            .desktop
            .as_ref()
            .unwrap()
            .invoke(Some(&call.observation().id), action)
            .map_err(|e| e.code())
    }
}
fn decode(request: &NativeRequest) -> Result<DesktopCallAction, &'static str> {
    let params = request.params();
    let raw = request.raw_params();
    if request.method() == "call.desktop.launch" {
        exact(params, &["callId", "application"])?;
        let config = params.get("application").unwrap();
        if !matches!(config, IvyJson::Object(_)) {
            return Err("operation_outcome_unknown");
        }
        exact(config, &["appUserModelId", "startIfMissing"])?;
        let application =
            nullable_string(config.get("appUserModelId").unwrap(), "invalid_request")?;
        let start_if_missing = config
            .get("startIfMissing")
            .unwrap()
            .as_bool()
            .ok_or("invalid_request")?;
        let application = application.ok_or("invalid_request")?;
        validate_application(&application).map_err(|_| "invalid_request")?;
        return Ok(DesktopCallAction::Launch {
            application,
            start_if_missing,
        });
    }
    let transition = matches!(
        request.method(),
        "call.desktop.pauseVoice" | "call.desktop.resumeVoice"
    );
    exact(
        params,
        if transition {
            &["callId", "desktop", "voiceInput", "generation"]
        } else {
            &["callId", "desktop", "voiceInput"]
        },
    )?;
    // GetInt32 runs directly before Desktop/input deserialization, so wrong
    // kinds, fractions, exponent spellings and overflow are native errors.
    let generation = if transition {
        Some(
            raw_field(params, raw, "generation")
                .parse::<i32>()
                .map_err(|_| "operation_outcome_unknown")?,
        )
    } else {
        None
    };
    let desktop = desktop_identity(
        params.get("desktop").unwrap(),
        raw_field(params, raw, "desktop"),
    )?;
    let input = NativeVoiceInput::read(
        params.get("voiceInput").unwrap(),
        raw_field(params, raw, "voiceInput"),
    )
    .map_err(|e| e.code())?;
    Ok(match request.method() {
        "call.desktop.startVoice" => DesktopCallAction::StartVoice { desktop, input },
        "call.desktop.stopVoice" => DesktopCallAction::StopVoice { desktop, input },
        "call.desktop.pauseVoice" => DesktopCallAction::PauseVoice {
            desktop,
            input,
            generation: generation.unwrap(),
        },
        "call.desktop.resumeVoice" => DesktopCallAction::ResumeVoice {
            desktop,
            input,
            generation: generation.unwrap(),
        },
        _ => return Err("invalid_request"),
    })
}
