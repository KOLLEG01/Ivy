use super::*;
use crate::micro::{MicroAttachmentStatus, MicroInputReceipt, MicroInputResult, MicroVoiceInput};
use ivy_contracts::{
    IvyJson,
    hash::digest,
    json::{parse, stringify},
};
use std::{
    collections::HashMap,
    io::Write,
    panic::{AssertUnwindSafe, catch_unwind},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};

fn num(v: i64) -> IvyJson {
    IvyJson::Number(v as f64)
}
fn arr(v: impl IntoIterator<Item = IvyJson>) -> IvyJson {
    IvyJson::Array(v.into_iter().collect())
}
fn text(v: &IvyJson, name: &str) -> String {
    v.get(name).unwrap().as_str().unwrap()
}
fn flag(v: &IvyJson, name: &str) -> bool {
    v.get(name).unwrap().as_bool().unwrap()
}
fn int(v: &IvyJson, name: &str) -> i64 {
    v.get(name).unwrap().as_f64().unwrap() as i64
}
fn fault() -> Box<dyn std::error::Error + Send + Sync> {
    std::io::Error::other("fixture").into()
}
fn capture(f: impl FnOnce() -> ActionResult<IvyJson>) -> IvyJson {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(value)) => IvyJson::object([("ok", true.into()), ("value", value)]),
        value => {
            let error = match value {
                Ok(Err(DesktopActionError::Argument(s) | DesktopActionError::Platform(s))) => s,
                _ => "fixture",
            };
            IvyJson::object([("ok", false.into()), ("error", error.into())])
        }
    }
}
struct State {
    row: IvyJson,
    trace: Vec<String>,
    counts: HashMap<String, usize>,
    sends: usize,
    pauses: usize,
}
impl State {
    fn new(row: &IvyJson) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            row: row.clone(),
            trace: vec![],
            counts: HashMap::new(),
            sends: 0,
            pauses: 0,
        }))
    }
}
fn step(state: &Arc<Mutex<State>>, name: &str) -> ActionResult<bool> {
    let mut s = state.lock().unwrap();
    s.trace.push(name.into());
    let count = *s.counts.entry(name.into()).or_default();
    *s.counts.get_mut(name).unwrap() += 1;
    if int(&s.row, &format!("{name}Fault")) == count as i64 {
        return Err(DesktopActionError::Unavailable);
    }
    let values = s.row.get(name).unwrap().as_array().unwrap();
    Ok(values[count.min(values.len() - 1)].as_bool().unwrap())
}
fn pause(state: &Arc<Mutex<State>>, ms: u64) -> MicroInputResult<()> {
    let mut s = state.lock().unwrap();
    s.trace.push(format!("pause:{ms}"));
    let count = s.pauses;
    s.pauses += 1;
    let fail = int(&s.row, "pauseFault") == count as i64;
    let panic = flag(&s.row, "pausePanic");
    drop(s);
    if fail {
        if panic {
            panic!("fixture pause")
        }
        return Err(fault());
    }
    Ok(())
}
struct Input(Arc<Mutex<State>>);
impl MicroVoiceInput for Input {
    fn close(&self) -> MicroInputResult<crate::micro::MicroClose> {
        panic!("unexpected dispose")
    }
    fn status(&self) -> MicroInputResult<MicroAttachmentStatus> {
        panic!("unexpected status read")
    }
    fn send_microphone(
        &self,
        pressed: bool,
        generation: i64,
        original: &dyn Fn() -> MicroInputResult<bool>,
    ) -> MicroInputResult<MicroInputReceipt> {
        let mut s = self.0.lock().unwrap();
        let count = s.sends;
        s.sends += 1;
        s.trace.push(format!(
            "send:{count}:{}:{generation}",
            if pressed { "down" } else { "up" }
        ));
        let values = s.row.get("phases").unwrap().as_array().unwrap();
        let mut phase = values[count.min(values.len() - 1)].as_str().unwrap();
        let live = s.row.get("live").unwrap().as_array().unwrap();
        let same_generation =
            live[count.min(live.len() - 1)].as_f64().unwrap() as i64 == generation;
        drop(s);
        if phase == "panic" {
            panic!("fixture send")
        }
        if phase == "throw" {
            return Err(fault());
        }
        if !same_generation || !original()? {
            phase = "not_submitted".into();
        }
        Ok(MicroInputReceipt {
            phase,
            pressed,
            generation,
        })
    }
}
fn envelope(result: IvyJson, state: &Arc<Mutex<State>>) -> IvyJson {
    IvyJson::object([
        ("result", result),
        (
            "trace",
            arr(state.lock().unwrap().trace.iter().cloned().map(Into::into)),
        ),
    ])
}
fn gesture(row: &IvyJson) -> IvyJson {
    let state = State::new(row);
    let result = capture(|| {
        crate::micro::dispatch_micro_gesture_with_pause(
            &Input(state.clone()),
            int(row, "generation"),
            flag(row, "cleanup"),
            &|| Ok(step(&state, "before")?),
            &|| Ok(step(&state, "release")?),
            &mut |ms| pause(&state, ms),
        )
        .map(|r| r.to_json())
        .map_err(|_| DesktopActionError::Argument("Exact original Micro generation required."))
    });
    envelope(result, &state)
}
fn settings(row: &IvyJson) -> NativeVoiceInput {
    NativeVoiceInput::new(
        None,
        flag(row, "fallback").then(|| {
            NativeHotkey::new(vec!["control".into()], 32, flag(row, "focusBefore")).unwrap()
        }),
        false,
    )
}
fn plan(row: &IvyJson, state: &Arc<Mutex<State>>) -> VoiceInputPlan {
    if text(row, "method") == "hotkey" {
        VoiceInputPlan::Hotkey
    } else {
        VoiceInputPlan::Micro {
            generation: (!row.get("generation").unwrap().is_null()).then(|| int(row, "generation")),
            device: flag(row, "device")
                .then(|| Arc::new(Input(state.clone())) as Arc<dyn MicroVoiceInput>),
        }
    }
}
fn focus(row: &IvyJson) -> IvyJson {
    let state = State::new(row);
    let result = capture(|| {
        prepare_input_focus(
            &plan(row, &state),
            &settings(row),
            &|| step(&state, "before"),
            &|| step(&state, "current"),
            &mut || {
                state.lock().unwrap().trace.push("focus".into());
                if flag(row, "focusFault") {
                    return Err(DesktopActionError::Unavailable);
                }
                Ok(flag(row, "focusOk"))
            },
        )
        .map(Into::into)
    });
    envelope(result, &state)
}
fn dispatch(row: &IvyJson) -> IvyJson {
    let state = State::new(row);
    let prior = if row.get("prior").unwrap().is_null() {
        None
    } else {
        let phase = match text(row, "prior").as_str() {
            "submitted" => "submitted",
            "not_submitted" => "not_submitted",
            "other" => "other",
            _ => "outcome_unknown",
        };
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
    };
    let result = capture(|| {
        dispatch_voice_with_pause(
            &plan(row, &state),
            &settings(row),
            flag(row, "cleanup"),
            &|| step(&state, "current"),
            &|| step(&state, "before"),
            &|| step(&state, "release"),
            &mut |hotkey, before, prepared| {
                state.lock().unwrap().trace.push(format!(
                    "toggle:{}:{}:{prepared}",
                    hotkey.key_code(),
                    hotkey.focus_before()
                ));
                if flag(row, "toggleFault") {
                    return Err(DesktopActionError::Unavailable);
                }
                let phase = if flag(row, "toggleChecks") && !before()? {
                    "not_submitted"
                } else {
                    match text(row, "hotkeyPhase").as_str() {
                        "submitted" => "submitted",
                        "not_submitted" => "not_submitted",
                        _ => "outcome_unknown",
                    }
                };
                Ok(HotkeyResult {
                    phase,
                    requested: 4,
                    submitted: Some(if phase == "submitted" { 4 } else { 0 }),
                    key_up_submitted: phase == "submitted",
                    error_code: None,
                })
            },
            prior,
            flag(row, "dictation"),
            &mut |ms| pause(&state, ms),
        )
        .map(|r| r.to_json())
    });
    envelope(result, &state)
}
fn parse_settings(raw: &str) -> Result<NativeVoiceInput, NativeConfigError> {
    NativeVoiceInput::read(&parse(raw, 1024 * 1024).unwrap(), raw)
}
fn read(raw: &str) -> IvyJson {
    match parse_settings(raw) {
        Err(error) => IvyJson::object([("ok", false.into()), ("error", error.code().into())]),
        Ok(value) => IvyJson::object([
            ("ok", true.into()),
            (
                "value",
                IvyJson::object([
                    (
                        "micro",
                        value.micro().map_or(IvyJson::Null, |m| {
                            IvyJson::object([
                                ("usbipExecutable", m.executable().into()),
                                ("executableHash", m.executable_hash().into()),
                                ("port", num(m.port().into())),
                            ])
                        }),
                    ),
                    (
                        "hotkeyFallback",
                        value.hotkey_fallback().map_or(IvyJson::Null, |h| {
                            IvyJson::object([
                                (
                                    "modifiers",
                                    arr(h.modifiers().iter().cloned().map(IvyJson::String)),
                                ),
                                ("keyCode", num(h.key_code().into())),
                                ("focusBeforeHotkey", h.focus_before().into()),
                            ])
                        }),
                    ),
                    ("adoptExisting", value.adopt_existing().into()),
                ]),
            ),
        ]),
    }
}
fn baseline() -> IvyJson {
    IvyJson::object([
        ("generation", num(1)),
        ("cleanup", false.into()),
        ("method", "micro".into()),
        ("device", true.into()),
        ("fallback", true.into()),
        ("focusBefore", false.into()),
        ("focusOk", true.into()),
        ("focusFault", false.into()),
        ("toggleFault", false.into()),
        ("toggleChecks", true.into()),
        ("hotkeyPhase", "submitted".into()),
        ("prior", IvyJson::Null),
        ("dictation", false.into()),
        ("pauseFault", num(-1)),
        ("pausePanic", false.into()),
        ("phases", arr(["submitted".into()])),
        ("live", arr([num(1)])),
        ("before", arr([true.into()])),
        ("release", arr([true.into()])),
        ("current", arr([true.into()])),
        ("beforeFault", num(-1)),
        ("releaseFault", num(-1)),
        ("currentFault", num(-1)),
    ])
}
fn configured(micro: &str, fallback: &str, adopt: &str) -> String {
    format!("{{\"micro\":{micro},\"hotkeyFallback\":{fallback},\"adoptExisting\":{adopt}}}")
}
#[test]
fn original_voice_settings_gesture_authority_and_fallback_agree() {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|info| {
        if !info
            .payload()
            .downcast_ref::<&str>()
            .is_some_and(|s| s.starts_with("fixture "))
        {
            eprintln!("{info}");
        }
    }));
    struct Restore(Option<Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if !std::thread::panicking() {
                if let Some(hook) = self.0.take() {
                    std::panic::set_hook(hook);
                }
            }
        }
    }
    let _restore = Restore(Some(hook));
    let hash = "a".repeat(64);
    let micro = format!(
        "{{\"usbipExecutable\":\"C:/fixture/usbip.exe\",\"executableHash\":\"sha256:{hash}\",\"port\":1024}}"
    );
    let hotkey = "{\"modifiers\":[\"control\",\"alt\"],\"keyCode\":32,\"focusBeforeHotkey\":false}";
    let mut raw_settings: Vec<String> = [
        "null",
        "false",
        "0",
        "[]",
        "{}",
        "{\"micro\":null}",
        "{\"micro\":null,\"hotkeyFallback\":null}",
        "{\"Micro\":null}",
        "{\"micro\":null,\"extra\":0}",
        "{\"micro\":null,\"adoptExisting\":null}",
        "{\"micro\":null,\"adoptExisting\":1}",
        "{\"micro\":null,\"adoptExisting\":\"false\"}",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    for m in ["null", "false", "0", "[]", "{}", micro.as_str()] {
        for h in ["null", "false", "0", "[]", "{}", hotkey] {
            for a in ["false", "true", "null", "0"] {
                raw_settings.push(configured(m, h, a));
            }
        }
    }
    for token in [
        "32.0",
        "3.2e1",
        "2147483648",
        "null",
        "\"32\"",
        "false",
        "-0",
        "33",
        "112",
        "135",
        "136",
    ] {
        raw_settings.push(configured(
            "null",
            &hotkey.replace(":32,", &format!(":{token},")),
            "false",
        ));
    }
    for token in [
        "1024.0",
        "1.024e3",
        "2147483648",
        "null",
        "\"1024\"",
        "false",
        "1023",
        "65535",
        "65536",
    ] {
        raw_settings.push(configured(
            &micro.replace(":1024}", &format!(":{token}}}")),
            hotkey,
            "false",
        ));
    }
    for m in [
        "null",
        "[]",
        "[null]",
        "[\"shift\"]",
        "[\"CONTROL\"]",
        "[\"control\",\"control\"]",
        "[\"control\",\"unknown\"]",
        "[\"windows\",\"shift\"]",
    ] {
        raw_settings.push(configured(
            "null",
            &format!("{{\"modifiers\":{m},\"keyCode\":32,\"focusBeforeHotkey\":true}}"),
            "false",
        ));
    }
    raw_settings.extend([
        "{\"m\\u0069cro\":null}".into(),
        configured(&micro, hotkey, "true"),
        configured(
            &micro.replace(&format!("{hash}\""), &format!("{hash}\\n\"")),
            hotkey,
            "false",
        ),
    ]);
    for m in [
        micro.replace(":1024}", ":1023}"),
        micro.replace("\"C:/fixture/usbip.exe\"", "null"),
    ] {
        for h in [
            hotkey,
            "{\"modifiers\":null,\"keyCode\":32,\"focusBeforeHotkey\":false}",
            "{\"modifiers\":[null],\"keyCode\":32,\"focusBeforeHotkey\":false}",
        ] {
            for a in ["false", "null", "0"] {
                raw_settings.push(configured(&m, h, a));
            }
        }
    }
    let mut matching = vec![];
    for m in ["null", micro.as_str()] {
        for h in [
            "null",
            hotkey,
            "{\"modifiers\":[\"alt\",\"control\"],\"keyCode\":32,\"focusBeforeHotkey\":false}",
            "{\"modifiers\":[\"control\",\"alt\"],\"keyCode\":65,\"focusBeforeHotkey\":true}",
        ] {
            for a in ["true", "false"] {
                matching.push(configured(m, h, a));
            }
        }
    }
    let mut gestures = vec![];
    for cleanup in [false, true] {
        for generation in [1, 9_007_199_254_740_991] {
            for a in [
                "submitted",
                "not_submitted",
                "outcome_unknown",
                "other",
                "throw",
            ] {
                for b in [
                    "submitted",
                    "not_submitted",
                    "outcome_unknown",
                    "other",
                    "throw",
                ] {
                    for c in [
                        "submitted",
                        "not_submitted",
                        "outcome_unknown",
                        "other",
                        "throw",
                    ] {
                        for d in [
                            "submitted",
                            "not_submitted",
                            "outcome_unknown",
                            "other",
                            "throw",
                        ] {
                            let mut row = baseline();
                            row.insert("cleanup", cleanup.into());
                            row.insert("generation", num(generation));
                            row.insert("live", arr([num(generation)]));
                            row.insert("phases", arr([a, b, c, d].map(Into::into)));
                            gestures.push(row);
                        }
                    }
                }
            }
        }
    }
    for cleanup in [false, true] {
        for fault_at in [0, 1, 2] {
            for panic in [false, true] {
                let mut r = baseline();
                r.insert("cleanup", cleanup.into());
                r.insert("pauseFault", num(fault_at));
                r.insert("pausePanic", panic.into());
                gestures.push(r);
            }
        }
        for name in ["before", "release"] {
            for at in [0, 1] {
                let mut r = baseline();
                r.insert("cleanup", cleanup.into());
                r.insert(format!("{name}Fault"), num(at));
                gestures.push(r);
            }
        }
        for at in 0..4 {
            for mode in ["generation", "panic"] {
                let mut r = baseline();
                r.insert("cleanup", cleanup.into());
                if mode == "generation" {
                    let mut live = vec![num(1); 4];
                    for value in live.iter_mut().skip(at) {
                        *value = num(2);
                    }
                    r.insert("live", arr(live));
                } else {
                    let mut phases = vec!["submitted".into(); 4];
                    phases[at] = "panic".into();
                    r.insert("phases", arr(phases));
                }
                gestures.push(r);
            }
        }
        for generation in [-1, 0, 9_007_199_254_740_992] {
            let mut r = baseline();
            r.insert("generation", num(generation));
            gestures.push(r);
        }
    }
    let mut focus_rows = vec![];
    for method in ["micro", "hotkey"] {
        for fallback in [false, true] {
            for focus_before in [false, true] {
                for before in [false, true] {
                    for current in [false, true] {
                        for ok in [false, true] {
                            for stage in ["none", "before", "current", "focus"] {
                                let mut r = baseline();
                                r.insert("method", method.into());
                                r.insert("fallback", fallback.into());
                                r.insert("focusBefore", focus_before.into());
                                r.insert("before", arr([before.into()]));
                                r.insert("current", arr([current.into()]));
                                r.insert("focusOk", ok.into());
                                if stage == "focus" {
                                    r.insert("focusFault", true.into());
                                } else if stage != "none" {
                                    r.insert(format!("{stage}Fault"), num(0));
                                }
                                focus_rows.push(r);
                            }
                        }
                    }
                }
            }
        }
    }
    let mut dispatch_rows = vec![];
    for method in ["micro", "hotkey"] {
        for fallback in [false, true] {
            for cleanup in [false, true] {
                for prior in [
                    IvyJson::Null,
                    "not_submitted".into(),
                    "submitted".into(),
                    "outcome_unknown".into(),
                    "other".into(),
                ] {
                    for dictation in [false, true] {
                        for mode in [
                            "normal",
                            "no_generation",
                            "no_device",
                            "current_false",
                            "before_false",
                            "release_false",
                            "current_throw",
                            "before_throw",
                            "release_throw",
                            "press_throw",
                            "release_throw_send",
                            "toggle_throw",
                        ] {
                            let mut r = baseline();
                            r.insert("method", method.into());
                            r.insert("fallback", fallback.into());
                            r.insert("cleanup", cleanup.into());
                            r.insert("prior", prior.clone());
                            r.insert("dictation", dictation.into());
                            match mode {
                                "no_generation" => r.insert("generation", IvyJson::Null),
                                "no_device" => r.insert("device", false.into()),
                                "current_false" | "before_false" | "release_false" => r.insert(
                                    mode.strip_suffix("_false").unwrap(),
                                    arr([false.into()]),
                                ),
                                "current_throw" | "before_throw" | "release_throw" => r.insert(
                                    format!("{}Fault", mode.strip_suffix("_throw").unwrap()),
                                    num(0),
                                ),
                                "press_throw" => r.insert("phases", arr(["throw".into()])),
                                "release_throw_send" => {
                                    r.insert("phases", arr(["submitted".into(), "throw".into()]))
                                }
                                "toggle_throw" => r.insert("toggleFault", true.into()),
                                _ => (),
                            };
                            dispatch_rows.push(r);
                        }
                    }
                }
            }
        }
    }
    let input = IvyJson::object([
        (
            "settings",
            arr(raw_settings.iter().cloned().map(Into::into)),
        ),
        ("matching", arr(matching.iter().cloned().map(Into::into))),
        ("gestures", arr(gestures.clone())),
        ("focus", arr(focus_rows.clone())),
        ("dispatch", arr(dispatch_rows.clone())),
    ]);
    let encoded = stringify(&input, 16 * 1024 * 1024).unwrap();
    let prefix = std::env::var("IVY_PHONE_VOICE_INPUT_TRACE").unwrap();
    std::fs::write(format!("{prefix}.input.json"), &encoded).unwrap();
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/phone-voice-input-oracle.ps1");
    let mut command =
        Command::new(std::env::var("IVY_POWERSHELL").unwrap_or_else(|_| "pwsh".into()));
    use std::os::windows::process::CommandExt;
    command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    let mut child = command
        .args(["-NoProfile", "-File"])
        .arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let copy = encoded.clone();
    let writer = std::thread::spawn(move || stdin.write_all(copy.as_bytes()).unwrap());
    let output = child.wait_with_output().unwrap();
    let written = writer.join();
    std::fs::write(format!("{prefix}.original.json"), &output.stdout).unwrap();
    std::fs::write(format!("{prefix}.original.err.log"), &output.stderr).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    written.unwrap();
    let original = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        32 * 1024 * 1024,
    )
    .unwrap();
    assert_eq!(
        original
            .get("inputSha256")
            .unwrap()
            .as_str()
            .unwrap()
            .to_ascii_lowercase(),
        digest(encoded.as_bytes()).trim_start_matches("sha256:")
    );
    let mut results = vec![];
    for (name, expected) in [
        (
            "settings",
            raw_settings.iter().map(|s| read(s)).collect::<Vec<_>>(),
        ),
        ("gestures", gestures.iter().map(gesture).collect()),
        ("focus", focus_rows.iter().map(focus).collect()),
        ("dispatch", dispatch_rows.iter().map(dispatch).collect()),
    ] {
        let actual = original.get(name).unwrap().as_array().unwrap();
        std::fs::write(
            format!("{prefix}.{name}.rust.json"),
            stringify(&arr(expected.clone()), 32 * 1024 * 1024).unwrap(),
        )
        .unwrap();
        assert_eq!(actual.len(), expected.len());
        for (i, (a, b)) in actual.iter().zip(expected).enumerate() {
            assert_eq!(a, &b, "{name}[{i}]");
        }
        results.push(IvyJson::object([
            ("name", name.into()),
            ("comparisons", num(actual.len() as i64)),
        ]));
    }
    let values = matching
        .iter()
        .map(|raw| parse_settings(raw).unwrap())
        .collect::<Vec<_>>();
    let mut matched = vec![];
    for a in &values {
        matched.push(a.matches(None));
        for b in &values {
            matched.push(a.matches(Some(b)));
        }
    }
    assert_eq!(
        original.get("matches").unwrap().get("matches"),
        Some(&arr(matched.iter().copied().map(Into::into)))
    );
    assert_eq!(
        original.get("matches").unwrap().get("snapshotStable"),
        Some(&true.into())
    );
    let hotkey = NativeHotkey::new(vec!["control".into(), "alt".into()], 32, false).unwrap();
    let snapshot = NativeVoiceInput::new(None, Some(hotkey), false).clone();
    assert_eq!(
        snapshot.hotkey_fallback().unwrap().modifiers(),
        [JsString::from("control"), JsString::from("alt")]
    );
    let observed = Input(State::new(&baseline()))
        .observe_voice(9_007_199_254_740_991)
        .unwrap();
    assert_eq!(
        original.get("matches").unwrap().get("observed"),
        Some(&IvyJson::object([
            ("state", observed.state.into()),
            ("generation", num(observed.generation)),
            ("dictationSequence", num(observed.dictation_sequence))
        ]))
    );
    results.push(IvyJson::object([
        ("name", "matches".into()),
        ("comparisons", num(matched.len() as i64)),
    ]));
    std::fs::write(
        format!("{prefix}.comparisons.json"),
        stringify(&arr(results), 1024 * 1024).unwrap(),
    )
    .unwrap();
}
