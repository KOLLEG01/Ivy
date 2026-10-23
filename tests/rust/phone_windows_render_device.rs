use super::*;
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{Arc, atomic::Ordering},
};
struct Script {
    faults: Vec<String>,
    trace: Vec<String>,
    bad: Vec<String>,
    threads: Vec<String>,
    live: usize,
    apartments: i32,
    allocations: HashMap<usize, Box<[u16]>>,
    events: HashMap<usize, bool>,
    id: String,
    flow: i32,
    state: u32,
    capture_id: String,
    capture_flow: i32,
    capture_state: u32,
    mix: Vec<u8>,
    activations: usize,
    client3: bool,
    fresh_client3: bool,
    periods: [u32; 4],
    latency: i64,
    frames: u32,
    align: u16,
    buffer: Vec<u8>,
    blocks: Vec<String>,
    gets: usize,
    waits: usize,
    stop: Arc<AtomicBool>,
    permitted: Arc<AtomicBool>,
}
type Shared = Rc<RefCell<Script>>;
thread_local! {static SCRIPT:RefCell<Option<Shared>>=const{RefCell::new(None)};}
fn script() -> Shared {
    SCRIPT.with(|x| x.borrow().as_ref().unwrap().clone())
}
impl Script {
    fn log(&mut self, name: impl Into<String>) -> Hr {
        let name = name.into();
        let fail = self.faults.contains(&name);
        self.trace.push(name);
        let thread = format!("{:?}", std::thread::current().id());
        if !self.threads.contains(&thread) {
            self.threads.push(thread);
        }
        if fail { -1 } else { 0 }
    }
    fn fault(&self, name: &str) -> bool {
        self.faults.iter().any(|x| x == name)
    }
    fn demand(&mut self, good: bool, name: &str) {
        if !good {
            self.bad.push(name.into());
        }
    }
}
fn same(a: &GUID, b: &GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|x| format!("{x:02x}")).collect()
}
fn mix(tag: u16, bits: u16, rate: u32, channels: u16, sub: u128) -> Vec<u8> {
    let extra = if tag == 65534 { 22 } else { 0 };
    let align = channels * (bits / 8);
    let wave = Wave {
        tag,
        channels,
        rate,
        bytes: rate.wrapping_mul(u32::from(align)),
        align,
        bits,
        extra,
    };
    let mut bytes =
        unsafe { std::slice::from_raw_parts((&wave as *const Wave).cast::<u8>(), 18) }.to_vec();
    if extra > 0 {
        bytes.extend(bits.to_le_bytes());
        bytes.extend(3_u32.to_le_bytes());
        let guid = GUID::from_u128(sub);
        bytes
            .extend(unsafe { std::slice::from_raw_parts((&guid as *const GUID).cast::<u8>(), 16) });
    }
    bytes
}
struct Fixture {
    shared: Shared,
}
impl Fixture {
    fn new(faults: &[&str]) -> Self {
        let shared = Rc::new(RefCell::new(Script {
            faults: faults.iter().map(|x| (*x).into()).collect(),
            trace: vec![],
            bad: vec![],
            threads: vec![],
            live: 0,
            apartments: 0,
            allocations: HashMap::new(),
            events: HashMap::new(),
            id: "render-fixture".into(),
            flow: 0,
            state: 1,
            capture_id: "capture-fixture".into(),
            capture_flow: 1,
            capture_state: 1,
            mix: mix(65534, 32, 48000, 2, super::super::format::FLOAT_SUBTYPE),
            activations: 0,
            client3: true,
            fresh_client3: true,
            periods: [480, 48, 96, 960],
            latency: 153000,
            frames: 64,
            align: 8,
            buffer: vec![],
            blocks: vec![],
            gets: 0,
            waits: 0,
            stop: Arc::new(AtomicBool::new(false)),
            permitted: Arc::new(AtomicBool::new(true)),
        }));
        SCRIPT.with(|x| {
            assert!(x.borrow().is_none());
            *x.borrow_mut() = Some(shared.clone());
        });
        Self { shared }
    }
    fn request(&self, low_latency: bool) -> Request {
        Request {
            endpoint: "render-fixture".into(),
            latency_ms: 10,
            low_latency,
        }
    }
    fn row(&self, result: Option<RenderError>, info: Option<&Info>) -> Value {
        let s = self.shared.borrow();
        assert_eq!(
            (s.live, s.apartments, s.events.len(), s.allocations.len()),
            (0, 0, 0, 0)
        );
        assert!(s.bad.is_empty(), "{:?}", s.bad);
        assert!(s.threads.len() <= 1);
        json!({"faults":s.faults,"error":result.map(|x|format!("{x:?}")),"info":info.map(|i|json!({"rate":i.rate,"channels":i.channels,"latency":i.latency_ms,"low":i.low_latency,"unavailable":i.unavailable.map(|x|format!("{x:?}")),"mix":hex(&i.mix)})),"trace":s.trace,"bad":s.bad,"threads":s.threads,"live":[s.live,s.apartments as usize,s.events.len(),s.allocations.len()],"activations":s.activations,"blocks":s.blocks})
    }
    fn check(&self, low: bool, expected: Option<RenderError>) -> Value {
        let request = self.request(low);
        let result = Opened::open(&request, api()).and_then(|x| x.configure(&request));
        let info = result.as_ref().ok().map(|x| x.info.clone());
        let error = result.as_ref().err().copied();
        assert_eq!(error, expected, "{:?}", self.shared.borrow().faults);
        if result.is_ok() {
            let s = self.shared.borrow();
            assert!(s.live >= 3);
            assert_eq!((s.apartments, s.events.len()), (1, 2));
            assert!(!s.trace.iter().any(|x| x.starts_with("start:")));
        }
        drop(result);
        self.row(error, info.as_ref())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        SCRIPT.with(|x| *x.borrow_mut() = None);
    }
}
#[repr(C)]
struct Object {
    table: *const std::ffi::c_void,
    kind: &'static str,
    number: usize,
}
fn object(kind: &'static str, number: usize) -> Raw {
    script().borrow_mut().live += 1;
    let table = match kind {
        "enumerator" => (&ENUMERATOR as *const Enumerator).cast(),
        "device" => (&DEVICE as *const Device).cast(),
        "endpoint" => (&ENDPOINT_TABLE as *const Endpoint).cast(),
        "client" => (&AUDIO as *const Client).cast(),
        "client3" => (&AUDIO3 as *const Client3).cast(),
        "render" => (&RENDER_TABLE as *const Render).cast(),
        _ => panic!("fixture object"),
    };
    Box::into_raw(Box::new(Object {
        table,
        kind,
        number,
    }))
    .cast()
}
fn number(raw: Raw) -> usize {
    unsafe { (*raw.cast::<Object>()).number }
}
unsafe extern "system" fn release(raw: Raw) -> u32 {
    let value = unsafe { Box::from_raw(raw.cast::<Object>()) };
    let shared = script();
    let mut s = shared.borrow_mut();
    let in_apartment = s.apartments > 0;
    s.demand(in_apartment, "release before apartment exit");
    s.log(format!("release:{}:{}", value.kind, value.number));
    s.live -= 1;
    0
}
unsafe extern "system" fn query(raw: Raw, iid: *const GUID, out: *mut Raw) -> Hr {
    let n = number(raw);
    let endpoint = same(unsafe { &*iid }, &ENDPOINT);
    let client3 = same(unsafe { &*iid }, &CLIENT3);
    let name = if endpoint && n == 9 {
        "query:capture".into()
    } else if endpoint {
        "query:endpoint".into()
    } else {
        format!("query:client3:{n}")
    };
    let (hr, null) = {
        let shared = script();
        let mut s = shared.borrow_mut();
        s.demand(endpoint || client3, "query IID");
        let mut hr = s.log(&name);
        let supported = if n > 1 { s.fresh_client3 } else { s.client3 };
        if client3 && !supported {
            hr = -1;
        }
        (hr, s.fault(&(name + "-null")))
    };
    if !null {
        unsafe {
            *out = object(if endpoint { "endpoint" } else { "client3" }, n);
        }
    }
    hr
}
const fn unknown() -> Unknown {
    Unknown {
        query,
        add: 0,
        release,
    }
}
const fn client() -> Client {
    Client {
        unknown: unknown(),
        initialize: initialize_client,
        size: buffer_size,
        latency: stream_latency,
        padding,
        supported: 0,
        mix: mix_format,
        period: 0,
        start: start_client,
        stop: stop_client,
        reset: reset_client,
        event: bind_event,
        service,
    }
}
static ENUMERATOR: Enumerator = Enumerator {
    unknown: unknown(),
    unused: [0; 2],
    get,
};
static DEVICE: Device = Device {
    unknown: unknown(),
    activate,
    properties: 0,
    id: endpoint_id,
    state: device_state,
};
static ENDPOINT_TABLE: Endpoint = Endpoint {
    unknown: unknown(),
    flow,
};
static AUDIO: Client = client();
static AUDIO3: Client3 = Client3 {
    base: client(),
    client2: [0; 3],
    periods: period,
    current: 0,
    initialize: initialize_low,
};
#[repr(C)]
struct Render {
    unknown: Unknown,
    buffer: unsafe extern "system" fn(Raw, u32, *mut *mut u8) -> Hr,
    release: unsafe extern "system" fn(Raw, u32, u32) -> Hr,
}
static RENDER_TABLE: Render = Render {
    unknown: unknown(),
    buffer: render_buffer,
    release: release_buffer,
};
unsafe extern "system" fn initialize(reserved: Raw, mode: u32) -> Hr {
    let shared = script();
    let mut s = shared.borrow_mut();
    s.demand(reserved.is_null() && mode == 0, "MTA initialization");
    let mut hr = s.log("initialize");
    if s.fault("changed-mode") {
        hr = 0x80010106_u32 as i32;
    }
    if hr >= 0 {
        s.apartments += 1;
    }
    if s.fault("s-false") { 1 } else { hr }
}
unsafe extern "system" fn uninitialize() {
    let shared = script();
    let mut s = shared.borrow_mut();
    let clean = s.live == 0 && s.events.is_empty() && s.allocations.is_empty();
    s.demand(clean, "apartment exits last");
    s.log("uninitialize");
    s.apartments -= 1;
}
unsafe extern "system" fn create(
    class: *const GUID,
    outer: Raw,
    context: u32,
    iid: *const GUID,
    out: *mut Raw,
) -> Hr {
    let (hr, null) = {
        let shared = script();
        let mut s = shared.borrow_mut();
        s.demand(
            same(unsafe { &*class }, &CLASS)
                && same(unsafe { &*iid }, &DEVICES)
                && outer.is_null()
                && context == 1,
            "enumerator activation",
        );
        (s.log("create"), s.fault("create-null"))
    };
    if !null {
        unsafe {
            *out = object("enumerator", 0);
        }
    }
    hr
}
unsafe extern "system" fn get(_: Raw, id: *const u16, out: *mut Raw) -> Hr {
    let mut len = 0;
    while unsafe { *id.add(len) } != 0 {
        len += 1;
    }
    let id = String::from_utf16(unsafe { std::slice::from_raw_parts(id, len) }).unwrap();
    let capture = id == "capture-fixture";
    let (hr, null) = {
        let shared = script();
        let mut s = shared.borrow_mut();
        s.demand(
            capture || id == "render-fixture",
            "exact requested endpoint",
        );
        let name = if capture { "get:capture" } else { "get" };
        (s.log(name), s.fault(&(name.to_owned() + "-null")))
    };
    if !null {
        unsafe {
            *out = object("device", if capture { 9 } else { 0 });
        }
    }
    hr
}
fn allocate(words: Vec<u16>) -> Raw {
    let mut words = words.into_boxed_slice();
    let raw = words.as_mut_ptr();
    script()
        .borrow_mut()
        .allocations
        .insert(raw as usize, words);
    raw.cast()
}
unsafe extern "system" fn endpoint_id(raw: Raw, out: *mut *mut u16) -> Hr {
    let (hr, words) = {
        let shared = script();
        let mut s = shared.borrow_mut();
        let capture = number(raw) == 9;
        let name = if capture { "id:capture" } else { "id" };
        let hr = s.log(name);
        let words = if s.fault(&(name.to_owned() + "-null")) {
            None
        } else if s.fault(&(name.to_owned() + "-utf16")) {
            Some(vec![0xd800, 0])
        } else if s.fault(&(name.to_owned() + "-long")) {
            Some(vec![65; 32769])
        } else if s.fault(&(name.to_owned() + "-wrong")) {
            Some("RENDER-fixture".encode_utf16().chain(Some(0)).collect())
        } else {
            Some(
                (if capture { &s.capture_id } else { &s.id })
                    .encode_utf16()
                    .chain(Some(0))
                    .collect(),
            )
        };
        (hr, words)
    };
    if let Some(words) = words {
        unsafe {
            *out = allocate(words).cast();
        }
    }
    hr
}
unsafe extern "system" fn free(raw: Raw) {
    let shared = script();
    let mut s = shared.borrow_mut();
    let owned = s.allocations.remove(&(raw as usize)).is_some();
    s.demand(owned, "exact allocation free");
    s.log("free");
}
unsafe extern "system" fn flow(raw: Raw, out: *mut i32) -> Hr {
    let shared = script();
    let mut s = shared.borrow_mut();
    unsafe {
        *out = if number(raw) == 9 {
            s.capture_flow
        } else {
            s.flow
        };
    }
    s.log(if number(raw) == 9 {
        "flow:capture"
    } else {
        "flow"
    })
}
unsafe extern "system" fn device_state(raw: Raw, out: *mut u32) -> Hr {
    let shared = script();
    let mut s = shared.borrow_mut();
    unsafe {
        *out = if number(raw) == 9 {
            s.capture_state
        } else {
            s.state
        };
    }
    s.log(if number(raw) == 9 {
        "state:capture"
    } else {
        "state"
    })
}
unsafe extern "system" fn activate(
    raw: Raw,
    iid: *const GUID,
    context: u32,
    params: Raw,
    out: *mut Raw,
) -> Hr {
    let (hr, null, n) = {
        let shared = script();
        let mut s = shared.borrow_mut();
        s.demand(
            number(raw) == 0
                && same(unsafe { &*iid }, &CLIENT)
                && context == 23
                && params.is_null(),
            "exact client activation",
        );
        s.activations += 1;
        let n = s.activations;
        let name = format!("activate:{n}");
        (s.log(&name), s.fault(&(name + "-null")), n)
    };
    if !null {
        unsafe {
            *out = object("client", n);
        }
    }
    hr
}
unsafe extern "system" fn mix_format(raw: Raw, out: *mut *mut Wave) -> Hr {
    let n = number(raw);
    let (hr, bytes, null) = {
        let shared = script();
        let mut s = shared.borrow_mut();
        (
            s.log(format!("mix:{n}")),
            s.mix.clone(),
            s.fault("mix-null"),
        )
    };
    if !null {
        let format = Format::from_bytes(&bytes);
        unsafe {
            *out = allocate(format.words).cast();
        }
    }
    hr
}
fn format_values(raw: *const Wave) -> (u16, u16, u32, u32, u16, u16, u16) {
    let f = unsafe { ptr::read_unaligned(raw) };
    (f.tag, f.channels, f.rate, f.bytes, f.align, f.bits, f.extra)
}
fn mix_matches(raw: *const Wave, s: &Script) -> bool {
    let header = unsafe { ptr::read_unaligned(raw) };
    let actual =
        unsafe { std::slice::from_raw_parts(raw.cast::<u8>(), 18 + usize::from(header.extra)) };
    let mut expected = s.mix.clone();
    if u16::from_le_bytes(expected[..2].try_into().unwrap()) == 1 {
        expected.truncate(18);
        expected[16..18].fill(0);
    }
    actual == expected
}
unsafe extern "system" fn period(
    raw: Raw,
    format: *const Wave,
    default: *mut u32,
    fundamental: *mut u32,
    minimum: *mut u32,
    maximum: *mut u32,
) -> Hr {
    let n = number(raw);
    let shared = script();
    let mut s = shared.borrow_mut();
    let good = mix_matches(format, &s);
    s.demand(good, "engine period exact mix bytes");
    unsafe {
        *default = s.periods[0];
        *fundamental = s.periods[1];
        *minimum = s.periods[2];
        *maximum = s.periods[3];
    }
    s.log(format!("period:{n}"))
}
unsafe extern "system" fn initialize_low(
    raw: Raw,
    flags: u32,
    frames: u32,
    format: *const Wave,
    session: *const GUID,
) -> Hr {
    let n = number(raw);
    let shared = script();
    let mut s = shared.borrow_mut();
    let expected = EnginePeriod {
        default: s.periods[0],
        fundamental: s.periods[1],
        minimum: s.periods[2],
        maximum: s.periods[3],
    }
    .choose();
    let good = flags == 0x40000
        && frames == expected
        && mix_matches(format, &s)
        && same(unsafe { &*session }, &EMPTY);
    s.demand(
        good,
        "client3 init slot/flags/period/exact mix/empty session",
    );
    s.align = format_values(format).4;
    s.log(format!("low:{n}"))
}
unsafe extern "system" fn initialize_client(
    raw: Raw,
    mode: i32,
    flags: u32,
    duration: i64,
    period: i64,
    format: *const Wave,
    session: *const GUID,
) -> Hr {
    let n = number(raw);
    let shared = script();
    let mut s = shared.borrow_mut();
    let mix = unsafe { ptr::read_unaligned(s.mix.as_ptr().cast::<Wave>()) };
    let expected = (
        3,
        mix.channels,
        mix.rate,
        mix.rate.wrapping_mul(u32::from(mix.channels) * 4),
        mix.channels * 4,
        32,
        0,
    );
    s.demand(
        mode == 0
            && flags == 0x88040000
            && duration == 100000
            && period == 0
            && format_values(format) == expected
            && same(unsafe { &*session }, &EMPTY),
        "standard source/flags/latency/session",
    );
    s.align = mix.channels * 4;
    s.log(format!("standard:{n}"))
}
unsafe extern "system" fn stream_latency(raw: Raw, out: *mut i64) -> Hr {
    let n = number(raw);
    let shared = script();
    let mut s = shared.borrow_mut();
    unsafe {
        *out = s.latency;
    }
    s.log(format!("latency:{n}"))
}
unsafe extern "system" fn event(manual: bool) -> Raw {
    let shared = script();
    let mut s = shared.borrow_mut();
    let name = if manual { "event:stop" } else { "event:frame" };
    if s.log(name) < 0 {
        return ptr::null_mut();
    }
    let id = if manual { 100 } else { 101 };
    let fresh = s.events.insert(id, manual).is_none();
    s.demand(fresh, "event once");
    id as Raw
}
unsafe extern "system" fn close(raw: Raw) -> i32 {
    let shared = script();
    let mut s = shared.borrow_mut();
    let manual = s.events.remove(&(raw as usize));
    s.demand(manual.is_some(), "exact owned event close");
    s.log(if manual == Some(true) {
        "close:stop"
    } else {
        "close:frame"
    });
    1
}
unsafe extern "system" fn bind_event(raw: Raw, event: Raw) -> Hr {
    let n = number(raw);
    let shared = script();
    let mut s = shared.borrow_mut();
    let valid = s.events.get(&(event as usize)) == Some(&false);
    s.demand(valid, "bind auto-reset frame event");
    s.log(format!("bind:{n}"))
}
unsafe extern "system" fn service(raw: Raw, iid: *const GUID, out: *mut Raw) -> Hr {
    let n = number(raw);
    let (hr, null) = {
        let shared = script();
        let mut s = shared.borrow_mut();
        s.demand(same(unsafe { &*iid }, &RENDER), "render service IID");
        (s.log(format!("service:{n}")), s.fault("service-null"))
    };
    if !null {
        unsafe {
            *out = object("render", n);
        }
    }
    hr
}
unsafe extern "system" fn buffer_size(raw: Raw, out: *mut u32) -> Hr {
    let n = number(raw);
    let shared = script();
    let mut s = shared.borrow_mut();
    unsafe {
        *out = s.frames;
    }
    s.log(format!("size:{n}"))
}
unsafe extern "system" fn padding(_: Raw, out: *mut u32) -> Hr {
    unsafe {
        *out = 0;
    }
    script().borrow_mut().log("padding")
}
unsafe extern "system" fn start_client(raw: Raw) -> Hr {
    script().borrow_mut().log(format!("start:{}", number(raw)))
}
unsafe extern "system" fn stop_client(raw: Raw) -> Hr {
    script().borrow_mut().log(format!("stop:{}", number(raw)))
}
unsafe extern "system" fn reset_client(raw: Raw) -> Hr {
    script().borrow_mut().log(format!("reset:{}", number(raw)))
}
unsafe extern "system" fn render_buffer(_: Raw, frames: u32, out: *mut *mut u8) -> Hr {
    let shared = script();
    let mut s = shared.borrow_mut();
    s.gets += 1;
    s.buffer = vec![0xcc; frames as usize * usize::from(s.align)];
    unsafe {
        *out = s.buffer.as_mut_ptr();
    }
    s.log("buffer")
}
unsafe extern "system" fn release_buffer(_: Raw, frames: u32, flags: u32) -> Hr {
    let shared = script();
    let mut s = shared.borrow_mut();
    let expected = s.frames;
    s.demand(
        frames == expected && flags == 0,
        "complete render frame release",
    );
    let bytes = hex(&s.buffer);
    s.blocks.push(bytes);
    s.log("release-buffer")
}
unsafe extern "system" fn wait(frame: Raw, stop: Raw, _: u32) -> u32 {
    let shared = script();
    let mut s = shared.borrow_mut();
    let valid = s.events.get(&(frame as usize)) == Some(&false)
        && s.events.get(&(stop as usize)) == Some(&true);
    s.demand(valid, "two retained event handles");
    s.waits += 1;
    if s.waits == 1 {
        s.permitted.store(false, Ordering::SeqCst);
    } else {
        s.stop.store(true, Ordering::SeqCst);
    }
    s.log("wait");
    0
}
unsafe extern "system" fn priority(task: *const u16, _: *mut u32) -> Raw {
    let expected = [80, 114, 111, 32, 65, 117, 100, 105, 111, 0];
    let shared = script();
    let mut s = shared.borrow_mut();
    s.demand(
        unsafe { std::slice::from_raw_parts(task, 10) } == expected,
        "Pro Audio task",
    );
    s.log("priority");
    102usize as Raw
}
unsafe extern "system" fn revert(_: Raw) -> i32 {
    script().borrow_mut().log("revert");
    1
}
fn api() -> Api {
    Api {
        initialize,
        uninitialize,
        create,
        free,
        event,
        signal: set_signal,
        pump: PumpApi {
            wait,
            close,
            priority,
            revert,
        },
    }
}
unsafe extern "system" fn set_signal(raw: Raw) -> i32 {
    let shared = script();
    let mut s = shared.borrow_mut();
    let valid = s.events.get(&(raw as usize)) == Some(&true);
    s.demand(valid, "signal retained stop event");
    s.log("signal");
    1
}
fn save(name: &str, rows: &[Value]) {
    if let Some(dir) = std::env::var_os("IVY_PHONE_RENDER_DEVICE_TRACE") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            std::path::Path::new(&dir).join(format!("{name}.json")),
            serde_json::to_vec_pretty(rows).unwrap(),
        )
        .unwrap();
    }
}
#[test]
fn staged_stream_retirement_keeps_exact_device_and_apartment_until_close() {
    let mut rows = vec![];
    for fault in [
        "",
        "event:stop",
        "activate:1",
        "mix:1",
        "standard:1",
        "event:frame",
        "bind:1",
        "service:1",
    ] {
        let faults = if fault.is_empty() {
            vec![]
        } else {
            vec![fault]
        };
        let f = Fixture::new(&faults);
        let request = f.request(false);
        let mut admission = Admission {
            opened: Opened::admit(&request, Some("capture-fixture"), api()).unwrap(),
            request,
        };
        let configured = admission.configure();
        let error = configured.as_ref().err().copied();
        assert_eq!(configured.is_ok(), fault.is_empty(), "{fault}");
        let signal = configured.as_ref().ok().map(|s| s.stop_signal());
        drop(configured);
        admission.retire();
        admission.retire();
        {
            let s = f.shared.borrow();
            assert_eq!((s.live, s.apartments, s.events.len()), (1, 1, 0));
            assert!(!s.trace.contains(&"release:device:0".into()));
        }
        if let Some(signal) = signal {
            signal.set().unwrap();
        }
        drop(admission);
        let mut row = f.row(error, None);
        row["case"] = json!(fault);
        rows.push(row);
    }
    save("render-device-retirement", &rows);
}
fn has(row: &Value, text: &str) -> bool {
    row["trace"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x.as_str() == Some(text))
}

#[test]
fn exact_endpoint_and_partial_acquisitions_retire_before_apartment_exit() {
    let mut rows = vec![];
    for (fault, error) in [
        ("initialize", RenderError::Client),
        ("changed-mode", RenderError::Client),
        ("create", RenderError::Client),
        ("create-null", RenderError::Client),
        ("get", RenderError::Client),
        ("get-null", RenderError::Client),
        ("id", RenderError::Client),
        ("id-null", RenderError::Client),
        ("id-utf16", RenderError::Client),
        ("id-long", RenderError::Client),
        ("id-wrong", RenderError::Owner),
        ("query:endpoint", RenderError::Client),
        ("query:endpoint-null", RenderError::Client),
        ("flow", RenderError::Client),
        ("state", RenderError::Client),
        ("event:stop", RenderError::Event),
        ("activate:1", RenderError::Client),
        ("activate:1-null", RenderError::Client),
        ("mix:1", RenderError::Mix),
        ("mix-null", RenderError::Mix),
    ] {
        let f = Fixture::new(&[fault]);
        let row = f.check(true, Some(error));
        if fault.starts_with("id") {
            assert!(!has(&row, "flow"));
        }
        if fault == "activate:1" {
            assert!(has(&row, "close:stop"));
        }
        rows.push(row);
    }
    for (name, flow, state) in [
        ("wrong-flow", 1, 1),
        ("inactive", 0, 2),
        ("state-carrier", 0, 3),
    ] {
        let f = Fixture::new(&[]);
        {
            let mut s = f.shared.borrow_mut();
            s.flow = flow;
            s.state = state;
        }
        let mut row = f.check(true, Some(RenderError::Owner));
        row["case"] = json!(name);
        assert!(!has(&row, "activate:1"));
        rows.push(row);
    }
    for (name, rate, channels) in [
        ("zero-rate", 0, 2),
        ("signed-rate", u32::MAX, 2),
        ("zero-channel", 48000, 0),
        ("multichannel", 48000, 3),
    ] {
        let f = Fixture::new(&[]);
        f.shared.borrow_mut().mix = mix(3, 32, rate, channels, 0);
        let mut row = f.check(true, Some(RenderError::Configuration));
        row["case"] = json!(name);
        assert!(!has(&row, "period:1"));
        rows.push(row);
    }
    {
        let f = Fixture::new(&[]);
        f.shared.borrow_mut().mix[16..18].copy_from_slice(&20_u16.to_le_bytes());
        rows.push(f.check(true, Some(RenderError::Mix)));
    }
    {
        let f = Fixture::new(&["s-false"]);
        rows.push(f.check(true, None));
    }
    for (id, ms) in [
        ("", 10),
        ("x\0y", 10),
        ("render-fixture", 1),
        ("render-fixture", 101),
    ] {
        let f = Fixture::new(&[]);
        let request = Request {
            endpoint: id.into(),
            latency_ms: ms,
            low_latency: true,
        };
        let result = Opened::open(&request, api());
        assert_eq!(result.err().unwrap(), RenderError::Configuration);
        let row = f.row(Some(RenderError::Configuration), None);
        assert!(row["trace"].as_array().unwrap().is_empty());
        rows.push(row);
    }
    assert_eq!(rows.len(), 33);
    save("render-device-admission", &rows);
}

#[test]
fn shared_initialization_fallback_recreation_and_latency_have_distinct_boundaries() {
    let mut rows = vec![];
    for (name, low, tag, bits, expected, reason) in [
        ("low-float", true, 65534, 32, true, None),
        ("standard", false, 65534, 32, false, None),
        ("low-pcm16", true, 1, 16, true, None),
        ("low-pcm24", true, 65534, 24, true, None),
        ("pcm32-fallback", true, 1, 32, false, Some("Format")),
        ("float64-fallback", true, 3, 64, false, Some("Format")),
    ] {
        let f = Fixture::new(&[]);
        let sub = if bits == 24 {
            super::super::format::PCM_SUBTYPE
        } else {
            super::super::format::FLOAT_SUBTYPE
        };
        f.shared.borrow_mut().mix = mix(tag, bits, 48000, 2, sub);
        let mut row = f.check(low, None);
        row["case"] = json!(name);
        assert_eq!(row["info"]["low"], expected);
        assert_eq!(row["info"]["unavailable"], json!(reason));
        assert_eq!(row["info"]["latency"], if expected { 2 } else { 15 });
        rows.push(row);
    }
    for (fault, reason, recreated) in [
        ("query:client3:1", "Client3", false),
        ("query:client3:1-null", "Client3", false),
        ("period:1", "Engine", false),
        ("low:1", "Engine", true),
    ] {
        let f = Fixture::new(&[fault]);
        let mut row = f.check(true, None);
        row["case"] = json!(fault);
        assert_eq!(row["info"]["unavailable"], reason);
        assert_eq!(row["activations"], if recreated { 2 } else { 1 });
        assert!(has(
            &row,
            if recreated {
                "standard:2"
            } else {
                "standard:1"
            }
        ));
        if fault == "period:1" {
            assert!(!has(&row, "low:1"));
        }
        rows.push(row);
    }
    {
        let f = Fixture::new(&["low:1"]);
        f.shared.borrow_mut().fresh_client3 = false;
        let row = f.check(true, None);
        assert_eq!(row["info"]["unavailable"], "Client3");
        let trace = row["trace"].as_array().unwrap();
        let position = |s: &str| trace.iter().position(|x| x.as_str() == Some(s)).unwrap();
        assert!(
            position("release:client3:1") < position("release:client:1")
                && position("release:client:1") < position("activate:2")
        );
        rows.push(row);
    }
    for fault in ["activate:2", "activate:2-null"] {
        let f = Fixture::new(&["low:1", fault]);
        rows.push(f.check(true, Some(RenderError::Client)));
    }
    for low in [false, true] {
        for (fault, error) in [
            ("event:frame", RenderError::Event),
            ("bind:1", RenderError::BindEvent),
            ("service:1", RenderError::Service),
            ("service-null", RenderError::Service),
        ] {
            let f = Fixture::new(&[fault]);
            let row = f.check(low, Some(error));
            if low {
                assert!(!has(&row, "standard:1"));
            }
            rows.push(row);
        }
    }
    for (fault, error) in [
        ("standard:1", RenderError::Initialize),
        ("latency:1", RenderError::Latency),
    ] {
        let f = Fixture::new(&[fault]);
        rows.push(f.check(false, Some(error)));
    }
    for (latency, expected) in [(0, 10), (9999, 0), (150001, 15), (-1, 0)] {
        let f = Fixture::new(&[]);
        f.shared.borrow_mut().latency = latency;
        let mut row = f.check(false, None);
        row["streamLatency"] = json!(latency);
        assert_eq!(row["info"]["latency"], expected);
        rows.push(row);
    }
    {
        let f = Fixture::new(&[]);
        f.shared.borrow_mut().latency = -10000;
        rows.push(f.check(false, Some(RenderError::Bound)));
    }
    {
        let f = Fixture::new(&[]);
        f.shared.borrow_mut().periods = [480, 0, 0, 960];
        let row = f.check(true, None);
        assert_eq!(row["info"]["latency"], 1);
        rows.push(row);
    }
    {
        let f = Fixture::new(&[]);
        let mut bytes = mix(1, 16, 48000, 1, 0);
        bytes[16..18].copy_from_slice(&65535_u16.to_le_bytes());
        f.shared.borrow_mut().mix = bytes;
        let row = f.check(true, None);
        assert_eq!(row["info"]["channels"], 1);
        assert!(row["info"]["mix"].as_str().unwrap().ends_with("0000"));
        rows.push(row);
    }
    {
        let f = Fixture::new(&[]);
        f.shared.borrow_mut().mix[12..14].copy_from_slice(&6_u16.to_le_bytes());
        rows.push(f.check(true, Some(RenderError::Configuration)));
    }
    assert_eq!(rows.len(), 31);
    save("render-device-initialization", &rows);
}

#[test]
fn native_setup_to_pump_preserves_pcm_authority_and_retains_every_owner() {
    let settings = AudioSettings {
        capture_endpoint: "capture-fixture".into(),
        render_endpoint: "render-fixture".into(),
        source_mode: "microphone".into(),
        capture_buffer_ms: 20,
        render_latency_ms: 10,
        queue_ms: 200,
        mute: None,
        playback_prebuffer_ms: 0,
        enable_wasapi_low_latency: true,
        loopback_render_endpoint: None,
    };
    assert_eq!(
        Request::new(&settings).unwrap().endpoint,
        settings.render_endpoint
    );
    let mut rows = vec![];
    for rate in [24000, 48000, 96000] {
        for bits in [16, 24, 32, 64] {
            let f = Fixture::new(&[]);
            {
                let mut s = f.shared.borrow_mut();
                s.mix = mix(
                    if bits == 32 || bits == 64 { 3 } else { 1 },
                    bits,
                    rate,
                    2,
                    0,
                );
            }
            let request = f.request(true);
            let mut stream = Opened::open(&request, api())
                .unwrap()
                .configure(&request)
                .unwrap();
            let info = stream.info.clone();
            let signal = stream.stop_signal();
            assert_eq!(signal.set(), Ok(()));
            let (stop, permitted) = {
                let s = f.shared.borrow();
                (s.stop.clone(), s.permitted.clone())
            };
            let permit = permitted.clone();
            let mut owner = WindowsAudioBuffers::new(
                &settings,
                Arc::new(move || permit.load(Ordering::SeqCst)),
            )
            .unwrap();
            owner.resume().unwrap();
            owner.write_received(&[0.5; 200]);
            owner.captured(&[0; 16], 0);
            let buffers = Mutex::new(owner);
            assert_eq!(stream.run(&stop, &buffers), Ok(()));
            {
                let s = f.shared.borrow();
                assert!(s.live >= 3);
                assert_eq!((s.apartments, s.events.len()), (1, 2));
                assert_eq!(s.blocks.len(), 2);
                assert!(s.blocks[0].chars().any(|c| c != '0'));
                assert!(s.blocks[1].chars().all(|c| c == '0'));
            }
            let status = buffers.lock().unwrap().snapshot();
            assert_eq!(
                (status.state, status.capture_queued, status.render_queued),
                ("suspended", 0, 0)
            );
            drop(stream);
            // A capability retained by another owner cannot signal a retired
            // handle, even if the OS later reuses its numeric value.
            assert_eq!(signal.set(), Ok(()));
            assert_eq!(
                f.shared
                    .borrow()
                    .trace
                    .iter()
                    .filter(|x| x.as_str() == "signal")
                    .count(),
                1
            );
            let mut row = f.row(None, Some(&info));
            row["bits"] = json!(bits);
            row["state"] = json!(status.state);
            let trace = row["trace"].as_array().unwrap();
            let position = |s: &str| trace.iter().position(|x| x.as_str() == Some(s)).unwrap();
            assert!(
                position("release:render:1") < position("release:client:1")
                    && position("release:client:1") < position("close:stop")
                    && position("close:stop") < position("close:frame")
                    && position("close:frame") < position("release:device:0")
                    && position("release:device:0") < position("uninitialize")
            );
            rows.push(row);
        }
    }
    assert_eq!(rows.len(), 12);
    save("render-device-composition", &rows);
}

#[test]
fn route_pair_admission_precedes_events_and_reuses_the_render_device() {
    let mut rows = vec![];
    for (fault, error) in [
        ("get:capture", RenderError::Client),
        ("get:capture-null", RenderError::Client),
        ("id:capture", RenderError::Client),
        ("id:capture-null", RenderError::Client),
        ("id:capture-utf16", RenderError::Client),
        ("id:capture-long", RenderError::Client),
        ("id:capture-wrong", RenderError::Owner),
        ("query:capture", RenderError::Client),
        ("query:capture-null", RenderError::Client),
        ("flow:capture", RenderError::Client),
        ("state:capture", RenderError::Client),
        ("get", RenderError::Client),
        ("id", RenderError::Client),
        ("query:endpoint", RenderError::Client),
        ("flow", RenderError::Client),
        ("state", RenderError::Client),
    ] {
        let f = Fixture::new(&[fault]);
        let result = Opened::admit(&f.request(false), Some("capture-fixture"), api());
        assert_eq!(result.err(), Some(error));
        let mut row = f.row(Some(error), None);
        row["case"] = json!(fault);
        assert!(!has(&row, "event:stop") && !has(&row, "activate:1"));
        if fault.contains("capture") {
            assert!(!has(&row, "get"));
        }
        rows.push(row);
    }
    for (name, flow, state) in [
        ("capture-wrong-flow", 0, 1),
        ("capture-inactive", 1, 2),
        ("capture-state-carrier", 1, 3),
    ] {
        let f = Fixture::new(&[]);
        {
            let mut s = f.shared.borrow_mut();
            s.capture_flow = flow;
            s.capture_state = state;
        }
        assert_eq!(
            Opened::admit(&f.request(false), Some("capture-fixture"), api()).err(),
            Some(RenderError::Owner)
        );
        let mut row = f.row(Some(RenderError::Owner), None);
        row["case"] = json!(name);
        assert!(!has(&row, "get"));
        rows.push(row);
    }
    for id in [
        String::new(),
        "x\0y".into(),
        "x".repeat(32768),
        "render-fixture".into(),
    ] {
        let f = Fixture::new(&[]);
        assert_eq!(
            Opened::admit(&f.request(false), Some(&id), api()).err(),
            Some(RenderError::Configuration)
        );
        let mut row = f.row(Some(RenderError::Configuration), None);
        row["case"] = json!("invalid-capture-id");
        assert!(row["trace"].as_array().unwrap().is_empty());
        rows.push(row);
    }
    for configured in [None, Some(false), Some(true)] {
        let f = Fixture::new(&[]);
        let request = f.request(configured.unwrap_or(false));
        let admitted = Opened::admit(&request, Some("capture-fixture"), api()).unwrap();
        {
            let s = f.shared.borrow();
            assert_eq!(
                (s.live, s.apartments, s.activations, s.events.len()),
                (1, 1, 0, 0)
            );
            assert!(!s.trace.iter().any(|x| x.starts_with("activate:")
                || x.starts_with("mix:")
                || x.starts_with("event:")));
        }
        let info = if configured.is_some() {
            let stream = admitted.initialize().unwrap().configure(&request).unwrap();
            let info = stream.info.clone();
            drop(stream);
            Some(info)
        } else {
            drop(admitted);
            None
        };
        let mut row = f.row(None, info.as_ref());
        row["case"] = json!(format!("configured-{configured:?}"));
        let trace = row["trace"].as_array().unwrap();
        let pos = |value: &str| {
            trace
                .iter()
                .position(|x| x.as_str() == Some(value))
                .unwrap()
        };
        assert!(
            pos("get:capture") < pos("get")
                && pos("state") < pos("release:device:9")
                && pos("release:device:9") < pos("release:enumerator:0")
        );
        assert_eq!(
            trace.iter().filter(|x| x.as_str() == Some("get")).count(),
            1
        );
        if configured.is_some() {
            assert!(pos("release:enumerator:0") < pos("event:stop"));
            assert_eq!(row["activations"], 1);
        }
        rows.push(row);
    }
    for (fault, error) in [
        ("event:stop", RenderError::Event),
        ("activate:1", RenderError::Client),
        ("mix:1", RenderError::Mix),
    ] {
        let f = Fixture::new(&[fault]);
        let request = f.request(false);
        let admitted = Opened::admit(&request, Some("capture-fixture"), api()).unwrap();
        let configured = admitted.initialize().and_then(|x| x.configure(&request));
        assert_eq!(configured.err(), Some(error));
        let mut row = f.row(Some(error), None);
        row["case"] = json!(format!("deferred-{fault}"));
        rows.push(row);
    }
    assert_eq!(rows.len(), 29);
    save("render-device-route-admission", &rows);
}
