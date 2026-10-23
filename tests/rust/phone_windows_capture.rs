use super::*;
use serde_json::{Value, json};
use std::{cell::RefCell, sync::Arc};

#[derive(Clone)]
struct Data {
    frames: u32,
    flags: u32,
    pointer: u8,
    bytes: Vec<u8>,
}
impl Data {
    fn pcm(samples: &[f32]) -> Self {
        Self {
            frames: samples.len() as u32 / 2,
            flags: 0,
            pointer: 0,
            bytes: samples.iter().flat_map(|x| x.to_le_bytes()).collect(),
        }
    }
}
struct Script {
    fault: &'static str,
    trace: Vec<String>,
    violations: Vec<String>,
    threads: Vec<String>,
    packets: Vec<Data>,
    packet: usize,
    lease: Option<u32>,
    next_calls: usize,
    wait_calls: usize,
    stop: Arc<AtomicBool>,
    stop_at_wait: bool,
    clients: i32,
    captures: i32,
    events: i32,
    priorities: i32,
    format: Option<Value>,
}
type Shared = Rc<RefCell<Script>>;
fn equal(a: &GUID, b: &GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}
thread_local! { static CURRENT: RefCell<Option<Shared>> = const { RefCell::new(None) }; }
fn current() -> Shared {
    CURRENT.with(|x| x.borrow().as_ref().unwrap().clone())
}
impl Script {
    fn log(&mut self, event: impl Into<String>) {
        self.trace.push(event.into());
        let thread = format!("{:?}", std::thread::current().id());
        if !self.threads.contains(&thread) {
            self.threads.push(thread);
        }
    }
    fn require(&mut self, condition: bool, detail: &str) {
        if !condition {
            self.violations.push(detail.into());
        }
    }
}
struct Fixture {
    script: Shared,
}
impl Fixture {
    fn new(fault: &'static str, packets: Vec<Data>) -> Self {
        let script = Rc::new(RefCell::new(Script {
            fault,
            trace: vec![],
            violations: vec![],
            threads: vec![],
            packets,
            packet: 0,
            lease: None,
            next_calls: 0,
            wait_calls: 0,
            stop: Arc::new(AtomicBool::new(false)),
            stop_at_wait: false,
            clients: 0,
            captures: 0,
            events: 0,
            priorities: 0,
            format: None,
        }));
        CURRENT.with(|x| {
            assert!(x.borrow().is_none());
            *x.borrow_mut() = Some(script.clone());
        });
        Self { script }
    }
    fn client(&self) -> Raw {
        self.script.borrow_mut().clients += 1;
        Box::into_raw(Box::new(Object {
            table: (&CLIENT as *const Client).cast(),
            script: self.script.clone(),
            capture: false,
        }))
        .cast()
    }
    fn prepare(&self, mode: CaptureMode, ms: u16) -> Result<CapturePump> {
        unsafe { CapturePump::prepare(self.client(), mode, ms, API) }
    }
    fn record(&self, name: &str, result: Result<()>, seen: Vec<Value>) -> Value {
        let s = self.script.borrow();
        assert!(s.violations.is_empty(), "{}: {:?}", name, s.violations);
        assert_eq!(
            (s.clients, s.captures, s.events, s.priorities, s.lease),
            (0, 0, 0, 0, None),
            "{}: {:?}",
            name,
            s.trace
        );
        assert_eq!(s.threads.len(), 1, "{}", name);
        json!({"name":name,"result":format!("{result:?}"),"trace":s.trace,"format":s.format,"callbacks":seen,"threads":s.threads,"live":[s.clients,s.captures,s.events,s.priorities],"violations":s.violations})
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        CURRENT.with(|x| *x.borrow_mut() = None);
    }
}
#[repr(C)]
struct Object {
    table: *const c_void,
    script: Shared,
    capture: bool,
}
unsafe fn script(raw: Raw) -> Shared {
    unsafe { (&*raw.cast::<Object>()).script.clone() }
}
unsafe extern "system" fn query(_: Raw, _: *const GUID, out: *mut Raw) -> Hr {
    unsafe {
        *out = ptr::null_mut();
    }
    -1
}
unsafe extern "system" fn add(_: Raw) -> u32 {
    1
}
unsafe extern "system" fn release(raw: Raw) -> u32 {
    let object = unsafe { Box::from_raw(raw.cast::<Object>()) };
    let mut s = object.script.borrow_mut();
    if object.capture {
        s.log("capture.release");
        s.captures -= 1;
    } else {
        s.log("client.release");
        s.clients -= 1;
    }
    0
}
unsafe extern "system" fn initialize(
    raw: Raw,
    mode: i32,
    flags: u32,
    duration: i64,
    period: i64,
    f: *const Format,
    session: *const GUID,
) -> Hr {
    let shared = unsafe { script(raw) };
    let mut s = shared.borrow_mut();
    s.log("initialize");
    let f = unsafe { ptr::read_unaligned(f) };
    let (tag, channels, rate, bytes, align, bits, extra) =
        (f.tag, f.channels, f.rate, f.bytes, f.align, f.bits, f.extra);
    s.format = Some(
        json!({"shareMode":mode,"flags":flags,"duration":duration,"period":period,"sessionZero":equal(unsafe{&*session},&GUID::from_u128(0)),"tag":tag,"channels":channels,"rate":rate,"bytes":bytes,"align":align,"bits":bits,"extra":extra}),
    );
    if s.fault == "initialize" { -1 } else { 0 }
}
unsafe extern "system" fn bind(raw: Raw, event: Raw) -> Hr {
    let shared = unsafe { script(raw) };
    let mut s = shared.borrow_mut();
    s.log("bind");
    s.require(event as usize == 1, "wrong event binding");
    if s.fault == "bind" { -1 } else { 0 }
}
unsafe extern "system" fn service(raw: Raw, iid: *const GUID, out: *mut Raw) -> Hr {
    let shared = unsafe { script(raw) };
    let mut s = shared.borrow_mut();
    s.log("service");
    s.require(equal(unsafe { &*iid }, &CAPTURE), "wrong service IID");
    if matches!(s.fault, "service" | "service-null") {
        unsafe {
            *out = ptr::null_mut();
        }
        return if s.fault == "service" { -1 } else { 0 };
    }
    s.captures += 1;
    unsafe {
        *out = Box::into_raw(Box::new(Object {
            table: (&CAPTURE_TABLE as *const Capture).cast(),
            script: shared.clone(),
            capture: true,
        }))
        .cast();
    }
    if s.fault == "service-nonnull-error" {
        -1
    } else {
        0
    }
}
unsafe extern "system" fn start(raw: Raw) -> Hr {
    let shared = unsafe { script(raw) };
    let mut s = shared.borrow_mut();
    s.log("start");
    if matches!(s.fault, "start" | "cleanup-start") {
        -1
    } else {
        0
    }
}
unsafe extern "system" fn stop(raw: Raw) -> Hr {
    let shared = unsafe { script(raw) };
    let mut s = shared.borrow_mut();
    s.log("stop");
    if s.fault.starts_with("cleanup") {
        -1
    } else {
        0
    }
}
unsafe extern "system" fn reset(raw: Raw) -> Hr {
    let shared = unsafe { script(raw) };
    let mut s = shared.borrow_mut();
    s.log("reset");
    if s.fault.starts_with("cleanup") {
        -1
    } else {
        0
    }
}
unsafe extern "system" fn next(raw: Raw, out: *mut u32) -> Hr {
    let shared = unsafe { script(raw) };
    let mut s = shared.borrow_mut();
    s.next_calls += 1;
    let call = s.next_calls;
    s.log(format!("next{call}"));
    if (call == 1 && s.fault == "next1")
        || (call == 2 && matches!(s.fault, "next2" | "next-release"))
    {
        return -1;
    }
    let index = s.packet + usize::from(s.lease.is_some());
    unsafe {
        *out = s.packets.get(index).map(|p| p.frames.max(1)).unwrap_or(0);
    }
    0
}
unsafe extern "system" fn buffer(
    raw: Raw,
    data: *mut *mut u8,
    frames: *mut u32,
    flags: *mut u32,
    position: *mut u64,
    timestamp: *mut u64,
) -> Hr {
    let shared = unsafe { script(raw) };
    let mut s = shared.borrow_mut();
    s.log("get");
    let absent = s.lease.is_none();
    s.require(absent, "overlapping packet lease");
    if s.fault == "buffer" {
        return -1;
    }
    if s.fault == "empty-untouched" {
        s.lease = Some(0);
        return 0x8890001;
    }
    let p = &s.packets[s.packet];
    let pointer = match p.pointer {
        1 => ptr::null_mut(),
        2 => 1usize as *mut u8,
        _ => p.bytes.as_ptr().cast_mut(),
    };
    unsafe {
        *data = pointer;
        *frames = p.frames;
        *flags = p.flags;
        *position = 1000 + s.packet as u64;
        *timestamp = u64::MAX - s.packet as u64;
    }
    let count = p.frames;
    s.lease = Some(count);
    0
}
unsafe extern "system" fn release_buffer(raw: Raw, frames: u32) -> Hr {
    let shared = unsafe { script(raw) };
    let mut s = shared.borrow_mut();
    s.log(format!("release{frames}"));
    let leased = s.lease.take();
    s.require(leased == Some(frames), "wrong/double release");
    s.packet += 1;
    if matches!(
        s.fault,
        "release" | "next-release" | "callback-release" | "panic-release"
    ) {
        -1
    } else {
        0
    }
}
static CLIENT: Client = Client {
    unknown: Unknown {
        query,
        add,
        release,
    },
    initialize,
    unused: [0; 6],
    start,
    stop,
    reset,
    event: bind,
    service,
};
static CAPTURE_TABLE: Capture = Capture {
    unknown: Unknown {
        query,
        add,
        release,
    },
    buffer,
    release: release_buffer,
    next,
};
unsafe extern "system" fn event() -> Raw {
    let shared = current();
    let mut s = shared.borrow_mut();
    s.log("event");
    if s.fault == "event" {
        ptr::null_mut()
    } else {
        s.events += 1;
        1usize as Raw
    }
}
unsafe extern "system" fn wait(event: Raw, ms: u32) -> u32 {
    let shared = current();
    let mut s = shared.borrow_mut();
    s.wait_calls += 1;
    s.log(format!("wait{ms}"));
    s.require(event as usize == 1, "wrong wait handle");
    if matches!(s.fault, "wait" | "cleanup-wait") {
        return u32::MAX;
    }
    if s.stop_at_wait || s.wait_calls > 1 {
        s.stop.store(true, Ordering::Release);
    }
    if s.fault == "timeout" { 258 } else { 0 }
}
unsafe extern "system" fn close(event: Raw) -> i32 {
    let shared = current();
    let mut s = shared.borrow_mut();
    s.log("event.close");
    s.require(event as usize == 1, "wrong close handle");
    s.events -= 1;
    if s.fault == "cleanup" { 0 } else { 1 }
}
unsafe extern "system" fn priority(task: *const u16, index: *mut u32) -> Raw {
    let shared = current();
    let mut s = shared.borrow_mut();
    s.log("priority");
    s.require(
        unsafe { std::slice::from_raw_parts(task, 10) }
            == [80, 114, 111, 32, 65, 117, 100, 105, 111, 0],
        "wrong MMCSS task",
    );
    s.require(unsafe { *index } == 0, "nonzero initial MMCSS index");
    unsafe {
        *index = 7;
    }
    if s.fault == "priority-null" {
        ptr::null_mut()
    } else {
        s.priorities += 1;
        2usize as Raw
    }
}
unsafe extern "system" fn revert(task: Raw) -> i32 {
    let shared = current();
    let mut s = shared.borrow_mut();
    s.log("priority.revert");
    s.require(task as usize == 2, "wrong priority handle");
    s.priorities -= 1;
    if s.fault == "cleanup" { 0 } else { 1 }
}
const API: Api = Api {
    event,
    wait,
    close,
    priority,
    revert,
};
fn persist(name: &str, rows: &[Value]) {
    if let Some(root) = std::env::var_os("IVY_PHONE_WINDOWS_CAPTURE_TRACE") {
        let root = std::path::PathBuf::from(root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join(format!("capture-{name}.json")),
            serde_json::to_vec(rows).unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn initialization_and_native_lifetime_faults() {
    assert_eq!(std::mem::size_of::<Format>(), 18);
    assert_eq!(std::mem::offset_of!(Client, start), 10 * 8);
    assert_eq!(std::mem::offset_of!(Client, service), 14 * 8);
    assert_eq!(std::mem::offset_of!(Capture, next), 5 * 8);
    let mut rows = vec![];
    for (mode, flags, ms) in [
        (CaptureMode::Microphone, 0x88040000u32, 10),
        (CaptureMode::EndpointLoopback, 0x88060000u32, 100),
        (CaptureMode::ProcessLoopback, 0x60000, 40),
    ] {
        let f = Fixture::new("", vec![]);
        let pump = f.prepare(mode, ms).unwrap();
        drop(pump);
        assert_eq!(
            f.script.borrow().format,
            Some(
                json!({"shareMode":0,"flags":flags,"duration":i64::from(ms)*10000,"period":0,"sessionZero":true,"tag":3,"channels":2,"rate":48000,"bytes":384000,"align":8,"bits":32,"extra":0})
            )
        );
        assert_eq!(
            f.script.borrow().trace,
            [
                "initialize",
                "event",
                "bind",
                "client.release",
                "event.close"
            ]
        );
        rows.push(f.record(&format!("mode-{flags:x}"), Ok(()), vec![]));
    }
    for ms in [0, 9, 101, u16::MAX] {
        let f = Fixture::new("", vec![]);
        assert!(matches!(
            f.prepare(CaptureMode::Microphone, ms),
            Err(CaptureError::Configuration)
        ));
        assert_eq!(f.script.borrow().trace, ["client.release"]);
        rows.push(f.record(
            &format!("invalid-ms-{ms}"),
            Err(CaptureError::Configuration),
            vec![],
        ));
    }
    assert!(matches!(
        unsafe { CapturePump::prepare(ptr::null_mut(), CaptureMode::Microphone, 40, API) },
        Err(CaptureError::Client)
    ));
    for (fault, error, expected) in [
        (
            "initialize",
            CaptureError::Initialize,
            vec!["initialize", "client.release"],
        ),
        (
            "event",
            CaptureError::Event,
            vec!["initialize", "event", "client.release"],
        ),
        (
            "bind",
            CaptureError::BindEvent,
            vec![
                "initialize",
                "event",
                "bind",
                "client.release",
                "event.close",
            ],
        ),
    ] {
        let f = Fixture::new(fault, vec![]);
        assert!(matches!(f.prepare(CaptureMode::Microphone,40),Err(e) if e==error));
        assert_eq!(f.script.borrow().trace, expected);
        rows.push(f.record(fault, Err(error), vec![]));
    }
    for (fault, expected) in [
        ("service", Some(CaptureError::Service)),
        ("service-null", Some(CaptureError::Service)),
        ("service-nonnull-error", Some(CaptureError::Service)),
        ("start", Some(CaptureError::Start)),
        ("wait", Some(CaptureError::Wait)),
        ("next1", Some(CaptureError::Next)),
        ("buffer", Some(CaptureError::Buffer)),
        ("priority-null", None),
        ("cleanup", None),
        ("cleanup-start", Some(CaptureError::Start)),
        ("cleanup-wait", Some(CaptureError::Wait)),
        ("timeout", None),
    ] {
        let f = Fixture::new(fault, vec![Data::pcm(&[0.25, 0.75])]);
        let pump = f.prepare(CaptureMode::Microphone, 40).unwrap();
        let stop = f.script.borrow().stop.clone();
        let result = pump.run(&stop, &mut |_, _, _, _| Ok(()));
        assert_eq!(result, expected.map_or(Ok(()), Err));
        let trace = f.script.borrow().trace.clone();
        assert!(trace.windows(2).any(|x| x == ["stop", "reset"]));
        assert!(trace.ends_with(&["client.release".into(), "event.close".into()]));
        if fault == "service-nonnull-error" {
            assert!(
                trace.iter().position(|x| x == "capture.release").unwrap()
                    < trace.iter().position(|x| x == "stop").unwrap()
            );
        }
        if fault == "priority-null" {
            assert!(!trace.contains(&"priority.revert".into()));
        }
        rows.push(f.record(fault, result, vec![]));
    }
    persist("lifetime", &rows);
}

#[test]
fn packet_leases_silence_positions_and_error_precedence() {
    let pcm = Data::pcm(&[0.25, 0.75, -2.0, 1.0]);
    let mut rows = vec![];
    for (name, fault, packet, expected) in [
        ("pcm", "", pcm.clone(), None),
        (
            "silent-null",
            "",
            Data {
                frames: 2,
                flags: 2,
                pointer: 1,
                bytes: vec![],
            },
            None,
        ),
        (
            "silent-poison",
            "",
            Data {
                frames: 96000,
                flags: 7,
                pointer: 2,
                bytes: vec![],
            },
            None,
        ),
        (
            "empty-poison",
            "",
            Data {
                frames: 0,
                flags: 0,
                pointer: 2,
                bytes: vec![],
            },
            None,
        ),
        ("empty-untouched", "empty-untouched", pcm.clone(), None),
        (
            "null",
            "",
            Data {
                frames: 1,
                flags: 0,
                pointer: 1,
                bytes: vec![],
            },
            Some(CaptureError::Pointer),
        ),
        (
            "bound",
            "",
            Data {
                frames: 96001,
                flags: 0,
                pointer: 2,
                bytes: vec![],
            },
            Some(CaptureError::Bound),
        ),
        (
            "release",
            "release",
            pcm.clone(),
            Some(CaptureError::Release),
        ),
        ("next2", "next2", pcm.clone(), Some(CaptureError::Next)),
        (
            "next-release",
            "next-release",
            pcm.clone(),
            Some(CaptureError::Release),
        ),
        (
            "callback",
            "callback",
            pcm.clone(),
            Some(CaptureError::Callback),
        ),
        (
            "callback-release",
            "callback-release",
            pcm.clone(),
            Some(CaptureError::Release),
        ),
        ("panic", "panic", pcm.clone(), Some(CaptureError::Callback)),
        (
            "panic-release",
            "panic-release",
            pcm.clone(),
            Some(CaptureError::Release),
        ),
    ] {
        let f = Fixture::new(fault, vec![packet.clone()]);
        let pump = f.prepare(CaptureMode::Microphone, 40).unwrap();
        let stop = f.script.borrow().stop.clone();
        let mut seen = vec![];
        let result=pump.run(&stop,&mut |bytes,flags,position,timestamp|{
            f.script.borrow_mut().log("callback");
            if flags&2!=0 {assert!(bytes.iter().all(|x|*x==0));}else if name=="pcm"{assert_eq!(bytes,packet.bytes);}
            seen.push(json!({"length":bytes.len(),"flags":flags,"position":position,"timestamp":timestamp,"nonzero":bytes.iter().filter(|x|**x!=0).count()}));
            if matches!(fault,"panic"|"panic-release") {panic!("scripted capture callback");}
            if matches!(fault,"callback"|"callback-release") {Err(CaptureError::Callback)}else{Ok(())}
        });
        assert_eq!(result, expected.map_or(Ok(()), Err), "{name}");
        let trace = f.script.borrow().trace.clone();
        assert_eq!(
            trace.iter().filter(|x| x.starts_with("release")).count(),
            1,
            "{name}"
        );
        if matches!(name, "null" | "bound") {
            assert!(seen.is_empty());
        } else {
            assert_eq!(seen.len(), 1);
        }
        if name == "empty-untouched" {
            assert_eq!(
                seen[0],
                json!({"length":0,"flags":0,"position":0,"timestamp":0,"nonzero":0})
            );
        }
        if name == "pcm" {
            assert_eq!(seen[0]["position"], 1000);
            assert_eq!(seen[0]["timestamp"], json!(u64::MAX));
            assert!(
                trace
                    .windows(3)
                    .any(|x| x == ["callback", "next2", "release2"])
            );
        }
        if matches!(
            fault,
            "callback" | "callback-release" | "panic" | "panic-release"
        ) {
            assert!(!trace.contains(&"next2".into()));
        }
        rows.push(f.record(name, result, seen));
    }
    let f = Fixture::new(
        "",
        vec![
            Data {
                frames: 1,
                flags: 2,
                pointer: 2,
                bytes: vec![],
            },
            Data {
                frames: 3,
                flags: 2,
                pointer: 1,
                bytes: vec![],
            },
            pcm,
        ],
    );
    let mut pump = f.prepare(CaptureMode::EndpointLoopback, 100).unwrap();
    let stop = f.script.borrow().stop.clone();
    let mut seen = vec![];
    // Read-only early drain does not start the client and is not a public pump API.
    pump.capture = Some(
        Reference::acquire(CaptureError::Service, |out| unsafe {
            service(pump.client.raw.as_ptr(), &CAPTURE, out)
        })
        .unwrap(),
    );
    let result = pump.drain(&mut |bytes, flags, pos, time| {
        if flags & 2 != 0 {
            assert!(bytes.iter().all(|x| *x == 0));
        }
        seen.push(json!([bytes.len(), flags, pos, time]));
        Ok(())
    });
    assert_eq!(result, Ok(()));
    assert_eq!(seen.len(), 3);
    assert_eq!(pump.silence.len(), 24);
    drop(pump);
    assert!(!stop.load(Ordering::Acquire));
    rows.push(f.record("multi-packet-silence-reuse", result, seen));
    persist("packets", &rows);
}

#[test]
fn stop_and_capture_buffer_composition() {
    use crate::media::{AudioSettings, windows_audio::WindowsAudioBuffers};
    let mut rows = vec![];
    for stop_at_wait in [false, true] {
        let f = Fixture::new("", vec![Data::pcm(&[0.25, 0.75])]);
        let pump = f.prepare(CaptureMode::Microphone, 40).unwrap();
        let stop = f.script.borrow().stop.clone();
        f.script.borrow_mut().stop_at_wait = stop_at_wait;
        if !stop_at_wait {
            stop.store(true, Ordering::Release);
        }
        let mut calls = 0;
        let result = pump.run(&stop, &mut |_, _, _, _| {
            calls += 1;
            Ok(())
        });
        assert_eq!(result, Ok(()));
        assert_eq!(calls, 0);
        assert_eq!(f.script.borrow().wait_calls, usize::from(stop_at_wait));
        assert!(!f.script.borrow().trace.contains(&"next1".into()));
        rows.push(f.record(
            if stop_at_wait {
                "stop-after-wait"
            } else {
                "already-stopped"
            },
            result,
            vec![],
        ));
    }
    for fail in [false, true] {
        let packets = if fail {
            vec![Data::pcm(&[f32::NAN, 0.0])]
        } else {
            vec![
                Data::pcm(&[0.25, 0.75, -2.0, 1.0]),
                Data {
                    frames: 1,
                    flags: 2,
                    pointer: 2,
                    bytes: vec![],
                },
            ]
        };
        let f = Fixture::new("", packets);
        let pump = f.prepare(CaptureMode::ProcessLoopback, 40).unwrap();
        let stop = f.script.borrow().stop.clone();
        let settings = AudioSettings {
            capture_endpoint: "capture".into(),
            render_endpoint: "render".into(),
            source_mode: "microphone".into(),
            capture_buffer_ms: 40,
            render_latency_ms: 20,
            queue_ms: 80,
            mute: None,
            playback_prebuffer_ms: 0,
            enable_wasapi_low_latency: false,
            loopback_render_endpoint: None,
        };
        let mut buffers = WindowsAudioBuffers::new(&settings, Arc::new(|| true)).unwrap();
        buffers.resume().unwrap();
        let result = pump.run(&stop, &mut |bytes, flags, _, _| {
            buffers.captured(bytes, flags);
            Ok(())
        });
        assert_eq!(result, Ok(()));
        let mut output = [9.0; 4];
        let count = buffers.read_captured(&mut output);
        if fail {
            assert_eq!(buffers.snapshot().state, "failed");
            assert_eq!(count, 0);
            assert_eq!(output, [0.0; 4]);
        } else {
            assert_eq!(count, 3);
            assert_eq!(output, [0.5, -0.5, 0.0, 0.0]);
        }
        let seen = vec![
            json!({"state":buffers.snapshot().state,"count":count,"pcm":output.map(f32::to_bits)}),
        ];
        buffers.close();
        assert_eq!(buffers.snapshot().state, "closed");
        rows.push(f.record(
            if fail {
                "composed-nonfinite"
            } else {
                "composed-pcm-silence"
            },
            result,
            seen,
        ));
    }
    persist("composition", &rows);
}
