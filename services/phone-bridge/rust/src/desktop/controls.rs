use ivy_contracts::{IvyJson, json::JsString};
use std::{
    collections::HashSet,
    panic::{AssertUnwindSafe, catch_unwind},
};

#[derive(Debug)]
pub enum ControlError {
    Labels,
    CandidateLimit,
    MatchLimit,
    InvalidObservation,
    NotCurrent,
    WrongThread,
    VoiceGroup,
    Provider(Box<dyn std::error::Error + Send + Sync>),
    Panicked,
}
impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Labels => "Bounded exact control labels required.",
            Self::CandidateLimit => "Desktop button inventory limit exceeded.",
            Self::MatchLimit => "Desktop control limit exceeded.",
            Self::InvalidObservation => "Desktop control observation unavailable.",
            Self::NotCurrent => "Current Desktop required.",
            Self::WrongThread => "Voice provider must remain on its owning thread.",
            Self::VoiceGroup => "One original Voice control group required.",
            Self::Provider(error) => return std::fmt::Display::fmt(error, f),
            Self::Panicked => "Desktop provider panicked.",
        })
    }
}
impl std::error::Error for ControlError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        if let Self::Provider(error) = self {
            Some(error.as_ref())
        } else {
            None
        }
    }
}
pub(super) fn guarded<T>(f: impl FnOnce() -> Result<T, ControlError>) -> Result<T, ControlError> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|_| ControlError::Panicked)?
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Control {
    pub id: Vec<i32>,
    pub parent_id: Vec<i32>,
    pub name: JsString,
    pub enabled: bool,
    pub offscreen: bool,
    pub toggle: Option<i32>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlWindow {
    pub handle: JsString,
    pub minimized: bool,
    pub controls: Vec<Control>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlsObservation {
    pub observed: bool,
    pub windows: Vec<ControlWindow>,
}
impl ControlsObservation {
    pub fn unavailable() -> Self {
        Self {
            observed: false,
            windows: Vec::new(),
        }
    }
    pub fn to_json(&self) -> IvyJson {
        IvyJson::object([
            (
                "state",
                (if self.observed {
                    "observed"
                } else {
                    "unavailable"
                })
                .into(),
            ),
            (
                "windows",
                IvyJson::Array(
                    self.windows
                        .iter()
                        .map(|window| {
                            IvyJson::object([
                                ("handle", IvyJson::String(window.handle.clone())),
                                ("minimized", window.minimized.into()),
                                (
                                    "controls",
                                    IvyJson::Array(
                                        window
                                            .controls
                                            .iter()
                                            .map(|control| {
                                                IvyJson::object([
                                                    ("id", ids(&control.id)),
                                                    ("parentId", ids(&control.parent_id)),
                                                    ("name", IvyJson::String(control.name.clone())),
                                                    ("enabled", control.enabled.into()),
                                                    ("offscreen", control.offscreen.into()),
                                                    (
                                                        "toggle",
                                                        control.toggle.map_or(IvyJson::Null, |n| {
                                                            IvyJson::Number(f64::from(n))
                                                        }),
                                                    ),
                                                ])
                                            })
                                            .collect(),
                                    ),
                                ),
                            ])
                        })
                        .collect(),
                ),
            ),
        ])
    }
}
pub(super) fn ids(values: &[i32]) -> IvyJson {
    IvyJson::Array(
        values
            .iter()
            .map(|n| IvyJson::Number(f64::from(*n)))
            .collect(),
    )
}
pub trait ControlSource {
    /// Fixed query/cache preparation must not inspect window/control evidence.
    fn prepare(&mut self, _labels: &[JsString]) -> Result<(), ControlError> {
        Ok(())
    }
    fn read(&mut self, labels: &[JsString]) -> Result<Vec<ControlWindow>, ControlError>;
    /// Always called after successful allocation, including failed reads. A close
    /// failure invalidates the entire observation, including an earlier positive.
    fn close(&mut self) -> Result<(), ControlError> {
        Ok(())
    }
}

pub fn validate_labels(labels: &[JsString]) -> Result<(), ControlError> {
    let mut names = HashSet::new();
    if !(1..=32).contains(&labels.len())
        || labels.iter().any(|label| {
            label.utf16_len() > 256
                || label.0.contains(&0)
                || label
                    .0
                    .iter()
                    .all(|unit| char::from_u32(u32::from(*unit)).is_some_and(char::is_whitespace))
                || !names.insert(label)
        })
    {
        return Err(ControlError::Labels);
    }
    Ok(())
}

pub fn select_named<T>(
    count: i32,
    mut at: impl FnMut(i32) -> Result<T, ControlError>,
    mut name: impl FnMut(&T) -> Result<Option<JsString>, ControlError>,
    labels: &[JsString],
) -> Result<Vec<T>, ControlError> {
    validate_labels(labels)?;
    if !(0..=4096).contains(&count) {
        return Err(ControlError::CandidateLimit);
    }
    let names: HashSet<_> = labels.iter().collect();
    let mut selected = Vec::new();
    for index in 0..count {
        let item = at(index)?;
        if !name(&item)?.is_some_and(|name| names.contains(&name)) {
            continue;
        }
        if selected.len() == 64 {
            return Err(ControlError::MatchLimit);
        }
        selected.push(item);
    }
    Ok(selected)
}

pub(super) fn validate_windows(
    windows: &[ControlWindow],
    labels: &[JsString],
) -> Result<(), ControlError> {
    let mut handles = HashSet::new();
    let mut ids = HashSet::new();
    if !(1..=32).contains(&windows.len()) {
        return Err(ControlError::InvalidObservation);
    }
    for window in windows {
        let valid_handle = window
            .handle
            .to_utf8()
            .ok()
            .and_then(|text| {
                text.parse::<i64>()
                    .ok()
                    .map(|n| n > 0 && n.to_string() == text)
            })
            .unwrap_or(false);
        if !valid_handle || !handles.insert(&window.handle) || window.controls.len() > 64 {
            return Err(ControlError::InvalidObservation);
        }
        for control in &window.controls {
            if !(1..=32).contains(&control.id.len())
                || !(1..=32).contains(&control.parent_id.len())
                || !ids.insert(&control.id)
                || control.id == control.parent_id
                || !labels.contains(&control.name)
                || control.toggle.is_some_and(|n| !(0..=2).contains(&n))
            {
                return Err(ControlError::InvalidObservation);
            }
        }
    }
    Ok(())
}

/// Invalid labels fail before allocation. Provider/owner/read/cleanup failures
/// return unavailable, and optional diagnostics never change that result.
pub fn read_controls<S: ControlSource>(
    labels: &[JsString],
    current: &dyn Fn() -> bool,
    open: impl FnOnce() -> Result<S, ControlError>,
    mut unavailable: Option<&mut dyn FnMut(&ControlError)>,
    mut read_started: Option<&mut dyn FnMut()>,
) -> Result<ControlsObservation, ControlError> {
    validate_labels(labels)?;
    let labels = labels.to_vec();
    let result = guarded(|| {
        if !current() {
            return Err(ControlError::NotCurrent);
        }
        let mut source = open()?;
        let observation = guarded(|| {
            source.prepare(&labels)?;
            if let Some(started) = &mut read_started {
                started();
            }
            if !current() {
                return Err(ControlError::NotCurrent);
            }
            let windows = source.read(&labels)?;
            validate_windows(&windows, &labels)?;
            Ok(ControlsObservation {
                observed: true,
                windows,
            })
        });
        // Match using/Dispose failure precedence and the final owner check.
        guarded(|| source.close())?;
        let observation = observation?;
        if !current() {
            return Err(ControlError::NotCurrent);
        }
        Ok(observation)
    });
    Ok(match result {
        Ok(observation) => observation,
        Err(error) => {
            let _ = guarded(|| {
                if let Some(diagnostic) = &mut unavailable {
                    diagnostic(&error);
                }
                Ok(())
            });
            ControlsObservation::unavailable()
        }
    })
}
