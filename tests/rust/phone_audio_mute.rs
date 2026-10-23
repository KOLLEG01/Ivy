use ivy_phone_bridge::media::{
    AudioMuteClock, AudioMuteDelay, AudioMuteDelayError, AudioMuteEndpoint, AudioMuteGuard,
    AudioMutePolicy, CallAudioError,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    rc::Rc,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
type Result<T> = std::result::Result<T, CallAudioError>;

#[derive(Clone)]
struct Endpoint {
    muted: bool,
    active: bool,
}
struct Data {
    endpoints: BTreeMap<String, Endpoint>,
    events: Vec<String>,
    fault: &'static str,
}
struct World {
    data: Mutex<Data>,
    failed: AtomicUsize,
    throw_failed: AtomicBool,
}
impl World {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            data: Mutex::new(Data {
                endpoints: [("s", false), ("c", true), ("S", false)]
                    .into_iter()
                    .map(|(id, muted)| {
                        (
                            id.into(),
                            Endpoint {
                                muted,
                                active: true,
                            },
                        )
                    })
                    .collect(),
                events: vec![],
                fault: "",
            }),
            failed: AtomicUsize::new(0),
            throw_failed: AtomicBool::new(false),
        })
    }
    fn failure(&self) {
        self.data.lock().unwrap().events.push("failed".into());
        self.failed.fetch_add(1, Ordering::SeqCst);
        assert!(
            !self.throw_failed.load(Ordering::SeqCst),
            "synthetic callback"
        );
    }
    fn open(self: &Arc<Self>, id: &str) -> Result<Box<dyn AudioMuteEndpoint>> {
        let mut data = self.data.lock().unwrap();
        data.events.push(format!("open:{id}"));
        if id == "s" && data.fault == "open" {
            return Err(CallAudioError::RouteFailed);
        }
        Ok(Box::new(Handle {
            world: self.clone(),
            id: id.into(),
            reads: AtomicUsize::new(0),
            _thread_bound: Rc::new(()),
        }))
    }
    fn snapshot(&self, name: &str, facts: Value) -> Value {
        let data = self.data.lock().unwrap();
        json!({"name":name,"facts":facts,"failed":self.failed.load(Ordering::SeqCst),"states":data.endpoints.iter().map(|(id,e)|(id.clone(),e.muted)).collect::<BTreeMap<_,_>>(),"events":data.events})
    }
}
// Deliberately !Send: a platform endpoint never crosses its observation thread.
struct Handle {
    world: Arc<World>,
    id: String,
    reads: AtomicUsize,
    _thread_bound: Rc<()>,
}
impl Handle {
    fn fault(&self, data: &Data, name: &str) -> bool {
        self.id == "s" && data.fault == name
    }
}
impl AudioMuteEndpoint for Handle {
    fn id(&self) -> Result<String> {
        let mut d = self.world.data.lock().unwrap();
        d.events.push(format!("id:{}", self.id));
        if self.fault(&d, "id") {
            return Err(CallAudioError::RouteFailed);
        }
        Ok(if self.fault(&d, "replacement") {
            "replacement".into()
        } else {
            self.id.clone()
        })
    }
    fn active_render(&self) -> Result<bool> {
        let mut d = self.world.data.lock().unwrap();
        d.events.push(format!("active:{}", self.id));
        if self.fault(&d, "active") {
            return Err(CallAudioError::RouteFailed);
        }
        Ok(!self.fault(&d, "inactive") && d.endpoints[&self.id].active)
    }
    fn muted(&self) -> Result<bool> {
        let mut d = self.world.data.lock().unwrap();
        let muted = d.endpoints[&self.id].muted;
        d.events
            .push(format!("get:{}:{}", self.id, u8::from(muted)));
        let fault = if self.reads.fetch_add(1, Ordering::SeqCst) == 0 {
            "read"
        } else {
            "confirm"
        };
        if self.fault(&d, fault) {
            return Err(CallAudioError::RouteFailed);
        }
        Ok(muted)
    }
    fn set_muted(&mut self, value: bool) -> Result<()> {
        let mut d = self.world.data.lock().unwrap();
        d.events
            .push(format!("set:{}:{}", self.id, u8::from(value)));
        if self.fault(&d, "set") {
            return Err(CallAudioError::RouteFailed);
        }
        if !self.fault(&d, "noapply") {
            d.endpoints.get_mut(&self.id).unwrap().muted = value;
        }
        Ok(())
    }
    fn close(&mut self) -> Result<()> {
        let mut d = self.world.data.lock().unwrap();
        d.events.push(format!("close:{}", self.id));
        if self.fault(&d, "close") {
            return Err(CallAudioError::RouteFailed);
        }
        Ok(())
    }
}
#[derive(Default)]
struct Clock {
    timers: Mutex<VecDeque<oneshot::Sender<()>>>,
    created: AtomicUsize,
    fail: AtomicBool,
}
impl AudioMuteClock for Clock {
    fn delay(
        &self,
        _: u16,
        stop: CancellationToken,
    ) -> std::result::Result<AudioMuteDelay, AudioMuteDelayError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(AudioMuteDelayError::Failed);
        }
        let (send, mut receive) = oneshot::channel();
        self.timers.lock().unwrap().push_back(send);
        self.created.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(async move {
            // Completion is latched: an already fired timer beats a later stop.
            tokio::select! { biased; value = &mut receive => value.map_err(|_|AudioMuteDelayError::Failed), _ = stop.cancelled() => Err(AudioMuteDelayError::Cancelled) }
        }))
    }
}
impl Clock {
    fn tick(&self) {
        self.timers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap()
            .send(())
            .unwrap();
    }
}
fn policy(muted: Option<&str>, unmute: bool, seconds: u16) -> AudioMutePolicy {
    AudioMutePolicy {
        muted_render_endpoint: muted.map(Into::into),
        unmute_receive: unmute,
        interval_seconds: seconds,
    }
}
fn start(
    w: &Arc<World>,
    c: &Arc<Clock>,
    p: AudioMutePolicy,
    receive: &str,
) -> Result<AudioMuteGuard> {
    let (failed, open) = (w.clone(), w.clone());
    AudioMuteGuard::with_clock(
        &p,
        receive,
        move || failed.failure(),
        move |id| open.open(id),
        c.clone(),
    )
}
async fn conflict(w: &Arc<World>, c: &Arc<Clock>, receive: &str) -> bool {
    match start(w, c, policy(None, true, 60), receive) {
        Err(_) => true,
        Ok(g) => {
            g.close().wait().await.unwrap();
            false
        }
    }
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("synthetic timeout");
}
async fn cases() -> Vec<Value> {
    let mut cases = vec![];
    for already in [false, true] {
        let w = World::new();
        let c = Arc::new(Clock::default());
        {
            let mut d = w.data.lock().unwrap();
            d.endpoints.get_mut("s").unwrap().muted = already;
            d.endpoints.get_mut("c").unwrap().muted = !already;
        }
        let g = start(&w, &c, policy(Some("s"), true, 1), "c").unwrap();
        g.close().wait().await.unwrap();
        cases.push(w.snapshot(
            if already { "already" } else { "initial" },
            json!({"timers":c.created.load(Ordering::SeqCst)}),
        ));
    }
    {
        let w = World::new();
        let c = Arc::new(Clock::default());
        let a = start(&w, &c, policy(Some("s"), true, 1), "c").unwrap();
        let b = start(&w, &c, policy(Some("s"), true, 1), "c").unwrap();
        let first = conflict(&w, &c, "s").await;
        a.close().wait().await.unwrap();
        let partial = conflict(&w, &c, "s").await;
        let distinct = start(&w, &c, policy(None, true, 60), "S").unwrap();
        distinct.close().wait().await.unwrap();
        b.close().wait().await.unwrap();
        let last = conflict(&w, &c, "s").await;
        cases.push(w.snapshot("leases",json!({"conflict":first,"partialConflict":partial,"finalConflict":last,"timers":c.created.load(Ordering::SeqCst)})));
    }
    for fault in [
        "open",
        "id",
        "replacement",
        "active",
        "inactive",
        "read",
        "set",
        "confirm",
        "noapply",
        "close",
    ] {
        let w = World::new();
        let c = Arc::new(Clock::default());
        w.data.lock().unwrap().fault = fault;
        let rejected = match start(&w, &c, policy(Some("s"), true, 1), "c") {
            Err(_) => true,
            Ok(g) => g.close().wait().await.is_err(),
        };
        w.data.lock().unwrap().fault = "";
        let retry = conflict(&w, &c, "s").await;
        cases.push(w.snapshot(&format!("fault-{fault}"),json!({"rejected":rejected,"retryConflict":retry,"timers":c.created.load(Ordering::SeqCst)})));
    }
    for (name, p) in [
        ("zero", policy(Some("s"), false, 0)),
        ("large", policy(Some("s"), false, 3601)),
        ("same", policy(Some("c"), false, 60)),
        ("space", policy(Some(" "), false, 60)),
        ("nul", policy(Some("s\0"), false, 60)),
    ] {
        let w = World::new();
        let c = Arc::new(Clock::default());
        let rejected = match start(&w, &c, p, "c") {
            Err(_) => true,
            Ok(g) => g.close().wait().await.is_err(),
        };
        cases.push(w.snapshot(
            &format!("invalid-{name}"),
            json!({"rejected":rejected,"timers":c.created.load(Ordering::SeqCst)}),
        ));
    }
    {
        let w = World::new();
        let c = Arc::new(Clock::default());
        start(&w, &c, policy(None, false, 60), "c")
            .unwrap()
            .close()
            .wait()
            .await
            .unwrap();
        cases.push(w.snapshot("empty", json!({"timers":c.created.load(Ordering::SeqCst)})));
    }
    {
        let w = World::new();
        let c = Arc::new(Clock::default());
        let g = start(&w, &c, policy(Some("s"), true, 1), "c").unwrap();
        {
            let mut d = w.data.lock().unwrap();
            d.endpoints.get_mut("s").unwrap().muted = false;
            d.endpoints.get_mut("c").unwrap().muted = true;
        }
        c.tick();
        until(|| c.created.load(Ordering::SeqCst) == 2).await;
        w.data
            .lock()
            .unwrap()
            .endpoints
            .get_mut("s")
            .unwrap()
            .active = false;
        c.tick();
        until(|| w.failed.load(Ordering::SeqCst) == 1).await;
        let retained = conflict(&w, &c, "s").await;
        g.close().wait().await.unwrap();
        w.data
            .lock()
            .unwrap()
            .endpoints
            .get_mut("s")
            .unwrap()
            .active = true;
        let last = conflict(&w, &c, "s").await;
        cases.push(w.snapshot("periodic",json!({"retainedConflict":retained,"finalConflict":last,"timers":c.created.load(Ordering::SeqCst)})));
    }
    for throws in [false, true] {
        let w = World::new();
        let c = Arc::new(Clock::default());
        w.throw_failed.store(throws, Ordering::SeqCst);
        c.fail.store(true, Ordering::SeqCst);
        let g = start(&w, &c, policy(Some("s"), true, 1), "c").unwrap();
        let retained = conflict(&w, &c, "s").await;
        let close_failed = g.close().wait().await.is_err();
        c.fail.store(false, Ordering::SeqCst);
        let last = conflict(&w, &c, "s").await;
        cases.push(w.snapshot(if throws{"timer-callback-throws"}else{"timer-failed"},json!({"retainedConflict":retained,"finalConflict":last,"closeFailed":close_failed,"timers":c.created.load(Ordering::SeqCst)})));
    }
    cases
}
fn trace(name: &str, value: &Value) {
    if let Some(dir) = std::env::var_os("IVY_PHONE_AUDIO_MUTE_TRACE") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            std::path::Path::new(&dir).join(name),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mute_policy_keeps_requested_state_releases_leases_and_retires_faults() {
    let rows = cases().await;
    assert_eq!(rows.len(), 22);
    for row in rows {
        let name = row["name"].as_str().unwrap();
        let facts = &row["facts"];
        match name {
            "initial" | "already" => {
                // Releasing the lease does not restore a stale pre-call state.
                assert_eq!(row["states"]["s"], true);
                assert_eq!(row["states"]["c"], false);
                assert_eq!(row["failed"], 0);
                assert_eq!(facts["timers"], 1);
            }
            "leases" => {
                assert_eq!(facts["conflict"], true);
                assert_eq!(facts["partialConflict"], true);
                assert_eq!(facts["finalConflict"], false);
            }
            "periodic" => {
                assert_eq!(facts["retainedConflict"], true);
                assert_eq!(facts["finalConflict"], false);
                assert_eq!(row["failed"], 1);
            }
            "empty" => {
                assert_eq!(facts["timers"], 1);
                assert_eq!(row["events"], json!([]));
                assert_eq!(row["failed"], 0);
            }
            "timer-failed" | "timer-callback-throws" => {
                assert_eq!(row["failed"], 1);
                assert_eq!(facts["retainedConflict"], true);
                assert_eq!(facts["finalConflict"], false);
                assert_eq!(facts["closeFailed"], name == "timer-callback-throws");
            }
            _ => {
                assert!(name.starts_with("fault-") || name.starts_with("invalid-"));
                assert_eq!(facts["rejected"], true, "{name}");
                if name.starts_with("invalid-") {
                    assert_eq!(facts["timers"], 0);
                    assert_eq!(row["events"], json!([]));
                } else {
                    assert_eq!(facts["retryConflict"], false, "{name}");
                }
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_close_retains_blocking_enforcement_after_observer_loss() {
    let w = World::new();
    let c = Arc::new(Clock::default());
    let entered = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let (open, seen, held, failed) = (w.clone(), entered.clone(), gate.clone(), w.clone());
    let g = AudioMuteGuard::with_clock(
        &policy(Some("s"), false, 1),
        "c",
        move || failed.failure(),
        move |id| {
            if seen.fetch_add(1, Ordering::SeqCst) == 1 {
                let (lock, wake) = &*held;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = wake.wait(released).unwrap();
                }
            }
            open.open(id)
        },
        c.clone(),
    )
    .unwrap();
    c.tick();
    until(|| entered.load(Ordering::SeqCst) == 2).await;
    let first = g.close();
    assert!(!first.finished());
    assert!(g.close().finished());
    assert!(conflict(&w, &c, "s").await);
    drop(first);
    drop(g);
    assert!(conflict(&w, &c, "s").await);
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    until(|| c.created.load(Ordering::SeqCst) == 2).await;
    let replacement = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match start(&w, &c, policy(None, true, 60), "s") {
                Ok(g) => break g,
                Err(_) => tokio::task::yield_now().await,
            }
        }
    })
    .await
    .unwrap();
    replacement.close().wait().await.unwrap();
    let result=w.snapshot("pending-close",json!({"secondCloseImmediate":true,"leaseHeldDuringBlockingIo":true,"allFirstObserversDropped":true,"timers":c.created.load(Ordering::SeqCst)}));
    assert_eq!(result["failed"], 0);
    assert_eq!(result["states"]["s"], false);
    assert_eq!(
        result["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e.as_str() == Some("close:s"))
            .count(),
        3
    );
    trace("mute-retention.json", &result);
}
