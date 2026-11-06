//! Durable native claims. Memory coordinates live calls; SQLite owns receipts.
use crate::{
    local_state::{LocalAgentState, Pin, SavedState},
    native_clock::NATIVE_CLOCK_PRINCIPAL,
    native_journal::{Identity, NativeJournal},
    service_work::{
        AgentWork,
        run::{Reply, Sender},
    },
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    agent_schema::validate_agent,
    hash::{hash_json, hash_object},
    native_backend::NativeProvider,
};
use std::{
    collections::{BTreeMap, HashMap},
    future::Future,
    pin::Pin as FuturePin,
    sync::{Arc, Mutex},
};

#[path = "native_operation_admission.rs"]
mod admission;

pub type StoreFuture<'a, T> = FuturePin<Box<dyn Future<Output = IvyResult<T>> + Send + 'a>>;
pub trait NativeOperationStore: Send + Sync {
    fn service_node_id(&self) -> &JsString;
    fn storage_identity(&self) -> Option<(&JsString, NativeProvider)> {
        None
    }
    fn owns_receipt(&self, value: &IvyJson) -> bool {
        value.get("serviceNodeId") == Some(&IvyJson::String(self.service_node_id().clone()))
    }
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
    storage_owner: JsString,
    storage_provider: Option<NativeProvider>,
    backend: Option<ivy_contracts::native_backend::BackendIdentity>,
}
impl SqliteOperationStore {
    pub fn new(state: Arc<Mutex<LocalAgentState>>) -> IvyResult<Self> {
        let locked = state.lock().map_err(|_| IvyError::internal())?;
        let service_node_id = locked.service_node_id.clone().into();
        let storage_owner = locked.storage_owner().into();
        let storage_provider = locked.native_provider()?;
        let backend = locked.backend_identity()?;
        drop(locked);
        Ok(Self {
            state,
            service_node_id,
            storage_owner,
            storage_provider,
            backend,
        })
    }
}
impl NativeOperationStore for SqliteOperationStore {
    fn service_node_id(&self) -> &JsString {
        &self.service_node_id
    }
    fn storage_identity(&self) -> Option<(&JsString, NativeProvider)> {
        Some((&self.storage_owner, self.storage_provider?))
    }
    fn owns_receipt(&self, value: &IvyJson) -> bool {
        let service = value.get("serviceNodeId").and_then(IvyJson::as_js_string);
        let selected =
            ivy_contracts::native_backend::BackendIdentity::matches(value, self.backend.as_ref());
        (service == Some(&self.service_node_id) && selected)
            || (service == Some(&self.storage_owner)
                && (selected || value.get("backend").is_none()))
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

pub use crate::service_work::AgentRun as NativeRun;
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
    admission: admission::Admission,
}
#[derive(Clone)]
pub struct NativeOperations {
    inner: Arc<Inner>,
}

/// Terminal observations enqueue reconciliation outside the native RPC actor.
/// Shutdown drains these tasks before closing the shared SQLite connection.
#[derive(Clone, Default)]
pub struct ReceiptWork {
    work: AgentWork,
}
impl ReceiptWork {
    pub(crate) fn track(
        &self,
        work: impl Future<Output = IvyResult<()>> + Send + 'static,
    ) -> IvyResult<NativeRun> {
        self.work.track(Box::pin(async move {
            work.await?;
            Ok(IvyJson::Null)
        }))
    }
    fn spawn(
        &self,
        work: impl Future<Output = IvyResult<()>> + Send + 'static,
        report: Arc<dyn Fn(IvyError) + Send + Sync>,
    ) -> IvyResult<()> {
        self.work.spawn_reported(work, report)
    }
    /// Individual writer failures use their existing observer. Preserve errors
    /// from the work owner so shutdown can finish cleanup and report failure.
    pub async fn drain(&self) -> IvyResult<()> {
        self.work.drain().await
    }
}
// The task owns completion, independently of callers dropping their wait handles.
struct Completion {
    inner: Arc<Inner>,
    index: u64,
    work: Option<Sender>,
    claimed: Sender,
    control: bool,
}
impl Completion {
    fn finish(&mut self, result: IvyResult<IvyJson>) {
        // Drop the coordinator's old claim/result handles before retaining the
        // final result. Actual duplicate observers keep their original claim.
        {
            let mut active = self.inner.active.lock().unwrap();
            if let Some(call) = active.calls.remove(&self.index) {
                active.indices.remove(&call.key);
            }
        }
        let result = self.inner.admission.budget.finish(result, self.control);
        if self.claimed.pending() {
            self.claimed.send(result.clone());
        }
        if let Some(mut work) = self.work.take() {
            work.send(result);
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
                admission: admission::Admission::new(),
            }),
        }
    }
    pub fn journal(&self) -> Arc<Mutex<NativeJournal>> {
        self.inner.memory.clone()
    }
    /// Report the persisted origin, independently of the current RPC route.
    /// Missing provider provenance must be seeded by the original owner.
    pub fn storage_identity(&self) -> IvyResult<IvyJson> {
        let (original_service, provider) =
            self.inner.store.storage_identity().ok_or_else(|| {
                IvyError::new(
                    "native_state_migration_required",
                    "The retained state has no original native provider identity.",
                )
            })?;
        Ok(IvyJson::object([
            (
                "serviceNodeId",
                self.inner.store.service_node_id().clone().into(),
            ),
            ("originalServiceNodeId", original_service.clone().into()),
            ("provider", provider.as_str().into()),
        ]))
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
            let _ = work.completed().await;
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
            let _ = work.completed().await;
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
                && self.inner.store.owns_receipt(&saved.value),
            "target_conflict",
            "Receipt identity differs from its Hive key.",
        )?;
        let live = self.inner.memory.lock().unwrap().get(identity)?;
        if let Some(live) =
            live.filter(|live| live.get("requestHash") == saved.value.get("requestHash"))
        {
            let active = self.inner.active.lock().unwrap().indices.contains_key(&key);
            if !active && terminal(&live) {
                let observed_hash = hash_json(&live)?;
                if observed_hash != hash_json(&saved.value)? {
                    if let Err(error) = self
                        .inner
                        .store
                        .write(key.clone(), live.clone(), Some(saved.pin))
                        .await
                    {
                        if error.code != "revision_conflict" {
                            return Err(error);
                        }
                        // Another observer may have saved this exact terminal
                        // receipt. Confirm it; never repeat the stale CAS or
                        // accept a different owner, request, outcome or payload.
                        let current = self.inner.store.read(key).await?;
                        if !current.as_ref().is_some_and(|saved| {
                            hash_json(&saved.value).is_ok_and(|hash| hash == observed_hash)
                        }) {
                            return Err(error);
                        }
                    }
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
        F: FnOnce(IvyJson) -> Fut + Send + 'static,
        Fut: Future<Output = IvyResult<IvyJson>> + Send + 'static,
    {
        let key = Self::key(&identity)?;
        let request_hash = hash_object([
            (&"method".into(), &IvyJson::String(method.clone())),
            (&"params".into(), &params),
        ])?;
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
            let retained = self
                .inner
                .admission
                .reserve(&identity, &method, &params, true, prevent)?;
            let claimed = prior.claimed.clone();
            let manager = self.clone();
            let (mut sender, result) = NativeRun::channel(retained.call.clone());
            let control = retained.control;
            // Eager even if the duplicate waiter is dropped, as with Promise.then.
            tokio::spawn(async move {
                let _retained = retained;
                let result = async {
                    let original = claimed.shared().await?;
                    if original.value.is_err() {
                        return Ok(original);
                    }
                    Ok(match manager.read(&identity).await? {
                        Some(value) => manager.inner.admission.budget.finish(Ok(value), control),
                        None => original,
                    })
                }
                .await;
                sender.send(match result {
                    Ok(value) => value,
                    Err(error) => manager.inner.admission.budget.finish(Err(error), control),
                });
            });
            return Ok(result);
        }
        let retained = self
            .inner
            .admission
            .reserve(&identity, &method, &params, false, prevent)?;
        let index = active.next;
        active.next = active.next.checked_add(1).ok_or_else(IvyError::internal)?;
        let (work, result) = NativeRun::channel(retained.call.clone());
        let (claimed, claim) = NativeRun::channel(retained.call.clone());
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
                work: result.clone(),
                claimed: claim,
            },
        );
        let mut completion = Completion {
            inner: self.inner.clone(),
            index,
            work: Some(work),
            claimed,
            control: retained.control,
        };
        let manager = self.clone();
        tokio::spawn(async move {
            let _retained = retained;
            let result = manager
                .execute(
                    identity,
                    method,
                    params,
                    key,
                    request_hash,
                    action,
                    prevent,
                    &mut completion.claimed,
                    completion.control,
                )
                .await;
            completion.finish(result);
        });
        Ok(result)
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
        claimed: &mut Sender,
        control: bool,
    ) -> IvyResult<IvyJson>
    where
        F: FnOnce(IvyJson) -> Fut,
        Fut: Future<Output = IvyResult<IvyJson>>,
    {
        if let Some(existing) = self.read(&identity).await? {
            require(
                existing.get("requestHash") == Some(&IvyJson::from(request_hash)),
                "mutation_conflict",
                "Operation has different original arguments.",
            )?;
            return Ok(existing);
        }
        let (accepted, intent) = {
            let mut memory = self.inner.memory.lock().unwrap();
            // A missing store observation does not erase the original journal
            // owner. Never reclaim, prevent again or forget an already known
            // operation merely because its durable read is temporarily absent.
            if let Some(prior) = memory.get(&identity)? {
                drop(memory);
                require(
                    self.inner.store.owns_receipt(&prior),
                    "target_conflict",
                    "The journal receipt belongs to another native owner.",
                )?;
                require(
                    prior.get("requestHash") == Some(&IvyJson::from(request_hash.as_str())),
                    "mutation_conflict",
                    "Operation has different original arguments.",
                )?;
                return Ok(prior);
            }
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
        enum Claim {
            Prevented(IvyJson),
            Accepted(Arc<Reply>),
        }
        let claim = if prevent {
            Claim::Prevented(accepted)
        } else {
            match self.inner.admission.budget.retain(Ok(accepted), control) {
                Ok(value) => Claim::Accepted(value),
                Err(error) => {
                    self.inner.memory.lock().unwrap().forget(&identity)?;
                    return Err(error);
                }
            }
        };
        // The store owns the persisted intent. Keep only its CAS pin while the
        // native action runs, rather than another complete receipt body.
        let pin = match self.inner.store.write(key.clone(), intent, None).await {
            Ok(saved) => saved.pin,
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
        match claim {
            Claim::Prevented(accepted) => {
                self.inner
                    .memory
                    .lock()
                    .unwrap()
                    .retained_in_hive(&identity)?;
                return Ok(accepted);
            }
            Claim::Accepted(value) => claimed.send(value),
        }
        let result = match action(params).await {
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
            .write(key, result.clone(), Some(pin))
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
