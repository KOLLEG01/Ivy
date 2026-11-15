//! The original service model catalog policy. Process launch, reconnect and
//! installed schema compilation stay with the Voice control connection owner.
use super::VoiceRpc;
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::codex_rpc::{NativeRpc, RequestOptions};
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

#[derive(Debug)]
pub struct VoiceReasoningEffort {
    pub reasoning_effort: JsString,
    description: JsString,
}

#[derive(Debug)]
pub struct VoiceModel {
    pub model: JsString,
    display_name: JsString,
    description: JsString,
    default_reasoning_effort: JsString,
    pub supported_reasoning_efforts: Box<[VoiceReasoningEffort]>,
    is_default: bool,
}

impl VoiceModel {
    fn from_wire(item: &IvyJson) -> IvyResult<Self> {
        Ok(Self {
            model: string(item, "model")?,
            display_name: string(item, "displayName")?,
            description: string(item, "description")?,
            default_reasoning_effort: string(item, "defaultReasoningEffort")?,
            supported_reasoning_efforts: item
                .required("supportedReasoningEfforts")?
                .as_array()
                .ok_or_else(IvyError::internal)?
                .iter()
                .map(|effort| {
                    Ok(VoiceReasoningEffort {
                        reasoning_effort: string(effort, "reasoningEffort")?,
                        description: string(effort, "description")?,
                    })
                })
                .collect::<IvyResult<_>>()?,
            is_default: item
                .required("isDefault")?
                .as_bool()
                .ok_or_else(IvyError::internal)?,
        })
    }

    fn to_wire(&self) -> IvyJson {
        IvyJson::object([
            ("model", self.model.clone().into()),
            ("displayName", self.display_name.clone().into()),
            ("description", self.description.clone().into()),
            (
                "defaultReasoningEffort",
                self.default_reasoning_effort.clone().into(),
            ),
            (
                "supportedReasoningEfforts",
                IvyJson::Array(
                    self.supported_reasoning_efforts
                        .iter()
                        .map(|effort| {
                            IvyJson::object([
                                ("reasoningEffort", effort.reasoning_effort.clone().into()),
                                ("description", effort.description.clone().into()),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("isDefault", self.is_default.into()),
        ])
    }
}

fn string(item: &IvyJson, key: &str) -> IvyResult<JsString> {
    item.required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}

/// Immutable advertised values; cache observers and the call policy share this
/// allocation. JSON is materialized only for the public status response.
#[derive(Debug, Default)]
pub struct VoiceModels {
    pub entries: Box<[VoiceModel]>,
}

impl VoiceModels {
    pub fn to_wire(&self) -> IvyJson {
        IvyJson::Array(self.entries.iter().map(VoiceModel::to_wire).collect())
    }

    pub fn from_wire(value: &IvyJson) -> IvyResult<Self> {
        Ok(Self {
            entries: value
                .as_array()
                .ok_or_else(IvyError::internal)?
                .iter()
                .map(VoiceModel::from_wire)
                .collect::<IvyResult<_>>()?,
        })
    }
}

type Loading = watch::Receiver<Option<IvyResult<Arc<VoiceModels>>>>;
struct Cache {
    // Remember identity without extending the transport actor's lifetime.
    connection: Weak<NativeRpc>,
    models: Arc<VoiceModels>,
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

/// Coalesced model discovery, sharing immutable values between observers.
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

    pub async fn models(&self) -> IvyResult<Arc<VoiceModels>> {
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
                !current || !cache.connection.ptr_eq(&Arc::downgrade(&connection.rpc))
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
                    let result = discover(&owner, &connection).await.map(Arc::new);
                    {
                        let mut state = owner.state.lock().unwrap();
                        if let Ok(models) = &result {
                            state.cache = Some(Cache {
                                connection: Arc::downgrade(&connection.rpc),
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

async fn discover(owner: &Inner, connection: &VoiceRpc) -> IvyResult<VoiceModels> {
    let definition = connection
        .rpc
        .catalog()
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
    let mut models = Vec::<VoiceModel>::new();
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
                || models.iter().any(|existing| {
                    Some(&existing.model) == item.get("model").and_then(IvyJson::as_js_string)
                })
            {
                return Err(IvyError::new(
                    "phone_voice_catalog_invalid",
                    "Codex returned a duplicate or oversized model catalog.",
                ));
            }
            models.push(VoiceModel::from_wire(item)?);
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
    Ok(VoiceModels {
        entries: models.into_boxed_slice(),
    })
}

#[cfg(test)]
#[path = "../../../../../tests/rust/phone_voice_models.rs"]
mod tests;
