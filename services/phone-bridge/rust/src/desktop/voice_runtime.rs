//! Compose the native owner/input primitives without duplicating Voice policy.
use super::{
    ActionResult, CallVoiceRuntime, CurrentDesktopOwner, DesktopActionError,
    DesktopCaptureCorrelation, DesktopOwner, KeyStroke, KeyboardInput, NativeVoiceInput,
    PhoneDesktopRuntime, VoiceDispatchResult, VoiceInputOwner, VoiceInputPlan, dispatch_hotkey,
    dispatch_voice, prepare_input_focus,
};
use crate::{
    media::AudioDesktopIdentity,
    micro::{MicroClose, MicroGestureResult, MicroInputResult},
};
use ivy_contracts::json::JsString;
use std::{io, sync::Arc};

type OwnerFactory =
    dyn Fn(&AudioDesktopIdentity) -> ActionResult<Box<dyn DesktopOwner>> + Send + Sync;
type KeyboardFactory = dyn Fn() -> ActionResult<Box<dyn KeyboardInput>> + Send + Sync;
type Exited = dyn Fn(&AudioDesktopIdentity) -> ActionResult<bool> + Send + Sync;

#[derive(Clone)]
pub struct DesktopVoiceRuntime {
    desktop: PhoneDesktopRuntime,
    input: VoiceInputOwner,
    owner: Arc<OwnerFactory>,
    keyboard: Arc<KeyboardFactory>,
    exited: Arc<Exited>,
}
impl DesktopVoiceRuntime {
    /// The native owner supplies its actual Micro factory through VoiceInputOwner.
    /// It is shared across calls; constructing a bridge does not select a device.
    pub fn new(desktop: PhoneDesktopRuntime, input: VoiceInputOwner) -> Self {
        Self {
            desktop,
            input,
            owner: Arc::new(platform_owner),
            keyboard: Arc::new(platform_keyboard),
            exited: Arc::new(platform_exited),
        }
    }
    pub fn with_owner(
        mut self,
        owner: impl Fn(&AudioDesktopIdentity) -> ActionResult<Box<dyn DesktopOwner>>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.owner = Arc::new(owner);
        self
    }
    pub fn with_keyboard(
        mut self,
        keyboard: impl Fn() -> ActionResult<Box<dyn KeyboardInput>> + Send + Sync + 'static,
    ) -> Self {
        self.keyboard = Arc::new(keyboard);
        self
    }
    pub fn with_exited(
        mut self,
        exited: impl Fn(&AudioDesktopIdentity) -> ActionResult<bool> + Send + Sync + 'static,
    ) -> Self {
        self.exited = Arc::new(exited);
        self
    }
    pub fn close(&self) -> MicroInputResult<MicroClose> {
        self.input.close()
    }
}
impl CallVoiceRuntime for DesktopVoiceRuntime {
    fn is_current(&self, desktop: &AudioDesktopIdentity) -> ActionResult<bool> {
        (self.owner)(desktop)?.is_current()
    }
    fn has_exited(&self, desktop: &AudioDesktopIdentity) -> ActionResult<bool> {
        (self.exited)(desktop)
    }
    fn capture(
        &self,
        desktop: &AudioDesktopIdentity,
        endpoint: &JsString,
    ) -> ActionResult<Option<DesktopCaptureCorrelation>> {
        self.desktop.voice_capture(desktop, endpoint)
    }
    fn select(
        &self,
        input: &NativeVoiceInput,
        wait: bool,
        original: &dyn Fn() -> ActionResult<bool>,
    ) -> ActionResult<VoiceInputPlan> {
        self.input
            .select(input, wait, &|| original().map_err(Into::into))
            .map_err(|_| DesktopActionError::Unavailable)
    }
    fn prepare_focus(
        &self,
        plan: &VoiceInputPlan,
        input: &NativeVoiceInput,
        desktop: &AudioDesktopIdentity,
        original: &dyn Fn() -> ActionResult<bool>,
    ) -> ActionResult<bool> {
        prepare_input_focus(
            plan,
            input,
            original,
            &|| self.is_current(desktop),
            &mut || {
                let mut owner = Owner((self.owner)(desktop)?);
                CurrentDesktopOwner {
                    owner: &mut owner,
                    current: original,
                }
                .try_focus()
            },
        )
    }
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
    ) -> ActionResult<VoiceDispatchResult> {
        dispatch_voice(
            plan,
            input,
            cleanup,
            &|| self.is_current(desktop),
            before,
            release,
            &mut |hotkey, original, prepared| {
                // Preserve factory order and construct thread-bound owners at use.
                let mut owner = Owner((self.owner)(desktop)?);
                let mut keyboard = Keyboard((self.keyboard)()?);
                dispatch_hotkey(
                    &mut CurrentDesktopOwner {
                        owner: &mut owner,
                        current: original,
                    },
                    &mut keyboard,
                    hotkey.modifiers(),
                    hotkey.key_code(),
                    hotkey.focus_before(),
                    prepared,
                )
            },
            prior_micro,
            was_dictation,
        )
    }
}
struct Owner(Box<dyn DesktopOwner>);
impl DesktopOwner for Owner {
    fn is_current(&mut self) -> ActionResult<bool> {
        self.0.is_current()
    }
    fn try_focus(&mut self) -> ActionResult<bool> {
        self.0.try_focus()
    }
    fn is_focused(&mut self) -> ActionResult<bool> {
        self.0.is_focused()
    }
}
struct Keyboard(Box<dyn KeyboardInput>);
impl KeyboardInput for Keyboard {
    fn is_idle(&mut self) -> ActionResult<bool> {
        self.0.is_idle()
    }
    fn send(&mut self, keys: &[KeyStroke]) -> ActionResult<i32> {
        self.0.send(keys)
    }
}

#[cfg(all(windows, target_pointer_width = "64"))]
fn platform_owner(desktop: &AudioDesktopIdentity) -> ActionResult<Box<dyn DesktopOwner>> {
    Ok(Box::new(super::WindowsDesktopProcessOwner::new(
        desktop.pid(),
        desktop.creation_ticks(),
        desktop.image(),
    )?))
}
#[cfg(not(all(windows, target_pointer_width = "64")))]
fn platform_owner(_: &AudioDesktopIdentity) -> ActionResult<Box<dyn DesktopOwner>> {
    Err(DesktopActionError::Platform("Windows x64 required."))
}
#[cfg(all(windows, target_pointer_width = "64"))]
fn platform_keyboard() -> ActionResult<Box<dyn KeyboardInput>> {
    Ok(Box::new(super::WindowsKeyboardInput::new()))
}
#[cfg(not(all(windows, target_pointer_width = "64")))]
fn platform_keyboard() -> ActionResult<Box<dyn KeyboardInput>> {
    Err(DesktopActionError::Platform("Windows x64 required."))
}

trait ExitObservation {
    fn has_exited(&mut self) -> io::Result<bool>;
    fn created_filetime(&mut self) -> io::Result<u64>;
}
fn process_exited<P: ExitObservation>(
    desktop: &AudioDesktopIdentity,
    open: impl FnOnce(u32) -> io::Result<Option<P>>,
) -> bool {
    // Missing PID proves exit; an inaccessible/disappearing StartTime read does
    // not. Each observation is fresh and checks only the original PID/start.
    let observe = || -> io::Result<bool> {
        let Some(mut process) = open(desktop.pid() as u32)? else {
            return Ok(true);
        };
        if process.has_exited()? {
            return Ok(true);
        }
        let ticks = process
            .created_filetime()?
            .checked_add(504_911_232_000_000_000)
            .filter(|v| *v <= 3_155_378_975_999_999_999);
        // DateTime.FromFileTime throws ArgumentOutOfRangeException here. The
        // original exit predicate catches ArgumentException as a proved exit,
        // separately from ordinary observation errors above.
        let Some(ticks) = ticks else { return Ok(true) };
        Ok(ticks != desktop.creation_ticks() as u64)
    };
    observe().unwrap_or(false)
}
#[cfg(all(windows, target_pointer_width = "64"))]
impl ExitObservation
    for ivy_host_runtime::platform::windows_process_inspection::ProcessObservation
{
    fn has_exited(&mut self) -> io::Result<bool> {
        Self::has_exited(self)
    }
    fn created_filetime(&mut self) -> io::Result<u64> {
        Self::created_filetime(self)
    }
}
#[cfg(all(windows, target_pointer_width = "64"))]
fn platform_exited(desktop: &AudioDesktopIdentity) -> ActionResult<bool> {
    Ok(process_exited(
        desktop,
        ivy_host_runtime::platform::windows_process_inspection::ProcessObservation::by_id,
    ))
}
#[cfg(not(all(windows, target_pointer_width = "64")))]
fn platform_exited(_: &AudioDesktopIdentity) -> ActionResult<bool> {
    Err(DesktopActionError::Platform("Windows x64 required."))
}

#[cfg(all(test, windows))]
#[path = "../../../../../tests/rust/phone_voice_runtime.rs"]
mod tests;
