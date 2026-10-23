use super::device::StopSignal;
use super::*;
use crate::media::AudioSettings;
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
#[derive(Default)]
struct Track {
    live: AtomicUsize,
    opened: AtomicUsize,
    dropped: AtomicUsize,
    trace: Mutex<Vec<(String, String)>>,
    signal_calls: Arc<AtomicUsize>,
    signal_gate: Mutex<Option<Arc<Gate>>>,
    signal_fail: AtomicBool,
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
    info: Info,
    signal: StopSignal,
    _probe: Arc<SignalProbe>,
    _thread: PhantomData<Rc<()>>,
}
#[derive(Default)]
struct SignalProbe {
    calls: Arc<AtomicUsize>,
    fail: bool,
    gate: Option<Arc<Gate>>,
}
unsafe extern "system" fn set_signal(raw: super::super::Raw) -> i32 {
    let probe = unsafe { &*(raw as *const SignalProbe) };
    probe.calls.fetch_add(1, Ordering::AcqRel);
    if let Some(gate) = &probe.gate {
        gate.wait();
    }
    i32::from(!probe.fail)
}
fn test_info() -> Info {
    let mut mix = vec![0; 18];
    mix[..2].copy_from_slice(&3_u16.to_le_bytes());
    mix[2..4].copy_from_slice(&2_u16.to_le_bytes());
    mix[4..8].copy_from_slice(&48000_u32.to_le_bytes());
    mix[8..12].copy_from_slice(&384000_u32.to_le_bytes());
    mix[12..14].copy_from_slice(&8_u16.to_le_bytes());
    mix[14..16].copy_from_slice(&32_u16.to_le_bytes());
    Info {
        rate: 48000,
        channels: 2,
        latency_ms: 10,
        low_latency: false,
        unavailable: None,
        mix,
    }
}
impl Stream for FakeStream {
    fn info(&self) -> &Info {
        &self.info
    }
    fn stop_signal(&self) -> StopSignal {
        self.signal.clone()
    }
    fn run(&mut self, stop: &AtomicBool, buffers: &Mutex<WindowsAudioBuffers>) -> Result<()> {
        self.track.record("run");
        let mut output = [0.0; 2];
        buffers.lock().unwrap().render(&mut output);
        if let Some(gate) = &self.run {
            gate.wait();
            let mut owner = buffers.lock().unwrap();
            owner.write_received(&[0.5, -1.0]);
            owner.render(&mut output);
            if owner.snapshot().state == "closed" {
                assert_eq!(output, [0.0; 2]);
            }
            self.track.record("late-callback");
        }
        if self.mode == "run-panic" {
            panic!("scripted native render panic");
        }
        if self.mode == "run-error" {
            return Err(RenderError::Wait);
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
        self.signal.revoke();
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
            return Err(RenderError::Client);
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
        let probe = Arc::new(SignalProbe {
            calls: track.signal_calls.clone(),
            fail: mode == "signal-error" || track.signal_fail.load(Ordering::Acquire),
            gate: track.signal_gate.lock().unwrap().clone(),
        });
        let signal = unsafe { StopSignal::new(Arc::as_ptr(&probe) as usize, set_signal) };
        let mut info = test_info();
        if mode == "bad-info" {
            info.unavailable = Some(device::Unavailable::Format);
            info.mix.clear();
        }
        Ok(Box::new(FakeStream {
            info,
            signal,
            _probe: probe,
            track,
            mode,
            run,
            drop,
            _thread: PhantomData,
        }) as Box<dyn Stream>)
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
    buffers.write_received(&[0.25, 0.75, 0.5, -1.0]);
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
    if let Some(root) = std::env::var_os("IVY_PHONE_RENDER_OWNER_TRACE") {
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
async fn source_authority_and_render_failure_keep_resources_until_close() {
    let mut rows = vec![];
    for (name, values, panic_at, mode, expected) in [
        ("guard-before", vec![false], 0, "", RenderError::Owner),
        (
            "bad-info",
            vec![true, true],
            0,
            "bad-info",
            RenderError::Configuration,
        ),
        ("guard-after", vec![true, false], 0, "", RenderError::Owner),
        (
            "guard-panic-before",
            vec![true, true],
            1,
            "",
            RenderError::Owner,
        ),
        (
            "guard-panic-after",
            vec![true, true],
            2,
            "",
            RenderError::Owner,
        ),
        (
            "open-error",
            vec![true, true],
            0,
            "open-error",
            RenderError::Client,
        ),
        (
            "open-panic",
            vec![true, true],
            0,
            "open-panic",
            RenderError::Cleanup,
        ),
    ] {
        let track = Arc::new(Track::default());
        let buffers = new_buffers();
        let result = Player::open_with(
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
    for mode in ["run-error", "run-panic", "drop-panic", "signal-error", ""] {
        let track = Arc::new(Track::default());
        let buffers = new_buffers();
        let player = Player::open_with(
            factory(&track, mode, None, None, None, None),
            guard(&track, vec![true, true], 0),
            buffers.clone(),
        )
        .await
        .unwrap();
        assert_eq!(player.status().latency_ms, 10);
        player.configure().await.unwrap();
        assert!(!player.status().low_latency);
        assert_eq!(player.status().unavailable, None);
        player.start().unwrap();
        assert_eq!(player.start(), Err(RenderError::Start));
        until(|| track.trace.lock().unwrap().iter().any(|(x, _)| x == "run")).await;
        if !matches!(mode, "run-error" | "run-panic") {
            until(|| buffers.lock().unwrap().snapshot().render_queued == 2).await;
            let mut pcm = [0.0; 2];
            assert_eq!(buffers.lock().unwrap().render(&mut pcm), 2);
            assert_eq!(pcm, [0.5, -1.0]);
            let stopped = player.stop();
            if mode == "signal-error" {
                assert_eq!(stopped, Err(RenderError::Signal));
            } else {
                stopped.unwrap();
            }
        }
        until(|| player.state.render.get().is_some()).await;
        assert_eq!(track.live.load(Ordering::Acquire), 1);
        assert_eq!(buffers.lock().unwrap().snapshot().state, "failed");
        let expected = match mode {
            "run-error" => Err(RenderError::Wait),
            "run-panic" => Err(RenderError::Callback),
            _ => Ok(()),
        };
        assert_eq!(player.state.render.get(), Some(&expected));
        let close_result = player.close().await;
        assert_eq!(
            close_result,
            if mode == "drop-panic" {
                Err(RenderError::Cleanup)
            } else if mode == "signal-error" {
                Err(RenderError::Signal)
            } else {
                Ok(())
            }
        );
        assert_eq!(player.close().await, close_result);
        assert_eq!(player.start(), Err(RenderError::Start));
        assert_eq!(buffers.lock().unwrap().snapshot().state, "closed");
        drop(player);
        rows.push(track.row(if mode.is_empty() { "normal-stop" } else { mode }));
    }
    let track = Arc::new(Track::default());
    let buffers = new_buffers();
    let player = Player::open_with(
        factory(&track, "", None, None, None, None),
        guard(&track, vec![true, true], 0),
        buffers.clone(),
    )
    .await
    .unwrap();
    player.stop().unwrap();
    assert_eq!(player.start(), Err(RenderError::Start));
    player.close().await.unwrap();
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
        let opening = Player::open_with(
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
    let player = Player::open_with(
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
    player.start().unwrap();
    until(|| run.entered.load(Ordering::Acquire)).await;
    let close = player.close();
    assert_eq!(buffers.lock().unwrap().snapshot().state, "closed");
    assert_eq!(buffers.lock().unwrap().snapshot().render_queued, 0);
    drop(close);
    assert!(player.state.joined.get().is_none());
    run.release();
    until(|| dispose.entered.load(Ordering::Acquire)).await;
    assert!(player.state.joined.get().is_none());
    assert_eq!(track.live.load(Ordering::Acquire), 1);
    assert_eq!(buffers.lock().unwrap().snapshot().render_queued, 0);
    dispose.release();
    until(|| exit.entered.load(Ordering::Acquire)).await;
    assert_eq!(player.state.outcome.get(), Some(&Ok(())));
    assert!(
        player.state.joined.get().is_none(),
        "completion must include actual thread exit, after its native disposal"
    );
    exit.release();
    player.close().await.unwrap();
    assert_eq!(player.state.joined.get(), Some(&Ok(())));
    drop(player);
    rows.push(track.row("abandoned-close-full-thread-join"));
    let track = Arc::new(Track::default());
    let buffers = new_buffers();
    let player = Player::open_with(
        factory(&track, "", None, None, None, None),
        guard(&track, vec![true, true], 0),
        buffers.clone(),
    )
    .await
    .unwrap();
    let state = player.state.clone();
    drop(player);
    until(|| state.joined.get().is_some()).await;
    assert_eq!(state.joined.get(), Some(&Ok(())));
    assert_eq!(buffers.lock().unwrap().snapshot().state, "closed");
    rows.push(track.row("dropped-player"));
    save("observers", &rows);
}

#[test]
fn missing_runtime_starts_no_native_work() {
    let track = Arc::new(Track::default());
    let buffers = new_buffers();
    let opening = Player::open_with(
        factory(&track, "", None, None, None, None),
        guard(&track, vec![true, true], 0),
        buffers,
    );
    drop(opening);
    assert!(track.trace.lock().unwrap().is_empty());
    save("runtime", &[track.row("missing-runtime")]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_signal_revocation_serializes_with_the_last_inflight_set() {
    let mut rows = vec![];
    for fail in [false, true] {
        let probe = Arc::new(SignalProbe {
            fail,
            ..SignalProbe::default()
        });
        let signal = unsafe { StopSignal::new(Arc::as_ptr(&probe) as usize, set_signal) };
        assert_eq!(
            signal.set(),
            if fail {
                Err(RenderError::Signal)
            } else {
                Ok(())
            }
        );
        signal.revoke();
        assert_eq!(signal.set(), Ok(()));
        assert_eq!(probe.calls.load(Ordering::Acquire), 1);
        rows.push(json!({"name":if fail{"failed-set-is-explicit"}else{"retired-signal-is-inert"},"calls":1,"afterRetirementCalled":false}));
    }
    let gate = Arc::new(Gate::default());
    let _release = ReleaseOnDrop(vec![gate.clone()]);
    let probe = Arc::new(SignalProbe {
        gate: Some(gate.clone()),
        ..SignalProbe::default()
    });
    let signal = unsafe { StopSignal::new(Arc::as_ptr(&probe) as usize, set_signal) };
    let active = signal.clone();
    let keep = probe.clone();
    let setting = std::thread::spawn(move || {
        let _keep = keep;
        active.set()
    });
    until(|| gate.entered.load(Ordering::Acquire)).await;
    let entered = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let active = signal.clone();
    let started = entered.clone();
    let finished = done.clone();
    let retiring = std::thread::spawn(move || {
        started.store(true, Ordering::Release);
        active.revoke();
        finished.store(true, Ordering::Release);
    });
    until(|| entered.load(Ordering::Acquire)).await;
    assert!(!done.load(Ordering::Acquire));
    gate.release();
    assert_eq!(setting.join().unwrap(), Ok(()));
    retiring.join().unwrap();
    assert!(done.load(Ordering::Acquire));
    assert_eq!(signal.set(), Ok(()));
    assert_eq!(probe.calls.load(Ordering::Acquire), 1);
    rows.push(
        json!({"name":"retire-waits-for-inflight-set","calls":1,"afterRetirementCalled":false}),
    );
    let track = Arc::new(Track::default());
    let gate = Arc::new(Gate::default());
    let _release = ReleaseOnDrop(vec![gate.clone()]);
    *track.signal_gate.lock().unwrap() = Some(gate.clone());
    track.signal_fail.store(true, Ordering::Release);
    let player = Arc::new(
        Player::open_with(
            factory(&track, "", None, None, None, None),
            guard(&track, vec![true, true], 0),
            new_buffers(),
        )
        .await
        .unwrap(),
    );
    player.start().unwrap();
    let active = player.clone();
    let stopping = std::thread::spawn(move || active.stop());
    until(|| gate.entered.load(Ordering::Acquire)).await;
    let active = player.clone();
    let entered = Arc::new(AtomicBool::new(false));
    let started = entered.clone();
    let closing = std::thread::spawn(move || {
        started.store(true, Ordering::Release);
        active.close()
    });
    until(|| entered.load(Ordering::Acquire)).await;
    assert!(player.state.joined.get().is_none());
    gate.release();
    assert_eq!(stopping.join().unwrap(), Err(RenderError::Signal));
    assert_eq!(closing.join().unwrap().await, Err(RenderError::Signal));
    let calls = track.signal_calls.load(Ordering::Acquire);
    assert_eq!(player.stop(), Err(RenderError::Signal));
    drop(player);
    assert_eq!(track.signal_calls.load(Ordering::Acquire), calls);
    let mut row = track.row("close-retains-concurrent-signal-failure");
    row["calls"] = json!(calls);
    row["afterRetirementCalled"] = json!(false);
    rows.push(row);
    save("signals", &rows);
}

struct PendingLease {
    track: Arc<Track>,
    live: Arc<AtomicUsize>,
    panic_on_drop: bool,
}
impl Drop for PendingLease {
    fn drop(&mut self) {
        self.track.record("admitted-device-drop");
        self.live.fetch_sub(1, Ordering::AcqRel);
        if self.panic_on_drop {
            panic!("scripted admitted device disposal panic");
        }
    }
}
struct DeferredAdmission {
    factory: Option<Factory>,
    device: Option<PendingLease>,
    gate: Option<Arc<Gate>>,
    mode: &'static str,
    _thread: PhantomData<Rc<()>>,
}
impl Admission for DeferredAdmission {
    fn configure(&mut self) -> Result<Box<dyn Stream>> {
        self.device.as_ref().unwrap().track.record("configure");
        if let Some(gate) = &self.gate {
            gate.wait();
        }
        if self.mode == "configure-error" {
            return Err(RenderError::Initialize);
        }
        if self.mode == "configure-panic" {
            panic!("scripted deferred configure panic");
        }
        self.factory.take().unwrap()()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_retirement_keeps_admitted_endpoint_until_final_close_and_thread_exit() {
    let mut rows = vec![];
    for action in [
        "pending",
        "configured",
        "running",
        "configure-error",
        "configure-panic",
        "drop-panic",
        "admit-drop-panic",
    ] {
        let track = Arc::new(Track::default());
        let live = Arc::new(AtomicUsize::new(0));
        let player = Player::open_admitted_with(
            admission_factory(&track, &live, action, None, None, None, None),
            guard(&track, vec![true, true], 0),
            new_buffers(),
            false,
        )
        .await
        .unwrap();
        if action != "pending" {
            let configured = player.configure().await;
            assert_eq!(
                configured,
                match action {
                    "configure-error" => Err(RenderError::Initialize),
                    "configure-panic" => Err(RenderError::Cleanup),
                    _ => Ok(()),
                }
            );
        }
        if action == "running" {
            player.start().unwrap();
        }
        drop(player.retire());
        let uncertain = matches!(action, "configure-panic" | "drop-panic");
        assert_eq!(
            player.retire().await,
            if uncertain {
                Err(RenderError::Cleanup)
            } else {
                Ok(())
            }
        );
        assert_eq!(track.live.load(Ordering::Acquire), 0);
        assert_eq!(live.load(Ordering::Acquire), 1);
        assert!(player.state.outcome.get().is_none());
        assert!(player.state.joined.get().is_none());
        assert_eq!(player.configure().await, Err(RenderError::Start));
        assert_eq!(player.start(), Err(RenderError::Start));
        assert_eq!(
            player.close().await,
            if uncertain || action == "admit-drop-panic" {
                Err(RenderError::Cleanup)
            } else {
                Ok(())
            }
        );
        rows.push(admission_row(&track, &live, action));
    }
    let track = Arc::new(Track::default());
    let live = Arc::new(AtomicUsize::new(0));
    let dispose = Arc::new(Gate::default());
    let exit = Arc::new(Gate::default());
    let _release = ReleaseOnDrop(vec![dispose.clone(), exit.clone()]);
    let player = Player::open_admitted_with(
        admission_factory(
            &track,
            &live,
            "",
            None,
            None,
            Some(dispose.clone()),
            Some(exit.clone()),
        ),
        guard(&track, vec![true, true], 0),
        new_buffers(),
        false,
    )
    .await
    .unwrap();
    player.configure().await.unwrap();
    let retirement = player.retire();
    until(|| dispose.entered.load(Ordering::Acquire)).await;
    assert_eq!(live.load(Ordering::Acquire), 1);
    assert!(player.state.retired.get().is_none());
    dispose.release();
    retirement.await.unwrap();
    assert_eq!(live.load(Ordering::Acquire), 1);
    let closing = player.close();
    until(|| exit.entered.load(Ordering::Acquire)).await;
    assert_eq!(live.load(Ordering::Acquire), 0);
    assert!(player.state.joined.get().is_none());
    exit.release();
    closing.await.unwrap();
    rows.push(admission_row(
        &track,
        &live,
        "retirement-before-endpoint-and-tls-exit",
    ));
    save("retirement", &rows);
}
fn admission_factory(
    track: &Arc<Track>,
    live: &Arc<AtomicUsize>,
    mode: &'static str,
    admit: Option<Arc<Gate>>,
    configure: Option<Arc<Gate>>,
    dispose: Option<Arc<Gate>>,
    exit: Option<Arc<Gate>>,
) -> AdmissionFactory {
    let factory = factory(track, mode, None, None, dispose, None);
    let track = track.clone();
    let live = live.clone();
    Box::new(move || {
        track.record("admit");
        if mode == "admit-error" {
            return Err(RenderError::Client);
        }
        if mode == "admit-panic" {
            panic!("scripted admission panic");
        }
        if let Some(gate) = exit {
            EXIT_BARRIER.with(|slot| {
                *slot.borrow_mut() = Some(ExitBarrier {
                    gate,
                    track: track.clone(),
                })
            });
        }
        live.fetch_add(1, Ordering::AcqRel);
        let device = PendingLease {
            track: track.clone(),
            live,
            panic_on_drop: mode == "admit-drop-panic",
        };
        if let Some(gate) = admit {
            gate.wait();
        }
        Ok(Box::new(DeferredAdmission {
            factory: Some(factory),
            device: Some(device),
            gate: configure,
            mode,
            _thread: PhantomData,
        }) as Box<dyn Admission>)
    })
}
fn admission_row(track: &Track, live: &AtomicUsize, name: &str) -> Value {
    assert_eq!(live.load(Ordering::Acquire), 0);
    let mut row = track.row(name);
    row["admittedDevices"] = json!(0);
    row
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deferred_admission_retains_device_configuration_and_close_across_observers() {
    let mut rows = vec![];
    for (name, values, panic_at, mode, configure, expected) in [
        (
            "admit-guard-before",
            vec![false],
            0,
            "",
            false,
            RenderError::Owner,
        ),
        (
            "admit-guard-panic-before",
            vec![true],
            1,
            "",
            false,
            RenderError::Owner,
        ),
        (
            "admit-error",
            vec![true, true],
            0,
            "admit-error",
            false,
            RenderError::Client,
        ),
        (
            "admit-panic",
            vec![true, true],
            0,
            "admit-panic",
            false,
            RenderError::Cleanup,
        ),
        (
            "configure-error",
            vec![true, true],
            0,
            "configure-error",
            true,
            RenderError::Initialize,
        ),
        (
            "configure-panic",
            vec![true, true],
            0,
            "configure-panic",
            true,
            RenderError::Cleanup,
        ),
        (
            "configure-guard-after",
            vec![true, false],
            0,
            "",
            true,
            RenderError::Owner,
        ),
        (
            "configure-guard-panic-after",
            vec![true, true],
            2,
            "",
            true,
            RenderError::Owner,
        ),
        (
            "configure-bad-info",
            vec![true, true],
            0,
            "bad-info",
            true,
            RenderError::Configuration,
        ),
    ] {
        let track = Arc::new(Track::default());
        let live = Arc::new(AtomicUsize::new(0));
        let buffers = new_buffers();
        let opened = Player::open_admitted_with(
            admission_factory(&track, &live, mode, None, None, None, None),
            guard(&track, values, panic_at),
            buffers.clone(),
            false,
        )
        .await;
        if configure {
            let player = opened.unwrap();
            assert!(player.state.info.get().is_none());
            assert_eq!(player.start(), Err(RenderError::Start));
            assert_eq!(player.configure().await, Err(expected));
            assert_eq!(
                player.close().await,
                if mode == "configure-panic" {
                    Err(RenderError::Cleanup)
                } else {
                    Ok(())
                }
            );
        } else {
            assert!(matches!(opened,Err(error) if error==expected));
        }
        assert_eq!(buffers.lock().unwrap().snapshot().state, "closed");
        rows.push(admission_row(&track, &live, name));
    }
    for action in ["configure", "close", "stop", "drop", "admit-drop-panic"] {
        let track = Arc::new(Track::default());
        let live = Arc::new(AtomicUsize::new(0));
        let mode = if action == "admit-drop-panic" {
            action
        } else {
            ""
        };
        let player = Player::open_admitted_with(
            admission_factory(&track, &live, mode, None, None, None, None),
            guard(&track, vec![true, true], 0),
            new_buffers(),
            false,
        )
        .await
        .unwrap();
        assert_eq!(live.load(Ordering::Acquire), 1);
        assert_eq!(track.opened.load(Ordering::Acquire), 0);
        assert_eq!(player.start(), Err(RenderError::Start));
        assert!(player.state.signal.get().is_none());
        assert!(player.state.info.get().is_none());
        if action == "configure" {
            let observer = player.configure();
            drop(observer);
            player.configure().await.unwrap();
            player.configure().await.unwrap();
            assert_eq!(player.status().latency_ms, 10);
            assert_eq!(live.load(Ordering::Acquire), 1);
            assert_eq!(
                track
                    .trace
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(x, _)| x == "configure")
                    .count(),
                1
            );
            player.start().unwrap();
            player.stop().unwrap();
            player.close().await.unwrap();
        } else if action == "drop" {
            let state = player.state.clone();
            drop(player);
            until(|| state.joined.get().is_some()).await;
            assert_eq!(state.joined.get(), Some(&Ok(())));
            rows.push(admission_row(&track, &live, "admitted-player-drop"));
            continue;
        } else {
            if action == "stop" {
                player.stop().unwrap();
                assert_eq!(player.configure().await, Err(RenderError::Start));
            }
            assert_eq!(
                player.close().await,
                if action == "admit-drop-panic" {
                    Err(RenderError::Cleanup)
                } else {
                    Ok(())
                }
            );
            assert_eq!(player.configure().await, Err(RenderError::Start));
            assert!(
                !track
                    .trace
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(x, _)| x == "configure")
            );
        }
        drop(player);
        rows.push(admission_row(&track, &live, action));
    }
    for polled in [false, true] {
        let track = Arc::new(Track::default());
        let live = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(Gate::default());
        let _release = ReleaseOnDrop(vec![gate.clone()]);
        let opening = Player::open_admitted_with(
            admission_factory(&track, &live, "", Some(gate.clone()), None, None, None),
            guard(&track, vec![true, true], 0),
            new_buffers(),
            false,
        );
        if polled {
            let observer = tokio::spawn(opening);
            until(|| gate.entered.load(Ordering::Acquire)).await;
            observer.abort();
            assert!(observer.await.err().unwrap().is_cancelled());
        } else {
            until(|| gate.entered.load(Ordering::Acquire)).await;
            drop(opening);
        }
        assert_eq!(live.load(Ordering::Acquire), 1);
        gate.release();
        until(|| live.load(Ordering::Acquire) == 0).await;
        rows.push(admission_row(
            &track,
            &live,
            if polled {
                "abandoned-polled-admission"
            } else {
                "abandoned-unpolled-admission"
            },
        ));
    }
    let track = Arc::new(Track::default());
    let live = Arc::new(AtomicUsize::new(0));
    let configure = Arc::new(Gate::default());
    let dispose = Arc::new(Gate::default());
    let exit = Arc::new(Gate::default());
    let _release = ReleaseOnDrop(vec![configure.clone(), dispose.clone(), exit.clone()]);
    let buffers = new_buffers();
    let player = Player::open_admitted_with(
        admission_factory(
            &track,
            &live,
            "",
            None,
            Some(configure.clone()),
            Some(dispose.clone()),
            Some(exit.clone()),
        ),
        guard(&track, vec![true, true], 0),
        buffers.clone(),
        false,
    )
    .await
    .unwrap();
    drop(player.configure());
    until(|| configure.entered.load(Ordering::Acquire)).await;
    let closing = player.close();
    drop(closing);
    assert_eq!(buffers.lock().unwrap().snapshot().state, "closed");
    assert!(player.state.joined.get().is_none());
    assert_eq!(live.load(Ordering::Acquire), 1);
    configure.release();
    until(|| dispose.entered.load(Ordering::Acquire)).await;
    assert!(player.state.joined.get().is_none());
    assert_eq!(live.load(Ordering::Acquire), 1);
    dispose.release();
    until(|| exit.entered.load(Ordering::Acquire)).await;
    assert_eq!(live.load(Ordering::Acquire), 0);
    assert!(player.state.joined.get().is_none());
    exit.release();
    player.close().await.unwrap();
    drop(player);
    rows.push(admission_row(
        &track,
        &live,
        "abandoned-configure-retains-dispose-and-thread-exit",
    ));
    assert_eq!(rows.len(), 17);
    save("route-admission", &rows);
}
