//! Desktop control evidence is an input to the live call owner, never authority
//! to identify a task, invoke a control or grant audio by itself.
mod controls;
mod voice;
mod voice_session;

pub use controls::{
    Control, ControlError, ControlSource, ControlWindow, ControlsObservation, read_controls,
    select_named, validate_labels,
};
pub use voice::{
    START_LABELS, STOP_LABELS, VoiceControlIdentity, VoiceControlObservation, classify_voice,
    voice_labels,
};
pub use voice_session::{DesktopVoiceSession, TimedVoiceControlObservation, VoiceSource};

#[cfg(all(test, windows))]
mod tests;
