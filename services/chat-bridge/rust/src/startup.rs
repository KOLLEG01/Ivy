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
    config::{InstanceConfig, configuration_path, native_instance_config},
    health::HealthFile,
};
use std::{ffi::OsString, sync::Arc};

/// An admitted instance, shared by the account owner and provider startup.
/// Credentials remain in the Host configuration and are never formatted here.
pub struct ChatStartup {
    config: InstanceConfig,
    generation: Arc<ChatGenerationConfig>,
    health: Arc<HealthFile>,
}

impl ChatStartup {
    /// Load the existing instance format and distribution assets without Node.
    pub async fn load(args: &[OsString], configuration: Option<OsString>) -> IvyResult<Self> {
        let path = configuration_path(args, configuration)?;
        Self::from_config(native_instance_config(path).await?).await
    }

    /// Hosts that already admitted the instance can use the same startup path.
    pub async fn from_config(config: InstanceConfig) -> IvyResult<Self> {
        let owner = config.clone();
        let generation = tokio::task::spawn_blocking(move || -> IvyResult<_> {
            let source = owner.json();
            let settings = service_settings(source)?;
            let distribution = owner.artifact_root().join("dist");
            Ok(Arc::new(ChatGenerationConfig {
                admission: ChatAdmission::new(settings.required("definition")?)?,
                catalogs: CheckedNativeContracts::new(&distribution),
                instructions: load_main_instructions(&distribution)?,
                notice_instructions: NoticeInstructions::load(&distribution)?,
                service_node_id: source
                    .required("serviceNodeId")?
                    .as_js_string()
                    .cloned()
                    .ok_or_else(IvyError::internal)?,
                data_root: Some(owner.data_root().to_path_buf()),
                diagnostic_notifications: settings
                    .get("diagnosticNotifications")
                    .and_then(Json::as_bool)
                    == Some(true),
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
            generation,
            health,
        })
    }

    pub fn config(&self) -> &InstanceConfig {
        &self.config
    }

    /// Assemble the native account/media owners from the same admitted instance.
    pub async fn start_native(self) -> IvyResult<ChatService> {
        let settings = service_settings(self.config.json())?;
        let Some(path) = settings.get("whatsappConfigPath").filter(|p| !p.is_null()) else {
            return self.start(None).await;
        };
        let path = path.as_js_string().ok_or_else(IvyError::internal)?;
        let config =
            crate::whatsapp::config::read_whatsapp_config(path, settings.required("definition")?)
                .await?;
        let parts = crate::whatsapp::service_parts::open(
            config,
            self.config.data_root().to_owned(),
            self.config.artifact_root().to_owned(),
            std::env::current_exe().map_err(|_| IvyError::internal())?,
            crate::whatsapp::messages::Translator::new(Some(&self.generation.language)),
        )
        .await?;
        let socket = parts.socket.clone();
        let journal = parts.journal.clone();
        match self.start(Some(parts)).await {
            Ok(service) => Ok(service),
            Err(error) => {
                let _ = socket.close().await;
                let _ = journal.close().await;
                Err(error)
            }
        }
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
