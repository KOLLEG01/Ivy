use super::*;
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Condvar, time::Duration};

static SERIAL: Mutex<()> = Mutex::new(());
static FIXTURE: OnceLock<Mutex<Fixture>> = OnceLock::new();
struct Input {
    frames: u32,
    flags: u32,
    values: Vec<f32>,
    null: bool,
}
impl Input {
    fn normal() -> Self {
        Self {
            frames: 3,
            flags: 0,
            values: vec![-0.25, 1.5, 0.0, 0.0, 0.0, 0.0],
            null: false,
        }
    }
}
struct Fixture {
    fault: &'static str,
    trace: Vec<(String, String)>,
    bad: Vec<String>,
    live: usize,
    handles: usize,
    apartments: usize,
    callbacks: usize,
    packets: VecDeque<Input>,
    releases: Vec<u32>,
    major: u32,
    build: u32,
    delayed: bool,
    callback: usize,
    expected_process: u32,
    expected_mode: i32,
    wait_gate: Arc<(Mutex<bool>, Condvar)>,
}
impl Default for Fixture {
    fn default() -> Self {
        Self {
            fault: "",
            trace: vec![],
            bad: vec![],
            live: 0,
            handles: 0,
            apartments: 0,
            callbacks: 0,
            packets: VecDeque::from([Input::normal()]),
            releases: vec![],
            major: 10,
            build: 20348,
            delayed: false,
            callback: 0,
            expected_process: std::process::id(),
            expected_mode: 1,
            wait_gate: Arc::new((Mutex::new(true), Condvar::new())),
        }
    }
}
fn with<T>(f: impl FnOnce(&mut Fixture) -> T) -> T {
    f(&mut FIXTURE
        .get_or_init(|| Mutex::new(Fixture::default()))
        .lock()
        .unwrap())
}
fn record(f: &mut Fixture, name: &str) -> Hr {
    f.trace
        .push((name.into(), format!("{:?}", std::thread::current().id())));
    if f.fault == name { -1 } else { 0 }
}
fn demand(f: &mut Fixture, good: bool, name: &str) {
    if !good {
        f.bad.push(name.into());
    }
}
#[repr(C)]
struct Object {
    table: *const c_void,
    kind: u32,
    callback: usize,
}
const fn unknown() -> Unknown {
    Unknown {
        query,
        add,
        release,
    }
}
static OPERATION: Operation = Operation {
    unknown: unknown(),
    result: activation_result,
};
static ACTIVATED: Unknown = unknown();
static AUDIO: Client = Client {
    unknown: unknown(),
    initialize: initialize_client,
    unused: [0; 6],
    start: start_client,
    stop: stop_client,
    reset: reset_client,
    event: set_event,
    service,
};
static SAMPLES: Capture = Capture {
    unknown: unknown(),
    buffer,
    release: release_packet,
    next,
};
fn object(kind: u32, callback: usize) -> Raw {
    with(|f| f.live += 1);
    let table: *const c_void = match kind {
        0 => (&OPERATION as *const Operation).cast(),
        1 => (&ACTIVATED as *const Unknown).cast(),
        2 => (&AUDIO as *const Client).cast(),
        _ => (&SAMPLES as *const Capture).cast(),
    };
    Box::into_raw(Box::new(Object {
        table,
        kind,
        callback,
    }))
    .cast()
}
unsafe extern "system" fn add(_: Raw) -> u32 {
    2
}
unsafe extern "system" fn release(raw: Raw) -> u32 {
    let value = unsafe { Box::from_raw(raw.cast::<Object>()) };
    with(|f| {
        record(f, &format!("release:{}", value.kind));
        f.live -= 1;
        if value.callback != 0 {
            f.callbacks -= 1;
        }
    });
    if value.callback != 0 {
        unsafe {
            callback_release(value.callback as Raw);
        }
    }
    0
}
unsafe extern "system" fn query(_: Raw, iid: *const GUID, out: *mut Raw) -> Hr {
    let good = !iid.is_null() && equal(unsafe { &*iid }, &CLIENT);
    let hr = with(|f| {
        demand(f, good, "query iid");
        record(f, "query")
    });
    unsafe {
        *out = object(2, 0);
    }
    hr
}
unsafe extern "system" fn initialize_com(reserved: Raw, mode: u32) -> Hr {
    with(|f| {
        demand(f, reserved.is_null() && mode == 0, "MTA");
        let hr = record(f, "initialize-com");
        if hr >= 0 {
            f.apartments += 1;
        }
        hr
    })
}
unsafe extern "system" fn uninitialize() {
    with(|f| {
        record(f, "uninitialize");
        f.apartments -= 1;
    });
}
unsafe extern "system" fn version(v: *mut Version) -> Hr {
    with(|f| {
        let v = unsafe { &mut *v };
        demand(
            f,
            v.size as usize == std::mem::size_of::<Version>(),
            "version size",
        );
        v.major = f.major;
        v.build = f.build;
        record(f, "version")
    })
}
unsafe extern "system" fn activate_interface(
    path: *const u16,
    iid: *const GUID,
    v: *mut Variant,
    handler: Raw,
    out: *mut Raw,
) -> Hr {
    let expected: Vec<u16> = "VAD\\Process_Loopback\0".encode_utf16().collect();
    let v = unsafe { &*v };
    let p = unsafe { &*v.blob.data.cast::<Activation>() };
    let params = v.kind == 65
        && v.reserved == [0; 3]
        && v.blob.size == 12
        && p.kind == 1
        && with(|f| p.process == f.expected_process && p.mode == f.expected_mode);
    let hr = with(|f| {
        demand(
            f,
            unsafe { std::slice::from_raw_parts(path, expected.len()) } == expected,
            "virtual path",
        );
        demand(
            f,
            equal(unsafe { &*iid }, &CLIENT) && params,
            "activation params",
        );
        record(f, "activate")
    });
    if with(|f| f.fault == "activate-null") {
        unsafe {
            *out = ptr::null_mut();
        }
        return 0;
    }
    unsafe {
        callback_add(handler);
    }
    with(|f| f.callbacks += 1);
    unsafe {
        *out = object(0, handler as usize);
    }
    if hr >= 0 {
        // QI and a duplicate notification exercise the actual callback vtable.
        let mut agile = ptr::null_mut();
        let qi = unsafe { callback_query(handler, &AGILE, &mut agile) };
        with(|f| demand(f, qi == 0 && agile == handler, "agile callback"));
        if !agile.is_null() {
            unsafe {
                callback_release(agile);
            }
        }
        if with(|f| {
            f.callback = handler as usize;
            f.delayed
        }) {
            return hr;
        }
        unsafe {
            callback_complete(handler, *out);
            callback_complete(handler, *out);
        }
    }
    hr
}
unsafe extern "system" fn activation_result(_: Raw, status: *mut Hr, out: *mut Raw) -> Hr {
    unsafe {
        *out = object(1, 0);
        *status = if with(|f| f.fault == "result-status") {
            -1
        } else {
            0
        };
    }
    with(|f| record(f, "result-call"))
}
unsafe extern "system" fn initialize_client(
    _: Raw,
    share: i32,
    flags: u32,
    duration: i64,
    period: i64,
    format: *const Format,
    context: *const GUID,
) -> Hr {
    let Format {
        tag,
        channels,
        rate,
        bytes,
        align,
        bits,
        extra,
    } = unsafe { ptr::read_unaligned(format) };
    with(|f| {
        demand(
            f,
            (share, flags, duration, period) == (0, 0x60000, 400000, 0),
            "stream mode",
        );
        demand(
            f,
            (tag, channels, rate, bytes, align, bits, extra) == (3, 2, 48000, 384000, 8, 32, 0),
            "float format",
        );
        demand(
            f,
            !context.is_null() && equal(unsafe { &*context }, &GUID::from_u128(0)),
            "session context",
        );
        record(f, "initialize-client")
    })
}
unsafe extern "system" fn create_event(
    attributes: Raw,
    manual: i32,
    initial: i32,
    name: *const u16,
) -> Raw {
    with(|f| {
        demand(
            f,
            attributes.is_null() && manual == 0 && initial == 0 && name.is_null(),
            "event options",
        );
        if record(f, "create-event") < 0 {
            return ptr::null_mut();
        }
        f.handles += 1;
        123usize as Raw
    })
}
unsafe extern "system" fn close_event(raw: Raw) -> i32 {
    with(|f| {
        demand(f, raw as usize == 123, "close event");
        record(f, "close-event");
        f.handles -= 1;
    });
    1
}
unsafe extern "system" fn set_event(_: Raw, raw: Raw) -> Hr {
    with(|f| {
        demand(f, raw as usize == 123, "set event");
        record(f, "set-event")
    })
}
unsafe extern "system" fn service(_: Raw, iid: *const GUID, out: *mut Raw) -> Hr {
    unsafe {
        *out = object(3, 0);
    }
    with(|f| {
        demand(f, equal(unsafe { &*iid }, &CAPTURE), "capture iid");
        record(f, "service")
    })
}
unsafe extern "system" fn start_client(_: Raw) -> Hr {
    with(|f| record(f, "start"))
}
unsafe extern "system" fn stop_client(_: Raw) -> Hr {
    with(|f| record(f, "stop"))
}
unsafe extern "system" fn reset_client(_: Raw) -> Hr {
    with(|f| record(f, "reset"))
}
unsafe extern "system" fn wait_event(_: Raw, ms: u32) -> u32 {
    let (failed, gate) = with(|f| {
        demand(f, ms == 120, "event timeout");
        (record(f, "wait") < 0, f.wait_gate.clone())
    });
    let mut ready = gate.0.lock().unwrap();
    while !*ready {
        ready = gate.1.wait(ready).unwrap();
    }
    std::thread::sleep(Duration::from_millis(1));
    if failed { u32::MAX } else { 258 }
}
unsafe extern "system" fn next(_: Raw, out: *mut u32) -> Hr {
    with(|f| {
        unsafe {
            *out = f.packets.front().map_or(0, |p| p.frames.max(1));
        }
        record(f, "next")
    })
}
unsafe extern "system" fn buffer(
    _: Raw,
    data: *mut *mut u8,
    frames: *mut u32,
    flags: *mut u32,
    position: *mut u64,
    timestamp: *mut u64,
) -> Hr {
    with(|f| {
        demand(
            f,
            position.is_null() && timestamp.is_null(),
            "unused positions",
        );
        let p = f.packets.front_mut().unwrap();
        unsafe {
            *frames = p.frames;
            *flags = p.flags;
            *data = if p.null {
                ptr::null_mut()
            } else if p.values.is_empty() {
                1usize as *mut u8
            } else {
                p.values.as_mut_ptr().cast()
            };
        }
        record(f, "buffer")
    })
}
unsafe extern "system" fn release_packet(_: Raw, frames: u32) -> Hr {
    with(|f| {
        let p = f.packets.pop_front().unwrap();
        demand(f, p.frames == frames, "released exact frame count");
        f.releases.push(frames);
        record(f, "release-packet")
    })
}
fn api() -> Api {
    Api {
        initialize: initialize_com,
        uninitialize,
        version,
        activate: activate_interface,
        event: create_event,
        wait: wait_event,
        close: close_event,
    }
}
fn request() -> ProbeCapture {
    ProbeCapture {
        sample_rate: 48000,
        channels: 2,
        buffer_ms: 40,
        excluded_process_id: std::process::id(),
    }
}
fn factory() -> WindowsProbeFactory {
    WindowsProbeFactory { api: api() }
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
fn save(name: &str, value: &Value) {
    if let Some(dir) = std::env::var_os("IVY_PHONE_LOOPBACK_WINDOWS_TRACE") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            std::path::Path::new(&dir).join(name),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }
}
fn cleaned() -> Value {
    with(|f| {
        assert_eq!((f.live, f.handles, f.apartments, f.callbacks), (0, 0, 0, 0));
        assert!(f.bad.is_empty(), "{:?}", f.bad);
        let threads: std::collections::HashSet<_> = f
            .trace
            .iter()
            .filter(|(n, _)| n != "version")
            .map(|(_, t)| t)
            .collect();
        assert_eq!(
            threads.len(),
            1,
            "COM/handle calls must stay on their worker"
        );
        json!({"live":f.live,"handles":f.handles,"apartments":f.apartments,"callbacks":f.callbacks,"trace":f.trace,"releases":f.releases,"oneNativeThread":true})
    })
}
#[test]
fn selected_process_tree_reuses_the_original_activation_owner() {
    let _serial = SERIAL.lock().unwrap();
    let mut rows = vec![];
    for (process, mode) in [(71, 0), (72, 1)] {
        with(|f| {
            *f = Fixture {
                expected_process: process,
                expected_mode: mode,
                ..Fixture::default()
            }
        });
        let apartment = Apartment::new(api()).unwrap();
        let client = activate_target(api(), process, mode).unwrap();
        drop(client);
        drop(apartment);
        rows.push(json!({"process":process,"mode":mode,"resources":cleaned()}));
    }
    for (process, mode) in [(0, 0), (71, -1), (71, 2)] {
        with(|f| *f = Fixture::default());
        assert!(matches!(
            activate_target(api(), process, mode),
            Err(ProbeError::Capture)
        ));
        with(|f| {
            assert!(f.trace.is_empty());
            assert_eq!((f.live, f.handles, f.apartments, f.callbacks), (0, 0, 0, 0));
        });
        rows.push(json!({"process":process,"mode":mode,"refusedBeforeNative":true}));
    }
    save("loopback-selected-targets.json", &json!(rows));
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn activation_and_capture_faults_release_owned_native_resources() {
    let _serial = SERIAL.lock().unwrap();
    let mut rows = vec![];
    for fault in [
        "",
        "initialize-com",
        "activate",
        "activate-null",
        "result-call",
        "result-status",
        "query",
        "initialize-client",
        "create-event",
        "set-event",
        "service",
        "start",
        "wait",
        "next",
        "buffer",
        "release-packet",
        "stop",
        "reset",
    ] {
        with(|f| {
            *f = Fixture {
                fault,
                ..Fixture::default()
            }
        });
        let events = ProbeEvents::new();
        let mut opened = false;
        let mut start_error = false;
        let mut capture_error = false;
        match factory().open(request()).await {
            Err(e) => assert_eq!(e, ProbeError::Capture),
            Ok(mut recorder) => {
                opened = true;
                recorder.subscribe(events.clone()).unwrap();
                start_error = recorder.start().is_err();
                if !start_error {
                    until(|| events.finished() || with(|f| !f.releases.is_empty())).await;
                    recorder.stop().unwrap();
                    until(|| events.finished()).await;
                    capture_error = events.wait().await.is_err();
                }
                recorder.close().await.unwrap();
                recorder.close().await.unwrap();
            }
        }
        assert_eq!(
            opened,
            !matches!(
                fault,
                "initialize-com"
                    | "activate"
                    | "activate-null"
                    | "result-call"
                    | "result-status"
                    | "query"
            )
        );
        if opened {
            assert_eq!(
                start_error,
                matches!(fault, "initialize-client" | "create-event" | "set-event")
            );
        }
        if opened && !start_error {
            assert_eq!(
                capture_error,
                matches!(
                    fault,
                    "service" | "start" | "wait" | "next" | "buffer" | "release-packet"
                )
            );
        }
        rows.push(json!({"fault":fault,"opened":opened,"startError":start_error,"captureError":capture_error,"resources":cleaned()}));
    }
    save("loopback-windows-faults.json", &json!(rows));
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn process_identity_format_packet_bounds_and_silent_data_are_exact() {
    let _serial = SERIAL.lock().unwrap();
    let mut rows = vec![];
    assert_eq!(std::mem::size_of::<Format>(), 18);
    assert_eq!(std::mem::size_of::<Activation>(), 12);
    assert_eq!(std::mem::size_of::<Variant>(), 24);
    assert_eq!(
        std::mem::offset_of!(Client, start),
        10 * std::mem::size_of::<usize>()
    );
    assert_eq!(
        std::mem::offset_of!(Client, service),
        14 * std::mem::size_of::<usize>()
    );
    for field in 0..4 {
        with(|f| *f = Fixture::default());
        let mut c = request();
        match field {
            0 => c.excluded_process_id += 1,
            1 => c.sample_rate = 44100,
            2 => c.channels = 1,
            _ => c.buffer_ms = 100,
        }
        assert!(factory().open(c).await.is_err());
        with(|f| assert!(f.trace.is_empty()));
    }
    for (major, build, fault, supported) in [
        (9, 99999, "", false),
        (10, 20347, "", false),
        (10, 20348, "", true),
        (11, 0, "", true),
        (10, 20348, "version", false),
    ] {
        with(|f| {
            *f = Fixture {
                major,
                build,
                fault,
                ..Fixture::default()
            }
        });
        assert_eq!(factory().supported(), supported);
        with(|f| assert_eq!(f.trace.len(), 1));
    }
    for name in [
        "normal",
        "metadata-flags",
        "silent-null",
        "silent-invalid-pointer",
        "empty",
        "null",
        "invalid-pcm",
        "packet-over",
        "total-over",
    ] {
        with(|f| {
            *f = Fixture::default();
            f.packets = match name {
                "metadata-flags" => VecDeque::from([Input {
                    flags: 5,
                    ..Input::normal()
                }]),
                "silent-null" => VecDeque::from([Input {
                    frames: 3,
                    flags: 2,
                    values: vec![],
                    null: true,
                }]),
                "silent-invalid-pointer" => VecDeque::from([Input {
                    frames: 3,
                    flags: 7,
                    values: vec![],
                    null: false,
                }]),
                "empty" => VecDeque::from([Input {
                    frames: 0,
                    flags: 0,
                    values: vec![],
                    null: true,
                }]),
                "null" => VecDeque::from([Input {
                    null: true,
                    ..Input::normal()
                }]),
                "invalid-pcm" => VecDeque::from([Input {
                    frames: 2,
                    flags: 0,
                    values: vec![0.25, f32::NAN, 0.0, 0.0],
                    null: false,
                }]),
                "packet-over" => VecDeque::from([Input {
                    frames: 96001,
                    flags: 0,
                    values: vec![],
                    null: false,
                }]),
                "total-over" => (0..6)
                    .map(|_| Input {
                        frames: 96000,
                        flags: 2,
                        values: vec![],
                        null: true,
                    })
                    .collect(),
                _ => VecDeque::from([Input::normal()]),
            };
        });
        let events = ProbeEvents::new();
        let mut recorder = factory().open(request()).await.unwrap();
        recorder.subscribe(events.clone()).unwrap();
        recorder.start().unwrap();
        until(|| with(|f| f.packets.is_empty()) || events.finished()).await;
        recorder.stop().unwrap();
        recorder.close().await.unwrap();
        let level = events.0.meter.result("completed");
        let error = events.wait().await.err();
        let (samples, peak, expected) = match name {
            "silent-null" | "silent-invalid-pointer" => (6, 0.0, None),
            "empty" => (0, 0.0, None),
            "null" => (0, 0.0, Some(ProbeError::Capture)),
            "invalid-pcm" => (0, 0.25, Some(ProbeError::InvalidPcm)),
            "packet-over" => (0, 0.0, Some(ProbeError::SampleBound)),
            "total-over" => (960000, 0.0, Some(ProbeError::SampleBound)),
            _ => (6, 1.0, None),
        };
        assert_eq!(
            (level.samples, level.peak, error),
            (samples, peak, expected)
        );
        rows.push(json!({"name":name,"samples":samples,"peak":peak,"error":format!("{error:?}"),"resources":cleaned()}));
    }
    save(
        "loopback-windows-packets.json",
        &json!({"packetCases":rows,"invalidAuthorityRequests":4,"versionCases":5,"structureSizes":[18,12,24],"audioClientSlots":[10,14]}),
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn late_activation_and_unpolled_close_keep_their_native_worker() {
    let _serial = SERIAL.lock().unwrap();
    with(|f| {
        *f = Fixture {
            delayed: true,
            ..Fixture::default()
        }
    });
    let opening = factory().open(request());
    drop(opening);
    until(|| with(|f| f.callback != 0)).await;
    assert_eq!(with(|f| (f.live, f.apartments, f.callbacks)), (1, 1, 1));
    let callback = with(|f| f.callback);
    std::thread::spawn(move || unsafe {
        callback_complete(callback as Raw, ptr::null_mut());
    })
    .join()
    .unwrap();
    until(|| with(|f| f.apartments == 0)).await;
    let late = cleaned();
    with(|f| {
        assert!(
            !f.trace
                .iter()
                .any(|(n, _)| n == "initialize-client" || n == "start")
        )
    });
    with(|f| {
        *f = Fixture::default();
        *f.wait_gate.0.lock().unwrap() = false;
    });
    let events = ProbeEvents::new();
    let mut recorder = factory().open(request()).await.unwrap();
    recorder.subscribe(events).unwrap();
    recorder.start().unwrap();
    until(|| with(|f| f.trace.iter().any(|(n, _)| n == "wait"))).await;
    let closed = recorder.close();
    drop(closed);
    drop(recorder);
    assert!(with(|f| f.live > 0 && f.handles == 1 && f.apartments == 1));
    with(|f| {
        *f.wait_gate.0.lock().unwrap() = true;
        f.wait_gate.1.notify_all();
    });
    until(|| with(|f| f.apartments == 0)).await;
    let close = cleaned();
    save(
        "loopback-windows-retention.json",
        &json!({"lateActivation":late,"unpolledClose":close,"allObserversDropped":true,"noStartAfterAbandonedOpen":true,"workerAndHandlesHeldUntilWaitReturns":true}),
    );
}
