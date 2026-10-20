use crate::{
    IvyError, IvyJson, IvyResult, JsString,
    clock::{Clock, SystemClock},
    rpc::{RequestOptions, RpcClient},
};
use futures_util::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use ivy_contracts::{
    json::{canonical, number_from_string, try_js_string},
    operation::operation_id_js,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, OnceLock, Weak},
};

type SharedResult<T> = Shared<BoxFuture<'static, IvyResult<T>>>;
type IdentityFuture = SharedResult<JsString>;
#[derive(Clone)]
struct Pending<T> {
    identity: Arc<()>,
    future: SharedResult<T>,
}
struct Scope {
    issued_at: i64,
    pending: Pending<JsString>,
}
#[derive(Default)]
struct Entries {
    epoch: Option<Pending<Option<IvyJson>>>,
    scopes: HashMap<String, Scope>,
    order: VecDeque<String>,
}
struct State {
    entries: Mutex<Entries>,
    clock: Arc<dyn Clock>,
}
struct Owner {
    client: Weak<dyn RpcClient>,
    state: Arc<State>,
}
fn state(client: &Arc<dyn RpcClient>) -> Arc<State> {
    static OWNERS: OnceLock<Mutex<HashMap<usize, Owner>>> = OnceLock::new();
    let mut owners = OWNERS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    // Do not retain RPC clients. Reap dead owners before any new cache access.
    owners.retain(|_, owner| owner.client.strong_count() > 0);
    let key = Arc::as_ptr(client) as *const () as usize;
    owners
        .entry(key)
        .or_insert_with(|| Owner {
            client: Arc::downgrade(client),
            state: Arc::new(State {
                entries: Mutex::new(Entries::default()),
                clock: Arc::new(SystemClock),
            }),
        })
        .state
        .clone()
}
fn drive<T: Clone + Send + Sync + 'static>(pending: &Pending<T>) {
    // Like a JS Promise, a started shared request survives an individual waiter.
    let future = pending.future.clone();
    // Enter the request before returning to the caller, rather than waiting for
    // the background task to be scheduled behind a later initialization call.
    if future.clone().now_or_never().is_some() {
        return;
    }
    tokio::spawn(async move {
        let _ = future.await;
    });
}
impl State {
    fn epoch(self: &Arc<Self>, client: Arc<dyn RpcClient>) -> IdentityFuture {
        self.raw_epoch(client)
            .map(|result| match result? {
                Some(IvyJson::String(ref value)) => Ok(value.clone()),
                None => Err(IvyError::new(
                    "invalid_arguments",
                    "Missing required property: runtimeEpoch",
                )),
                Some(_) => Err(IvyError::new(
                    "invalid_frame",
                    "Hive returned an invalid runtime epoch.",
                )),
            })
            .boxed()
            .shared()
    }
    fn raw_epoch(self: &Arc<Self>, client: Arc<dyn RpcClient>) -> SharedResult<Option<IvyJson>> {
        let mut entries = self.entries.lock().unwrap();
        if let Some(pending) = &entries.epoch {
            return pending.future.clone();
        }
        let identity = Arc::new(());
        let token = identity.clone();
        let owner = Arc::downgrade(self);
        let future = async move {
            let result = async {
                let status = client
                    .request(
                        "system.status",
                        IvyJson::object(Vec::<(String, IvyJson)>::new()),
                        RequestOptions::default(),
                    )
                    .await?;
                // The original property lookup does not validate the value.
                // Missing fields resolve successfully and stay cached; only a
                // null receiver rejects the lookup.
                if status.is_null() {
                    Err(IvyError::internal())
                } else {
                    Ok(status.get("runtimeEpoch").cloned())
                }
            }
            .await;
            // Promise callbacks settle asynchronously even when request() has
            // an immediately available result. Keep synchronous sibling starts
            // attached to this same pending lookup until that settlement.
            tokio::task::yield_now().await;
            if result.is_err() {
                if let Some(owner) = owner.upgrade() {
                    let mut entries = owner.entries.lock().unwrap();
                    if entries
                        .epoch
                        .as_ref()
                        .is_some_and(|entry| Arc::ptr_eq(&entry.identity, &token))
                    {
                        entries.epoch = None;
                    }
                }
            }
            result
        }
        .boxed()
        .shared();
        let pending = Pending {
            identity,
            future: future.clone(),
        };
        entries.epoch = Some(pending.clone());
        drop(entries);
        drive(&pending);
        future
    }
    async fn new_id(
        self: &Arc<Self>,
        client: Arc<dyn RpcClient>,
        nonce: Option<JsString>,
    ) -> IvyResult<JsString> {
        let epoch = self.raw_epoch(client).await?;
        let time = self.clock.utc_millis();
        let nonce = nonce.unwrap_or_else(|| uuid::Uuid::new_v4().to_string().into());
        id_from_epoch(epoch.as_ref(), time, &nonce)
    }
    fn scoped(
        self: &Arc<Self>,
        client: Arc<dyn RpcClient>,
        scope: &IvyJson,
    ) -> IvyResult<IdentityFuture> {
        let key = canonical(scope, 65_536)?;
        let now = self.clock.utc_millis();
        let mut entries = self.entries.lock().unwrap();
        while let Some(key) = entries.order.front() {
            if i128::from(entries.scopes[key].issued_at) + 86_400_000 > i128::from(now) {
                break;
            }
            let key = entries.order.pop_front().unwrap();
            entries.scopes.remove(&key);
        }
        if let Some(entry) = entries.scopes.get(&key) {
            return Ok(entry.pending.future.clone());
        }
        let identity = Arc::new(());
        let token = identity.clone();
        let owner = self.clone();
        let scope_key = key.clone();
        let future = async move {
            let result = owner.new_id(client, None).await;
            if result.is_err() {
                let mut entries = owner.entries.lock().unwrap();
                if entries
                    .scopes
                    .get(&scope_key)
                    .is_some_and(|entry| Arc::ptr_eq(&entry.pending.identity, &token))
                {
                    entries.scopes.remove(&scope_key);
                    entries.order.retain(|key| key != &scope_key);
                }
            }
            result
        }
        .boxed()
        .shared();
        let pending = Pending {
            identity,
            future: future.clone(),
        };
        entries.order.push_back(key.clone());
        entries.scopes.insert(
            key,
            Scope {
                issued_at: now,
                pending: pending.clone(),
            },
        );
        drop(entries);
        drive(&pending);
        Ok(future)
    }
}

fn invalid_identity() -> IvyError {
    IvyError::new("invalid_arguments", "Invalid operation identity fields.")
}

fn id_from_epoch(epoch: Option<&IvyJson>, time: i64, nonce: &JsString) -> IvyResult<JsString> {
    let Some(epoch) = epoch else {
        return Err(IvyError::internal());
    };
    match epoch {
        IvyJson::Null => Err(IvyError::internal()),
        IvyJson::String(value) => operation_id_js(
            value,
            u64::try_from(time).map_err(|_| invalid_identity())?,
            nonce,
        ),
        IvyJson::Array(values) => {
            if values.is_empty()
                || values
                    .iter()
                    .any(|value| matches!(value, IvyJson::String(s) if s.0 == [58]))
            {
                return Err(invalid_identity());
            }
            if !(0..=9_007_199_254_740_991).contains(&time)
                || nonce.0.is_empty()
                || nonce.0.len() > 128
                || nonce.0.contains(&58)
            {
                return Err(invalid_identity());
            }
            // Array.includes(':') checks elements before template coercion.
            // Revalidating the resulting text would reject original successes
            // such as ['epoch:part'], and coercion follows nonce validation.
            let mut identity = try_js_string(epoch)?;
            identity.0.push(58);
            identity.0.extend(time.to_string().encode_utf16());
            identity.0.push(58);
            identity.0.extend_from_slice(&nonce.0);
            Ok(identity)
        }
        IvyJson::Object(_) => {
            let positive_length = match epoch.get("length") {
                None | Some(IvyJson::Null) => false,
                Some(IvyJson::Bool(value)) => *value,
                Some(IvyJson::Number(value)) => *value > 0.0,
                Some(value) => number_from_string(&try_js_string(value)?) > 0.0,
            };
            // JSON objects cannot provide the callable includes method that
            // the original invokes after a positive length check.
            Err(if positive_length {
                IvyError::internal()
            } else {
                invalid_identity()
            })
        }
        _ => Err(invalid_identity()),
    }
}

/// A started lookup whose original success or failure survives cache eviction.
/// Cloning or waiting never reacquires an epoch from the client cache.
#[derive(Clone)]
pub struct RuntimeEpochWaiter {
    future: IdentityFuture,
}
impl RuntimeEpochWaiter {
    pub async fn get(&self) -> IvyResult<JsString> {
        self.future.clone().await
    }
}
/// Start inside a Tokio runtime; retain the returned waiter for one workflow
/// owner, including when a later owner retries after this lookup has failed.
pub fn start_runtime_epoch(client: Arc<dyn RpcClient>) -> RuntimeEpochWaiter {
    RuntimeEpochWaiter {
        future: state(&client).epoch(client),
    }
}

/// Cache one successful epoch per live RPC client; concurrent callers share the request.
pub async fn runtime_epoch(client: Arc<dyn RpcClient>) -> IvyResult<JsString> {
    start_runtime_epoch(client).get().await
}
pub async fn new_operation_id(
    client: Arc<dyn RpcClient>,
    nonce: Option<JsString>,
) -> IvyResult<JsString> {
    state(&client).new_id(client, nonce).await
}
/// Stable for a live workflow owner. Persist the returned identity before durable effects.
pub async fn scoped_operation_id(
    client: Arc<dyn RpcClient>,
    scope: &IvyJson,
) -> IvyResult<JsString> {
    state(&client).scoped(client, scope)?.await
}
