//! Cached execution profile with durable compare-and-swap and original replay rules.
use crate::{
    local_state::{LocalAgentState, Pin, SavedState},
    native_journal::JournalClock,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, agent_schema::validate_agent, hash::hash_json,
};
use ivy_sdk::service::{ServiceFuture, ToolHandler};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    pin::Pin as FuturePin,
    sync::{Arc, Mutex},
};

pub type ProfileFuture<'a, T> = FuturePin<Box<dyn Future<Output = IvyResult<T>> + Send + 'a>>;
pub trait CapabilityStore: Send + Sync {
    fn read(&self) -> ProfileFuture<'_, Option<SavedState>>;
    fn write(&self, value: IvyJson, previous: Option<Pin>) -> ProfileFuture<'_, SavedState>;
}

/// Uses the service's shared SQLite owner; no database lock survives an await.
pub struct SqliteCapabilityStore(pub Arc<Mutex<LocalAgentState>>);
impl CapabilityStore for SqliteCapabilityStore {
    fn read(&self) -> ProfileFuture<'_, Option<SavedState>> {
        let state = self.0.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                state
                    .lock()
                    .map_err(|_| IvyError::internal())?
                    .read("capabilities", "profile")
            })
            .await
            .map_err(|_| IvyError::internal())?
        })
    }
    fn write(&self, value: IvyJson, previous: Option<Pin>) -> ProfileFuture<'_, SavedState> {
        let state = self.0.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                state.lock().map_err(|_| IvyError::internal())?.write(
                    "capabilities",
                    "profile",
                    &value,
                    previous.as_ref(),
                )
            })
            .await
            .map_err(|_| IvyError::internal())?
        })
    }
}

/// Startup supplies a synchronous notification closure which checks the current
/// service readiness before sending agent/capabilities at schema version 1.0.0.
pub type CapabilityPublisher = dyn Fn(&IvyJson) -> IvyResult<()> + Send + Sync;

struct Inner {
    service_node_id: JsString,
    host_id: JsString,
    store: Arc<dyn CapabilityStore>,
    saved: Mutex<SavedState>,
    defaults: IvyJson,
    publish: Arc<CapabilityPublisher>,
    clock: Arc<JournalClock>,
}
#[derive(Clone)]
pub struct CapabilityManagement(Arc<Inner>);

impl CapabilityManagement {
    pub async fn open(
        store: Arc<dyn CapabilityStore>,
        service_node_id: JsString,
        host_id: JsString,
        capabilities: Option<IvyJson>,
        defaults: IvyJson,
        publish: Arc<CapabilityPublisher>,
    ) -> IvyResult<Self> {
        Self::with_clock(
            store,
            service_node_id,
            host_id,
            capabilities,
            defaults,
            publish,
            Arc::new(|| {
                chrono::Utc::now()
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                    .into()
            }),
        )
        .await
    }

    pub async fn with_clock(
        store: Arc<dyn CapabilityStore>,
        service_node_id: JsString,
        host_id: JsString,
        capabilities: Option<IvyJson>,
        defaults: IvyJson,
        publish: Arc<CapabilityPublisher>,
        clock: Arc<JournalClock>,
    ) -> IvyResult<Self> {
        let saved = match store.read().await? {
            Some(saved) => saved,
            None => {
                store
                    .write(
                        IvyJson::object([
                            ("serviceNodeId", IvyJson::String(service_node_id.clone())),
                            ("hostId", IvyJson::String(host_id.clone())),
                            ("revision", IvyJson::Number(1.0)),
                            (
                                "capabilities",
                                capabilities
                                    .filter(|v| !v.is_null())
                                    .unwrap_or(IvyJson::Array(vec![])),
                            ),
                            ("updatedAt", IvyJson::String(clock())),
                            ("operationId", IvyJson::Null),
                        ]),
                        None,
                    )
                    .await?
            }
        };
        Ok(Self(Arc::new(Inner {
            service_node_id,
            host_id,
            store,
            saved: Mutex::new(saved),
            defaults,
            publish,
            clock,
        })))
    }

    pub fn profile(&self) -> IvyResult<IvyJson> {
        Ok(self
            .0
            .saved
            .lock()
            .map_err(|_| IvyError::internal())?
            .value
            .clone())
    }

    pub fn handlers(&self) -> HashMap<JsString, ToolHandler> {
        [
            "capabilities",
            "environmentDefaults",
            "configureCapabilities",
        ]
        .into_iter()
        .map(|name| {
            let owner = self.clone();
            let handler: ToolHandler = Arc::new(move |args, context| {
                let owner = owner.clone();
                Box::pin(async move {
                    match name {
                        "capabilities" => owner.profile(),
                        "environmentDefaults" => Ok(owner.0.defaults.clone()),
                        _ => owner.configure(args, context.operation_id).await,
                    }
                }) as ServiceFuture<IvyJson>
            });
            (format!("agent.{name}").into(), handler)
        })
        .collect()
    }

    async fn configure(
        &self,
        request: IvyJson,
        operation_id: Option<JsString>,
    ) -> IvyResult<IvyJson> {
        validate_agent("ConfigureCapabilitiesInput", &request)?;
        if request.get("operationId").and_then(IvyJson::as_js_string) != operation_id.as_ref() {
            return Err(IvyError::new(
                "mutation_conflict",
                "Tool and capability operation IDs must match.",
            ));
        }
        let capabilities = IvyJson::Array(
            request
                .required("capabilities")?
                .as_array()
                .unwrap()
                .iter()
                .map(|value| {
                    IvyJson::object([
                        ("key", value.get("key").unwrap().clone()),
                        (
                            "label",
                            IvyJson::String(
                                value.get("label").unwrap().as_js_string().unwrap().trim(),
                            ),
                        ),
                    ])
                })
                .collect(),
        );
        let mut keys = HashSet::new();
        for capability in capabilities.as_array().unwrap() {
            if !keys.insert(
                capability
                    .get("key")
                    .unwrap()
                    .as_js_string()
                    .unwrap()
                    .clone(),
            ) || capability
                .get("label")
                .unwrap()
                .as_js_string()
                .unwrap()
                .0
                .is_empty()
            {
                return Err(IvyError::new(
                    "invalid_arguments",
                    "Capability keys must be unique and labels nonempty.",
                ));
            }
        }
        // Snapshot the current pin before awaiting storage, as in the original.
        // Concurrent updates compete through SQLite rather than a held async lock.
        let saved = self
            .0
            .saved
            .lock()
            .map_err(|_| IvyError::internal())?
            .clone();
        if saved.value.is_null() {
            return Err(IvyError::internal());
        }
        if saved.value.get("operationId") == request.get("operationId") {
            if hash_json(saved.value.required("capabilities")?)? != hash_json(&capabilities)? {
                return Err(IvyError::new(
                    "mutation_conflict",
                    "Capability operation was already used for another profile.",
                ));
            }
            return Ok(saved.value);
        }
        if saved.value.get("revision") != request.get("expectedRevision") {
            return Err(IvyError::new(
                "revision_conflict",
                "Capability profile changed; reload its current revision.",
            ));
        }
        let next = IvyJson::object([
            (
                "serviceNodeId",
                IvyJson::String(self.0.service_node_id.clone()),
            ),
            ("hostId", IvyJson::String(self.0.host_id.clone())),
            (
                "revision",
                IvyJson::Number(request.required("expectedRevision")?.as_f64().unwrap() + 1.0),
            ),
            ("capabilities", capabilities),
            ("updatedAt", IvyJson::String((self.0.clock)())),
            ("operationId", request.required("operationId")?.clone()),
        ]);
        let saved = self.0.store.write(next.clone(), Some(saved.pin)).await?;
        *self.0.saved.lock().map_err(|_| IvyError::internal())? = saved;
        (self.0.publish)(&next)?;
        Ok(next)
    }
}
