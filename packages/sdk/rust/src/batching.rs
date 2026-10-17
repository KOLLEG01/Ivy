use crate::{
    IvyError, IvyJson, IvyResult, Outcome,
    admission::{RequestPermit, RequestQueue},
    rpc::response_value,
};
use futures_util::future::BoxFuture;
use ivy_contracts::{
    json::{canonical, stringify},
    limits::*,
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

type Exchange =
    dyn Fn(String, CancellationToken) -> BoxFuture<'static, IvyResult<IvyJson>> + Send + Sync;
struct Consumer {
    result: oneshot::Sender<IvyResult<IvyJson>>,
    sent: Arc<AtomicBool>,
}
struct Read {
    key: String,
    id: String,
    frame: IvyJson,
    bytes: usize,
    consumers: Mutex<HashMap<u64, Consumer>>,
    sent: AtomicBool,
    cancellation: CancellationToken,
    permit: Mutex<Option<RequestPermit>>,
    batch: Mutex<Option<Weak<Batch>>>,
}
struct Batch {
    reads: Vec<Arc<Read>>,
    cancellation: CancellationToken,
}
struct State {
    reads: HashMap<String, Arc<Read>>,
    waiting: Vec<Arc<Read>>,
    timer: bool,
}
pub(crate) struct ReadBatcher {
    state: Mutex<State>,
    sequence: AtomicU64,
    queue: Arc<RequestQueue>,
    exchange: Arc<Exchange>,
}
struct Observation {
    owner: Arc<ReadBatcher>,
    read: Arc<Read>,
    consumer: u64,
}
impl Drop for Observation {
    fn drop(&mut self) {
        let empty = {
            let mut state = self.owner.state.lock().unwrap();
            let mut consumers = self.read.consumers.lock().unwrap();
            consumers.remove(&self.consumer);
            let empty = consumers.is_empty();
            if empty
                && state
                    .reads
                    .get(&self.read.key)
                    .is_some_and(|current| Arc::ptr_eq(current, &self.read))
            {
                state.reads.remove(&self.read.key);
            }
            empty
        };
        if empty {
            self.owner.retire(&self.read);
        }
    }
}
impl ReadBatcher {
    pub(crate) fn new(queue: Arc<RequestQueue>, exchange: Arc<Exchange>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                reads: HashMap::new(),
                waiting: Vec::new(),
                timer: false,
            }),
            sequence: AtomicU64::new(0),
            queue,
            exchange,
        })
    }
    pub(crate) fn invalidate(&self) {
        self.state.lock().unwrap().reads.clear();
    }
    pub(crate) async fn request(
        self: &Arc<Self>,
        method: &str,
        params: IvyJson,
        sent: Arc<AtomicBool>,
    ) -> IvyResult<IvyJson> {
        let key = format!("{method}\0{}", canonical(&params, MANAGEMENT_FRAME_BYTES)?);
        let consumer = self.sequence.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        let (read, created) = {
            let mut state = self.state.lock().unwrap();
            let (read, created) = if let Some(read) = state.reads.get(&key) {
                (read.clone(), false)
            } else {
                let id = uuid::Uuid::new_v4().to_string();
                let frame = IvyJson::object([
                    ("jsonrpc", "2.0".into()),
                    ("id", id.clone().into()),
                    ("method", method.into()),
                    ("params", params),
                ]);
                let bytes = stringify(&frame, MANAGEMENT_FRAME_BYTES)?.len();
                let read = Arc::new(Read {
                    key: key.clone(),
                    id,
                    frame,
                    bytes,
                    consumers: Mutex::new(HashMap::new()),
                    sent: AtomicBool::new(false),
                    cancellation: CancellationToken::new(),
                    permit: Mutex::new(None),
                    batch: Mutex::new(None),
                });
                state.reads.insert(key, read.clone());
                (read, true)
            };
            if read.sent.load(Ordering::Acquire) {
                sent.store(true, Ordering::Release);
            }
            read.consumers.lock().unwrap().insert(
                consumer,
                Consumer {
                    result: sender,
                    sent,
                },
            );
            (read, created)
        };
        let observation = Observation {
            owner: self.clone(),
            read: read.clone(),
            consumer,
        };
        if created {
            let owner = self.clone();
            tokio::spawn(async move {
                owner.admit(read).await;
            });
        }
        let result = receiver.await.map_err(|_| IvyError::internal())?;
        drop(observation);
        result
    }
    async fn admit(self: Arc<Self>, read: Arc<Read>) {
        let provider = is_provider(&read.frame);
        match self.queue.acquire(provider, &read.cancellation).await {
            Err(error) => self.finish(&read, Err(error)),
            Ok(permit) => {
                *read.permit.lock().unwrap() = Some(permit);
                if read.cancellation.is_cancelled() {
                    self.retire(&read);
                    return;
                }
                let schedule = {
                    let mut state = self.state.lock().unwrap();
                    state.waiting.push(read);
                    let schedule = !state.timer;
                    state.timer = true;
                    schedule
                };
                if schedule {
                    let owner = self.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        owner.flush();
                    });
                }
            }
        }
    }
    fn flush(self: &Arc<Self>) {
        let waiting = {
            let mut state = self.state.lock().unwrap();
            state.timer = false;
            std::mem::take(&mut state.waiting)
        };
        let mut lanes: Vec<(String, Vec<Arc<Read>>)> = Vec::new();
        for read in waiting {
            if read.cancellation.is_cancelled() {
                continue;
            }
            let key = if is_provider(&read.frame) {
                let params = read.frame.get("params").unwrap();
                let selector = IvyJson::object(
                    ["serviceNodeId", "serviceName", "resourceRef"]
                        .map(|key| (key, params.get(key).cloned().unwrap_or(IvyJson::Null))),
                );
                match canonical(&selector, MANAGEMENT_FRAME_BYTES) {
                    Ok(key) => format!("provider:{key}"),
                    Err(error) => {
                        self.finish(&read, Err(error));
                        continue;
                    }
                }
            } else {
                "hive".into()
            };
            if let Some((_, lane)) = lanes.iter_mut().find(|(k, _)| k == &key) {
                lane.push(read);
            } else {
                lanes.push((key, vec![read]));
            }
        }
        for (_, lane) in lanes {
            let mut batch = Vec::new();
            let mut bytes = 2;
            for read in lane {
                if !batch.is_empty()
                    && (batch.len() == RPC_BATCH_REQUESTS
                        || bytes + read.bytes + 1 > RPC_BATCH_REQUEST_BYTES)
                {
                    self.launch(std::mem::take(&mut batch));
                    bytes = 2;
                }
                bytes += read.bytes + 1;
                batch.push(read);
            }
            if !batch.is_empty() {
                self.launch(batch);
            }
        }
    }
    fn launch(self: &Arc<Self>, reads: Vec<Arc<Read>>) {
        let batch = Arc::new(Batch {
            reads,
            cancellation: CancellationToken::new(),
        });
        for read in &batch.reads {
            *read.batch.lock().unwrap() = Some(Arc::downgrade(&batch));
            let consumers = read.consumers.lock().unwrap();
            read.sent.store(true, Ordering::Release);
            for consumer in consumers.values() {
                consumer.sent.store(true, Ordering::Release);
            }
        }
        if batch.reads.iter().all(|r| r.cancellation.is_cancelled()) {
            batch.cancellation.cancel();
        }
        let owner = self.clone();
        tokio::spawn(async move {
            owner.send(batch).await;
        });
    }
    async fn send(self: Arc<Self>, batch: Arc<Batch>) {
        let frame = if batch.reads.len() == 1 {
            batch.reads[0].frame.clone()
        } else {
            IvyJson::Array(batch.reads.iter().map(|r| r.frame.clone()).collect())
        };
        let result = match stringify(&frame, MANAGEMENT_FRAME_BYTES) {
            Ok(body) => (self.exchange)(body, batch.cancellation.clone()).await,
            Err(error) => Err(error),
        };
        let results = match result {
            Ok(value) => batch_results(&batch.reads, &value),
            Err(error) => vec![Err(error); batch.reads.len()],
        };
        for (read, result) in batch.reads.iter().zip(results) {
            self.finish(read, result);
        }
    }
    fn finish(&self, read: &Arc<Read>, result: IvyResult<IvyJson>) {
        let consumers = {
            let mut state = self.state.lock().unwrap();
            if state
                .reads
                .get(&read.key)
                .is_some_and(|current| Arc::ptr_eq(current, read))
            {
                state.reads.remove(&read.key);
            }
            std::mem::take(&mut *read.consumers.lock().unwrap())
        };
        self.retire(read);
        for (_, consumer) in consumers {
            let _ = consumer.result.send(result.clone());
        }
    }
    fn retire(&self, read: &Arc<Read>) {
        {
            let mut state = self.state.lock().unwrap();
            if state
                .reads
                .get(&read.key)
                .is_some_and(|current| Arc::ptr_eq(current, read))
            {
                state.reads.remove(&read.key);
            }
        }
        read.cancellation.cancel();
        read.permit.lock().unwrap().take();
        if let Some(batch) = read
            .batch
            .lock()
            .unwrap()
            .take()
            .and_then(|batch| batch.upgrade())
        {
            if batch.reads.iter().all(|r| r.cancellation.is_cancelled()) {
                batch.cancellation.cancel();
            }
        }
    }
}
fn is_provider(frame: &IvyJson) -> bool {
    matches!(
        frame.get("method").and_then(|v| v.as_str().ok()).as_deref(),
        Some("tools.call" | "discovery.call")
    )
}
fn invalid(message: &str) -> IvyError {
    IvyError::new("invalid_frame", message).with_outcome(Outcome::Unknown)
}
fn batch_results(reads: &[Arc<Read>], value: &IvyJson) -> Vec<IvyResult<IvyJson>> {
    let Some(frames) = value.as_array() else {
        if reads.len() > 1 && !value.get("id").is_some_and(IvyJson::is_null) {
            return vec![Err(invalid("Hive returned no batch response.")); reads.len()];
        }
        return reads.iter().map(|r| response_value(value, &r.id)).collect();
    };
    let mut indexed = HashMap::new();
    for frame in frames {
        let id = frame.get("id").and_then(|v| v.as_str().ok());
        let Some(id) = id.filter(|id| reads.iter().any(|r| &r.id == id)) else {
            return vec![Err(invalid("Hive batch response identities are invalid.")); reads.len()];
        };
        if indexed.insert(id, frame).is_some() {
            return vec![Err(invalid("Hive batch response identities are invalid.")); reads.len()];
        }
    }
    if indexed.len() != reads.len() {
        return vec![Err(invalid("Hive batch response is incomplete.")); reads.len()];
    }
    reads
        .iter()
        .map(|r| response_value(indexed[&r.id], &r.id))
        .collect()
}
