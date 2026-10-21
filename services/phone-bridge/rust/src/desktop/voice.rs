use super::controls::{ControlWindow, ControlsObservation, ids, validate_windows};
use ivy_contracts::{IvyJson, json::JsString};

pub const START_LABELS: &[&str] = &[
    "Start voice chat",
    "Sprachchat starten",
    "Start new voice chat",
    "Neuen Sprachchat starten",
];
pub const STOP_LABELS: &[&str] = &[
    "Stop voice chat",
    "End voice chat",
    "Sprachchat stoppen",
    "Sprachchat beenden",
];
const MUTE: &[&str] = &["Mute microphone", "Mikrofon stummschalten"];
const UNMUTE: &[&str] = &["Unmute microphone", "Mikrofonstummschaltung aufheben"];
const TRANSITIONS: &[&str] = &[
    "Cancel voice chat",
    "Ending voice chat…",
    "Starting voice chat",
    "Sprachchat abbrechen",
    "Sprachchat wird beendet…",
    "Sprachchat wird gestartet",
];
pub fn voice_labels() -> Vec<JsString> {
    [START_LABELS, STOP_LABELS, MUTE, UNMUTE, TRANSITIONS]
        .into_iter()
        .flatten()
        .map(|s| JsString::from(*s))
        .collect()
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceControlIdentity {
    pub window: JsString,
    pub parent_id: Vec<i32>,
    pub microphone_id: Vec<i32>,
    pub stop_id: Vec<i32>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceControlObservation {
    pub state: &'static str,
    pub identity: Option<VoiceControlIdentity>,
}
impl VoiceControlObservation {
    pub fn unavailable() -> Self {
        Self::state("unavailable")
    }
    fn state(state: &'static str) -> Self {
        Self {
            state,
            identity: None,
        }
    }
    pub fn to_json(&self) -> IvyJson {
        IvyJson::object([
            ("state", self.state.into()),
            (
                "identity",
                self.identity.as_ref().map_or(IvyJson::Null, |id| {
                    IvyJson::object([
                        ("window", IvyJson::String(id.window.clone())),
                        ("parentId", ids(&id.parent_id)),
                        ("microphoneId", ids(&id.microphone_id)),
                        ("stopId", ids(&id.stop_id)),
                    ])
                }),
            ),
        ])
    }
}

pub fn classify_voice(observation: Option<&ControlsObservation>) -> VoiceControlObservation {
    let result = VoiceControlObservation::state;
    let Some(observation) = observation.filter(|o| o.observed) else {
        return result("unavailable");
    };
    if validate_windows(&observation.windows, &voice_labels()).is_err() {
        return result("unavailable");
    }
    let items: Vec<_> = observation
        .windows
        .iter()
        .flat_map(|window| window.controls.iter().map(move |control| (window, control)))
        .collect();
    let is = |labels: &[&str], name: &JsString| {
        labels
            .iter()
            .any(|s| name.0.iter().copied().eq(s.encode_utf16()))
    };
    let visible = |window: &ControlWindow, offscreen| !window.minimized && !offscreen;
    let voice: Vec<_> = items
        .iter()
        .filter(|(_, c)| !is(START_LABELS, &c.name))
        .collect();
    if voice.is_empty() {
        return result(
            if items
                .iter()
                .any(|(w, c)| is(START_LABELS, &c.name) && visible(w, c.offscreen) && c.enabled)
            {
                "idle"
            } else {
                "unavailable"
            },
        );
    }
    let microphones: Vec<_> = voice
        .iter()
        .filter(|(_, c)| is(MUTE, &c.name) || is(UNMUTE, &c.name))
        .collect();
    let endings: Vec<_> = voice
        .iter()
        .filter(|(_, c)| is(STOP_LABELS, &c.name))
        .collect();
    let changing: Vec<_> = voice
        .iter()
        .filter(|(_, c)| is(TRANSITIONS, &c.name))
        .collect();
    if microphones.len() > 1 || endings.len() > 1 || changing.len() > 1 {
        return result("ambiguous");
    }
    if voice.iter().any(|(w, c)| !visible(w, c.offscreen)) {
        return result("unavailable");
    }
    if !changing.is_empty() {
        return result("transition");
    }
    if microphones.len() != 1 || endings.len() != 1 {
        return result("unavailable");
    }
    let (mic_window, mic) = microphones[0];
    let (stop_window, stop) = endings[0];
    if mic_window.handle != stop_window.handle || mic.parent_id != stop.parent_id {
        return result("ambiguous");
    }
    if !mic.enabled || !stop.enabled {
        return result("transition");
    }
    let muted = is(UNMUTE, &mic.name);
    if mic.toggle != Some(i32::from(muted)) || matches!(stop.toggle, Some(1 | 2)) {
        return result("unavailable");
    }
    VoiceControlObservation {
        state: if muted { "muted" } else { "active" },
        identity: Some(VoiceControlIdentity {
            window: mic_window.handle.clone(),
            parent_id: mic.parent_id.clone(),
            microphone_id: mic.id.clone(),
            stop_id: stop.id.clone(),
        }),
    }
}
