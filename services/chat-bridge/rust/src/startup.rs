//! Host configuration and distribution assets for the existing service lifecycle.
use crate::{
    admission::ChatAdmission,
    generation::ChatGenerationConfig,
    main_context::load_main_instructions,
    notice_input::NoticeInstructions,
    runtime::{ChatHealth, ChatService, WhatsAppServiceParts, service_settings},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, checked_native_contract::CheckedNativeContracts,
};
use ivy_host_runtime::{
    config::{InstanceConfig, configuration_path, instance_config},
    health::HealthFile,
    node_runtime::NodeRuntime,
};
use std::{ffi::OsString, path::Path, sync::Arc};

/// An admitted instance, shared by the account owner and provider startup.
/// Credentials remain in the Host configuration and are never formatted here.
pub struct ChatStartup {
    config: InstanceConfig,
    node: NodeRuntime,
    generation: Arc<ChatGenerationConfig>,
    health: Arc<HealthFile>,
}

impl ChatStartup {
    /// The v1 dispatcher supplies its own absolute Node executable. Configuration
    /// selection and launch identity retain the shared Host behavior.
    pub async fn load(
        args: &[OsString],
        configuration: Option<OsString>,
        node_executable: &Path,
    ) -> IvyResult<Self> {
        let path = configuration_path(args, configuration)?;
        let node = NodeRuntime::discover(Some(node_executable)).await?;
        let config = instance_config(path, &node).await?;
        let owner = config.clone();
        let generation = tokio::task::spawn_blocking(move || -> IvyResult<_> {
            let source = owner.json();
            let settings = service_settings(source)?;
            Ok(Arc::new(ChatGenerationConfig {
                admission: ChatAdmission::new(settings.required("definition")?)?,
                catalogs: CheckedNativeContracts::new(owner.artifact_root()),
                instructions: load_main_instructions(owner.artifact_root())?,
                notice_instructions: NoticeInstructions::load(owner.artifact_root())?,
                service_node_id: source
                    .required("serviceNodeId")?
                    .as_js_string()
                    .cloned()
                    .ok_or_else(IvyError::internal)?,
                data_root: Some(owner.data_root().to_path_buf()),
                language: settings
                    .get("language")
                    .filter(|value| !value.is_null())
                    .unwrap_or(&Json::from("en"))
                    .as_js_string()
                    .cloned()
                    .ok_or_else(IvyError::internal)?,
            }))
        })
        .await
        .map_err(|_| IvyError::internal())??;
        let health = Arc::new(HealthFile::new(config.clone()));
        Ok(Self {
            config,
            node,
            generation,
            health,
        })
    }

    pub fn config(&self) -> &InstanceConfig {
        &self.config
    }

    pub fn node_runtime(&self) -> &NodeRuntime {
        &self.node
    }

    /// Account resources remain explicit until their production owners open them.
    /// Both branches use the same admitted configuration and health identity.
    pub async fn start(self, whatsapp: Option<WhatsAppServiceParts>) -> IvyResult<ChatService> {
        let writes = self.health.clone();
        let control = self.health;
        let health = ChatHealth {
            write: Arc::new(move |value| {
                let health = writes.clone();
                Box::pin(async move {
                    health
                        .write(value.ready, value.details, value.generation)
                        .await
                })
            }),
            control: Arc::new(move || {
                let health = control.clone();
                Box::pin(async move { Ok(health.control().await?.is_some()) })
            }),
        };
        match whatsapp {
            Some(parts) => {
                ChatService::start_with_whatsapp(self.config.json(), self.generation, health, parts)
                    .await
            }
            None => ChatService::start(self.config.json(), self.generation, health, None).await,
        }
    }
}
