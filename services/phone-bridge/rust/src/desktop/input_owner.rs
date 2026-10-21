//! The native owner's one Micro selection and its original retry/disposal state.
use super::{NativeVoiceInput, VoiceInputPlan};
use crate::{
    micro::{MicroAttachmentStatus, MicroClose, MicroInputResult, MicroVoiceInput},
    native_config::NativeMicroSettings,
};
use std::{
    cell::RefCell,
    sync::{Arc, Mutex},
    time::Duration,
};

/// Only the original IO/access and socket exception classes become status.
/// Other factory failures propagate, after retaining the attempted selection.
pub enum MicroFactoryError {
    ClientUnavailable,
    AttachmentConflict,
    Other(Box<dyn std::error::Error + Send + Sync>),
}
pub type MicroFactoryResult = Result<Option<Arc<dyn MicroVoiceInput>>, MicroFactoryError>;
pub trait VoiceInputClock: Send + Sync {
    fn retry_time_millis(&self) -> i64;
    fn timestamp(&self) -> Duration;
    fn elapsed(&self, since: Duration) -> Duration {
        self.timestamp().saturating_sub(since)
    }
    fn sleep(&self, milliseconds: u64) -> MicroInputResult<()>;
}
#[cfg(windows)]
struct WindowsInputClock(std::time::Instant);
#[cfg(windows)]
impl VoiceInputClock for WindowsInputClock {
    fn retry_time_millis(&self) -> i64 {
        // The original Environment.TickCount64 uptime clock, separate from
        // Stopwatch's high-resolution elapsed time used by the readiness wait.
        unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() as i64 }
    }
    fn timestamp(&self) -> Duration {
        self.0.elapsed()
    }
    fn sleep(&self, milliseconds: u64) -> MicroInputResult<()> {
        std::thread::sleep(Duration::from_millis(milliseconds));
        Ok(())
    }
}
type Factory = dyn Fn(&NativeMicroSettings) -> MicroFactoryResult + Send + Sync;
#[derive(Default)]
struct State {
    selected: Option<NativeMicroSettings>,
    device: Option<Arc<dyn MicroVoiceInput>>,
    attempted: bool,
    error: Option<&'static str>,
    retry_after: i64,
    closed: bool,
    disposal: Option<MicroClose>,
}
struct Owner {
    input_sync: Mutex<()>,
    state: Mutex<State>,
    factory: Arc<Factory>,
    time: Arc<dyn VoiceInputClock>,
}
#[derive(Clone)]
pub struct VoiceInputOwner(Arc<Owner>);
thread_local! { static ENTERED: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) }; }
impl VoiceInputOwner {
    #[cfg(windows)]
    pub fn new(
        factory: impl Fn(&NativeMicroSettings) -> MicroFactoryResult + Send + Sync + 'static,
    ) -> Self {
        Self::with_clock(
            factory,
            Arc::new(WindowsInputClock(std::time::Instant::now())),
        )
    }
    pub fn with_clock(
        factory: impl Fn(&NativeMicroSettings) -> MicroFactoryResult + Send + Sync + 'static,
        time: Arc<dyn VoiceInputClock>,
    ) -> Self {
        Self(Arc::new(Owner {
            input_sync: Mutex::new(()),
            state: Mutex::new(State::default()),
            factory: Arc::new(factory),
            time,
        }))
    }
    // C# Monitor admits same-thread reentry into this owner. Never hold the data
    // mutex across device/factory callbacks; other threads still share its gate.
    fn synchronized<T>(&self, work: impl FnOnce() -> T) -> T {
        let key = Arc::as_ptr(&self.0) as usize;
        ENTERED.with(|entered| {
            if entered.borrow().contains(&key) {
                return work();
            }
            let _gate = self.0.input_sync.lock().unwrap_or_else(|e| e.into_inner());
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
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn micro_status(&self) -> MicroInputResult<Option<MicroAttachmentStatus>> {
        self.synchronized(|| {
            self.retry_conflicting()?;
            let (device, attempted, error) = {
                let s = self.state();
                (s.device.clone(), s.attempted, s.error)
            };
            match device {
                Some(device) => device.status().map(Some),
                None => Ok(attempted.then(|| MicroAttachmentStatus {
                    state: "unavailable".into(),
                    generation: None,
                    error_code: Some(error.unwrap_or("micro_client_unavailable").into()),
                })),
            }
        })
    }
    pub fn prepare_micro(&self, micro: NativeMicroSettings) -> MicroInputResult<()> {
        self.select(
            &NativeVoiceInput::new(Some(micro), None, false),
            false,
            &|| Ok(true),
        )
        .map(|_| ())
    }
    pub fn select(
        &self,
        settings: &NativeVoiceInput,
        wait_for_ready: bool,
        original: &dyn Fn() -> MicroInputResult<bool>,
    ) -> MicroInputResult<VoiceInputPlan> {
        let Some(micro) = settings.micro() else {
            return Ok(if settings.hotkey_fallback().is_some() {
                VoiceInputPlan::Hotkey
            } else {
                empty()
            });
        };
        let admitted = self.synchronized(|| {
            let attempt = {
                let s = self.state();
                if s.closed {
                    return Ok(None);
                }
                if s.selected
                    .as_ref()
                    .is_some_and(|previous| previous != micro)
                {
                    return Err(std::io::Error::other(
                        "Micro configuration changed within the native owner.",
                    )
                    .into());
                }
                !s.attempted
                    || s.device.is_none()
                        && s.error == Some("micro_attachment_conflict")
                        && self.0.time.retry_time_millis() >= s.retry_after
            };
            if attempt {
                {
                    let mut s = self.state();
                    s.selected = Some(micro.clone());
                    s.attempted = true;
                }
                match (self.0.factory)(micro) {
                    // Select intentionally does not clear an earlier error. The
                    // separate status retry does, even for a null factory result.
                    Ok(device) => self.state().device = device,
                    Err(MicroFactoryError::ClientUnavailable) => {
                        self.state().error = Some("micro_client_unavailable")
                    }
                    Err(MicroFactoryError::AttachmentConflict) => {
                        let after = self.0.time.retry_time_millis().wrapping_add(1000);
                        let mut s = self.state();
                        s.error = Some("micro_attachment_conflict");
                        s.retry_after = after;
                    }
                    Err(MicroFactoryError::Other(error)) => return Err(error),
                }
            }
            Ok(Some(self.state().device.clone()))
        })?;
        let Some(device) = admitted else {
            return Ok(empty());
        };
        self.wait_for_input(device, settings, wait_for_ready, original)
    }
    fn wait_for_input(
        &self,
        device: Option<Arc<dyn MicroVoiceInput>>,
        settings: &NativeVoiceInput,
        wait_for_ready: bool,
        original: &dyn Fn() -> MicroInputResult<bool>,
    ) -> MicroInputResult<VoiceInputPlan> {
        let began = self.0.time.timestamp();
        loop {
            if !original()? {
                return Ok(empty());
            }
            let Some(device) = &device else { break };
            let status = device.status()?;
            if status.state == "ready" && status.generation.is_some() {
                return Ok(VoiceInputPlan::Micro {
                    generation: status.generation,
                    device: Some(device.clone()),
                });
            }
            if !wait_for_ready
                || !matches!(status.state.as_str(), "starting" | "attached")
                || self.0.time.elapsed(began) >= Duration::from_secs(10)
            {
                break;
            }
            self.0.time.sleep(50)?;
        }
        Ok(if settings.hotkey_fallback().is_some() {
            VoiceInputPlan::Hotkey
        } else {
            VoiceInputPlan::Micro {
                generation: None,
                device,
            }
        })
    }
    fn retry_conflicting(&self) -> MicroInputResult<()> {
        let selected = {
            let s = self.state();
            if s.closed
                || s.device.is_some()
                || s.selected.is_none()
                || s.error != Some("micro_attachment_conflict")
                || self.0.time.retry_time_millis() < s.retry_after
            {
                return Ok(());
            }
            s.selected.clone().unwrap()
        };
        match (self.0.factory)(&selected) {
            Ok(device) => {
                let mut s = self.state();
                s.device = device;
                s.error = None;
            }
            Err(MicroFactoryError::AttachmentConflict) => {
                self.state().retry_after = self.0.time.retry_time_millis().wrapping_add(1000)
            }
            Err(MicroFactoryError::ClientUnavailable) => {
                self.state().error = Some("micro_client_unavailable")
            }
            Err(MicroFactoryError::Other(error)) => return Err(error),
        }
        Ok(())
    }
    pub fn close(&self) -> MicroInputResult<MicroClose> {
        self.synchronized(|| {
            let device = {
                let mut s = self.state();
                s.closed = true;
                if let Some(disposal) = &s.disposal {
                    return Ok(disposal.clone());
                }
                s.device.clone()
            };
            let disposal = match device {
                Some(device) => device.close()?,
                None => MicroClose::completed(Ok(())),
            };
            self.state().disposal = Some(disposal.clone());
            Ok(disposal)
        })
    }
}
fn empty() -> VoiceInputPlan {
    VoiceInputPlan::Micro {
        generation: None,
        device: None,
    }
}

#[cfg(all(test, windows))]
#[path = "../../../../../tests/rust/phone_voice_input_owner.rs"]
mod tests;
