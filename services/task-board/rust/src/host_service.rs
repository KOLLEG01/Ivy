//! Bind an admitted Host instance to the service before its generation starts.
//! The caller supplies the exact registry and current contract implementation.
use crate::{
    journal_store::Contracts,
    service::{ServiceOwner, ServiceSetup, TaskBoardService, service},
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
use ivy_host_runtime::{
    config::{InstanceConfig, instance_config},
    health::HealthFile,
    node_runtime::NodeRuntime,
};
use ivy_sdk::service::Dependency;
use std::{path::PathBuf, sync::Arc};

pub struct PreparedTaskBoard {
    pub config: InstanceConfig,
    pub health: Arc<HealthFile>,
    pub service: TaskBoardService,
    pub owner: ServiceOwner,
}

pub async fn prepare<P: Contracts>(
    path: impl Into<PathBuf>,
    node: &NodeRuntime,
    contracts: &P,
    registry: Dependency<Json>,
    tools: Vec<JsString>,
) -> IvyResult<PreparedTaskBoard> {
    let config = instance_config(path, node).await?;
    let value = config.json();
    let text = |key: &str| -> IvyResult<JsString> {
        value
            .required(key)?
            .as_js_string()
            .cloned()
            .ok_or_else(IvyError::internal)
    };
    // Keep Settings admission before the component/credential guard, as in the
    // original entrypoint. No registry discovery or Hive connection starts here.
    let (service, owner) = service(
        ServiceSetup {
            component_id: text("componentId")?,
            public_base_url: text("publicBaseUrl")?.to_utf8()?,
            credential: value
                .get("credential")
                .map(Json::as_str)
                .transpose()?
                .unwrap_or_default(),
            service_node_id: text("serviceNodeId")?,
            host_id: Some(text("hostId")?),
            version: text("version")?,
            build_id: text("buildId")?,
            settings: value.required("settings")?.clone(),
            registry,
            tools,
        },
        contracts,
    )?;
    let health = Arc::new(HealthFile::new(config.clone()));
    Ok(PreparedTaskBoard {
        config,
        health,
        service,
        owner,
    })
}
