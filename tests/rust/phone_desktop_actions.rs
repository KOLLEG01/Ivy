use super::*;
use crate::desktop::*;
use ivy_contracts::{
    IvyJson,
    json::{JsString, parse, stringify},
};
use std::{
    cell::{Cell, RefCell},
    ffi::{CStr, c_char, c_void},
    path::Path,
    rc::Rc,
};
const LIMIT: usize = 32 * 1024 * 1024;
fn n(value: i32) -> IvyJson {
    IvyJson::Number(value.into())
}
fn array(values: impl IntoIterator<Item = IvyJson>) -> IvyJson {
    IvyJson::Array(values.into_iter().collect())
}
fn strings(values: &[&str]) -> IvyJson {
    array(values.iter().map(|s| (*s).into()))
}
fn bools(values: &[bool]) -> IvyJson {
    array(values.iter().map(|b| (*b).into()))
}
fn num(row: &IvyJson, key: &str) -> i32 {
    row.get(key).unwrap().as_f64().unwrap() as i32
}
fn flag(row: &IvyJson, key: &str) -> bool {
    row.get(key).unwrap().as_bool().unwrap()
}
fn string(row: &IvyJson, key: &str) -> String {
    row.get(key).unwrap().as_str().unwrap()
}
fn names(row: &IvyJson) -> Vec<JsString> {
    row.get("modifiers")
        .unwrap()
        .as_array()
        .map(|a| {
            a.iter()
                .map(|s| s.as_js_string().unwrap().clone())
                .collect()
        })
        .unwrap_or_default()
}
fn yes(row: &IvyJson, key: &str, index: usize) -> bool {
    let v = row.get(key).unwrap().as_array().unwrap();
    v[index.min(v.len() - 1)].as_bool().unwrap()
}
fn capture(f: impl FnOnce() -> ActionResult<IvyJson>) -> IvyJson {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(Ok(v)) => IvyJson::object([("ok", true.into()), ("value", v)]),
        Ok(Err(DesktopActionError::Argument(e))) => {
            IvyJson::object([("ok", false.into()), ("error", e.into())])
        }
        _ => IvyJson::object([("ok", false.into()), ("error", "fixture".into())]),
    }
}
type Trace = Rc<RefCell<Vec<IvyJson>>>;
fn log(trace: &Trace, value: impl Into<String>) {
    trace.borrow_mut().push(value.into().into());
}
fn with_trace(result: IvyJson, trace: Trace) -> IvyJson {
    IvyJson::object([
        ("result", result),
        ("trace", IvyJson::Array(trace.borrow().clone())),
    ])
}
fn target(path: &str, application: &str) -> crate::media::AudioDesktopIdentity {
    crate::media::AudioDesktopIdentity::new(123, 1, Path::new(path), application).unwrap()
}
struct App<'a> {
    row: &'a IvyJson,
    trace: Trace,
    image: &'a str,
}
impl DesktopApplication for App<'_> {
    fn observe(&mut self) -> ActionResult<Option<DesktopObservation>> {
        log(&self.trace, "observe");
        if string(self.row, "fault") == "observe" {
            return Err(DesktopActionError::Unavailable);
        }
        Ok(match string(self.row, "state").as_str() {
            "null" => None,
            "unavailable" => Some(DesktopObservation::unavailable()),
            "ambiguous" => Some(DesktopObservation::select(
                true,
                vec![
                    target(self.image, "Fixture.Package!UI"),
                    target(self.image, "Fixture.Package!UI"),
                ],
            )),
            "ready" => Some(DesktopObservation::select(
                true,
                vec![target(self.image, "Fixture.Package!UI")],
            )),
            "waiting" => Some(DesktopObservation::select(true, vec![])),
            _ => Some(DesktopObservation::select(false, vec![])),
        })
    }
    fn activate(&mut self) -> ActionResult<i32> {
        log(&self.trace, "activate");
        if string(self.row, "fault") == "activate" {
            Err(DesktopActionError::Unavailable)
        } else {
            Ok(num(self.row, "pid"))
        }
    }
}
struct Owner<'a> {
    row: &'a IvyJson,
    trace: Trace,
    calls: usize,
}
impl Owner<'_> {
    fn step(&self, method: &str, value: bool) -> ActionResult<bool> {
        log(&self.trace, method);
        if string(self.row, "fault") == method {
            Err(DesktopActionError::Unavailable)
        } else {
            Ok(value)
        }
    }
}
impl DesktopOwner for Owner<'_> {
    fn is_current(&mut self) -> ActionResult<bool> {
        let i = self.calls;
        self.calls += 1;
        self.step("owner", yes(self.row, "owners", i))
    }
    fn try_focus(&mut self) -> ActionResult<bool> {
        self.step("focus", flag(self.row, "focusOk"))
    }
    fn is_focused(&mut self) -> ActionResult<bool> {
        self.step("focused", flag(self.row, "focused"))
    }
}
struct Keyboard<'a> {
    row: &'a IvyJson,
    trace: Trace,
    calls: usize,
}
impl KeyboardInput for Keyboard<'_> {
    fn is_idle(&mut self) -> ActionResult<bool> {
        log(&self.trace, "idle");
        if string(self.row, "fault") == "idle" {
            Err(DesktopActionError::Unavailable)
        } else {
            Ok(flag(self.row, "idle"))
        }
    }
    fn send(&mut self, keys: &[KeyStroke]) -> ActionResult<i32> {
        log(
            &self.trace,
            format!(
                "send:{}",
                keys.iter()
                    .map(|k| format!("{}:{}", k.key, if k.up { "up" } else { "down" }))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        );
        let i = self.calls;
        self.calls += 1;
        if string(self.row, "fault") == if i == 0 { "send" } else { "release" } {
            Err(DesktopActionError::Unavailable)
        } else {
            Ok(num(self.row, if i == 0 { "sent" } else { "released" }))
        }
    }
}
fn current<'a>(row: &'a IvyJson, trace: Trace) -> impl Fn() -> ActionResult<bool> + 'a {
    let calls = Cell::new(0usize);
    move || {
        log(&trace, "current");
        let i = calls.get();
        calls.set(i + 1);
        if i as i32 == num(row, "currentFault") {
            return Err(DesktopActionError::Unavailable);
        }
        Ok(yes(row, "currents", i))
    }
}
fn chord(row: &IvyJson) -> IvyJson {
    let trace = Rc::new(RefCell::new(Vec::new()));
    let current = current(row, trace.clone());
    let mut owner = Owner {
        row,
        trace: trace.clone(),
        calls: 0,
    };
    let mut keyboard = Keyboard {
        row,
        trace: trace.clone(),
        calls: 0,
    };
    let result = capture(|| {
        let modifiers = names(row);
        let key = num(row, "key");
        let (before, prepared) = (flag(row, "focusBefore"), flag(row, "prepared"));
        let result = if flag(row, "wrapped") {
            dispatch_hotkey(
                &mut CurrentDesktopOwner {
                    owner: &mut owner,
                    current: &current,
                },
                &mut keyboard,
                &modifiers,
                key,
                before,
                prepared,
            )
        } else {
            dispatch_hotkey(&mut owner, &mut keyboard, &modifiers, key, before, prepared)
        }?;
        Ok(result.to_json())
    });
    with_trace(result, trace)
}
fn launch(row: &IvyJson, image: &str) -> IvyJson {
    let trace = Rc::new(RefCell::new(Vec::new()));
    let current = current(row, trace.clone());
    let mut app = App {
        row,
        trace: trace.clone(),
        image,
    };
    let result = capture(|| {
        Ok(if flag(row, "wrapped") {
            launch_desktop(
                &mut CurrentApplication {
                    application: &mut app,
                    current: &current,
                },
                flag(row, "start"),
            )
        } else {
            launch_desktop(&mut app, flag(row, "start"))
        }
        .to_json())
    });
    with_trace(result, trace)
}
fn focus(row: &IvyJson) -> IvyJson {
    let trace = Rc::new(RefCell::new(Vec::new()));
    let calls = RefCell::new(std::collections::HashMap::<&str, usize>::new());
    let step = |key: &'static str| {
        log(&trace, key);
        let mut counts = calls.borrow_mut();
        let i = *counts.get(key).unwrap_or(&0);
        counts.insert(key, i + 1);
        if string(row, "fault") == key {
            Err(DesktopActionError::Unavailable)
        } else {
            Ok(yes(row, key, i))
        }
    };
    let result = capture(|| {
        confirm_activation(
            || step("current"),
            || step("foreground"),
            || step("restore"),
            || step("activate"),
            || step("processed"),
        )
        .map(Into::into)
    });
    with_trace(result, trace)
}
fn controls(value: &IvyJson) -> Vec<ControlWindow> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|w| ControlWindow {
            handle: w.get("handle").unwrap().as_js_string().unwrap().clone(),
            minimized: flag(w, "minimized"),
            controls: w
                .get("controls")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|c| Control {
                    id: c
                        .get("id")
                        .unwrap()
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|n| n.as_f64().unwrap() as i32)
                        .collect(),
                    parent_id: vec![42, 0],
                    name: c.get("name").unwrap().as_js_string().unwrap().clone(),
                    enabled: flag(c, "enabled"),
                    offscreen: flag(c, "offscreen"),
                    toggle: None,
                })
                .collect(),
        })
        .collect()
}
struct Source<'a> {
    row: &'a IvyJson,
    trace: Trace,
}
impl Source<'_> {
    fn log(&self, key: &str) -> Result<(), ControlError> {
        log(&self.trace, key);
        let fault = string(self.row, "fault");
        if fault == key || fault == "read" && key.starts_with("read:") {
            Err(ControlError::Panicked)
        } else {
            Ok(())
        }
    }
}
impl ControlSource for Source<'_> {
    fn read(&mut self, labels: &[JsString]) -> Result<Vec<ControlWindow>, ControlError> {
        self.log(&format!(
            "read:{}",
            labels
                .iter()
                .map(|s| s.to_utf8().unwrap())
                .collect::<Vec<_>>()
                .join("|")
        ))?;
        Ok(controls(self.row.get("windows").unwrap()))
    }
    fn close(&mut self) -> Result<(), ControlError> {
        self.log("close")
    }
}
impl InvokeSource for Source<'_> {
    fn invoke(
        &mut self,
        window: &JsString,
        control: &Control,
        current: &dyn Fn() -> bool,
    ) -> Result<DesktopInvokePhase, ControlError> {
        self.log(&format!(
            "invoke:{}:{}",
            window.to_utf8().unwrap(),
            control
                .id
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ))?;
        for _ in 0..num(self.row, "checks") {
            if !current() {
                return Ok(DesktopInvokePhase::NotSubmitted);
            }
        }
        Ok(match string(self.row, "phase").as_str() {
            "submitted" => DesktopInvokePhase::Submitted,
            "outcome_unknown" => DesktopInvokePhase::OutcomeUnknown,
            _ => DesktopInvokePhase::NotSubmitted,
        })
    }
}
fn invoke(row: &IvyJson) -> IvyJson {
    let trace = Rc::new(RefCell::new(Vec::new()));
    let (current_calls, owner_calls) = (Cell::new(0usize), Cell::new(0usize));
    let check = |name: &str, key: &str, fault: &str, calls: &Cell<usize>| {
        log(&trace, name);
        let i = calls.get();
        calls.set(i + 1);
        if i as i32 == num(row, fault) {
            panic!("fixture");
        }
        yes(row, key, i)
    };
    let current = || check("current", "currents", "currentFault", &current_calls);
    let owner = || check("owner", "owners", "ownerFault", &owner_calls);
    let result = capture(|| {
        Ok(invoke_voice(flag(row, "stop"), &current, &owner, || {
            let source = Source {
                row,
                trace: trace.clone(),
            };
            source.log("open")?;
            Ok(source)
        })
        .as_str()
        .into())
    });
    with_trace(result, trace)
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryW(path: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
}
struct Fixture {
    module: *mut c_void,
    reset: unsafe extern "C" fn(i32, i32, u32, u32),
    trace: unsafe extern "C" fn() -> *const c_char,
    send: SendInputFn,
    key_state: KeyStateFn,
}
impl Fixture {
    fn load() -> Self {
        let path: Vec<u16> = std::env::var("IVY_PHONE_ACTIONS_FIXTURE")
            .expect("SDK fixture required")
            .encode_utf16()
            .chain([0])
            .collect();
        let module = unsafe { LoadLibraryW(path.as_ptr()) };
        assert!(!module.is_null());
        macro_rules! symbol {
            ($name:literal,$ty:ty) => {{
                let p = unsafe { GetProcAddress(module, concat!($name, "\0").as_ptr()) };
                assert!(!p.is_null(), $name);
                unsafe { std::mem::transmute::<*mut c_void, $ty>(p) }
            }};
        }
        Self {
            module,
            reset: symbol!("FixtureReset", unsafe extern "C" fn(i32, i32, u32, u32)),
            trace: symbol!("FixtureTrace", unsafe extern "C" fn() -> *const c_char),
            send: symbol!("FixtureSendInput", SendInputFn),
            key_state: symbol!("FixtureKeyState", KeyStateFn),
        }
    }
    fn run(&self, row: &IvyJson) -> IvyJson {
        let unsigned = |key| row.get(key).unwrap().as_f64().unwrap() as u32;
        unsafe {
            (self.reset)(
                num(row, "pressed"),
                num(row, "word"),
                unsigned("sent"),
                unsigned("released"),
            );
        }
        let mut keyboard = WindowsKeyboardInput {
            send: self.send,
            key_state: self.key_state,
        };
        let result = capture(|| {
            if string(row, "kind") == "idle" {
                keyboard.is_idle().map(Into::into)
            } else {
                let keys: Vec<_> = row
                    .get("keys")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|k| KeyStroke {
                        key: num(k, "key") as u16,
                        up: flag(k, "up"),
                    })
                    .collect();
                keyboard.send(&keys).map(|v| n(v))
            }
        });
        IvyJson::object([
            ("result", result),
            (
                "trace",
                unsafe { CStr::from_ptr((self.trace)()) }
                    .to_str()
                    .unwrap()
                    .into(),
            ),
        ])
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        unsafe {
            FreeLibrary(self.module);
        }
    }
}
fn modifiers() -> Vec<Vec<&'static str>> {
    fn extend(prefix: Vec<&'static str>, out: &mut Vec<Vec<&'static str>>) {
        out.push(prefix.clone());
        for key in ["control", "alt", "shift", "windows"] {
            if !prefix.contains(&key) {
                let mut next = prefix.clone();
                next.push(key);
                extend(next, out);
            }
        }
    }
    let mut out = Vec::new();
    extend(vec![], &mut out);
    out.extend([
        vec!["control", "control"],
        vec!["Control"],
        vec!["control", "unknown"],
        vec!["control", "alt", "shift", "windows", "extra"],
    ]);
    out
}
fn row(fields: impl IntoIterator<Item = (&'static str, IvyJson)>) -> IvyJson {
    IvyJson::object(fields)
}
fn keys_json(keys: &[KeyStroke]) -> IvyJson {
    array(
        keys.iter()
            .map(|k| row([("key", n(i32::from(k.key))), ("up", k.up.into())])),
    )
}
fn window(handle: &str, minimized: bool, buttons: Vec<(&str, bool, bool)>) -> IvyJson {
    row([
        ("handle", handle.into()),
        ("minimized", minimized.into()),
        (
            "controls",
            array(
                buttons
                    .into_iter()
                    .enumerate()
                    .map(|(i, (name, enabled, offscreen))| {
                        row([
                            ("id", array([n(42), n(i as i32 + 1)])),
                            ("parentId", array([n(42), n(0)])),
                            ("name", name.into()),
                            ("enabled", enabled.into()),
                            ("offscreen", offscreen.into()),
                            ("toggle", IvyJson::Null),
                        ])
                    }),
            ),
        ),
    ])
}
fn corpus(image: &str) -> IvyJson {
    let mut applications: Vec<IvyJson> = [
        "",
        "A!B",
        "a!b!c",
        "a!",
        "!b",
        "a!b\n",
        " a!b",
        "a\0!b",
        "é!UI",
        "a!\u{200b}",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    for length in [1, 124, 125, 126, 127, 128, 129] {
        applications.push(format!("{}!UI", "A".repeat(length)).into());
    }
    for code in 0..=127 {
        let c = char::from_u32(code).unwrap();
        applications.push(format!("A{c}!UI").into());
        applications.push(format!("A!U{c}I").into());
    }
    let mut plans = Vec::new();
    for modifiers in modifiers() {
        for key in [-1, 0, 31, 32, 33, 64, 65, 90, 91, 111, 112, 135, 136] {
            plans.push(row([("modifiers", strings(&modifiers)), ("key", n(key))]));
        }
    }
    let mut chords = Vec::new();
    for modifiers in [
        vec!["control"],
        vec!["control", "shift"],
        vec!["windows", "alt", "control", "shift"],
    ] {
        let size = 2 * (modifiers.len() as i32 + 1);
        for wrapped in [false, true] {
            for focus in [false, true] {
                for prepared in [false, true] {
                    for profile in 0..6 {
                        for sent in [-1, 0, 1, size - 1, size, size + 1] {
                            for released in [0, size / 2] {
                                chords.push(row([
                                    ("modifiers", strings(&modifiers)),
                                    ("key", n(32)),
                                    (
                                        "owners",
                                        bools(if profile == 1 {
                                            &[false]
                                        } else if profile == 2 {
                                            &[true, false]
                                        } else {
                                            &[true]
                                        }),
                                    ),
                                    ("currents", bools(&[true])),
                                    ("currentFault", n(-1)),
                                    ("wrapped", wrapped.into()),
                                    ("focusBefore", focus.into()),
                                    ("prepared", prepared.into()),
                                    ("focusOk", (profile != 3).into()),
                                    ("focused", (profile != 4).into()),
                                    ("idle", (profile != 5).into()),
                                    ("sent", n(sent)),
                                    ("released", n(released)),
                                    ("fault", "".into()),
                                ]));
                            }
                        }
                    }
                }
            }
        }
    }
    for fault in ["owner", "focus", "focused", "idle", "send", "release"] {
        for wrapped in [false, true] {
            for current_fault in [-1, 0, 1, 2, 3, 4, 5, 6] {
                chords.push(row([
                    ("modifiers", strings(&["control", "shift"])),
                    ("key", n(32)),
                    ("owners", bools(&[true])),
                    ("currents", bools(&[true])),
                    ("currentFault", n(current_fault)),
                    ("wrapped", wrapped.into()),
                    ("focusBefore", true.into()),
                    ("prepared", false.into()),
                    ("focusOk", true.into()),
                    ("focused", true.into()),
                    ("idle", true.into()),
                    ("sent", n(1)),
                    ("released", n(3)),
                    ("fault", fault.into()),
                ]));
            }
        }
    }
    let mut launches = Vec::new();
    for state in [
        "null",
        "unavailable",
        "ambiguous",
        "ready",
        "waiting",
        "absent",
    ] {
        for start in [false, true] {
            for wrapped in [false, true] {
                for currents in [
                    vec![true],
                    vec![false],
                    vec![true, false],
                    vec![true, true, false],
                ] {
                    for pid in [-1, 0, 123] {
                        for fault in ["", "observe", "activate"] {
                            for current_fault in [-1, 0, 1, 2] {
                                launches.push(row([
                                    ("state", state.into()),
                                    ("start", start.into()),
                                    ("wrapped", wrapped.into()),
                                    ("currents", bools(&currents)),
                                    ("pid", n(pid)),
                                    ("fault", fault.into()),
                                    ("currentFault", n(current_fault)),
                                ]));
                            }
                        }
                    }
                }
            }
        }
    }
    let mut focus = Vec::new();
    for bits in 0..32 {
        for fault in [
            "",
            "current",
            "foreground",
            "restore",
            "activate",
            "processed",
        ] {
            for current_stop in [-1, 0, 1, 2] {
                let current: Vec<_> = (0..3).map(|i| bits & 1 != 0 && i != current_stop).collect();
                focus.push(row([
                    ("current", bools(&current)),
                    ("foreground", bools(&[bits & 2 != 0])),
                    ("restore", bools(&[bits & 4 != 0])),
                    ("activate", bools(&[bits & 8 != 0])),
                    ("processed", bools(&[bits & 16 != 0])),
                    ("fault", fault.into()),
                ]));
            }
        }
    }
    let mut invokes = Vec::new();
    for stop in [false, true] {
        let labels = if stop { STOP_LABELS } else { START_LABELS };
        let mut windows = vec![
            array([]),
            array([window("123", false, vec![])]),
            array([window("123", true, vec![(labels[0], true, false)])]),
            array([window("123", false, vec![(labels[0], false, false)])]),
            array([window("123", false, vec![(labels[0], true, true)])]),
            array([window(
                "123",
                false,
                vec![
                    (labels[0], true, false),
                    (labels[0], true, false),
                    (labels[1], true, false),
                ],
            )]),
            array([
                window("123", false, vec![(labels[0], true, false)]),
                window("124", false, vec![(labels[0], true, false)]),
            ]),
        ];
        for label in labels {
            windows.push(array([window("123", false, vec![(*label, true, false)])]));
        }
        windows.push(array([window(
            "123",
            false,
            vec![
                (labels[2], true, false),
                (labels[0], true, false),
                (labels[1], true, false),
            ],
        )]));
        for windows in windows {
            for fault in ["", "open", "read", "close", "invoke:123:42,1"] {
                for phase in ["submitted", "not_submitted", "outcome_unknown"] {
                    for profile in 0..7 {
                        let (currents, owners) = match profile {
                            1 => (vec![false], vec![true]),
                            2 => (vec![true, false], vec![true]),
                            3 => (vec![true], vec![false]),
                            4 => (vec![true], vec![true, false]),
                            _ => (vec![true], vec![true]),
                        };
                        invokes.push(row([
                            ("windows", windows.clone()),
                            ("stop", stop.into()),
                            ("fault", fault.into()),
                            ("phase", phase.into()),
                            ("checks", n(1)),
                            ("currents", bools(&currents)),
                            ("owners", bools(&owners)),
                            ("currentFault", n(if profile == 5 { 0 } else { -1 })),
                            ("ownerFault", n(if profile == 6 { 1 } else { -1 })),
                        ]));
                    }
                }
            }
        }
    }
    let mut native = Vec::new();
    for pressed in [-1, 0, 1, 16, 91, 254, 255] {
        for word in [0, 1, 32767, 32768, 65535] {
            native.push(row([
                ("kind", "idle".into()),
                ("pressed", n(pressed)),
                ("word", n(word)),
                ("sent", n(0)),
                ("released", n(0)),
                ("keys", array([])),
            ]));
        }
    }
    for modifiers in modifiers().into_iter().filter(|m| m.len() <= 4) {
        if let Ok(plan) = plan_hotkey(
            &modifiers
                .iter()
                .map(|s| JsString::from(*s))
                .collect::<Vec<_>>(),
            32,
        ) {
            for sent in [0, 1, plan.len() as i64, 4294967295i64] {
                native.push(row([
                    ("kind", "send".into()),
                    ("pressed", n(-1)),
                    ("word", n(0)),
                    ("sent", IvyJson::Number(sent as f64)),
                    ("released", n(0)),
                    (
                        "keys",
                        array(
                            plan.iter()
                                .map(|k| row([("key", n(i32::from(k.key))), ("up", k.up.into())])),
                        ),
                    ),
                ]));
            }
        }
    }
    row([
        ("image", image.into()),
        ("applications", IvyJson::Array(applications)),
        ("plans", IvyJson::Array(plans)),
        ("chords", IvyJson::Array(chords)),
        ("launches", IvyJson::Array(launches)),
        ("focus", IvyJson::Array(focus)),
        ("invokes", IvyJson::Array(invokes)),
        ("native", IvyJson::Array(native)),
    ])
}
fn reentrant() -> IvyJson {
    struct App {
        inner: bool,
        trace: Trace,
    }
    impl DesktopApplication for App {
        fn observe(&mut self) -> ActionResult<Option<DesktopObservation>> {
            log(
                &self.trace,
                if self.inner {
                    "inner.observe"
                } else {
                    "outer.observe"
                },
            );
            if !self.inner {
                let result = launch_desktop(
                    &mut App {
                        inner: true,
                        trace: self.trace.clone(),
                    },
                    false,
                );
                log(&self.trace, format!("nested_launch:{}", result.phase));
            }
            Ok(Some(DesktopObservation::select(false, vec![])))
        }
        fn activate(&mut self) -> ActionResult<i32> {
            log(&self.trace, "outer.activate");
            Ok(123)
        }
    }
    struct Owner;
    impl DesktopOwner for Owner {
        fn is_current(&mut self) -> ActionResult<bool> {
            Ok(true)
        }
        fn try_focus(&mut self) -> ActionResult<bool> {
            Ok(true)
        }
        fn is_focused(&mut self) -> ActionResult<bool> {
            Ok(true)
        }
    }
    struct Keyboard {
        inner: bool,
        trace: Trace,
    }
    impl KeyboardInput for Keyboard {
        fn is_idle(&mut self) -> ActionResult<bool> {
            Ok(true)
        }
        fn send(&mut self, keys: &[KeyStroke]) -> ActionResult<i32> {
            log(
                &self.trace,
                format!(
                    "{}.send:{}",
                    if self.inner { "inner" } else { "outer" },
                    keys.len()
                ),
            );
            if !self.inner {
                let result = dispatch_hotkey(
                    &mut Owner,
                    &mut Keyboard {
                        inner: true,
                        trace: self.trace.clone(),
                    },
                    &["control".into()],
                    32,
                    false,
                    false,
                )?;
                log(&self.trace, format!("nested_hotkey:{}", result.phase));
            }
            Ok(keys.len() as i32)
        }
    }
    let trace = Rc::new(RefCell::new(Vec::new()));
    let launch = launch_desktop(
        &mut App {
            inner: false,
            trace: trace.clone(),
        },
        true,
    );
    let hotkey = dispatch_hotkey(
        &mut Owner,
        &mut Keyboard {
            inner: false,
            trace: trace.clone(),
        },
        &["control".into()],
        32,
        false,
        false,
    )
    .unwrap();
    let events = trace.borrow().clone();
    row([
        ("launch", launch.phase.into()),
        ("hotkey", hotkey.phase.into()),
        ("trace", IvyJson::Array(events)),
    ])
}
#[test]
fn original_launch_input_focus_and_invoke_policy_agree() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|info| {
        if info.payload().downcast_ref::<&str>() != Some(&"fixture") {
            eprintln!("{info}");
        }
    }));
    let result = std::panic::catch_unwind(compare_original_actions);
    // Changing a hook during unwinding itself panics. Restore after the caught
    // test outcome, retaining the original failure instead of aborting twice.
    std::panic::set_hook(previous);
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}
fn compare_original_actions() {
    assert_eq!(std::mem::size_of::<Input>(), 40);
    assert_eq!(std::mem::offset_of!(Input, key), 8);
    assert_eq!(std::mem::offset_of!(Input, extra), 24);
    let fixture = Fixture::load();
    let image = r"C:\fixture\fixture-desktop.exe";
    let input = corpus(image);
    let original = parse(
        include_str!("../fixtures/phone-desktop-actions-cases.json"),
        LIMIT,
    )
    .unwrap();
    let apply = |key: &str, f: &dyn Fn(&IvyJson) -> IvyJson| {
        array(input.get(key).unwrap().as_array().unwrap().iter().map(f))
    };
    let actual = row([
        ("reentrant", reentrant()),
        (
            "applications",
            apply("applications", &|v| {
                capture(|| {
                    let a = v.as_str().unwrap();
                    validate_application(&a)?;
                    assert!(
                        crate::media::AudioDesktopIdentity::new(123, 1, Path::new(image), &a)
                            .is_ok()
                    );
                    Ok(true.into())
                })
            }),
        ),
        (
            "plans",
            apply("plans", &|v| {
                capture(|| Ok(keys_json(&plan_hotkey(&names(v), num(v, "key"))?)))
            }),
        ),
        ("chords", apply("chords", &chord)),
        ("launches", apply("launches", &|v| launch(v, image))),
        ("focus", apply("focus", &focus)),
        ("invokes", apply("invokes", &invoke)),
        ("native", apply("native", &|v| fixture.run(v))),
    ]);

    assert_eq!(actual.get("reentrant"), original.get("reentrant"));
    let mut total = 2;
    for key in [
        "applications",
        "plans",
        "chords",
        "launches",
        "focus",
        "invokes",
        "native",
    ] {
        let expected = original.get(key).unwrap().as_array().unwrap();
        let rows = actual.get(key).unwrap().as_array().unwrap();
        assert_eq!(rows.len(), expected.len());
        for (index, (a, b)) in rows.iter().zip(expected).enumerate() {
            assert_eq!(
                a,
                b,
                "{key} case {index}: {}",
                stringify(&input.get(key).unwrap().as_array().unwrap()[index], LIMIT).unwrap()
            );
        }
        total += rows.len();
    }
    println!(
        "{total} original Desktop action comparisons; SDK keyboard ABI only, no real input or activation"
    );
}
