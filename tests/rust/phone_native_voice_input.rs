use super::*;
use crate::{
    desktop::{
        ActionResult, DesktopApplication, DesktopObservation, DesktopVoiceRuntime,
        MicroFactoryError, PhoneDesktopRuntime, VoiceInputOwner,
    },
    micro::{
        MicroAttachmentStatus, MicroClose, MicroInputReceipt, MicroInputResult, MicroVoiceInput,
    },
    native_desktop::NativeDesktopCommands,
    native_rpc::{NativeDispatcher, NativeReply},
    native_sip::{NativeCallConfiguration, NativeSipMediaFactory, NativeSipMediaFuture},
};
use serde_json::{Value, json};
use std::{
    net::UdpSocket,
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use tokio::sync::Notify;

#[path = "phone_native_default_input.rs"]
mod default_input;

const EPOCH: &str = "11111111-1111-4111-8111-111111111111";
const APP: &str = "Phone.Fixture!App";
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
}
fn trace(name: &str, value: &Value) {
    if let Ok(root) = std::env::var("IVY_PHONE_NATIVE_VOICE_INPUT_TRACE") {
        std::fs::write(
            Path::new(&root).join(name),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }
}
fn request(method: &str, params: &str, operation: usize, sequence: usize) -> NativeRequest {
    let operation = if matches!(
        method,
        "status" | "heartbeat" | "desktop.observe" | "inventory"
    ) {
        "null".into()
    } else {
        format!("\"00000000-0000-4000-8000-{operation:012x}\"")
    };
    NativeRequest::parse(format!("{{\"version\":1,\"epoch\":\"{EPOCH}\",\"requestId\":{sequence},\"operationId\":{operation},\"method\":\"{method}\",\"params\":{params}}}").as_bytes()).unwrap()
}
fn project(method: &str, reply: NativeReply) -> Value {
    if let Some(code) = reply.outcome().error_code() {
        return json!({"error":code});
    }
    let value: Value = serde_json::from_str(reply.outcome().result_json().unwrap()).unwrap();
    match method {
        "status" | "heartbeat" => {
            json!({"configured":value["configured"],"call":value["call"],"registration":value["registration"],"micro":value["micro"],"callIds":value["callIds"],"endpointPresent":value["endpoint"].is_string(),"media":value["media"]})
        }
        "configure" => {
            json!({"configured":value["configured"],"endpointPresent":value["endpoint"].is_string()})
        }
        _ => value,
    }
}
#[derive(Default)]
struct Counts {
    trace: Mutex<Vec<String>>,
    factories: AtomicUsize,
    closes: AtomicUsize,
    media: AtomicUsize,
}
impl Counts {
    fn record(&self, value: &str) {
        self.trace.lock().unwrap().push(value.into());
    }
}
struct Media(Arc<Counts>);
impl NativeSipMediaFactory for Media {
    fn create(&self, _: String, _: Arc<NativeCallConfiguration>) -> NativeSipMediaFuture {
        self.0.media.fetch_add(1, Ordering::SeqCst);
        panic!("No media/audio/RTP may be created by input configuration");
    }
}
struct Application(Arc<Counts>);
impl DesktopApplication for Application {
    fn observe(&mut self) -> ActionResult<Option<DesktopObservation>> {
        self.0.record("observe");
        Ok(Some(DesktopObservation::unavailable()))
    }
    fn activate(&mut self) -> ActionResult<i32> {
        panic!("No activation allowed")
    }
}
struct Device {
    row: Value,
    counts: Arc<Counts>,
    pending: Option<Arc<Notify>>,
}
fn failure() -> Box<dyn std::error::Error + Send + Sync> {
    std::io::Error::other("fixture").into()
}
impl MicroVoiceInput for Device {
    fn status(&self) -> MicroInputResult<MicroAttachmentStatus> {
        self.counts.record("status");
        if self.row["kind"] == "status-io" {
            return Err(failure());
        }
        Ok(MicroAttachmentStatus {
            state: self.row["status"]["state"].as_str().unwrap().into(),
            generation: self.row["status"]["generation"].as_i64(),
            error_code: self.row["status"]["errorCode"].as_str().map(str::to_owned),
        })
    }
    fn close(&self) -> MicroInputResult<MicroClose> {
        self.counts.closes.fetch_add(1, Ordering::SeqCst);
        self.counts.record("close");
        match self.row["close"].as_str().unwrap() {
            "throw" => Err(failure()),
            "failed" => Ok(MicroClose::completed(Err(Arc::from(failure())))),
            "pending" => {
                let pending = self.pending.clone().unwrap();
                MicroClose::start(async move {
                    pending.notified().await;
                    Ok(())
                })
            }
            _ => Ok(MicroClose::completed(Ok(()))),
        }
    }
    fn send_microphone(
        &self,
        _: bool,
        _: i64,
        _: &dyn Fn() -> MicroInputResult<bool>,
    ) -> MicroInputResult<MicroInputReceipt> {
        panic!("No Micro input allowed")
    }
}
#[derive(Clone)]
struct Remaining {
    mode: &'static str,
    counts: Arc<Counts>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}
impl NativeHandler for Remaining {
    fn invoke(&self, _: NativeRequest) -> NativeInvocation {
        panic!("Unexpected delegated platform method")
    }
    fn start(&self, _: NativeRequest) -> NativeInvocationStart {
        self.counts.record("delegated-start");
        Box::pin(async {
            Box::pin(async { NativeOutcome::result("{\"delegated\":true}".into()) })
                as NativeInvocation
        })
    }
    fn cleanup_slots(&self) -> usize {
        7
    }
}
impl NativeRuntime for Remaining {
    fn shutdown(&self) -> NativeShutdown {
        self.counts.record("remaining");
        let owner = self.clone();
        if self.mode == "constructor-panic" {
            panic!("fixture remaining shutdown constructor");
        }
        Box::pin(async move {
            owner.entered.notify_one();
            if owner.mode == "pending" {
                owner.release.notified().await;
            }
            if owner.mode == "poll-panic" {
                panic!("fixture remaining shutdown poll");
            }
            if owner.mode == "failed" {
                return Err(NativeShutdownError::OutcomeUnknown);
            }
            Ok(())
        })
    }
}
fn assemble(
    row: &Value,
    remaining: Remaining,
    pending: Option<Arc<Notify>>,
) -> (Arc<NativeSipCommands>, VoiceInputOwner) {
    let counts = remaining.counts.clone();
    let factory_counts = counts.clone();
    let selected = row.clone();
    let input = VoiceInputOwner::new(move |_| {
        factory_counts.factories.fetch_add(1, Ordering::SeqCst);
        factory_counts.record("factory");
        match selected["kind"].as_str().unwrap() {
            "null" => Ok(None),
            "io" | "access" => Err(MicroFactoryError::ClientUnavailable),
            "socket" => Err(MicroFactoryError::AttachmentConflict),
            "other" => Err(MicroFactoryError::Other(failure())),
            _ => Ok(Some(Arc::new(Device {
                row: selected.clone(),
                counts: factory_counts.clone(),
                pending: pending.clone(),
            }))),
        }
    });
    let application = counts.clone();
    let desktop = PhoneDesktopRuntime::with_application(move |id| {
        assert_eq!(id, Some(APP));
        application.record("application");
        Ok(Some(Box::new(Application(application.clone()))))
    });
    let voice = DesktopVoiceRuntime::new(desktop.clone(), input.clone());
    let owner = NativeSipOperations::with_desktop_runtime(
        Arc::new(Media(counts.clone())),
        desktop.clone(),
        Arc::new(voice),
    );
    let platform: Arc<dyn NativeRuntime> = if remaining.mode == "quiet" {
        Arc::new(NativeDesktopCommands::with_runtime(
            owner.clone(),
            desktop,
            Arc::new(remaining),
        ))
    } else {
        Arc::new(remaining)
    };
    (
        Arc::new(NativeSipCommands::with_voice_input(
            owner,
            platform,
            input.clone(),
            |_| panic!("No realtime call exists"),
        )),
        input,
    )
}
fn remaining(mode: &'static str, counts: Arc<Counts>) -> Remaining {
    Remaining {
        mode,
        counts,
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    }
}
fn params(row: &Value) -> String {
    json!({"binding":{"address":"127.0.0.1","port":0,"transport":"udp","rtpPortRangeStart":57000,"rtpPortRangeEnd":57006},"codecs":{"preferences":["PCMA"]},"incomingPeers":[],"registration":null,"password":null,"micro":if row["kind"]=="none" {Value::Null} else {json!({"usbipExecutable":"C:/fixture/usbip.exe","executableHash":format!("sha256:{}","a".repeat(64)),"port":3249})}}).to_string()
}
fn close_result(value: Result<(), NativeShutdownError>) -> Value {
    match value {
        Ok(()) => json!({"ok":true}),
        Err(_) => json!({"error":"operation_outcome_unknown"}),
    }
}
fn cases() -> Vec<Value> {
    let base = |name: &str, kind: &str, close: &str, generation: Value, error: Value| json!({"name":name,"kind":kind,"close":close,"status":{"state":"ready","generation":generation,"errorCode":error}});
    let mut out = Vec::new();
    for kind in [
        "none",
        "device",
        "null",
        "io",
        "access",
        "socket",
        "other",
        "status-io",
    ] {
        out.push(base(kind, kind, "ok", json!(7), Value::Null));
    }
    for (name, generation) in [
        ("generation-null", Value::Null),
        ("generation-min", json!(i64::MIN)),
        ("generation-max", json!(i64::MAX)),
        ("generation-above-safe", json!(9007199254740993i64)),
    ] {
        out.push(base(
            name,
            "device",
            "ok",
            generation,
            json!("é\n\"fixture"),
        ));
    }
    for close in ["throw", "failed"] {
        out.push(base(
            &format!("close-{close}"),
            "device",
            close,
            json!(7),
            Value::Null,
        ));
    }
    out
}
async fn scenario(row: &Value) -> Value {
    let counts = Arc::new(Counts::default());
    let other = remaining("quiet", counts.clone());
    let (handler, _) = assemble(row, other, None);
    let dispatcher = NativeDispatcher::new(EPOCH.into(), 64, handler.clone()).unwrap();
    let mut rows = Vec::new();
    let application = json!({"appUserModelId":APP}).to_string();
    let params = params(row);
    for (method, args, op, seq) in [
        ("status", "{}", 0, 1),
        ("desktop.observe", application.as_str(), 0, 2),
        ("configure", params.as_str(), 1, 3),
        ("configure", params.as_str(), 1, 4),
        ("configure", "{}", 2, 5),
        ("status", "{}", 0, 6),
        ("heartbeat", "{}", 0, 7),
        ("desktop.observe", application.as_str(), 0, 8),
    ] {
        rows.push(project(
            method,
            dispatcher.execute(request(method, args, op, seq)).await,
        ));
    }
    let endpoint = handler.owner.observation().endpoint.unwrap();
    rows.push(close_result(handler.shutdown().await));
    rows.push(close_result(handler.shutdown().await));
    rows.push(project(
        "status",
        dispatcher.execute(request("status", "{}", 0, 9)).await,
    ));
    // Ignore the synthetic remaining layer when comparing the original owner.
    let trace: Vec<_> = counts
        .trace
        .lock()
        .unwrap()
        .iter()
        .filter(|x| x.as_str() != "remaining")
        .cloned()
        .collect();
    assert_eq!(counts.media.load(Ordering::SeqCst), 0);
    let value = json!({"name":row["name"],"rows":rows,"trace":trace,"factories":counts.factories.load(Ordering::SeqCst),"closes":counts.closes.load(Ordering::SeqCst),"mediaFactories":0,"bindingReleased":UdpSocket::bind(endpoint).is_ok()});
    value
}
fn original(cases: &[Value]) -> Value {
    let data: Value =
        serde_json::from_str(include_str!("../fixtures/phone-native-platform-cases.json")).unwrap();
    assert_eq!(data["voice"]["cases"], json!(cases));
    data["voice"]["expected"].clone()
}
#[test]
fn original_configuration_status_replay_desktop_and_final_input_disposal_agree() {
    let cases = cases();
    let actual = runtime().block_on(async {
        let mut out = Vec::new();
        for row in &cases {
            out.push(scenario(row).await);
        }
        Value::Array(out)
    });
    trace("matrix-rust.json", &actual);
    let expected = original(&cases);
    let differences: Vec<_> = actual
        .as_array()
        .unwrap()
        .iter()
        .zip(expected.as_array().unwrap())
        .filter(|(a, b)| a != b)
        .map(|(a, b)| json!({"rust":a,"original":b}))
        .collect();
    trace(
        "matrix-comparisons.json",
        &json!({"cases":cases.len(),"differences":differences,"rust":actual,"original":expected,"scope":"Original full projected no-call status/configuration/Desktop results, exact Micro Int64 fields and owner trace; ephemeral SIP endpoint normalized to presence."}),
    );
    assert_eq!(actual, expected);
}
#[test]
fn retained_finally_disposal_survives_dropped_waiters_and_remaining_failures() {
    runtime().block_on(async {
        let mut values=Vec::new();
        for mode in ["failed","constructor-panic","poll-panic","pending"] {
            let counts=Arc::new(Counts::default());let other=remaining(mode,counts.clone());let entered=other.entered.clone();let release=other.release.clone();let device_done=Arc::new(Notify::new());
            let row=json!({"kind":"device","close":"pending","status":{"state":"ready","generation":7,"errorCode":null}});
            let (handler,input)=assemble(&row,other,Some(device_done.clone()));
            let dispatcher=NativeDispatcher::new(EPOCH.into(),64,handler.clone()).unwrap();
            let configured=dispatcher.execute(request("configure",&params(&row),1,1)).await;assert!(configured.outcome().error_code().is_none());
            assert_eq!(handler.cleanup_slots(),4); // SIP's own cleanup policy still owns capacity.
            // The wrapper preserves the remaining admission prefix and capacity.
            assert_eq!(handler.remaining.cleanup_slots(),7);
            let started=handler.remaining.start(request("inventory","{}",0,2));assert!(counts.trace.lock().unwrap().contains(&"delegated-start".into()));
            assert_eq!(started.await.await.result_json(),Some("{\"delegated\":true}"));
            drop(handler.shutdown());
            assert!(handler.owner.is_stopping());
            if mode=="pending" {
                tokio::time::timeout(Duration::from_secs(2),entered.notified()).await.unwrap();
                assert_eq!(counts.closes.load(Ordering::SeqCst),0);release.notify_one();
            }
            tokio::time::timeout(Duration::from_secs(2),async {while counts.closes.load(Ordering::SeqCst)==0 {tokio::time::sleep(Duration::from_millis(2)).await;}}).await.unwrap();
            let mut waiter=handler.shutdown();assert!(tokio::time::timeout(Duration::from_millis(30),&mut waiter).await.is_err());drop(waiter);
            // No new input factory may enter after final disposal begins.
            let before=counts.factories.load(Ordering::SeqCst);let selected=input.micro_status().unwrap();assert!(selected.is_some());assert_eq!(counts.factories.load(Ordering::SeqCst),before);
            device_done.notify_one();let outcome=tokio::time::timeout(Duration::from_secs(2),handler.shutdown()).await.unwrap();
            assert_eq!(outcome.is_ok(),mode=="pending");let replay=handler.shutdown().await;assert_eq!(outcome,replay);
            assert_eq!(counts.closes.load(Ordering::SeqCst),1);let events=counts.trace.lock().unwrap().clone();
            assert!(events.iter().position(|x|x=="remaining").unwrap()<events.iter().position(|x|x=="close").unwrap());
            values.push(json!({"mode":mode,"result":close_result(outcome),"events":events,"factories":before,"closes":1,"droppedObserversRetained":true,"delegatedPrefixAndCapacityPreserved":true}));
        }
        trace("retention-controls.json",&json!({"cases":values,"scope":"Actual Rust composition with injected remaining-future failure/panic/pending and input disposal; no original counterpart for these injected modular runtime controls."}));
    });
}
