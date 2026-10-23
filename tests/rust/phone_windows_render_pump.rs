use super::*;
use serde_json::{Value, json};
use std::{cell::RefCell, collections::VecDeque, sync::Arc};

#[derive(Clone, Copy)]
enum Read {
    Full,
    Count(usize),
    Zero,
    Error,
    Panic,
    Oversize,
}
struct Script {
    fault: &'static str,
    trace: Vec<String>,
    bad: Vec<String>,
    threads: Vec<String>,
    clients: i32,
    renders: i32,
    events: i32,
    priorities: i32,
    frames: u32,
    align: u16,
    buffer: Vec<u8>,
    lease: Option<u32>,
    abandoned: u32,
    gets: usize,
    releases: usize,
    reads: usize,
    waits: usize,
    read_plan: VecDeque<Read>,
    waits_plan: VecDeque<(u32, bool)>,
    padding: VecDeque<u32>,
    blocks: Vec<Value>,
    stop: Arc<AtomicBool>,
    revoke: Option<Arc<AtomicBool>>,
}
type Shared = Rc<RefCell<Script>>;
thread_local! { static CURRENT:RefCell<Option<Shared>>=const{RefCell::new(None)}; }
fn current() -> Shared {
    CURRENT.with(|x| x.borrow().as_ref().unwrap().clone())
}
impl Script {
    fn log(&mut self, text: impl Into<String>) {
        self.trace.push(text.into());
        let id = format!("{:?}", std::thread::current().id());
        if !self.threads.contains(&id) {
            self.threads.push(id);
        }
    }
    fn require(&mut self, test: bool, text: &str) {
        if !test {
            self.bad.push(text.into());
        }
    }
}
struct Fixture {
    shared: Shared,
}
impl Fixture {
    fn new(fault: &'static str, reads: &[Read], waits: &[(u32, bool)], padding: &[u32]) -> Self {
        let shared = Rc::new(RefCell::new(Script {
            fault,
            trace: vec![],
            bad: vec![],
            threads: vec![],
            clients: 0,
            renders: 0,
            events: 0,
            priorities: 0,
            frames: 40,
            align: 8,
            buffer: vec![],
            lease: None,
            abandoned: 0,
            gets: 0,
            releases: 0,
            reads: 0,
            waits: 0,
            read_plan: reads.iter().copied().collect(),
            waits_plan: waits.iter().copied().collect(),
            padding: padding.iter().copied().collect(),
            blocks: vec![],
            stop: Arc::new(AtomicBool::new(false)),
            revoke: None,
        }));
        CURRENT.with(|x| {
            assert!(x.borrow().is_none());
            *x.borrow_mut() = Some(shared.clone());
        });
        Self { shared }
    }
    fn interface(&self, render: bool) -> Raw {
        let mut s = self.shared.borrow_mut();
        if render {
            s.renders += 1;
        } else {
            s.clients += 1;
        }
        Box::into_raw(Box::new(Object {
            table: if render {
                (&RENDER as *const Render).cast()
            } else {
                (&CLIENT as *const Client).cast()
            },
            shared: self.shared.clone(),
            render,
        }))
        .cast()
    }
    fn prepare(&self, align: u16, latency: u32) -> Result<RenderPump> {
        self.shared.borrow_mut().align = align;
        let fault = self.shared.borrow().fault;
        let client = if fault == "null-client" {
            ptr::null_mut()
        } else {
            self.interface(false)
        };
        let render = if fault == "null-render" {
            ptr::null_mut()
        } else {
            self.interface(true)
        };
        let frame = if fault == "null-frame" {
            ptr::null_mut()
        } else {
            self.shared.borrow_mut().events += 1;
            1usize as Raw
        };
        let stop = if fault == "null-stop" {
            ptr::null_mut()
        } else {
            self.shared.borrow_mut().events += 1;
            2usize as Raw
        };
        unsafe { RenderPump::prepare(client, render, frame, stop, align, latency, API) }
    }
    fn run(&self, align: u16, latency: u32, sink: &mut RenderSink<'_>) -> (Result<()>, Value) {
        let result = match self.prepare(align, latency) {
            Err(e) => Err(e),
            Ok(mut pump) => {
                let stop = self.shared.borrow().stop.clone();
                let result = pump.run(&stop, sink);
                {
                    let mut s = self.shared.borrow_mut();
                    s.log("owner.retained");
                    assert_eq!((s.clients, s.renders, s.events, s.priorities), (1, 1, 2, 0));
                }
                drop(pump);
                result
            }
        };
        let s = self.shared.borrow();
        assert!(s.bad.is_empty(), "{:?}: {:?}", s.trace, s.bad);
        assert_eq!(
            (s.clients, s.renders, s.events, s.priorities, s.lease),
            (0, 0, 0, 0, None)
        );
        assert_eq!(s.threads.len(), 1);
        (
            result,
            json!({"fault":s.fault,"result":format!("{result:?}"),"trace":s.trace,"blocks":s.blocks,"gets":s.gets,"reads":s.reads,"releases":s.releases,"waits":s.waits,"abandonedLeases":s.abandoned,"live":[s.clients,s.renders,s.events,s.priorities],"threads":s.threads,"bad":s.bad}),
        )
    }
    fn run_script(&self, align: u16, latency: u32) -> (Result<()>, Value) {
        let shared = self.shared.clone();
        self.run(align, latency, &mut move |bytes| {
            let mut s = shared.borrow_mut();
            s.reads += 1;
            let read = s.reads;
            s.log(format!("source{read}:{}", bytes.len()));
            let planned = s.read_plan.pop_front().unwrap_or(Read::Full);
            drop(s);
            bytes.fill(0x40 + read as u8);
            match planned {
                Read::Full => Ok(bytes.len()),
                Read::Count(n) => Ok(n),
                Read::Zero => Ok(0),
                Read::Error => Err(RenderError::Callback),
                Read::Panic => panic!("Synthetic source panic"),
                Read::Oversize => Ok(bytes.len() + 1),
            }
        })
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
    shared: Shared,
    render: bool,
}
unsafe fn script(raw: Raw) -> Shared {
    unsafe { (&*raw.cast::<Object>()).shared.clone() }
}
unsafe extern "system" fn release_interface(raw: Raw) -> u32 {
    let object = unsafe { Box::from_raw(raw.cast::<Object>()) };
    let mut s = object.shared.borrow_mut();
    if object.render {
        s.log("render.release");
        s.renders -= 1;
        if s.lease.take().is_some() {
            s.abandoned += 1;
        }
    } else {
        s.log("client.release");
        s.clients -= 1;
    }
    0
}
unsafe extern "system" fn size(raw: Raw, out: *mut u32) -> Hr {
    let shared = unsafe { script(raw) };
    let mut s = shared.borrow_mut();
    s.log("size");
    unsafe {
        *out = s.frames;
    }
    if s.fault == "size" { -1 } else { 0 }
}
unsafe extern "system" fn padding(raw: Raw, out: *mut u32) -> Hr {
    let shared = unsafe { script(raw) };
    let mut s = shared.borrow_mut();
    let value = s.padding.pop_front().unwrap_or(0);
    s.log(format!("padding:{value}"));
    unsafe {
        *out = value;
    }
    if s.fault == "padding" { -1 } else { 0 }
}
unsafe extern "system" fn start(raw: Raw) -> Hr {
    let shared = unsafe { script(raw) };
    let mut s = shared.borrow_mut();
    s.log("start");
    if s.fault == "start" { -1 } else { 0 }
}
unsafe extern "system" fn stop(raw: Raw) -> Hr {
    let shared = unsafe { script(raw) };
    let mut s = shared.borrow_mut();
    s.log("stop");
    if s.fault == "stop" { -1 } else { 0 }
}
unsafe extern "system" fn reset(raw: Raw) -> Hr {
    let shared = unsafe { script(raw) };
    let mut s = shared.borrow_mut();
    s.log("reset");
    if s.fault == "reset" { -1 } else { 0 }
}
unsafe extern "system" fn buffer(raw: Raw, frames: u32, out: *mut *mut u8) -> Hr {
    let shared = unsafe { script(raw) };
    let mut s = shared.borrow_mut();
    s.gets += 1;
    let get = s.gets;
    s.log(format!("buffer{get}:{frames}"));
    let leased = s.lease.is_some();
    s.require(!leased, "overlapping lease");
    if s.fault == "buffer" || s.fault == "buffer-nonnull-error" || s.fault == "buffer2" && get == 2
    {
        unsafe {
            *out = if s.fault == "buffer-nonnull-error" {
                1usize as *mut u8
            } else {
                ptr::null_mut()
            };
        }
        return -1;
    }
    s.lease = Some(frames);
    if s.fault == "null-buffer" || frames == 0 {
        unsafe {
            *out = ptr::null_mut();
        }
    } else if frames > 1000 {
        unsafe {
            *out = 1usize as *mut u8;
        }
    } else {
        let length = frames as usize * usize::from(s.align);
        s.buffer.resize(length, 0xcc);
        s.buffer.fill(0xcc);
        unsafe {
            *out = s.buffer.as_mut_ptr();
        }
    }
    0
}
unsafe extern "system" fn release_buffer(raw: Raw, frames: u32, flags: u32) -> Hr {
    let shared = unsafe { script(raw) };
    let mut s = shared.borrow_mut();
    s.releases += 1;
    let call = s.releases;
    s.log(format!("release{call}:{frames}:{flags}"));
    let leased = s.lease;
    s.require(
        leased.is_some_and(|n| frames <= n),
        "release without extent",
    );
    let hex: String = s.buffer.iter().map(|b| format!("{b:02x}")).collect();
    s.blocks
        .push(json!({"frames":frames,"flags":flags,"hex":hex}));
    if s.fault == "release-both"
        || s.fault == "release1" && call == 1
        || s.fault == "release-dispose"
    {
        return -1;
    }
    s.lease = None;
    0
}
unsafe extern "system" fn wait_api(frame: Raw, stop: Raw, ms: u32) -> u32 {
    let shared = current();
    let mut s = shared.borrow_mut();
    s.waits += 1;
    let index = s.waits;
    s.log(format!("wait{index}:{ms}"));
    s.require(
        frame as usize == 1 && stop as usize == 2,
        "wrong wait events",
    );
    if index == 1 {
        if let Some(p) = &s.revoke {
            p.store(false, Ordering::SeqCst);
        }
    }
    let (result, stopped) = s.waits_plan.pop_front().unwrap_or_else(|| {
        s.bad.push("unexpected wait".into());
        (1, true)
    });
    if stopped {
        s.stop.store(true, Ordering::Release);
    }
    result
}
unsafe extern "system" fn close_api(raw: Raw) -> i32 {
    let shared = current();
    let mut s = shared.borrow_mut();
    s.log(format!("event.close:{}", raw as usize));
    s.events -= 1;
    if s.fault == "close" { 0 } else { 1 }
}
unsafe extern "system" fn priority(task: *const u16, index: *mut u32) -> Raw {
    let shared = current();
    let mut s = shared.borrow_mut();
    s.log("priority");
    let expected = [80, 114, 111, 32, 65, 117, 100, 105, 111, 0];
    s.require(
        unsafe { std::slice::from_raw_parts(task, 10) } == expected,
        "wrong priority task",
    );
    unsafe {
        *index = 7;
    }
    if s.fault == "priority-null" {
        ptr::null_mut()
    } else {
        s.priorities += 1;
        3usize as Raw
    }
}
unsafe extern "system" fn revert(raw: Raw) -> i32 {
    let shared = current();
    let mut s = shared.borrow_mut();
    s.log("revert");
    s.require(raw as usize == 3, "wrong priority handle");
    s.priorities -= 1;
    if s.fault == "revert" { 0 } else { 1 }
}
static CLIENT: Client = Client {
    unknown: Unknown {
        unused: [0; 2],
        release: release_interface,
    },
    initialize: 0,
    size,
    latency: 0,
    padding,
    unused: [0; 3],
    start,
    stop,
    reset,
    event: 0,
    service: 0,
};
static RENDER: Render = Render {
    unknown: Unknown {
        unused: [0; 2],
        release: release_interface,
    },
    buffer,
    release: release_buffer,
};
const API: Api = Api {
    wait: wait_api,
    close: close_api,
    priority,
    revert,
};
fn save(name: &str, rows: &[Value]) {
    if let Some(dir) = std::env::var_os("IVY_PHONE_RENDER_PUMP_TRACE") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            std::path::Path::new(&dir).join(format!("{name}.json")),
            serde_json::to_vec_pretty(rows).unwrap(),
        )
        .unwrap();
    }
}
fn has(row: &Value, entry: &str) -> bool {
    row["trace"].as_array().unwrap().iter().any(|s| s == entry)
}

#[test]
fn native_lifetime_faults_retain_interfaces_and_preserve_cleanup_order() {
    let mut rows = vec![];
    for fault in [
        "null-client",
        "null-render",
        "null-frame",
        "null-stop",
        "align",
        "latency",
        "size",
        "buffer",
        "buffer-nonnull-error",
        "null-buffer",
        "start",
        "wait",
        "abandoned",
        "padding",
        "stop",
        "reset",
        "priority-null",
        "revert",
        "close",
        "frame-bound",
        "byte-bound",
    ] {
        let waits = if fault == "wait" {
            vec![(u32::MAX, false)]
        } else if fault == "abandoned" {
            vec![(128, false)]
        } else if fault == "padding" {
            vec![(0, false)]
        } else {
            vec![(1, true)]
        };
        let f = Fixture::new(fault, &[Read::Full], &waits, &[0]);
        if fault == "frame-bound" {
            f.shared.borrow_mut().frames = i32::MAX as u32 + 1;
        }
        if fault == "byte-bound" {
            f.shared.borrow_mut().frames = 536870912;
        }
        let (result, row) = f.run_script(
            if fault == "align" { 0 } else { 8 },
            if fault == "latency" { u32::MAX } else { 10 },
        );
        let expected = match fault {
            "null-client" | "null-render" | "null-frame" | "null-stop" | "align" | "latency" => {
                Err(RenderError::Configuration)
            }
            "size" => Err(RenderError::BufferSize),
            "buffer" | "buffer-nonnull-error" => Err(RenderError::Buffer),
            "null-buffer" => Err(RenderError::Pointer),
            "start" => Err(RenderError::Start),
            "wait" | "abandoned" => Err(RenderError::Wait),
            "padding" => Err(RenderError::Padding),
            "stop" => Err(RenderError::Stop),
            "reset" => Err(RenderError::Reset),
            "frame-bound" | "byte-bound" => Err(RenderError::Bound),
            _ => Ok(()),
        };
        assert_eq!(result, expected, "{fault}");
        if !matches!(fault, "reset" | "priority-null" | "revert" | "close") {
            assert!(!has(&row, "reset"), "{fault}");
        }
        if has(&row, "owner.retained") {
            let trace = row["trace"].as_array().unwrap();
            let pos = |s: &str| trace.iter().position(|x| x == s).unwrap();
            assert!(pos("render.release") < pos("client.release"));
            assert!(pos("client.release") < pos("event.close:2"));
            assert!(pos("event.close:2") < pos("event.close:1"));
        }
        rows.push(row);
    }
    assert_eq!(rows.len(), 21);
    save("render-pump-lifetime", &rows);
}

#[test]
fn packet_leases_initial_fill_wait_threshold_and_end_drain_follow_original() {
    let mut rows = vec![];
    for (name, reads, waits, pads, expected) in [
        ("initial-zero", vec![Read::Zero], vec![], vec![], Ok(())),
        ("zero-frames", vec![Read::Full], vec![], vec![], Ok(())),
        ("already-stopped", vec![Read::Full], vec![], vec![], Ok(())),
        (
            "short-initial",
            vec![Read::Count(9)],
            vec![(1, true)],
            vec![],
            Ok(()),
        ),
        (
            "source-error",
            vec![Read::Error],
            vec![],
            vec![],
            Err(RenderError::Callback),
        ),
        (
            "source-panic",
            vec![Read::Panic],
            vec![],
            vec![],
            Err(RenderError::Callback),
        ),
        (
            "source-oversize",
            vec![Read::Oversize],
            vec![],
            vec![],
            Err(RenderError::Bound),
        ),
        (
            "release1",
            vec![Read::Count(9)],
            vec![],
            vec![],
            Err(RenderError::Release),
        ),
        (
            "release-both",
            vec![Read::Zero],
            vec![],
            vec![],
            Err(RenderError::Release),
        ),
        (
            "release-dispose",
            vec![Read::Error],
            vec![],
            vec![],
            Err(RenderError::Release),
        ),
        (
            "buffer2",
            vec![Read::Full],
            vec![(0, false)],
            vec![0],
            Err(RenderError::Buffer),
        ),
        (
            "threshold",
            vec![Read::Full, Read::Full],
            vec![(258, false), (0, false), (1, true)],
            vec![30, 29],
            Ok(()),
        ),
        (
            "padding-over-total",
            vec![Read::Full],
            vec![(0, false), (1, true)],
            vec![41],
            Ok(()),
        ),
        (
            "end-drain",
            vec![Read::Full, Read::Zero],
            vec![(0, false), (258, false), (0, false)],
            vec![0, 5, 0],
            Ok(()),
        ),
        (
            "short-refill",
            vec![Read::Full, Read::Count(3)],
            vec![(0, false), (1, true)],
            vec![0],
            Ok(()),
        ),
        (
            "stop-event-without-stop-state",
            vec![Read::Full, Read::Full],
            vec![(1, false), (1, true)],
            vec![0],
            Ok(()),
        ),
        (
            "zero-latency",
            vec![Read::Full],
            vec![(1, true)],
            vec![],
            Ok(()),
        ),
    ] {
        let f = Fixture::new(name, &reads, &waits, &pads);
        if name == "zero-frames" {
            f.shared.borrow_mut().frames = 0;
        }
        if name == "already-stopped" {
            f.shared.borrow().stop.store(true, Ordering::Release);
        }
        let (result, row) = f.run_script(8, if name == "zero-latency" { 0 } else { 10 });
        assert_eq!(result, expected, "{name}");
        match name {
            "initial-zero" | "zero-frames" => {
                assert!(!has(&row, "start"));
                assert!(!has(&row, "stop"));
                assert!(has(&row, "release1:0:2"));
            }
            "already-stopped" => {
                assert!(has(&row, "start") && has(&row, "stop") && has(&row, "reset"));
                assert_eq!(row["waits"], 0);
            }
            "short-initial" => assert!(has(&row, "release1:1:0")),
            "source-error" | "source-panic" | "source-oversize" => {
                assert!(has(&row, "release1:40:0"))
            }
            "release1" => {
                assert!(has(&row, "release1:1:0") && has(&row, "release2:40:0"));
                assert_eq!(row["releases"], 2);
            }
            "release-both" => {
                assert!(has(&row, "release1:0:2") && has(&row, "release2:40:0"));
                assert_eq!(row["releases"], 2);
            }
            "release-dispose" => assert_eq!(row["releases"], 1),
            "threshold" => {
                assert_eq!(row["gets"], 2);
                assert!(has(&row, "buffer2:11"));
            }
            "padding-over-total" => assert_eq!(row["gets"], 1),
            "end-drain" => {
                assert_eq!(row["gets"], 2);
                assert_eq!(row["waits"], 3);
                assert!(has(&row, "release2:0:2") && has(&row, "reset"));
            }
            "short-refill" => assert!(has(&row, "release2:0:0")),
            "stop-event-without-stop-state" => assert_eq!(row["gets"], 2),
            "zero-latency" => assert!(has(&row, "wait1:0")),
            _ => {}
        }
        rows.push(row);
    }
    for align in [1, 2, 4, 6] {
        let f = Fixture::new("alignment", &[Read::Count(3)], &[(1, true)], &[]);
        let (result, mut row) = f.run_script(align, 10);
        assert_eq!(result, Ok(()));
        assert!(has(&row, &format!("source1:{}", 40 * usize::from(align))));
        assert!(has(&row, &format!("release1:{}:0", 3 / usize::from(align))));
        row["align"] = json!(align);
        rows.push(row);
    }
    assert_eq!(rows.len(), 21);
    save("render-pump-packets", &rows);
}

#[test]
fn render_provider_composition_keeps_pcm_authority_under_the_common_lock() {
    use crate::media::{
        AudioSettings,
        windows_audio::{WindowsAudioBuffers, render::RenderOutput},
    };
    use std::sync::Mutex;
    let mut rows = vec![];
    for rate in [24000, 48000, 96000] {
        let f = Fixture::new("composition", &[], &[(0, false), (1, true)], &[0]);
        let permitted = Arc::new(AtomicBool::new(true));
        f.shared.borrow_mut().revoke = Some(permitted.clone());
        let p = permitted.clone();
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
        let mut buffers =
            WindowsAudioBuffers::new(&settings, Arc::new(move || p.load(Ordering::SeqCst)))
                .unwrap();
        buffers.resume().unwrap();
        buffers.write_received(&[0.5; 200]);
        buffers.captured(&[0; 16], 0);
        let owner = Mutex::new((buffers, RenderOutput::new(rate, 2).unwrap()));
        let (result, row) = f.run(8, 10, &mut |bytes| {
            let mut state = owner.lock().unwrap();
            let (buffers, renderer) = &mut *state;
            let mut output = vec![0.75; bytes.len() / 4];
            renderer.read(buffers, &mut output);
            for (value, word) in output.iter().zip(bytes.chunks_exact_mut(4)) {
                word.copy_from_slice(&value.to_le_bytes());
            }
            Ok(bytes.len())
        });
        assert_eq!(result, Ok(()));
        let state = owner.lock().unwrap();
        let status = state.0.snapshot();
        assert_eq!(
            (status.state, status.capture_queued, status.render_queued),
            ("suspended", 0, 0)
        );
        let blocks = row["blocks"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert!(blocks[0]["hex"].as_str().unwrap().chars().any(|c| c != '0'));
        assert!(blocks[1]["hex"].as_str().unwrap().chars().all(|c| c == '0'));
        rows.push(json!({"rate":rate,"pump":row,"state":status.state,"generation":status.generation,"captureQueued":status.capture_queued,"renderQueued":status.render_queued}));
    }
    save("render-pump-composition", &rows);
}
