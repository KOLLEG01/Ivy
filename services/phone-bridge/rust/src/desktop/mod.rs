//! Desktop control evidence is an input to the live call owner, never authority
//! to identify a task, invoke a control or grant audio by itself.
mod actions;
mod audio_process;
mod call_session;
mod call_voice;
mod capture;
mod capture_owner;
mod controls;
mod input_owner;
mod runtime;
mod voice;
mod voice_input;
mod voice_runtime;
mod voice_session;
#[cfg(all(windows, target_pointer_width = "64"))]
mod windows;
#[cfg(all(windows, target_pointer_width = "64"))]
mod windows_activation;
#[cfg(all(windows, target_pointer_width = "64"))]
mod windows_application;
#[cfg(all(windows, target_pointer_width = "64"))]
mod windows_audio_process;
#[cfg(all(windows, target_pointer_width = "64"))]
mod windows_capture;
#[cfg(all(windows, target_pointer_width = "64"))]
mod windows_input;
#[cfg(all(windows, target_pointer_width = "64"))]
mod windows_owner;

pub use actions::{
    ActionResult, CurrentApplication, CurrentDesktopOwner, DesktopActionError, DesktopApplication,
    DesktopInvokePhase, DesktopLaunchResult, DesktopObservation, DesktopOwner, HotkeyResult,
    InvokeSource, KeyStroke, KeyboardInput, confirm_activation, dispatch_hotkey, invoke_voice,
    launch_desktop, plan_hotkey, validate_application,
};
pub use audio_process::{
    AudioProcessOwnership, AudioProcessRecord, AudioProcessSnapshot, AudioProcessState,
    ProcessImageEqual,
};
pub use call_session::{
    CallDesktopError, CallDesktopSession, DesktopCallAction, DesktopCallOperation,
};
pub use call_voice::{
    CallVoiceAudio, CallVoiceRuntime, CallVoiceSession, VoiceBaselineObservation,
    VoiceLifetimeResult, VoiceOperation, VoiceStop, VoiceTime,
};
pub use capture::{CaptureEndpoint, CaptureObservation, CaptureSession, validate_capture_text};
pub use capture_owner::{DesktopCaptureCorrelation, DesktopCaptureIdentity};
#[cfg(all(windows, target_pointer_width = "64"))]
pub use windows_activation::WindowsApplicationActivation;
#[cfg(all(windows, target_pointer_width = "64"))]
pub use windows_application::WindowsDesktopApplication;
#[cfg(all(windows, target_pointer_width = "64"))]
pub use windows_audio_process::WindowsAudioProcessSnapshot;
#[cfg(all(test, windows, target_pointer_width = "64"))]
pub(crate) use windows_audio_process::tests::verify_audio_process_native;
#[cfg(all(windows, target_pointer_width = "64"))]
pub use windows_capture::WindowsCaptureEndpoint;
#[cfg(all(test, windows, target_pointer_width = "64"))]
pub(crate) use windows_capture::tests::verify_capture_native;
#[cfg(all(windows, target_pointer_width = "64"))]
pub use windows_input::WindowsKeyboardInput;
#[cfg(all(windows, target_pointer_width = "64"))]
pub use windows_owner::WindowsDesktopProcessOwner;

pub use controls::{
    Control, ControlError, ControlSource, ControlWindow, ControlsObservation, read_controls,
    select_named, validate_labels,
};
pub use input_owner::{MicroFactoryError, MicroFactoryResult, VoiceInputClock, VoiceInputOwner};
pub use runtime::PhoneDesktopRuntime;
pub use voice::{
    START_LABELS, STOP_LABELS, VoiceControlIdentity, VoiceControlObservation, classify_voice,
    voice_labels,
};
pub use voice_input::{
    NativeHotkey, NativeVoiceInput, VoiceDispatchResult, VoiceInputPlan, dispatch_voice,
    prepare_input_focus,
};
pub use voice_runtime::DesktopVoiceRuntime;
pub use voice_session::{DesktopVoiceSession, TimedVoiceControlObservation, VoiceSource};
#[cfg(all(windows, target_pointer_width = "64"))]
pub use windows::{
    AccessibilityError, DesktopReadStage, WindowsControlSource, invoke_voice_windows,
};

#[cfg(all(test, windows))]
#[path = "../../../../../tests/rust/phone_desktop_controls.rs"]
mod tests;
