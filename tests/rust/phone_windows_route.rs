use super::*;
use crate::media::AudioMutePolicy;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};
use tokio::sync::Notify;

#[derive(Default)]
struct Gate {
    entered: AtomicBool,
    released: AtomicBool,
    changed: Notify,
}
impl Gate {
    async fn wait(&self) {
        self.entered.store(true, Ordering::Release);
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.released.load(Ordering::Acquire) {
                return;
            }
            changed.await;
        }
    }
    fn release(&self) {
        self.released.store(true, Ordering::Release);
        self.changed.notify_waiters();
    }
}
struct Release(Arc<Gate>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.release();
    }
}
#[derive(Default)]
struct Plan {
    trace: Mutex<Vec<String>>,
    fault: Mutex<Option<String>>,
    gate: Mutex<HashMap<String, Arc<Gate>>>,
    supported: AtomicBool,
    desktop_checks: AtomicUsize,
    buffers: Mutex<Option<Buffers>>,
    failed_callback: AtomicBool,
}
impl Plan {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            supported: AtomicBool::new(true),
            ..Self::default()
        })
    }
    fn at(&self, step: &str) -> Result<()> {
        self.trace.lock().unwrap().push(step.into());
        let fault = self.fault.lock().unwrap().clone();
        match fault.as_deref() {
            Some(fault) if fault == step => Err(CallAudioError::RouteFailed),
            Some(fault) if fault == format!("panic:{step}") => panic!("scripted {step} panic"),
            _ => Ok(()),
        }
    }
    fn work(self: &Arc<Self>, step: String) -> Task<()> {
        let plan = self.clone();
        Box::pin(async move {
            plan.at(&step)?;
            let gate = plan.gate.lock().unwrap().get(&step).cloned();
            if let Some(gate) = gate {
                gate.wait().await;
            }
            Ok(())
        })
    }
    fn steps(&self) -> Vec<String> {
        self.trace.lock().unwrap().clone()
    }
    fn row(&self, name: &str) -> Value {
        json!({"name":name,"trace":self.steps()})
    }
}
struct Stream {
    plan: Arc<Plan>,
    name: &'static str,
}
impl StreamOwner for Stream {
    fn configure(&self) -> Task<()> {
        self.plan.work(format!("{}-configure", self.name))
    }
    fn start(&self) -> Result<()> {
        self.plan.at(&format!("{}-start", self.name))
    }
    fn retire(&self) -> Task<()> {
        self.plan.work(format!("{}-retire", self.name))
    }
    fn close(&self) -> Task<()> {
        self.plan.work(format!("{}-close", self.name))
    }
}
impl RenderOwner for Stream {
    fn status(&self) -> Status {
        self.plan.at("player-status").unwrap();
        Status {
            latency_ms: 7,
            low_latency: false,
            unavailable: Some("engine reason".into()),
        }
    }
}
struct Mute(Arc<Plan>);
impl MuteOwner for Mute {
    fn close(&self) -> Task<()> {
        self.0.work("mute-close".into())
    }
}
struct Fake(Arc<Plan>);
impl Backend for Fake {
    fn process_supported(&self) -> bool {
        self.0.at("version").unwrap();
        self.0.supported.load(Ordering::Acquire)
    }
    fn desktop(&self, desktop: Option<AudioDesktopIdentity>) -> Task<()> {
        if desktop.is_none() {
            return Box::pin(async { Ok(()) });
        }
        let index = self.0.desktop_checks.fetch_add(1, Ordering::AcqRel);
        self.0.work(
            if index == 0 {
                "desktop-before"
            } else {
                "desktop-after"
            }
            .into(),
        )
    }
    fn player(
        &self,
        settings: AudioSettings,
        _: Option<AudioDesktopIdentity>,
        buffers: Buffers,
    ) -> Task<Arc<dyn RenderOwner>> {
        assert_eq!(settings.capture_endpoint, "capture");
        assert_eq!(settings.render_endpoint, "render");
        *self.0.buffers.lock().unwrap() = Some(buffers);
        let plan = self.0.clone();
        Box::pin(async move {
            plan.work("player-admit".into()).await?;
            Ok(Arc::new(Stream {
                plan,
                name: "player",
            }) as Arc<dyn RenderOwner>)
        })
    }
    fn recorder(
        &self,
        settings: AudioSettings,
        desktop: Option<AudioDesktopIdentity>,
        buffers: Buffers,
    ) -> Task<Arc<dyn StreamOwner>> {
        assert!(Arc::ptr_eq(
            self.0.buffers.lock().unwrap().as_ref().unwrap(),
            &buffers
        ));
        if settings.source_mode == "desktop_process" {
            assert_eq!(desktop.unwrap().pid(), 71);
        }
        let plan = self.0.clone();
        Box::pin(async move {
            plan.work("recorder-admit".into()).await?;
            Ok(Arc::new(Stream {
                plan,
                name: "recorder",
            }) as Arc<dyn StreamOwner>)
        })
    }
    fn mute(&self, settings: AudioSettings, buffers: Buffers) -> Task<Arc<dyn MuteOwner>> {
        assert!(settings.mute.is_some());
        assert!(Arc::ptr_eq(
            self.0.buffers.lock().unwrap().as_ref().unwrap(),
            &buffers
        ));
        let plan = self.0.clone();
        Box::pin(async move {
            plan.work("mute-open".into()).await?;
            if plan.failed_callback.load(Ordering::Acquire) {
                buffers.lock().unwrap().fail();
            }
            Ok(Arc::new(Mute(plan)) as Arc<dyn MuteOwner>)
        })
    }
}
fn settings(mode: &str, mute: bool) -> AudioSettings {
    AudioSettings {
        capture_endpoint: "capture".into(),
        render_endpoint: "render".into(),
        source_mode: mode.into(),
        capture_buffer_ms: 40,
        render_latency_ms: 20,
        queue_ms: 80,
        playback_prebuffer_ms: 0,
        enable_wasapi_low_latency: true,
        loopback_render_endpoint: Some("loopback".into()),
        mute: mute.then(|| AudioMutePolicy {
            muted_render_endpoint: Some("mute".into()),
            unmute_receive: true,
            interval_seconds: 3,
        }),
    }
}
fn desktop() -> AudioDesktopIdentity {
    AudioDesktopIdentity::new(
        71,
        123,
        std::path::Path::new("C:/synthetic/editor.exe"),
        "editor!App",
    )
    .unwrap()
}
fn open(
    plan: &Arc<Plan>,
    settings: AudioSettings,
    desktop: Option<AudioDesktopIdentity>,
    stop: CancellationToken,
    permit: Arc<AtomicBool>,
) -> Task<Arc<dyn CallAudioRoute>> {
    begin_open(
        Arc::new(Fake(plan.clone())),
        settings,
        desktop,
        Arc::new(move || permit.load(Ordering::Acquire)),
        stop,
    )
}
async fn until(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !ready() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
fn save(name: &str, rows: &[Value]) {
    if let Some(root) = std::env::var_os("IVY_PHONE_WINDOWS_ROUTE_TRACE") {
        let root = std::path::PathBuf::from(root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join(format!("route-{name}.json")),
            serde_json::to_vec(rows).unwrap(),
        )
        .unwrap();
    }
}
const CLEANUP: [&str; 5] = [
    "recorder-retire",
    "player-retire",
    "mute-close",
    "recorder-close",
    "player-close",
];
fn tail(plan: &Plan, expected: &[&str]) {
    let trace = plan.steps();
    assert!(
        trace.ends_with(&expected.iter().map(|s| s.to_string()).collect::<Vec<_>>()),
        "{trace:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admission_order_and_original_suspended_status_cover_all_sources() {
    let mut rows = vec![];
    for mode in [
        "microphone",
        "virtual_speaker",
        "system_excluding_runtime",
        "desktop_process",
    ] {
        for mute in [false, true] {
            let plan = Plan::new();
            let route = open(
                &plan,
                settings(mode, mute),
                Some(desktop()),
                CancellationToken::new(),
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .unwrap();
            let mut expected = vec![];
            if matches!(mode, "system_excluding_runtime" | "desktop_process") {
                expected.push("version");
            }
            expected.extend(["desktop-before", "player-admit"]);
            if mute {
                expected.push("mute-open");
            }
            expected.extend([
                "player-configure",
                "player-status",
                "recorder-admit",
                "recorder-configure",
                "desktop-after",
                "recorder-start",
                "player-start",
            ]);
            assert_eq!(plan.steps(), expected);
            assert_eq!(
                route.state(),
                PcmAudioState {
                    open: false,
                    generation: 0
                }
            );
            let status = route.status();
            let number = |key| status.get(key).unwrap().as_f64().unwrap();
            assert_eq!(status.get("state").unwrap().as_str().unwrap(), "suspended");
            assert_eq!(status.get("sourceMode").unwrap().as_str().unwrap(), mode);
            assert_eq!(
                [
                    number("sampleRate"),
                    number("channels"),
                    number("captureDroppedSamples"),
                    number("renderDroppedSamples"),
                    number("captureQueuedSamples"),
                    number("renderQueuedSamples"),
                    number("renderLatencyMs")
                ],
                [48000.0, 1.0, 0.0, 0.0, 0.0, 0.0, 7.0]
            );
            assert!(matches!(
                status.get("lowLatencyActive"),
                Some(IvyJson::Bool(false))
            ));
            assert_eq!(
                status
                    .get("lowLatencyUnavailableReason")
                    .unwrap()
                    .as_str()
                    .unwrap(),
                "engine reason"
            );
            assert_eq!(status.into_object().unwrap().len(), 11);
            route.close().await.unwrap();
            let cleanup: Vec<_> = CLEANUP
                .into_iter()
                .filter(|s| mute || *s != "mute-close")
                .collect();
            tail(&plan, &cleanup);
            rows.push(plan.row(&format!("{mode}-mute-{mute}")));
        }
    }
    save("ready", &rows);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preflight_and_failed_preparation_publish_release_only_after_cleanup() {
    let mut rows = vec![];
    for kind in [
        "unsupported",
        "unsupported-before-settings",
        "missing-desktop",
        "settings",
        "cancel-before",
    ] {
        let plan = Plan::new();
        let mut config = settings("desktop_process", true);
        let stop = CancellationToken::new();
        let mut owner = Some(desktop());
        let expected = match kind {
            "unsupported" | "unsupported-before-settings" => {
                plan.supported.store(false, Ordering::Release);
                if kind.ends_with("settings") {
                    config.queue_ms = 0;
                }
                CallAudioError::NotReady
            }
            "missing-desktop" => {
                owner = None;
                CallAudioError::InvalidRequest
            }
            "settings" => {
                config.queue_ms = 0;
                CallAudioError::InvalidRequest
            }
            _ => {
                stop.cancel();
                CallAudioError::PreparationReleased
            }
        };
        assert!(
            matches!(open(&plan, config, owner, stop, Arc::new(AtomicBool::new(true))).await, Err(error) if error == expected)
        );
        assert_eq!(plan.steps(), ["version"]);
        rows.push(plan.row(kind));
    }
    for step in [
        "desktop-before",
        "player-admit",
        "mute-open",
        "player-configure",
        "player-status",
        "recorder-admit",
        "recorder-configure",
        "desktop-after",
        "recorder-start",
        "player-start",
    ] {
        for panic in [false, true] {
            let plan = Plan::new();
            *plan.fault.lock().unwrap() = Some(if panic {
                format!("panic:{step}")
            } else {
                step.into()
            });
            let result = open(
                &plan,
                settings("microphone", true),
                Some(desktop()),
                CancellationToken::new(),
                Arc::new(AtomicBool::new(true)),
            )
            .await;
            let uncertain = panic || step == "player-status"; // status unwrap intentionally models an impossible native metadata panic
            assert!(
                matches!(result, Err(error) if error == if uncertain { CallAudioError::CleanupUnconfirmed } else { CallAudioError::PreparationReleased }),
                "{step}/{panic}"
            );
            let steps = plan.steps();
            let has_player = step != "desktop-before" && step != "player-admit";
            let has_mute = has_player && step != "mute-open";
            let has_recorder = [
                "recorder-configure",
                "desktop-after",
                "recorder-start",
                "player-start",
            ]
            .contains(&step);
            let expected: Vec<_> = CLEANUP
                .into_iter()
                .filter(|s| match *s {
                    "recorder-retire" | "recorder-close" => has_recorder,
                    "mute-close" => has_mute,
                    _ => has_player,
                })
                .collect();
            tail(&plan, &expected);
            assert!(!steps.iter().any(|s| s == "player-start") || step == "player-start");
            rows.push(plan.row(&format!("fault-{step}-panic-{panic}")));
        }
    }
    for after in ["recorder-configure", "player-configure"] {
        let plan = Plan::new();
        let gate = Arc::new(Gate::default());
        let _release = Release(gate.clone());
        plan.gate.lock().unwrap().insert(after.into(), gate.clone());
        let stop = CancellationToken::new();
        let observer = open(
            &plan,
            settings("microphone", true),
            None,
            stop.clone(),
            Arc::new(AtomicBool::new(true)),
        );
        until(|| gate.entered.load(Ordering::Acquire)).await;
        stop.cancel();
        gate.release();
        assert!(matches!(
            observer.await,
            Err(CallAudioError::PreparationReleased)
        ));
        assert!(!plan.steps().iter().any(|s| s.ends_with("-start")));
        tail(&plan, &CLEANUP);
        rows.push(plan.row(&format!("cancel-at-{after}")));
    }
    save("failures", &rows);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_retains_all_finally_stages_and_errors_across_lost_observers() {
    let mut rows = vec![];
    for step in CLEANUP {
        for panic in [false, true] {
            let plan = Plan::new();
            let route = open(
                &plan,
                settings("microphone", true),
                None,
                CancellationToken::new(),
                Arc::new(AtomicBool::new(true)),
            )
            .await
            .unwrap();
            *plan.fault.lock().unwrap() = Some(if panic {
                format!("panic:{step}")
            } else {
                step.into()
            });
            drop(route.close());
            assert_eq!(route.state().open, false);
            assert_eq!(
                route.status().get("state").unwrap().as_str().unwrap(),
                "closed"
            );
            assert_eq!(route.close().await, Err(CallAudioError::CleanupUnconfirmed));
            assert_eq!(route.close().await, Err(CallAudioError::CleanupUnconfirmed));
            tail(&plan, &CLEANUP);
            for stage in CLEANUP {
                assert_eq!(plan.steps().iter().filter(|s| *s == stage).count(), 1);
            }
            rows.push(plan.row(&format!("cleanup-{step}-panic-{panic}")));
        }
    }
    for polled in [false, true] {
        let plan = Plan::new();
        let gate = Arc::new(Gate::default());
        let _release = Release(gate.clone());
        plan.gate
            .lock()
            .unwrap()
            .insert("recorder-configure".into(), gate.clone());
        let observer = open(
            &plan,
            settings("microphone", true),
            None,
            CancellationToken::new(),
            Arc::new(AtomicBool::new(true)),
        );
        if polled {
            let task = tokio::spawn(observer);
            until(|| gate.entered.load(Ordering::Acquire)).await;
            task.abort();
            assert!(task.await.err().unwrap().is_cancelled());
        } else {
            until(|| gate.entered.load(Ordering::Acquire)).await;
            drop(observer);
        }
        gate.release();
        until(|| plan.steps().iter().any(|s| s == "player-close")).await;
        tail(&plan, &CLEANUP);
        rows.push(plan.row(&format!("open-observer-lost-{polled}")));
    }
    for drop_route in [false, true] {
        let plan = Plan::new();
        let gate = Arc::new(Gate::default());
        let _release = Release(gate.clone());
        plan.gate
            .lock()
            .unwrap()
            .insert("recorder-retire".into(), gate.clone());
        let route = open(
            &plan,
            settings("microphone", true),
            None,
            CancellationToken::new(),
            Arc::new(AtomicBool::new(true)),
        )
        .await
        .unwrap();
        let closing = route.close();
        drop(closing);
        until(|| gate.entered.load(Ordering::Acquire)).await;
        assert!(!plan.steps().iter().any(|s| s == "mute-close"));
        assert_eq!(
            route.status().get("state").unwrap().as_str().unwrap(),
            "closed"
        );
        if drop_route {
            drop(route);
            gate.release();
            until(|| plan.steps().iter().any(|s| s == "player-close")).await;
        } else {
            gate.release();
            route.close().await.unwrap();
        }
        tail(&plan, &CLEANUP);
        rows.push(plan.row(&format!("close-observer-lost-drop-route-{drop_route}")));
    }
    let plan = Plan::new();
    let route = open(
        &plan,
        settings("microphone", true),
        None,
        CancellationToken::new(),
        Arc::new(AtomicBool::new(true)),
    )
    .await
    .unwrap();
    drop(route);
    until(|| plan.steps().iter().any(|s| s == "player-close")).await;
    tail(&plan, &CLEANUP);
    rows.push(plan.row("last-route-drop"));
    save("cleanup", &rows);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pcm_generation_status_and_send_share_callback_authority_lock() {
    let plan = Plan::new();
    let permit = Arc::new(AtomicBool::new(false));
    let route = open(
        &plan,
        settings("microphone", false),
        None,
        CancellationToken::new(),
        permit.clone(),
    )
    .await
    .unwrap();
    assert_eq!(route.resume(), Err(CallAudioError::OwnerChanged));
    permit.store(true, Ordering::Release);
    route.resume().unwrap();
    let first = route.state();
    assert_eq!(
        first,
        PcmAudioState {
            open: true,
            generation: 1
        }
    );
    route.resume().unwrap();
    assert_eq!(route.state(), first);
    let buffers = plan.buffers.lock().unwrap().clone().unwrap();
    let captured: Vec<u8> = [0.25f32, 0.75, -1.0, -1.0]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
    buffers.lock().unwrap().captured(&captured, 0);
    let mut output = [99.0; 4];
    assert_eq!(route.read_captured(&mut output, first.generation), 2);
    assert_eq!(output, [0.5, -1.0, 0.0, 0.0]);
    assert!(route.write_received(&[0.25, -0.75], first.generation));
    assert_eq!(
        route.status().get("renderQueuedSamples").unwrap().as_f64(),
        Some(2.0)
    );
    route.suspend().unwrap();
    route.resume().unwrap();
    let second = route.state();
    assert_eq!(second.generation, 2);
    output.fill(99.0);
    assert_eq!(route.read_captured(&mut output, first.generation), 0);
    assert_eq!(output, [0.0; 4]);
    assert!(!route.write_received(&[1.0], first.generation));
    let mut calls = 0;
    assert!(!route.with_send_state(first, &mut || calls += 1));
    assert_eq!(calls, 0);
    let started = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let mut thread = None;
    assert!(route.with_send_state(second, &mut || {
        calls += 1;
        let route = route.clone();
        let worker_started = started.clone();
        let worker_finished = finished.clone();
        thread = Some(std::thread::spawn(move || {
            worker_started.store(true, Ordering::Release);
            route.suspend().unwrap();
            worker_finished.store(true, Ordering::Release);
        }));
        while !started.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        assert!(!finished.load(Ordering::Acquire));
    }));
    thread.take().unwrap().join().unwrap();
    assert_eq!(calls, 1);
    assert!(finished.load(Ordering::Acquire));
    let silent = route.state();
    assert!(!silent.open);
    assert!(route.with_send_state(silent, &mut || calls += 1));
    assert_eq!(calls, 2);
    route.resume().unwrap();
    permit.store(false, Ordering::Release);
    assert_eq!(
        route.status().get("state").unwrap().as_str().unwrap(),
        "open"
    ); // status does not invoke permit
    assert!(!route.state().open);
    assert_eq!(
        route.status().get("state").unwrap().as_str().unwrap(),
        "suspended"
    );
    permit.store(true, Ordering::Release);
    route.resume().unwrap();
    let current = route.state();
    assert!(!route.write_received(&[f32::NAN], current.generation));
    assert!(route.failed());
    assert_eq!(route.send_health(), AudioSendHealth::Failed);
    assert_eq!(route.resume(), Err(CallAudioError::RouteFailed));
    route.close().await.unwrap();
    buffers.lock().unwrap().fail();
    assert!(!route.failed());
    assert_eq!(
        route.status().get("state").unwrap().as_str().unwrap(),
        "closed"
    );
    save("pcm", &[plan.row("shared-lock-generation-pcm-send-status")]);
}

#[test]
fn missing_runtime_opens_no_backend_and_reports_not_ready() {
    let plan = Plan::new();
    let mut work = open(
        &plan,
        settings("microphone", false),
        None,
        CancellationToken::new(),
        Arc::new(AtomicBool::new(true)),
    );
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(matches!(
        work.as_mut().poll(&mut context),
        std::task::Poll::Ready(Err(CallAudioError::NotReady))
    ));
    assert!(plan.steps().is_empty());
    save("runtime", &[plan.row("runtime-unavailable-no-native-work")]);
}
