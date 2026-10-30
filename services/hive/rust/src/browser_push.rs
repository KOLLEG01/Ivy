//! Browser push delivery coordination. Encryption and HTTPS are required providers.
use crate::storage::{Payload, RequestOptions, StorageClient};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, json::stringify};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    task::AbortHandle,
    time::{Instant, MissedTickBehavior},
};

pub const POLL_INTERVAL_MS: u64 = 15_000;
pub type RequestFuture = Pin<Box<dyn Future<Output = IvyResult<IvyJson>> + Send>>;
pub type SendFuture = Pin<Box<dyn Future<Output = IvyResult<u16>> + Send>>;
pub type CloseFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Issue the request when called; its future observes that posted operation.
pub trait Worker: Send + Sync {
    fn request(&self, payload: Payload) -> RequestFuture;
}
impl Worker for StorageClient {
    fn request(&self, payload: Payload) -> RequestFuture {
        StorageClient::request(self, payload, RequestOptions::default())
    }
}
#[derive(Clone)]
pub struct VapidDetails {
    pub public_key: String,
    pub private_key: String,
    pub subject: String,
}
impl VapidDetails {
    /// Read the persisted key object at the worker boundary. The configured
    /// subject belongs to the sender, independently of stored extra fields.
    pub fn from_keys(keys: &IvyJson, subject: &JsString) -> IvyResult<Self> {
        keys.as_object().ok_or_else(IvyError::internal)?;
        Ok(Self {
            public_key: field(keys, "publicKey")?.as_str()?,
            private_key: field(keys, "privateKey")?.as_str()?,
            subject: subject.to_utf8()?,
        })
    }
}
pub trait Sender: Send + Sync {
    /// Return the HTTP status after delivery, or a transport/encryption error.
    fn send(&self, subscription: IvyJson, payload: String, details: VapidDetails) -> SendFuture;
}

#[derive(Default)]
struct State {
    requested: bool,
    stopped: bool,
    running: Option<watch::Receiver<bool>>,
    timer: Option<AbortHandle>,
}
struct Inner {
    worker: Arc<dyn Worker>,
    sender: Arc<dyn Sender>,
    subject: JsString,
    state: Mutex<State>,
}
/// Call lifecycle methods within the owning Tokio runtime.
#[derive(Clone)]
pub struct BrowserPush(Arc<Inner>);

fn field<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a IvyJson> {
    value.get(key).ok_or_else(IvyError::internal)
}
impl BrowserPush {
    pub fn new(worker: Arc<dyn Worker>, subject: JsString, sender: Arc<dyn Sender>) -> Self {
        Self(Arc::new(Inner {
            worker,
            sender,
            subject,
            state: Mutex::new(State::default()),
        }))
    }
    pub fn start(&self) {
        let mut state = self.0.state.lock().unwrap();
        if state.stopped || state.timer.is_some() {
            return;
        }
        let owner = self.clone();
        let first = Instant::now() + Duration::from_millis(POLL_INTERVAL_MS);
        let timer = tokio::spawn(async move {
            let mut interval =
                tokio::time::interval_at(first, Duration::from_millis(POLL_INTERVAL_MS));
            interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                owner.wake();
            }
        });
        state.timer = Some(timer.abort_handle());
        drop(state);
        self.wake();
    }
    pub fn wake(&self) {
        let mut state = self.0.state.lock().unwrap();
        if state.stopped {
            return;
        }
        state.requested = true;
        if state.running.is_some() {
            return;
        }
        let (done, waiting) = watch::channel(false);
        state.running = Some(waiting);
        state.requested = false;
        drop(state);
        // Own the admitted storage request before returning. Further wakes
        // coalesce while the current drain is running.
        let first = self.next();
        let owner = self.clone();
        tokio::spawn(async move {
            let _ = owner.drain(first).await;
            let restart = {
                let mut state = owner.0.state.lock().unwrap();
                state.running = None;
                state.requested && !state.stopped
            };
            if restart {
                owner.wake();
            }
            done.send_replace(true);
        });
    }
    fn next(&self) -> RequestFuture {
        self.0
            .worker
            .request(IvyJson::object([("action", "browserPush.next".into())]).into())
    }
    async fn drain(&self, mut next: RequestFuture) -> IvyResult<()> {
        loop {
            let batch = next.await?;
            let has_more = field(&batch, "hasMore")?
                .as_bool()
                .ok_or_else(IvyError::internal)?;
            let jobs = field(&batch, "deliveries")?
                .as_array()
                .ok_or_else(IvyError::internal)?;
            if !jobs.is_empty() {
                let keys = self
                    .0
                    .worker
                    .request(IvyJson::object([("action", "browserPush.keys".into())]).into())
                    .await?;
                let details = VapidDetails::from_keys(&keys, &self.0.subject)?;
                // Validate the batch before starting deliveries. Invalid worker
                // data leaves the queue intact for repair and a later wake.
                let jobs = jobs
                    .iter()
                    .map(|job| {
                        Ok((
                            field(job, "id")?.clone(),
                            field(job, "subscription")?.clone(),
                            stringify(field(job, "notice")?, usize::MAX)?,
                        ))
                    })
                    .collect::<IvyResult<Vec<_>>>()?;
                let pending = jobs
                    .into_iter()
                    .map(|(id, subscription, payload)| {
                        (
                            id,
                            self.0.sender.send(subscription, payload, details.clone()),
                        )
                    })
                    .collect::<Vec<_>>();
                let (completed, mut replies) = mpsc::unbounded_channel();
                for (id, sent) in pending {
                    let worker = self.0.worker.clone();
                    let completed = completed.clone();
                    tokio::spawn(async move {
                        let result = async {
                            // Zero is the existing persisted retry outcome when
                            // no HTTP response is available.
                            let status = sent.await.unwrap_or(0);
                            let request = IvyJson::object([
                                ("action", "browserPush.complete".into()),
                                ("id", id),
                                ("status", IvyJson::Number(status as f64)),
                            ]);
                            worker.request(request.into()).await.map(|_| ())
                        }
                        .await;
                        let _ = completed.send(result);
                    });
                }
                drop(completed);
                let mut failure = None;
                while let Some(result) = replies.recv().await {
                    if let Err(error) = result {
                        failure.get_or_insert(error);
                    }
                }
                if let Some(error) = failure {
                    return Err(error);
                }
            }
            {
                let mut state = self.0.state.lock().unwrap();
                if state.stopped || !(state.requested || has_more || !jobs.is_empty()) {
                    return Ok(());
                }
                state.requested = false;
            }
            next = self.next();
        }
    }
    /// Stop admission/timer immediately, then observe only the current drain.
    /// Caller abandonment does not cancel already issued sends or completions.
    pub fn close(&self) -> CloseFuture {
        let mut state = self.0.state.lock().unwrap();
        state.stopped = true;
        if let Some(timer) = state.timer.take() {
            timer.abort();
        }
        let running = state.running.clone();
        Box::pin(async move {
            if let Some(mut waiting) = running {
                loop {
                    if *waiting.borrow_and_update() {
                        break;
                    }
                    if waiting.changed().await.is_err() {
                        break;
                    }
                }
            }
        })
    }
}
