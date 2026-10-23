use super::*;
use crate::{
    desktop::{
        DesktopApplication, DesktopObservation, DesktopOwner, DesktopVoiceRuntime, KeyStroke,
        KeyboardInput, VoiceInputOwner,
    },
    media::{
        AudioRoutePermit, AudioSendHealth, AudioSettings, CallAudioError, CallAudioFactory,
        CallAudioRoute, PcmAudioPort, PcmAudioState, PcmSendPort,
    },
};
use ivy_contracts::json::stringify;
use std::{
    fs,
    path::Path,
    pin::Pin,
    sync::{Condvar, atomic::AtomicUsize},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
#[path = "phone_call_desktop_faults.rs"]
mod exception_policy;
const ID: &str = "11111111-aaaa-4111-8111-111111111111";
const OTHER: &str = "22222222-bbbb-4222-8222-222222222222";
const APP: &str = "Phone.Fixture!App";
fn n(v: usize) -> IvyJson {
    IvyJson::Number(v as f64)
}
fn arr(v: &[&str]) -> IvyJson {
    IvyJson::Array(v.iter().map(|s| (*s).into()).collect())
}
fn set(row: &mut IvyJson, key: &str, value: IvyJson) {
    let IvyJson::Object(fields) = row else {
        panic!()
    };
    fields.iter_mut().find(|(k, _)| k == &key.into()).unwrap().1 = value;
}
fn row(name: &str, audio: bool, actions: &[&str]) -> IvyJson {
    IvyJson::object([
        ("name", name.into()),
        ("id", ID.into()),
        ("audio", audio.into()),
        ("prepared", true.into()),
        ("current", true.into()),
        ("initialCapture", false.into()),
        ("application", "ready".into()),
        ("failure", "".into()),
        ("actions", arr(actions)),
    ])
}
fn cases() -> Vec<IvyJson> {
    let mut rows = vec![];
    for audio in [false, true] {
        for (name, actions) in [
            ("launch-once", vec!["launch", "launch", "close", "close"]),
            (
                "start-stop",
                vec!["start", "has", "stop", "stop", "close", "has"],
            ),
            (
                "stop-before-start",
                vec!["stop", "start", "resume:1", "close"],
            ),
            (
                "closed-admission",
                vec![
                    "close",
                    "launch",
                    "launch-badapp",
                    "start",
                    "stop",
                    "pause:0",
                    "resume:1",
                    "close",
                ],
            ),
            (
                "original-call",
                vec![
                    "launch-wrong-id",
                    "start-wrong-id",
                    "stop-wrong-id",
                    "pause-wrong-id:0",
                    "resume-wrong-id:1",
                    "close",
                ],
            ),
            (
                "invalid-application",
                vec!["launch-badapp", "launch", "close"],
            ),
            (
                "voice-transitions",
                vec![
                    "start", "pause:0", "has", "pause:0", "resume:1", "has", "pause:1", "resume:2",
                    "stop", "close",
                ],
            ),
            (
                "transition-identity",
                vec![
                    "start",
                    "pause:1",
                    "pause-wrong-desktop:0",
                    "pause-wrong-input:0",
                    "resume:0",
                    "pause:0",
                    "resume-wrong-desktop:1",
                    "resume-wrong-input:1",
                    "resume:2",
                    "resume:1",
                    "stop-wrong-desktop",
                    "stop-wrong-input",
                    "stop",
                    "close",
                ],
            ),
            (
                "generation-bounds",
                vec![
                    "start",
                    "pause:-1",
                    "pause:129",
                    "resume:-1",
                    "resume:129",
                    "close",
                ],
            ),
            (
                "case-insensitive-image",
                vec![
                    "start-case-image",
                    "pause-case-image:0",
                    "resume-case-image:1",
                    "stop-case-image",
                    "close",
                ],
            ),
            (
                "wrong-prepared-desktop",
                vec!["start-wrong-desktop", "start", "stop", "close"],
            ),
            (
                "retained-disposal",
                vec!["start", "close-drop", "close", "close", "has"],
            ),
        ] {
            rows.push(row(&format!("{name}-{audio}"), audio, &actions));
        }
    }
    for application in ["absent", "waiting"] {
        let mut c = row(
            &format!("launch-{application}"),
            false,
            &["launch", "close"],
        );
        set(&mut c, "application", application.into());
        rows.push(c);
    }
    for failure in ["factory-argument", "factory-error", "observe"] {
        let mut c = row(
            &format!("launch-{failure}"),
            false,
            &["launch", "launch", "close"],
        );
        set(&mut c, "failure", failure.into());
        rows.push(c);
    }
    for audio in [false, true] {
        let mut c = row(
            &format!("current-ended-{audio}"),
            audio,
            &["start", "launch", "pause:0", "stop", "close"],
        );
        set(&mut c, "current", false.into());
        rows.push(c);
    }
    let mut c = row(
        "unprepared-audio",
        true,
        &["start", "start", "stop", "close"],
    );
    set(&mut c, "prepared", false.into());
    rows.push(c);
    for adopt in [false, true] {
        let mut c = row(
            &format!("existing-capture-{adopt}"),
            true,
            if adopt {
                &[
                    "start-adopt",
                    "has",
                    "pause-adopt:0",
                    "resume-adopt:1",
                    "stop-adopt",
                    "close",
                ]
            } else {
                &["start", "has", "pause:0", "stop", "close"]
            },
        );
        set(&mut c, "initialCapture", true.into());
        rows.push(c);
    }
    for id in [
        "bad",
        "{11111111-aaaa-4111-8111-111111111111}",
        " 11111111-AAAA-4111-8111-111111111111 ",
    ] {
        let mut c = row(&format!("constructor-{id}"), false, &["launch", "close"]);
        set(&mut c, "id", id.into());
        rows.push(c);
    }
    rows
}
struct Context {
    row: IvyJson,
    capture: AtomicBool,
    chords: AtomicUsize,
    factories: AtomicUsize,
    activations: AtomicUsize,
    disposed: AtomicUsize,
    hold: bool,
    entered: (Mutex<bool>, Condvar),
    release: (Mutex<bool>, Condvar),
}
impl Context {
    fn new(row: IvyJson, hold: bool) -> Arc<Self> {
        Arc::new(Self {
            capture: AtomicBool::new(row.get("initialCapture").unwrap().as_bool().unwrap()),
            row,
            chords: AtomicUsize::new(0),
            factories: AtomicUsize::new(0),
            activations: AtomicUsize::new(0),
            disposed: AtomicUsize::new(0),
            hold,
            entered: (Mutex::new(false), Condvar::new()),
            release: (Mutex::new(false), Condvar::new()),
        })
    }
    fn text(&self, key: &str) -> String {
        self.row.get(key).unwrap().as_str().unwrap()
    }
    fn flag(&self, key: &str) -> bool {
        self.row.get(key).unwrap().as_bool().unwrap()
    }
    fn count(&self, v: &AtomicUsize) -> IvyJson {
        n(v.load(Ordering::Acquire))
    }
    fn enter(&self) {
        *self.entered.0.lock().unwrap() = true;
        self.entered.1.notify_all();
        let guard = self.release.0.lock().unwrap();
        let (guard, _) = self
            .release
            .1
            .wait_timeout_while(guard, Duration::from_secs(5), |ready| !*ready)
            .unwrap();
        assert!(*guard, "fixture bound");
    }
    fn wait_entered(&self) {
        let guard = self.entered.0.lock().unwrap();
        let (guard, _) = self
            .entered
            .1
            .wait_timeout_while(guard, Duration::from_secs(5), |ready| !*ready)
            .unwrap();
        assert!(*guard, "factory did not enter");
    }
    fn release(&self) {
        *self.release.0.lock().unwrap() = true;
        self.release.1.notify_all();
    }
}
struct Application(Arc<Context>);
impl DesktopApplication for Application {
    fn observe(&mut self) -> ActionResult<Option<DesktopObservation>> {
        if self.0.text("failure") == "observe" {
            return Err(DesktopActionError::Unavailable);
        }
        Ok(Some(match self.0.text("application").as_str() {
            "ready" => DesktopObservation::select(true, vec![identity(false, false)]),
            "waiting" => DesktopObservation::select(true, vec![]),
            _ => DesktopObservation::select(false, vec![]),
        }))
    }
    fn activate(&mut self) -> ActionResult<i32> {
        self.0.activations.fetch_add(1, Ordering::AcqRel);
        Ok(101)
    }
}
struct Owner;
impl DesktopOwner for Owner {
    fn is_current(&mut self) -> ActionResult<bool> {
        Ok(true)
    }
    fn try_focus(&mut self) -> ActionResult<bool> {
        Ok(true)
    }
    fn is_focused(&mut self) -> ActionResult<bool> {
        Ok(true)
    }
}
struct Keyboard(Arc<Context>);
impl KeyboardInput for Keyboard {
    fn is_idle(&mut self) -> ActionResult<bool> {
        Ok(true)
    }
    fn send(&mut self, keys: &[KeyStroke]) -> ActionResult<i32> {
        if keys.len() == 4 {
            self.0.chords.fetch_add(1, Ordering::AcqRel);
            self.0.capture.fetch_xor(true, Ordering::AcqRel);
        }
        Ok(keys.len() as i32)
    }
}
struct Route {
    ctx: Arc<Context>,
    state: Mutex<PcmAudioState>,
}
impl PcmAudioPort for Route {
    fn state(&self) -> PcmAudioState {
        *self.state.lock().unwrap()
    }
    fn read_captured(&self, out: &mut [f32], _: u64) -> usize {
        out.fill(0.0);
        0
    }
    fn write_received(&self, _: &[f32], _: u64) -> bool {
        false
    }
}
impl PcmSendPort for Route {
    fn with_send_state(&self, state: PcmAudioState, action: &mut dyn FnMut()) -> bool {
        let current = self.state.lock().unwrap();
        if *current != state {
            return false;
        }
        action();
        true
    }
    fn send_health(&self) -> AudioSendHealth {
        AudioSendHealth::Ready
    }
}
impl CallAudioRoute for Route {
    fn status(&self) -> IvyJson {
        IvyJson::object([(
            "state",
            if self.state().open {
                "open"
            } else {
                "suspended"
            }
            .into(),
        )])
    }
    fn failed(&self) -> bool {
        false
    }
    fn resume(&self) -> std::result::Result<(), CallAudioError> {
        let mut s = self.state.lock().unwrap();
        s.open = true;
        s.generation += 1;
        Ok(())
    }
    fn suspend(&self) -> std::result::Result<(), CallAudioError> {
        self.state.lock().unwrap().open = false;
        Ok(())
    }
    fn close(
        &self,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<(), CallAudioError>> + Send + '_>> {
        self.state.lock().unwrap().open = false;
        self.ctx.disposed.fetch_add(1, Ordering::AcqRel);
        Box::pin(async { Ok(()) })
    }
}
struct Factory(Arc<Context>);
impl CallAudioFactory for Factory {
    fn open<'a>(
        &'a self,
        _: &'a AudioSettings,
        _: Option<&'a AudioDesktopIdentity>,
        _: AudioRoutePermit,
        _: CancellationToken,
    ) -> Pin<
        Box<
            dyn Future<Output = std::result::Result<Arc<dyn CallAudioRoute>, CallAudioError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            Ok(Arc::new(Route {
                ctx: self.0.clone(),
                state: Mutex::new(PcmAudioState {
                    open: false,
                    generation: 0,
                }),
            }) as Arc<dyn CallAudioRoute>)
        })
    }
}
fn identity(other: bool, case: bool) -> AudioDesktopIdentity {
    AudioDesktopIdentity::new(
        if other { 102 } else { 101 },
        1,
        Path::new(if case {
            r"C:\FIXTURE\VOICE.EXE"
        } else {
            r"C:\fixture\voice.exe"
        }),
        APP,
    )
    .unwrap()
}
fn input(wrong: bool, adopt: bool) -> NativeVoiceInput {
    NativeVoiceInput::new(
        None,
        Some(
            crate::desktop::NativeHotkey::new(
                vec!["control".into()],
                if wrong { 65 } else { 32 },
                false,
            )
            .unwrap(),
        ),
        adopt,
    )
}
fn action(token: &str) -> DesktopCallAction {
    let name = token.split(':').next().unwrap();
    let desktop = identity(token.contains("desktop"), token.contains("case-image"));
    let input = input(token.contains("input"), token.contains("adopt"));
    if name.starts_with("launch") {
        DesktopCallAction::Launch {
            application: if token.contains("badapp") { "bad" } else { APP }.into(),
            start_if_missing: true,
        }
    } else if name.starts_with("start") {
        DesktopCallAction::StartVoice { desktop, input }
    } else if name.starts_with("stop") {
        DesktopCallAction::StopVoice { desktop, input }
    } else if name.starts_with("pause") {
        DesktopCallAction::PauseVoice {
            desktop,
            input,
            generation: token.split(':').nth(1).unwrap().parse().unwrap(),
        }
    } else {
        DesktopCallAction::ResumeVoice {
            desktop,
            input,
            generation: token.split(':').nth(1).unwrap().parse().unwrap(),
        }
    }
}
fn runtime(ctx: &Arc<Context>) -> (PhoneDesktopRuntime, Arc<dyn CallVoiceRuntime>) {
    let desktop = PhoneDesktopRuntime::with_application({
        let ctx = ctx.clone();
        move |_| {
            ctx.factories.fetch_add(1, Ordering::AcqRel);
            if ctx.hold {
                ctx.enter()
            }
            match ctx.text("failure").as_str() {
                "factory-argument" => Err(DesktopActionError::Argument("fixture")),
                "factory-error" => Err(DesktopActionError::Unavailable),
                _ => Ok(Some(Box::new(Application(ctx.clone())))),
            }
        }
    })
    .with_capture_owner({
        let ctx = ctx.clone();
        move |_, endpoint| {
            let captured = ctx.capture.load(Ordering::Acquire);
            Ok(Some(crate::desktop::DesktopCaptureCorrelation {
                endpoint_id: endpoint.clone(),
                state: if captured { "matched" } else { "none" },
                identity: if captured {
                    Some(crate::desktop::DesktopCaptureIdentity {
                        instance_id: "session".into(),
                        pid: 202,
                        process_start_ticks: 42,
                    })
                } else {
                    None
                },
            }))
        }
    });
    let voice = DesktopVoiceRuntime::new(
        desktop.clone(),
        VoiceInputOwner::new(|_| panic!("No Micro effect authorized")),
    )
    .with_owner(|_| Ok(Box::new(Owner)))
    .with_keyboard({
        let ctx = ctx.clone();
        move || Ok(Box::new(Keyboard(ctx.clone())))
    })
    .with_exited(|_| Ok(false));
    (desktop, Arc::new(voice))
}
async fn audio(ctx: &Arc<Context>) -> Option<CallAudioPort> {
    if !ctx.flag("audio") {
        return None;
    }
    let port = CallAudioPort::new(ctx.text("id"), Arc::new(Factory(ctx.clone()))).unwrap();
    if ctx.flag("prepared") {
        port.prepare(
            AudioSettings {
                capture_endpoint: "capture-fixture".into(),
                render_endpoint: "render-fixture".into(),
                source_mode: "desktop_process".into(),
                capture_buffer_ms: 20,
                render_latency_ms: 10,
                queue_ms: 60,
                mute: None,
                playback_prebuffer_ms: 0,
                enable_wasapi_low_latency: false,
                loopback_render_endpoint: None,
            },
            Some(identity(false, false)),
            CancellationToken::new(),
        )
        .unwrap()
        .wait()
        .await
        .unwrap();
    }
    Some(port)
}
fn snapshot(owner: &CallDesktopSession) -> IvyJson {
    let state = owner.state();
    IvyJson::object([
        ("closed", owner.0.closed.load(Ordering::Acquire).into()),
        ("paused", state.paused.into()),
        ("generation", IvyJson::Number(state.generation.into())),
        (
            "work",
            IvyJson::Array(
                state
                    .work
                    .iter()
                    .map(|(key, _)| key.as_str().into())
                    .collect(),
            ),
        ),
        ("voice", state.voice.is_some().into()),
    ])
}
fn outcome(result: Result<IvyJson>) -> IvyJson {
    let code = result
        .as_ref()
        .err()
        .map(|e| e.code().into())
        .unwrap_or(IvyJson::Null);
    IvyJson::object([
        ("ok", result.is_ok().into()),
        ("value", result.unwrap_or(IvyJson::Null)),
        ("code", code),
    ])
}
async fn run_case(row: IvyJson) -> IvyJson {
    let ctx = Context::new(row, false);
    let port = audio(&ctx).await;
    let (runtime, voice) = runtime(&ctx);
    let current = ctx.clone();
    let owner = CallDesktopSession::new(
        ctx.text("id"),
        runtime,
        voice,
        Arc::new(move |_| Ok(current.flag("current"))),
        port.clone(),
    );
    let constructed = owner.is_ok();
    let construction = outcome(owner.as_ref().map(|_| IvyJson::Null).map_err(Clone::clone));
    let mut actions = vec![];
    if let Ok(owner) = owner {
        let mut closing: Option<DesktopCallOperation<()>> = None;
        let id = ctx.text("id");
        for token in ctx.row.get("actions").unwrap().as_array().unwrap() {
            let token = token.as_str().unwrap();
            let result = if token.starts_with("close") {
                let pending = owner.close();
                let same = closing.as_ref().is_some_and(|p| p.same_work(&pending));
                closing = Some(pending.clone());
                if token == "close-drop" {
                    drop(pending);
                    Ok(IvyJson::object([("queued", true.into())]))
                } else {
                    pending.wait().await.map(|_| {
                        IvyJson::object([("same", same.into()), ("complete", true.into())])
                    })
                }
            } else if token == "has" {
                Ok(owner
                    .has_original_voice_capture(&identity(false, false))
                    .into())
            } else {
                match owner.invoke(
                    Some(if token.contains("wrong-id") {
                        OTHER
                    } else {
                        &id
                    }),
                    action(&token),
                ) {
                    Ok(work) => {
                        if token.ends_with("-drop") {
                            drop(work);
                            Ok(IvyJson::object([("queued", true.into())]))
                        } else {
                            work.wait().await
                        }
                    }
                    Err(error) => Err(error),
                }
            };
            actions.push(IvyJson::object([
                ("action", token.into()),
                ("result", outcome(result)),
                ("snapshot", snapshot(&owner)),
            ]));
        }
        owner.close().wait().await.unwrap();
    }
    if let Some(port) = port {
        port.close().wait().await.unwrap();
    }
    IvyJson::object([
        ("name", ctx.text("name").into()),
        ("constructed", constructed.into()),
        ("construction", construction),
        ("actions", IvyJson::Array(actions)),
        ("chords", ctx.count(&ctx.chords)),
        ("factories", ctx.count(&ctx.factories)),
        ("activations", ctx.count(&ctx.activations)),
        ("routeDisposals", ctx.count(&ctx.disposed)),
    ])
}
async fn held() -> IvyJson {
    let ctx = Context::new(row("held", false, &[]), true);
    let (runtime, voice) = runtime(&ctx);
    let owner =
        CallDesktopSession::new(ID.into(), runtime, voice, Arc::new(|_| Ok(true)), None).unwrap();
    let launch = owner.invoke(Some(ID), action("launch")).unwrap();
    ctx.wait_entered();
    let duplicate = owner
        .invoke(Some(ID), action("launch"))
        .err()
        .unwrap()
        .code();
    let blocked = owner
        .invoke(Some(ID), action("start"))
        .err()
        .unwrap()
        .code();
    let stopped = owner
        .invoke(Some(ID), action("stop"))
        .unwrap()
        .wait()
        .await
        .unwrap();
    let closing = owner.close();
    let early = closing.result().is_some();
    ctx.release();
    let launch = launch.wait().await.unwrap();
    closing.wait().await.unwrap();
    IvyJson::object([
        ("duplicate", duplicate.into()),
        ("blocked", blocked.into()),
        ("stopped", stopped),
        ("closeBeforeRelease", early.into()),
        ("launch", launch),
        ("closed", snapshot(&owner)),
        ("chords", ctx.count(&ctx.chords)),
        ("factories", ctx.count(&ctx.factories)),
    ])
}
fn save(name: &str, value: &IvyJson) {
    if let Some(dir) = std::env::var_os("IVY_PHONE_CALL_DESKTOP_TRACE") {
        fs::write(
            Path::new(&dir).join(name),
            stringify(value, 8 * 1024 * 1024).unwrap(),
        )
        .unwrap();
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn original_call_admission_generations_retained_voice_and_drain() {
    let mut rows = vec![];
    for case in cases() {
        rows.push(run_case(case).await)
    }
    assert!(rows.len() > 30);
    let held = held().await;
    assert_eq!(held.get("closeBeforeRelease").unwrap(), &false.into());
    save(
        "call-desktop-ordinary.json",
        &IvyJson::object([("rows", IvyJson::Array(rows)), ("held", held)]),
    );
}
