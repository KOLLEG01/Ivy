use super::*;
use serde_json::{Value, json};
use std::{cell::RefCell, collections::HashMap};
#[derive(Default)]
struct Script {
    fault: &'static str,
    trace: Vec<String>,
    bad: Vec<String>,
    live: usize,
    apartments: usize,
    events: usize,
    priorities: usize,
    strings: HashMap<usize, Box<[u16]>>,
    id: String,
    flow: i32,
    state: u32,
    supported: bool,
}
thread_local! {static SCRIPT:RefCell<Option<Rc<RefCell<Script>>>>=const{RefCell::new(None)};}
fn script() -> Rc<RefCell<Script>> {
    SCRIPT.with(|s| s.borrow().as_ref().unwrap().clone())
}
fn record(s: &mut Script, name: impl Into<String>) -> Hr {
    let name = name.into();
    let failed = s.fault == name;
    s.trace.push(name);
    if failed { -1 } else { 0 }
}
fn demand(s: &mut Script, good: bool, name: &str) {
    if !good {
        s.bad.push(name.into());
    }
}
fn equal(a: &GUID, b: &GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}
struct Fixture;
impl Fixture {
    fn new(fault: &'static str, id: &str, flow: i32) -> Self {
        SCRIPT.with(|s| {
            assert!(s.borrow().is_none());
            *s.borrow_mut() = Some(Rc::new(RefCell::new(Script {
                fault,
                id: id.into(),
                flow,
                state: 1,
                supported: true,
                ..Script::default()
            })));
        });
        Self
    }
    fn finish(&self, name: &str, result: Result<()>) -> Value {
        let shared = script();
        let s = shared.borrow();
        assert!(s.bad.is_empty(), "{name}: {:?}", s.bad);
        assert_eq!(
            (
                s.live,
                s.apartments,
                s.strings.len(),
                s.events,
                s.priorities
            ),
            (0, 0, 0, 0, 0),
            "{name}: {:?}",
            s.trace
        );
        json!({"name":name,"result":format!("{result:?}"),"trace":s.trace,"live":s.live,"apartments":s.apartments,"allocatedStrings":s.strings.len(),"events":s.events,"priorities":s.priorities,"bad":s.bad})
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        SCRIPT.with(|s| *s.borrow_mut() = None);
    }
}
#[repr(C)]
struct Object {
    table: *const std::ffi::c_void,
    kind: &'static str,
}
fn object(kind: &'static str) -> Raw {
    script().borrow_mut().live += 1;
    let table = match kind {
        "enumerator" => (&ENUMERATOR as *const Enumerator).cast(),
        "device" => (&DEVICE as *const Device).cast(),
        "endpoint" => (&FLOW as *const Endpoint).cast(),
        "client" => (&AUDIO as *const super::super::Client).cast(),
        "capture" => (&CAPTURE as *const super::super::Capture).cast(),
        _ => (&UNKNOWN as *const super::super::Unknown).cast(),
    };
    Box::into_raw(Box::new(Object { table, kind })).cast()
}
unsafe extern "system" fn query(_: Raw, iid: *const GUID, out: *mut Raw) -> Hr {
    let hr = {
        let shared = script();
        let mut s = shared.borrow_mut();
        demand(&mut s, equal(unsafe { &*iid }, &ENDPOINT), "endpoint IID");
        record(&mut s, "query")
    };
    unsafe {
        *out = object("endpoint");
    }
    hr
}
unsafe extern "system" fn add(_: Raw) -> u32 {
    1
}
unsafe extern "system" fn release(raw: Raw) -> u32 {
    let object = unsafe { Box::from_raw(raw.cast::<Object>()) };
    let shared = script();
    let mut s = shared.borrow_mut();
    record(&mut s, format!("release:{}", object.kind));
    s.live -= 1;
    0
}
const fn unknown() -> super::super::Unknown {
    super::super::Unknown {
        query,
        add,
        release,
    }
}
static UNKNOWN: super::super::Unknown = unknown();
static ENUMERATOR: Enumerator = Enumerator {
    unknown: unknown(),
    unused: [0; 2],
    get,
};
static DEVICE: Device = Device {
    unknown: unknown(),
    activate,
    properties: 0,
    id,
    state,
};
static FLOW: Endpoint = Endpoint {
    unknown: unknown(),
    flow,
};
static AUDIO: super::super::Client = super::super::Client {
    unknown: unknown(),
    initialize: initialize_client,
    unused: [0; 6],
    start: start_client,
    stop: stop_client,
    reset: reset_client,
    event: bind_event,
    service: capture_service,
};
static CAPTURE: super::super::Capture = super::super::Capture {
    unknown: unknown(),
    buffer: no_buffer,
    release: release_packet,
    next: next_packet,
};
unsafe extern "system" fn initialize_client(
    _: Raw,
    mode: i32,
    flags: u32,
    duration: i64,
    period: i64,
    format: *const super::super::Format,
    _: *const GUID,
) -> Hr {
    let shared = script();
    let mut s = shared.borrow_mut();
    let f = unsafe { ptr::read_unaligned(format) };
    demand(
        &mut s,
        mode == 0
            && flags == 0x88040000
            && duration == 400000
            && period == 0
            && (f.tag, f.channels, f.rate, f.bytes, f.align, f.bits, f.extra)
                == (3, 2, 48000, 384000, 8, 32, 0),
        "capture format",
    );
    record(&mut s, "pump-initialize")
}
unsafe extern "system" fn start_client(_: Raw) -> Hr {
    record(&mut script().borrow_mut(), "pump-start")
}
unsafe extern "system" fn stop_client(_: Raw) -> Hr {
    record(&mut script().borrow_mut(), "pump-stop")
}
unsafe extern "system" fn reset_client(_: Raw) -> Hr {
    record(&mut script().borrow_mut(), "pump-reset")
}
unsafe extern "system" fn bind_event(_: Raw, event: Raw) -> Hr {
    let shared = script();
    let mut s = shared.borrow_mut();
    demand(&mut s, event as usize == 1, "pump event");
    record(&mut s, "pump-bind")
}
unsafe extern "system" fn capture_service(_: Raw, iid: *const GUID, out: *mut Raw) -> Hr {
    let hr = {
        let shared = script();
        let mut s = shared.borrow_mut();
        demand(
            &mut s,
            equal(unsafe { &*iid }, &super::super::CAPTURE),
            "capture service",
        );
        record(&mut s, "pump-service")
    };
    unsafe {
        *out = object("capture");
    }
    hr
}
unsafe extern "system" fn no_buffer(
    _: Raw,
    _: *mut *mut u8,
    _: *mut u32,
    _: *mut u32,
    _: *mut u64,
    _: *mut u64,
) -> Hr {
    -1
}
unsafe extern "system" fn release_packet(_: Raw, _: u32) -> Hr {
    -1
}
unsafe extern "system" fn next_packet(_: Raw, out: *mut u32) -> Hr {
    unsafe {
        *out = 0;
    }
    0
}
unsafe extern "system" fn event() -> Raw {
    let shared = script();
    let mut s = shared.borrow_mut();
    if record(&mut s, "pump-event") < 0 {
        return ptr::null_mut();
    }
    s.events += 1;
    1usize as Raw
}
unsafe extern "system" fn wait(_: Raw, _: u32) -> u32 {
    u32::MAX
}
unsafe extern "system" fn close_event(_: Raw) -> i32 {
    let shared = script();
    let mut s = shared.borrow_mut();
    record(&mut s, "pump-close");
    s.events -= 1;
    1
}
unsafe extern "system" fn priority(_: *const u16, _: *mut u32) -> Raw {
    let shared = script();
    let mut s = shared.borrow_mut();
    record(&mut s, "pump-priority");
    s.priorities += 1;
    2usize as Raw
}
unsafe extern "system" fn revert(_: Raw) -> i32 {
    let shared = script();
    let mut s = shared.borrow_mut();
    record(&mut s, "pump-revert");
    s.priorities -= 1;
    1
}
fn pump_api() -> super::super::Api {
    super::super::Api {
        event,
        wait,
        close: close_event,
        priority,
        revert,
    }
}
unsafe extern "system" fn initialize(reserved: Raw, mode: u32) -> Hr {
    let shared = script();
    let mut s = shared.borrow_mut();
    demand(&mut s, reserved.is_null() && mode == 0, "MTA apartment");
    let hr = record(&mut s, "initialize");
    if s.fault == "changed-mode" {
        return 0x80010106u32 as i32;
    }
    if hr >= 0 {
        s.apartments += 1;
    }
    if s.fault == "init-sfalse" { 1 } else { hr }
}
unsafe extern "system" fn uninitialize() {
    let shared = script();
    let mut s = shared.borrow_mut();
    record(&mut s, "uninitialize");
    s.apartments -= 1;
}
unsafe extern "system" fn create(
    class: *const GUID,
    outer: Raw,
    context: u32,
    iid: *const GUID,
    out: *mut Raw,
) -> Hr {
    let hr = {
        let shared = script();
        let mut s = shared.borrow_mut();
        demand(
            &mut s,
            equal(unsafe { &*class }, &CLASS)
                && equal(unsafe { &*iid }, &DEVICES)
                && outer.is_null()
                && context == 1,
            "enumerator activation",
        );
        record(&mut s, "create")
    };
    unsafe {
        *out = object("enumerator");
    }
    if script().borrow().fault == "create-null" {
        unsafe {
            release(*out);
            *out = ptr::null_mut();
        }
    }
    hr
}
unsafe extern "system" fn get(_: Raw, id: *const u16, out: *mut Raw) -> Hr {
    let mut n = 0;
    while unsafe { *id.add(n) } != 0 {
        n += 1;
    }
    let actual = String::from_utf16(unsafe { std::slice::from_raw_parts(id, n) }).unwrap();
    let hr = {
        let shared = script();
        let mut s = shared.borrow_mut();
        let expected = s.id.clone();
        demand(&mut s, actual == expected, "exact requested endpoint");
        record(&mut s, "get")
    };
    unsafe {
        *out = object("device");
    }
    hr
}
unsafe extern "system" fn id(_: Raw, out: *mut *mut u16) -> Hr {
    let shared = script();
    let mut s = shared.borrow_mut();
    let hr = record(&mut s, "id");
    if s.fault == "id-null" {
        unsafe {
            *out = ptr::null_mut();
        }
        return 0;
    }
    let value: Vec<u16> = match s.fault {
        "id-changed" => "other\0".encode_utf16().collect(),
        "id-invalid" => vec![0xd800, 0],
        "id-unbounded" => vec![65; 32768],
        _ => s.id.encode_utf16().chain(Some(0)).collect(),
    };
    let mut value = value.into_boxed_slice();
    let raw = value.as_mut_ptr();
    s.strings.insert(raw as usize, value);
    unsafe {
        *out = raw;
    }
    hr
}
unsafe extern "system" fn free(raw: Raw) {
    let shared = script();
    let mut s = shared.borrow_mut();
    record(&mut s, "free");
    let existed = s.strings.remove(&(raw as usize)).is_some();
    demand(&mut s, existed, "foreign/double free");
}
unsafe extern "system" fn flow(_: Raw, out: *mut i32) -> Hr {
    let shared = script();
    let mut s = shared.borrow_mut();
    unsafe {
        *out = s.flow;
    }
    record(&mut s, "flow")
}
unsafe extern "system" fn state(_: Raw, out: *mut u32) -> Hr {
    let shared = script();
    let mut s = shared.borrow_mut();
    unsafe {
        *out = s.state;
    }
    record(&mut s, "state")
}
unsafe extern "system" fn activate(
    _: Raw,
    iid: *const GUID,
    context: u32,
    params: Raw,
    out: *mut Raw,
) -> Hr {
    let hr = {
        let shared = script();
        let mut s = shared.borrow_mut();
        demand(
            &mut s,
            equal(unsafe { &*iid }, &CLIENT) && context == 23 && params.is_null(),
            "audio client activation",
        );
        record(&mut s, "activate")
    };
    unsafe {
        *out = object("client");
    }
    if script().borrow().fault == "activate-null" {
        unsafe {
            release(*out);
            *out = ptr::null_mut();
        }
    }
    hr
}
fn process(pid: u32, include: bool, out: *mut Raw) -> Hr {
    let hr = {
        let shared = script();
        let mut s = shared.borrow_mut();
        record(&mut s, format!("process:{pid}:{include}"));
        if s.fault == "process" { -1 } else { 0 }
    };
    unsafe {
        *out = object("client");
    }
    hr
}
fn supported() -> bool {
    let shared = script();
    let mut s = shared.borrow_mut();
    record(&mut s, "supported");
    s.supported
}
fn api() -> Api {
    Api {
        initialize,
        uninitialize,
        create,
        free,
        process,
        supported,
    }
}
fn save(name: &str, rows: &[Value]) {
    if let Some(root) = std::env::var_os("IVY_PHONE_CAPTURE_OWNER_TRACE") {
        let root = std::path::PathBuf::from(root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join(format!("device-{name}.json")),
            serde_json::to_vec(rows).unwrap(),
        )
        .unwrap();
    }
}
fn request(source: Source) -> Request {
    Request {
        source,
        buffer_ms: 40,
        desktop: None,
    }
}
#[test]
fn deferred_activation_retires_clients_before_releasing_endpoint_and_apartment() {
    let mut rows = vec![];
    for fault in [
        "",
        "activate",
        "activate-null",
        "pump-initialize",
        "pump-event",
        "pump-bind",
    ] {
        let f = Fixture::new(fault, "mic", 1);
        let request = request(Source::Microphone("mic".into()));
        let (opened, validation) = Opened::admit_retained(&request, api()).unwrap();
        let mut admission = Admission {
            opened,
            request,
            validation,
        };
        assert_eq!((script().borrow().live, script().borrow().events), (1, 0));
        assert!(!script().borrow().trace.contains(&"activate".into()));
        let prepared = admission.configure_with(|client, mode, ms| unsafe {
            CapturePump::prepare(client.into_raw(), mode, ms, pump_api())
        });
        let error = prepared.as_ref().err().copied();
        if fault.is_empty() {
            assert!(prepared.is_ok());
        } else {
            assert!(error.is_some(), "{fault}");
        }
        drop(prepared);
        admission.retire();
        admission.retire();
        assert_eq!(
            (
                script().borrow().live,
                script().borrow().events,
                script().borrow().apartments
            ),
            (1, 0, 1)
        );
        assert!(!script().borrow().trace.contains(&"release:device".into()));
        drop(admission);
        rows.push(f.finish(
            if fault.is_empty() {
                "configured-client-retired-device-retained"
            } else {
                fault
            },
            error.map_or(Ok(()), Err),
        ));
    }
    for source in [
        Source::Microphone("mic".into()),
        Source::Process {
            pid: 71,
            include_tree: true,
        },
    ] {
        let f = Fixture::new("", "mic", 1);
        let request = request(source);
        let opened = Opened::admit(&request, api()).unwrap();
        assert!(
            !script()
                .borrow()
                .trace
                .iter()
                .any(|s| s == "activate" || s.starts_with("process:"))
        );
        drop(Admission {
            opened,
            request,
            validation: Ok(()),
        });
        rows.push(f.finish("unconfigured-admission", Ok(())));
    }
    for (fault, flow, state) in [
        ("id", 1, 1),
        ("id-changed", 1, 1),
        ("query", 1, 1),
        ("flow", 1, 1),
        ("state", 1, 1),
        ("", 0, 1),
        ("", 1, 2),
    ] {
        let f = Fixture::new(fault, "mic", flow);
        script().borrow_mut().state = state;
        let request = request(Source::Microphone("mic".into()));
        let (opened, validation) = Opened::admit_retained(&request, api()).unwrap();
        assert!(validation.is_err());
        let mut admission = Admission {
            opened,
            request,
            validation,
        };
        assert!(
            admission
                .configure_with(|_, _, _| panic!("invalid endpoint activated"))
                .is_err()
        );
        admission.retire();
        assert_eq!(
            (script().borrow().live, script().borrow().apartments),
            (1, 1)
        );
        assert!(!script().borrow().trace.contains(&"release:device".into()));
        drop(admission);
        rows.push(f.finish(
            &format!("retained-validation-{fault}-flow-{flow}-state-{state}"),
            validation,
        ));
    }
    save("retirement", &rows);
}

#[test]
fn exact_endpoint_activation_and_all_failed_outputs_are_retired() {
    assert_eq!(std::mem::offset_of!(Enumerator, get), 5 * 8);
    assert_eq!(std::mem::offset_of!(Device, id), 5 * 8);
    assert_eq!(std::mem::offset_of!(Endpoint, flow), 3 * 8);
    let mut rows = vec![];
    for (source, flow) in [
        (Source::Microphone("mic-λ".into()), 1),
        (Source::Loopback("loop-λ".into()), 0),
    ] {
        let id = match &source {
            Source::Microphone(s) | Source::Loopback(s) => s,
            _ => unreachable!(),
        };
        let f = Fixture::new("", id, flow);
        let opened = Opened::open(&request(source.clone()), api()).unwrap();
        assert_eq!(script().borrow().live, 2);
        assert!(opened.device.is_some());
        drop(opened);
        let trace = script().borrow().trace.clone();
        assert!(trace.ends_with(&[
            "release:client".into(),
            "release:device".into(),
            "uninitialize".into()
        ]));
        rows.push(f.finish(if flow == 1 { "microphone" } else { "loopback" }, Ok(())));
    }
    for (fault, error) in [
        ("initialize", CaptureError::Client),
        ("changed-mode", CaptureError::Client),
        ("create", CaptureError::Client),
        ("create-null", CaptureError::Client),
        ("get", CaptureError::Client),
        ("id", CaptureError::Client),
        ("id-null", CaptureError::Client),
        ("id-changed", CaptureError::Owner),
        ("id-invalid", CaptureError::Client),
        ("id-unbounded", CaptureError::Client),
        ("query", CaptureError::Client),
        ("flow", CaptureError::Client),
        ("state", CaptureError::Client),
        ("activate", CaptureError::Client),
        ("activate-null", CaptureError::Client),
    ] {
        let f = Fixture::new(fault, "mic", 1);
        let result = Opened::open(&request(Source::Microphone("mic".into())), api()).map(drop);
        assert_eq!(result, Err(error), "{fault}");
        rows.push(f.finish(fault, result));
    }
    for bad_state in [0, 2, 4, 8, 3] {
        let f = Fixture::new("", "mic", 1);
        script().borrow_mut().state = bad_state;
        let result = Opened::open(&request(Source::Microphone("mic".into())), api()).map(drop);
        assert_eq!(result, Err(CaptureError::Owner));
        assert!(!script().borrow().trace.contains(&"activate".into()));
        rows.push(f.finish(&format!("state-{bad_state}"), result));
    }
    for flow in [-1, 0, 2] {
        let f = Fixture::new("", "mic", flow);
        let result = Opened::open(&request(Source::Microphone("mic".into())), api()).map(drop);
        assert_eq!(result, Err(CaptureError::Owner));
        assert!(!script().borrow().trace.contains(&"state".into()));
        rows.push(f.finish(&format!("flow-{flow}"), result));
    }
    let f = Fixture::new("init-sfalse", "mic", 1);
    let result = Opened::open(&request(Source::Microphone("mic".into())), api()).map(drop);
    assert_eq!(result, Ok(()));
    rows.push(f.finish("balanced-sfalse", result));
    save("endpoints", &rows);
}

#[test]
fn process_tree_selection_and_request_validation_precede_native_work() {
    let mut rows = vec![];
    for (pid, include_tree) in [(std::process::id(), false), (71, true)] {
        let f = Fixture::new("", "", 0);
        let source = Source::Process { pid, include_tree };
        assert_eq!(request(source.clone()).mode(), CaptureMode::ProcessLoopback);
        let result = Opened::open(&request(source), api()).map(drop);
        assert_eq!(result, Ok(()));
        assert_eq!(
            script().borrow().trace,
            vec![
                "supported".into(),
                "initialize".into(),
                format!("process:{pid}:{include_tree}"),
                "release:client".into(),
                "uninitialize".into()
            ]
        );
        rows.push(f.finish(if include_tree { "include" } else { "exclude" }, result));
    }
    let f = Fixture::new("process", "", 0);
    let result = Opened::open(
        &request(Source::Process {
            pid: 71,
            include_tree: true,
        }),
        api(),
    )
    .map(drop);
    assert_eq!(result, Err(CaptureError::Client));
    rows.push(f.finish("process-failed-output", result));
    drop(f);
    let f = Fixture::new("", "", 0);
    script().borrow_mut().supported = false;
    let result = Opened::open(
        &request(Source::Process {
            pid: 71,
            include_tree: true,
        }),
        api(),
    )
    .map(drop);
    assert_eq!(result, Err(CaptureError::Unavailable));
    assert_eq!(script().borrow().trace, ["supported"]);
    rows.push(f.finish("unsupported-before-com", result));
    drop(f);
    for source in [
        Source::Microphone(" ".into()),
        Source::Loopback("a\0b".into()),
        Source::Microphone("x".repeat(32768)),
        Source::Process {
            pid: 0,
            include_tree: false,
        },
    ] {
        let f = Fixture::new("", "", 0);
        let result = Opened::open(&request(source), api()).map(drop);
        assert_eq!(result, Err(CaptureError::Configuration));
        assert!(script().borrow().trace.is_empty());
        rows.push(f.finish("invalid-source", result));
    }
    for ms in [0, 9, 101] {
        let f = Fixture::new("", "mic", 1);
        let mut r = request(Source::Microphone("mic".into()));
        r.buffer_ms = ms;
        let result = Opened::open(&r, api()).map(drop);
        assert_eq!(result, Err(CaptureError::Configuration));
        assert!(script().borrow().trace.is_empty());
        rows.push(f.finish(&format!("buffer-{ms}"), result));
    }
    save("process", &rows);
}

#[test]
fn settings_select_only_the_exact_original_source() {
    let mut settings = AudioSettings {
        capture_endpoint: "mic".into(),
        render_endpoint: "speaker".into(),
        source_mode: "microphone".into(),
        capture_buffer_ms: 40,
        render_latency_ms: 20,
        queue_ms: 80,
        mute: None,
        playback_prebuffer_ms: 0,
        enable_wasapi_low_latency: false,
        loopback_render_endpoint: Some("loop".into()),
    };
    let desktop = AudioDesktopIdentity::new(
        71,
        99,
        std::path::Path::new("C:/synthetic/editor.exe"),
        "editor!App",
    )
    .unwrap();
    for (mode, expected) in [
        ("microphone", Source::Microphone("mic".into())),
        ("virtual_speaker", Source::Loopback("loop".into())),
        (
            "system_excluding_runtime",
            Source::Process {
                pid: std::process::id(),
                include_tree: false,
            },
        ),
        (
            "desktop_process",
            Source::Process {
                pid: 71,
                include_tree: true,
            },
        ),
    ] {
        settings.source_mode = mode.into();
        let request = Request::new(&settings, Some(&desktop)).unwrap();
        assert_eq!(request.source, expected);
        assert_eq!(request.buffer_ms, 40);
        assert_eq!(request.desktop.unwrap().pid(), 71);
    }
    assert!(matches!(
        Request::new(&settings, None),
        Err(CaptureError::Owner)
    ));
    settings.queue_ms = 20;
    assert!(matches!(
        Request::new(&settings, Some(&desktop)),
        Err(CaptureError::Configuration)
    ));
    save(
        "settings",
        &[
            json!({"sourceModes":4,"missingDesktopRefused":true,"invalidSettingsRefused":true,"nativeCalls":0}),
        ],
    );
}

#[test]
fn pump_preparation_owns_the_activated_reference_through_disposal() {
    let mut rows = vec![];
    for (fault, expected) in [
        ("", None),
        ("pump-initialize", Some(CaptureError::Initialize)),
        ("pump-event", Some(CaptureError::Event)),
        ("pump-bind", Some(CaptureError::BindEvent)),
    ] {
        let f = Fixture::new(fault, "mic", 1);
        let request = request(Source::Microphone("mic".into()));
        let opened = Opened::open(&request, api()).unwrap();
        let prepared = opened.prepare(&request, |client, mode, ms| unsafe {
            CapturePump::prepare(client.into_raw(), mode, ms, pump_api())
        });
        let result = match prepared {
            Ok(mut stream) => {
                assert!(expected.is_none());
                let result = stream.run(&AtomicBool::new(true), &mut |_, _, _, _| {
                    panic!("stopped pump delivered a packet")
                });
                assert_eq!(result, Ok(()));
                assert_eq!(script().borrow().live, 3);
                assert_eq!(script().borrow().events, 1);
                assert!(!script().borrow().trace.contains(&"release:client".into()));
                drop(stream);
                result
            }
            Err(error) => {
                assert_eq!(Some(error), expected);
                Err(error)
            }
        };
        let trace = script().borrow().trace.clone();
        assert!(trace.ends_with(&["release:device".into(), "uninitialize".into()]));
        if fault.is_empty() {
            assert!(trace.windows(5).any(|p| p
                == [
                    "release:capture",
                    "release:client",
                    "pump-close",
                    "release:device",
                    "uninitialize"
                ]));
        }
        rows.push(f.finish(
            if fault.is_empty() {
                "retained-native-pump"
            } else {
                fault
            },
            result,
        ));
    }
    save("pump", &rows);
}
