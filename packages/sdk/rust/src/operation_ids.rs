use crate::{
    IvyError, IvyJson, IvyResult, JsString,
    clock::{Clock, SystemClock},
    rpc::{RequestOptions, RpcClient},
};
use futures_util::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use ivy_contracts::{json::canonical, operation::operation_id_js};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, OnceLock, Weak},
};

type IdentityFuture = Shared<BoxFuture<'static, IvyResult<JsString>>>;
#[derive(Clone)]
struct Pending {
    identity: Arc<()>,
    future: IdentityFuture,
}
struct Scope {
    issued_at: i64,
    pending: Pending,
}
#[derive(Default)]
struct Entries {
    epoch: Option<Pending>,
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
fn drive(pending: &Pending) {
    // Like a JS Promise, a started shared request survives an individual waiter.
    let future = pending.future.clone();
    tokio::spawn(async move {
        let _ = future.await;
    });
}
impl State {
    fn epoch(self: &Arc<Self>, client: Arc<dyn RpcClient>) -> IdentityFuture {
        let mut entries = self.entries.lock().unwrap();
        if let Some(pending) = &entries.epoch {
            return pending.future.clone();
        }
        let identity = Arc::new(());
        let token = identity.clone();
        let owner = Arc::downgrade(self);
        let future = async move {
            let result = async {
                client
                    .request(
                        "system.status",
                        IvyJson::object(Vec::<(String, IvyJson)>::new()),
                        RequestOptions::default(),
                    )
                    .await?
                    .required("runtimeEpoch")?
                    .as_js_string()
                    .cloned()
                    .ok_or_else(|| {
                        IvyError::new("invalid_frame", "Hive returned an invalid runtime epoch.")
                    })
            }
            .await;
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
        let epoch = self.epoch(client).await?;
        let time = u64::try_from(self.clock.utc_millis()).map_err(|_| {
            IvyError::new("invalid_arguments", "Invalid operation identity fields.")
        })?;
        operation_id_js(
            &epoch,
            time,
            &nonce.unwrap_or_else(|| uuid::Uuid::new_v4().to_string().into()),
        )
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

/// Cache one successful epoch per live RPC client; concurrent callers share the request.
pub async fn runtime_epoch(client: Arc<dyn RpcClient>) -> IvyResult<JsString> {
    state(&client).epoch(client).await
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
