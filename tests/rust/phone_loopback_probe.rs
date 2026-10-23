use ivy_phone_bridge::loopback_probe::*;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub(crate) struct ProbeGate {
    ready: AtomicBool,
    changed: Notify,
}
impl ProbeGate {
    pub(crate) fn release(&self) {
        self.ready.store(true, Ordering::SeqCst);
        self.changed.notify_waiters();
    }
    async fn wait(&self) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.ready.load(Ordering::SeqCst) {
                return;
            }
            changed.await;
        }
    }
}
struct Data {
    name: &'static str,
    events: Mutex<Vec<String>>,
    opens: AtomicUsize,
    closes: AtomicUsize,
    live: AtomicUsize,
    supported: AtomicBool,
    stop_on_callback: Mutex<Option<CancellationToken>>,
    open_gate: ProbeGate,
    close_gate: ProbeGate,
}
#[derive(Clone)]
pub(crate) struct World(Arc<Data>);
impl World {
    pub(crate) fn new(name: &'static str) -> Self {
        let w = Self(Arc::new(Data {
            name,
            events: Mutex::new(vec![]),
            opens: AtomicUsize::new(0),
            closes: AtomicUsize::new(0),
            live: AtomicUsize::new(0),
            supported: AtomicBool::new(true),
            stop_on_callback: Mutex::new(None),
            open_gate: ProbeGate::default(),
            close_gate: ProbeGate::default(),
        }));
        w.0.open_gate.release();
        w.0.close_gate.release();
        w
    }
    pub(crate) fn gated() -> Self {
        let w = Self::new("gated");
        w.0.open_gate.ready.store(false, Ordering::SeqCst);
        w.0.close_gate.ready.store(false, Ordering::SeqCst);
        w
    }
    pub(crate) fn opens(&self) -> usize {
        self.0.opens.load(Ordering::SeqCst)
    }
    pub(crate) fn closes(&self) -> usize {
        self.0.closes.load(Ordering::SeqCst)
    }
    pub(crate) fn live(&self) -> usize {
        self.0.live.load(Ordering::SeqCst)
    }
    pub(crate) fn release_open(&self) {
        self.0.open_gate.release();
    }
    pub(crate) fn release_close(&self) {
        self.0.close_gate.release();
    }
    pub(crate) fn events(&self) -> Vec<String> {
        self.0.events.lock().unwrap().clone()
    }
    fn log(&self, s: &str) {
        self.0.events.lock().unwrap().push(s.into());
    }
}
impl ProbeFactory for World {
    fn supported(&self) -> bool {
        self.0.supported.load(Ordering::SeqCst)
    }
    fn open(&self, c: ProbeCapture) -> ProbeFuture<Box<dyn ProbeRecorder>> {
        assert_eq!(
            c,
            ProbeCapture {
                sample_rate: 48000,
                channels: 2,
                buffer_ms: 40,
                excluded_process_id: std::process::id()
            }
        );
        for e in ["format:48000:2", "buffer:40", "process:exclude", "build"] {
            self.log(e);
        }
        self.0.opens.fetch_add(1, Ordering::SeqCst);
        let w = self.clone();
        Box::pin(async move {
            if w.0.name == "open-failed" {
                return Err(ProbeError::Capture);
            }
            w.0.open_gate.wait().await;
            w.0.live.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Recorder {
                world: w,
                events: None,
                closed: false,
            }) as Box<dyn ProbeRecorder>)
        })
    }
}
struct Recorder {
    world: World,
    events: Option<ProbeEvents>,
    closed: bool,
}
fn pcm(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|f| f.to_le_bytes()).collect()
}
impl ProbeRecorder for Recorder {
    fn subscribe(&mut self, events: ProbeEvents) -> Result<(), ProbeError> {
        self.world.log("subscribe:data");
        self.world.log("subscribe:stopped");
        self.events = Some(events);
        Ok(())
    }
    fn start(&mut self) -> Result<(), ProbeError> {
        self.world.log("start");
        let n = self.world.0.name;
        if n == "start-panics" {
            panic!("synthetic recorder start panic");
        }
        if n == "start-failed" {
            return Err(ProbeError::Capture);
        }
        let values = if n == "quiet-silent" {
            vec![f32::NAN, f32::INFINITY]
        } else if n.starts_with("invalid") {
            vec![0.25, f32::NAN]
        } else {
            vec![-0.25, 1.5, 0.0]
        };
        self.events
            .as_ref()
            .unwrap()
            .data(&pcm(&values), n == "quiet-silent");
        if n == "early-stop" {
            self.events.as_ref().unwrap().stopped(Ok(()));
        } else if n == "stopped-error" {
            self.events
                .as_ref()
                .unwrap()
                .stopped(Err(ProbeError::Capture));
        }
        Ok(())
    }
    fn stop(&mut self) -> Result<(), ProbeError> {
        self.world.log("stop");
        let n = self.world.0.name;
        if n == "stop-panics" {
            panic!("synthetic recorder stop panic");
        }
        if n == "stop-failed" || n == "invalid-stop-failed" {
            return Err(ProbeError::Capture);
        }
        if n != "stop-timeout" {
            self.events
                .as_ref()
                .unwrap()
                .stopped(if n == "cancel-stopped-error" {
                    Err(ProbeError::Capture)
                } else {
                    Ok(())
                });
        }
        if let Some(stop) = self.world.0.stop_on_callback.lock().unwrap().as_ref() {
            stop.cancel();
        }
        Ok(())
    }
    fn close(&mut self) -> ProbeFuture<()> {
        self.world.log("close");
        assert!(!self.closed);
        self.closed = true;
        self.world.0.closes.fetch_add(1, Ordering::SeqCst);
        let w = self.world.clone();
        Box::pin(async move {
            w.0.close_gate.wait().await;
            w.0.live.fetch_sub(1, Ordering::SeqCst);
            if w.0.name == "close-panics" {
                panic!("synthetic recorder close future panic");
            }
            if w.0.name == "close-failed" || w.0.name == "invalid-close-failed" {
                Err(ProbeError::Cleanup)
            } else {
                Ok(())
            }
        })
    }
}
pub(crate) async fn until(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
pub(crate) fn save(name: &str, value: &Value) {
    if let Some(dir) = std::env::var_os("IVY_PHONE_LOOPBACK_PROBE_TRACE") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            std::path::Path::new(&dir).join(name),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }
}
fn projected(result: LoopbackProbeResult) -> Value {
    assert_eq!(result.excluded_process_id, std::process::id());
    let mut v: Value =
        serde_json::from_str(&ivy_contracts::json::stringify(&result.json(), 65536).unwrap())
            .unwrap();
    v.as_object_mut().unwrap().remove("excludedProcessId");
    v
}
fn error(e: ProbeError) -> &'static str {
    match e {
        ProbeError::Cancelled => "cancelled",
        ProbeError::SampleBound => "bound",
        ProbeError::InvalidPcm => "pcm",
        ProbeError::StoppedEarly => "early",
        ProbeError::StopTimeout => "timeout",
        ProbeError::Cleanup => "cleanup",
        _ => "capture",
    }
}
fn meter() -> Value {
    let mut rows = vec![];
    for (name, values, silent, state) in [
        ("empty", vec![], false, "completed"),
        ("clipped", vec![-0.25, 1.5, 0.0], false, "completed"),
        ("threshold", vec![0.0001], false, "completed"),
        (
            "above-threshold",
            vec![f32::from_bits(0.0001f32.to_bits() + 1)],
            false,
            "completed",
        ),
        ("tiny", vec![f32::from_bits(1)], false, "completed"),
        ("silent", vec![f32::NAN, f32::INFINITY], true, "completed"),
        ("interrupted", vec![1.0], false, "interrupted"),
        ("partial-invalid", vec![0.25, f32::NAN], false, "completed"),
    ] {
        let m = LoopbackProbeMeter::default();
        let e = m.add(&pcm(&values), silent).err().map(error);
        rows.push(json!({"name":name,"error":e,"result":projected(m.result(state))}));
    }
    for (name, count, repeats) in [
        ("packet-limit", 192000, 1),
        ("packet-over", 192001, 1),
        ("total-limit", 192000, 5),
        ("total-over", 192000, 6),
    ] {
        let m = LoopbackProbeMeter::default();
        let mut e = None;
        for _ in 0..repeats {
            if let Err(f) = m.add(&vec![0; count * 4], true) {
                e = Some(error(f));
                break;
            }
        }
        rows.push(json!({"name":name,"error":e,"result":projected(m.result("completed"))}));
    }
    json!(rows)
}
async fn lifecycle() -> Value {
    let mut rows = vec![];
    for name in [
        "completed",
        "quiet-silent",
        "initially-busy",
        "interrupted",
        "pre-cancelled",
        "open-failed",
        "start-failed",
        "early-stop",
        "stopped-error",
        "invalid-pcm",
        "invalid-stop-failed",
        "invalid-close-failed",
        "stop-failed",
        "close-failed",
        "stop-timeout",
        "cancel-on-stop",
        "cancel-stopped-error",
    ] {
        let w = World::new(name);
        let count = AtomicUsize::new(0);
        let stop = CancellationToken::new();
        if matches!(name, "cancel-on-stop" | "cancel-stopped-error") {
            *w.0.stop_on_callback.lock().unwrap() = Some(stop.clone());
        }
        if name == "pre-cancelled" {
            stop.cancel();
        }
        let operation = PhoneLoopbackProbe::start(
            Arc::new(w.clone()),
            move || {
                let n = count.fetch_add(1, Ordering::SeqCst);
                name != "initially-busy"
                    && (!matches!(
                        name,
                        "interrupted"
                            | "stop-failed"
                            | "close-failed"
                            | "stop-timeout"
                            | "cancel-on-stop"
                            | "cancel-stopped-error"
                    ) || n == 0)
            },
            stop,
        );
        let (result, e) = match operation.wait().await {
            Ok(r) => (Some(projected(r)), None),
            Err(e) => (None, Some(error(e))),
        };
        assert_eq!(w.live(), 0);
        rows.push(json!({"name":name,"error":e,"result":result,"events":w.events()}));
    }
    json!(rows)
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_meter_bounds_and_synthetic_recorder_lifetimes_have_explicit_results() {
    let rows = meter();
    assert_eq!(rows.as_array().unwrap().len(), 12);
    for row in rows.as_array().unwrap() {
        let name = row["name"].as_str().unwrap();
        let result = &row["result"];
        assert_eq!(result["sampleRate"], 48000);
        assert_eq!(result["channels"], 2);
        match name {
            "packet-over" | "total-over" => assert_eq!(row["error"], "bound"),
            "partial-invalid" => {
                assert_eq!(row["error"], "pcm");
                assert_eq!(result["samples"], 0);
            }
            _ => assert!(row["error"].is_null(), "{name}"),
        }
        if name == "clipped" {
            assert_eq!(result["peak"], 1.0);
            assert_eq!(result["samples"], 3);
            assert_eq!(result["detected"], true);
        }
        if name == "above-threshold" {
            assert_eq!(result["detected"], true);
        }
        if ["empty", "threshold", "tiny", "silent"].contains(&name) {
            assert_eq!(result["detected"], false);
        }
        if ["total-limit", "total-over"].contains(&name) {
            assert_eq!(result["samples"], 960000);
        }
    }
    let rows = lifecycle().await;
    assert_eq!(rows.as_array().unwrap().len(), 17);
    for row in rows.as_array().unwrap() {
        let name = row["name"].as_str().unwrap();
        let events = row["events"].as_array().unwrap();
        let count = |event: &str| {
            events
                .iter()
                .filter(|value| value.as_str() == Some(event))
                .count()
        };
        // Opening completes before cancellation/idle admission. Every returned
        // recorder must close even when it never starts.
        let opened = name != "open-failed";
        assert_eq!(count("close"), usize::from(opened), "{name}");
        assert_eq!(
            count("stop"),
            usize::from(
                opened && !["pre-cancelled", "initially-busy", "start-failed"].contains(&name)
            ),
            "{name}"
        );
        if opened {
            assert_eq!(
                events.last().and_then(Value::as_str),
                Some("close"),
                "{name}"
            );
        }
        let error = match name {
            "completed" | "quiet-silent" | "initially-busy" | "interrupted" | "cancel-on-stop" => {
                None
            }
            "pre-cancelled" => Some("cancelled"),
            "early-stop" => Some("early"),
            "invalid-pcm" => Some("pcm"),
            "close-failed" | "invalid-close-failed" => Some("cleanup"),
            "stop-timeout" => Some("timeout"),
            _ => Some("capture"),
        };
        assert_eq!(row["error"].as_str(), error, "{name}");
        if error.is_none() {
            assert_eq!(
                row["result"]["state"],
                if ["initially-busy", "interrupted", "cancel-on-stop"].contains(&name) {
                    "interrupted"
                } else {
                    "completed"
                }
            );
        }
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_keeps_late_open_and_close_after_all_observers_leave() {
    let w = World::gated();
    let stop = CancellationToken::new();
    let op = PhoneLoopbackProbe::start(Arc::new(w.clone()), || true, stop.clone());
    assert_eq!(w.opens(), 1);
    assert!(!op.finished());
    stop.cancel();
    drop(op);
    w.release_open();
    until(|| w.closes() == 1).await;
    assert_eq!(w.live(), 1);
    assert_eq!(
        w.events(),
        vec![
            "format:48000:2",
            "buffer:40",
            "process:exclude",
            "build",
            "close"
        ]
    );
    w.release_close();
    until(|| w.live() == 0).await;
    let unsupported = World::new("unused");
    unsupported.0.supported.store(false, Ordering::SeqCst);
    let op = PhoneLoopbackProbe::start(
        Arc::new(unsupported.clone()),
        || panic!("idle must not run"),
        CancellationToken::new(),
    );
    assert_eq!(op.wait().await, Err(ProbeError::Unsupported));
    assert_eq!(unsupported.opens(), 0);
    let m = LoopbackProbeMeter::default();
    m.add(&[0xff, 0xff, 0xff], false).unwrap();
    assert_eq!(m.result("completed").samples, 0);
    save(
        "probe-retained-open-close.json",
        &json!({"opens":w.opens(),"closes":w.closes(),"live":w.live(),"events":w.events(),"allObserversDropped":true,"unsupportedOpened":unsupported.opens(),"trailingBytesIgnored":true}),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_sync_panics_still_dispose_the_original_recorder() {
    let mut rows = vec![];
    for name in ["start-panics", "stop-panics", "idle-panics", "close-panics"] {
        let w = World::new(name);
        let count = AtomicUsize::new(0);
        let op = PhoneLoopbackProbe::start(
            Arc::new(w.clone()),
            move || {
                if name == "idle-panics" {
                    panic!("synthetic idle callback panic");
                }
                count.fetch_add(1, Ordering::SeqCst) == 0
            },
            CancellationToken::new(),
        );
        let actual = op.wait().await;
        let expected = if name == "close-panics" {
            ProbeError::Cleanup
        } else {
            ProbeError::Capture
        };
        assert_eq!(actual, Err(expected));
        assert_eq!(w.closes(), 1);
        assert_eq!(w.live(), 0);
        if name == "idle-panics" {
            assert!(!w.events().iter().any(|e| e == "start"));
        }
        rows.push(json!({"name":name,"error":error(expected),"closes":w.closes(),"live":w.live(),"events":w.events()}));
    }
    save(
        "probe-panic-cleanup.json",
        &json!({"cases":rows,"scope":"Rust-only injected synchronous start/stop/idle and asynchronous close-future panics; caught panics remain in stderr."}),
    );
}
