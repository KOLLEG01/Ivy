//! The approved final-audio exception through the actual call owner. These
//! cases deliberately do not claim original parity for the final-audio fault.
use super::*;
use crate::desktop::VoiceTime;
use ivy_contracts::json::JsString;
use std::{
    sync::{Barrier, mpsc},
    time::Instant,
};

const FINAL_ERROR: DesktopActionError = DesktopActionError::Platform("Inert final-clear failure.");
#[derive(Clone, Copy)]
enum Fault {
    ClearError,
    ClearPanic,
    RevokePanic,
}
struct Audio {
    fault: Fault,
    trace: Mutex<Vec<&'static str>>,
    clears: AtomicUsize,
    reached: Option<mpsc::Sender<()>>,
    release: Option<Arc<Barrier>>,
}
impl Audio {
    fn log(&self, event: &'static str) {
        self.trace.lock().unwrap().push(event);
    }
    fn fail(&self) -> ActionResult<()> {
        self.log("exception-policy:final-audio-failure");
        if let Some(reached) = &self.reached {
            reached.send(()).unwrap();
            self.release.as_ref().unwrap().wait();
        }
        match self.fault {
            Fault::ClearError => Err(FINAL_ERROR),
            _ => panic!("Inert final-audio panic"),
        }
    }
    fn snapshot(&self) -> Vec<&'static str> {
        self.trace.lock().unwrap().clone()
    }
}
impl CallVoiceAudio for Audio {
    fn voice_target(&self) -> ActionResult<(AudioDesktopIdentity, JsString)> {
        self.log("target");
        Ok((identity(false, false), "capture-fixture".into()))
    }
    fn bind(&self, _: &JsString) -> ActionResult<()> {
        self.log("bind");
        Ok(())
    }
    fn grant(&self) -> ActionResult<()> {
        self.log("grant");
        Ok(())
    }
    fn clear(&self) -> ActionResult<()> {
        self.log("clear");
        if self.clears.fetch_add(1, Ordering::AcqRel) == 1 {
            self.fail()?
        }
        Ok(())
    }
    fn release(&self) -> ActionResult<()> {
        self.log("release");
        Ok(())
    }
    fn arm_recovery(&self) -> ActionResult<()> {
        self.log("arm");
        Ok(())
    }
    fn revoke(&self) {
        self.log("revoke");
        if matches!(self.fault, Fault::RevokePanic) {
            self.fail().unwrap();
        }
    }
}
struct Clock(Instant);
impl VoiceTime for Clock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}
fn make(
    fault: Fault,
    held: bool,
) -> (
    CallDesktopSession,
    Arc<Context>,
    Arc<Audio>,
    Option<mpsc::Receiver<()>>,
    Option<Arc<Barrier>>,
) {
    let mut r = row("exception-policy", true, &[]);
    if matches!(fault, Fault::RevokePanic) {
        set(&mut r, "initialCapture", true.into());
    }
    let ctx = Context::new(r, false);
    let (desktop, runtime) = runtime(&ctx);
    let (reached, entered) = mpsc::channel();
    let release = Arc::new(Barrier::new(2));
    let audio = Arc::new(Audio {
        fault,
        trace: Mutex::new(vec![]),
        clears: AtomicUsize::new(0),
        reached: held.then_some(reached),
        release: held.then_some(release.clone()),
    });
    let factory = audio.clone();
    let owner = CallDesktopSession::with_factory(
        ID.into(),
        desktop,
        Arc::new(|_| Ok(true)),
        Some(audio.clone()),
        Arc::new(move |desktop, input, current| {
            CallVoiceSession::with_time(
                factory.clone(),
                desktop,
                input,
                runtime.clone(),
                current,
                Arc::new(Clock(Instant::now())),
                None,
            )
        }),
    )
    .unwrap();
    (
        owner,
        ctx,
        audio,
        held.then_some(entered),
        held.then_some(release),
    )
}
async fn verify(
    owner: &CallDesktopSession,
    ctx: &Arc<Context>,
    audio: &Arc<Audio>,
    expected: DesktopActionError,
    active: bool,
) -> IvyJson {
    let voice = owner.state().voice.clone().unwrap();
    let trace = audio.snapshot();
    let chords = ctx.chords.load(Ordering::Acquire);
    assert_eq!(voice.final_audio_failure(), Some(expected.clone()));
    assert_eq!(
        voice.retire_for_transition().wait().await,
        Err(expected.clone())
    );
    assert_eq!(voice.stop().wait().await, Err(expected.clone()));
    assert_eq!(voice.pause_for_transition().err(), Some(expected.clone()));
    assert!(!owner.has_original_voice_capture(&identity(false, false)));
    assert!(!owner.state().paused);
    assert_eq!(
        owner.invoke(Some(ID), action("resume:1")).err(),
        Some(CallDesktopError::NotReady)
    );
    let launch = owner.invoke(Some(ID), action("launch")).unwrap();
    assert_eq!(launch.wait().await, Err(expected.clone().into()));
    assert_eq!(
        ctx.factories.load(Ordering::Acquire),
        0,
        "No application factory after known final failure"
    );
    assert_eq!(
        owner.invoke(Some(ID), action("stop")).unwrap().wait().await,
        Err(expected.clone().into())
    );
    let close = owner.close();
    let retained = close.clone();
    drop(close);
    assert_eq!(retained.clone().wait().await, Err(expected.clone().into()));
    let again = owner.close();
    assert!(retained.same_work(&again));
    assert_eq!(again.wait().await, Err(expected.clone().into()));
    assert_eq!(
        audio.snapshot(),
        trace,
        "No audio callback after final failure"
    );
    assert!(!trace.contains(&"release"));
    assert_eq!(
        ctx.chords.load(Ordering::Acquire),
        chords,
        "No input after final failure"
    );
    assert!(
        owner
            .state()
            .work
            .iter()
            .all(|(_, work)| work.result().is_some())
    );
    IvyJson::object([
        ("activeStart", active.into()),
        ("error", expected.to_string().into()),
        ("owner", snapshot(owner)),
        ("audio", arr(&trace)),
        ("chords", n(chords)),
        ("closeFailed", true.into()),
        ("replacementReady", false.into()),
    ])
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn final_audio_failure_retains_call_owner_and_blocks_cleanup_retry() {
    let mut rows = vec![];
    for fault in [Fault::ClearError, Fault::ClearPanic, Fault::RevokePanic] {
        let (owner, ctx, audio, _, _) = make(fault, false);
        let start = owner.invoke(Some(ID), action("start")).unwrap();
        let expected = if matches!(fault, Fault::ClearError) {
            FINAL_ERROR
        } else {
            DesktopActionError::Unavailable
        };
        let active = !matches!(fault, Fault::RevokePanic);
        if active {
            let result = start.wait().await.unwrap();
            assert_eq!(
                result
                    .get("result")
                    .unwrap()
                    .get("state")
                    .unwrap()
                    .as_str()
                    .unwrap(),
                "active"
            );
            assert_eq!(
                owner
                    .invoke(Some(ID), action("pause:0"))
                    .unwrap()
                    .wait()
                    .await,
                Err(expected.clone().into())
            );
        } else {
            assert_eq!(start.wait().await, Err(expected.clone().into()));
        }
        rows.push(verify(&owner, &ctx, &audio, expected, active).await);
    }
    // Stop/dispose queued while the real Voice thread holds the final-audio gate.
    let (owner, ctx, audio, entered, release) = make(Fault::ClearError, true);
    owner
        .invoke(Some(ID), action("start"))
        .unwrap()
        .wait()
        .await
        .unwrap();
    let pause = owner.invoke(Some(ID), action("pause:0")).unwrap();
    entered
        .unwrap()
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    let (attempted, attempts) = mpsc::channel();
    let callers: Vec<_> = (0..4)
        .map(|_| {
            let owner = owner.clone();
            let attempted = attempted.clone();
            let runtime = tokio::runtime::Handle::current();
            std::thread::spawn(move || {
                let _runtime = runtime.enter();
                attempted.send(()).unwrap();
                owner.invoke(Some(ID), action("stop"))
            })
        })
        .collect();
    for _ in 0..4 {
        attempts.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    let closing = {
        let owner = owner.clone();
        let runtime = tokio::runtime::Handle::current();
        std::thread::spawn(move || {
            let _runtime = runtime.enter();
            owner.close()
        })
    };
    release.unwrap().wait();
    assert_eq!(pause.wait().await, Err(FINAL_ERROR.into()));
    for caller in callers {
        match caller.join().unwrap() {
            Ok(work) => assert_eq!(work.wait().await, Err(FINAL_ERROR.into())),
            Err(error) => assert!(matches!(
                error,
                CallDesktopError::Conflict | CallDesktopError::Stopping
            )),
        }
    }
    assert_eq!(
        closing.join().unwrap().wait().await,
        Err(FINAL_ERROR.into())
    );
    let trace = audio.snapshot();
    assert_eq!(
        trace
            .iter()
            .filter(|e| **e == "exception-policy:final-audio-failure")
            .count(),
        1
    );
    assert!(!trace.contains(&"revoke"));
    assert!(!trace.contains(&"release"));
    assert_eq!(ctx.chords.load(Ordering::Acquire), 1);
    let voice = owner.state().voice.clone().unwrap();
    assert_eq!(voice.retire_for_transition().wait().await, Err(FINAL_ERROR));
    assert_eq!(owner.close().wait().await, Err(FINAL_ERROR.into()));
    assert_eq!(audio.snapshot(), trace);
    rows.push(IvyJson::object([
        ("case", "concurrent-stop-and-dispose".into()),
        ("owner", snapshot(&owner)),
        ("audio", arr(&trace)),
        ("chords", n(1)),
        ("closeFailed", true.into()),
    ]));
    assert_eq!(rows.len(), 4);
    save("call-desktop-exception-policy.json", &IvyJson::Array(rows));
}
