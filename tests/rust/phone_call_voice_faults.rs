//! Explicit final-audio exception policy; these faults are not original parity.
use super::*;
use std::sync::{Barrier, atomic::AtomicUsize, mpsc};

const FINAL_CLEAR_ERROR: DesktopActionError =
    DesktopActionError::Platform("Inert final-clear failure.");

#[derive(Clone, Copy)]
enum Fault {
    ClearError,
    ClearPanic,
    RevokePanic,
}
struct FaultAudio {
    context: Arc<Context>,
    fault: Fault,
    clears: AtomicUsize,
    reached: Option<mpsc::Sender<()>>,
    release: Option<Arc<Barrier>>,
}
impl FaultAudio {
    fn fail(&self) -> ActionResult<()> {
        self.context.log("exception-policy:final-audio-failure");
        if let Some(reached) = &self.reached {
            reached.send(()).unwrap();
            self.release.as_ref().unwrap().wait();
        }
        match self.fault {
            Fault::ClearError => Err(FINAL_CLEAR_ERROR),
            Fault::ClearPanic | Fault::RevokePanic => panic!("Inert final-audio panic"),
        }
    }
}
impl CallVoiceAudio for FaultAudio {
    fn voice_target(&self) -> ActionResult<(AudioDesktopIdentity, JsString)> {
        self.context.voice_target()
    }
    fn bind(&self, endpoint: &JsString) -> ActionResult<()> {
        self.context.bind(endpoint)
    }
    fn grant(&self) -> ActionResult<()> {
        self.context.grant()
    }
    fn clear(&self) -> ActionResult<()> {
        self.context.clear()?;
        // Pause's initial clear succeeds; only the original finally clear fails.
        if self.clears.fetch_add(1, Ordering::SeqCst) == 1 {
            self.fail()?;
        }
        Ok(())
    }
    fn release(&self) -> ActionResult<()> {
        self.context.release()
    }
    fn arm_recovery(&self) -> ActionResult<()> {
        self.context.arm_recovery()
    }
    fn revoke(&self) {
        self.context.revoke();
        if matches!(self.fault, Fault::RevokePanic) {
            self.fail().unwrap();
        }
    }
}

fn context(row: IvyJson) -> Arc<Context> {
    Arc::new(Context {
        row,
        trace: Mutex::new(vec![]),
        now: AtomicU64::new(0),
        counts: Mutex::new(Counts::default()),
        session: Mutex::new(None),
    })
}
fn session(ctx: &Arc<Context>, audio: Arc<FaultAudio>) -> CallVoiceSession {
    let original = ctx.clone();
    let session = CallVoiceSession::with_time(
        audio,
        identity(false),
        input("hotkey", false),
        Arc::new(Runtime(ctx.clone())),
        Arc::new(move |cleanup| original.original(cleanup)),
        ctx.clone(),
        None,
    )
    .unwrap();
    *ctx.session.lock().unwrap() = Some(session.clone());
    session
}
fn completion(value: ActionResult<VoiceLifetimeResult>) -> IvyJson {
    match value {
        Ok(value) => projected(value),
        Err(error) => IvyJson::object([("error", error.to_string().into())]),
    }
}
async fn retained_failure(
    session: &CallVoiceSession,
    ctx: &Arc<Context>,
    expected: DesktopActionError,
    active: bool,
    dispatches: usize,
) -> IvyJson {
    let started = session.0.started.result().unwrap();
    if active {
        assert_eq!(started.as_ref().unwrap().state, "active");
    } else {
        assert_eq!(started, Err(expected.clone()));
    }
    assert_eq!(session.0.finished.wait().await, Err(expected.clone()));
    let trace = ctx.trace.lock().unwrap().clone();
    assert_eq!(ctx.counts.lock().unwrap().dispatch, dispatches);
    assert_eq!(
        trace.last().unwrap(),
        "exception-policy:final-audio-failure"
    );
    for _ in 0..3 {
        let stopped = session.stop();
        assert!(Arc::ptr_eq(&stopped.0, &session.0.finished.0));
        assert_eq!(stopped.wait().await, Err(expected.clone()));
        assert_eq!(session.pause_for_transition().err(), Some(expected.clone()));
        assert_eq!(
            session.retire_for_transition().wait().await,
            Err(expected.clone())
        );
        assert!(!session.has_original_capture());
        assert!(session.start().is_err());
    }
    let close = session.close();
    let saved = close.clone();
    drop(close);
    assert_eq!(saved.wait().await, Err(expected.clone()));
    let repeated = session.close();
    assert!(Arc::ptr_eq(&saved.0, &repeated.0));
    assert_eq!(repeated.wait().await, Err(expected.clone()));
    // No release, replacement admission, audio retry or cleanup input followed
    // the failure. Reading the cached one-shot completions has no effects.
    assert_eq!(*ctx.trace.lock().unwrap(), trace);
    assert_eq!(session.0.started.result().unwrap(), started);
    let state = session.0.state.lock().unwrap();
    assert_eq!(state.final_audio_failure, Some(expected.clone()));
    assert!(!state.disposed);
    drop(state);
    *ctx.session.lock().unwrap() = None;
    IvyJson::object([
        ("name", ctx.text("name").into()),
        ("policy", "explicit-final-audio-exception".into()),
        ("started", completion(started)),
        ("finished", completion(Err(expected))),
        ("disposed", false.into()),
        ("dispatches", n(dispatches)),
        (
            "trace",
            IvyJson::Array(trace.into_iter().map(Into::into).collect()),
        ),
    ])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn final_audio_failure_preserves_completion_and_fences_cleanup() {
    let mut rows = vec![];
    for (name, fault, captures, active, dispatches) in [
        (
            "final-revoke-before-dispatch",
            Fault::RevokePanic,
            vec!["matched"],
            false,
            0,
        ),
        (
            "final-revoke-after-submission",
            Fault::RevokePanic,
            vec!["none", "ambiguous"],
            false,
            1,
        ),
        (
            "final-revoke-after-active",
            Fault::RevokePanic,
            vec!["none", "matched"],
            true,
            1,
        ),
        (
            "final-clear-error-after-active",
            Fault::ClearError,
            vec!["none", "matched"],
            true,
            1,
        ),
        (
            "final-clear-panic-after-active",
            Fault::ClearPanic,
            vec!["none", "matched"],
            true,
            1,
        ),
    ] {
        let mut row = row(name, &captures);
        if matches!(fault, Fault::RevokePanic) {
            set(&mut row, "control", "none".into());
            set(&mut row, "currentEndsAt", n(5));
        } else {
            set(&mut row, "control", "pause".into());
        }
        let ctx = context(row);
        let audio = Arc::new(FaultAudio {
            context: ctx.clone(),
            fault,
            clears: AtomicUsize::new(0),
            reached: None,
            release: None,
        });
        let session = session(&ctx, audio);
        session.0.state.lock().unwrap().launched = true;
        session.run();
        rows.push(
            retained_failure(
                &session,
                &ctx,
                if matches!(fault, Fault::ClearError) {
                    FINAL_CLEAR_ERROR
                } else {
                    DesktopActionError::Unavailable
                },
                active,
                dispatches,
            )
            .await,
        );
    }

    // Actual retained Voice thread: stop callers queue behind the final-audio
    // primitive. Failure publication and the gate prevent a later revoke.
    let mut row = row(
        "final-failure-with-concurrent-stop-and-retirement",
        &["matched"],
    );
    set(&mut row, "control", "none".into());
    let ctx = context(row);
    let (reached, entered) = mpsc::channel();
    let release = Arc::new(Barrier::new(2));
    let audio = Arc::new(FaultAudio {
        context: ctx.clone(),
        fault: Fault::RevokePanic,
        clears: AtomicUsize::new(0),
        reached: Some(reached),
        release: Some(release.clone()),
    });
    let session = session(&ctx, audio);
    let start = session.start().unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let retirement = session.retire_for_transition();
    let saved_retirement = retirement.clone();
    drop(retirement);
    let (attempted, attempts) = mpsc::channel();
    let callers: Vec<_> = (0..4)
        .map(|_| {
            let owner = session.clone();
            let attempted = attempted.clone();
            std::thread::spawn(move || {
                attempted.send(()).unwrap();
                owner.stop()
            })
        })
        .collect();
    for _ in 0..4 {
        attempts.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    release.wait();
    assert_eq!(start.wait().await, Err(DesktopActionError::Unavailable));
    for caller in callers {
        assert_eq!(
            caller.join().unwrap().wait().await,
            Err(DesktopActionError::Unavailable)
        );
    }
    assert_eq!(
        saved_retirement.wait().await,
        Err(DesktopActionError::Unavailable)
    );
    rows.push(retained_failure(&session, &ctx, DesktopActionError::Unavailable, false, 0).await);
    assert_eq!(rows.len(), 6);
    save("call-voice-exception-policy.json", &IvyJson::Array(rows));
}
