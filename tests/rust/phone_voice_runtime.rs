use super::*;
use crate::{
    desktop::NativeHotkey,
    micro::{MicroAttachmentStatus, MicroInputReceipt, MicroVoiceInput},
};
use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
};
use std::{collections::HashMap, fs, path::Path, sync::Mutex};

fn n(v: i64) -> IvyJson {
    IvyJson::Number(v as f64)
}
fn set(row: &mut IvyJson, key: &str, value: IvyJson) {
    let IvyJson::Object(fields) = row else {
        panic!()
    };
    fields.iter_mut().find(|(k, _)| k == &key.into()).unwrap().1 = value;
}
fn array(values: &[&str]) -> IvyJson {
    IvyJson::Array(values.iter().map(|v| (*v).into()).collect())
}
fn row(name: &str, mode: &str) -> IvyJson {
    IvyJson::object([
        ("name", name.into()),
        ("mode", mode.into()),
        ("focusBefore", true.into()),
        ("cleanup", false.into()),
        ("dictation", false.into()),
        ("fault", "".into()),
        ("prior", "".into()),
        ("status", "ready".into()),
        ("factory", "device".into()),
        ("capture", "none".into()),
        ("endpoint", "capture-fixture".into()),
        ("exited", false.into()),
        ("before", IvyJson::Array(vec![true.into()])),
        ("release", IvyJson::Array(vec![true.into()])),
        ("current", IvyJson::Array(vec![true.into()])),
        ("focus", IvyJson::Array(vec![true.into()])),
        ("focused", IvyJson::Array(vec![true.into()])),
        ("idle", IvyJson::Array(vec![true.into()])),
        ("sent", IvyJson::Array(vec![n(4), n(2)])),
        ("phases", array(&["submitted"])),
        (
            "actions",
            array(&[
                "current", "exited", "capture", "select", "focus", "dispatch", "close", "close",
            ]),
        ),
    ])
}
fn cases() -> Vec<IvyJson> {
    let mut rows = vec![];
    for mode in ["hotkey", "micro", "micro-fallback", "none"] {
        rows.push(row(mode, mode));
    }
    let mut c = row("cleanup-gesture", "micro");
    set(&mut c, "cleanup", true.into());
    rows.push(c);
    let mut c = row("focus-not-requested", "hotkey");
    set(&mut c, "focusBefore", false.into());
    rows.push(c);
    for name in ["before", "release", "current", "focus", "focused", "idle"] {
        let mut c = row(&format!("{name}-refused"), "hotkey");
        set(&mut c, name, IvyJson::Array(vec![false.into()]));
        rows.push(c);
    }
    for fault in [
        "owner:101:504911232000000123",
        "keyboard",
        "current",
        "focus",
        "focused",
        "idle",
        "send",
        "capture:101:capture-fixture",
        "exited:101",
    ] {
        let mut c = row(&format!("fault-{fault}"), "hotkey");
        set(&mut c, "fault", fault.into());
        rows.push(c);
    }
    for (name, values) in [
        ("partial-release", vec![n(1), n(2)]),
        ("partial-unreleased", vec![n(1), n(0)]),
        ("rejected", vec![n(0)]),
        ("count-out-of-range", vec![n(9), n(2)]),
    ] {
        let mut c = row(name, "hotkey");
        set(&mut c, "sent", IvyJson::Array(values));
        rows.push(c);
    }
    for mode in ["micro", "micro-fallback"] {
        for factory in ["null", "io", "error"] {
            let mut c = row(&format!("{mode}-{factory}"), mode);
            set(&mut c, "factory", factory.into());
            rows.push(c);
        }
    }
    for state in ["unavailable", "attached"] {
        let mut c = row(&format!("status-{state}"), "micro-fallback");
        set(&mut c, "status", state.into());
        rows.push(c);
    }
    for phase in ["not_submitted", "outcome_unknown"] {
        let mut c = row(&format!("micro-{phase}"), "micro");
        set(&mut c, "phases", array(&[phase]));
        rows.push(c);
    }
    for fault in ["status", "micro", "device.close"] {
        let mut c = row(&format!("micro-{fault}-fault"), "micro");
        set(&mut c, "fault", fault.into());
        rows.push(c);
    }
    for key in ["before", "release", "current"] {
        let mut c = row(&format!("micro-{key}-refused"), "micro");
        set(&mut c, key, IvyJson::Array(vec![false.into()]));
        rows.push(c);
    }
    for (prior, dictation) in [
        ("not_submitted", false),
        ("outcome_unknown", false),
        ("submitted", false),
        ("submitted", true),
    ] {
        let mut c = row(&format!("prior-{prior}-{dictation}"), "hotkey");
        set(&mut c, "prior", prior.into());
        set(&mut c, "dictation", dictation.into());
        rows.push(c);
    }
    let mut c = row("close-then-reselect", "micro-fallback");
    set(
        &mut c,
        "actions",
        array(&["select", "close", "close", "select", "focus", "dispatch"]),
    );
    rows.push(c);
    for capture in ["null", "unavailable"] {
        let mut c = row(&format!("capture-{capture}"), "none");
        set(&mut c, "capture", capture.into());
        set(&mut c, "actions", array(&["capture", "capture", "exited"]));
        rows.push(c);
    }
    let mut c = row("proven-exit", "none");
    set(&mut c, "exited", true.into());
    set(&mut c, "actions", array(&["exited", "exited"]));
    rows.push(c);
    for key in ["before", "current"] {
        let mut c = row(&format!("{key}-changes-during-focus"), "hotkey");
        set(&mut c, key, IvyJson::Array(vec![true.into(), false.into()]));
        rows.push(c);
    }
    for (name, endpoint) in [
        ("empty", String::new()),
        ("blank", " ".into()),
        ("control", "capture\nfixture".into()),
        ("over-bound", "x".repeat(1025)),
    ] {
        let mut c = row(&format!("trusted-voice-endpoint-{name}"), "none");
        set(&mut c, "endpoint", endpoint.into());
        set(&mut c, "actions", array(&["capture", "capture"]));
        rows.push(c);
    }
    rows
}
struct Context {
    row: IvyJson,
    trace: Vec<String>,
    counts: HashMap<String, usize>,
}
impl Context {
    fn text(&self, key: &str) -> String {
        self.row.get(key).unwrap().as_str().unwrap()
    }
    fn flag(&self, key: &str) -> bool {
        self.row.get(key).unwrap().as_bool().unwrap()
    }
    fn next(&mut self, key: &str) -> usize {
        let n = self.counts.entry(key.into()).or_default();
        let old = *n;
        *n += 1;
        old
    }
    fn event(&mut self, key: &str) -> ActionResult<()> {
        self.trace.push(key.into());
        if self.text("fault") == key {
            Err(DesktopActionError::Unavailable)
        } else {
            Ok(())
        }
    }
    fn step(&mut self, key: &str) -> ActionResult<bool> {
        self.event(key)?;
        let index = self.next(key);
        let values = self.row.get(key).unwrap().as_array().unwrap();
        Ok(values[index.min(values.len() - 1)].as_bool().unwrap())
    }
}
type Shared = Arc<Mutex<Context>>;
struct OwnerInput(Shared);
impl DesktopOwner for OwnerInput {
    fn is_current(&mut self) -> ActionResult<bool> {
        self.0.lock().unwrap().step("current")
    }
    fn try_focus(&mut self) -> ActionResult<bool> {
        self.0.lock().unwrap().step("focus")
    }
    fn is_focused(&mut self) -> ActionResult<bool> {
        self.0.lock().unwrap().step("focused")
    }
}
struct KeyboardInputFixture(Shared);
impl KeyboardInput for KeyboardInputFixture {
    fn is_idle(&mut self) -> ActionResult<bool> {
        self.0.lock().unwrap().step("idle")
    }
    fn send(&mut self, keys: &[KeyStroke]) -> ActionResult<i32> {
        let mut ctx = self.0.lock().unwrap();
        ctx.event(&format!(
            "keys:{}",
            keys.iter()
                .map(|k| format!("{}:{}", k.key, if k.up { "up" } else { "down" }))
                .collect::<Vec<_>>()
                .join(",")
        ))?;
        if ctx.text("fault") == "send" {
            return Err(DesktopActionError::Unavailable);
        }
        let i = ctx.next("sent");
        let v = ctx.row.get("sent").unwrap().as_array().unwrap();
        Ok(v[i.min(v.len() - 1)].as_f64().unwrap() as i32)
    }
}
struct Device(Shared);
impl MicroVoiceInput for Device {
    fn close(&self) -> MicroInputResult<MicroClose> {
        self.0.lock().unwrap().event("device.close")?;
        Ok(MicroClose::completed(Ok(())))
    }
    fn status(&self) -> MicroInputResult<MicroAttachmentStatus> {
        let mut ctx = self.0.lock().unwrap();
        ctx.event("status")?;
        let state = ctx.text("status");
        Ok(MicroAttachmentStatus {
            generation: if state == "ready" { Some(7) } else { None },
            state,
            error_code: None,
        })
    }
    fn send_microphone(
        &self,
        pressed: bool,
        generation: i64,
        original: &dyn Fn() -> MicroInputResult<bool>,
    ) -> MicroInputResult<MicroInputReceipt> {
        let mut ctx = self.0.lock().unwrap();
        ctx.event(&format!(
            "micro:{}:{generation}",
            if pressed { "down" } else { "up" }
        ))?;
        if ctx.text("fault") == "micro" {
            return Err(io::Error::other("fixture").into());
        }
        let i = ctx.next("phases");
        let v = ctx.row.get("phases").unwrap().as_array().unwrap();
        let mut phase = v[i.min(v.len() - 1)].as_str().unwrap();
        drop(ctx);
        if !original()? {
            phase = "not_submitted".into()
        }
        Ok(MicroInputReceipt {
            phase,
            pressed,
            generation,
        })
    }
}
fn identity() -> AudioDesktopIdentity {
    AudioDesktopIdentity::new(
        101,
        504_911_232_000_000_123,
        Path::new(r"C:\fixture\voice.exe"),
        "Phone.Fixture!App",
    )
    .unwrap()
}
fn settings(ctx: &Context) -> NativeVoiceInput {
    let mode = ctx.text("mode");
    let micro = if mode.starts_with("micro") {
        let raw = r#"{"micro":{"usbipExecutable":"C:\\fixture\\usbip.exe","executableHash":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","port":3240}}"#;
        let json = parse(raw, 65536).unwrap();
        Some(
            NativeVoiceInput::read(&json, raw)
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
        if mode == "hotkey" || mode == "micro-fallback" {
            Some(NativeHotkey::new(vec!["control".into()], 32, ctx.flag("focusBefore")).unwrap())
        } else {
            None
        },
        false,
    )
}
fn prior(phase: &str) -> Option<MicroGestureResult> {
    if phase.is_empty() {
        None
    } else {
        let phase = match phase {
            "submitted" => "submitted",
            "not_submitted" => "not_submitted",
            _ => "outcome_unknown",
        };
        Some(MicroGestureResult {
            phase,
            press: MicroInputReceipt {
                phase: phase.into(),
                pressed: true,
                generation: 7,
            },
            release: None,
            latch_press: None,
            latch_release: None,
        })
    }
}
async fn run_case(row: IvyJson) -> IvyJson {
    let ctx = Arc::new(Mutex::new(Context {
        row: row.clone(),
        trace: vec![],
        counts: HashMap::new(),
    }));
    let input = VoiceInputOwner::new({
        let ctx = ctx.clone();
        move |config| {
            let mut state = ctx.lock().unwrap();
            state
                .event(&format!("factory:{}", config.port()))
                .map_err(|e| super::super::MicroFactoryError::Other(e.into()))?;
            match state.text("factory").as_str() {
                "null" => Ok(None),
                "io" => Err(super::super::MicroFactoryError::ClientUnavailable),
                "error" => Err(super::super::MicroFactoryError::Other(
                    io::Error::other("fixture").into(),
                )),
                _ => Ok(Some(Arc::new(Device(ctx.clone())))),
            }
        }
    });
    let desktop =
        PhoneDesktopRuntime::with_application(|_| panic!("No application effect permitted"))
            .with_capture_owner({
                let ctx = ctx.clone();
                move |id, endpoint| {
                    let mut state = ctx.lock().unwrap();
                    state.event(&format!(
                        "capture:{}:{}",
                        id.pid(),
                        endpoint.to_utf8().unwrap()
                    ))?;
                    Ok(match state.text("capture").as_str() {
                        "null" => None,
                        "unavailable" => Some(DesktopCaptureCorrelation {
                            endpoint_id: endpoint.clone(),
                            state: "unavailable",
                            identity: None,
                        }),
                        _ => Some(DesktopCaptureCorrelation {
                            endpoint_id: endpoint.clone(),
                            state: "none",
                            identity: None,
                        }),
                    })
                }
            });
    let runtime = DesktopVoiceRuntime::new(desktop, input)
        .with_owner({
            let ctx = ctx.clone();
            move |id| {
                ctx.lock().unwrap().event(&format!(
                    "owner:{}:{}",
                    id.pid(),
                    id.creation_ticks()
                ))?;
                Ok(Box::new(OwnerInput(ctx.clone())))
            }
        })
        .with_keyboard({
            let ctx = ctx.clone();
            move || {
                ctx.lock().unwrap().event("keyboard")?;
                Ok(Box::new(KeyboardInputFixture(ctx.clone())))
            }
        })
        .with_exited({
            let ctx = ctx.clone();
            move |id| {
                let mut state = ctx.lock().unwrap();
                state.event(&format!("exited:{}", id.pid()))?;
                Ok(state.flag("exited"))
            }
        });
    let settings = settings(&ctx.lock().unwrap());
    let identity = identity();
    let before = || ctx.lock().unwrap().step("before");
    let release = || ctx.lock().unwrap().step("release");
    let mut plan = None;
    let mut closing: Option<MicroClose> = None;
    let mut actions = vec![];
    for action in row.get("actions").unwrap().as_array().unwrap() {
        let action = action.as_str().unwrap();
        let result: ActionResult<IvyJson> = match action.as_str() {
            "current" => runtime.is_current(&identity).map(Into::into),
            "exited" => runtime.has_exited(&identity).map(Into::into),
            "capture" => runtime
                .capture(
                    &identity,
                    &row.get("endpoint").unwrap().as_js_string().unwrap().clone(),
                )
                .map(|v| v.map_or(IvyJson::Null, |v| v.to_json())),
            "select" => runtime.select(&settings, false, &before).map(|p| {
                let value = IvyJson::object([
                    ("method", p.method().into()),
                    (
                        "generation",
                        match &p {
                            VoiceInputPlan::Micro {
                                generation: Some(g),
                                ..
                            } => n(*g),
                            _ => IvyJson::Null,
                        },
                    ),
                    (
                        "device",
                        matches!(
                            &p,
                            VoiceInputPlan::Micro {
                                device: Some(_),
                                ..
                            }
                        )
                        .into(),
                    ),
                ]);
                plan = Some(p);
                value
            }),
            "focus" => plan
                .as_ref()
                .ok_or(DesktopActionError::Unavailable)
                .and_then(|p| runtime.prepare_focus(p, &settings, &identity, &before))
                .map(Into::into),
            "dispatch" => plan
                .as_ref()
                .ok_or(DesktopActionError::Unavailable)
                .and_then(|p| {
                    let state = ctx.lock().unwrap();
                    let cleanup = state.flag("cleanup");
                    let dictation = state.flag("dictation");
                    let prior = prior(&state.text("prior"));
                    drop(state);
                    runtime.dispatch(
                        p, &settings, &identity, cleanup, &before, &release, prior, dictation,
                    )
                })
                .map(|v| v.to_json()),
            _ => match runtime.clone().close() {
                Ok(work) => {
                    let same = closing.as_ref().is_some_and(|p| p.same_work(&work));
                    closing = Some(work.clone());
                    work.wait()
                        .await
                        .map(|_| {
                            IvyJson::object([("same", same.into()), ("complete", true.into())])
                        })
                        .map_err(|_| DesktopActionError::Unavailable)
                }
                Err(_) => Err(DesktopActionError::Unavailable),
            },
        };
        actions.push(IvyJson::object([
            ("action", action.into()),
            (
                "result",
                IvyJson::object([
                    ("ok", result.is_ok().into()),
                    ("value", result.unwrap_or(IvyJson::Null)),
                ]),
            ),
        ]));
    }
    let state = ctx.lock().unwrap();
    IvyJson::object([
        ("name", state.text("name").into()),
        ("actions", IvyJson::Array(actions)),
        (
            "trace",
            IvyJson::Array(state.trace.iter().map(|s| s.as_str().into()).collect()),
        ),
    ])
}
fn save(name: &str, value: &IvyJson) {
    if let Some(dir) = std::env::var_os("IVY_PHONE_VOICE_RUNTIME_TRACE") {
        fs::write(
            Path::new(&dir).join(name),
            stringify(value, 8 * 1024 * 1024).unwrap(),
        )
        .unwrap();
    }
}
fn exit_observation_boundaries() -> (Vec<IvyJson>, Vec<IvyJson>) {
    struct Probe {
        exited: Option<bool>,
        created: Option<u64>,
        trace: Arc<Mutex<Vec<&'static str>>>,
    }
    impl Drop for Probe {
        fn drop(&mut self) {
            self.trace.lock().unwrap().push("drop");
        }
    }
    impl ExitObservation for Probe {
        fn has_exited(&mut self) -> io::Result<bool> {
            self.trace.lock().unwrap().push("exited");
            self.exited.ok_or_else(|| io::Error::other("unavailable"))
        }
        fn created_filetime(&mut self) -> io::Result<u64> {
            self.trace.lock().unwrap().push("created");
            self.created
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "disappeared after open"))
        }
    }
    let desktop = identity();
    let mut rows = vec![];
    let mut inputs = vec![];
    for (name, open, exited, created, expected, calls) in [
        ("missing", 0, Some(false), Some(123), true, vec!["open"]),
        (
            "inaccessible",
            1,
            Some(false),
            Some(123),
            false,
            vec!["open"],
        ),
        (
            "exit-read-failure",
            2,
            None,
            Some(123),
            false,
            vec!["open", "exited", "drop"],
        ),
        (
            "exited",
            2,
            Some(true),
            None,
            true,
            vec!["open", "exited", "drop"],
        ),
        (
            "creation-read-failure",
            2,
            Some(false),
            None,
            false,
            vec!["open", "exited", "created", "drop"],
        ),
        (
            "same-process",
            2,
            Some(false),
            Some(123),
            false,
            vec!["open", "exited", "created", "drop"],
        ),
        (
            "replacement",
            2,
            Some(false),
            Some(124),
            true,
            vec!["open", "exited", "created", "drop"],
        ),
        (
            "overflow",
            2,
            Some(false),
            Some(u64::MAX),
            true,
            vec!["open", "exited", "created", "drop"],
        ),
        (
            "invalid-date",
            2,
            Some(false),
            Some(3_155_378_975_999_999_999),
            true,
            vec!["open", "exited", "created", "drop"],
        ),
    ] {
        let trace = Arc::new(Mutex::new(vec![]));
        let result = process_exited(&desktop, |pid| {
            assert_eq!(pid, 101);
            trace.lock().unwrap().push("open");
            match open {
                0 => Ok(None),
                1 => Err(io::Error::new(io::ErrorKind::PermissionDenied, "fixture")),
                _ => Ok(Some(Probe {
                    exited,
                    created,
                    trace: trace.clone(),
                })),
            }
        });
        assert_eq!(result, expected, "{name}");
        assert_eq!(*trace.lock().unwrap(), calls, "{name}");
        inputs.push(IvyJson::object([
            ("name", name.into()),
            ("open", n(open)),
            ("exited", exited.map_or(IvyJson::Null, Into::into)),
            (
                "created",
                created.map_or(IvyJson::Null, |v| v.to_string().into()),
            ),
        ]));
        rows.push(IvyJson::object([
            ("name", name.into()),
            ("exited", result.into()),
            ("trace", array(&calls)),
        ]));
    }
    save(
        "voice-runtime-exit-boundaries.json",
        &IvyJson::Array(rows.clone()),
    );
    (inputs, rows)
}

#[tokio::test]
async fn composed_owner_input_focus_dispatch_and_cleanup_preserve_boundaries() {
    exit_observation_boundaries();
    let mut rows = vec![];
    for row in cases() {
        rows.push(run_case(row).await)
    }
    assert!(rows.len() > 40);
    let hotkey = &rows[0];
    assert!(
        hotkey
            .get("trace")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap().starts_with("keys:"))
    );
    save("voice-runtime-ordinary.json", &IvyJson::Array(rows));
}
