//! The original service model catalog policy. Process launch, reconnect and
//! installed schema compilation stay with the Voice control connection owner.
use super::VoiceRpc;
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::codex_rpc::RequestOptions;
use std::{
    collections::HashSet,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, Weak},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;

/// Required connection-owner boundary, shared with other Voice operations.
/// `open` applies the owner's stopping/reconnect policy before returning its
/// current RPC. `current` observes identity without opening another connection.
pub trait VoiceModelControl: Send + Sync {
    fn open(&self) -> Pin<Box<dyn Future<Output = IvyResult<VoiceRpc>> + Send + '_>>;
    fn current(&self) -> Option<VoiceRpc>;
}

type Loading = watch::Receiver<Option<IvyResult<IvyJson>>>;
struct Cache {
    // Remember identity without extending the transport actor's lifetime.
    connection: Weak<IvyJson>,
    models: IvyJson,
    expires_at: i128,
}
#[derive(Default)]
struct State {
    cache: Option<Cache>,
    loading: Option<Loading>,
}
struct Inner {
    control: Arc<dyn VoiceModelControl>,
    now: Arc<dyn Fn() -> i128 + Send + Sync>,
    state: Mutex<State>,
}

/// Coalesced model discovery, with independent result copies for every observer.
/// Dropping an observer leaves the dispatched load running to its normal result.
#[derive(Clone)]
pub struct VoiceModelCatalog(Arc<Inner>);

impl VoiceModelCatalog {
    pub fn new(control: Arc<dyn VoiceModelControl>) -> Self {
        Self::with_clock(control, Arc::new(wall_time_ms))
    }

    fn with_clock(
        control: Arc<dyn VoiceModelControl>,
        now: Arc<dyn Fn() -> i128 + Send + Sync>,
    ) -> Self {
        Self(Arc::new(Inner {
            control,
            now,
            state: Mutex::new(State::default()),
        }))
    }

    pub async fn models(&self) -> IvyResult<IvyJson> {
        // Opening precedes even a cache hit, preserving the owner's stop policy.
        let connection = self.0.control.open().await?;
        let current = self
            .0
            .control
            .current()
            .is_some_and(|rpc| rpc.same_connection(&connection))
            && connection.transport().connected();
        let mut loading = {
            let mut state = self.0.state.lock().unwrap();
            if state.cache.as_ref().is_some_and(|cache| {
                !current
                    || !cache
                        .connection
                        .ptr_eq(&Arc::downgrade(&connection.catalog))
            }) {
                state.cache = None;
            }
            if let Some(cache) = &state.cache {
                if cache.expires_at > (self.0.now)() {
                    return Ok(cache.models.clone());
                }
            }
            // An unexpected producer panic must not poison every future read.
            if state
                .loading
                .as_ref()
                .is_some_and(|load| load.has_changed().is_err())
            {
                state.loading = None;
            }
            if let Some(loading) = &state.loading {
                loading.clone()
            } else {
                let (send, receive) = watch::channel(None);
                state.loading = Some(receive.clone());
                let owner = self.0.clone();
                tokio::spawn(async move {
                    let result = discover(&owner, &connection).await;
                    {
                        let mut state = owner.state.lock().unwrap();
                        if let Ok(models) = &result {
                            state.cache = Some(Cache {
                                connection: Arc::downgrade(&connection.catalog),
                                models: models.clone(),
                                expires_at: (owner.now)() + 60_000,
                            });
                        }
                        state.loading = None;
                    }
                    send.send_replace(Some(result));
                });
                receive
            }
        };
        loop {
            if let Some(result) = loading.borrow_and_update().as_ref() {
                return result.clone();
            }
            loading.changed().await.map_err(|_| IvyError::internal())?;
        }
    }
}

pub(super) fn wall_time_ms() -> i128 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(value) => value.as_millis() as i128,
        Err(value) => -(value.duration().as_millis() as i128),
    }
}

async fn discover(owner: &Inner, connection: &VoiceRpc) -> IvyResult<IvyJson> {
    let definition = connection
        .catalog
        .get("clientRequests")
        .and_then(IvyJson::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("method") == Some(&"model/list".into()))
        })
        .ok_or_else(|| {
            IvyError::new(
                "native_method_unavailable",
                "Configured Codex does not provide a model catalog.",
            )
        })?;
    let output_schema = definition
        .get("outputSchema")
        .ok_or_else(IvyError::internal)?;
    let mut models = Vec::<IvyJson>::new();
    let mut cursors = HashSet::<JsString>::new();
    let mut cursor = None;
    loop {
        let mut params = IvyJson::object([
            ("limit", IvyJson::Number(100.0)),
            ("includeHidden", IvyJson::Bool(false)),
        ]);
        if let Some(cursor) = cursor.take() {
            if let IvyJson::Object(fields) = &mut params {
                fields.push(("cursor".into(), IvyJson::String(cursor)));
            }
        }
        // Each original call reads the current connection, without opening or
        // reconnecting it. The final result still belongs to the initial one.
        let rpc = owner.control.current().ok_or_else(|| {
            IvyError::new(
                "phone_voice_unavailable",
                "Codex Voice transport is unavailable.",
            )
        })?;
        let page = rpc
            .call(
                "model/list",
                params,
                RequestOptions {
                    detach_on_timeout: true,
                    ..RequestOptions::default()
                },
                None,
            )
            .await?;
        connection
            .schemas
            .validate(output_schema, &page, 256 * 1024)?;
        for item in page
            .get("data")
            .and_then(IvyJson::as_array)
            .ok_or_else(IvyError::internal)?
        {
            if item.get("hidden").and_then(IvyJson::as_bool) == Some(true) {
                continue;
            }
            if models.len() >= 256
                || models
                    .iter()
                    .any(|existing| existing.get("model") == item.get("model"))
            {
                return Err(IvyError::new(
                    "phone_voice_catalog_invalid",
                    "Codex returned a duplicate or oversized model catalog.",
                ));
            }
            let fields = [
                "model",
                "displayName",
                "description",
                "defaultReasoningEffort",
                "supportedReasoningEfforts",
                "isDefault",
            ]
            .into_iter()
            .map(|key| {
                Ok((
                    JsString::from(key),
                    item.get(key).cloned().ok_or_else(IvyError::internal)?,
                ))
            })
            .collect::<IvyResult<Vec<_>>>()?;
            models.push(IvyJson::Object(fields));
        }
        cursor = match page.get("nextCursor") {
            None | Some(IvyJson::Null) => None,
            Some(IvyJson::String(value)) if value.0.is_empty() => None,
            Some(IvyJson::String(value)) => Some(value.clone()),
            _ => return Err(IvyError::internal()), // Rejected by the installed schema.
        };
        let Some(next) = &cursor else { break };
        if cursors.contains(next) || cursors.len() >= 256 {
            return Err(IvyError::new(
                "phone_voice_catalog_invalid",
                "Codex model catalog pagination did not complete.",
            ));
        }
        cursors.insert(next.clone());
    }
    if !owner
        .control
        .current()
        .is_some_and(|rpc| rpc.same_connection(connection))
        || !connection.transport().connected()
    {
        return Err(IvyError::new(
            "phone_voice_unavailable",
            "Codex model catalog lost its control connection.",
        ));
    }
    Ok(IvyJson::Array(models))
}

#[cfg(test)]
#[path = "../../../../../tests/rust/phone_voice_models.rs"]
mod tests;
