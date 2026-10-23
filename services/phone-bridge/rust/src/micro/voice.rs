//! The two-tap ACT10 gesture and its original-generation release policy.
//! Submission receipts never assert that Voice started or stopped.
use ivy_contracts::IvyJson;
use std::panic::{AssertUnwindSafe, catch_unwind};

pub type MicroInputResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicroAttachmentStatus {
    pub state: String,
    pub generation: Option<i64>,
    pub error_code: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicroInputReceipt {
    pub phase: String,
    pub pressed: bool,
    pub generation: i64,
}
impl MicroInputReceipt {
    pub fn to_json(&self) -> IvyJson {
        IvyJson::object([
            ("phase", self.phase.as_str().into()),
            ("pressed", self.pressed.into()),
            ("generation", IvyJson::Number(self.generation as f64)),
        ])
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicroVoiceObservation {
    pub state: String,
    pub generation: i64,
    pub dictation_sequence: i64,
}
pub trait MicroVoiceInput: Send + Sync {
    /// Start/return device-owned disposal synchronously. Constructor failures
    /// propagate; an already returned operation retains its eventual failure.
    fn close(&self) -> MicroInputResult<super::MicroClose>;
    fn status(&self) -> MicroInputResult<MicroAttachmentStatus>;
    fn send_microphone(
        &self,
        pressed: bool,
        generation: i64,
        original: &dyn Fn() -> MicroInputResult<bool>,
    ) -> MicroInputResult<MicroInputReceipt>;
    fn observe_voice(&self, generation: i64) -> MicroInputResult<MicroVoiceObservation> {
        Ok(MicroVoiceObservation {
            state: "unavailable".into(),
            generation,
            dictation_sequence: 0,
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicroGestureResult {
    pub phase: &'static str,
    pub press: MicroInputReceipt,
    pub release: Option<MicroInputReceipt>,
    pub latch_press: Option<MicroInputReceipt>,
    pub latch_release: Option<MicroInputReceipt>,
}
impl MicroGestureResult {
    pub fn to_json(&self) -> IvyJson {
        let receipt = |r: &Option<MicroInputReceipt>| {
            r.as_ref().map_or(IvyJson::Null, MicroInputReceipt::to_json)
        };
        IvyJson::object([
            ("phase", self.phase.into()),
            ("press", self.press.to_json()),
            ("release", receipt(&self.release)),
            ("latchPress", receipt(&self.latch_press)),
            ("latchRelease", receipt(&self.latch_release)),
        ])
    }
}
fn guarded<T>(f: impl FnOnce() -> MicroInputResult<T>) -> Option<T> {
    catch_unwind(AssertUnwindSafe(f)).ok()?.ok()
}
pub fn dispatch_micro_gesture(
    device: &dyn MicroVoiceInput,
    generation: i64,
    cleanup: bool,
    before_press: &dyn Fn() -> MicroInputResult<bool>,
    release_authority: &dyn Fn() -> MicroInputResult<bool>,
) -> MicroInputResult<MicroGestureResult> {
    dispatch_micro_gesture_with_pause(
        device,
        generation,
        cleanup,
        before_press,
        release_authority,
        &mut |ms| {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            Ok(())
        },
    )
}
pub(crate) fn dispatch_micro_gesture_with_pause(
    device: &dyn MicroVoiceInput,
    generation: i64,
    cleanup: bool,
    before_press: &dyn Fn() -> MicroInputResult<bool>,
    release_authority: &dyn Fn() -> MicroInputResult<bool>,
    pause: &mut dyn FnMut(u64) -> MicroInputResult<()>,
) -> MicroInputResult<MicroGestureResult> {
    if !(1..=9_007_199_254_740_991).contains(&generation) {
        return Err(std::io::Error::other("Exact original Micro generation required.").into());
    }
    let unknown = || MicroInputReceipt {
        phase: "outcome_unknown".into(),
        pressed: true,
        generation,
    };
    let press = guarded(|| device.send_microphone(true, generation, before_press));
    let mut result = MicroGestureResult {
        phase: "outcome_unknown",
        press: press.clone().unwrap_or_else(unknown),
        release: None,
        latch_press: None,
        latch_release: None,
    };
    let Some(press) = press else {
        return Ok(result);
    };
    if press.phase == "not_submitted" {
        result.phase = "not_submitted";
        return Ok(result);
    }
    if press.phase != "submitted" {
        return Ok(result);
    }
    let paused = guarded(|| pause(if cleanup { 650 } else { 50 })).is_some();
    // Always release on the same original generation, even after a pause fault
    // or cancellation. The device still applies the separate cleanup authority.
    result.release = guarded(|| device.send_microphone(false, generation, release_authority));
    if !paused
        || result
            .release
            .as_ref()
            .is_none_or(|r| r.phase != "submitted")
    {
        return Ok(result);
    }
    if cleanup {
        result.phase = "submitted";
        return Ok(result);
    }
    if guarded(|| pause(70)).is_none() {
        return Ok(result);
    }
    let latch = guarded(|| device.send_microphone(true, generation, before_press));
    result.latch_press = Some(latch.clone().unwrap_or_else(unknown));
    if latch.is_none_or(|r| r.phase != "submitted") {
        return Ok(result);
    }
    let paused = guarded(|| pause(50)).is_some();
    result.latch_release = guarded(|| device.send_microphone(false, generation, release_authority));
    if paused
        && result
            .latch_release
            .as_ref()
            .is_some_and(|r| r.phase == "submitted")
    {
        result.phase = "submitted";
    }
    Ok(result)
}
