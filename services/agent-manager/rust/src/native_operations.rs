//! Durable native claims. Memory coordinates live calls; SQLite owns receipts.
use crate::{
    local_state::{LocalAgentState, Pin, SavedState},
    native_clock::NATIVE_CLOCK_PRINCIPAL,
    native_journal::{Identity, NativeJournal},
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, agent_schema::validate_agent, hash::hash_json,
};
use std::{
    collections::{BTreeMap, HashMap},
    future::Future,
    pin::Pin as FuturePin,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

pub type StoreFuture<'a, T> = FuturePin<Box<dyn Future<Output = IvyResult<T>> + Send + 'a>>;
pub trait NativeOperationStore: Send + Sync {
    fn service_node_id(&self) -> &JsString;
    fn read(&self, key: String) -> StoreFuture<'_, Option<SavedState>>;
    fn write(
        &self,
        key: String,
        value: IvyJson,
        previous: Option<Pin>,
    ) -> StoreFuture<'_, SavedState>;
}

/// Shares the service's one connection, serializing SQLite off the async worker.
/// No transaction or database guard survives an await.
pub struct SqliteOperationStore {
    state: Arc<Mutex<LocalAgentState>>,
    service_node_id: JsString,
}
impl SqliteOperationStore {
    pub fn new(state: Arc<Mutex<LocalAgentState>>) -> IvyResult<Self> {
        let service_node_id = state
            .lock()
            .map_err(|_| IvyError::internal())?
            .service_node_id
            .clone()
            .into();
        Ok(Self {
            state,
            service_node_id,
        })
    }
}
impl NativeOperationStore for SqliteOperationStore {
    fn service_node_id(&self) -> &JsString {
        &self.service_node_id
    }
    fn read(&self, key: String) -> StoreFuture<'_, Option<SavedState>> {
        let state = self.state.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                state
                    .lock()
                    .map_err(|_| IvyError::internal())?
                    .read("operation", &key)
            })
            .await
            .map_err(|_| IvyError::internal())?
        })
    }
    fn write(
        &self,
        key: String,
        value: IvyJson,
        previous: Option<Pin>,
    ) -> StoreFuture<'_, SavedState> {
        let state = self.state.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                state.lock().map_err(|_| IvyError::internal())?.write(
                    "operation",
                    &key,
                    &value,
                    previous.as_ref(),
                )
            })
            .await
            .map_err(|_| IvyError::internal())?
        })
    }
}

type Receipt = Option<IvyResult<IvyJson>>;
#[derive(Clone)]
pub struct NativeRun {
    result: watch::Receiver<Receipt>,
}
impl NativeRun {
    pub async fn wait(mut self) -> IvyResult<IvyJson> {
        loop {
            if let Some(result) = self.result.borrow_and_update().clone() {
                return result;
            }
            self.result
                .changed()
                .await
                .map_err(|_| IvyError::internal())?;
        }
    }
}
struct Active {
    key: String,
    request_hash: String,
    method: JsString,
    thread_id: Option<JsString>,
    work: NativeRun,
    claimed: NativeRun,
}
#[derive(Default)]
struct ActiveCalls {
    indices: HashMap<String, u64>,
    calls: BTreeMap<u64, Active>,
    next: u64,
}
struct Inner {
    store: Arc<dyn NativeOperationStore>,
    memory: Arc<Mutex<NativeJournal>>,
    active: Mutex<ActiveCalls>,
}
#[derive(Clone)]
pub struct NativeOperations {
    inner: Arc<Inner>,
}

/// Terminal observations enqueue reconciliation outside the native RPC actor.
/// Shutdown drains these tasks before closing the shared SQLite connection.
#[derive(Clone, Default)]
pub struct ReceiptWork {
    state: Arc<Mutex<ReceiptTasks>>,
}
#[derive(Default)]
struct ReceiptTasks {
    next: u64,
    tasks: BTreeMap<u64, watch::Receiver<bool>>,
}
struct ReceiptCompletion {
    work: ReceiptWork,
    index: u64,
    sender: Option<watch::Sender<bool>>,
    report: Arc<dyn Fn(IvyError) + Send + Sync>,
}
impl ReceiptCompletion {
    fn finish(&mut self) {
        self.work.state.lock().unwrap().tasks.remove(&self.index);
        if let Some(sender) = self.sender.take() {
            sender.send_replace(true);
        }
    }
}
impl Drop for ReceiptCompletion {
    fn drop(&mut self) {
        if self.sender.is_some() {
            (self.report)(IvyError::internal());
            self.finish();
        }
    }
}
impl ReceiptWork {
    fn spawn(
        &self,
        work: impl Future<Output = IvyResult<()>> + Send + 'static,
        report: Arc<dyn Fn(IvyError) + Send + Sync>,
    ) -> IvyResult<()> {
        let (sender, receiver) = watch::channel(false);
        let index = {
            let mut state = self.state.lock().unwrap();
            let index = state.next;
            state.next = state.next.checked_add(1).ok_or_else(IvyError::internal)?;
            state.tasks.insert(index, receiver);
            index
        };
        let mut completion = ReceiptCompletion {
            work: self.clone(),
            index,
            sender: Some(sender),
            report,
        };
        tokio::spawn(async move {
            if let Err(error) = work.await {
                (completion.report)(error);
            }
            completion.finish();
        });
        Ok(())
    }
    pub async fn drain(&self) {
        loop {
            let next = self
                .state
                .lock()
                .unwrap()
                .tasks
                .first_key_value()
                .map(|(_, value)| value.clone());
            let Some(mut receiver) = next else {
                return;
            };
            loop {
                if *receiver.borrow_and_update() {
                    break;
                }
                if receiver.changed().await.is_err() {
                    break;
                }
            }
        }
    }
}
// The task owns completion, independently of callers dropping their wait handles.
struct Completion {
    inner: Arc<Inner>,
    index: u64,
    work: Option<watch::Sender<Receipt>>,
    claimed: watch::Sender<Receipt>,
}
impl Completion {
    fn finish(&mut self, result: IvyResult<IvyJson>) {
        let unclaimed = self.claimed.borrow().is_none();
        if unclaimed {
            self.claimed.send_replace(Some(result.clone()));
        }
        let mut active = self.inner.active.lock().unwrap();
        if let Some(call) = active.calls.remove(&self.index) {
            active.indices.remove(&call.key);
        }
        if let Some(work) = self.work.take() {
            work.send_replace(Some(result));
        }
    }
}
impl Drop for Completion {
    fn drop(&mut self) {
        if self.work.is_some() {
            self.finish(Err(IvyError::internal()));
        }
    }
}
fn terminal(value: &IvyJson) -> bool {
    ["succeeded", "failed", "outcome_unknown"]
        .iter()
        .any(|phase| is(value, "phase", phase))
}
fn is(value: &IvyJson, name: &str, expected: &str) -> bool {
    value.get(name) == Some(&IvyJson::from(expected))
}
fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}

impl NativeOperations {
    pub fn new(store: Arc<dyn NativeOperationStore>, memory: Arc<Mutex<NativeJournal>>) -> Self {
        Self {
            inner: Arc::new(Inner {
                store,
                memory,
                active: Mutex::new(ActiveCalls::default()),
            }),
        }
    }
    pub fn journal(&self) -> Arc<Mutex<NativeJournal>> {
        self.inner.memory.clone()
    }
    /// Installs the service's terminal callback. A weak coordinator avoids a
    /// journal -> callback -> coordinator -> journal ownership cycle.
    pub fn reconcile_terminals(&self, report: Arc<dyn Fn(IvyError) + Send + Sync>) -> ReceiptWork {
        let work = ReceiptWork::default();
        let worker = work.clone();
        let owner = Arc::downgrade(&self.inner);
        self.inner
            .memory
            .lock()
            .unwrap()
            .set_on_terminal(Some(Box::new(move |value| {
                if value.get("callerPrincipalId") == Some(&IvyJson::from(NATIVE_CLOCK_PRINCIPAL)) {
                    return Ok(());
                }
                let Some(inner) = owner.upgrade() else {
                    return Ok(());
                };
                let identity = Identity::from_operation(value)?;
                worker.spawn(
                    async move { NativeOperations { inner }.settle(&identity).await },
                    report.clone(),
                )
            })));
        work
    }
    fn key(identity: &Identity) -> IvyResult<String> {
        hash_json(&identity.value())
    }
    pub fn request_id(&self, identity: &Identity) -> IvyResult<String> {
        Ok(Self::key(identity)?[7..39].to_owned())
    }
    pub async fn settle(&self, identity: &Identity) -> IvyResult<()> {
        let key = Self::key(identity)?;
        let work = {
            let active = self.inner.active.lock().unwrap();
            active
                .indices
                .get(&key)
                .and_then(|index| active.calls.get(index))
                .map(|call| call.work.clone())
        };
        if let Some(work) = work {
            let _ = work.wait().await;
        }
        self.read(identity).await?;
        Ok(())
    }
    pub async fn settle_turn(&self, thread_id: &JsString) {
        let mut cursor = 0;
        loop {
            // Like a JS Map iterator, visit matching calls inserted while a prior
            // call is awaited, in insertion order (including delete/reinsert).
            let next = {
                let active = self.inner.active.lock().unwrap();
                active
                    .calls
                    .range(cursor..)
                    .find(|(_, call)| {
                        call.method == JsString::from("turn/start")
                            && call.thread_id.as_ref() == Some(thread_id)
                    })
                    .map(|(index, call)| (*index, call.work.clone()))
            };
            let Some((index, work)) = next else {
                return;
            };
            cursor = index + 1;
            let _ = work.wait().await;
        }
    }
    pub async fn read(&self, identity: &Identity) -> IvyResult<Option<IvyJson>> {
        let key = Self::key(identity)?;
        let Some(saved) = self.inner.store.read(key.clone()).await? else {
            return Ok(None);
        };
        validate_agent("Operation", &saved.value)?;
        require(
            saved.value.get("callerPrincipalId")
                == Some(&IvyJson::String(identity.caller_principal_id.clone()))
                && saved.value.get("operationId")
                    == Some(&IvyJson::String(identity.operation_id.clone()))
                && saved.value.get("serviceNodeId")
                    == Some(&IvyJson::String(self.inner.store.service_node_id().clone())),
            "target_conflict",
            "Receipt identity differs from its Hive key.",
        )?;
        let live = self.inner.memory.lock().unwrap().get(identity)?;
        if let Some(live) =
            live.filter(|live| live.get("requestHash") == saved.value.get("requestHash"))
        {
            let active = self.inner.active.lock().unwrap().indices.contains_key(&key);
            if !active && terminal(&live) {
                if hash_json(&live)? != hash_json(&saved.value)? {
                    self.inner
                        .store
                        .write(key, live.clone(), Some(saved.pin))
                        .await?;
                }
                self.inner
                    .memory
                    .lock()
                    .unwrap()
                    .retained_in_hive(identity)?;
            }
            return Ok(Some(live));
        }
        let mut value = saved.value;
        if is(&value, "phase", "accepted") || is(&value, "phase", "dispatched") {
            value.insert("phase", "outcome_unknown".into());
            value.insert("code", "native_connection_lost".into());
        }
        Ok(Some(value))
    }
    /// Admit immediately. The primary task persists its claim before calling
    /// action; duplicates wait only for that claim, then read the live receipt.
    pub fn run<F, Fut>(
        &self,
        identity: Identity,
        method: JsString,
        params: IvyJson,
        action: F,
        prevent: bool,
    ) -> IvyResult<NativeRun>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = IvyResult<IvyJson>> + Send + 'static,
    {
        let key = Self::key(&identity)?;
        let request_hash = hash_json(&IvyJson::object([
            ("method", IvyJson::String(method.clone())),
            ("params", params.clone()),
        ]))?;
        let mut active = self.inner.active.lock().unwrap();
        if let Some(prior) = active
            .indices
            .get(&key)
            .and_then(|index| active.calls.get(index))
        {
            require(
                prior.request_hash == request_hash,
                "mutation_conflict",
                "Operation has different arguments.",
            )?;
            let claimed = prior.claimed.clone();
            let manager = self.clone();
            let (sender, result) = watch::channel(None);
            // Eager even if the duplicate waiter is dropped, as with Promise.then.
            tokio::spawn(async move {
                let result = async {
                    let value = claimed.wait().await?;
                    Ok(manager.read(&identity).await?.unwrap_or(value))
                }
                .await;
                sender.send_replace(Some(result));
            });
            return Ok(NativeRun { result });
        }
        let index = active.next;
        active.next = active.next.checked_add(1).ok_or_else(IvyError::internal)?;
        let (work, result) = watch::channel(None);
        let (claimed, claim) = watch::channel(None);
        let thread_id = params
            .as_object()
            .and_then(|_| params.get("threadId"))
            .and_then(IvyJson::as_js_string)
            .cloned();
        active.indices.insert(key.clone(), index);
        active.calls.insert(
            index,
            Active {
                key: key.clone(),
                request_hash: request_hash.clone(),
                method: method.clone(),
                thread_id,
                work: NativeRun {
                    result: result.clone(),
                },
                claimed: NativeRun { result: claim },
            },
        );
        let mut completion = Completion {
            inner: self.inner.clone(),
            index,
            work: Some(work),
            claimed,
        };
        let manager = self.clone();
        tokio::spawn(async move {
            let result = manager
                .execute(
                    identity,
                    method,
                    params,
                    key,
                    request_hash,
                    action,
                    prevent,
                    &completion.claimed,
                )
                .await;
            completion.finish(result);
        });
        Ok(NativeRun { result })
    }
    async fn execute<F, Fut>(
        &self,
        identity: Identity,
        method: JsString,
        params: IvyJson,
        key: String,
        request_hash: String,
        action: F,
        prevent: bool,
        claimed: &watch::Sender<Receipt>,
    ) -> IvyResult<IvyJson>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = IvyResult<IvyJson>>,
    {
        if let Some(existing) = self.read(&identity).await? {
            require(
                existing.get("requestHash") == Some(&IvyJson::from(request_hash)),
                "mutation_conflict",
                "Operation has different original arguments.",
            )?;
            claimed.send_replace(Some(Ok(existing.clone())));
            return Ok(existing);
        }
        let (accepted, intent) = {
            let mut memory = self.inner.memory.lock().unwrap();
            let accepted = if prevent {
                memory.prevent(&identity, &method, &params)?
            } else {
                memory.accept(&identity, &method, &params)?.0
            };
            let mut intent = accepted.clone();
            if !prevent {
                intent.insert("phase", "dispatched".into());
                intent.insert(
                    "epoch",
                    memory
                        .epoch()
                        .cloned()
                        .map(IvyJson::String)
                        .unwrap_or(IvyJson::Null),
                );
                let request_id = if method == JsString::from("agent.answer") {
                    params
                        .get("identity")
                        .and_then(|v| v.get("requestId"))
                        .cloned()
                        .ok_or_else(IvyError::internal)?
                } else {
                    self.request_id(&identity)?.into()
                };
                intent.insert("requestId", request_id);
            }
            (accepted, intent)
        };
        validate_agent("Operation", &intent)?;
        let saved = match self.inner.store.write(key.clone(), intent, None).await {
            Ok(saved) => saved,
            Err(error) => {
                self.inner.memory.lock().unwrap().forget(&identity)?;
                if [
                    "mutation_conflict",
                    "revision_conflict",
                    "path_conflict",
                    "name_conflict",
                    "already_exists",
                ]
                .contains(&error.code.as_str())
                {
                    if let Some(found) = self.read(&identity).await? {
                        require(
                            found.get("requestHash") == Some(&IvyJson::from(request_hash)),
                            "mutation_conflict",
                            "Operation has different arguments.",
                        )?;
                        return Ok(found);
                    }
                }
                return Err(error);
            }
        };
        claimed.send_replace(Some(Ok(accepted.clone())));
        if prevent {
            self.inner
                .memory
                .lock()
                .unwrap()
                .retained_in_hive(&identity)?;
            return Ok(accepted);
        }
        let result = match action().await {
            Ok(result) => result,
            Err(error) => {
                let mut memory = self.inner.memory.lock().unwrap();
                match memory.get(&identity)? {
                    Some(observed) if is(&observed, "phase", "accepted") => {
                        memory.reject_before_dispatch(&identity, &error.code_js())?
                    }
                    Some(observed) if terminal(&observed) => observed,
                    _ => return Err(error), // A dispatched effect without a terminal receipt remains uncertain.
                }
            }
        };
        self.inner
            .store
            .write(key, result.clone(), Some(saved.pin))
            .await?;
        if terminal(&result) {
            self.inner
                .memory
                .lock()
                .unwrap()
                .retained_in_hive(&identity)?;
        }
        Ok(result)
    }
}
