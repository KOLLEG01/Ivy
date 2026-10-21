//! Browser push delivery coordination. Encryption and HTTPS are required providers.
use crate::{worker_client::RequestOptions, worker_runtime::StorageClient};
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
pub type SendFuture = Pin<Box<dyn Future<Output = SendOutcome> + Send>>;
pub type CloseFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Issue the request when called; its future observes that posted operation.
pub trait Worker: Send + Sync {
    fn request(&self, payload: IvyJson) -> RequestFuture;
}
impl Worker for StorageClient {
    fn request(&self, payload: IvyJson) -> RequestFuture {
        let receiver = StorageClient::request(self, payload, RequestOptions::default());
        Box::pin(async move { receiver.await.map_err(|_| IvyError::internal())? })
    }
}
/// `None` preserves an absent statusCode. Rejected nullish status codes become
/// zero; a nullish thrown value itself cannot be inspected by the source catch.
pub enum SendOutcome {
    Response(Option<IvyJson>),
    Rejected(Option<IvyJson>),
    UninspectableFailure,
}
pub trait Sender: Send + Sync {
    /// Supply the actual web-push encryption/HTTPS operation, including its
    /// timeout. No transport, plaintext delivery or generated-key fallback exists.
    fn send(&self, subscription: IvyJson, payload: String, options: IvyJson) -> SendFuture;
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
/// Call lifecycle methods within the owning Tokio runtime. Background interval
/// tasks do not keep that runtime alive, like the original unref'ed timer.
#[derive(Clone)]
pub struct BrowserPush(Arc<Inner>);

fn field<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a IvyJson> {
    value.get(key).ok_or_else(IvyError::internal)
}
fn truthy(value: Option<&IvyJson>) -> bool {
    match value {
        None | Some(IvyJson::Null) => false,
        Some(IvyJson::Bool(v)) => *v,
        Some(IvyJson::Number(v)) => *v != 0.0 && !v.is_nan(),
        Some(IvyJson::String(v)) => !v.0.is_empty(),
        _ => true,
    }
}
fn options(keys: &IvyJson, subject: &JsString) -> IvyJson {
    let mut details = match keys {
        IvyJson::Object(_) => keys.clone(),
        IvyJson::Array(values) => IvyJson::object(
            values
                .iter()
                .enumerate()
                .map(|(i, v)| (i.to_string(), v.clone())),
        ),
        IvyJson::String(value) => IvyJson::object(
            value
                .0
                .iter()
                .enumerate()
                .map(|(i, v)| (i.to_string(), IvyJson::String(JsString(vec![*v])))),
        ),
        _ => IvyJson::object(std::iter::empty::<(&str, IvyJson)>()),
    };
    details.insert("subject", IvyJson::String(subject.clone()));
    IvyJson::object([
        ("vapidDetails", details),
        ("TTL", IvyJson::Number(3600.0)),
        ("timeout", IvyJson::Number(10_000.0)),
    ])
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
        // The source retains only its latest interval handle, also on repeated
        // start. Dropping an older Tokio handle detaches rather than cancels it.
        self.0.state.lock().unwrap().timer = Some(timer.abort_handle());
        self.wake();
    }
    pub fn wake(&self) {
        let mut state = self.0.state.lock().unwrap();
        state.requested = true;
        if state.running.is_some() || state.stopped {
            return;
        }
        let (done, waiting) = watch::channel(false);
        state.running = Some(waiting);
        state.requested = false;
        drop(state);
        // Post before returning, so a second synchronous wake remains pending
        // while the first request is outstanding, as at the source's first await.
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
            .request(IvyJson::object([("action", "browserPush.next".into())]))
    }
    async fn drain(&self, mut next: RequestFuture) -> IvyResult<()> {
        loop {
            let batch = next.await?;
            if truthy(batch.get("hasMore")) {
                self.0.state.lock().unwrap().requested = true;
            }
            let jobs = field(&batch, "deliveries")?
                .as_array()
                .ok_or_else(IvyError::internal)?;
            if !jobs.is_empty() {
                let keys = self
                    .0
                    .worker
                    .request(IvyJson::object([("action", "browserPush.keys".into())]))
                    .await?;
                // Invoke every sender before observing any result. An early
                // completion failure must not cancel the other mapped jobs.
                let pending = jobs
                    .iter()
                    .map(|job| {
                        let sent = (|| {
                            Ok(self.0.sender.send(
                                field(job, "subscription")?.clone(),
                                stringify(field(job, "notice")?, usize::MAX)?,
                                options(&keys, &self.0.subject),
                            ))
                        })();
                        (job.get("id").cloned(), sent)
                    })
                    .collect::<Vec<(Option<IvyJson>, IvyResult<SendFuture>)>>();
                let (completed, mut replies) = mpsc::unbounded_channel();
                for (id, sent) in pending {
                    let worker = self.0.worker.clone();
                    let completed = completed.clone();
                    tokio::spawn(async move {
                        let result = async {
                            let status = match sent?.await {
                                SendOutcome::Response(status) => status,
                                SendOutcome::Rejected(status) => Some(
                                    status
                                        .filter(|v| !v.is_null())
                                        .unwrap_or_else(|| IvyJson::Number(0.0)),
                                ),
                                SendOutcome::UninspectableFailure => {
                                    return Err(IvyError::internal());
                                }
                            };
                            let mut request =
                                IvyJson::object([("action", "browserPush.complete".into())]);
                            if let Some(id) = id {
                                request.insert("id", id);
                            }
                            if let Some(status) = status {
                                request.insert("status", status);
                            }
                            worker.request(request).await.map(|_| ())
                        }
                        .await;
                        let _ = completed.send(result);
                    });
                }
                drop(completed);
                while let Some(result) = replies.recv().await {
                    result?;
                }
                self.0.state.lock().unwrap().requested = true;
            }
            {
                let mut state = self.0.state.lock().unwrap();
                if !state.requested || state.stopped {
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
