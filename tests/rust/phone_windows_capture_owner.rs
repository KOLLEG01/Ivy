use super::*;
use crate::media::AudioSettings;
use crate::media::windows_capture::CaptureSink;
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::VecDeque,
    marker::PhantomData,
    rc::Rc,
    sync::{Condvar, atomic::AtomicUsize},
    time::Duration,
};

#[derive(Default)]
struct Gate {
    entered: AtomicBool,
    open: Mutex<bool>,
    changed: Condvar,
}
impl Gate {
    fn wait(&self) {
        self.entered.store(true, Ordering::Release);
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.changed.wait(open).unwrap();
        }
    }
    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }
}
struct ReleaseOnDrop(Vec<Arc<Gate>>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        for gate in &self.0 {
            gate.release();
        }
    }
}
struct EndpointLease {
    live: Arc<AtomicUsize>,
    track: Arc<Track>,
    panic: bool,
}
impl Drop for EndpointLease {
    fn drop(&mut self) {
        self.track.record("endpoint-drop");
        self.live.fetch_sub(1, Ordering::AcqRel);
        if self.panic {
            panic!("scripted endpoint disposal panic");
        }
    }
}
struct Pending {
    factory: Option<Factory>,
    _endpoint: EndpointLease,
    mode: &'static str,
}
impl Admission for Pending {
    fn configure(&mut self) -> Result<Box<dyn CaptureStream>> {
        self._endpoint.track.record("configure");
        match self.mode {
            "configure-error" => Err(CaptureError::Initialize),
            "configure-panic" => panic!("scripted deferred capture panic"),
            _ => self.factory.take().unwrap()(),
        }
    }
}
fn pending(
    track: &Arc<Track>,
    live: &Arc<AtomicUsize>,
    mode: &'static str,
    dispose: Option<Arc<Gate>>,
    exit: Option<Arc<Gate>>,
) -> AdmissionFactory {
    let stream = factory(track, mode, None, None, dispose, exit);
    let track = track.clone();
    let live = live.clone();
    Box::new(move || {
        track.record("admit");
        live.fetch_add(1, Ordering::AcqRel);
        Ok(Box::new(Pending {
            factory: Some(stream),
            mode,
            _endpoint: EndpointLease {
                live,
                track,
                panic: mode == "endpoint-panic",
            },
        }) as Box<dyn Admission>)
    })
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deferred_capture_retirement_keeps_endpoint_until_close_and_actual_thread_exit() {
    let mut rows = vec![];
    for action in [
        "pending",
        "configured",
        "running",
        "configure-error",
        "configure-panic",
        "drop-panic",
        "endpoint-panic",
    ] {
        let track = Arc::new(Track::default());
        let live = Arc::new(AtomicUsize::new(0));
        let recorder = Recorder::open_admitted_with(
            pending(&track, &live, action, None, None),
            guard(&track, vec![true, true], 0),
            new_buffers(),
            false,
        )
        .await
        .unwrap();
        assert_eq!(track.opened.load(Ordering::Acquire), 0);
        assert_eq!(recorder.start(), Err(CaptureError::Start));
        if action != "pending" {
            drop(recorder.configure());
            assert_eq!(
                recorder.configure().await,
                match action {
                    "configure-error" => Err(CaptureError::Initialize),
                    "configure-panic" => Err(CaptureError::Cleanup),
                    _ => Ok(()),
                }
            );
        }
        if action == "running" {
            recorder.start().unwrap();
        }
        drop(recorder.retire());
        let uncertain = matches!(action, "configure-panic" | "drop-panic");
        assert_eq!(
            recorder.retire().await,
            if uncertain {
                Err(CaptureError::Cleanup)
            } else {
                Ok(())
            }
        );
        assert_eq!(track.live.load(Ordering::Acquire), 0);
        assert_eq!(live.load(Ordering::Acquire), 1);
        assert!(recorder.state.outcome.get().is_none());
        assert!(recorder.state.joined.get().is_none());
        assert_eq!(recorder.configure().await, Err(CaptureError::Start));
        assert_eq!(recorder.start(), Err(CaptureError::Start));
        assert_eq!(
            recorder.close().await,
            if uncertain || action == "endpoint-panic" {
                Err(CaptureError::Cleanup)
            } else {
                Ok(())
            }
        );
        assert_eq!(live.load(Ordering::Acquire), 0);
        rows.push(track.row(action));
    }
    let track = Arc::new(Track::default());
    let live = Arc::new(AtomicUsize::new(0));
    let dispose = Arc::new(Gate::default());
    let exit = Arc::new(Gate::default());
    let _release = ReleaseOnDrop(vec![dispose.clone(), exit.clone()]);
    let recorder = Recorder::open_admitted_with(
        pending(&track, &live, "", Some(dispose.clone()), Some(exit.clone())),
        guard(&track, vec![true, true], 0),
        new_buffers(),
        false,
    )
    .await
    .unwrap();
    recorder.configure().await.unwrap();
    let retirement = recorder.retire();
    until(|| dispose.entered.load(Ordering::Acquire)).await;
    assert_eq!(live.load(Ordering::Acquire), 1);
    assert!(recorder.state.retired.get().is_none());
    dispose.release();
    retirement.await.unwrap();
    assert_eq!(live.load(Ordering::Acquire), 1);
    let closing = recorder.close();
    until(|| exit.entered.load(Ordering::Acquire)).await;
    assert_eq!(live.load(Ordering::Acquire), 0);
    assert!(recorder.state.joined.get().is_none());
    exit.release();
    closing.await.unwrap();
    rows.push(track.row("retirement-before-endpoint-and-tls-exit"));
    save("retirement", &rows);
}
#[derive(Default)]
struct Track {
    live: AtomicUsize,
    opened: AtomicUsize,
    dropped: AtomicUsize,
    trace: Mutex<Vec<(String, String)>>,
}
impl Track {
    fn record(&self, name: &str) {
        self.trace
            .lock()
            .unwrap()
            .push((name.into(), format!("{:?}", std::thread::current().id())));
    }
    fn row(&self, name: &str) -> Value {
        assert_eq!(self.live.load(Ordering::Acquire), 0);
        let trace = self.trace.lock().unwrap().clone();
        let threads: std::collections::HashSet<_> =
            trace.iter().map(|(_, thread)| thread).collect();
        assert!(threads.len() <= 1, "{name}: {trace:?}");
        assert_eq!(
            self.opened.load(Ordering::Acquire),
            self.dropped.load(Ordering::Acquire)
        );
        json!({"name":name,"live":0,"opened":self.opened.load(Ordering::Acquire),"dropped":self.dropped.load(Ordering::Acquire),"trace":trace,"nativeThreads":threads.len()})
    }
}
struct ExitBarrier {
    gate: Arc<Gate>,
    track: Arc<Track>,
}
impl Drop for ExitBarrier {
    fn drop(&mut self) {
        self.track.record("thread-exit-enter");
        self.gate.wait();
        self.track.record("thread-exit-done");
    }
}
thread_local! {static EXIT_BARRIER:RefCell<Option<ExitBarrier>>=const{RefCell::new(None)};}
struct FakeStream {
    track: Arc<Track>,
    mode: &'static str,
    run: Option<Arc<Gate>>,
    drop: Option<Arc<Gate>>,
    _thread: PhantomData<Rc<()>>,
}
impl CaptureStream for FakeStream {
    fn run(&mut self, stop: &AtomicBool, sink: &mut CaptureSink<'_>) -> Result<()> {
        self.track.record("run");
        let bytes: Vec<u8> = [0.25f32, 0.75, -1.0, -1.0]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        sink(&bytes, 0, 7, u64::MAX)?;
        if let Some(gate) = &self.run {
            gate.wait();
            sink(&bytes, 0, 8, u64::MAX - 1)?;
            self.track.record("late-callback");
        }
        if self.mode == "run-panic" {
            panic!("scripted native stream panic");
        }
        if self.mode == "run-error" {
            return Err(CaptureError::Wait);
        }
        while !stop.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        self.track.record("stopped");
        Ok(())
    }
}
impl Drop for FakeStream {
    fn drop(&mut self) {
        self.track.record("dispose-enter");
        if let Some(gate) = &self.drop {
            gate.wait();
        }
        self.track.live.fetch_sub(1, Ordering::AcqRel);
        self.track.dropped.fetch_add(1, Ordering::AcqRel);
        self.track.record("dispose-done");
        if self.mode == "drop-panic" {
            panic!("scripted native disposal panic");
        }
    }
}
fn factory(
    track: &Arc<Track>,
    mode: &'static str,
    open: Option<Arc<Gate>>,
    run: Option<Arc<Gate>>,
    drop: Option<Arc<Gate>>,
    exit: Option<Arc<Gate>>,
) -> Factory {
    let track = track.clone();
    Box::new(move || {
        track.record("open");
        if let Some(gate) = open {
            gate.wait();
        }
        if mode == "open-error" {
            return Err(CaptureError::Client);
        }
        if mode == "open-panic" {
            panic!("scripted activation panic");
        }
        if let Some(gate) = exit {
            EXIT_BARRIER.with(|slot| {
                *slot.borrow_mut() = Some(ExitBarrier {
                    gate,
                    track: track.clone(),
                })
            });
        }
        track.live.fetch_add(1, Ordering::AcqRel);
        track.opened.fetch_add(1, Ordering::AcqRel);
        Ok(Box::new(FakeStream {
            track,
            mode,
            run,
            drop,
            _thread: PhantomData,
        }) as Box<dyn CaptureStream>)
    })
}
fn guard(track: &Arc<Track>, values: Vec<bool>, panic_at: usize) -> GuardFactory {
    let track = track.clone();
    Box::new(move || {
        let mut values = VecDeque::from(values);
        let mut calls = 0;
        Ok(Box::new(move || {
            calls += 1;
            track.record(&format!("guard-{calls}"));
            if calls == panic_at {
                panic!("scripted desktop guard panic");
            }
            values.pop_front().unwrap_or(true)
        }) as Guard)
    })
}
fn new_buffers() -> Buffers {
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
    Arc::new(Mutex::new(buffers))
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
fn save(name: &str, rows: &[Value]) {
    if let Some(root) = std::env::var_os("IVY_PHONE_CAPTURE_OWNER_TRACE") {
        let root = std::path::PathBuf::from(root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join(format!("worker-{name}.json")),
            serde_json::to_vec(rows).unwrap(),
        )
        .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_authority_and_capture_failure_keep_resources_until_close() {
    let mut rows = vec![];
    for (name, values, panic_at, mode, expected) in [
        ("guard-before", vec![false], 0, "", CaptureError::Owner),
        ("guard-after", vec![true, false], 0, "", CaptureError::Owner),
        (
            "guard-panic-before",
            vec![true, true],
            1,
            "",
            CaptureError::Owner,
        ),
        (
            "guard-panic-after",
            vec![true, true],
            2,
            "",
            CaptureError::Owner,
        ),
        (
            "open-error",
            vec![true, true],
            0,
            "open-error",
            CaptureError::Client,
        ),
        (
            "open-panic",
            vec![true, true],
            0,
            "open-panic",
            CaptureError::Cleanup,
        ),
    ] {
        let track = Arc::new(Track::default());
        let buffers = new_buffers();
        let result = Recorder::open_with(
            factory(&track, mode, None, None, None, None),
            guard(&track, values, panic_at),
            buffers.clone(),
        )
        .await;
        assert!(matches!(result,Err(error) if error==expected), "{name}");
        assert_eq!(buffers.lock().unwrap().snapshot().state, "closed");
        if name == "guard-before" || name == "guard-panic-before" {
            assert!(!track.trace.lock().unwrap().iter().any(|(x, _)| x == "open"));
        }
        rows.push(track.row(name));
    }
    for mode in ["run-error", "run-panic", "drop-panic", ""] {
        let track = Arc::new(Track::default());
        let buffers = new_buffers();
        let recorder = Recorder::open_with(
            factory(&track, mode, None, None, None, None),
            guard(&track, vec![true, true], 0),
            buffers.clone(),
        )
        .await
        .unwrap();
        recorder.start().unwrap();
        assert_eq!(recorder.start(), Err(CaptureError::Start));
        until(|| track.trace.lock().unwrap().iter().any(|(x, _)| x == "run")).await;
        if !matches!(mode, "run-error" | "run-panic") {
            until(|| buffers.lock().unwrap().snapshot().capture_queued == 2).await;
            let mut pcm = [0.0; 2];
            assert_eq!(buffers.lock().unwrap().read_captured(&mut pcm), 2);
            assert_eq!(pcm, [0.5, -1.0]);
            recorder.stop();
        }
        until(|| recorder.state.capture.get().is_some()).await;
        assert_eq!(track.live.load(Ordering::Acquire), 1);
        assert_eq!(buffers.lock().unwrap().snapshot().state, "failed");
        let expected = match mode {
            "run-error" => Err(CaptureError::Wait),
            "run-panic" => Err(CaptureError::Callback),
            _ => Ok(()),
        };
        assert_eq!(recorder.state.capture.get(), Some(&expected));
        let close_result = recorder.close().await;
        assert_eq!(
            close_result,
            if mode == "drop-panic" {
                Err(CaptureError::Cleanup)
            } else {
                Ok(())
            }
        );
        assert_eq!(recorder.close().await, close_result);
        assert_eq!(recorder.start(), Err(CaptureError::Start));
        assert_eq!(buffers.lock().unwrap().snapshot().state, "closed");
        drop(recorder);
        rows.push(track.row(if mode.is_empty() { "normal-stop" } else { mode }));
    }
    let track = Arc::new(Track::default());
    let buffers = new_buffers();
    let recorder = Recorder::open_with(
        factory(&track, "", None, None, None, None),
        guard(&track, vec![true, true], 0),
        buffers.clone(),
    )
    .await
    .unwrap();
    recorder.stop();
    assert_eq!(recorder.start(), Err(CaptureError::Start));
    recorder.close().await.unwrap();
    assert!(!track.trace.lock().unwrap().iter().any(|(x, _)| x == "run"));
    rows.push(track.row("close-before-start"));
    save("lifetime", &rows);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lost_observers_and_thread_exit_are_in_the_retained_close() {
    let mut rows = vec![];
    for polled in [false, true] {
        let track = Arc::new(Track::default());
        let buffers = new_buffers();
        let opening_gate = Arc::new(Gate::default());
        let _release = ReleaseOnDrop(vec![opening_gate.clone()]);
        let opening = Recorder::open_with(
            factory(&track, "", Some(opening_gate.clone()), None, None, None),
            guard(&track, vec![true, true], 0),
            buffers.clone(),
        );
        if polled {
            let task = tokio::spawn(opening);
            until(|| opening_gate.entered.load(Ordering::Acquire)).await;
            task.abort();
            match task.await {
                Err(error) => assert!(error.is_cancelled()),
                Ok(_) => panic!("pending open was not cancelled"),
            };
        } else {
            until(|| opening_gate.entered.load(Ordering::Acquire)).await;
            drop(opening);
        }
        assert_eq!(buffers.lock().unwrap().snapshot().state, "closed");
        opening_gate.release();
        until(|| {
            track
                .trace
                .lock()
                .unwrap()
                .iter()
                .any(|(x, _)| x == "dispose-done")
        })
        .await;
        rows.push(track.row(if polled {
            "abandoned-polled-open"
        } else {
            "abandoned-unpolled-open"
        }));
    }
    let track = Arc::new(Track::default());
    let buffers = new_buffers();
    let run = Arc::new(Gate::default());
    let dispose = Arc::new(Gate::default());
    let exit = Arc::new(Gate::default());
    let _release = ReleaseOnDrop(vec![run.clone(), dispose.clone(), exit.clone()]);
    let recorder = Recorder::open_with(
        factory(
            &track,
            "",
            None,
            Some(run.clone()),
            Some(dispose.clone()),
            Some(exit.clone()),
        ),
        guard(&track, vec![true, true], 0),
        buffers.clone(),
    )
    .await
    .unwrap();
    recorder.start().unwrap();
    until(|| run.entered.load(Ordering::Acquire)).await;
    let close = recorder.close();
    assert_eq!(buffers.lock().unwrap().snapshot().state, "closed");
    assert_eq!(buffers.lock().unwrap().snapshot().capture_queued, 0);
    drop(close);
    assert!(recorder.state.joined.get().is_none());
    run.release();
    until(|| dispose.entered.load(Ordering::Acquire)).await;
    assert!(recorder.state.joined.get().is_none());
    assert_eq!(track.live.load(Ordering::Acquire), 1);
    assert_eq!(buffers.lock().unwrap().snapshot().capture_queued, 0);
    dispose.release();
    until(|| exit.entered.load(Ordering::Acquire)).await;
    assert_eq!(recorder.state.outcome.get(), Some(&Ok(())));
    assert!(
        recorder.state.joined.get().is_none(),
        "completion must include actual thread exit, after its native disposal"
    );
    exit.release();
    recorder.close().await.unwrap();
    assert_eq!(recorder.state.joined.get(), Some(&Ok(())));
    drop(recorder);
    rows.push(track.row("abandoned-close-full-thread-join"));
    let track = Arc::new(Track::default());
    let buffers = new_buffers();
    let recorder = Recorder::open_with(
        factory(&track, "", None, None, None, None),
        guard(&track, vec![true, true], 0),
        buffers.clone(),
    )
    .await
    .unwrap();
    let state = recorder.state.clone();
    drop(recorder);
    until(|| state.joined.get().is_some()).await;
    assert_eq!(state.joined.get(), Some(&Ok(())));
    assert_eq!(buffers.lock().unwrap().snapshot().state, "closed");
    rows.push(track.row("dropped-recorder"));
    save("observers", &rows);
}

#[test]
fn missing_runtime_starts_no_native_work() {
    let track = Arc::new(Track::default());
    let buffers = new_buffers();
    let opening = Recorder::open_with(
        factory(&track, "", None, None, None, None),
        guard(&track, vec![true, true], 0),
        buffers,
    );
    drop(opening);
    assert!(track.trace.lock().unwrap().is_empty());
    save("runtime", &[track.row("missing-runtime")]);
}
