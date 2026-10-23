use super::*;
use serde_json::{Value, json};
use std::path::Path;
const DESKTOP: &str = r#"{"pid":123,"startTimeUtcTicks":"1","imagePath":"C:\\fixture\\voice.exe","appUserModelId":"Fixture.Package!UI"}"#;
const SETTINGS: &str = r#"{"captureEndpointId":"capture-fixture","renderEndpointId":"render-fixture","sourceMode":"desktop_process","captureBufferMs":20,"renderLatencyMs":10,"queueMs":60}"#;
fn params(prepare: bool) -> String {
    if prepare {
        format!(r#"{{"callId":"{CALL}","settings":{SETTINGS},"desktop":{DESKTOP}}}"#)
    } else {
        format!(r#"{{"callId":"{CALL}","desktop":{DESKTOP},"threadId":"{CALL}"}}"#)
    }
}
fn cases() -> Vec<Value> {
    let mut cases = Vec::new();
    for stage in ["unconfigured", "ready", "prepared", "closed"] {
        for (prepare, variants) in [
            (
                true,
                vec![
                    "valid",
                    "settings-null",
                    "settings-type",
                    "settings-bounds",
                    "desktop-null",
                    "desktop-type",
                    "desktop-format",
                    "extra",
                ],
            ),
            (
                false,
                vec![
                    "valid",
                    "thread-format",
                    "thread-type",
                    "desktop-null",
                    "desktop-type",
                    "desktop-format",
                ],
            ),
        ] {
            for variant in variants {
                let mut value: Value = serde_json::from_str(&params(prepare)).unwrap();
                match variant {
                    "settings-null" => value["settings"] = Value::Null,
                    "settings-type" => value["settings"] = json!(7),
                    "settings-bounds" => value["settings"]["queueMs"] = json!(0),
                    "desktop-null" => value["desktop"] = Value::Null,
                    "desktop-type" => value["desktop"] = json!(7),
                    "desktop-format" => value["desktop"]["startTimeUtcTicks"] = json!("01"),
                    "thread-format" => value["threadId"] = json!("not-a-guid"),
                    "thread-type" => value["threadId"] = json!(7),
                    "extra" => value["extra"] = Value::Null,
                    _ => {}
                }
                cases.push(json!({"name":format!("{stage}:{}:{variant}",if prepare{"prepare"}else{"rebind"}),"stage":stage,"method":if prepare{"call.audio.prepare"}else{"call.audio.rebind"},"params":value.to_string()}));
            }
        }
    }
    for (name, raw) in [
        ("pid-fraction", DESKTOP.replace("123", "123.0")),
        ("pid-exponent", DESKTOP.replace("123", "123e0")),
        ("pid-overflow", DESKTOP.replace("123", "2147483648")),
    ] {
        cases.push(json!({"name":name,"stage":"prepared","method":"call.audio.prepare","params":params(true).replace(DESKTOP,&raw)}));
    }
    cases.push(json!({"name":"settings-fraction","stage":"prepared","method":"call.audio.prepare","params":params(true).replace("\"captureBufferMs\":20","\"captureBufferMs\":20.0")}));
    assert_eq!(cases.len(), 60);
    cases
}
fn save(name: &str, value: &Value) {
    if let Some(root) = std::env::var_os("IVY_PHONE_NATIVE_AUDIO_TRACE") {
        std::fs::write(
            Path::new(&root).join(name),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }
}
fn projection(out: &NativeOutcome) -> Value {
    match out.error_code() {
        Some(error) => json!({"error":error}),
        None => {
            let v: Value = serde_json::from_str(out.result_json().unwrap()).unwrap();
            json!({"callId":v["callId"],"state":v["state"],"routePresent":!v["route"].is_null()})
        }
    }
}
fn handler(owner: NativeSipOperations) -> (Arc<NativeSipCommands>, Arc<Remaining>) {
    let remaining = Arc::new(Remaining::default());
    remaining.close_gate.release();
    let commands = Arc::new(NativeSipCommands::new(
        owner,
        remaining.clone(),
        |_| Box::pin(async { Ok(()) }),
        NativeSipViews::new(|| Ok("null".into()), |_| Ok("null".into())),
    ));
    (commands, remaining)
}
async fn scenario(row: &Value) -> Value {
    let route = Arc::new(Route::new());
    let routes = Arc::new(Factory::routes(vec![route.clone()]));
    let media = Arc::new(NativeSipSessionFactory::new(
        routes.clone(),
        None,
        Arc::new(Clock),
        |_| Arc::new(Events::default()),
    ));
    let owner = NativeSipOperations::new(media);
    let stage = row["stage"].as_str().unwrap();
    let mut endpoint = None;
    let mut transport = None;
    if stage != "unconfigured" {
        endpoint = Some(
            owner
                .configure(&configuration(BASE), |_| async { Ok(()) })
                .unwrap()
                .wait()
                .await
                .unwrap(),
        );
    }
    if matches!(stage, "prepared" | "closed") {
        let call = owner.prepare(CALL.into()).unwrap();
        call.prepared().await.unwrap();
        transport = Some(call.media().unwrap().observation().receive.transport);
    }
    if stage == "closed" {
        owner.close().wait().await.unwrap();
    }
    let (commands, remaining) = handler(owner);
    let rpc = NativeDispatcher::new(CALL.into(), 128, commands.clone()).unwrap();
    let method = row["method"].as_str().unwrap();
    let params = row["params"].as_str().unwrap();
    let mut responses = vec![projection(
        rpc.execute(command(method, params, 1, 1)).await.outcome(),
    )];
    if row["name"] == "prepared:prepare:valid" {
        responses.push(projection(
            rpc.execute(command(method, params, 1, 2)).await.outcome(),
        ));
        responses.push(projection(
            rpc.execute(command(method, params, 2, 3)).await.outcome(),
        ));
    }
    commands.shutdown().await.unwrap();
    assert!(remaining.methods.lock().unwrap().is_empty());
    let sip_released = endpoint.is_none_or(|e| std::net::UdpSocket::bind(e).is_ok());
    let media_released = transport.is_none_or(|t| {
        std::net::UdpSocket::bind(t.local_rtp).is_ok()
            && std::net::UdpSocket::bind(t.local_rtcp).is_ok()
    });
    assert!(sip_released && media_released);
    json!({"name":row["name"],"responses":responses,"routeOpens":routes.opens.load(Ordering::SeqCst),"routeDisposals":route.state.lock().unwrap().disposals,"sipReleased":sip_released})
}
fn original(cases: &[Value]) -> Value {
    let input = json!(cases);
    original_named(&input, "")
}
fn original_named(input: &Value, prefix: &str) -> Value {
    let data: Value =
        serde_json::from_str(include_str!("../fixtures/phone-native-platform-cases.json")).unwrap();
    let row = &data[if prefix.is_empty() {
        "audio"
    } else {
        assert_eq!(prefix, "connected-");
        "connected"
    }];
    assert_eq!(&row["cases"], input);
    row["expected"].clone()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn original_audio_dto_guard_order_replay_and_route_retirement_agree() {
    let cases = cases();
    let mut rows = Vec::new();
    for row in &cases {
        rows.push(scenario(row).await);
    }
    let actual = Value::Array(rows);
    save("rust.json", &actual);
    let expected = original(&cases);
    let differences: Vec<_> = actual
        .as_array()
        .unwrap()
        .iter()
        .zip(expected.as_array().unwrap())
        .filter(|(a, b)| a != b)
        .map(|(a, b)| json!({"rust":a,"original":b}))
        .collect();
    save(
        "comparisons.json",
        &json!({"cases":60,"differences":differences,"rust":actual,"original":expected,"rustSIPAndRTPBoundSocketsRetire":true,"scope":"Actual native RPC/call/media audio preparation and rebind admission. Inert audio routes; no audio/device/input or dial. Audio result projects callId/state/route presence; provider-specific route status and ephemeral ports are excluded. SIP retirement is observed in both; positive RTP-port rebind is Rust-only, not asserted for the original. Connected successful Voice recovery is not exercised."}),
    );
    assert_eq!(actual, expected);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_audio_preparation_keeps_original_receipt_during_release_and_id_reuse() {
    let route = Arc::new(Route::new());
    let gate = Arc::new(Gate::default());
    let mut raw_factory = Factory::routes(vec![route.clone()]);
    raw_factory.gate = Some(gate.clone());
    let routes = Arc::new(raw_factory);
    let media = Arc::new(NativeSipSessionFactory::new(
        routes.clone(),
        None,
        Arc::new(Clock),
        |_| Arc::new(Events::default()),
    ));
    let owner = NativeSipOperations::new(media);
    owner
        .configure(&configuration(BASE), |_| async { Ok(()) })
        .unwrap()
        .wait()
        .await
        .unwrap();
    let call = owner.prepare(CALL.into()).unwrap();
    call.prepared().await.unwrap();
    let (commands, remaining) = handler(owner.clone());
    let rpc = NativeDispatcher::new(CALL.into(), 128, commands.clone()).unwrap();
    let params = params(true);
    cancel(rpc.execute(command("call.audio.prepare", &params, 1, 1))).await;
    gate.wait().await;
    cancel(rpc.execute(command("call.audio.prepare", &params, 1, 2))).await;
    assert_eq!(routes.opens.load(Ordering::SeqCst), 1);
    let releasing = owner.release(&call).unwrap();
    drop(releasing);
    sleep(Duration::from_millis(25)).await;
    assert_eq!(owner.observation().calls.len(), 1);
    gate.release();
    owner.release(&call).unwrap().wait().await.unwrap();
    assert_eq!(route.state.lock().unwrap().disposals, 1);
    let retired = rpc
        .execute(command("call.audio.prepare", &params, 1, 3))
        .await;
    assert_eq!(
        retired.outcome().error_code(),
        Some("operation_outcome_unknown")
    );
    let next = owner.prepare(CALL.into()).unwrap();
    next.prepared().await.unwrap();
    let replay = rpc
        .execute(command("call.audio.prepare", &params, 1, 4))
        .await;
    assert_eq!(
        replay.outcome().error_code(),
        retired.outcome().error_code()
    );
    assert_eq!(routes.opens.load(Ordering::SeqCst), 1);
    owner.release(&next).unwrap().wait().await.unwrap();
    commands.shutdown().await.unwrap();
    assert!(remaining.methods.lock().unwrap().is_empty());
    save(
        "retention.json",
        &json!({"droppedObserversRetainOneAudioOpen":true,"callSlotHeldDuringPendingPreparation":true,"routeDisposedExactlyOnce":true,"originalUnknownReceiptSurvivesCallIdReuse":true,"liveCallsAfterCleanup":owner.observation().calls.len(),"scope":"Rust-only pending inert factory. No original OS failure or connected Voice rebind qualification."}),
    );
}

struct RecoveryFactory {
    inner: Factory,
    first: Arc<Route>,
    old_retired_before_open: AtomicBool,
}
impl CallAudioFactory for RecoveryFactory {
    fn open<'a>(
        &'a self,
        settings: &'a AudioSettings,
        desktop: Option<&'a AudioDesktopIdentity>,
        permit: AudioRoutePermit,
        stop: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<Arc<dyn CallAudioRoute>, CallAudioError>> + Send + 'a>>
    {
        Box::pin(async move {
            if self.inner.opens.load(Ordering::SeqCst) == 1 {
                let state = self.first.state.lock().unwrap();
                assert_eq!((state.mode, state.disposals), ("closed", 1));
                self.old_retired_before_open.store(true, Ordering::SeqCst);
            }
            self.inner.open(settings, desktop, permit, stop).await
        })
    }
}
fn connected_params(prepare: bool) -> String {
    params(prepare)
        .replace("\"pid\":123", "\"pid\":101")
        .replace("Fixture.Package!UI", "Phone.Fixture!App")
}
struct Connected {
    owner: NativeSipOperations,
    commands: Arc<NativeSipCommands>,
    rpc: NativeDispatcher,
    peer: UdpSocket,
    media_peer: (UdpSocket, UdpSocket),
    ctx: Arc<super::desktop::Context>,
    routes: Arc<RecoveryFactory>,
    first: Arc<Route>,
    second: Arc<Route>,
    endpoint: std::net::SocketAddr,
    transport: ivy_phone_bridge::media::RtpTransportObservation,
}
impl Connected {
    async fn new(close_gate: Option<Arc<Gate>>) -> Self {
        use rsipstack::sip::Method;
        let mut first = Route::new();
        first.close_gate = close_gate;
        let first = Arc::new(first);
        let second = Arc::new(Route::new());
        let routes = Arc::new(RecoveryFactory {
            inner: Factory::routes(vec![first.clone(), second.clone()]),
            first: first.clone(),
            old_retired_before_open: AtomicBool::new(false),
        });
        let ctx = Arc::new(super::desktop::Context::default());
        let (desktop, voice) = super::desktop::runtime(ctx.clone());
        let owner = NativeSipOperations::with_desktop_runtime(
            Arc::new(NativeSipSessionFactory::new(
                routes.clone(),
                None,
                Arc::new(Clock),
                |_| Arc::new(Events::default()),
            )),
            desktop,
            voice,
        );
        let endpoint = owner
            .configure(&configuration(BASE), |_| async { Ok(()) })
            .unwrap()
            .wait()
            .await
            .unwrap();
        let call = owner.prepare(CALL.into()).unwrap();
        call.prepared().await.unwrap();
        let transport = call.media().unwrap().observation().receive.transport;
        let (commands, _) = handler(owner.clone());
        let rpc = NativeDispatcher::new(CALL.into(), 128, commands.clone()).unwrap();
        let prepared = rpc
            .execute(command("call.audio.prepare", &connected_params(true), 1, 1))
            .await;
        assert_eq!(prepared.outcome().error_code(), None);
        let features = rpc
            .execute(command(
                "call.features",
                &format!(r#"{{"callId":"{CALL}","challenge":null}}"#),
                2,
                2,
            ))
            .await;
        assert_eq!(features.outcome().error_code(), None);
        let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let media_peer = pair().await;
        let dial = format!(
            r#"{{"callId":"{CALL}","destination":"sip:peer@{}","username":"fixture-user","password":null,"ringSeconds":5,"waiting":false}}"#,
            peer.local_addr().unwrap()
        );
        let work = rpc.execute(command("call.dial", &dial, 3, 3));
        let accept = async {
            let (invite, remote) = crate::sip_host::received(&peer).await;
            assert_eq!(invite.method, Method::Invite);
            assert_eq!(remote, endpoint);
            let sdp = format!(
                "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {} RTP/AVP 8\r\na=rtpmap:8 PCMA/8000\r\na=sendrecv\r\n",
                media_peer.0.local_addr().unwrap().port()
            );
            let extra = format!(
                "Contact: <sip:peer@{}>\r\nContent-Type: application/sdp\r\n",
                peer.local_addr().unwrap()
            );
            crate::sip_host::send(
                &peer,
                remote,
                &crate::sip_host::response(&invite, 200, &extra, &sdp),
            )
            .await;
            assert_eq!(crate::sip_host::received(&peer).await.0.method, Method::Ack);
        };
        let (dialed, ()) = tokio::join!(work, accept);
        assert_eq!(dialed.outcome().error_code(), None);
        assert_eq!(
            owner.find(CALL).unwrap().observation().phase,
            ivy_phone_bridge::sip::SipHostCallPhase::Connected
        );
        let voice = rpc
            .execute(command(
                "call.desktop.startVoice",
                &super::desktop::params("call.desktop.startVoice"),
                4,
                4,
            ))
            .await;
        assert_eq!(voice.outcome().error_code(), None);
        let result: Value = serde_json::from_str(voice.outcome().result_json().unwrap()).unwrap();
        assert_eq!(result["result"]["state"], "active");
        assert!(ctx.capture.load(Ordering::SeqCst));
        assert!(
            owner
                .find(CALL)
                .unwrap()
                .media()
                .unwrap()
                .call_audio()
                .state()
                .open
        );
        Self {
            owner,
            commands,
            rpc,
            peer,
            media_peer,
            ctx,
            routes,
            first,
            second,
            endpoint,
            transport,
        }
    }
    async fn bye(&self) {
        use rsipstack::sip::Method;
        let (bye, remote) = crate::sip_host::received(&self.peer).await;
        assert_eq!(bye.method, Method::Bye);
        crate::sip_host::send(
            &self.peer,
            remote,
            &crate::sip_host::response(&bye, 200, "", ""),
        )
        .await;
    }
    async fn close(&self, mode: &str) {
        let call = self.owner.find(CALL).unwrap();
        self.owner.hangup(&call).unwrap();
        self.bye().await;
        let closed = self.commands.shutdown().await;
        assert!(
            closed.is_ok(),
            "{mode}: shutdown={closed:?}, call={:?}, audio={:?}, capture={}, chords={}, routeDisposals={}/{}",
            call.observation(),
            call.media().unwrap().call_audio().status().state,
            self.ctx.capture.load(Ordering::SeqCst),
            self.ctx.chords.load(Ordering::SeqCst),
            self.first.state.lock().unwrap().disposals,
            self.second.state.lock().unwrap().disposals,
        );
        assert!(self.owner.observation().calls.is_empty());
        assert!(std::net::UdpSocket::bind(self.endpoint).is_ok());
        assert!(std::net::UdpSocket::bind(self.transport.local_rtp).is_ok());
        assert!(std::net::UdpSocket::bind(self.transport.local_rtcp).is_ok());
        // Retain both owned peer media bindings through native cleanup.
        assert_ne!(self.media_peer.0.local_addr().unwrap().port(), 0);
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connected_voice_rebind_identity_replay_and_replacement_match_original() {
    let modes = ["healthy", "foreign", "capture-lost", "recover"];
    let mut rows = vec![];
    for mode in modes {
        let c = Connected::new(None).await;
        if mode != "healthy" {
            c.first.mode("failed");
        }
        if mode == "capture-lost" {
            c.ctx.capture.store(false, Ordering::SeqCst);
        }
        let mut params = connected_params(false);
        if mode == "foreign" {
            params = params.replace("\"pid\":101", "\"pid\":102");
        }
        if mode == "recover" {
            params = params.replace("fixture\\\\voice.exe", "FIXTURE\\\\VOICE.EXE");
            params = params.replace(
                &format!("\"threadId\":\"{CALL}\""),
                "\"threadId\":\"22222222-2222-4222-8222-222222222222\"",
            );
        }
        let first = c
            .rpc
            .execute(command("call.audio.rebind", &params, 10, 10))
            .await;
        let mut responses = vec![projection(first.outcome())];
        match mode {
            "healthy" => assert_eq!(first.outcome().error_code(), Some("runtime_not_ready")),
            "foreign" | "capture-lost" => assert_eq!(
                first.outcome().error_code(),
                Some("phone_audio_owner_changed")
            ),
            _ => {
                assert_eq!(first.outcome().error_code(), None);
                assert!(c.routes.old_retired_before_open.load(Ordering::SeqCst));
                responses.push(projection(
                    c.rpc
                        .execute(command("call.audio.rebind", &params, 10, 11))
                        .await
                        .outcome(),
                ));
                responses.push(projection(
                    c.rpc
                        .execute(command("call.audio.rebind", &params, 11, 12))
                        .await
                        .outcome(),
                ));
                assert_eq!(responses[0], responses[1]);
                assert_eq!(responses[2], json!({"error":"runtime_not_ready"}));
            }
        }
        // Restore the original inert capture after the negative observation.
        // Its keyboard provider models a toggle, so leaving capture absent
        // would make the later cleanup gesture start a synthetic new capture.
        if mode == "capture-lost" {
            c.ctx.capture.store(true, Ordering::SeqCst);
        }
        c.close(mode).await;
        rows.push(json!({"name":mode,"responses":responses,"opens":c.routes.inner.opens.load(Ordering::SeqCst),"oldRetiredBeforeReplacement":c.routes.old_retired_before_open.load(Ordering::SeqCst),"firstDisposals":c.first.state.lock().unwrap().disposals,"secondDisposals":c.second.state.lock().unwrap().disposals,"chords":c.ctx.chords.load(Ordering::SeqCst),"sipReleased":true}));
    }
    let actual = json!(rows);
    save("connected-rust.json", &actual);
    let expected = original_named(&json!({"connected":modes}), "connected-");
    save(
        "connected-comparisons.json",
        &json!({"cases":4,"rust":actual,"original":expected,"equal":actual==expected,"scope":"Actual owned loopback connected SIP/native command/Voice/audio owners with inert Desktop/capture/input/routes. Audio response projects callId/state/route presence; provider status/ephemeral ports are omitted. SIP retirement observed in both; RTP-port retirement Rust-only. Replacement route is delivered only after old route close. Recovery uses case-insensitive image and an unrelated valid thread GUID, matching original behavior. No real call, audio, device or user input."}),
    );
    assert_eq!(actual, expected);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_rebind_keeps_original_receipt_and_call_until_release_finishes() {
    let gate = Arc::new(Gate::default());
    let c = Connected::new(Some(gate.clone())).await;
    c.first.mode("failed");
    let params = connected_params(false);
    cancel(c.rpc.execute(command("call.audio.rebind", &params, 10, 10))).await;
    gate.wait().await;
    cancel(c.rpc.execute(command("call.audio.rebind", &params, 10, 11))).await;
    assert_eq!(c.routes.inner.opens.load(Ordering::SeqCst), 1);
    let call = c.owner.find(CALL).unwrap();
    drop(c.owner.release(&call).unwrap());
    c.bye().await;
    assert_eq!(c.owner.observation().calls.len(), 1);
    assert!(!c.second.state().open);
    gate.release();
    c.owner.release(&call).unwrap().wait().await.unwrap();
    let ended = c
        .rpc
        .execute(command("call.audio.rebind", &params, 10, 12))
        .await;
    assert_eq!(
        ended.outcome().error_code(),
        Some("phone_audio_owner_changed")
    );
    let next = c.owner.prepare(CALL.into()).unwrap();
    next.prepared().await.unwrap();
    let replay = c
        .rpc
        .execute(command("call.audio.rebind", &params, 10, 13))
        .await;
    assert_eq!(replay.outcome().error_code(), ended.outcome().error_code());
    assert_eq!(c.routes.inner.opens.load(Ordering::SeqCst), 1);
    c.owner.release(&next).unwrap().wait().await.unwrap();
    c.commands.shutdown().await.unwrap();
    assert_eq!(c.first.state.lock().unwrap().disposals, 1);
    assert_eq!(c.second.state.lock().unwrap().disposals, 0);
    assert!(std::net::UdpSocket::bind(c.endpoint).is_ok());
    save(
        "connected-retention.json",
        &json!({"droppedOriginalAndReplayObserversRetainOneRebind":true,"oldCloseCompletesBeforeReplacement":true,"releaseHoldsCallSlotUntilOldRouteClose":true,"revokedReplacementNeverOpened":true,"originalOwnerChangedReceiptSurvivesCallIdReuse":true,"firstDisposals":1,"secondDisposals":0,"liveCallsAfterCleanup":c.owner.observation().calls.len(),"scope":"Rust-only controlled old-route close gate with actual connected SIP/native/Voice owners and inert providers; no original OS failure or hardware proof."}),
    );
}
