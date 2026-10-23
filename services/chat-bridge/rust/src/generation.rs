//! A provider generation owns its connection, caches and SQLite handle. Accepted
//! callers retain read leases; retirement takes the handle only after they settle.
use crate::{
    admission::ChatAdmission, bridge::ChatBridge, chat_main::ChatMain, collector::ChatCollector,
    delivery::ChatDelivery, diagnostic_notifications::DiagnosticNotifications,
    native_driver::ChatNativeDriver, native_plan::resolve_chat_plan,
    notice_input::NoticeInstructions, operations::ChatOperations, store::ChatStore,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, checked_native_contract::CheckedNativeContracts,
};
use ivy_sdk::{
    clock::{Clock, SystemClock},
    rpc::{RequestOptions, RpcClient},
    service::{InvocationContext, ServiceConnection},
    tools::ToolClients,
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{OwnedRwLockReadGuard, RwLock, watch};

/// Distribution assets and immutable admission are shared across reconnects.
/// The host configuration adapter supplies the already resolved data directory.
pub struct ChatGenerationConfig {
    pub admission: ChatAdmission,
    pub catalogs: CheckedNativeContracts,
    pub instructions: JsString,
    pub notice_instructions: NoticeInstructions,
    pub service_node_id: JsString,
    pub data_root: Option<PathBuf>,
    pub language: JsString,
}

pub struct ChatGeneration {
    pub connection: ServiceConnection,
    config: Arc<ChatGenerationConfig>,
    tools: Arc<ToolClients>,
    store: Arc<RwLock<Option<ChatStore>>>,
    accepting: AtomicBool,
    retirement: Mutex<Option<watch::Receiver<Option<IvyResult<()>>>>>,
}

fn unavailable() -> IvyError {
    IvyError::new(
        "service_not_ready",
        "ChatBridge has no verified current Hive connection.",
    )
}

impl ChatGeneration {
    pub async fn reconcile(
        config: Arc<ChatGenerationConfig>,
        connection: ServiceConnection,
    ) -> IvyResult<Arc<Self>> {
        Self::with_clock(config, connection, Arc::new(SystemClock)).await
    }
    pub async fn with_clock(
        config: Arc<ChatGenerationConfig>,
        connection: ServiceConnection,
        clock: Arc<dyn Clock>,
    ) -> IvyResult<Arc<Self>> {
        let client: Arc<dyn RpcClient> = Arc::new(connection.clone());
        let tools = Arc::new(ToolClients::new(client.clone()));
        let root = config
            .admission
            .definition()
            .get("rootObjectId")
            .filter(|v| !v.is_null())
            .map(Json::as_str)
            .transpose()?;
        let store = ChatStore::with_clock(client, root, config.data_root.clone(), clock)
            .await
            .map_err(|e| e.into_ivy())?;
        let result: IvyResult<()> = async {
            let native = ChatNativeDriver::new(
                &store,
                tools.clone(),
                &config.admission,
                &config.catalogs,
                config.service_node_id.clone(),
                &config.instructions,
            )?;
            native.verify_owner().await?;
            let status = connection
                .request(
                    "system.status",
                    Json::object([] as [(&str, Json); 0]),
                    RequestOptions::default(),
                )
                .await?;
            store
                .set_runtime_epoch(
                    status
                        .required("runtimeEpoch")?
                        .as_js_string()
                        .cloned()
                        .ok_or_else(IvyError::internal)?,
                )
                .await
                .map_err(|e| e.into_ivy())?;
            if let Some(root) = &store.root_object_id {
                let root = connection
                    .request(
                        "objects.stat",
                        Json::object([("objectId", root.as_str().into())]),
                        RequestOptions::default(),
                    )
                    .await?;
                if root
                    .get("effectivelyArchived")
                    .and_then(Json::as_bool)
                    .unwrap_or(false)
                {
                    return Err(IvyError::new(
                        "chat_scope_mismatch",
                        "The configured ChatBridge root must be active.",
                    ));
                }
            }
            resolve_chat_plan(
                &store,
                &config.catalogs,
                config.admission.definition().required("nativePlan")?,
                &config.instructions,
            )?;
            let operations = ChatOperations::new(&store, &config.admission)?;
            ChatMain::new(&operations, &native, &config.notice_instructions)?
                .find()
                .await?;
            connection.throw_if_aborted()
        }
        .await;
        if let Err(error) = result {
            store.close().await.map_err(|e| e.into_ivy())?;
            return Err(error);
        }
        Ok(Arc::new(Self {
            connection,
            config,
            tools,
            store: Arc::new(RwLock::new(Some(store))),
            accepting: AtomicBool::new(true),
            retirement: Mutex::new(None),
        }))
    }

    pub fn generation(&self) -> u64 {
        self.connection.generation()
    }

    /// Acquire synchronously when the provider selects this generation, before
    /// its returned invocation future can be suspended or its socket can retire.
    pub fn accept(&self) -> IvyResult<ChatGenerationLease> {
        if !self.accepting.load(Ordering::Acquire) {
            return Err(unavailable());
        }
        let store = self
            .store
            .clone()
            .try_read_owned()
            .map_err(|_| unavailable())?;
        if store.is_none() || !self.accepting.load(Ordering::Acquire) {
            return Err(unavailable());
        }
        Ok(ChatGenerationLease {
            connection: self.connection.clone(),
            config: self.config.clone(),
            tools: self.tools.clone(),
            store,
        })
    }

    /// Stop admission once, then keep the close future alive even if an observer
    /// is dropped. Accepted native replies and blocking SQLite jobs are not killed.
    pub fn retire(&self) -> watch::Receiver<Option<IvyResult<()>>> {
        let mut retirement = self.retirement.lock().unwrap();
        if let Some(value) = &*retirement {
            return value.clone();
        }
        self.accepting.store(false, Ordering::Release);
        let store = self.store.clone();
        let (send, receive) = watch::channel(None);
        tokio::spawn(async move {
            let store = store.write().await.take();
            let result = match store {
                Some(store) => store.close().await.map_err(|e| e.into_ivy()),
                None => Ok(()),
            };
            send.send_replace(Some(result));
        });
        *retirement = Some(receive.clone());
        receive
    }
}

pub async fn settled(mut retirement: watch::Receiver<Option<IvyResult<()>>>) -> IvyResult<()> {
    loop {
        if let Some(result) = retirement.borrow_and_update().clone() {
            return result;
        }
        retirement
            .changed()
            .await
            .map_err(|_| IvyError::internal())?;
    }
}

pub struct ChatGenerationLease {
    pub connection: ServiceConnection,
    config: Arc<ChatGenerationConfig>,
    tools: Arc<ToolClients>,
    store: OwnedRwLockReadGuard<Option<ChatStore>>,
}
impl ChatGenerationLease {
    pub fn store(&self) -> &ChatStore {
        self.store.as_ref().unwrap()
    }
    pub fn config(&self) -> &ChatGenerationConfig {
        &self.config
    }
    pub fn native(&self) -> IvyResult<ChatNativeDriver<'_>> {
        ChatNativeDriver::new(
            self.store(),
            self.tools.clone(),
            &self.config.admission,
            &self.config.catalogs,
            self.config.service_node_id.clone(),
            &self.config.instructions,
        )
    }
    pub fn operations(&self) -> IvyResult<ChatOperations<'_>> {
        ChatOperations::new(self.store(), &self.config.admission)
    }
    pub async fn invoke(
        &self,
        name: &str,
        args: &Json,
        context: &InvocationContext,
    ) -> IvyResult<Json> {
        let native = self.native()?;
        let operations = self.operations()?;
        let main = ChatMain::new(&operations, &native, &self.config.notice_instructions)?;
        let delivery = ChatDelivery::new(&main);
        let evidence = native.evidence()?;
        let collector =
            ChatCollector::new(&operations, &evidence, &delivery, &self.config.catalogs);
        ChatBridge::new(&delivery, &collector, self.connection.generation())?
            .invoke(name, args, context)
            .await
    }
    pub async fn verify_owner(&self) -> IvyResult<()> {
        self.native()?.verify_owner().await
    }
    pub async fn monitor_diagnostics(&self) -> IvyResult<()> {
        self.monitor_diagnostics_with_clock(Arc::new(SystemClock))
            .await
    }
    pub async fn monitor_diagnostics_with_clock(&self, clock: Arc<dyn Clock>) -> IvyResult<()> {
        let native = self.native()?;
        let operations = self.operations()?;
        let main = ChatMain::new(&operations, &native, &self.config.notice_instructions)?;
        DiagnosticNotifications::with_clock(&operations, &main, self.config.language.clone(), clock)
            .tick(RequestOptions {
                cancellation: self.connection.signal(),
                ..Default::default()
            })
            .await
    }
}
