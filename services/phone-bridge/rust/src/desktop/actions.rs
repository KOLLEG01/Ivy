//! Desktop effects retain the original owner checks and uncertain outcomes.
//! A submitted launch, chord or control Invoke never grants audio authority.
use super::{Control, ControlError, ControlSource, ControlWindow, START_LABELS, STOP_LABELS};
use crate::media::AudioDesktopIdentity;
use ivy_contracts::{IvyJson, json::JsString};
use std::{
    cell::Cell,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Mutex,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopActionError {
    Argument(&'static str),
    Unavailable,
}
impl std::fmt::Display for DesktopActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Argument(message) => f.write_str(message),
            Self::Unavailable => f.write_str("Desktop input unavailable."),
        }
    }
}
impl std::error::Error for DesktopActionError {}
pub type ActionResult<T> = Result<T, DesktopActionError>;
fn guarded<T>(f: impl FnOnce() -> ActionResult<T>) -> ActionResult<T> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|_| DesktopActionError::Unavailable)?
}

pub fn validate_application(value: &str) -> ActionResult<()> {
    let mut parts = value.split('!');
    let valid = |part: Option<&str>| {
        part.is_some_and(|p| {
            !p.is_empty()
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        })
    };
    if value.len() > 128 || !valid(parts.next()) || !valid(parts.next()) || parts.next().is_some() {
        return Err(DesktopActionError::Argument(
            "An exact packaged application ID is required.",
        ));
    }
    Ok(())
}
#[derive(Clone)]
pub struct DesktopObservation {
    pub state: &'static str,
    pub identity: Option<AudioDesktopIdentity>,
}
impl DesktopObservation {
    pub fn unavailable() -> Self {
        Self {
            state: "unavailable",
            identity: None,
        }
    }
    pub fn select(matching_process_exists: bool, mut windows: Vec<AudioDesktopIdentity>) -> Self {
        if windows.len() > 1 {
            Self {
                state: "ambiguous",
                identity: None,
            }
        } else if windows.len() == 1 {
            Self {
                state: "ready",
                identity: windows.pop(),
            }
        } else {
            Self {
                state: if matching_process_exists {
                    "waiting"
                } else {
                    "absent"
                },
                identity: None,
            }
        }
    }
    pub fn to_json(&self) -> IvyJson {
        IvyJson::object([
            ("state", self.state.into()),
            ("identity", identity_json(self.identity.as_ref())),
        ])
    }
}
pub trait DesktopApplication {
    fn observe(&mut self) -> ActionResult<Option<DesktopObservation>>;
    fn activate(&mut self) -> ActionResult<i32>;
}
#[derive(Clone)]
pub struct DesktopLaunchResult {
    pub phase: &'static str,
    pub identity: Option<AudioDesktopIdentity>,
    pub activation_pid: Option<i32>,
    pub error_code: Option<&'static str>,
}
impl DesktopLaunchResult {
    pub fn to_json(&self) -> IvyJson {
        IvyJson::object([
            ("phase", self.phase.into()),
            ("identity", identity_json(self.identity.as_ref())),
            (
                "activationPid",
                self.activation_pid
                    .map(|n| IvyJson::Number(n.into()))
                    .unwrap_or(IvyJson::Null),
            ),
            (
                "errorCode",
                self.error_code.map(Into::into).unwrap_or(IvyJson::Null),
            ),
        ])
    }
}
fn identity_json(identity: Option<&AudioDesktopIdentity>) -> IvyJson {
    let Some(identity) = identity else {
        return IvyJson::Null;
    };
    #[cfg(windows)]
    let image = {
        use std::os::windows::ffi::OsStrExt;
        JsString(identity.image().as_os_str().encode_wide().collect())
    };
    #[cfg(not(windows))]
    let image = JsString::from(identity.image().to_string_lossy().as_ref());
    IvyJson::object([
        ("pid", IvyJson::Number(identity.pid().into())),
        (
            "startTimeUtcTicks",
            identity.creation_ticks().to_string().into(),
        ),
        ("imagePath", IvyJson::String(image)),
        ("appUserModelId", identity.application().into()),
    ])
}

static LAUNCH_GATE: Mutex<()> = Mutex::new(());
static INPUT_GATE: Mutex<()> = Mutex::new(());
thread_local! {static LAUNCH_ENTERED:Cell<bool>=const{Cell::new(false)};static INPUT_ENTERED:Cell<bool>=const{Cell::new(false)};}
// C# Monitor permits same-thread reentry. Preserve it without adding another
// dependency or allowing a different thread through the domain's original gate.
fn serialized<T>(gate: &Mutex<()>, entered: &Cell<bool>, f: impl FnOnce() -> T) -> T {
    if entered.get() {
        return f();
    }
    let _lock = gate.lock().unwrap_or_else(|e| e.into_inner());
    entered.set(true);
    struct Reset<'a>(&'a Cell<bool>);
    impl Drop for Reset<'_> {
        fn drop(&mut self) {
            self.0.set(false);
        }
    }
    let _reset = Reset(entered);
    f()
}
pub fn launch_desktop(
    application: &mut impl DesktopApplication,
    start_if_missing: bool,
) -> DesktopLaunchResult {
    LAUNCH_ENTERED.with(|entered| {
        serialized(&LAUNCH_GATE, entered, || {
            let observed = guarded(|| application.observe()).ok().flatten();
            let result = |phase, identity, activation_pid, error_code| DesktopLaunchResult {
                phase,
                identity,
                activation_pid,
                error_code,
            };
            match observed {
                None => result(
                    "not_submitted",
                    None,
                    None,
                    Some("desktop_observation_unavailable"),
                ),
                Some(o) => match o.state {
                    "unavailable" => result(
                        "not_submitted",
                        None,
                        None,
                        Some("desktop_observation_unavailable"),
                    ),
                    "ambiguous" => result("not_submitted", None, None, Some("desktop_ambiguous")),
                    "ready" => result("ready", o.identity, None, None),
                    "waiting" => result("waiting", None, None, None),
                    _ if !start_if_missing => {
                        result("not_submitted", None, None, Some("desktop_missing"))
                    }
                    _ => match guarded(|| application.activate()) {
                        Ok(pid) if pid > 0 => result("submitted", None, Some(pid), None),
                        _ => result(
                            "outcome_unknown",
                            None,
                            None,
                            Some("desktop_launch_outcome_unknown"),
                        ),
                    },
                },
            }
        })
    })
}
pub struct CurrentApplication<'a, A: DesktopApplication> {
    pub application: &'a mut A,
    pub current: &'a dyn Fn() -> ActionResult<bool>,
}
impl<A: DesktopApplication> DesktopApplication for CurrentApplication<'_, A> {
    fn observe(&mut self) -> ActionResult<Option<DesktopObservation>> {
        if !(self.current)()? {
            return Ok(Some(DesktopObservation::unavailable()));
        }
        let result = self.application.observe()?;
        Ok(if (self.current)()? {
            result
        } else {
            Some(DesktopObservation::unavailable())
        })
    }
    fn activate(&mut self) -> ActionResult<i32> {
        if !(self.current)()? {
            return Err(DesktopActionError::Argument(
                "Original call ended before activation.",
            ));
        }
        self.application.activate()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyStroke {
    pub key: u16,
    pub up: bool,
}
pub trait KeyboardInput {
    fn is_idle(&mut self) -> ActionResult<bool>;
    fn send(&mut self, keys: &[KeyStroke]) -> ActionResult<i32>;
}
pub trait DesktopOwner {
    fn is_current(&mut self) -> ActionResult<bool>;
    fn try_focus(&mut self) -> ActionResult<bool>;
    fn is_focused(&mut self) -> ActionResult<bool>;
}
pub struct CurrentDesktopOwner<'a, O: DesktopOwner> {
    pub owner: &'a mut O,
    pub current: &'a dyn Fn() -> ActionResult<bool>,
}
impl<O: DesktopOwner> DesktopOwner for CurrentDesktopOwner<'_, O> {
    fn is_current(&mut self) -> ActionResult<bool> {
        Ok((self.current)()? && self.owner.is_current()? && (self.current)()?)
    }
    fn try_focus(&mut self) -> ActionResult<bool> {
        Ok(self.is_current()? && self.owner.try_focus()? && self.is_current()?)
    }
    fn is_focused(&mut self) -> ActionResult<bool> {
        Ok(self.is_current()? && self.owner.is_focused()? && self.is_current()?)
    }
}
pub fn confirm_activation(
    mut current: impl FnMut() -> ActionResult<bool>,
    mut foreground: impl FnMut() -> ActionResult<bool>,
    mut restore: impl FnMut() -> ActionResult<bool>,
    mut activate: impl FnMut() -> ActionResult<bool>,
    mut processed: impl FnMut() -> ActionResult<bool>,
) -> ActionResult<bool> {
    if !current()? {
        return Ok(false);
    }
    if foreground()? {
        return current();
    }
    if !restore()? || !current()? || !activate()? || !processed()? {
        return Ok(false);
    }
    Ok(current()? && foreground()?)
}
pub fn plan_hotkey(modifiers: &[JsString], key_code: i32) -> ActionResult<Vec<KeyStroke>> {
    if !(1..=4).contains(&modifiers.len())
        || !(key_code == 32 || (65..=90).contains(&key_code) || (112..=135).contains(&key_code))
    {
        return Err(DesktopActionError::Argument(
            "Invalid configured Voice hotkey.",
        ));
    }
    let mut selected: std::collections::HashSet<_> = modifiers.iter().cloned().collect();
    if selected.len() != modifiers.len()
        || !["control", "alt", "windows"]
            .iter()
            .any(|s| selected.contains(&JsString::from(*s)))
    {
        return Err(DesktopActionError::Argument(
            "Voice hotkey requires distinct modifiers and Control, Alt or Windows.",
        ));
    }
    let mut keys = Vec::new();
    for (name, key) in [
        ("control", 0x11),
        ("alt", 0x12),
        ("shift", 0x10),
        ("windows", 0x5b),
    ] {
        if selected.remove(&JsString::from(name)) {
            keys.push(key);
        }
    }
    if !selected.is_empty() {
        return Err(DesktopActionError::Argument(
            "Unknown Voice hotkey modifier.",
        ));
    }
    keys.push(key_code as u16);
    Ok(keys
        .iter()
        .map(|&key| KeyStroke { key, up: false })
        .chain(keys.iter().rev().map(|&key| KeyStroke { key, up: true }))
        .collect())
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotkeyResult {
    pub phase: &'static str,
    pub requested: i32,
    pub submitted: Option<i32>,
    pub key_up_submitted: bool,
    pub error_code: Option<&'static str>,
}
impl HotkeyResult {
    pub fn to_json(&self) -> IvyJson {
        IvyJson::object([
            ("phase", self.phase.into()),
            ("requested", IvyJson::Number(self.requested.into())),
            (
                "submitted",
                self.submitted
                    .map(|n| IvyJson::Number(n.into()))
                    .unwrap_or(IvyJson::Null),
            ),
            ("keyUpSubmitted", self.key_up_submitted.into()),
            (
                "errorCode",
                self.error_code.map(Into::into).unwrap_or(IvyJson::Null),
            ),
        ])
    }
}
pub fn dispatch_hotkey(
    owner: &mut impl DesktopOwner,
    keyboard: &mut impl KeyboardInput,
    modifiers: &[JsString],
    key_code: i32,
    focus_before: bool,
    focus_prepared: bool,
) -> ActionResult<HotkeyResult> {
    let plan = plan_hotkey(modifiers, key_code)?;
    Ok(INPUT_ENTERED.with(|entered| {
        serialized(&INPUT_GATE, entered, || {
            let result = |phase, submitted, key_up_submitted, error_code| HotkeyResult {
                phase,
                requested: plan.len() as i32,
                submitted,
                key_up_submitted,
                error_code,
            };
            let refusal = guarded(|| {
                if !owner.is_current()? {
                    return Ok(Some("desktop_owner_changed"));
                }
                if focus_before && !focus_prepared && !owner.try_focus()? {
                    return Ok(Some("desktop_focus_failed"));
                }
                if !keyboard.is_idle()? {
                    return Ok(Some("keyboard_busy"));
                }
                if !owner.is_current()? {
                    return Ok(Some("desktop_owner_changed"));
                }
                if focus_before && !owner.is_focused()? {
                    return Ok(Some("desktop_focus_failed"));
                }
                Ok(None)
            });
            match refusal {
                Ok(Some(error)) => return result("not_submitted", Some(0), false, Some(error)),
                Err(_) => {
                    return result("not_submitted", Some(0), false, Some("input_unavailable"));
                }
                Ok(None) => {}
            }
            let mut submitted = None;
            if let Ok(count) = guarded(|| keyboard.send(&plan)) {
                if (0..=plan.len() as i32).contains(&count) {
                    submitted = Some(count);
                }
                if count == plan.len() as i32 {
                    return result("submitted", Some(count), true, None);
                }
                if count == 0 {
                    return result("not_submitted", Some(0), false, Some("input_rejected"));
                }
            }
            let releases = &plan[plan.len() / 2..];
            let released = guarded(|| keyboard.send(releases))
                .is_ok_and(|count| count == releases.len() as i32);
            result(
                "outcome_unknown",
                submitted,
                released,
                Some(if submitted.is_some() {
                    "partial_input"
                } else {
                    "input_outcome_unknown"
                }),
            )
        })
    }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopInvokePhase {
    NotSubmitted,
    Submitted,
    OutcomeUnknown,
}
impl DesktopInvokePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotSubmitted => "not_submitted",
            Self::Submitted => "submitted",
            Self::OutcomeUnknown => "outcome_unknown",
        }
    }
}
pub trait InvokeSource: ControlSource {
    fn invoke(
        &mut self,
        window: &JsString,
        control: &Control,
        current: &dyn Fn() -> bool,
    ) -> Result<DesktopInvokePhase, ControlError>;
}
pub fn invoke_voice<S: InvokeSource>(
    stop: bool,
    current: &dyn Fn() -> bool,
    owner_current: &dyn Fn() -> bool,
    open: impl FnOnce() -> Result<S, ControlError>,
) -> DesktopInvokePhase {
    // These two checks precede the original try/using scope.
    if !current() || !owner_current() {
        return DesktopInvokePhase::NotSubmitted;
    }
    let Ok(mut source) = super::controls::guarded(open) else {
        return DesktopInvokePhase::NotSubmitted;
    };
    let result = super::controls::guarded(|| {
        let labels: Vec<JsString> = (if stop { STOP_LABELS } else { START_LABELS })
            .iter()
            .map(|s| JsString::from(*s))
            .collect();
        let windows = source.read(&labels)?;
        let mut selected = None;
        for label in &labels {
            let matches: Vec<(&ControlWindow, &Control)> = windows
                .iter()
                .filter(|w| !w.minimized)
                .flat_map(|w| w.controls.iter().map(move |c| (w, c)))
                .filter(|(_, c)| c.enabled && !c.offscreen && c.name == *label)
                .collect();
            if !matches.is_empty() {
                if matches.len() == 1 {
                    selected = Some(matches[0]);
                }
                break;
            }
        }
        let Some((window, control)) = selected else {
            return Ok(DesktopInvokePhase::NotSubmitted);
        };
        if !current() || !owner_current() {
            return Ok(DesktopInvokePhase::NotSubmitted);
        }
        source.invoke(&window.handle, control, current)
    });
    let closed = super::controls::guarded(|| source.close());
    match (result, closed) {
        (Ok(phase), Ok(())) => phase,
        _ => DesktopInvokePhase::NotSubmitted,
    }
}
