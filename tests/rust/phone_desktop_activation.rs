use super::*;
use crate::desktop::{DesktopApplication, DesktopObservation, launch_desktop};
use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
};
use std::{
    ffi::{CStr, c_char},
    io::Write,
    path::Path,
    process::{Command, Stdio},
};
const LIMIT: usize = 32 * 1024 * 1024;
#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryW(path: *const u16) -> Raw;
    fn GetProcAddress(module: Raw, name: *const u8) -> Raw;
    fn FreeLibrary(module: Raw) -> i32;
}
struct Fixture {
    module: Raw,
    reset: unsafe extern "C" fn(i32, i32, i32, u32, i32, i32, i32, u32),
    trace: unsafe extern "C" fn() -> *const c_char,
    references: unsafe extern "C" fn() -> i32,
    native: Native,
}
impl Fixture {
    fn load() -> Self {
        let path: Vec<_> = std::env::var("IVY_PHONE_ACTIVATION_FIXTURE")
            .expect("SDK fixture required")
            .encode_utf16()
            .chain([0])
            .collect();
        let module = unsafe { LoadLibraryW(path.as_ptr()) };
        assert!(!module.is_null());
        macro_rules! symbol {
            ($name:literal,$ty:ty) => {{
                let value = unsafe { GetProcAddress(module, concat!($name, "\0").as_ptr()) };
                assert!(!value.is_null(), $name);
                unsafe { std::mem::transmute::<Raw, $ty>(value) }
            }};
        }
        Self {
            module,
            reset: symbol!(
                "FixtureReset",
                unsafe extern "C" fn(i32, i32, i32, u32, i32, i32, i32, u32)
            ),
            trace: symbol!("FixtureTrace", unsafe extern "C" fn() -> *const c_char),
            references: symbol!("FixtureReferences", unsafe extern "C" fn() -> i32),
            native: Native {
                station: symbol!("FixtureStation", StationFn),
                information: symbol!("FixtureInformation", InformationFn),
                initialize: symbol!("FixtureInitialize", InitializeFn),
                uninitialize: symbol!("FixtureUninitialize", UninitializeFn),
                create: symbol!("FixtureCreate", CreateFn),
            },
        }
    }
    fn row(&self, row: &IvyJson) -> IvyJson {
        let signed = |name| row.get(name).unwrap().as_f64().unwrap() as i32;
        let unsigned = |name| row.get(name).unwrap().as_f64().unwrap() as u32;
        unsafe {
            (self.reset)(
                signed("init"),
                signed("create"),
                signed("act"),
                unsigned("pid"),
                signed("output"),
                signed("station"),
                signed("information"),
                unsigned("flags"),
            )
        };
        let result = (|| {
            let application = WindowsApplicationActivation::with_native(
                &row.get("application").unwrap().as_str().unwrap(),
                self.native,
            )?;
            match row.get("operation").unwrap().as_str().unwrap().as_str() {
                "construct" => Ok(true.into()),
                "direct" => application
                    .activate()
                    .map(|pid| IvyJson::Number(pid.into())),
                _ => {
                    struct App(WindowsApplicationActivation);
                    impl DesktopApplication for App {
                        fn observe(&mut self) -> ActionResult<Option<DesktopObservation>> {
                            Ok(Some(DesktopObservation::select(false, vec![])))
                        }
                        fn activate(&mut self) -> ActionResult<i32> {
                            self.0.activate()
                        }
                    }
                    Ok(launch_desktop(&mut App(application), true).to_json())
                }
            }
        })();
        let result = match result {
            Ok(value) => IvyJson::object([("ok", true.into()), ("value", value)]),
            Err(DesktopActionError::Argument(error)) => IvyJson::object([
                ("ok", false.into()),
                ("kind", "argument".into()),
                ("error", error.into()),
            ]),
            Err(DesktopActionError::Platform(error)) => IvyJson::object([
                ("ok", false.into()),
                ("kind", "platform".into()),
                ("error", error.into()),
            ]),
            Err(DesktopActionError::Unavailable) => {
                IvyJson::object([("ok", false.into()), ("kind", "unavailable".into())])
            }
        };
        IvyJson::object([
            ("result", result),
            (
                "trace",
                unsafe { CStr::from_ptr((self.trace)()) }
                    .to_str()
                    .unwrap()
                    .into(),
            ),
            (
                "references",
                IvyJson::Number(unsafe { (self.references)() }.into()),
            ),
        ])
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        unsafe { FreeLibrary(self.module) };
    }
}
fn row(
    application: &str,
    operation: &str,
    init: i32,
    create: i32,
    act: i32,
    pid: u32,
    output: i32,
    station: i32,
    information: i32,
    flags: u32,
) -> IvyJson {
    IvyJson::object([
        ("application", application.into()),
        ("operation", operation.into()),
        ("init", IvyJson::Number(init.into())),
        ("create", IvyJson::Number(create.into())),
        ("act", IvyJson::Number(act.into())),
        ("pid", IvyJson::Number(pid.into())),
        ("output", IvyJson::Number(output.into())),
        ("station", IvyJson::Number(station.into())),
        ("information", IvyJson::Number(information.into())),
        ("flags", IvyJson::Number(flags.into())),
    ])
}
fn corpus() -> Vec<IvyJson> {
    let mut rows = Vec::new();
    let longest = format!("{}!Z", "a".repeat(126));
    for application in [
        "Fixture.Package!UI",
        "a!b",
        longest.as_str(),
        "",
        "a",
        "a!",
        "!b",
        "a!b!c",
        "a! b",
        "a!b\0",
        "á!b",
        &format!("{}!Z", "a".repeat(127)),
    ] {
        for station in [0, 1] {
            for information in [0, 1, -1] {
                for flags in [0, 1, 2, 3, u32::MAX] {
                    rows.push(row(
                        application,
                        "construct",
                        0,
                        0,
                        0,
                        123,
                        1,
                        station,
                        information,
                        flags,
                    ));
                }
            }
        }
    }
    for application in ["Fixture.Package!UI", longest.as_str()] {
        for operation in ["direct", "dispatch"] {
            for init in [
                0,
                1,
                2,
                CHANGED_MODE,
                0x80004005u32 as i32,
                0x80070005u32 as i32,
            ] {
                for create in [
                    0,
                    1,
                    0x80004005u32 as i32,
                    0x80004002u32 as i32,
                    0x80070005u32 as i32,
                ] {
                    for output in [0, 1] {
                        for act in [0, 1, 2, 0x80004005u32 as i32, 0x80070005u32 as i32] {
                            for pid in [0, 1, 123, i32::MAX as u32, i32::MAX as u32 + 1, u32::MAX] {
                                rows.push(row(
                                    application,
                                    operation,
                                    init,
                                    create,
                                    act,
                                    pid,
                                    output,
                                    1,
                                    1,
                                    1,
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    rows
}
#[test]
fn original_activation_hresult_pid_and_cleanup_match_sdk_boundaries() {
    assert_eq!(size_of::<UserObjectFlags>(), 12);
    assert_eq!(std::mem::offset_of!(UserObjectFlags, flags), 8);
    assert_eq!(
        std::mem::offset_of!(ActivationManager, activate),
        3 * size_of::<usize>()
    );
    let fixture = Fixture::load();
    let input = IvyJson::Array(corpus());
    let text = stringify(&input, LIMIT).unwrap();
    let prefix = std::env::var("IVY_PHONE_ACTIVATION_TRACE").expect("Evidence prefix required");
    std::fs::write(format!("{prefix}.input.json"), &text).unwrap();
    let mut child = Command::new(std::env::var("IVY_POWERSHELL").unwrap_or("pwsh".into()))
        .args(["-NoProfile", "-File"])
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../tests/fixtures/phone-desktop-activation/oracle.ps1"),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    std::fs::write(format!("{prefix}.original.json"), &output.stdout).unwrap();
    std::fs::write(format!("{prefix}.original.err.log"), &output.stderr).unwrap();
    assert!(
        output.status.success(),
        "original: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let original = parse(std::str::from_utf8(&output.stdout).unwrap(), LIMIT).unwrap();
    // Read-only current-window-station evidence; no activation or focus occurs.
    assert_eq!(
        Native::default().interactive(),
        original
            .get("actualUserInteractive")
            .unwrap()
            .as_bool()
            .unwrap()
    );
    let actual: Vec<_> = input
        .as_array()
        .unwrap()
        .iter()
        .map(|row| fixture.row(row))
        .collect();
    std::fs::write(
        format!("{prefix}.rust.json"),
        stringify(&IvyJson::Array(actual.clone()), LIMIT).unwrap(),
    )
    .unwrap();
    let expected = original.get("rows").unwrap().as_array().unwrap();
    assert_eq!(actual.len(), expected.len());
    for (index, (a, b)) in actual.iter().zip(expected).enumerate() {
        assert_eq!(
            a,
            b,
            "case {index}: {}",
            stringify(&input.as_array().unwrap()[index], LIMIT).unwrap()
        );
    }
    assert!(
        actual
            .iter()
            .all(|r| r.get("references").unwrap().as_f64() == Some(0.0)),
        "Owned activation manager references remain"
    );
    println!(
        "{} original activation/constructor comparisons; SDK boundaries only, no app activation",
        actual.len()
    );
}
