use super::*;
use crate::{
    desktop::NativeHotkey,
    micro::{
        MicroAttachmentStatus, MicroClose, MicroInputReceipt, MicroInputResult, MicroVoiceInput,
    },
    native_config::NativeMicroSettings,
};
use ivy_contracts::json::{parse, stringify};
use std::{fs, path::Path, sync::atomic::AtomicU64};

#[path = "phone_call_voice_faults.rs"]
mod exception_policy;

fn n(value: usize) -> IvyJson {
    IvyJson::Number(value as f64)
}
fn yes(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}
fn row(name: &str, captures: &[&str]) -> IvyJson {
    IvyJson::object([
        ("name", name.into()),
        ("mode", "hotkey".into()),
        (
            "captures",
            IvyJson::Array(captures.iter().map(|s| (*s).into()).collect()),
        ),
        ("dispatches", IvyJson::Array(vec!["submitted".into()])),
        (
            "actions",
            IvyJson::Array(vec!["run".into(), "has".into(), "close".into()]),
        ),
        ("adopt", false.into()),
        ("prepared", true.into()),
        ("mismatch", false.into()),
        ("focus", true.into()),
        ("baselineError", false.into()),
        ("checkAuthority", false.into()),
        ("exited", false.into()),
        ("originalAllowed", true.into()),
        ("cleanupAllowed", true.into()),
        ("deviceGeneration", n(1)),
        ("dictationAt", n(1_000_000_000)),
        ("currentErrorAt", n(1_000_000_000)),
        ("currentEndsAt", n(1_000_000_000)),
        ("lateAbsentAt", n(1_000_000_000)),
        ("control", "stop".into()),
        ("controlAt", n(1)),
    ])
}
fn set(row: &mut IvyJson, key: &str, value: IvyJson) {
    let IvyJson::Object(fields) = row else {
        panic!()
    };
    fields.iter_mut().find(|(k, _)| k == &key.into()).unwrap().1 = value;
}
fn strings(items: &[&str]) -> IvyJson {
    IvyJson::Array(items.iter().map(|s| (*s).into()).collect())
}
fn cases() -> Vec<IvyJson> {
    let mut rows = vec![];
    for (name, captures) in [
        ("hotkey-start-stop", vec!["none", "matched", "none"]),
        ("baseline-null", vec!["null"]),
        ("baseline-matched", vec!["matched"]),
        ("baseline-ambiguous", vec!["ambiguous"]),
        ("baseline-wrong-endpoint", vec!["wrong-endpoint"]),
        ("baseline-none-with-identity", vec!["none-identity"]),
        (
            "baseline-matched-without-identity",
            vec!["matched-no-identity"],
        ),
        ("baseline-error", vec!["error"]),
        ("unavailable-retries-exhausted", vec!["unavailable"]),
        (
            "unavailable-then-start",
            vec!["unavailable", "unavailable", "none", "matched", "none"],
        ),
        ("confirmation-ambiguous", vec!["none", "ambiguous", "none"]),
        ("confirmation-deadline", vec!["none"]),
        ("confirmation-error", vec!["none", "error", "none"]),
    ] {
        rows.push(row(name, &captures));
    }
    for phase in ["not_submitted", "outcome_unknown", "error"] {
        let mut c = row(&format!("input-{phase}"), &["none", "none"]);
        set(&mut c, "dispatches", strings(&[phase]));
        rows.push(c);
    }
    for field in ["focus", "prepared", "originalAllowed"] {
        let mut c = row(&format!("refused-{field}"), &["none"]);
        set(&mut c, field, false.into());
        rows.push(c);
    }
    let mut c = row("wrong-prepared-desktop", &["none"]);
    set(&mut c, "mismatch", true.into());
    rows.push(c);
    let mut c = row("diagnostic-observer-throws", &["none", "matched", "none"]);
    set(&mut c, "baselineError", true.into());
    rows.push(c);
    let mut c = row("adopt-existing", &["matched", "none"]);
    set(&mut c, "adopt", true.into());
    rows.push(c);
    let mut c = row("adopt-existing-changed-capture", &["matched", "other"]);
    set(&mut c, "adopt", true.into());
    set(&mut c, "actions", strings(&["run", "has"]));
    rows.push(c);
    for mode in ["micro", "micro-fallback"] {
        let mut c = row(
            &format!("{mode}-start-stop"),
            &[
                "none", "matched", "matched", "matched", "matched", "matched", "matched",
                "matched", "matched", "matched", "matched", "matched", "matched", "matched",
                "matched", "matched", "matched", "matched", "none",
            ],
        );
        set(&mut c, "mode", mode.into());
        set(&mut c, "lateAbsentAt", n(19));
        rows.push(c);
    }
    for mode in ["micro", "micro-fallback"] {
        let mut c = row(
            &format!("{mode}-end-after-five-seconds"),
            &["none", "matched"],
        );
        set(&mut c, "mode", mode.into());
        set(&mut c, "lateAbsentAt", n(70));
        rows.push(c);
    }
    let mut captures = vec!["none"];
    captures.extend(std::iter::repeat_n("matched", 67));
    captures.extend(["other", "none"]);
    let mut c = row("micro-end-replacement-refuses-hotkey", &captures);
    set(&mut c, "mode", "micro-fallback".into());
    rows.push(c);
    let mut c = row(
        "micro-refused-hotkey-fallback",
        &["none", "matched", "none"],
    );
    set(&mut c, "mode", "micro-fallback".into());
    set(
        &mut c,
        "dispatches",
        strings(&["not_submitted", "submitted", "submitted"]),
    );
    rows.push(c);
    let mut c = row("micro-unknown-no-hotkey-retry", &["none", "none"]);
    set(&mut c, "mode", "micro-fallback".into());
    set(
        &mut c,
        "dispatches",
        strings(&["outcome_unknown", "submitted"]),
    );
    rows.push(c);
    let mut c = row("dictation-without-fallback", &["none", "none"]);
    set(&mut c, "mode", "micro".into());
    set(&mut c, "dictationAt", n(2));
    rows.push(c);
    let mut c = row(
        "dictation-hotkey-fallback",
        &["none", "none", "matched", "none"],
    );
    set(&mut c, "mode", "micro-fallback".into());
    set(&mut c, "dictationAt", n(2));
    rows.push(c);
    let mut c = row("dictation-remains-bound", &["none", "matched"]);
    set(&mut c, "mode", "micro-fallback".into());
    set(&mut c, "dictationAt", n(2));
    rows.push(c);
    let mut c = row("micro-reconnect-before-cleanup", &["none", "matched"]);
    set(&mut c, "mode", "micro".into());
    set(&mut c, "deviceGeneration", n(2));
    set(&mut c, "lateAbsentAt", n(19));
    rows.push(c);
    let mut c = row(
        "health-suspend-and-restore",
        &["none", "matched", "unavailable", "matched", "none", "none"],
    );
    set(&mut c, "controlAt", n(4));
    rows.push(c);
    let mut c = row("health-capture-ended", &["none", "matched", "none"]);
    set(&mut c, "controlAt", n(1000));
    rows.push(c);
    let mut c = row(
        "health-owner-changed-late-absence",
        &["none", "matched", "other"],
    );
    set(&mut c, "controlAt", n(1000));
    set(&mut c, "lateAbsentAt", n(12));
    rows.push(c);
    let mut c = row("cleanup-authority-lost", &["none", "matched", "none"]);
    set(&mut c, "cleanupAllowed", false.into());
    set(
        &mut c,
        "actions",
        strings(&["run", "close", "close", "has"]),
    );
    rows.push(c);
    let mut c = row(
        "original-capture-query",
        &["none", "matched", "matched", "none"],
    );
    set(&mut c, "control", "has-stop".into());
    rows.push(c);
    let mut c = row(
        "original-capture-query-replacement",
        &["none", "matched", "other", "none"],
    );
    set(&mut c, "control", "has-stop".into());
    rows.push(c);
    let mut c = row(
        "pause-retire-and-cached-close",
        &["none", "matched", "none"],
    );
    set(&mut c, "control", "pause".into());
    set(
        &mut c,
        "actions",
        strings(&["run", "retire", "close", "stop", "has"]),
    );
    rows.push(c);
    let mut c = row(
        "retirement-survives-dropped-observer",
        &["none", "matched", "none"],
    );
    set(&mut c, "control", "pause".into());
    set(
        &mut c,
        "actions",
        strings(&["run", "retire-drop", "close", "has"]),
    );
    rows.push(c);
    let mut c = row(
        "cleanup-survives-dropped-observer",
        &["none", "matched", "none"],
    );
    set(&mut c, "actions", strings(&["run", "close-drop", "has"]));
    rows.push(c);
    let mut c = row("stop-before-start", &["none"]);
    set(
        &mut c,
        "actions",
        strings(&["stop", "start", "pause", "close", "close", "has"]),
    );
    rows.push(c);
    let mut c = row("one-start-thread", &["none", "matched", "none"]);
    set(
        &mut c,
        "actions",
        strings(&["start", "start", "has", "stop", "close"]),
    );
    rows.push(c);
    let mut c = row("current-fault-after-dispatch", &["none", "matched", "none"]);
    set(&mut c, "currentErrorAt", n(3));
    rows.push(c);
    let mut c = row("cleanup-exited-original", &["none", "matched", "none"]);
    set(&mut c, "currentEndsAt", n(4));
    set(&mut c, "exited", true.into());
    rows.push(c);
    let mut c = row("callback-authority", &["none", "matched", "none"]);
    set(&mut c, "checkAuthority", true.into());
    rows.push(c);
    rows
}
#[derive(Default)]
struct Counts {
    current: usize,
    capture: usize,
    select: usize,
    dispatch: usize,
    observe: usize,
    monitor: usize,
}
struct Context {
    row: IvyJson,
    trace: Mutex<Vec<String>>,
    now: AtomicU64,
    counts: Mutex<Counts>,
    session: Mutex<Option<CallVoiceSession>>,
}
impl Context {
    fn log(&self, message: impl Into<String>) {
        self.trace.lock().unwrap().push(message.into());
    }
    fn int(&self, key: &str) -> usize {
        self.row.get(key).unwrap().as_f64().unwrap() as usize
    }
    fn bool(&self, key: &str) -> bool {
        self.row.get(key).unwrap().as_bool().unwrap()
    }
    fn text(&self, key: &str) -> String {
        self.row.get(key).unwrap().as_str().unwrap()
    }
    fn next(&self, field: fn(&mut Counts) -> &mut usize) -> usize {
        let mut counts = self.counts.lock().unwrap();
        let value = field(&mut counts);
        *value += 1;
        *value
    }
    fn original(&self, cleanup: bool) -> ActionResult<bool> {
        self.log(format!("original:{}", yes(cleanup)));
        Ok(self.bool(if cleanup {
            "cleanupAllowed"
        } else {
            "originalAllowed"
        }))
    }
}
fn identity(mismatch: bool) -> AudioDesktopIdentity {
    AudioDesktopIdentity::new(
        if mismatch { 102 } else { 101 },
        1,
        Path::new(r"C:\fixture\voice.exe"),
        "Phone.Fixture!App",
    )
    .unwrap()
}
impl CallVoiceAudio for Context {
    fn voice_target(&self) -> ActionResult<(AudioDesktopIdentity, JsString)> {
        self.log("audio.target");
        if !self.bool("prepared") {
            return Err(DesktopActionError::Unavailable);
        }
        Ok((identity(self.bool("mismatch")), "capture-fixture".into()))
    }
    fn bind(&self, endpoint: &JsString) -> ActionResult<()> {
        self.log(format!("audio.bind:{}", endpoint.to_utf8().unwrap()));
        Ok(())
    }
    fn grant(&self) -> ActionResult<()> {
        self.log("audio.grant");
        Ok(())
    }
    fn clear(&self) -> ActionResult<()> {
        self.log("audio.clear");
        Ok(())
    }
    fn release(&self) -> ActionResult<()> {
        self.log("audio.release");
        Ok(())
    }
    fn arm_recovery(&self) -> ActionResult<()> {
        self.log("audio.arm");
        Ok(())
    }
    fn revoke(&self) {
        self.log("audio.revoke");
    }
}
impl VoiceTime for Context {
    fn now(&self) -> Duration {
        Duration::from_millis(self.now.load(Ordering::SeqCst))
    }
    fn wait(&self, stop: &VoiceStop, duration: Duration) -> ActionResult<bool> {
        let ms = duration.as_millis() as u64;
        self.log(format!("wait:{ms}"));
        if !stop.is_set() {
            self.now.fetch_add(ms, Ordering::SeqCst);
            if ms == 1000 && self.next(|c| &mut c.monitor) == self.int("controlAt") {
                let session = self.session.lock().unwrap().clone().unwrap();
                match self.text("control").as_str() {
                    "stop" => {
                        session.stop();
                    }
                    "pause" => {
                        session.pause_for_transition()?;
                    }
                    "has-stop" => {
                        self.log(format!("has:{}", yes(session.has_original_capture())));
                        session.stop();
                    }
                    _ => {}
                }
            }
        }
        Ok(stop.is_set())
    }
    fn sleep(&self, duration: Duration) -> ActionResult<()> {
        let ms = duration.as_millis() as u64;
        self.log(format!("sleep:{ms}"));
        self.now.fetch_add(ms, Ordering::SeqCst);
        Ok(())
    }
}
struct Device(Arc<Context>);
impl MicroVoiceInput for Device {
    fn close(&self) -> MicroInputResult<MicroClose> {
        panic!("No device lifetime action authorized")
    }
    fn status(&self) -> MicroInputResult<MicroAttachmentStatus> {
        self.0.log("device.status");
        Ok(MicroAttachmentStatus {
            state: "ready".into(),
            generation: Some(self.0.int("deviceGeneration") as i64),
            error_code: None,
        })
    }
    fn send_microphone(
        &self,
        _: bool,
        _: i64,
        _: &dyn Fn() -> MicroInputResult<bool>,
    ) -> MicroInputResult<MicroInputReceipt> {
        panic!("No HID input authorized")
    }
    fn observe_voice(&self, generation: i64) -> MicroInputResult<MicroVoiceObservation> {
        self.0.log(format!("device.observe:{generation}"));
        let count = self.0.next(|c| &mut c.observe);
        Ok(MicroVoiceObservation {
            state: "ready".into(),
            generation,
            dictation_sequence: if count >= self.0.int("dictationAt") {
                1
            } else {
                0
            },
        })
    }
}
struct Runtime(Arc<Context>);
impl CallVoiceRuntime for Runtime {
    fn is_current(&self, _: &AudioDesktopIdentity) -> ActionResult<bool> {
        self.0.log("runtime.current");
        if self.0.text("name") == "one-start-thread" {
            #[link(name = "ole32")]
            unsafe extern "system" {
                fn CoGetApartmentType(kind: *mut i32, qualifier: *mut i32) -> i32;
            }
            let (mut kind, mut qualifier) = (-1, -1);
            assert_eq!(unsafe { CoGetApartmentType(&mut kind, &mut qualifier) }, 0);
            assert_eq!(kind, 1);
            self.0.log("apartment:MTA");
        }
        let count = self.0.next(|c| &mut c.current);
        if count == self.0.int("currentErrorAt") {
            return Err(DesktopActionError::Unavailable);
        }
        Ok(count < self.0.int("currentEndsAt"))
    }
    fn has_exited(&self, _: &AudioDesktopIdentity) -> ActionResult<bool> {
        self.0.log("runtime.exited");
        Ok(self.0.bool("exited"))
    }
    fn capture(
        &self,
        _: &AudioDesktopIdentity,
        endpoint: &JsString,
    ) -> ActionResult<Option<DesktopCaptureCorrelation>> {
        let count = self.0.next(|c| &mut c.capture);
        let captures = self.0.row.get("captures").unwrap().as_array().unwrap();
        let mut value = captures[(count - 1).min(captures.len() - 1)]
            .as_str()
            .unwrap();
        if count >= self.0.int("lateAbsentAt") {
            value = "none".into();
        }
        self.0.log(format!("capture:{value}"));
        if value == "error" {
            return Err(DesktopActionError::Unavailable);
        }
        if value == "null" {
            return Ok(None);
        }
        let state = match value.as_str() {
            "other" | "wrong-endpoint" | "matched-no-identity" => "matched",
            "none-identity" => "none",
            "matched" => "matched",
            "none" => "none",
            "ambiguous" => "ambiguous",
            _ => "unavailable",
        };
        let identity =
            if state == "matched" && value != "matched-no-identity" || value == "none-identity" {
                Some(DesktopCaptureIdentity {
                    instance_id: if value == "other" {
                        "replacement"
                    } else {
                        "session"
                    }
                    .into(),
                    pid: 202,
                    process_start_ticks: if value == "other" { 43 } else { 42 },
                })
            } else {
                None
            };
        Ok(Some(DesktopCaptureCorrelation {
            endpoint_id: if value == "wrong-endpoint" {
                "other-endpoint".into()
            } else {
                endpoint.clone()
            },
            state,
            identity,
        }))
    }
    fn select(
        &self,
        input: &NativeVoiceInput,
        wait: bool,
        original: &dyn Fn() -> ActionResult<bool>,
    ) -> ActionResult<VoiceInputPlan> {
        self.0.log(format!(
            "select:{}:{}",
            yes(wait),
            yes(input.hotkey_fallback().is_some())
        ));
        if !original()? {
            return Ok(VoiceInputPlan::Micro {
                generation: None,
                device: None,
            });
        }
        if input.micro().is_none() {
            return Ok(if input.hotkey_fallback().is_none() {
                VoiceInputPlan::Micro {
                    generation: None,
                    device: None,
                }
            } else {
                VoiceInputPlan::Hotkey
            });
        }
        let selected = self.0.next(|c| &mut c.select);
        Ok(VoiceInputPlan::Micro {
            generation: Some(if selected == 1 {
                1
            } else {
                self.0.int("deviceGeneration") as i64
            }),
            device: Some(Arc::new(Device(self.0.clone()))),
        })
    }
    fn prepare_focus(
        &self,
        plan: &VoiceInputPlan,
        input: &NativeVoiceInput,
        _: &AudioDesktopIdentity,
        original: &dyn Fn() -> ActionResult<bool>,
    ) -> ActionResult<bool> {
        self.0.log(format!(
            "focus:{}:{}",
            plan.method(),
            yes(input.hotkey_fallback().is_some())
        ));
        if self.0.bool("checkAuthority") && !original()? {
            return Ok(false);
        }
        Ok(self.0.bool("focus"))
    }
    fn dispatch(
        &self,
        plan: &VoiceInputPlan,
        input: &NativeVoiceInput,
        _: &AudioDesktopIdentity,
        cleanup: bool,
        before: &dyn Fn() -> ActionResult<bool>,
        release: &dyn Fn() -> ActionResult<bool>,
        prior: Option<MicroGestureResult>,
        dictation: bool,
    ) -> ActionResult<VoiceDispatchResult> {
        self.0.log(format!(
            "dispatch:{}:{}:{}:{}:{}",
            plan.method(),
            yes(cleanup),
            yes(input.hotkey_fallback().is_some()),
            prior.as_ref().map_or("null", |p| p.phase),
            yes(dictation)
        ));
        let count = self.0.next(|c| &mut c.dispatch);
        let values = self.0.row.get("dispatches").unwrap().as_array().unwrap();
        let value = values[(count - 1).min(values.len() - 1)].as_str().unwrap();
        if value == "error" {
            return Err(DesktopActionError::Unavailable);
        }
        let mut phase = match value.as_str() {
            "submitted" => "submitted",
            "outcome_unknown" => "outcome_unknown",
            _ => "not_submitted",
        };
        if self.0.bool("checkAuthority") {
            if !before()? {
                phase = "not_submitted";
            }
            release()?;
        }
        let micro = if plan.method() == "micro" {
            Some(MicroGestureResult {
                phase,
                press: MicroInputReceipt {
                    phase: phase.into(),
                    pressed: true,
                    generation: 1,
                },
                release: None,
                latch_press: None,
                latch_release: None,
            })
        } else {
            prior
        };
        Ok(VoiceDispatchResult {
            phase,
            method: plan.method(),
            hotkey: None,
            micro,
        })
    }
}
fn input(mode: &str, adopt: bool) -> NativeVoiceInput {
    let micro: Option<NativeMicroSettings> = if mode.starts_with("micro") {
        let raw = r#"{"micro":{"usbipExecutable":"C:\\fixture\\usbip.exe","executableHash":"sha256:0000000000000000000000000000000000000000000000000000000000000000","port":3240}}"#;
        Some(
            NativeVoiceInput::read(&parse(raw, 4096).unwrap(), raw)
                .unwrap()
                .micro()
                .unwrap()
                .clone(),
        )
    } else {
        None
    };
    NativeVoiceInput::new(
        micro,
        if matches!(mode, "hotkey" | "micro-fallback") {
            Some(NativeHotkey::new(vec!["control".into(), "alt".into()], 32, false).unwrap())
        } else {
            None
        },
        adopt,
    )
}
fn projected(result: VoiceLifetimeResult) -> IvyJson {
    IvyJson::object([
        ("state", result.state.into()),
        (
            "dispatch",
            result.dispatch.map_or(IvyJson::Null, |d| {
                IvyJson::object([
                    ("phase", d.phase.into()),
                    ("method", d.method.into()),
                    ("micro", d.micro.map_or(IvyJson::Null, |m| m.phase.into())),
                ])
            }),
        ),
        ("reason", result.reason.map_or(IvyJson::Null, Into::into)),
    ])
}
fn snapshot(session: &CallVoiceSession) -> IvyJson {
    let state = session.0.state.lock().unwrap();
    IvyJson::object([
        ("launched", state.launched.into()),
        ("disposed", state.disposed.into()),
        ("preserveAudio", state.preserve_audio.into()),
        ("stopped", session.0.stop.is_set().into()),
        (
            "started",
            session
                .0
                .started
                .result()
                .map_or(IvyJson::Null, |r| projected(r.unwrap())),
        ),
        (
            "finished",
            session
                .0
                .finished
                .result()
                .map_or(IvyJson::Null, |r| projected(r.unwrap())),
        ),
    ])
}
async fn run_case(row: IvyJson) -> IvyJson {
    let ctx = Arc::new(Context {
        row,
        trace: Mutex::new(vec![]),
        now: AtomicU64::new(0),
        counts: Mutex::new(Counts::default()),
        session: Mutex::new(None),
    });
    let original = ctx.clone();
    let baseline = ctx.clone();
    let session = CallVoiceSession::with_time(
        ctx.clone(),
        identity(false),
        input(&ctx.text("mode"), ctx.bool("adopt")),
        Arc::new(Runtime(ctx.clone())),
        Arc::new(move |cleanup| original.original(cleanup)),
        ctx.clone(),
        Some(Arc::new(move |b| {
            baseline.log(format!(
                "baseline:{}:{}:{}:{}",
                b.capture_state.unwrap_or("null"),
                b.reason.unwrap_or("null"),
                b.capture_ms,
                b.age_ms
            ));
            if baseline.bool("baselineError") {
                panic!("Inert diagnostic observer")
            }
        })),
    );
    let mut actions = vec![];
    let constructed = session.is_ok();
    if let Ok(session) = session {
        *ctx.session.lock().unwrap() = Some(session.clone());
        for action in ctx.row.get("actions").unwrap().as_array().unwrap() {
            let action = action.as_str().unwrap();
            let mut value = IvyJson::Null;
            let result = match action.as_str() {
                "run" => {
                    session.0.state.lock().unwrap().launched = true;
                    session.run();
                    Ok(())
                }
                "start" => match session.start() {
                    Ok(start) => {
                        value = projected(start.wait().await.unwrap());
                        session.0.finished.wait().await.map(|_| ())
                    }
                    Err(error) => Err(error),
                },
                "stop" => {
                    if let Some(result) = session.stop().result() {
                        value = projected(result.unwrap());
                    }
                    Ok(())
                }
                "pause" => session.pause_for_transition().map(|pause| {
                    if let Some(result) = pause.result() {
                        value = projected(result.unwrap());
                    }
                }),
                "retire" => session.retire_for_transition().wait().await,
                "retire-drop" => {
                    drop(session.retire_for_transition());
                    Ok(())
                }
                "close" => session.close().wait().await,
                "close-drop" => {
                    drop(session.close());
                    let closing = session.0.state.lock().unwrap().closing.clone().unwrap();
                    closing.wait().await
                }
                "has" => {
                    value = session.has_original_capture().into();
                    Ok(())
                }
                _ => panic!("Unknown fixture action"),
            };
            actions.push(IvyJson::object([
                ("action", action.into()),
                ("ok", result.is_ok().into()),
                ("value", value),
                ("snapshot", snapshot(&session)),
            ]));
        }
        *ctx.session.lock().unwrap() = None;
    }
    let trace = IvyJson::Array(
        ctx.trace
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.clone().into())
            .collect(),
    );
    IvyJson::object([
        ("name", ctx.text("name").into()),
        ("constructed", constructed.into()),
        ("actions", IvyJson::Array(actions)),
        ("trace", trace),
        (
            "now",
            IvyJson::Number(ctx.now.load(Ordering::SeqCst) as f64),
        ),
    ])
}
fn save(name: &str, value: &IvyJson) {
    if let Some(dir) = std::env::var_os("IVY_PHONE_CALL_VOICE_TRACE") {
        fs::write(
            Path::new(&dir).join(name),
            stringify(value, 32 * 1024 * 1024).unwrap(),
        )
        .unwrap();
    }
}
async fn actual_audio_adapter() {
    use crate::media::{
        AudioRoutePermit, AudioSendHealth, AudioSettings, CallAudioError, CallAudioFactory,
        CallAudioRoute, PcmAudioPort, PcmAudioState, PcmSendPort,
    };
    use std::{future::Future, pin::Pin};
    use tokio_util::sync::CancellationToken;
    struct Route(
        Mutex<PcmAudioState>,
        AtomicBool,
        std::sync::atomic::AtomicUsize,
    );
    impl PcmAudioPort for Route {
        fn state(&self) -> PcmAudioState {
            self.2.fetch_add(1, Ordering::SeqCst);
            *self.0.lock().unwrap()
        }
        fn read_captured(&self, out: &mut [f32], _: u64) -> usize {
            out.fill(0.0);
            0
        }
        fn write_received(&self, _: &[f32], _: u64) -> bool {
            false
        }
    }
    impl PcmSendPort for Route {
        fn with_send_state(&self, admitted: PcmAudioState, action: &mut dyn FnMut()) -> bool {
            let state = self.0.lock().unwrap();
            if *state != admitted {
                return false;
            }
            action();
            true
        }
        fn send_health(&self) -> AudioSendHealth {
            AudioSendHealth::Ready
        }
    }
    impl CallAudioRoute for Route {
        fn status(&self) -> IvyJson {
            IvyJson::object([(
                "state",
                if self.state().open {
                    "open"
                } else {
                    "suspended"
                }
                .into(),
            )])
        }
        fn failed(&self) -> bool {
            self.1.load(Ordering::SeqCst)
        }
        fn resume(&self) -> Result<(), CallAudioError> {
            let mut state = self.0.lock().unwrap();
            state.open = true;
            state.generation += 1;
            Ok(())
        }
        fn suspend(&self) -> Result<(), CallAudioError> {
            self.0.lock().unwrap().open = false;
            Ok(())
        }
        fn close(&self) -> Pin<Box<dyn Future<Output = Result<(), CallAudioError>> + Send + '_>> {
            self.0.lock().unwrap().open = false;
            Box::pin(async { Ok(()) })
        }
    }
    struct Factory(Arc<Route>);
    impl CallAudioFactory for Factory {
        fn open<'a>(
            &'a self,
            _: &'a AudioSettings,
            _: Option<&'a AudioDesktopIdentity>,
            _: AudioRoutePermit,
            _: CancellationToken,
        ) -> Pin<
            Box<dyn Future<Output = Result<Arc<dyn CallAudioRoute>, CallAudioError>> + Send + 'a>,
        > {
            Box::pin(async { Ok(self.0.clone() as Arc<dyn CallAudioRoute>) })
        }
    }
    let route = Arc::new(Route(
        Mutex::new(PcmAudioState {
            open: false,
            generation: 0,
        }),
        AtomicBool::new(false),
        std::sync::atomic::AtomicUsize::new(0),
    ));
    let port = CallAudioPort::new(
        "11111111-1111-4111-8111-111111111111".into(),
        Arc::new(Factory(route.clone())),
    )
    .unwrap();
    assert!(port.voice_target().is_err());
    port.prepare(
        AudioSettings {
            capture_endpoint: "capture-fixture".into(),
            render_endpoint: "render-fixture".into(),
            source_mode: "desktop_process".into(),
            capture_buffer_ms: 20,
            render_latency_ms: 10,
            queue_ms: 60,
            mute: None,
            playback_prebuffer_ms: 0,
            enable_wasapi_low_latency: false,
            loopback_render_endpoint: None,
        },
        Some(identity(false)),
        CancellationToken::new(),
    )
    .unwrap()
    .wait()
    .await
    .unwrap();
    let pcm_reads = route.2.load(Ordering::SeqCst);
    let (desktop, endpoint) = CallVoiceAudio::voice_target(&port).unwrap();
    assert_eq!(route.2.load(Ordering::SeqCst), pcm_reads);
    assert!(desktop.same(&identity(false)));
    assert_eq!(endpoint, JsString::from("capture-fixture"));
    assert!(CallVoiceAudio::bind(&port, &"wrong".into()).is_err());
    CallVoiceAudio::bind(&port, &endpoint).unwrap();
    CallVoiceAudio::grant(&port).unwrap();
    CallVoiceAudio::arm_recovery(&port).unwrap();
    CallVoiceAudio::clear(&port).unwrap();
    CallVoiceAudio::release(&port).unwrap();
    route.1.store(true, Ordering::SeqCst);
    assert!(port.voice_target().is_err());
    route.1.store(false, Ordering::SeqCst);
    assert!(port.voice_target().is_err());
    CallVoiceAudio::revoke(&port);
    assert!(port.voice_target().is_err());
    assert!(CallVoiceAudio::bind(&port, &endpoint).is_err());
    port.close().wait().await.unwrap();
}
#[tokio::test]
async fn retained_voice_capture_authority_transitions_and_cleanup() {
    actual_audio_adapter().await;
    let mut rows = vec![];
    for case in cases() {
        rows.push(run_case(case).await);
    }
    let find = |name: &str| {
        rows.iter()
            .find(|r| r.get("name").unwrap().as_str().unwrap() == name)
            .unwrap()
    };
    let first = |name: &str| {
        find(name).get("actions").unwrap().as_array().unwrap()[0]
            .get("snapshot")
            .unwrap()
    };
    assert_eq!(
        first("hotkey-start-stop")
            .get("started")
            .unwrap()
            .get("state")
            .unwrap()
            .as_str()
            .unwrap(),
        "active"
    );
    assert_eq!(
        first("hotkey-start-stop")
            .get("finished")
            .unwrap()
            .get("state")
            .unwrap()
            .as_str()
            .unwrap(),
        "stopped"
    );
    for name in [
        "micro-start-stop",
        "micro-fallback-start-stop",
        "micro-reconnect-before-cleanup",
    ] {
        assert_eq!(
            first(name)
                .get("started")
                .unwrap()
                .get("state")
                .unwrap()
                .as_str()
                .unwrap(),
            "active",
            "{name}"
        );
        assert_eq!(
            first(name)
                .get("finished")
                .unwrap()
                .get("state")
                .unwrap()
                .as_str()
                .unwrap(),
            "stopped",
            "{name}"
        );
    }
    assert_eq!(
        first("baseline-matched")
            .get("started")
            .unwrap()
            .get("state")
            .unwrap()
            .as_str()
            .unwrap(),
        "not_started"
    );
    assert_eq!(
        first("input-outcome_unknown")
            .get("started")
            .unwrap()
            .get("reason")
            .unwrap()
            .as_str()
            .unwrap(),
        "input_outcome_unknown"
    );
    assert_eq!(
        first("dictation-without-fallback")
            .get("started")
            .unwrap()
            .get("reason")
            .unwrap()
            .as_str()
            .unwrap(),
        "micro_dictation_detected"
    );
    let pause = find("pause-retire-and-cached-close")
        .get("trace")
        .unwrap()
        .as_array()
        .unwrap();
    assert!(pause.iter().any(|v| v.as_str().unwrap() == "audio.release"));
    assert!(!pause.iter().any(|v| v.as_str().unwrap() == "audio.revoke"));
    save("call-voice-ordinary.json", &IvyJson::Array(rows));
}
