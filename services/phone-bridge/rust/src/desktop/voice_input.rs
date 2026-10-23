//! Explicit Voice input settings and routing. An uncertain Micro effect cannot
//! be retried as a hotkey; capture/Voice authority belongs to the call worker.
use super::{ActionResult, DesktopActionError, HotkeyResult, plan_hotkey};
use crate::{
    micro::{
        MicroGestureResult, MicroInputResult, MicroVoiceInput, dispatch_micro_gesture_with_pause,
    },
    native_config::{NativeConfigError, NativeMicroSettings, NativeMicroValue},
};
use ivy_contracts::{IvyJson, json::JsString};
use std::sync::Arc;

#[derive(Clone, PartialEq, Eq)]
pub struct NativeHotkey {
    modifiers: Vec<JsString>,
    key_code: i32,
    focus_before: bool,
}
impl NativeHotkey {
    pub fn new(modifiers: Vec<JsString>, key_code: i32, focus_before: bool) -> ActionResult<Self> {
        plan_hotkey(&modifiers, key_code)?;
        Ok(Self {
            modifiers,
            key_code,
            focus_before,
        })
    }
    pub fn modifiers(&self) -> &[JsString] {
        &self.modifiers
    }
    pub fn key_code(&self) -> i32 {
        self.key_code
    }
    pub fn focus_before(&self) -> bool {
        self.focus_before
    }
}
#[derive(Clone, PartialEq, Eq)]
pub struct NativeVoiceInput {
    micro: Option<NativeMicroSettings>,
    fallback: Option<NativeHotkey>,
    adopt_existing: bool,
}
impl NativeVoiceInput {
    pub fn new(
        micro: Option<NativeMicroSettings>,
        fallback: Option<NativeHotkey>,
        adopt_existing: bool,
    ) -> Self {
        Self {
            micro,
            fallback,
            adopt_existing,
        }
    }
    pub fn micro(&self) -> Option<&NativeMicroSettings> {
        self.micro.as_ref()
    }
    pub fn hotkey_fallback(&self) -> Option<&NativeHotkey> {
        self.fallback.as_ref()
    }
    pub fn adopt_existing(&self) -> bool {
        self.adopt_existing
    }
    pub fn matches(&self, other: Option<&Self>) -> bool {
        other.is_some_and(|other| self == other)
    }
    /// Raw value spans retain System.Text.Json's integer-token rules. Native
    /// envelopes already reject duplicate decoded property names before routing.
    pub fn read(value: &IvyJson, raw: &str) -> Result<Self, NativeConfigError> {
        use NativeConfigError::{InvalidRequest, OutcomeUnknown};
        let fields = value.as_object().ok_or(OutcomeUnknown)?;
        let spans = crate::native_rpc::root_values(raw);
        if fields.iter().any(|(key, _)| {
            !["micro", "hotkeyFallback", "adoptExisting"]
                .iter()
                .any(|s| key == &(*s).into())
        }) || value.get("micro").is_none()
        {
            return Err(InvalidRequest);
        }
        let token = |name: &str| {
            fields
                .iter()
                .position(|(key, _)| key == &name.into())
                .and_then(|i| spans.get(i).copied())
                .ok_or(InvalidRequest)
        };
        let micro = match value.get("micro").unwrap() {
            IvyJson::Null => None,
            micro => Some(NativeMicroValue::read(micro, token("micro")?)?),
        };
        let fallback = match value.get("hotkeyFallback") {
            None | Some(IvyJson::Null) => None,
            Some(hotkey) => {
                let keys = hotkey.as_object().ok_or(OutcomeUnknown)?;
                if keys.len() != 3
                    || ["modifiers", "keyCode", "focusBeforeHotkey"]
                        .iter()
                        .any(|s| hotkey.get(s).is_none())
                {
                    return Err(InvalidRequest);
                }
                let modifiers = match hotkey.get("modifiers").unwrap() {
                    IvyJson::Null => None,
                    value => Some(
                        value
                            .as_array()
                            .ok_or(InvalidRequest)?
                            .iter()
                            .map(|s| match s {
                                IvyJson::Null => Ok(None),
                                _ => s.as_js_string().cloned().map(Some).ok_or(InvalidRequest),
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                    ),
                };
                let tokens = crate::native_rpc::root_values(token("hotkeyFallback")?);
                let key_at = keys
                    .iter()
                    .position(|(s, _)| s == &"keyCode".into())
                    .ok_or(InvalidRequest)?;
                let key_code = tokens
                    .get(key_at)
                    .ok_or(InvalidRequest)?
                    .parse::<i32>()
                    .map_err(|_| InvalidRequest)?;
                let focus = hotkey
                    .get("focusBeforeHotkey")
                    .and_then(IvyJson::as_bool)
                    .ok_or(InvalidRequest)?;
                Some((modifiers, key_code, focus))
            }
        };
        let adopt_existing = match value.get("adoptExisting") {
            None => false,
            Some(value) => value.as_bool().ok_or(OutcomeUnknown)?,
        };
        let micro = micro.map(NativeMicroValue::validate).transpose()?;
        let fallback = fallback
            .map(|(modifiers, key_code, focus)| {
                let modifiers = modifiers
                    .ok_or(InvalidRequest)?
                    .into_iter()
                    .map(|s| s.ok_or(InvalidRequest))
                    .collect::<Result<Vec<_>, _>>()?;
                NativeHotkey::new(modifiers, key_code, focus).map_err(|_| InvalidRequest)
            })
            .transpose()?;
        Ok(Self::new(micro, fallback, adopt_existing))
    }
}
#[derive(Clone)]
pub enum VoiceInputPlan {
    Micro {
        generation: Option<i64>,
        device: Option<Arc<dyn MicroVoiceInput>>,
    },
    Hotkey,
}
impl VoiceInputPlan {
    pub fn method(&self) -> &'static str {
        match self {
            Self::Micro { .. } => "micro",
            Self::Hotkey => "hotkey",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceDispatchResult {
    pub phase: &'static str,
    pub method: &'static str,
    pub hotkey: Option<HotkeyResult>,
    pub micro: Option<MicroGestureResult>,
}
impl VoiceDispatchResult {
    pub fn to_json(&self) -> IvyJson {
        IvyJson::object([
            ("phase", self.phase.into()),
            ("method", self.method.into()),
            (
                "hotkey",
                self.hotkey
                    .as_ref()
                    .map_or(IvyJson::Null, HotkeyResult::to_json),
            ),
            (
                "micro",
                self.micro
                    .as_ref()
                    .map_or(IvyJson::Null, MicroGestureResult::to_json),
            ),
        ])
    }
}
pub fn prepare_input_focus(
    plan: &VoiceInputPlan,
    settings: &NativeVoiceInput,
    original: &dyn Fn() -> ActionResult<bool>,
    is_current: &dyn Fn() -> ActionResult<bool>,
    prepare_focus: &mut dyn FnMut() -> ActionResult<bool>,
) -> ActionResult<bool> {
    match plan {
        VoiceInputPlan::Micro { .. } => Ok(original()? && is_current()?),
        VoiceInputPlan::Hotkey => {
            if !settings
                .hotkey_fallback()
                .is_some_and(NativeHotkey::focus_before)
            {
                return Ok(true);
            }
            prepare_focus()
        }
    }
}
pub fn dispatch_voice(
    plan: &VoiceInputPlan,
    settings: &NativeVoiceInput,
    cleanup: bool,
    is_current: &dyn Fn() -> ActionResult<bool>,
    before_press: &dyn Fn() -> ActionResult<bool>,
    release_authority: &dyn Fn() -> ActionResult<bool>,
    toggle: &mut dyn FnMut(
        &NativeHotkey,
        &dyn Fn() -> ActionResult<bool>,
        bool,
    ) -> ActionResult<HotkeyResult>,
    prior_micro: Option<MicroGestureResult>,
    prior_micro_was_dictation: bool,
) -> ActionResult<VoiceDispatchResult> {
    dispatch_voice_with_pause(
        plan,
        settings,
        cleanup,
        is_current,
        before_press,
        release_authority,
        toggle,
        prior_micro,
        prior_micro_was_dictation,
        &mut |ms| {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            Ok(())
        },
    )
}
fn dispatch_voice_with_pause(
    plan: &VoiceInputPlan,
    settings: &NativeVoiceInput,
    cleanup: bool,
    is_current: &dyn Fn() -> ActionResult<bool>,
    before_press: &dyn Fn() -> ActionResult<bool>,
    release_authority: &dyn Fn() -> ActionResult<bool>,
    toggle: &mut dyn FnMut(
        &NativeHotkey,
        &dyn Fn() -> ActionResult<bool>,
        bool,
    ) -> ActionResult<HotkeyResult>,
    prior_micro: Option<MicroGestureResult>,
    prior_micro_was_dictation: bool,
    pause: &mut dyn FnMut(u64) -> MicroInputResult<()>,
) -> ActionResult<VoiceDispatchResult> {
    match plan {
        VoiceInputPlan::Micro { generation, device } => {
            let (Some(generation), Some(device)) = (generation, device) else {
                return Ok(VoiceDispatchResult {
                    phase: "not_submitted",
                    method: "micro",
                    hotkey: None,
                    micro: None,
                });
            };
            let result = dispatch_micro_gesture_with_pause(
                device.as_ref(),
                *generation,
                cleanup,
                &|| Ok(is_current()? && before_press()?),
                &|| Ok(is_current()? && release_authority()?),
                pause,
            )
            .map_err(|_| {
                DesktopActionError::Argument("Exact original Micro generation required.")
            })?;
            Ok(VoiceDispatchResult {
                phase: result.phase,
                method: "micro",
                hotkey: None,
                micro: Some(result),
            })
        }
        VoiceInputPlan::Hotkey => {
            let fallback = settings
                .hotkey_fallback()
                .filter(|_| {
                    prior_micro
                        .as_ref()
                        .is_none_or(|r| r.phase == "not_submitted" || prior_micro_was_dictation)
                })
                .ok_or(DesktopActionError::Platform(
                    "Hotkey fallback requires explicit configuration and no unknown Micro effect.",
                ))?;
            let hotkey = toggle(fallback, before_press, true)?;
            Ok(VoiceDispatchResult {
                phase: hotkey.phase,
                method: "hotkey",
                hotkey: Some(hotkey),
                micro: prior_micro,
            })
        }
    }
}

#[cfg(all(test, windows))]
#[path = "../../../../../tests/rust/phone_voice_input.rs"]
mod tests;
