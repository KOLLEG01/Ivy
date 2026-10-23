use super::lifecycle_boundaries::{Caller, micro_json};
use super::*;
use ivy_phone_bridge::{
    desktop::VoiceInputOwner,
    micro::{
        MicroAttachmentStatus, MicroClose, MicroInputReceipt, MicroInputResult, MicroVoiceInput,
    },
    native_rpc::{NativeInvocationStart, NativeReply},
};
use std::sync::Condvar;

const OTHER: &str = "21111111-1111-4111-8111-111111111111";
const FIRST: u16 = 47000;
const LAST: u16 = 47006;

fn project(method: &str, reply: &NativeReply) -> IvyJson {
    if let Some(error) = reply.outcome().error_code() {
        return IvyJson::object([("error", error.into())]);
    }
    let value = value(reply.outcome());
    match method {
        "call.prepare" => IvyJson::object([
            ("id", value.get("id").unwrap().clone()),
            ("state", value.get("state").unwrap().clone()),
        ]),
        "status" => IvyJson::object([
            ("configured", value.get("configured").unwrap().clone()),
            (
                "endpointPresent",
                (!value.get("endpoint").unwrap().is_null()).into(),
            ),
            ("micro", value.get("micro").unwrap().clone()),
        ]),
        _ => value,
    }
}
fn counted_status(reply: NativeReply) -> IvyJson {
    let value = value(reply.outcome());
    IvyJson::object([
        ("configured", value.get("configured").unwrap().clone()),
        ("callIds", value.get("callIds").unwrap().clone()),
    ])
}
fn released(endpoint: std::net::SocketAddr) -> bool {
    std::net::UdpSocket::bind(endpoint).is_ok()
}

struct ConstructionFactory {
    inner: Arc<NativeSipSessionFactory>,
    starts: AtomicUsize,
    mode: usize,
    gate: Arc<Gate>,
}
impl NativeSipMediaFactory for ConstructionFactory {
    fn create(
        &self,
        id: String,
        configuration: Arc<NativeCallConfiguration>,
    ) -> NativeSipMediaFuture {
        let first = self.starts.fetch_add(1, Ordering::SeqCst) == 0;
        let (inner, gate, mode) = (self.inner.clone(), self.gate.clone(), self.mode);
        Box::pin(async move {
            if first {
                gate.block().await;
                match mode {
                    1 => return Err(SipHostError::InvalidRequest),
                    2 => return Err(SipHostError::InitializationFailed),
                    _ => (),
                }
            }
            inner.create(id, configuration).await
        })
    }
}
async fn construction(mode: usize) -> IvyJson {
    let routes = Arc::new(Factory::routes(vec![]));
    let gate = Arc::new(Gate::default());
    let factory = Arc::new(ConstructionFactory {
        inner: Arc::new(NativeSipSessionFactory::new(
            routes.clone(),
            None,
            Arc::new(Clock),
            |_| Arc::new(Events::default()),
        )),
        starts: AtomicUsize::new(0),
        mode,
        gate: gate.clone(),
    });
    let owner = NativeSipOperations::new(factory.clone());
    let remaining = Arc::new(Remaining::default());
    remaining.close_gate.release();
    let handler = Arc::new(NativeSipCommands::new(
        owner.clone(),
        remaining,
        |_| Box::pin(async { Ok(()) }),
        closed_platform_views(),
    ));
    let dispatcher = Arc::new(NativeDispatcher::new(CALL.into(), 64, handler.clone()).unwrap());
    let params = BASE
        .replace("57000", &FIRST.to_string())
        .replace("57998", &LAST.to_string());
    let configured = dispatcher
        .execute(command("configure", &params, 1, 1))
        .await;
    assert_eq!(configured.outcome().error_code(), None);
    let bound = owner.observation().endpoint.unwrap();
    let mut holders = Vec::new();
    if mode == 3 {
        for port in FIRST..=LAST + 1 {
            holders.push(
                std::net::UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, port)).expect(
                    "Every fixture RTP/RTCP port must be owned before forcing allocation failure",
                ),
            );
        }
    }
    let held = holders.len();
    let selected = format!(r#"{{"callId":"{CALL}"}}"#);
    let other = format!(r#"{{"callId":"{OTHER}"}}"#);
    cancel(dispatcher.execute(command("call.prepare", &selected, 2, 2))).await;
    until(|| gate.entered.load(Ordering::SeqCst)).await;
    let second_dispatcher = dispatcher.clone();
    let second = tokio::spawn(async move {
        second_dispatcher
            .execute(command("call.hangup", &other, 3, 3))
            .await
    });
    sleep(Duration::from_millis(10)).await;
    let before = second.is_finished();
    let receipts = dispatcher.receipt_count();
    let starts = factory.starts.load(Ordering::SeqCst);
    gate.release();
    let prepared = dispatcher
        .execute(command("call.prepare", &selected, 2, 4))
        .await;
    let other_result = second.await.unwrap();
    let replay = dispatcher
        .execute(command("call.prepare", &selected, 2, 5))
        .await;
    let same = prepared.outcome() == replay.outcome();
    let status = counted_status(dispatcher.execute(command("status", "{}", 0, 6)).await);
    let mut probe = Vec::new();
    if mode == 3 {
        for attempt in 0..8 {
            probe.push(project(
                "call.prepare",
                &dispatcher
                    .execute(command(
                        "call.prepare",
                        &selected,
                        20 + attempt,
                        20 + attempt,
                    ))
                    .await,
            ));
        }
    }
    let retired = dispatcher
        .execute(command("call.release", &selected, 4, 7))
        .await;
    holders.clear();
    let next = dispatcher
        .execute(command("call.prepare", &selected, 5, 8))
        .await;
    assert_eq!(
        next.outcome().error_code(),
        None,
        "Replacement fixture media must construct"
    );
    let old_prepare = dispatcher
        .execute(command("call.prepare", &selected, 2, 9))
        .await;
    let old_release = dispatcher
        .execute(command("call.release", &selected, 4, 10))
        .await;
    let replacement = counted_status(dispatcher.execute(command("status", "{}", 0, 11)).await);
    let final_release = dispatcher
        .execute(command("call.release", &selected, 6, 12))
        .await;
    handler.shutdown().await.unwrap();
    assert!(!before);
    assert_eq!(receipts, 2);
    assert_eq!(starts, 1);
    assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
    // The actual call media, including every successful replacement, has closed.
    for port in FIRST..=LAST + 1 {
        assert!(std::net::UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_ok());
    }
    IvyJson::object([
        ("kind", "construction".into()),
        ("mode", n(mode)),
        ("secondBeforeRelease", before.into()),
        ("receiptsBeforeRelease", n(receipts)),
        (
            "invocationLockHeld",
            (receipts == 2 && !before && starts == 1).into(),
        ),
        (
            "rows",
            IvyJson::Array(vec![
                project("call.prepare", &prepared),
                project("call.hangup", &other_result),
                project("call.prepare", &replay),
                status,
                project("call.release", &retired),
                project("call.prepare", &next),
                project("call.prepare", &old_prepare),
                project("call.release", &old_release),
                replacement,
                project("call.release", &final_release),
            ]),
        ),
        ("failedPrepareProbe", IvyJson::Array(probe)),
        ("replaySame", same.into()),
        ("factories", n(factory.starts.load(Ordering::SeqCst))),
        ("ownedHeldPorts", n(held)),
        ("bindingReleased", released(bound).into()),
    ])
}

#[derive(Default)]
struct StatusGate {
    armed: AtomicBool,
    entered: AtomicBool,
    released: Mutex<bool>,
    changed: Condvar,
}
impl StatusGate {
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
}
struct PrefixDevice {
    gate: Arc<StatusGate>,
    reads: AtomicUsize,
    closes: AtomicUsize,
}
impl MicroVoiceInput for PrefixDevice {
    fn status(&self) -> MicroInputResult<MicroAttachmentStatus> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.gate.armed.swap(false, Ordering::SeqCst) {
            self.gate.entered.store(true, Ordering::SeqCst);
            let (released, _) = self
                .gate
                .changed
                .wait_timeout_while(
                    self.gate.released.lock().unwrap(),
                    Duration::from_secs(10),
                    |value| !*value,
                )
                .unwrap();
            assert!(*released, "Fixture status release unavailable");
        }
        Ok(MicroAttachmentStatus {
            state: "ready".into(),
            generation: Some(1),
            error_code: None,
        })
    }
    fn close(&self) -> MicroInputResult<MicroClose> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        Ok(MicroClose::completed(Ok(())))
    }
    fn send_microphone(
        &self,
        _: bool,
        _: i64,
        _: &dyn Fn() -> MicroInputResult<bool>,
    ) -> MicroInputResult<MicroInputReceipt> {
        panic!("No input authorized")
    }
}
fn micro_params(base: &str) -> String {
    [base.trim_end_matches('}'), r#", "micro":{"usbipExecutable":"C:/fixture/usbip.exe","executableHash":"sha256:0000000000000000000000000000000000000000000000000000000000000000","port":3240}}"#].concat()
}
async fn status_prefix(method: &'static str) -> IvyJson {
    let (owner, _, routes, base) = actual_owner(FIRST, LAST);
    let gate = Arc::new(StatusGate::default());
    let device = Arc::new(PrefixDevice {
        gate: gate.clone(),
        reads: AtomicUsize::new(0),
        closes: AtomicUsize::new(0),
    });
    let factory_device = device.clone();
    let input = VoiceInputOwner::new(move |_| Ok(Some(factory_device.clone())));
    let caller = Arc::new(Caller {
        input: input.clone(),
        closes: AtomicUsize::new(0),
    });
    let setup = input.clone();
    let status = input.clone();
    let handler = Arc::new(NativeSipCommands::new(
        owner.clone(),
        caller,
        move |micro| {
            let setup = setup.clone();
            Box::pin(async move {
                setup
                    .prepare_micro(micro.unwrap())
                    .map_err(|_| SipHostError::CleanupUnconfirmed)
            })
        },
        NativeSipViews::new(move || micro_json(&status), |_| Ok("null".into())),
    ));
    let dispatcher = Arc::new(NativeDispatcher::new(CALL.into(), 64, handler.clone()).unwrap());
    let configured = dispatcher
        .execute(command("configure", &micro_params(&base), 1, 1))
        .await;
    assert_eq!(configured.outcome().error_code(), None);
    let endpoint = owner.observation().endpoint.unwrap();
    gate.armed.store(true, Ordering::SeqCst);
    let first_dispatcher = dispatcher.clone();
    let first =
        tokio::spawn(async move { first_dispatcher.execute(command(method, "{}", 0, 2)).await });
    until(|| gate.entered.load(Ordering::SeqCst)).await;
    let second_dispatcher = dispatcher.clone();
    let second = tokio::spawn(async move {
        second_dispatcher
            .execute(command("configure", "{}", 2, 3))
            .await
    });
    sleep(Duration::from_millis(10)).await;
    let before = second.is_finished();
    let receipts = dispatcher.receipt_count();
    gate.release();
    let first = first.await.unwrap();
    let conflict = second.await.unwrap();
    let fresh = dispatcher.execute(command(method, "{}", 0, 4)).await;
    handler.shutdown().await.unwrap();
    assert!(!before);
    assert_eq!(receipts, 1);
    assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
    IvyJson::object([
        ("kind", "status".into()),
        ("method", method.into()),
        ("secondBeforeRelease", before.into()),
        ("receiptsBeforeRelease", n(receipts)),
        ("invocationLockHeld", (receipts == 1 && !before).into()),
        (
            "rows",
            IvyJson::Array(vec![
                project("status", &first),
                project("configure", &conflict),
                project("status", &fresh),
            ]),
        ),
        ("reads", n(device.reads.load(Ordering::SeqCst))),
        ("deviceDisposals", n(device.closes.load(Ordering::SeqCst))),
        ("bindingReleased", released(endpoint).into()),
    ])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn original_construction_retirement_replay_and_synchronous_status_prefix_agree() {
    let base = BASE
        .replace("57000", &FIRST.to_string())
        .replace("57998", &LAST.to_string());
    let mut inputs = Vec::new();
    let mut rows = Vec::new();
    for mode in 0..=3 {
        inputs.push(IvyJson::object([
            ("kind", "construction".into()),
            ("mode", n(mode)),
            ("params", parse(&base, 65536).unwrap()),
        ]));
        rows.push(construction(mode).await);
    }
    for method in ["status", "heartbeat"] {
        inputs.push(IvyJson::object([
            ("kind", "status".into()),
            ("method", method.into()),
            ("params", parse(&micro_params(&base), 65536).unwrap()),
        ]));
        rows.push(status_prefix(method).await);
    }
    let actual = IvyJson::Array(rows);
    trace("construction-prefix-inputs", IvyJson::Array(inputs.clone()));
    trace("construction-prefix-rust", actual.clone());
    let original = boundary_cases("construction", &inputs);
    trace("construction-prefix-original", original.clone());
    // Original process handle observations are retained verbatim, not represented
    // by invented Rust counts or taken as a proof of resource quiescence.
    let mut semantic = original.clone();
    let IvyJson::Array(rows) = &mut semantic else {
        panic!("Original rows unavailable")
    };
    for row in rows {
        if let IvyJson::Object(fields) = row {
            fields.retain(|(key, _)| key != &"resources".into());
        }
    }
    assert_eq!(actual, semantic);
}

struct PrefixRemaining {
    gate: Arc<Gate>,
    calls: Arc<AtomicUsize>,
}
impl NativeHandler for PrefixRemaining {
    fn cleanup_slots(&self) -> usize {
        0
    }
    fn start(&self, request: NativeRequest) -> NativeInvocationStart {
        let (gate, calls) = (self.gate.clone(), self.calls.clone());
        Box::pin(async move {
            gate.block().await;
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                NativeOutcome::result(format!(r#"{{"method":"{}"}}"#, request.method()))
            }) as NativeInvocation
        })
    }
    fn invoke(&self, _: NativeRequest) -> NativeInvocation {
        panic!("Original prefix must be forwarded")
    }
}
impl NativeRuntime for PrefixRemaining {
    fn shutdown(&self) -> NativeShutdown {
        Box::pin(async { Ok(()) })
    }
}
#[tokio::test]
async fn caller_owned_methods_keep_their_original_start_prefix() {
    let (owner, _, _, _) = actual_owner(FIRST, LAST);
    let gate = Arc::new(Gate::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let handler = Arc::new(NativeSipCommands::new(
        owner,
        Arc::new(PrefixRemaining {
            gate: gate.clone(),
            calls: calls.clone(),
        }),
        |_| Box::pin(async { Ok(()) }),
        closed_platform_views(),
    ));
    let dispatcher = NativeDispatcher::new(CALL.into(), 64, handler.clone()).unwrap();
    cancel(dispatcher.execute(command("inventory", "{}", 0, 1))).await;
    until(|| gate.entered.load(Ordering::SeqCst)).await;
    cancel(dispatcher.execute(command("configure", "{}", 1, 2))).await;
    sleep(Duration::from_millis(10)).await;
    assert_eq!(dispatcher.receipt_count(), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    gate.release();
    dispatcher.execute(command("configure", "{}", 1, 3)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    handler.shutdown().await.unwrap();
}
