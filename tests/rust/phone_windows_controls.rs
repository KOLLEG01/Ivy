use super::*;
use ivy_contracts::{
    IvyJson,
    json::{parse as decode, stringify as encode},
};
use std::{
    cell::{Cell, RefCell},
    ffi::{CStr, c_char, c_void},
    rc::Rc,
};

fn stringify(value: &IvyJson) -> String {
    encode(value, 16 * 1024 * 1024).unwrap()
}
fn parse(text: &str) -> IvyJson {
    decode(text, 16 * 1024 * 1024).unwrap()
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryW(path: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
}
struct Fixture {
    module: *mut c_void,
    reset: unsafe extern "C" fn(i32, i32) -> i32,
    automation: unsafe extern "C" fn() -> com::Raw,
    failure: unsafe extern "C" fn(i32, i32, i32),
    phase: unsafe extern "C" fn(i32),
    trace: unsafe extern "C" fn() -> *const c_char,
    references: unsafe extern "C" fn() -> i32,
    ordinal: unsafe extern "C" fn(isize) -> i32,
    destroy: unsafe extern "C" fn(),
}
impl Fixture {
    fn load() -> Self {
        let path = std::env::var("IVY_PHONE_UIA_FIXTURE").expect("SDK-built fixture path required");
        let path: Vec<u16> = path.encode_utf16().chain([0]).collect();
        let module = unsafe { LoadLibraryW(path.as_ptr()) };
        assert!(!module.is_null(), "SDK fixture load failed");
        macro_rules! symbol {
            ($name:literal,$ty:ty) => {{
                let p = unsafe { GetProcAddress(module, concat!($name, "\0").as_ptr()) };
                assert!(!p.is_null(), $name);
                unsafe { std::mem::transmute::<*mut c_void, $ty>(p) }
            }};
        }
        Self {
            module,
            reset: symbol!("FixtureReset", unsafe extern "C" fn(i32, i32) -> i32),
            automation: symbol!("FixtureAutomation", unsafe extern "C" fn() -> com::Raw),
            failure: symbol!("FixtureFailure", unsafe extern "C" fn(i32, i32, i32)),
            phase: symbol!("FixturePhase", unsafe extern "C" fn(i32)),
            trace: symbol!("FixtureTrace", unsafe extern "C" fn() -> *const c_char),
            references: symbol!("FixtureReferences", unsafe extern "C" fn() -> i32),
            ordinal: symbol!("FixtureWindowOrdinal", unsafe extern "C" fn(isize) -> i32),
            destroy: symbol!("FixtureDestroy", unsafe extern "C" fn()),
        }
    }
    fn windows(&self, mut windows: Vec<ControlWindow>) -> IvyJson {
        for window in &mut windows {
            let handle = String::from_utf16(&window.handle.0)
                .unwrap()
                .parse()
                .unwrap();
            window.handle = unsafe { (self.ordinal)(handle) }.to_string().into();
        }
        super::super::ControlsObservation {
            observed: true,
            windows,
        }
        .to_json()
        .remove("windows")
        .unwrap()
    }
    fn run(&self, item: &IvyJson) -> IvyJson {
        let n = |key| item.get(key).unwrap().as_f64().unwrap() as i32;
        assert_eq!(unsafe { (self.reset)(n("mode"), n("windows")) }, 1);
        let stages = Rc::new(RefCell::new(Vec::new()));
        let trace = stages.clone();
        let stage_throw = item.get("stageThrow").unwrap().as_bool().unwrap();
        let mut source = WindowsControlSource::new(
            std::process::id() as i32,
            item.get("action").unwrap().as_str().unwrap() != "observe",
            Some(Box::new(move |s| {
                trace.borrow_mut().push(IvyJson::object([
                    ("stage", s.stage.into()),
                    ("count", IvyJson::Number(s.count.into())),
                ]));
                if stage_throw {
                    panic!("fixture stage");
                }
            })),
        )
        .unwrap();
        // Exercise real COM apartment/activation boundaries with an SDK provider
        // that reads no desktop controls.
        let automation = source.keep(Reference::take(unsafe { (self.automation)() }).unwrap());
        source.automation = Some(automation);
        unsafe {
            (self.failure)(n("failure"), n("hr"), n("hide"));
        }
        let labels = vec![
            JsString::from("Mute microphone"),
            JsString::from("Stop voice chat"),
        ];
        let (mut before, mut after) = (0, 0);
        let result = capture(|| {
            unsafe {
                let table = automation.table::<Automation>();
                check((table.auto_focus)(automation.raw(), 0))?;
                check((table.connection_timeout)(automation.raw(), 250))?;
                check((table.transaction_timeout)(automation.raw(), 500))?;
            }
            source.prepare(&labels)?;
            source.prepare(&labels.clone())?;
            let read = source.read(&labels)?;
            let action = item.get("action").unwrap().as_str().unwrap();
            if action == "prepare_changed" {
                source.prepare(&[labels[1].clone(), labels[0].clone()])?;
                return Ok(IvyJson::Null);
            }
            if action == "read_twice" {
                source.read(&labels)?;
                return Ok(IvyJson::Null);
            }
            if action == "read" || action == "observe" {
                return Ok(self.windows(read));
            }
            unsafe {
                (self.phase)(1);
                before = (self.references)();
            }
            let operation = capture(|| {
                let original = read
                    .first()
                    .ok_or_else(|| invalid("Fixture read has no original window."))?;
                if action == "refresh" {
                    return source
                        .refresh(VoiceControlIdentity {
                            window: original.handle.clone(),
                            parent_id: vec![42, 0],
                            microphone_id: vec![42, 1],
                            stop_id: vec![42, 2],
                        })
                        .map(|w| self.windows(w));
                }
                let current = Cell::new(0);
                let stop = n("currentStop");
                source
                    .invoke(
                        &original.handle,
                        original
                            .controls
                            .first()
                            .ok_or_else(|| invalid("Fixture read has no original control."))?,
                        &|| {
                            let i = current.get();
                            current.set(i + 1);
                            if stop <= -2 && i == -(stop + 2) {
                                panic!("fixture current");
                            }
                            i != stop
                        },
                    )
                    .map(|r| r.as_str().into())
            });
            after = unsafe { (self.references)() };
            Ok(IvyJson::object([("operation", operation)]))
        });
        source.close().unwrap();
        source.close().unwrap();
        unsafe {
            (self.destroy)();
        }
        IvyJson::object([
            ("result", result),
            ("stages", IvyJson::Array(stages.borrow().clone())),
            (
                "trace",
                unsafe { CStr::from_ptr((self.trace)()) }
                    .to_str()
                    .unwrap()
                    .into(),
            ),
            ("released", (unsafe { (self.references)() } == 0).into()),
            ("transientReleased", (before == after).into()),
        ])
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        unsafe {
            (self.destroy)();
            FreeLibrary(self.module);
        }
    }
}
fn capture(f: impl FnOnce() -> Result<IvyJson, ControlError>) -> IvyJson {
    match f() {
        Ok(value) => IvyJson::object([("ok", true.into()), ("value", value)]),
        Err(error) => {
            let property = if let ControlError::Provider(error) = error {
                error
                    .downcast_ref::<AccessibilityError>()
                    .and_then(|e| e.property_id)
            } else {
                None
            };
            IvyJson::object([
                ("ok", false.into()),
                (
                    "propertyId",
                    property
                        .map(|v| IvyJson::Number(v.into()))
                        .unwrap_or(IvyJson::Null),
                ),
            ])
        }
    }
}
fn case(
    mode: i32,
    action: &str,
    windows: i32,
    failure: i32,
    hr: i32,
    hide: i32,
    current: i32,
    stage_throw: bool,
) -> IvyJson {
    IvyJson::object([
        ("mode", IvyJson::Number(mode.into())),
        ("action", action.into()),
        ("windows", IvyJson::Number(windows.into())),
        ("failure", IvyJson::Number(failure.into())),
        ("hr", IvyJson::Number(hr.into())),
        ("hide", IvyJson::Number(hide.into())),
        ("currentStop", IvyJson::Number(current.into())),
        ("stageThrow", stage_throw.into()),
    ])
}
#[test]
fn sdk_com_cache_refresh_invoke_and_lifetime_match_original() {
    use std::mem::{offset_of, size_of};
    assert_eq!(size_of::<Variant>(), 24);
    assert_eq!(offset_of!(Variant, data), 8);
    for (actual, slot) in [
        (offset_of!(Automation, element), 6),
        (offset_of!(Automation, tree), 13),
        (offset_of!(Automation, raw_walker), 16),
        (offset_of!(Automation, cache), 20),
        (offset_of!(Automation, property), 23),
        (offset_of!(Automation, auto_focus), 59),
        (offset_of!(Automation, connection_timeout), 61),
        (offset_of!(Automation, transaction_timeout), 63),
        (offset_of!(Element, find_all), 8),
        (offset_of!(Element, update), 9),
        (offset_of!(Element, current_ex), 11),
        (offset_of!(Element, cached_ex), 13),
        (offset_of!(Element, pattern), 14),
        (offset_of!(Walker, normalize), 8),
        (offset_of!(Walker, parent_cached), 9),
    ] {
        assert_eq!(actual, slot * size_of::<usize>());
    }
    let fixture = Fixture::load();
    let vectors = parse(include_str!(
        "../fixtures/phone-desktop-windows-controls-cases.json"
    ));
    let rows = vectors.get("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 104);
    let released = |actual: &IvyJson, context: &str| {
        assert_eq!(
            actual.get("released").unwrap().as_bool(),
            Some(true),
            "retained refs: {context}"
        );
        assert_eq!(
            actual.get("transientReleased").unwrap().as_bool(),
            Some(true),
            "transient refs: {context}"
        );
    };
    for (index, row) in rows.iter().enumerate() {
        let input = row.get("input").unwrap();
        let actual = fixture.run(input);
        assert_eq!(
            actual.get("result"),
            row.get("result"),
            "public case {index}: {}",
            stringify(input)
        );
        released(&actual, &format!("public case {index}"));
    }
    // Retaining invocation references does not change a plain observation.
    for (mode, windows) in (0..29).map(|mode| (mode, 1)) {
        let read = case(mode, "read", windows, -1, 0, -1, -1, false);
        let expected = rows
            .iter()
            .find(|row| row.get("input") == Some(&read))
            .unwrap();
        let actual = fixture.run(&case(mode, "observe", windows, -1, 0, -1, -1, false));
        assert_eq!(
            actual.get("result"),
            expected.get("result"),
            "observation mode {mode}, windows {windows}"
        );
        released(&actual, "observation");
    }
    // Observation admits separate windows containing the same control IDs;
    // retained invocation identities must remain unique across their windows.
    let window = rows[0]
        .get("result")
        .unwrap()
        .get("value")
        .unwrap()
        .as_array()
        .unwrap()[0]
        .clone();
    for windows in [0, 2, 32, 33] {
        let expected = if windows > 32 {
            IvyJson::object([("ok", false.into()), ("propertyId", IvyJson::Null)])
        } else {
            IvyJson::object([
                ("ok", true.into()),
                (
                    "value",
                    IvyJson::Array(
                        (0..windows)
                            .map(|index| {
                                let mut value = window.clone();
                                value.insert("handle", index.to_string().into());
                                value
                            })
                            .collect(),
                    ),
                ),
            ])
        };
        let actual = fixture.run(&case(0, "observe", windows, -1, 0, -1, -1, false));
        assert_eq!(
            actual.get("result"),
            Some(&expected),
            "observed windows {windows}"
        );
        released(&actual, "multiple observed windows");
    }
    // Exercise cleanup at every native boundary reached by the current provider.
    // Call ordinals are implementation details, so they select injected failures
    // without indexing a historical provider's public result table.
    let mut faults = 0;
    for action in ["observe", "read", "refresh", "invoke"] {
        let base = fixture.run(&case(0, action, 1, -1, 0, -1, -1, false));
        released(&base, "fault baseline");
        let calls = base
            .get("trace")
            .unwrap()
            .as_str()
            .unwrap()
            .split(';')
            .filter(|s| !s.is_empty())
            .count() as i32;
        assert!(calls > 0);
        for call in 0..calls {
            for hr in [0x80070005u32 as i32, 1] {
                let actual = fixture.run(&case(0, action, 1, call, hr, -1, -1, false));
                released(&actual, &format!("{action}, call {call}, HRESULT {hr}"));
                faults += 1;
            }
        }
        for hide in [3, 16, calls - 1] {
            released(
                &fixture.run(&case(0, action, 1, -1, 0, hide, -1, false)),
                "hidden owned window",
            );
            faults += 1;
        }
    }
    // A failing out call may still return a reference. Retire it immediately and
    // preserve the native failure code; null results still require success.
    assert_eq!(unsafe { (fixture.reset)(0, 0) }, 1);
    let mut source = WindowsControlSource::new(std::process::id() as i32, false, None).unwrap();
    for status in [0x80070005u32 as i32, 1] {
        let result = source.out(|out| unsafe {
            *out = (fixture.automation)();
            status
        });
        let error = match result {
            Err(ControlError::Provider(error)) => error,
            _ => panic!("native error required"),
        };
        assert_eq!(
            error.downcast_ref::<AccessibilityError>().unwrap().hresult,
            if status < 0 {
                status
            } else {
                0x80004005u32 as i32
            }
        );
        assert_eq!(unsafe { (fixture.references)() }, 0);
    }
    assert!(source.out(|_| 0).is_err());
    source.close().unwrap();
    // An existing STA must reject the source without stealing apartment ownership.
    std::thread::spawn(|| unsafe {
        assert_eq!(com::CoInitializeEx(std::ptr::null_mut(), 2), 0);
        assert!(WindowsControlSource::new(0, false, None).is_err());
        assert_eq!(com::CoInitializeEx(std::ptr::null_mut(), 2), 1);
        com::CoUninitialize();
        com::CoUninitialize();
    })
    .join()
    .unwrap();
    println!(
        "{} SDK-backed source cases; public outcomes, scoped releases and HRESULT boundaries",
        rows.len() + 33 + faults
    );
}
