//! Desktop control evidence is an input to the live call owner, never authority
//! to identify a task, invoke a control or grant audio by itself.
mod actions;
mod controls;
mod voice;
mod voice_session;
#[cfg(all(windows, target_pointer_width = "64"))]
mod windows;
#[cfg(all(windows, target_pointer_width = "64"))]
mod windows_input;

pub use actions::{
    ActionResult, CurrentApplication, CurrentDesktopOwner, DesktopActionError, DesktopApplication,
    DesktopInvokePhase, DesktopLaunchResult, DesktopObservation, DesktopOwner, HotkeyResult,
    InvokeSource, KeyStroke, KeyboardInput, confirm_activation, dispatch_hotkey, invoke_voice,
    launch_desktop, validate_application,
};
#[cfg(all(windows, target_pointer_width = "64"))]
pub use windows_input::WindowsKeyboardInput;

pub use controls::{
    Control, ControlError, ControlSource, ControlWindow, ControlsObservation, read_controls,
    select_named, validate_labels,
};
pub use voice::{
    START_LABELS, STOP_LABELS, VoiceControlIdentity, VoiceControlObservation, classify_voice,
    voice_labels,
};
pub use voice_session::{DesktopVoiceSession, TimedVoiceControlObservation, VoiceSource};
#[cfg(all(windows, target_pointer_width = "64"))]
pub use windows::{
    AccessibilityError, DesktopReadStage, WindowsControlSource, invoke_voice_windows,
};

#[cfg(all(test, windows))]
#[path = "../../../../../tests/rust/phone_desktop_controls.rs"]
mod tests;
