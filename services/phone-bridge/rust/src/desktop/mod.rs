//! Desktop control evidence is an input to the live call owner, never authority
//! to identify a task, invoke a control or grant audio by itself.
mod controls;
mod voice;
mod voice_session;
#[cfg(all(windows, target_pointer_width = "64"))]
mod windows;

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
pub use windows::{DesktopInvokePhase, DesktopReadStage, WindowsControlSource};

#[cfg(all(test, windows))]
#[path = "../../../../../tests/rust/phone_desktop_controls.rs"]
mod tests;
