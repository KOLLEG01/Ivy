use super::feedback::row;
use super::*;
use ivy_phone_bridge::native_sip::NativeSipCall;

pub(super) struct Scenario<'a> {
    pub settings: &'a str,
    pub gate: Arc<Gate>,
    pub routes: Arc<Factory>,
    pub audio_cases: &'a [String],
}
fn settings() -> &'static str {
    r#"{"captureEndpointId":"capture-fixture","renderEndpointId":"render-fixture","sourceMode":"microphone","captureBufferMs":20,"renderLatencyMs":10,"queueMs":60}"#
}
fn params(settings: &str) -> String {
    format!(r#"{{"callId":"{CALL}","settings":{settings}}}"#)
}
pub(super) fn ends_itself(scenario: &str) -> bool {
    matches!(
        scenario,
        "screening_timeout" | "screening_decline" | "screening_end_during_bridge"
    )
}
pub(super) async fn prepare(
    dispatcher: &NativeDispatcher,
    settings: &str,
    base: u64,
) -> Vec<IvyJson> {
    let params = params(settings);
    let rows = vec![
        row(dispatcher, "call.screening.prepare", &params, base).await,
        row(dispatcher, "call.screening.prepare", &params, base + 1).await,
    ];
    assert_eq!(
        rows[0]
            .get("output")
            .unwrap()
            .get("json")
            .unwrap()
            .as_str()
            .unwrap(),
        "{\"prepared\":true}"
    );
    assert_eq!(
        rows[1]
            .get("output")
            .unwrap()
            .get("error")
            .unwrap()
            .as_str()
            .unwrap(),
        "operation_outcome_unknown"
    );
    rows
}
pub(super) async fn connected(
    call: &NativeSipCall,
    dispatcher: &Arc<NativeDispatcher>,
    rtp: &UdpSocket,
    scenario: &str,
    work: &Scenario<'_>,
    base: u64,
    rows: &mut Vec<IvyJson>,
) {
    assert_eq!(call.features().observation().screening, "waiting");
    let mut packet = [0u8; 2048];
    timeout(Duration::from_secs(1), async {
        loop {
            let (length, _) = rtp.recv_from(&mut packet).await.unwrap();
            assert_eq!(length, 172);
            assert_eq!(packet[0], 128);
            assert_eq!(packet[1] & 127, 8);
            if packet[12..length].iter().any(|byte| *byte != 213) {
                break;
            }
        }
    })
    .await
    .unwrap();
    rows.push(
        row(
            dispatcher,
            "call.screening.bridge",
            &params("null"),
            base + rows.len() as u64,
        )
        .await,
    );
    if scenario == "screening_retired" {
        return;
    }
    if scenario == "screening_decline" {
        call.features().digit('2', InputSource::SipInfo).unwrap();
        until(|| call.observation().ending).await;
        return;
    }
    if scenario == "screening_timeout" {
        until(|| call.features().observation().screening == "timeout").await;
        return;
    }
    call.features().digit('1', InputSource::SipInfo).unwrap();
    assert_eq!(call.features().observation().screening, "accepted");
    let drain_until = tokio::time::Instant::now() + Duration::from_millis(150);
    while timeout(
        drain_until.saturating_duration_since(tokio::time::Instant::now()),
        rtp.recv_from(&mut packet),
    )
    .await
    .is_ok()
    {}
    let (length, _) = timeout(Duration::from_secs(1), rtp.recv_from(&mut packet))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(length, 172);
    assert!(packet[12..length].iter().all(|byte| *byte == 213));
    if scenario == "screening_accept_late_bridge" {
        sleep(Duration::from_millis(1200)).await;
    }
    for settings in work.audio_cases {
        let next = row(
            dispatcher,
            "call.screening.bridge",
            &params(settings),
            base + rows.len() as u64,
        )
        .await;
        assert_eq!(
            next.get("output")
                .unwrap()
                .get("error")
                .unwrap()
                .as_str()
                .unwrap(),
            "invalid_request"
        );
        rows.push(next);
    }
    rows.push(
        row(
            dispatcher,
            "call.screening.bridge",
            &params("{}"),
            base + rows.len() as u64,
        )
        .await,
    );
    if scenario == "screening_end_during_bridge" {
        let original = dispatcher.clone();
        let raw = params(settings());
        let sequence = base + rows.len() as u64;
        let raw_copy = raw.clone();
        let observer = tokio::spawn(async move {
            row(&original, "call.screening.bridge", &raw_copy, sequence).await
        });
        until(|| work.routes.opens.load(Ordering::SeqCst) == 3).await;
        observer.abort(); // Native work and original route preparation remain retained.
        let ended = row(
            dispatcher,
            "call.hangup",
            &format!(r#"{{"callId":"{CALL}"}}"#),
            sequence + 1,
        )
        .await;
        work.gate.release();
        rows.push(row(dispatcher, "call.screening.bridge", &raw, sequence).await);
        rows.push(ended);
        return;
    }
    for method in [
        "call.screening.bridge",
        "call.screening.bridge",
        "call.windows.connect",
    ] {
        rows.push(
            row(
                dispatcher,
                method,
                &params(settings()),
                base + rows.len() as u64,
            )
            .await,
        );
    }
    if scenario == "screening_bridge" {
        sleep(Duration::from_millis(1200)).await;
    }
    for key in ['*', '0', '#'] {
        call.features().digit(key, InputSource::SipInfo).unwrap();
    }
    assert_eq!(call.features().observation().screening, "bridged");
    assert_eq!(call.features().observation().command_sequence, 0);
    assert_eq!(call.media().unwrap().call_audio().status().state, "open");
}

struct NativeRoute(Arc<dyn CallAudioRoute>);
impl PcmAudioPort for NativeRoute {
    fn state(&self) -> PcmAudioState {
        self.0.state()
    }
    fn read_captured(&self, out: &mut [f32], generation: u64) -> usize {
        self.0.read_captured(out, generation)
    }
    fn write_received(&self, input: &[f32], generation: u64) -> bool {
        self.0.write_received(input, generation)
    }
}
impl PcmSendPort for NativeRoute {
    fn with_send_state(&self, state: PcmAudioState, action: &mut dyn FnMut()) -> bool {
        self.0.with_send_state(state, action)
    }
    fn send_health(&self) -> AudioSendHealth {
        self.0.send_health()
    }
}
impl CallAudioRoute for NativeRoute {
    fn status(&self) -> IvyJson {
        IvyJson::object([
            ("state", self.0.status().get("state").unwrap().clone()),
            ("sourceMode", "microphone".into()),
            ("sampleRate", n(48000)),
            ("channels", n(1)),
            ("captureDroppedSamples", n(0)),
            ("renderDroppedSamples", n(0)),
            ("captureQueuedSamples", n(0)),
            ("renderQueuedSamples", n(0)),
            ("renderLatencyMs", n(10)),
            ("lowLatencyActive", IvyJson::Null),
            ("lowLatencyUnavailableReason", IvyJson::Null),
        ])
    }
    fn failed(&self) -> bool {
        self.0.failed()
    }
    fn resume(&self) -> Result<(), CallAudioError> {
        self.0.resume()
    }
    fn suspend(&self) -> Result<(), CallAudioError> {
        self.0.suspend()
    }
    fn close(&self) -> Pin<Box<dyn Future<Output = Result<(), CallAudioError>> + Send + '_>> {
        self.0.close()
    }
}
struct NativeFactory(Arc<Factory>);
impl CallAudioFactory for NativeFactory {
    fn open<'a>(
        &'a self,
        settings: &'a AudioSettings,
        desktop: Option<&'a AudioDesktopIdentity>,
        permit: AudioRoutePermit,
        stop: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<Arc<dyn CallAudioRoute>, CallAudioError>> + Send + 'a>>
    {
        Box::pin(async move {
            Ok(Arc::new(NativeRoute(
                self.0.open(settings, desktop, permit, stop).await?,
            )) as Arc<dyn CallAudioRoute>)
        })
    }
}

fn announcement() -> (crate::call_audio::TempWav, String) {
    let bytes = crate::call_audio::wav(8000, 1, 16000);
    let hash = ivy_contracts::hash::digest(&bytes);
    let file = crate::call_audio::TempWav::new(&bytes);
    (file, hash)
}
fn screening(file: &std::path::Path, hash: &str) -> String {
    stringify(
        &IvyJson::object([
            ("wavPath", file.to_string_lossy().to_string().into()),
            ("sha256", hash.into()),
            ("timeoutSeconds", n(1)),
            ("repeatCount", n(0)),
        ]),
        65536,
    )
    .unwrap()
}
fn audio_cases() -> Vec<String> {
    let base = settings();
    let mut cases = vec!["null".into(), "{}".into()];
    for (old, new) in [
        ("\"capture-fixture\"", "null"),
        ("\"capture-fixture\"", "\"render-fixture\""),
        ("\"render-fixture\"", "\" \""),
        ("\"microphone\"", "null"),
        ("\"microphone\"", "\"desktop_process\""),
        ("\"microphone\"", "\"virtual_speaker\""),
        ("\"captureBufferMs\":20", "\"captureBufferMs\":9"),
        ("\"captureBufferMs\":20", "\"captureBufferMs\":101"),
        ("\"renderLatencyMs\":10", "\"renderLatencyMs\":1"),
        ("\"renderLatencyMs\":10", "\"renderLatencyMs\":101"),
        ("\"queueMs\":60", "\"queueMs\":19"),
        ("\"queueMs\":60", "\"queueMs\":201"),
    ] {
        cases.push(base.replace(old, new));
    }
    for addition in [
        r#""playbackPrebufferMs":-1"#,
        r#""playbackPrebufferMs":61"#,
        r#""mutePolicy":{"intervalSeconds":0}"#,
        r#""mutePolicy":{"intervalSeconds":3601}"#,
        r#""mutePolicy":{"mutedRenderEndpointId":"render-fixture"}"#,
    ] {
        cases.push(base.trim_end_matches('}').to_string() + "," + addition + "}");
    }
    cases
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_screening_composes_original_announcement_deadline_bridge_and_replacement() {
    let (file, hash) = announcement();
    let screening = screening(&file.0, &hash);
    for mode in ["initial", "delayed"] {
        let gate = Arc::new(Gate::default());
        let route_list = (0..3).map(|_| Arc::new(Route::new())).collect::<Vec<_>>();
        let routes = Arc::new(Factory {
            routes: Mutex::new(route_list.iter().cloned().map(Ok).collect()),
            gate: Some(gate.clone()),
            gate_at: 2,
            opens: AtomicUsize::new(0),
        });
        let factory = Arc::new(CountedFactory {
            inner: NativeSipSessionFactory::new(
                Arc::new(NativeFactory(routes.clone())),
                None,
                Arc::new(Clock),
                |_| Arc::new(Events::default()),
            ),
            starts: AtomicUsize::new(0),
        });
        let owner = NativeSipOperations::new(factory.clone());
        let config = BASE
            .replace("57000", "44000")
            .replace("57998", "44998")
            .replace(
                "\"transport\":\"udp\"",
                &format!("\"transport\":\"udp\",\"outgoingOfferMode\":\"{mode}\""),
            );
        let remaining = Arc::new(Remaining::default());
        remaining.close_gate.release();
        let views = NativeSipViews::new(|| Ok("null".into()), |_| Ok("null".into()));
        let handler = Arc::new(NativeSipCommands::new(
            owner.clone(),
            remaining.clone(),
            |_| Box::pin(async { Ok(()) }),
            views,
        ));
        let dispatcher =
            Arc::new(NativeDispatcher::new(CALL.into(), 128, handler.clone()).unwrap());
        let selected = format!(r#"{{"callId":"{CALL}"}}"#);
        let mut steps: Vec<(String, String)> = vec![("configure".into(), config)];
        let audio_cases = audio_cases();
        for scenario in [
            "screening_accept_late_bridge",
            "screening_bridge",
            "screening_end_during_bridge",
            "screening_timeout",
            "screening_decline",
            "screening_retired",
        ] {
            steps.push(("call.prepare".into(), selected.clone()));
            let cases = if scenario == "screening_accept_late_bridge" {
                audio_cases.join(",")
            } else {
                String::new()
            };
            steps.push(("fixture.outgoing".into(),format!(r#"{{"callId":"{CALL}","scenario":"{scenario}","screeningSettings":{screening},"audioSettingsCases":[{cases}]}}"#)));
        }
        let original = super::dial::expected(&format!("native-screening-{mode}"));
        trace(
            &format!("native-screening-original-{mode}"),
            IvyJson::object([("original", original.clone())]),
        );
        let mut rows = Vec::new();
        for (index, (method, params)) in steps.iter().enumerate() {
            let result = if method == "fixture.outgoing" {
                let scenario = parse(params, 65536)
                    .unwrap()
                    .get("scenario")
                    .unwrap()
                    .as_str()
                    .unwrap();
                super::dial::outgoing(
                    &owner,
                    dispatcher.clone(),
                    &scenario,
                    10000 + 1000 * index as u64,
                    None,
                    None,
                    None,
                    Some(Scenario {
                        settings: &screening,
                        gate: gate.clone(),
                        routes: routes.clone(),
                        audio_cases: if scenario == "screening_accept_late_bridge" {
                            &audio_cases
                        } else {
                            &[]
                        },
                    }),
                )
                .await
            } else {
                let reply = dispatcher
                    .execute(command(method, params, index as u64 + 1, index as u64 + 1))
                    .await;
                value(reply.outcome())
            };
            rows.push(IvyJson::object([(
                "json",
                stringify(&result, 65536).unwrap().into(),
            )]));
        }
        trace(
            &format!("native-screening-{mode}"),
            IvyJson::object([
                ("rust", IvyJson::Array(rows.clone())),
                ("original", original.clone()),
                ("savedReceiptsAfterReplacement", true.into()),
                ("cancelledBridgeObserverRetained", true.into()),
                ("physicalAudioOpens", n(0)),
                ("syntheticRouteOpens", n(3)),
            ]),
        );
        super::dial::compare(&rows, &original, &steps);
        assert_eq!(factory.starts.load(Ordering::SeqCst), 12);
        assert_eq!(routes.opens.load(Ordering::SeqCst), 3);
        assert!(
            route_list
                .iter()
                .all(|route| route.state.lock().unwrap().disposals == 1)
        );
        assert!(remaining.methods.lock().unwrap().is_empty());
        handler.shutdown().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_screening_and_audio_dto_precedence_defaults_and_failed_preparation_match_original()
{
    let (file, hash) = announcement();
    let valid = screening(&file.0, &hash);
    let selected = format!(r#"{{"callId":"{CALL}"}}"#);
    let (owner, factory, routes, config) = actual_owner(43000, 43998);
    let remaining = Arc::new(Remaining::default());
    remaining.close_gate.release();
    let handler = Arc::new(NativeSipCommands::new(
        owner,
        remaining.clone(),
        |_| Box::pin(async { Ok(()) }),
        closed_platform_views(),
    ));
    let mut steps: Vec<(String, String)> = vec![
        ("call.screening.prepare".into(), "{}".into()),
        ("call.screening.bridge".into(), "{}".into()),
        ("call.windows.connect".into(), "{}".into()),
        ("configure".into(), config),
    ];
    for method in [
        "call.screening.prepare",
        "call.screening.bridge",
        "call.windows.connect",
    ] {
        for raw in [
            "{}",
            r#"{"callId":null,"settings":null}"#,
            r#"{"callId":42,"settings":null}"#,
            r#"{"callId":"missing","settings":3}"#,
        ] {
            steps.push((method.into(), raw.into()));
        }
    }
    steps.push(("call.prepare".into(), selected.clone()));
    let mut screening_cases = vec![
        "null".into(),
        "{}".into(),
        "3".into(),
        "[]".into(),
        r#"{"unknown":true}"#.into(),
        valid.replace(&hash, &hash.to_uppercase()),
        valid.replace("\"repeatCount\":0", "\"repeatCount\":4"),
        valid.replace("\"timeoutSeconds\":1", "\"timeoutSeconds\":0"),
        valid.replace("\"timeoutSeconds\":1", "\"timeoutSeconds\":121"),
    ];
    let missing = file
        .0
        .with_extension("missing")
        .to_string_lossy()
        .to_string();
    let mut bad_path = parse(&valid, 65536).unwrap();
    bad_path.insert("wavPath", missing.into());
    screening_cases.push(stringify(&bad_path, 65536).unwrap());
    for value in ["null", "1.0", "1e0", "2147483648", "-2147483649"] {
        screening_cases.push(valid.replace(
            "\"timeoutSeconds\":1",
            &format!("\"timeoutSeconds\":{value}"),
        ));
    }
    for settings in screening_cases {
        steps.push(("call.screening.prepare".into(), params(&settings)));
    }
    for method in ["call.screening.bridge", "call.windows.connect"] {
        for raw in [
            "null",
            "{}",
            "3",
            "[]",
            r#"{"unknown":true}"#,
            r#"{"captureBufferMs":1.0}"#,
            r#"{"queueMs":1e0}"#,
            r#"{"queueMs":2147483648}"#,
            r#"{"enableWasapiLowLatency":null}"#,
            r#"{"mutePolicy":{"intervalSeconds":null}}"#,
            r#"{"mutePolicy":{"intervalSeconds":1.0}}"#,
            r#"{"captureEndpointId":false}"#,
        ] {
            steps.push((method.into(), params(raw)));
        }
    }
    // A failed load does not consume screening; success does, before bounds.
    steps.push(("call.screening.prepare".into(), params(&valid)));
    steps.push((
        "call.screening.prepare".into(),
        params(&valid.replace("\"timeoutSeconds\":1", "\"timeoutSeconds\":0")),
    ));
    steps.push((
        "call.screening.prepare".into(),
        params(&valid.replace("\"timeoutSeconds\":1", "\"timeoutSeconds\":1.0")),
    ));
    steps.push(("call.hangup".into(), selected.clone()));
    steps.push(("call.screening.prepare".into(), params("null")));
    steps.push(("call.release".into(), selected));
    let original = super::dial::expected("native-screening-validation");
    let dispatcher = NativeDispatcher::new(CALL.into(), 128, handler.clone()).unwrap();
    let mut rows = Vec::new();
    for (index, (method, params)) in steps.iter().enumerate() {
        let r = row(&dispatcher, method, params, index as u64 + 1).await;
        rows.push(r.get("output").unwrap().clone());
    }
    trace(
        "native-screening-validation",
        IvyJson::object([
            (
                "rawInputs",
                format!(
                    "[{}]",
                    steps
                        .iter()
                        .map(|(m, p)| format!(r#"{{"method":"{m}","params":{p}}}"#))
                        .collect::<Vec<_>>()
                        .join(",")
                )
                .into(),
            ),
            ("rust", IvyJson::Array(rows.clone())),
            ("original", original.clone()),
            ("physicalAudioOpens", n(0)),
        ]),
    );
    super::dial::compare(&rows, &original, &steps);
    assert_eq!(factory.starts.load(Ordering::SeqCst), 1);
    assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
    assert!(remaining.methods.lock().unwrap().is_empty());
    handler.shutdown().await.unwrap();
}
