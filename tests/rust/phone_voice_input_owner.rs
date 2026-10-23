use super::*;
use crate::{
    desktop::{DesktopActionError, NativeHotkey},
    micro::MicroInputReceipt,
};
use ivy_contracts::{IvyJson, json::parse};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Weak,
        atomic::{AtomicUsize, Ordering},
    },
};
fn n(v: i64) -> IvyJson {
    IvyJson::Number(v as f64)
}
fn array(v: impl IntoIterator<Item = IvyJson>) -> IvyJson {
    IvyJson::Array(v.into_iter().collect())
}
fn int(v: &IvyJson, name: &str) -> i64 {
    v.get(name).unwrap().as_f64().unwrap() as i64
}
fn flag(v: &IvyJson, name: &str) -> bool {
    v.get(name).unwrap().as_bool().unwrap()
}
fn text(v: &IvyJson, name: &str) -> String {
    v.get(name).unwrap().as_str().unwrap()
}
fn error() -> Box<dyn std::error::Error + Send + Sync> {
    std::io::Error::other("fixture").into()
}
fn pick(v: &IvyJson, name: &str, index: usize) -> IvyJson {
    let a = v.get(name).unwrap().as_array().unwrap();
    a[index.min(a.len() - 1)].clone()
}
fn captured(result: MicroInputResult<IvyJson>) -> IvyJson {
    match result {
        Ok(value) => IvyJson::object([("ok", true.into()), ("value", value)]),
        Err(e) => {
            let message = if e
                .downcast_ref::<DesktopActionError>()
                .is_some_and(|e| matches!(e, DesktopActionError::Argument(_)))
            {
                "invalid_request"
            } else if e.to_string() == "Micro configuration changed within the native owner." {
                "Micro configuration changed within the native owner."
            } else {
                "fixture"
            };
            IvyJson::object([("ok", false.into()), ("error", message.into())])
        }
    }
}
struct Fixture {
    row: IvyJson,
    trace: Vec<String>,
    factory: usize,
    status: usize,
    original: usize,
    sleep: usize,
    close: usize,
    tick: i64,
    now: u64,
    owner: Option<Weak<Owner>>,
    pending: Option<tokio::sync::oneshot::Sender<()>>,
}
impl Fixture {
    fn new(row: IvyJson) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            row,
            trace: vec![],
            factory: 0,
            status: 0,
            original: 0,
            sleep: 0,
            close: 0,
            tick: 10000,
            now: 0,
            owner: None,
            pending: None,
        }))
    }
}
struct Time(Arc<Mutex<Fixture>>);
impl VoiceInputClock for Time {
    fn retry_time_millis(&self) -> i64 {
        let mut s = self.0.lock().unwrap();
        let v = s.tick;
        s.trace.push(format!("tick:{v}"));
        v
    }
    fn timestamp(&self) -> Duration {
        let mut s = self.0.lock().unwrap();
        let v = s.now;
        s.trace.push(format!("start:{v}"));
        Duration::from_millis(v)
    }
    fn elapsed(&self, since: Duration) -> Duration {
        let mut s = self.0.lock().unwrap();
        let v = Duration::from_millis(s.now).saturating_sub(since);
        s.trace.push(format!("elapsed:{}", v.as_millis()));
        v
    }
    fn sleep(&self, ms: u64) -> MicroInputResult<()> {
        let mut s = self.0.lock().unwrap();
        s.trace.push(format!("sleep:{ms}"));
        let at = s.sleep;
        s.sleep += 1;
        if int(&s.row, "sleepFault") == at as i64 {
            return Err(error());
        }
        s.now += ms;
        s.tick += ms as i64;
        Ok(())
    }
}
fn settings(which: i64, fallback: bool) -> NativeVoiceInput {
    let micro=(which!=0).then(||{
        let raw = r#"{"micro":{"usbipExecutable":"C:/fixture/usbip.exe","executableHash":"sha256:HASH","port":PORT}}"#
            .replace("HASH", &"a".repeat(64))
            .replace("PORT", &(1023 + which).to_string());
  NativeVoiceInput::read(&parse(&raw,65536).unwrap(),&raw).unwrap().micro().unwrap().clone()
 });
    NativeVoiceInput::new(
        micro,
        fallback.then(|| NativeHotkey::new(vec!["control".into()], 32, false).unwrap()),
        false,
    )
}
fn original(state: &Arc<Mutex<Fixture>>) -> MicroInputResult<bool> {
    let mut s = state.lock().unwrap();
    let i = s.original;
    s.original += 1;
    s.trace.push(format!("original:{i}"));
    if int(&s.row, "originalFault") == i as i64 {
        return Err(error());
    }
    Ok(pick(&s.row, "originals", i).as_bool().unwrap())
}
fn reenter(state: &Arc<Mutex<Fixture>>, where_: &str, i: usize) -> MicroInputResult<()> {
    let (action, owner) = {
        let mut s = state.lock().unwrap();
        if int(&s.row, &format!("{where_}ReenterAt")) != i as i64 {
            return Ok(());
        }
        let action = text(&s.row, &format!("{where_}Reenter"));
        if action == "none" {
            return Ok(());
        }
        s.trace.push(format!("{where_}.reenter:{action}"));
        (
            action,
            VoiceInputOwner(s.owner.as_ref().unwrap().upgrade().unwrap()),
        )
    };
    match action.as_str() {
        "status" => {
            owner.micro_status()?;
        }
        "close" => {
            assert!(owner.close()?.result().unwrap().is_ok());
        }
        _ => {
            owner.select(&settings(1, false), false, &|| original(state))?;
        }
    }
    Ok(())
}
struct Device {
    state: Arc<Mutex<Fixture>>,
    id: usize,
}
impl MicroVoiceInput for Device {
    fn status(&self) -> MicroInputResult<MicroAttachmentStatus> {
        let (item, i) = {
            let mut s = self.state.lock().unwrap();
            let i = s.status;
            s.status += 1;
            s.trace.push(format!("status:{}:{i}", self.id));
            let item = pick(&s.row, "statuses", i);
            let delta = int(&item, "delta");
            s.now += delta as u64;
            s.tick += delta;
            (item, i)
        };
        reenter(&self.state, "status", i)?;
        if flag(&item, "fault") {
            return Err(error());
        }
        Ok(MicroAttachmentStatus {
            state: text(&item, "state"),
            generation: (!item.get("generation").unwrap().is_null())
                .then(|| int(&item, "generation")),
            error_code: None,
        })
    }
    fn send_microphone(
        &self,
        _: bool,
        _: i64,
        _: &dyn Fn() -> MicroInputResult<bool>,
    ) -> MicroInputResult<MicroInputReceipt> {
        panic!("unexpected input")
    }
    fn close(&self) -> MicroInputResult<MicroClose> {
        let (mode, i) = {
            let mut s = self.state.lock().unwrap();
            let i = s.close;
            s.close += 1;
            s.trace.push(format!("close:{}:{i}", self.id));
            (pick(&s.row, "closeReturns", i).as_str().unwrap(), i)
        };
        reenter(&self.state, "close", i)?;
        match mode.as_str() {
            "throw" => Err(error()),
            "failed" => Ok(MicroClose::completed(Err(Arc::new(std::io::Error::other(
                "fixture",
            ))))),
            "pending" => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                self.state.lock().unwrap().pending = Some(tx);
                MicroClose::start(async move {
                    rx.await
                        .map_err(|e| Arc::new(e) as Arc<dyn std::error::Error + Send + Sync>)?;
                    Ok(())
                })
            }
            _ => Ok(MicroClose::completed(Ok(()))),
        }
    }
}
fn factory(state: &Arc<Mutex<Fixture>>, micro: &NativeMicroSettings) -> MicroFactoryResult {
    let (i, mode) = {
        let mut s = state.lock().unwrap();
        let i = s.factory;
        s.factory += 1;
        s.trace.push(format!("factory:{i}:{}", micro.port()));
        (i, pick(&s.row, "factories", i).as_str().unwrap())
    };
    reenter(state, "factory", i).map_err(MicroFactoryError::Other)?;
    {
        let mut s = state.lock().unwrap();
        let delta = int(&s.row, "factoryDelta");
        s.tick += delta;
        s.now += delta as u64;
    }
    match mode.as_str() {
        "device" => Ok(Some(Arc::new(Device {
            state: state.clone(),
            id: i + 1,
        }))),
        "null" => Ok(None),
        "io" | "access" => Err(MicroFactoryError::ClientUnavailable),
        "socket" => Err(MicroFactoryError::AttachmentConflict),
        "argument" => Err(MicroFactoryError::Other(Box::new(
            DesktopActionError::Argument("fixture"),
        ))),
        "panic" => panic!("fixture factory"),
        _ => Err(MicroFactoryError::Other(error())),
    }
}
async fn script(row: &IvyJson) -> IvyJson {
    let state = Fixture::new(row.clone());
    let state_factory = state.clone();
    let owner = VoiceInputOwner::with_clock(
        move |m| factory(&state_factory, m),
        Arc::new(Time(state.clone())),
    );
    state.lock().unwrap().owner = Some(Arc::downgrade(&owner.0));
    let mut disposal: Option<MicroClose> = None;
    let mut results = vec![];
    for step in row.get("steps").unwrap().as_array().unwrap() {
        let action = text(step, "action");
        if action == "complete" {
            let tx = state.lock().unwrap().pending.take().unwrap();
            tx.send(()).unwrap();
            disposal.clone().unwrap().wait().await.unwrap();
            results.push(captured(Ok(true.into())));
            continue;
        }
        let result = catch_unwind(AssertUnwindSafe(|| -> MicroInputResult<IvyJson> {
            match action.as_str() {
                "select" => {
                    let p = owner.select(
                        &settings(int(step, "micro"), flag(step, "fallback")),
                        flag(step, "wait"),
                        &|| original(&state),
                    )?;
                    Ok(match p {
                        VoiceInputPlan::Hotkey => IvyJson::object([
                            ("method", "hotkey".into()),
                            ("generation", IvyJson::Null),
                            ("device", false.into()),
                        ]),
                        VoiceInputPlan::Micro { generation, device } => IvyJson::object([
                            ("method", "micro".into()),
                            ("generation", generation.map_or(IvyJson::Null, n)),
                            ("device", device.is_some().into()),
                        ]),
                    })
                }
                "prepare" => {
                    owner.prepare_micro(settings(1, false).micro().unwrap().clone())?;
                    Ok(true.into())
                }
                "status" => Ok(owner.micro_status()?.map_or(IvyJson::Null, |s| {
                    IvyJson::object([
                        ("state", s.state.into()),
                        ("generation", s.generation.map_or(IvyJson::Null, n)),
                        ("errorCode", s.error_code.map_or(IvyJson::Null, Into::into)),
                    ])
                })),
                "time" => {
                    let mut s = state.lock().unwrap();
                    s.tick = int(step, "tick");
                    s.now = int(step, "now") as u64;
                    Ok(true.into())
                }
                _ => {
                    let close = owner.close()?;
                    let result = close.result();
                    let value = IvyJson::object([
                        (
                            "same",
                            disposal
                                .as_ref()
                                .is_some_and(|old| old.same_work(&close))
                                .into(),
                        ),
                        ("finished", result.is_some().into()),
                        (
                            "error",
                            if result.is_some_and(|r| r.is_err()) {
                                "fixture".into()
                            } else {
                                IvyJson::Null
                            },
                        ),
                    ]);
                    disposal = Some(close);
                    Ok(value)
                }
            }
        }));
        results.push(captured(result.unwrap_or_else(|_| Err(error()))));
    }
    let trace = array(state.lock().unwrap().trace.iter().cloned().map(Into::into));
    IvyJson::object([("results", array(results)), ("trace", trace)])
}
fn select(micro: i64, fallback: bool, wait: bool) -> IvyJson {
    IvyJson::object([
        ("action", "select".into()),
        ("micro", n(micro)),
        ("fallback", fallback.into()),
        ("wait", wait.into()),
    ])
}
fn action(name: &str) -> IvyJson {
    IvyJson::object([("action", name.into())])
}
fn time(tick: i64, now: i64) -> IvyJson {
    IvyJson::object([
        ("action", "time".into()),
        ("tick", n(tick)),
        ("now", n(now)),
    ])
}
fn status(state: &str, generation: Option<i64>, delta: i64, fault: bool) -> IvyJson {
    IvyJson::object([
        ("state", state.into()),
        ("generation", generation.map_or(IvyJson::Null, n)),
        ("delta", n(delta)),
        ("fault", fault.into()),
    ])
}
fn baseline() -> IvyJson {
    IvyJson::object([
        ("factories", array(["device".into()])),
        ("statuses", array([status("ready", Some(1), 0, false)])),
        ("originals", array([true.into()])),
        ("closeReturns", array(["ok".into()])),
        ("factoryDelta", n(0)),
        ("sleepFault", n(-1)),
        ("originalFault", n(-1)),
        ("factoryReenterAt", n(-1)),
        ("factoryReenter", "none".into()),
        ("statusReenterAt", n(-1)),
        ("statusReenter", "none".into()),
        ("closeReenterAt", n(-1)),
        ("closeReenter", "none".into()),
        (
            "steps",
            array([
                action("status"),
                select(1, false, false),
                action("status"),
                action("close"),
                action("close"),
            ]),
        ),
    ])
}
struct ConcurrentDevice(AtomicUsize);
impl MicroVoiceInput for ConcurrentDevice {
    fn status(&self) -> MicroInputResult<MicroAttachmentStatus> {
        Ok(MicroAttachmentStatus {
            state: "ready".into(),
            generation: Some(1),
            error_code: None,
        })
    }
    fn send_microphone(
        &self,
        _: bool,
        _: i64,
        _: &dyn Fn() -> MicroInputResult<bool>,
    ) -> MicroInputResult<MicroInputReceipt> {
        panic!("unexpected input")
    }
    fn close(&self) -> MicroInputResult<MicroClose> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(MicroClose::completed(Ok(())))
    }
}
fn concurrent() -> IvyJson {
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release = Mutex::new(release_rx);
    let calls = Arc::new(AtomicUsize::new(0));
    let device = Arc::new(ConcurrentDevice(AtomicUsize::new(0)));
    let call_count = calls.clone();
    let input = device.clone();
    let owner = VoiceInputOwner::new(move |_| {
        call_count.fetch_add(1, Ordering::SeqCst);
        entered_tx.send(()).unwrap();
        release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        Ok(Some(input.clone()))
    });
    let selecting_owner = owner.clone();
    let selecting = std::thread::spawn(move || {
        selecting_owner
            .select(&settings(1, false), false, &|| Ok(true))
            .unwrap()
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let (start_tx, start_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let closing_owner = owner.clone();
    let closing = std::thread::spawn(move || {
        start_tx.send(()).unwrap();
        done_tx.send(closing_owner.close().unwrap()).unwrap();
    });
    start_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let early = done_rx.recv_timeout(Duration::from_millis(50)).ok();
    release_tx.send(()).unwrap();
    let before = early.is_some();
    let close = early.unwrap_or_else(|| done_rx.recv_timeout(Duration::from_secs(2)).unwrap());
    let selected = selecting.join().unwrap();
    closing.join().unwrap();
    let last = owner.close().unwrap();
    IvyJson::object([
        ("factoryCalls", n(calls.load(Ordering::SeqCst) as i64)),
        ("closeCalls", n(device.0.load(Ordering::SeqCst) as i64)),
        ("closeBeforeFactoryReleased", before.into()),
        (
            "selectReady",
            matches!(
                selected,
                VoiceInputPlan::Micro {
                    generation: Some(1),
                    ..
                }
            )
            .into(),
        ),
        ("closeFinished", close.result().is_some().into()),
        ("sameWork", last.same_work(&close).into()),
    ])
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn original_selection_retry_reentry_and_disposal_owner_agree() {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|info| {
        if !info
            .payload()
            .downcast_ref::<&str>()
            .is_some_and(|s| s.starts_with("fixture "))
        {
            eprintln!("{info}");
        }
    }));
    let mut rows = vec![];
    for mode in [
        "device", "null", "io", "access", "socket", "argument", "other", "panic",
    ] {
        for state_name in ["ready", "starting", "attached", "unavailable", "other"] {
            for fallback in [false, true] {
                for wait in [false, true] {
                    for originals in [vec![true], vec![false], vec![true, false]] {
                        let mut r = baseline();
                        r.insert("factories", array([mode.into()]));
                        r.insert("statuses", array([status(state_name, Some(1), 0, false)]));
                        r.insert("originals", array(originals.into_iter().map(Into::into)));
                        r.insert(
                            "steps",
                            array([
                                action("status"),
                                select(1, fallback, wait),
                                action("status"),
                                select(1, fallback, false),
                                action("close"),
                                select(1, fallback, false),
                                select(0, fallback, false),
                                action("status"),
                                action("close"),
                            ]),
                        );
                        rows.push(r);
                    }
                }
            }
        }
    }
    for next in [
        "device", "null", "io", "access", "socket", "argument", "other", "panic",
    ] {
        for retry in ["select", "status"] {
            for final_retry in ["select", "status"] {
                let mut r = baseline();
                r.insert(
                    "factories",
                    array(["socket".into(), next.into(), "device".into()]),
                );
                let retry_action = if retry == "select" {
                    select(1, true, false)
                } else {
                    action("status")
                };
                let final_action = if final_retry == "select" {
                    select(1, false, false)
                } else {
                    action("status")
                };
                r.insert(
                    "steps",
                    array([
                        select(1, true, false),
                        time(10999, 999),
                        action("status"),
                        select(1, true, false),
                        time(11000, 1000),
                        retry_action,
                        final_action,
                        time(12000, 2000),
                        action("status"),
                        select(1, false, false),
                        action("close"),
                    ]),
                );
                rows.push(r);
            }
        }
    }
    for generation in [None, Some(0), Some(1), Some(9007199254740991)] {
        for delta in [9999, 10000, 10001] {
            for second in ["ready", "attached", "starting"] {
                let mut r = baseline();
                r.insert(
                    "statuses",
                    array([
                        status("starting", None, delta, false),
                        status(second, generation, 0, false),
                    ]),
                );
                r.insert("steps", array([select(1, true, true), action("close")]));
                rows.push(r);
            }
        }
    }
    for mode in ["io", "socket", "device"] {
        for delta in [1, 1000] {
            let mut r = baseline();
            r.insert("factoryDelta", n(delta));
            r.insert("factories", array([mode.into(), "device".into()]));
            r.insert(
                "steps",
                array([
                    action("prepare"),
                    time(11000, 1000),
                    action("status"),
                    time(12000, 2000),
                    action("status"),
                    action("close"),
                ]),
            );
            rows.push(r);
        }
    }
    for where_ in ["factory", "status", "close"] {
        for re in ["status", "select", "close"] {
            if where_ == "close" && re == "close" {
                continue;
            }
            let mut r = baseline();
            r.insert(format!("{where_}ReenterAt"), n(0));
            r.insert(format!("{where_}Reenter"), re.into());
            rows.push(r);
        }
    }
    for close_returns in [
        vec!["ok"],
        vec!["failed"],
        vec!["throw", "ok"],
        vec!["pending"],
    ] {
        for initial_close in [false, true] {
            let mut r = baseline();
            r.insert(
                "closeReturns",
                array(close_returns.iter().map(|s| (*s).into())),
            );
            let mut steps = vec![];
            if initial_close {
                steps.push(action("close"));
            }
            steps.extend([
                select(1, true, false),
                select(2, false, false),
                action("close"),
                action("status"),
                select(1, true, true),
                select(0, true, true),
                action("close"),
            ]);
            if !initial_close && close_returns[0] == "pending" {
                steps.extend([action("complete"), action("close")]);
            }
            r.insert("steps", array(steps));
            rows.push(r);
        }
    }
    for fault_name in ["sleepFault", "originalFault", "status"] {
        for at in [0, 1] {
            let mut r = baseline();
            if fault_name == "status" {
                r.insert(
                    "statuses",
                    array([
                        status("starting", None, 0, at == 0),
                        status("ready", Some(7), 0, at == 1),
                    ]),
                );
            } else {
                r.insert(fault_name, n(at));
                r.insert(
                    "statuses",
                    array([
                        status("starting", None, 0, false),
                        status("starting", None, 0, false),
                        status("ready", Some(7), 0, false),
                    ]),
                );
            }
            r.insert(
                "steps",
                array([
                    select(1, true, true),
                    action("status"),
                    select(2, false, false),
                    action("close"),
                    action("close"),
                ]),
            );
            rows.push(r);
        }
    }
    let real_clock = WindowsInputClock(std::time::Instant::now());
    let before_tick = real_clock.retry_time_millis();
    let data = parse(
        include_str!("../fixtures/phone-voice-input-cases.json"),
        32 * 1024 * 1024,
    )
    .unwrap();
    let old = data.get("owner").unwrap();
    let mut actual = vec![];
    for row in &rows {
        actual.push(script(row).await);
    }
    let expected = old.get("scripts").unwrap().as_array().unwrap();
    assert_eq!(expected.len(), actual.len());
    for (i, (a, b)) in actual.iter().zip(expected).enumerate() {
        assert_eq!(a, b, "owner script {i}");
    }
    let after_tick = real_clock.retry_time_millis();
    assert!(after_tick >= before_tick);
    assert_eq!(old.get("serialization"), Some(&concurrent()));
    std::panic::set_hook(hook);
    // No observer, including an unpolled/dropped one, owns disposal's execution.
    let (tx, rx) = tokio::sync::oneshot::channel();
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    let close = MicroClose::start(async move {
        rx.await.unwrap();
        count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
    .unwrap();
    let observer = close.clone().wait();
    drop(observer);
    tx.send(()).unwrap();
    close.clone().wait().await.unwrap();
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert!(close.clone().wait().await.is_ok());
    let failed = MicroClose::start(async {
        Err(Arc::new(std::io::Error::other("fixture")) as Arc<dyn std::error::Error + Send + Sync>)
    })
    .unwrap();
    let a = failed.clone().wait().await.unwrap_err();
    let b = failed.wait().await.unwrap_err();
    assert!(Arc::ptr_eq(&a, &b));
}
