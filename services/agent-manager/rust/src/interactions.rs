//! Transient caller-scoped native replies, independent of durable operation receipts.
//! A missing reply never proves that a native mutation did not execute.

use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome, hash::hash_json, json::stringify,
};
use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;

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

type Reply = Option<IvyResult<IvyJson>>;
struct Entry {
    caller: JsString,
    id: JsString,
    hash: String,
    reply: watch::Receiver<Reply>,
    bytes: usize,
    expires: Option<u64>,
}
struct State {
    entries: Vec<Entry>,
    limits: Limits,
}
impl State {
    fn prune(&mut self, now: u64) {
        self.entries
            .retain(|e| e.expires.is_none_or(|expires| expires > now));
        let mut bytes: usize = self.entries.iter().map(|e| e.bytes).sum();
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
}

#[derive(Clone)]
pub struct NativeInteractions {
    state: Arc<Mutex<State>>,
    epoch: JsString,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
}

/// A retained handle is shared by duplicate callers even after cache eviction.
/// Dropping it does not cancel the independently owned native dispatch.
#[derive(Debug)]
pub struct PendingInteraction(watch::Receiver<Reply>);
impl PendingInteraction {
    pub async fn wait(mut self) -> IvyResult<IvyJson> {
        loop {
            if let Some(result) = self.0.borrow_and_update().clone() {
                return result;
            }
            if self.0.changed().await.is_err() {
                return Err(IvyError::internal());
            }
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
        let hash = hash_json(&IvyJson::object([
            ("method", IvyJson::String(method.clone())),
            ("params", params.clone()),
        ]))?;
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
        state.prune((self.clock)());
        if state.entries.len() >= state.limits.count {
            return Err(IvyError::new(
                "native_capacity",
                "Native interaction capacity is occupied.",
            ));
        }
        let (sender, receiver) = watch::channel(None);
        state.entries.push(Entry {
            caller: caller.clone(),
            id: id.clone(),
            hash,
            reply: receiver.clone(),
            bytes: 0,
            expires: None,
        });
        let this = self.clone();
        tokio::spawn(async move {
            let request = Request {
                request_id: format!("interaction-{}", uuid::Uuid::new_v4()),
                method,
                params,
                detach_on_timeout: true,
                interaction_priority: true,
            };
            let result = dispatch(request).await.and_then(|reply| {
                let value = IvyJson::object([
                    ("operationId", IvyJson::String(id.clone())),
                    ("epoch", IvyJson::String(this.epoch.clone())),
                    ("reply", reply),
                ]);
                // JSON.stringify charges actual escaped wire bytes, not canonical
                // key order or Rust's in-memory representation.
                let bytes = stringify(&value, usize::MAX)?.len();
                Ok((value, bytes))
            });
            if let Ok(mut state) = this.state.lock() {
                if let Some(index) = state
                    .entries
                    .iter()
                    .position(|e| e.caller == caller && e.id == id)
                {
                    if result
                        .as_ref()
                        .is_err_and(|e| e.outcome == Outcome::NotExecuted)
                    {
                        state.entries.remove(index);
                    } else {
                        let ttl = state.limits.ttl_ms;
                        let entry = &mut state.entries[index];
                        entry.bytes = result.as_ref().map_or(0, |(_, bytes)| *bytes);
                        entry.expires = Some((this.clock)().saturating_add(ttl));
                    }
                }
                state.prune((this.clock)());
            }
            sender.send_replace(Some(result.map(|(value, _)| value)));
        });
        Ok(PendingInteraction(receiver))
    }
}
