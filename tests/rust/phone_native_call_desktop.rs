use super::*;
use ivy_phone_bridge::desktop::{
    ActionResult, DesktopApplication, DesktopCaptureCorrelation, DesktopCaptureIdentity,
    DesktopObservation, DesktopOwner, DesktopVoiceRuntime, KeyStroke, KeyboardInput,
    PhoneDesktopRuntime, VoiceInputOwner,
};
use ivy_phone_bridge::ipc::FRAME_BYTES;
use std::{fs, path::Path};
#[path = "phone_native_call_desktop_faults.rs"]
mod exception_policy;
const APP: &str = "Phone.Fixture!App";
#[derive(Default)]
pub(super) struct Context {
    pub(super) capture: AtomicBool,
    pub(super) chords: AtomicUsize,
    applications: AtomicUsize,
    activations: AtomicUsize,
    hold: Option<Arc<CaptureGate>>,
    image: String,
}
struct Application(Arc<Context>);
impl DesktopApplication for Application {
    fn observe(&mut self) -> ActionResult<Option<DesktopObservation>> {
        Ok(Some(DesktopObservation::select(
            true,
            vec![identity(&self.0.image)],
        )))
    }
    fn activate(&mut self) -> ActionResult<i32> {
        self.0.activations.fetch_add(1, Ordering::SeqCst);
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
            self.0.chords.fetch_add(1, Ordering::SeqCst);
            self.0.capture.fetch_xor(true, Ordering::SeqCst);
        }
        Ok(keys.len() as i32)
    }
}
pub(super) fn identity(image: &str) -> AudioDesktopIdentity {
    AudioDesktopIdentity::new(
        101,
        1,
        Path::new(if image.is_empty() {
            r"C:\fixture\voice.exe"
        } else {
            image
        }),
        APP,
    )
    .unwrap()
}
pub(super) fn runtime(ctx: Arc<Context>) -> (PhoneDesktopRuntime, Arc<DesktopVoiceRuntime>) {
    let desktop = PhoneDesktopRuntime::with_application({
        let ctx = ctx.clone();
        move |_| {
            ctx.applications.fetch_add(1, Ordering::SeqCst);
            if let Some(g) = &ctx.hold {
                g.block()
            }
            Ok(Some(Box::new(Application(ctx.clone()))))
        }
    })
    .with_capture_owner({
        let ctx = ctx.clone();
        move |_, endpoint| {
            let captured = ctx.capture.load(Ordering::SeqCst);
            Ok(Some(DesktopCaptureCorrelation {
                endpoint_id: endpoint.clone(),
                state: if captured { "matched" } else { "none" },
                identity: captured.then(|| DesktopCaptureIdentity {
                    instance_id: "session".into(),
                    pid: 202,
                    process_start_ticks: 42,
                }),
            }))
        }
    });
    let voice = DesktopVoiceRuntime::new(
        desktop.clone(),
        VoiceInputOwner::new(|_| panic!("No Micro effect authorized")),
    )
    .with_owner(|_| Ok(Box::new(Owner)))
    .with_keyboard(move || Ok(Box::new(Keyboard(ctx.clone()))))
    .with_exited(|_| Ok(false));
    (desktop, Arc::new(voice))
}
pub(super) fn params(method: &str) -> String {
    if method == "call.desktop.launch" {
        format!(
            r#"{{"callId":"{CALL}","application":{{"appUserModelId":"{APP}","startIfMissing":true}}}}"#
        )
    } else {
        format!(
            r#"{{"callId":"{CALL}","desktop":{{"pid":101,"startTimeUtcTicks":"1","imagePath":"C:\\fixture\\voice.exe","appUserModelId":"{APP}"}},"voiceInput":{{"micro":null,"hotkeyFallback":{{"modifiers":["control"],"keyCode":32,"focusBeforeHotkey":false}},"adoptExisting":false}}{}}}"#,
            if method.contains("pause") {
                ",\"generation\":0"
            } else if method.contains("resume") {
                ",\"generation\":1"
            } else {
                ""
            }
        )
    }
}
fn item(method: &str, params: String) -> IvyJson {
    IvyJson::object([("method", method.into()), ("params", params.into())])
}
fn scenario(name: &str, stage: &str, audio: bool, actions: Vec<IvyJson>) -> IvyJson {
    IvyJson::object([
        ("name", name.into()),
        ("stage", stage.into()),
        ("audio", audio.into()),
        ("image", "".into()),
        ("actions", IvyJson::Array(actions)),
    ])
}
fn cases() -> Vec<IvyJson> {
    let methods = [
        "call.desktop.launch",
        "call.desktop.startVoice",
        "call.desktop.stopVoice",
        "call.desktop.pauseVoice",
        "call.desktop.resumeVoice",
    ];
    let mut rows = vec![];
    for stage in ["unconfigured", "prepared", "ended", "closed"] {
        let mut steps = vec![];
        for method in methods {
            for p in [
                "{}".into(),
                "{\"callId\":null}".into(),
                "{\"callId\":12}".into(),
                "{\"callId\":\"bad\"}".into(),
                "{\"callId\":\"22222222-2222-4222-8222-222222222222\"}".into(),
                params(method).replace(CALL, &CALL.to_uppercase()),
            ] {
                steps.push(item(method, p));
            }
        }
        rows.push(scenario(stage, stage, false, steps));
    }
    let mut invalid = vec![];
    for method in methods {
        let p = params(method);
        for raw in [
            p.replacen("\"callId\"", "\"CallId\"", 1),
            p.replacen("{", "{\"extra\":1,", 1),
            p.replace("\"pid\":101", "\"pid\":101.0"),
            p.replace("\"pid\":101", "\"pid\":101e0"),
            p.replace("\"pid\":101", "\"pid\":null"),
            p.replace(
                "\"startTimeUtcTicks\":\"1\"",
                "\"startTimeUtcTicks\":\"01\"",
            ),
            p.replace("\"micro\":null", "\"micro\":3"),
            p.replace("\"adoptExisting\":false", "\"adoptExisting\":null"),
            p.replace("\"keyCode\":32", "\"keyCode\":32.0"),
        ] {
            invalid.push(item(method, raw));
        }
    }
    for raw in [
        "null",
        "3",
        "[]",
        "{}",
        r#"{"appUserModelId":null,"startIfMissing":true}"#,
        r#"{"appUserModelId":"bad","startIfMissing":true}"#,
        r#"{"appUserModelId":"Phone.Fixture!App","startIfMissing":null}"#,
    ] {
        invalid.push(item(
            "call.desktop.launch",
            format!(r#"{{"callId":"{CALL}","application":{raw}}}"#),
        ));
    }
    rows.push(scenario("raw-dto-order", "prepared", false, invalid));
    let mut steps = vec![item(
        "call.desktop.startVoice",
        params("call.desktop.startVoice"),
    )];
    for method in ["call.desktop.pauseVoice", "call.desktop.resumeVoice"] {
        let p = params(method);
        let original = if method.contains("pause") {
            ":0}"
        } else {
            ":1}"
        };
        for raw in [
            "null",
            "1.0",
            "1e0",
            "2147483648",
            "\"0\"",
            "[]",
            "true",
            "-1",
            "129",
        ] {
            steps.push(item(method, p.replace(original, &format!(":{raw}}}"))));
        }
        steps.push(item(method, p.replace("\"pid\":101", "\"pid\":102")));
    }
    steps.extend([
        item("call.desktop.pauseVoice", params("call.desktop.pauseVoice")),
        item(
            "call.desktop.resumeVoice",
            params("call.desktop.resumeVoice"),
        ),
        item("call.desktop.stopVoice", params("call.desktop.stopVoice")),
    ]);
    rows.push(scenario("active-generations", "prepared", true, steps));
    rows.push(scenario(
        "unprepared-constructor",
        "prepared",
        false,
        vec![
            item("call.desktop.startVoice", params("call.desktop.startVoice")),
            item("call.desktop.startVoice", params("call.desktop.startVoice")),
            item("call.desktop.stopVoice", params("call.desktop.stopVoice")),
        ],
    ));
    let mut large = scenario(
        "native-output-byte-limit",
        "prepared",
        false,
        vec![item("call.desktop.launch", params("call.desktop.launch"))],
    );
    let IvyJson::Object(fields) = &mut large else {
        unreachable!()
    };
    fields
        .iter_mut()
        .find(|(k, _)| k == &"image".into())
        .unwrap()
        .1 = format!("C:\\fixture\\{}.exe", "é".repeat(30000)).into();
    rows.push(large);
    rows
}
fn response(out: &NativeOutcome) -> IvyJson {
    IvyJson::object([
        ("ok", out.error_code().is_none().into()),
        (
            "result",
            out.result_json()
                .map(|v| parse(v, FRAME_BYTES).unwrap())
                .unwrap_or(IvyJson::Null),
        ),
        (
            "error",
            out.error_code().map(Into::into).unwrap_or(IvyJson::Null),
        ),
        (
            "raw",
            out.result_json().map(Into::into).unwrap_or(IvyJson::Null),
        ),
    ])
}
async fn run_case(row: &IvyJson) -> IvyJson {
    let ctx = Arc::new(Context {
        image: row.get("image").unwrap().as_str().unwrap(),
        ..Default::default()
    });
    let (desktop, voice) = runtime(ctx.clone());
    let route = Arc::new(Route::new());
    let routes = Arc::new(Factory::routes(vec![route.clone()]));
    let media = Arc::new(NativeSipSessionFactory::new(
        routes,
        None,
        Arc::new(Clock),
        |_| Arc::new(Events::default()),
    ));
    let owner = NativeSipOperations::with_desktop_runtime(media, desktop, voice);
    let remaining = Arc::new(Remaining::default());
    remaining.close_gate.release();
    let handler = Arc::new(NativeSipCommands::new(
        owner.clone(),
        remaining.clone(),
        |_| Box::pin(async { Ok(()) }),
        NativeSipViews::new(|| Ok("null".into()), |_| Ok("null".into())),
    ));
    let dispatcher = NativeDispatcher::new(CALL.into(), 1024, handler.clone()).unwrap();
    let stage = row.get("stage").unwrap().as_str().unwrap();
    if stage != "unconfigured" {
        owner
            .configure(&configuration(BASE), |_| async { Ok(()) })
            .unwrap()
            .wait()
            .await
            .unwrap();
        let call = owner.prepare(CALL.into()).unwrap();
        call.prepared().await.unwrap();
        if row.get("audio").unwrap().as_bool().unwrap() {
            let mut settings = settings();
            settings.source_mode = "desktop_process".into();
            call.media()
                .unwrap()
                .prepare_audio(settings, Some(identity("")), CancellationToken::new())
                .unwrap()
                .wait()
                .await
                .unwrap();
        }
        if stage == "ended" {
            owner.hangup(&call).unwrap();
        }
    }
    if stage == "closed" {
        owner.close().wait().await.unwrap();
    }
    let mut responses = vec![];
    for (i, step) in row
        .get("actions")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let method = step.get("method").unwrap().as_str().unwrap();
        let p = step.get("params").unwrap().as_str().unwrap();
        let out = dispatcher
            .execute(command(&method, &p, (i + 1) as u64, (i + 1) as u64))
            .await;
        responses.push(response(out.outcome()));
    }
    handler.shutdown().await.unwrap();
    assert!(remaining.methods.lock().unwrap().is_empty());
    let s = route.state.lock().unwrap();
    IvyJson::object([
        ("name", row.get("name").unwrap().clone()),
        ("responses", IvyJson::Array(responses)),
        ("applications", n(ctx.applications.load(Ordering::SeqCst))),
        ("activations", n(ctx.activations.load(Ordering::SeqCst))),
        ("chords", n(ctx.chords.load(Ordering::SeqCst))),
        ("routeDisposals", n(s.disposals)),
    ])
}
fn save(name: &str, value: &IvyJson) {
    if let Some(dir) = std::env::var_os("IVY_PHONE_NATIVE_CALL_DESKTOP_TRACE") {
        fs::write(
            Path::new(&dir).join(name),
            stringify(value, 8 * 1024 * 1024).unwrap(),
        )
        .unwrap();
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_desktop_dto_receipts_and_retirement() {
    let mut rows = vec![];
    for c in cases() {
        rows.push(run_case(&c).await);
    }
    assert_eq!(rows.len(), 8);
    save("native-call-desktop-ordinary.json", &IvyJson::Array(rows));
    retained_drain().await;
}
async fn retained_drain() {
    let gate = Arc::new(CaptureGate::default());
    let ctx = Arc::new(Context {
        hold: Some(gate.clone()),
        ..Default::default()
    });
    let (desktop, voice) = runtime(ctx.clone());
    let (_, factory, _, _) = actual_owner(43000, 43998);
    let owner = NativeSipOperations::with_desktop_runtime(factory.clone(), desktop, voice);
    owner
        .configure(&configuration(BASE), |_| async { Ok(()) })
        .unwrap()
        .wait()
        .await
        .unwrap();
    let call = owner.prepare(CALL.into()).unwrap();
    call.prepared().await.unwrap();
    let remaining = Arc::new(Remaining::default());
    remaining.close_gate.release();
    let handler = Arc::new(NativeSipCommands::new(
        owner.clone(),
        remaining,
        |_| Box::pin(async { Ok(()) }),
        closed_platform_views(),
    ));
    let dispatcher = NativeDispatcher::new(CALL.into(), 64, handler.clone()).unwrap();
    let p = params("call.desktop.launch");
    cancel(dispatcher.execute(command("call.desktop.launch", &p, 1, 1))).await;
    until(|| gate.entered.load(Ordering::SeqCst)).await;
    let stopped = dispatcher
        .execute(command(
            "call.desktop.stopVoice",
            &params("call.desktop.stopVoice"),
            2,
            2,
        ))
        .await;
    assert!(stopped.outcome().error_code().is_none());
    let release = owner.release(&call).unwrap();
    sleep(Duration::from_millis(20)).await;
    assert!(release.result().is_none());
    assert!(matches!(
        owner.prepare(CALL.into()),
        Err(SipHostError::OperationConflict)
    ));
    gate.release();
    release.wait().await.unwrap();
    let replay = dispatcher
        .execute(command("call.desktop.launch", &p, 1, 3))
        .await;
    assert!(replay.outcome().error_code().is_none());
    assert_eq!(ctx.applications.load(Ordering::SeqCst), 1);
    let replacement = owner.prepare(CALL.into()).unwrap();
    replacement.prepared().await.unwrap();
    assert_eq!(factory.starts.load(Ordering::SeqCst), 2);
    handler.shutdown().await.unwrap();
    save(
        "native-call-desktop-retained.json",
        &IvyJson::object([
            ("originalFactory", n(1)),
            ("replacementFactories", n(2)),
            ("releaseWaitedForDesktop", true.into()),
            ("replayedAfterObserverLoss", true.into()),
        ]),
    );
}
