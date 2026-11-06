//! Transient caller-scoped native replies, independent of durable operation receipts.
//! A missing reply never proves that a native mutation did not execute.

use crate::service_work::run::{self, AgentRun};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, Outcome, hash::hash_object};
use std::{
    fmt,
    future::Future,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy)]
pub struct Limits {
    pub count: usize,
    pub bytes: usize,
    pub ttl_ms: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            count: 256,
            bytes: 32 * 1024 * 1024,
            ttl_ms: 5 * 60_000,
        }
    }
}

/// The native transport must honor both flags; a timeout detaches its waiter,
/// retaining the native request ID until a late reply or connection close.
pub struct Request {
    pub request_id: String,
    pub method: JsString,
    pub params: IvyJson,
    pub detach_on_timeout: bool,
    pub interaction_priority: bool,
}

struct Entry {
    caller: JsString,
    id: JsString,
    hash: String,
    reply: AgentRun,
    bytes: usize,
    expires: Option<u64>,
}
impl Entry {
    fn metadata_bytes(&self) -> usize {
        Self::metadata(&self.caller, &self.id, &self.hash)
    }
    fn metadata(caller: &JsString, id: &JsString, hash: &String) -> usize {
        std::mem::size_of::<Self>()
            + (caller.0.capacity() + id.0.capacity()) * 2
            + hash.capacity()
            + std::mem::size_of::<run::Reply>()
            + 256 // shared result cell, watch and reference-count metadata
    }
}
struct State {
    entries: Vec<Entry>,
    limits: Limits,
    budget: run::Budget,
}
impl State {
    fn prune(&mut self, now: u64, incoming: usize) {
        self.entries
            .retain(|e| e.expires.is_none_or(|expires| expires > now));
        let mut bytes = self.bytes().saturating_add(incoming);
        let mut index = 0;
        while index < self.entries.len() {
            if self.entries.len() < self.limits.count && bytes < self.limits.bytes {
                break;
            }
            if self.entries[index].expires.is_none() {
                index += 1;
                continue;
            }
            bytes -= self.entries.remove(index).bytes;
        }
    }
    fn bytes(&self) -> usize {
        self.entries
            .iter()
            .fold(0usize, |bytes, e| bytes.saturating_add(e.bytes))
    }
    fn reserve(&mut self, input: usize) -> IvyResult<run::Admission> {
        loop {
            match self.budget.reserve(true, input) {
                Ok(admission) => return Ok(admission),
                Err(error) => {
                    // A cache-only result must not fill the shared call limit
                    // until TTL. Caller-held handles keep their own reservation.
                    let Some(index) = self
                        .entries
                        .iter()
                        .position(|e| e.expires.is_some() && e.reply.cache_only())
                    else {
                        return Err(error);
                    };
                    self.entries.remove(index);
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct NativeInteractions {
    state: Arc<Mutex<State>>,
    epoch: JsString,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
}

/// A retained handle is shared by duplicate callers even after cache eviction.
/// Dropping it does not cancel the independently owned native dispatch.
pub struct PendingInteraction(AgentRun);
impl fmt::Debug for PendingInteraction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingInteraction").finish_non_exhaustive()
    }
}
impl PendingInteraction {
    pub async fn wait(self) -> IvyResult<IvyJson> {
        self.0.wait().await
    }
}

struct Completion {
    owner: NativeInteractions,
    caller: JsString,
    id: JsString,
    budget: run::Budget,
    sender: Option<run::Sender>,
}
impl Completion {
    fn finish(&mut self, value: IvyResult<IvyJson>, returned: bool) {
        let Some(mut sender) = self.sender.take() else {
            return;
        };
        let unexecuted = returned
            && value
                .as_ref()
                .is_err_and(|e| e.outcome == Outcome::NotExecuted);
        let result = self.budget.finish(value, true);
        if let Ok(mut state) = self.owner.state.lock() {
            if let Some(index) = state
                .entries
                .iter()
                .position(|e| e.caller == self.caller && e.id == self.id)
            {
                if unexecuted {
                    state.entries.remove(index);
                } else {
                    let ttl = state.limits.ttl_ms;
                    let entry = &mut state.entries[index];
                    entry.bytes = entry.metadata_bytes().saturating_add(match &result.value {
                        Ok(value) => value.retained_bytes(),
                        Err(error) => error.retained_bytes(),
                    });
                    // Producer loss does not settle native work. Protect the
                    // original identity while releasing its freed input bytes.
                    entry.expires = returned.then(|| (self.owner.clock)().saturating_add(ttl));
                }
            }
            if returned {
                state.prune((self.owner.clock)(), 0);
            }
        }
        sender.send(result);
    }
}
impl Drop for Completion {
    fn drop(&mut self) {
        if self.sender.is_some() {
            self.finish(Err(IvyError::internal()), false);
        }
    }
}

impl NativeInteractions {
    pub fn new(epoch: JsString, limits: Limits) -> Self {
        Self::with_clock(
            epoch,
            limits,
            Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64
            }),
        )
    }

    pub fn with_clock(
        epoch: JsString,
        limits: Limits,
        clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                entries: Vec::new(),
                limits,
                budget: run::Budget::default(),
            })),
            epoch,
            clock,
        }
    }

    pub fn lookup(&self, caller: &JsString, id: &JsString) -> IvyResult<PendingInteraction> {
        let state = self.state.lock().map_err(|_| IvyError::internal())?;
        let entry = state
            .entries
            .iter()
            .find(|e| &e.caller == caller && &e.id == id)
            .filter(|e| e.expires.is_none_or(|expires| expires > (self.clock)()));
        entry.map(|e| PendingInteraction(e.reply.clone())).ok_or_else(||
            IvyError::new("interaction_expired", "No retained interaction reply; reconcile with native thread state before another mutation.")
                .with_outcome(Outcome::Unknown))
    }

    /// The service's existing control tick also frees expired replies while idle.
    /// Outstanding dispatches and caller-held reply handles keep their own owners.
    pub fn prune(&self) -> IvyResult<usize> {
        let mut state = self.state.lock().map_err(|_| IvyError::internal())?;
        let before = state.entries.len();
        state.prune((self.clock)(), 0);
        Ok(before - state.entries.len())
    }

    /// Admission is synchronous so callers can observe capacity/conflict before
    /// starting work. The supplied dispatch future runs once on the owning runtime.
    pub fn run<F, Fut>(
        &self,
        caller: JsString,
        id: JsString,
        method: JsString,
        params: IvyJson,
        dispatch: F,
    ) -> IvyResult<PendingInteraction>
    where
        F: FnOnce(Request) -> Fut + Send + 'static,
        Fut: Future<Output = IvyResult<IvyJson>> + Send + 'static,
    {
        let method_key = JsString::from("method");
        let params_key = JsString::from("params");
        let method = IvyJson::String(method);
        let hash = hash_object([(&method_key, &method), (&params_key, &params)])?;
        let mut state = self.state.lock().map_err(|_| IvyError::internal())?;
        // Match the original ordering: a retained identity wins before pruning,
        // even if lookup would report its TTL expired.
        if let Some(entry) = state
            .entries
            .iter()
            .find(|e| e.caller == caller && e.id == id)
        {
            if entry.hash != hash {
                return Err(IvyError::new(
                    "mutation_conflict",
                    "Interaction identity has different arguments.",
                ));
            }
            return Ok(PendingInteraction(entry.reply.clone()));
        }
        // Reserve inputs until completion; transport queues have their own budgets.
        let bytes = Entry::metadata(&caller, &id, &hash)
            .saturating_add((caller.0.capacity() + id.0.capacity() + self.epoch.0.capacity()) * 2)
            .saturating_add(method.retained_bytes())
            .saturating_add(params.retained_bytes());
        state.prune((self.clock)(), bytes);
        if state.entries.len() >= state.limits.count
            || state.bytes().saturating_add(bytes) >= state.limits.bytes
        {
            return Err(IvyError::new(
                "native_capacity",
                "Native interaction count or retained-memory capacity is occupied.",
            ));
        }
        // All interaction requests already use the transport's control priority.
        let admission = state.reserve(bytes)?;
        let budget = state.budget.clone();
        let (sender, receiver) = AgentRun::channel(admission.call.clone());
        state.entries.push(Entry {
            caller: caller.clone(),
            id: id.clone(),
            hash,
            reply: receiver.clone(),
            bytes,
            expires: None,
        });
        drop(state);
        let method = method
            .into_js_string()
            .expect("interaction method is a string");
        let mut completion = Completion {
            owner: self.clone(),
            caller,
            id,
            budget,
            sender: Some(sender),
        };
        tokio::spawn(async move {
            let _input = admission;
            let request = Request {
                request_id: format!("interaction-{}", uuid::Uuid::new_v4()),
                method,
                params,
                detach_on_timeout: true,
                interaction_priority: true,
            };
            let result = dispatch(request).await.map(|reply| {
                IvyJson::object([
                    ("operationId", IvyJson::String(completion.id.clone())),
                    ("epoch", IvyJson::String(completion.owner.epoch.clone())),
                    ("reply", reply),
                ])
            });
            completion.finish(result, true);
        });
        Ok(PendingInteraction(receiver))
    }
}
