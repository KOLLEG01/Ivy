use super::*;
use crate::{
    desktop::{
        ActionResult, Control, ControlWindow, ControlsObservation, DesktopApplication,
        DesktopObservation,
    },
    media::AudioDesktopIdentity,
    native_rpc::NativeDispatcher,
    native_sip::{NativeCallConfiguration, NativeSipMediaFactory, NativeSipMediaFuture},
    native_sip_commands::NativeSipCommands,
    native_sip_status::NativeSipViews,
};
use ivy_contracts::json::parse;
#[path = "phone_native_capture.rs"]
mod capture;
#[path = "phone_native_process.rs"]
mod process_commands;
#[path = "phone_process_ownership.rs"]
mod process_ownership;
use std::{
    fs,
    path::Path,
    sync::{atomic::AtomicUsize, mpsc},
    time::Duration,
};

const APP: &str = "Phone.Fixture!App";
const EPOCH: &str = "11111111-1111-4111-8111-111111111111";
const OP: &str = "22222222-2222-4222-8222-222222222222";
#[derive(Clone)]
struct Case {
    method: &'static str,
    params: Vec<String>,
    state: &'static str,
    failure: &'static str,
    close_at: &'static str,
    closed: bool,
    pid: i32,
    image: String,
}
impl Case {
    fn new(method: &'static str, params: &str) -> Self {
        Self {
            method,
            params: vec![params.into()],
            state: "absent",
            failure: "",
            close_at: "",
            closed: false,
            pid: 101,
            image: String::new(),
        }
    }
}
struct NoMedia;
impl NativeSipMediaFactory for NoMedia {
    fn create(&self, _: String, _: Arc<NativeCallConfiguration>) -> NativeSipMediaFuture {
        panic!("Desktop fixture must not construct media")
    }
}
struct Remaining {
    closes: AtomicUsize,
    fail: bool,
}
impl NativeHandler for Remaining {
    fn invoke(&self, _: NativeRequest) -> NativeInvocation {
        Box::pin(async { NativeOutcome::result("3".into()) })
    }
    fn cleanup_slots(&self) -> usize {
        4
    }
}
impl NativeRuntime for Remaining {
    fn shutdown(&self) -> NativeShutdown {
        self.closes.fetch_add(1, Ordering::SeqCst);
        let fail = self.fail;
        Box::pin(async move {
            if fail {
                Err(NativeShutdownError::OutcomeUnknown)
            } else {
                Ok(())
            }
        })
    }
}
struct Application {
    case: Case,
    trace: Arc<Mutex<Vec<String>>>,
    sip: NativeSipOperations,
    done: Option<mpsc::Sender<()>>,
}
impl Application {
    fn close(&self, at: &str) {
        if self.case.close_at == at {
            drop(self.sip.close())
        }
    }
}
impl Drop for Application {
    fn drop(&mut self) {
        if let Some(done) = self.done.take() {
            let _ = done.send(());
        }
    }
}
impl DesktopApplication for Application {
    fn observe(&mut self) -> ActionResult<Option<DesktopObservation>> {
        self.trace.lock().unwrap().push("observe".into());
        self.close("observe");
        match self.case.failure {
            "observe-argument" => return Err(DesktopActionError::Argument("fixture")),
            "observe-error" => return Err(DesktopActionError::Unavailable),
            "observe-null" => return Ok(None),
            _ => {}
        }
        let image = if self.case.image == "unpaired" {
            use std::os::windows::ffi::OsStringExt;
            std::ffi::OsString::from_wide(
                &[
                    r"C:\Fixture\".encode_utf16().collect::<Vec<_>>(),
                    vec![0xd800],
                    ".exe".encode_utf16().collect(),
                ]
                .concat(),
            )
        } else if self.case.image.is_empty() {
            r"C:\Fixture\ChatGPT.exe".into()
        } else {
            self.case.image.clone().into()
        };
        let identity = |pid| {
            AudioDesktopIdentity::new(pid, 504_911_232_000_001_234, Path::new(&image), APP).unwrap()
        };
        Ok(Some(match self.case.state {
            "ready" => DesktopObservation::select(true, vec![identity(101)]),
            "waiting" => DesktopObservation::select(true, vec![]),
            "ambiguous" => DesktopObservation::select(true, vec![identity(101), identity(102)]),
            "unavailable" => DesktopObservation::unavailable(),
            _ => DesktopObservation::select(false, vec![]),
        }))
    }
    fn activate(&mut self) -> ActionResult<i32> {
        self.trace.lock().unwrap().push("activate".into());
        self.close("activate");
        match self.case.failure {
            "activate-argument" => Err(DesktopActionError::Argument("fixture")),
            "activate-error" => Err(DesktopActionError::Unavailable),
            _ => Ok(self.case.pid),
        }
    }
}
fn request(method: &str, params: &str, id: usize) -> NativeRequest {
    NativeRequest::parse(format!(r#"{{"version":1,"epoch":"{EPOCH}","requestId":{id},"operationId":{},"method":"{method}","params":{params}}}"#,
        if crate::ipc::is_read_only(method) { "null".into() } else { format!("\"{OP}\"") }).as_bytes()).unwrap()
}
fn controls_observation(case: &Case, labels: &[JsString]) -> Option<ControlsObservation> {
    if case.failure == "controls-null" {
        return None;
    }
    let count = if case.state == "many" {
        32
    } else if case.state == "empty" || case.state == "unavailable" {
        0
    } else {
        1
    };
    Some(ControlsObservation {
        observed: case.state != "unavailable",
        windows: (0..count)
            .map(|w| ControlWindow {
                handle: (41 + w).to_string().into(),
                minimized: case.state == "minimized",
                controls: (0..if case.state == "many" {
                    64
                } else {
                    labels.len()
                })
                    .map(|i| Control {
                        id: vec![w as i32 + 1, i as i32 + 2],
                        parent_id: vec![w as i32 + 1],
                        name: if case.image == "unpaired" {
                            JsString(vec![0xd800])
                        } else if case.image.is_empty() {
                            labels[i % labels.len()].clone()
                        } else {
                            case.image.clone().into()
                        },
                        enabled: i % 2 == 0,
                        offscreen: i % 2 != 0,
                        toggle: if i % 4 == 3 { None } else { Some(i as i32 % 4) },
                    })
                    .collect(),
            })
            .collect(),
    })
}
fn projection(outcome: &NativeOutcome) -> IvyJson {
    IvyJson::object([
        ("ok", outcome.error_code().is_none().into()),
        (
            "result",
            outcome
                .result_json()
                .map(|v| parse(v, 65536).unwrap())
                .unwrap_or(IvyJson::Null),
        ),
        (
            "error",
            outcome
                .error_code()
                .map(Into::into)
                .unwrap_or(IvyJson::Null),
        ),
        (
            "raw",
            outcome
                .result_json()
                .map(Into::into)
                .unwrap_or(IvyJson::Null),
        ),
    ])
}
async fn run_case(case: &Case) -> IvyJson {
    let sip = NativeSipOperations::new(Arc::new(NoMedia));
    let trace = Arc::new(Mutex::new(vec![]));
    let desktop = PhoneDesktopRuntime::with_application({
        let case = case.clone();
        let sip = sip.clone();
        let trace = trace.clone();
        move |id| {
            trace
                .lock()
                .unwrap()
                .push(format!("factory:{}", id.unwrap_or("<null>")));
            if case.close_at == "factory" {
                drop(sip.close())
            }
            match case.failure {
                "factory-argument" => Err(DesktopActionError::Argument("fixture")),
                "factory-error" => Err(DesktopActionError::Unavailable),
                "factory-null" => Ok(None),
                _ => Ok(Some(Box::new(Application {
                    case: case.clone(),
                    sip: sip.clone(),
                    trace: trace.clone(),
                    done: None,
                }))),
            }
        }
    })
    .with_controls({
        let case = case.clone();
        let trace = trace.clone();
        let sip = sip.clone();
        move |id, labels| {
            trace.lock().unwrap().push(format!(
                "controls:{}:{}:{}:{}:{}",
                id.pid(),
                id.creation_ticks(),
                id.image().display(),
                id.application(),
                labels
                    .iter()
                    .map(|s| s.to_utf8().unwrap())
                    .collect::<Vec<_>>()
                    .join("|")
            ));
            if case.close_at == "controls" {
                drop(sip.close());
            }
            match case.failure {
                "controls-argument" => Err(DesktopActionError::Argument("fixture")),
                "controls-error" => Err(DesktopActionError::Unavailable),
                _ => Ok(controls_observation(&case, labels)),
            }
        }
    });
    let desktop = desktop.with_capture({
        let case = case.clone();
        let trace = trace.clone();
        let sip = sip.clone();
        move |id| capture::observe_case(&case, &trace, &sip, id)
    });
    let desktop = process_commands::wire(desktop, case, &trace, &sip);
    let remaining = Arc::new(Remaining {
        closes: AtomicUsize::new(0),
        fail: false,
    });
    let platform = Arc::new(NativeDesktopCommands::with_runtime(
        sip.clone(),
        desktop,
        remaining.clone(),
    ));
    let runtime = Arc::new(NativeSipCommands::new(
        sip.clone(),
        platform,
        |_| Box::pin(async { panic!("No Micro setup authorized") }),
        NativeSipViews::new(|| Ok("null".into()), |_| Ok("null".into())),
    ));
    let dispatcher = NativeDispatcher::new(EPOCH.into(), 64, runtime.clone()).unwrap();
    if case.closed {
        drop(sip.close())
    }
    let mut responses = vec![];
    for (n, params) in case.params.iter().enumerate() {
        responses.push(projection(
            dispatcher
                .execute(request(case.method, params, n + 1))
                .await
                .outcome(),
        ));
    }
    let result = IvyJson::object([
        ("responses", IvyJson::Array(responses)),
        (
            "trace",
            IvyJson::Array(
                trace
                    .lock()
                    .unwrap()
                    .iter()
                    .cloned()
                    .map(Into::into)
                    .collect(),
            ),
        ),
        ("closed", sip.is_stopping().into()),
    ]);
    runtime.shutdown().await.unwrap();
    assert_eq!(remaining.closes.load(Ordering::SeqCst), 1);
    result
}
fn response(value: &IvyJson) -> &IvyJson {
    &value.get("responses").unwrap().as_array().unwrap()[0]
}
fn trace(value: &IvyJson) -> Vec<String> {
    value
        .get("trace")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect()
}
fn save(name: &str, value: &IvyJson) {
    if let Some(root) = std::env::var_os("IVY_PHONE_NATIVE_DESKTOP_TRACE") {
        fs::write(
            Path::new(&root).join(name),
            stringify(value, 4 * 1024 * 1024).unwrap(),
        )
        .unwrap();
    }
}
const OBSERVE: &str = r#"{"appUserModelId":"Phone.Fixture!App"}"#;
const LAUNCH: &str = r#"{"appUserModelId":"Phone.Fixture!App","startIfMissing":true}"#;

#[tokio::test]
async fn validation_replay_and_shutdown_preserve_original_application_work() {
    let mut rows = vec![];
    for method in ["desktop.observe", "desktop.launch"] {
        let mut case = Case::new(method, "{}");
        case.closed = true;
        let actual = run_case(&case).await;
        assert_eq!(
            response(&actual).get("error").unwrap().as_str().unwrap(),
            if method == "desktop.observe" {
                "runtime_stopping"
            } else {
                "invalid_request"
            }
        );
        assert!(trace(&actual).is_empty());
        rows.push(actual);
        for close_at in ["factory", "observe"] {
            let mut case = Case::new(
                method,
                if method == "desktop.observe" {
                    OBSERVE
                } else {
                    LAUNCH
                },
            );
            case.state = "ready";
            case.close_at = close_at;
            let actual = run_case(&case).await;
            let result = response(&actual).get("result").unwrap();
            if method == "desktop.observe" {
                assert_eq!(result.get("state").unwrap().as_str().unwrap(), "ready");
            } else {
                assert_eq!(
                    result.get("errorCode").unwrap().as_str().unwrap(),
                    "desktop_observation_unavailable"
                );
                assert!(!trace(&actual).contains(&"activate".into()));
            }
            rows.push(actual);
        }
    }
    for method in ["desktop.observe", "desktop.launch"] {
        for closed in [false, true] {
            let params = if method == "desktop.observe" {
                r#"{"appUserModelId":"\ud800"}"#
            } else {
                r#"{"appUserModelId":"\ud800","startIfMissing":true}"#
            };
            let mut case = Case::new(method, params);
            case.closed = closed;
            let actual = run_case(&case).await;
            let expected = if method == "desktop.launch" {
                "invalid_request"
            } else if closed {
                "runtime_stopping"
            } else {
                "operation_outcome_unknown"
            };
            assert_eq!(
                response(&actual).get("error").unwrap().as_str().unwrap(),
                expected
            );
            assert!(trace(&actual).is_empty());
            rows.push(actual);
        }
    }
    let mut replay = Case::new("desktop.launch", LAUNCH);
    replay.params.push(LAUNCH.into());
    let actual = run_case(&replay).await;
    assert_eq!(
        trace(&actual),
        [
            format!("factory:{APP}"),
            "observe".into(),
            "activate".into()
        ]
    );
    let replies = actual.get("responses").unwrap().as_array().unwrap();
    assert_eq!(replies[0], replies[1]);
    rows.push(actual);
    replay.params[1] = LAUNCH.replace(":true", ": true");
    let actual = run_case(&replay).await;
    assert_eq!(
        actual.get("responses").unwrap().as_array().unwrap()[1]
            .get("error")
            .unwrap()
            .as_str()
            .unwrap(),
        "operation_conflict"
    );
    rows.push(actual);
    for method in ["desktop.observe", "desktop.launch"] {
        let mut c = Case::new(
            method,
            if method == "desktop.observe" {
                OBSERVE
            } else {
                LAUNCH
            },
        );
        c.failure = "factory-null";
        let actual = run_case(&c).await;
        if method == "desktop.observe" {
            assert_eq!(
                response(&actual).get("error").unwrap().as_str().unwrap(),
                "operation_outcome_unknown"
            );
        } else {
            assert_eq!(
                response(&actual)
                    .get("result")
                    .unwrap()
                    .get("phase")
                    .unwrap()
                    .as_str()
                    .unwrap(),
                "not_submitted"
            );
        }
        rows.push(actual);
    }

    // The blocking action starts independently of whether its returned observer
    // is ever polled. Stop during factory entry must still veto later activation.
    let sip = NativeSipOperations::new(Arc::new(NoMedia));
    let log = Arc::new(Mutex::new(vec![]));
    let (enter_tx, enter_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let release = Mutex::new(release_rx);
    let done = Mutex::new(Some(done_tx));
    let desktop = PhoneDesktopRuntime::with_application({
        let sip = sip.clone();
        let log = log.clone();
        move |_| {
            log.lock().unwrap().push("factory".into());
            enter_tx.send(()).unwrap();
            release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
            Ok(Some(Box::new(Application {
                case: Case::new("desktop.launch", LAUNCH),
                sip: sip.clone(),
                trace: log.clone(),
                done: done.lock().unwrap().take(),
            })))
        }
    });
    let remaining = Arc::new(Remaining {
        closes: AtomicUsize::new(0),
        fail: false,
    });
    let layer = NativeDesktopCommands::with_runtime(sip.clone(), desktop, remaining);
    let work = layer.invoke(request("desktop.launch", LAUNCH, 1));
    enter_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    drop(work);
    drop(sip.close());
    release_tx.send(()).unwrap();
    done_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(*log.lock().unwrap(), vec!["factory"]);
    layer.shutdown().await.unwrap();
    for fail in [false, true] {
        let sip = NativeSipOperations::new(Arc::new(NoMedia));
        let remaining = Arc::new(Remaining {
            closes: AtomicUsize::new(0),
            fail,
        });
        let layer = NativeDesktopCommands::with_runtime(
            sip,
            PhoneDesktopRuntime::with_application(|_| panic!("No factory authorized")),
            remaining.clone(),
        );
        assert_eq!(
            layer
                .invoke(request("inventory", "{}", 1))
                .await
                .result_json(),
            Some("3")
        );
        drop(layer.shutdown());
        assert_eq!(layer.shutdown().await.is_err(), fail);
        assert_eq!(layer.shutdown().await.is_err(), fail);
        assert_eq!(remaining.closes.load(Ordering::SeqCst), 1);
        assert_eq!(
            layer
                .invoke(request("desktop.observe", OBSERVE, 2))
                .await
                .error_code(),
            Some("runtime_stopping")
        );
    }
    for (image, success) in [
        (r"C:\Fixture\é😀+&'`.exe".to_owned(), true),
        ("unpaired".into(), true),
        (format!("C:\\Fixture\\{}.exe", "+".repeat(10800)), true),
        (format!("C:\\Fixture\\{}.exe", "+".repeat(10820)), false),
    ] {
        let mut case = Case::new("desktop.observe", OBSERVE);
        case.state = "ready";
        case.image = image;
        let actual = run_case(&case).await;
        assert_eq!(
            response(&actual).get("ok").unwrap().as_bool(),
            Some(success)
        );
        if success {
            assert!(
                response(&actual)
                    .get("raw")
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .is_ascii()
            );
        }
        rows.push(actual);
    }
    save("ordinary.json", &IvyJson::Array(rows));
    controls_admission_and_worker().await;
    capture::ordinary().await;
}
const IDENTITY: &str = r#"{"pid":101,"startTimeUtcTicks":"504911232000001234","imagePath":"C:\\Fixture\\ChatGPT.exe","appUserModelId":"Phone.Fixture!App"}"#;
fn control_params(identity: &str, labels: &str) -> String {
    format!(r#"{{"desktop":{identity},"labels":{labels}}}"#)
}
async fn controls_admission_and_worker() {
    let valid = control_params(IDENTITY, r#"["Start","Voice"]"#);
    let mut rows = vec![];
    for (params, closed, expected) in [
        (valid.clone(), false, None),
        (valid.clone(), true, Some("runtime_stopping")),
        ("{}".into(), true, Some("runtime_stopping")),
        (
            control_params("null", "3"),
            false,
            Some("operation_outcome_unknown"),
        ),
        (
            control_params(&IDENTITY.replace(":101", ":1.01e2"), "3"),
            false,
            Some("invalid_request"),
        ),
        (
            control_params(IDENTITY, r#"["Start","Start"]"#),
            false,
            Some("invalid_request"),
        ),
        (
            control_params(IDENTITY, r#"["\ud800"]"#),
            false,
            Some("invalid_request"),
        ),
    ] {
        let mut c = Case::new("desktop.controls", &params);
        c.closed = closed;
        let actual = run_case(&c).await;
        assert_eq!(
            response(&actual)
                .get("error")
                .unwrap()
                .as_str()
                .ok()
                .as_deref(),
            expected
        );
        assert_eq!(trace(&actual).is_empty(), expected.is_some());
        rows.push(actual);
    }
    for failure in ["controls-null", "controls-argument", "controls-error"] {
        let mut c = Case::new("desktop.controls", &valid);
        c.failure = failure;
        let actual = run_case(&c).await;
        assert_eq!(trace(&actual).len(), 1);
        assert_eq!(
            response(&actual)
                .get("error")
                .unwrap()
                .as_str()
                .ok()
                .as_deref(),
            match failure {
                "controls-argument" => Some("invalid_request"),
                "controls-error" => Some("operation_outcome_unknown"),
                _ => None,
            }
        );
        rows.push(actual);
    }
    let mut c = Case::new("desktop.controls", &valid);
    c.close_at = "controls";
    c.params.push(valid.clone());
    let actual = run_case(&c).await;
    assert_eq!(trace(&actual).len(), 1);
    assert_eq!(response(&actual).get("ok").unwrap().as_bool(), Some(true));
    assert_eq!(
        actual.get("responses").unwrap().as_array().unwrap()[1]
            .get("error")
            .unwrap()
            .as_str()
            .unwrap(),
        "runtime_stopping"
    );
    rows.push(actual);
    let mut c = Case::new("desktop.controls", &valid);
    c.state = "many";
    let actual = run_case(&c).await;
    assert_eq!(
        response(&actual).get("error").unwrap().as_str().unwrap(),
        "operation_outcome_unknown"
    );
    rows.push(actual);
    // An admitted read starts even when its result observer is never polled,
    // and survives a dropped observer plus SIP stopping while inside the read.
    let sip = NativeSipOperations::new(Arc::new(NoMedia));
    let (entered, enter) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let (finished, finish) = mpsc::channel();
    let released = Mutex::new(released);
    let desktop = PhoneDesktopRuntime::with_application(|_| panic!("No application authorized"))
        .with_controls(move |_, _| {
            entered.send(()).unwrap();
            released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
            finished.send(()).unwrap();
            Ok(Some(ControlsObservation::unavailable()))
        });
    let layer = NativeDesktopCommands::with_runtime(
        sip.clone(),
        desktop,
        Arc::new(Remaining {
            closes: AtomicUsize::new(0),
            fail: false,
        }),
    );
    let work = layer.invoke(request("desktop.controls", &valid, 1));
    enter.recv_timeout(Duration::from_secs(3)).unwrap();
    drop(work);
    drop(sip.close());
    release.send(()).unwrap();
    finish.recv_timeout(Duration::from_secs(3)).unwrap();
    layer.shutdown().await.unwrap();
    save("controls-ordinary.json", &IvyJson::Array(rows));
}
