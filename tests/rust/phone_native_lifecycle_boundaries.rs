use super::*;
use ivy_phone_bridge::{
    desktop::{MicroFactoryError, VoiceInputOwner},
    micro::{
        MicroAttachmentStatus, MicroClose, MicroInputReceipt, MicroInputResult, MicroVoiceInput,
    },
    native_rpc::NativeReply,
    native_sip::NativeSipError,
};
use std::{error::Error, fmt, net::SocketAddr, sync::OnceLock};

#[derive(Debug)]
struct Argument;
impl fmt::Display for Argument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("fixture argument")
    }
}
impl Error for Argument {}
fn classify(error: &(dyn Error + Send + Sync + 'static)) -> SipHostError {
    if error.is::<Argument>() {
        SipHostError::InvalidRequest
    } else {
        SipHostError::CleanupUnconfirmed
    }
}
fn released(endpoint: SocketAddr) -> bool {
    std::net::UdpSocket::bind(endpoint).is_ok()
}
struct Device {
    mode: usize,
    close_mode: usize,
    endpoint: Arc<OnceLock<SocketAddr>>,
    closes: AtomicUsize,
    caller_saw_released: Mutex<Option<bool>>,
}
impl MicroVoiceInput for Device {
    fn status(&self) -> MicroInputResult<MicroAttachmentStatus> {
        match self.mode {
            6 => Err(Box::new(Argument)),
            7 => Err(std::io::Error::other("fixture status").into()),
            _ => Ok(MicroAttachmentStatus {
                state: "ready".into(),
                generation: Some(1),
                error_code: None,
            }),
        }
    }
    fn close(&self) -> MicroInputResult<MicroClose> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        *self.caller_saw_released.lock().unwrap() = Some(released(*self.endpoint.get().unwrap()));
        if self.close_mode == 2 {
            return Err(std::io::Error::other("fixture dispose constructor").into());
        }
        Ok(MicroClose::completed(if self.close_mode == 1 {
            Err(Arc::new(std::io::Error::other("fixture dispose work")))
        } else {
            Ok(())
        }))
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
pub(super) struct Caller {
    pub(super) input: VoiceInputOwner,
    pub(super) closes: AtomicUsize,
}
impl NativeHandler for Caller {
    fn cleanup_slots(&self) -> usize {
        0
    }
    fn invoke(&self, _: NativeRequest) -> NativeInvocation {
        panic!("No platform command authorized")
    }
}
impl NativeRuntime for Caller {
    fn shutdown(&self) -> NativeShutdown {
        self.closes.fetch_add(1, Ordering::SeqCst);
        let work = self.input.close();
        Box::pin(async move {
            let work = work.map_err(|_| NativeShutdownError::OutcomeUnknown)?;
            work.wait()
                .await
                .map_err(|_| NativeShutdownError::OutcomeUnknown)
        })
    }
}
fn input_owner(mode: usize, device: Arc<Device>, factories: Arc<AtomicUsize>) -> VoiceInputOwner {
    VoiceInputOwner::new(move |_| {
        factories.fetch_add(1, Ordering::SeqCst);
        match mode {
            0 => Ok(None),
            1 | 2 => Err(MicroFactoryError::ClientUnavailable),
            3 => Err(MicroFactoryError::AttachmentConflict),
            4 => Err(MicroFactoryError::Other(Box::new(Argument))),
            5 => Err(MicroFactoryError::Other(
                std::io::Error::other("fixture factory").into(),
            )),
            _ => Ok(Some(device.clone())),
        }
    })
}
pub(super) fn micro_json(input: &VoiceInputOwner) -> Result<String, NativeSipError> {
    let status = input.micro_status().map_err(|e| classify(e.as_ref()))?;
    let value = status.map_or(IvyJson::Null, |s| {
        IvyJson::object([
            ("state", s.state.into()),
            (
                "generation",
                s.generation
                    .map_or(IvyJson::Null, |g| IvyJson::Number(g as f64)),
            ),
            (
                "errorCode",
                s.error_code.map_or(IvyJson::Null, IvyJson::from),
            ),
        ])
    });
    Ok(stringify(&value, 65536).unwrap())
}
fn params(base: String) -> String {
    [base.trim_end_matches('}'),r#", "micro":{"usbipExecutable":"C:/fixture/usbip.exe","executableHash":"sha256:0000000000000000000000000000000000000000000000000000000000000000","port":3240}}"#].concat()
}
fn project(method: &str, reply: &NativeReply) -> IvyJson {
    if let Some(error) = reply.outcome().error_code() {
        return IvyJson::object([("error", error.into())]);
    }
    let v = value(reply.outcome());
    match method {
        "configure" => IvyJson::object([("configured", v.get("configured").unwrap().clone())]),
        "call.prepare" => IvyJson::object([
            ("id", v.get("id").unwrap().clone()),
            ("state", v.get("state").unwrap().clone()),
        ]),
        "status" => IvyJson::object([
            ("configured", v.get("configured").unwrap().clone()),
            (
                "endpointPresent",
                (!v.get("endpoint").unwrap().is_null()).into(),
            ),
            ("micro", v.get("micro").unwrap().clone()),
        ]),
        _ => v,
    }
}
async fn partial(mode: usize, close_mode: usize) -> IvyJson {
    let (owner, factory, routes, base) = actual_owner(45000, 45998);
    let params = params(base);
    let endpoint = Arc::new(OnceLock::new());
    let device = Arc::new(Device {
        mode,
        close_mode,
        endpoint: endpoint.clone(),
        closes: AtomicUsize::new(0),
        caller_saw_released: Mutex::new(None),
    });
    let factories = Arc::new(AtomicUsize::new(0));
    let input = input_owner(mode, device.clone(), factories.clone());
    let caller = Arc::new(Caller {
        input: input.clone(),
        closes: AtomicUsize::new(0),
    });
    let setup = input.clone();
    let status = input.clone();
    let handler = Arc::new(NativeSipCommands::new(
        owner.clone(),
        caller.clone(),
        move |micro| {
            let setup = setup.clone();
            Box::pin(async move {
                setup
                    .prepare_micro(micro.unwrap())
                    .map_err(|e| classify(e.as_ref()))
            })
        },
        NativeSipViews::new(move || micro_json(&status), |_| Ok("null".into())),
    ));
    let dispatcher = NativeDispatcher::new(CALL.into(), 64, handler.clone()).unwrap();
    let selected = format!(r#"{{"callId":"{CALL}"}}"#);
    let configured = dispatcher
        .execute(command("configure", &params, 1, 1))
        .await;
    let mut rows = vec![project("configure", &configured)];
    let factories_after_configure = factories.load(Ordering::SeqCst);
    let bound = owner.observation().endpoint.unwrap();
    endpoint.set(bound).unwrap();
    let endpoint_bound = !released(bound);
    let replay = dispatcher
        .execute(command("configure", &params, 1, 2))
        .await;
    rows.push(project("configure", &replay));
    let replay_same = configured.outcome() == replay.outcome();
    rows.push(project(
        "configure",
        &dispatcher.execute(command("configure", "{}", 3, 3)).await,
    ));
    rows.push(project(
        "status",
        &dispatcher.execute(command("status", "{}", 0, 4)).await,
    ));
    let prepared = dispatcher
        .execute(command("call.prepare", &selected, 2, 5))
        .await;
    assert_eq!(
        prepared.outcome().error_code(),
        None,
        "fixture media must construct"
    );
    rows.push(project("call.prepare", &prepared));
    rows.push(project(
        "call.prepare",
        &dispatcher
            .execute(command("call.prepare", &selected, 2, 6))
            .await,
    ));
    rows.push(project(
        "call.release",
        &dispatcher
            .execute(command("call.release", &selected, 4, 7))
            .await,
    ));
    rows.push(project(
        "status",
        &dispatcher.execute(command("status", "{}", 0, 8)).await,
    ));
    drop(handler.shutdown());
    let disposed = handler.shutdown().await;
    assert_eq!(handler.shutdown().await, disposed);
    assert_eq!(caller.closes.load(Ordering::SeqCst), 1);
    assert_eq!(factory.starts.load(Ordering::SeqCst), 1);
    assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
    IvyJson::object([
        ("mode", n(mode)),
        ("closeMode", n(close_mode)),
        ("rows", IvyJson::Array(rows)),
        ("endpointBound", endpoint_bound.into()),
        ("factoriesAfterConfigure", n(factories_after_configure)),
        ("replaySame", replay_same.into()),
        ("ports", n(factory.starts.load(Ordering::SeqCst))),
        (
            "callerSawBindingReleased",
            device
                .caller_saw_released
                .lock()
                .unwrap()
                .map_or(IvyJson::Null, IvyJson::from),
        ),
        ("deviceDisposals", n(device.closes.load(Ordering::SeqCst))),
        (
            "disposalError",
            if disposed.is_err() {
                "operation_outcome_unknown".into()
            } else {
                IvyJson::Null
            },
        ),
        ("cachedDisposal", true.into()),
        ("bindingReleased", released(bound).into()),
    ])
}
async fn concurrent(mode: usize) -> IvyJson {
    let (owner, factory, _, base) = actual_owner(45000, 45998);
    let params = params(base);
    let gate = Arc::new(Gate::default());
    let entered = gate.clone();
    let factories = Arc::new(AtomicUsize::new(0));
    let count = factories.clone();
    let caller = Arc::new(Remaining::default());
    caller.close_gate.release();
    let handler = Arc::new(NativeSipCommands::new(
        owner.clone(),
        caller,
        move |_| {
            let gate = entered.clone();
            let count = count.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                gate.block().await;
                match mode {
                    4 => Err(SipHostError::InvalidRequest),
                    5 => Err(SipHostError::CleanupUnconfirmed),
                    _ => Ok(()),
                }
            })
        },
        closed_platform_views(),
    ));
    let dispatcher = NativeDispatcher::new(CALL.into(), 64, handler.clone()).unwrap();
    cancel(dispatcher.execute(command("configure", &params, 1, 1))).await;
    until(|| gate.entered.load(Ordering::SeqCst)).await;
    let bound = owner.observation().endpoint.unwrap();
    assert!(!released(bound));
    let selected = format!(r#"{{"callId":"{CALL}"}}"#);
    cancel(dispatcher.execute(command("call.prepare", &selected, 2, 2))).await;
    // Give the owned admission a chance to run. The single-thread runtime must
    // remain responsive while its original synchronous prefix is suspended.
    sleep(Duration::from_millis(10)).await;
    let before = dispatcher.receipt_count();
    let ports_before = factory.starts.load(Ordering::SeqCst);
    assert_eq!(before, 1);
    assert_eq!(ports_before, 0);
    let pending = timeout(
        Duration::from_millis(10),
        dispatcher.execute(command("call.prepare", &selected, 2, 3)),
    )
    .await;
    assert!(pending.is_err());
    gate.release();
    let configured = dispatcher
        .execute(command("configure", &params, 1, 4))
        .await;
    let prepared = dispatcher
        .execute(command("call.prepare", &selected, 2, 5))
        .await;
    assert_eq!(
        prepared.outcome().error_code(),
        None,
        "fixture media must construct"
    );
    let replay = dispatcher
        .execute(command("call.prepare", &selected, 2, 6))
        .await;
    assert_eq!(prepared.outcome(), replay.outcome());
    dispatcher
        .execute(command("call.release", &selected, 4, 7))
        .await;
    handler.shutdown().await.unwrap();
    IvyJson::object([
        ("mode", n(mode)),
        ("secondBeforeRelease", false.into()),
        ("endpointBound", true.into()),
        ("receiptsBeforeRelease", n(before)),
        ("portsBeforeRelease", n(ports_before)),
        ("configured", project("configure", &configured)),
        ("prepared", project("call.prepare", &prepared)),
        ("replay", project("call.prepare", &replay)),
        ("replaySame", true.into()),
        ("factories", n(factories.load(Ordering::SeqCst))),
        ("ports", n(factory.starts.load(Ordering::SeqCst))),
        ("bindingReleased", released(bound).into()),
        ("invocationLockHeld", true.into()),
    ])
}
#[cfg(windows)]
#[tokio::test]
async fn original_entered_configuration_admission_partial_start_and_disposal_agree() {
    let base = params(BASE.replace("57000", "45000").replace("57998", "45998"));
    let mut inputs = Vec::new();
    let mut partial_rows = Vec::new();
    let mut concurrent_rows = Vec::new();
    for (mode, close_mode) in (0..=8).map(|m| (m, 0)).chain([(8, 1), (8, 2)]) {
        inputs.push(IvyJson::object([
            ("kind", "partial".into()),
            ("mode", n(mode)),
            ("closeMode", n(close_mode)),
            ("params", parse(&base, 65536).unwrap()),
        ]));
        partial_rows.push(partial(mode, close_mode).await);
    }
    for mode in [0, 4, 5] {
        inputs.push(IvyJson::object([
            ("kind", "concurrent".into()),
            ("mode", n(mode)),
            ("params", parse(&base, 65536).unwrap()),
        ]));
        concurrent_rows.push(concurrent(mode).await);
    }
    let actual = IvyJson::object([
        ("partial", IvyJson::Array(partial_rows)),
        ("concurrent", IvyJson::Array(concurrent_rows)),
    ]);
    trace("lifecycle-boundary-inputs", IvyJson::Array(inputs.clone()));
    trace("lifecycle-boundary-rust", actual.clone());
    let original = boundary_cases("boundaries", &inputs);
    trace("lifecycle-boundary-original", original.clone());
    assert_eq!(actual, original);
}

#[tokio::test]
async fn stopping_waits_for_the_entered_setup_prefix_without_cancelling_it() {
    let (owner, _, _, params) = actual_owner(45000, 45998);
    let gate = Arc::new(Gate::default());
    let setup = gate.clone();
    let caller = Arc::new(Remaining::default());
    caller.close_gate.release();
    let handler = Arc::new(NativeSipCommands::new(
        owner.clone(),
        caller,
        move |_| {
            let setup = setup.clone();
            Box::pin(async move {
                setup.block().await;
                Ok(())
            })
        },
        closed_platform_views(),
    ));
    let dispatcher = NativeDispatcher::new(CALL.into(), 16, handler.clone()).unwrap();
    cancel(dispatcher.execute(command("configure", &params, 1, 1))).await;
    until(|| gate.entered.load(Ordering::SeqCst)).await;
    let mut stopping = Box::pin(dispatcher.stop());
    assert!(
        timeout(Duration::from_millis(10), &mut stopping)
            .await
            .is_err()
    );
    assert!(!owner.observation().stopping);
    gate.release();
    stopping.await;
    let reply = dispatcher
        .execute(command("configure", &params, 1, 2))
        .await;
    assert_eq!(reply.outcome().error_code(), Some("runtime_stopping"));
    handler.shutdown().await.unwrap();
    trace(
        "lifecycle-boundary-stop",
        IvyJson::object([
            ("enteredPrefixRetained", true.into()),
            ("stopPrecedesReplay", true.into()),
        ]),
    );
}
