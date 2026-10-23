//! Parent201's explicit final-audio exception, through actual native owners.
use super::*;
#[path = "phone_native_call_desktop_entry.rs"]
mod entry;
struct FaultControl {
    at: AtomicUsize,
    suspends: AtomicUsize,
    panic: bool,
    gate: Option<Arc<CaptureGate>>,
}
struct FaultRoute {
    route: Arc<dyn CallAudioRoute>,
    control: Arc<FaultControl>,
}
impl PcmAudioPort for FaultRoute {
    fn state(&self) -> PcmAudioState {
        self.route.state()
    }
    fn read_captured(&self, out: &mut [f32], g: u64) -> usize {
        self.route.read_captured(out, g)
    }
    fn write_received(&self, input: &[f32], g: u64) -> bool {
        self.route.write_received(input, g)
    }
}
impl PcmSendPort for FaultRoute {
    fn with_send_state(&self, s: PcmAudioState, f: &mut dyn FnMut()) -> bool {
        self.route.with_send_state(s, f)
    }
    fn send_health(&self) -> AudioSendHealth {
        self.route.send_health()
    }
}
impl CallAudioRoute for FaultRoute {
    fn status(&self) -> IvyJson {
        self.route.status()
    }
    fn failed(&self) -> bool {
        self.route.failed()
    }
    fn resume(&self) -> Result<(), CallAudioError> {
        self.route.resume()
    }
    fn suspend(&self) -> Result<(), CallAudioError> {
        let index = self.control.suspends.fetch_add(1, Ordering::SeqCst) + 1;
        if index == self.control.at.load(Ordering::SeqCst) {
            if let Some(g) = &self.control.gate {
                g.block()
            }
            if self.control.panic {
                panic!("Inert final-audio panic")
            }
            return Err(CallAudioError::RouteFailed);
        }
        self.route.suspend()
    }
    fn close(&self) -> Pin<Box<dyn Future<Output = Result<(), CallAudioError>> + Send + '_>> {
        self.route.close()
    }
}
struct FaultFactory {
    inner: Arc<Factory>,
    control: Arc<FaultControl>,
}
impl CallAudioFactory for FaultFactory {
    fn open<'a>(
        &'a self,
        s: &'a AudioSettings,
        d: Option<&'a AudioDesktopIdentity>,
        p: AudioRoutePermit,
        c: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<Arc<dyn CallAudioRoute>, CallAudioError>> + Send + 'a>>
    {
        Box::pin(async move {
            let route = self.inner.open(s, d, p, c).await?;
            Ok(Arc::new(FaultRoute {
                route,
                control: self.control.clone(),
            }) as Arc<dyn CallAudioRoute>)
        })
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn final_audio_failure_fences_actual_native_release_and_shutdown() {
    let mut rows = vec![];
    for mode in [
        "clear-error",
        "clear-panic",
        "revoke-panic",
        "concurrent-clear-error",
    ] {
        let ctx = Arc::new(Context::default());
        if mode == "revoke-panic" {
            ctx.capture.store(true, Ordering::SeqCst);
        }
        let (desktop, voice) = runtime(ctx.clone());
        let route = Arc::new(Route::new());
        let gate = (mode == "concurrent-clear-error").then(|| Arc::new(CaptureGate::default()));
        let control = Arc::new(FaultControl {
            at: AtomicUsize::new(usize::MAX),
            suspends: AtomicUsize::new(0),
            panic: mode.ends_with("panic"),
            gate: gate.clone(),
        });
        let factory = Arc::new(FaultFactory {
            inner: Arc::new(Factory::routes(vec![route.clone()])),
            control: control.clone(),
        });
        let media = Arc::new(NativeSipSessionFactory::new(
            factory,
            None,
            Arc::new(Clock),
            |_| Arc::new(Events::default()),
        ));
        let owner = NativeSipOperations::with_desktop_runtime(media, desktop, voice);
        let endpoint = owner
            .configure(&configuration(BASE), |_| async { Ok(()) })
            .unwrap()
            .wait()
            .await
            .unwrap();
        let call = owner.prepare(CALL.into()).unwrap();
        call.prepared().await.unwrap();
        let media = call.media().unwrap();
        let mut settings = settings();
        settings.source_mode = "desktop_process".into();
        media
            .prepare_audio(settings, Some(identity("")), CancellationToken::new())
            .unwrap()
            .wait()
            .await
            .unwrap();
        let remaining = Arc::new(Remaining::default());
        remaining.close_gate.release();
        let handler = Arc::new(NativeSipCommands::new(
            owner.clone(),
            remaining.clone(),
            |_| Box::pin(async { Ok(()) }),
            NativeSipViews::new(|| Ok("null".into()), |_| Ok("null".into())),
        ));
        let dispatcher = NativeDispatcher::new(CALL.into(), 64, handler.clone()).unwrap();
        if mode == "revoke-panic" {
            control.at.store(
                control.suspends.load(Ordering::SeqCst) + 1,
                Ordering::SeqCst,
            );
        }
        let started = dispatcher
            .execute(command(
                "call.desktop.startVoice",
                &params("call.desktop.startVoice"),
                1,
                1,
            ))
            .await;
        if mode == "revoke-panic" {
            assert_eq!(
                started.outcome().error_code(),
                Some("operation_outcome_unknown")
            );
        } else {
            assert_eq!(
                value(started.outcome())
                    .get("result")
                    .unwrap()
                    .get("state")
                    .unwrap()
                    .as_str()
                    .unwrap(),
                "active"
            );
            control.at.store(
                control.suspends.load(Ordering::SeqCst) + 2,
                Ordering::SeqCst,
            );
            let d = dispatcher.clone();
            let pause = tokio::spawn(async move {
                d.execute(command(
                    "call.desktop.pauseVoice",
                    &params("call.desktop.pauseVoice"),
                    2,
                    2,
                ))
                .await
            });
            if let Some(g) = &gate {
                until(|| g.entered.load(Ordering::SeqCst)).await;
                let original = owner.clone();
                let stop =
                    tokio::spawn(async move { original.release(&call).unwrap().wait().await });
                sleep(Duration::from_millis(20)).await;
                g.release();
                assert_eq!(stop.await.unwrap(), Err(SipHostError::CleanupUnconfirmed));
            }
            assert_eq!(
                pause.await.unwrap().outcome().error_code(),
                Some("operation_outcome_unknown")
            );
        }
        let call = owner.find(CALL).unwrap();
        let before = control.suspends.load(Ordering::SeqCst);
        let chords = ctx.chords.load(Ordering::SeqCst);
        assert_eq!(
            owner.release(&call).unwrap().wait().await,
            Err(SipHostError::CleanupUnconfirmed)
        );
        assert!(owner.find(CALL).is_some());
        assert!(matches!(
            owner.prepare(CALL.into()),
            Err(SipHostError::OperationConflict)
        ));
        assert!(matches!(
            owner.prepare("22222222-2222-4222-8222-222222222222".into()),
            Err(SipHostError::OperationConflict)
        ));
        assert_eq!(
            handler.shutdown().await,
            Err(NativeShutdownError::OutcomeUnknown)
        );
        assert_eq!(
            handler.shutdown().await,
            Err(NativeShutdownError::OutcomeUnknown)
        );
        assert_eq!(remaining.closes.load(Ordering::SeqCst), 1);
        assert!(UdpSocket::bind(endpoint).await.is_err());
        let replay = dispatcher
            .execute(command(
                "call.desktop.startVoice",
                &params("call.desktop.startVoice"),
                1,
                8,
            ))
            .await;
        assert_eq!(replay.outcome(), started.outcome());
        media.call_audio().revoke();
        assert_eq!(
            media.call_audio().clear_authority(),
            Err(CallAudioError::CleanupUnconfirmed)
        );
        assert_eq!(
            media.call_audio().close().wait().await,
            Err(CallAudioError::CleanupUnconfirmed)
        );
        assert!(!media.call_audio().state().open);
        let mut pcm = [1.0; 8];
        assert_eq!(media.call_audio().read_captured(&mut pcm, 0), 0);
        assert!(pcm.iter().all(|s| *s == 0.0));
        assert_eq!(control.suspends.load(Ordering::SeqCst), before);
        assert_eq!(route.state.lock().unwrap().disposals, 0);
        assert_eq!(ctx.chords.load(Ordering::SeqCst), chords);
        rows.push(IvyJson::object([
            ("mode", mode.into()),
            ("started", response(started.outcome())),
            ("releaseFailed", true.into()),
            ("nativeShutdownFailed", true.into()),
            ("slotRetained", true.into()),
            ("bindingRetained", true.into()),
            ("routeDisposals", n(0)),
            ("suspendsAfterFailure", n(0)),
            ("inputAfterFailure", n(0)),
        ]));
    }
    assert_eq!(rows.len(), 4);
    save(
        "native-call-desktop-exception-policy.json",
        &IvyJson::Array(rows),
    );
}
