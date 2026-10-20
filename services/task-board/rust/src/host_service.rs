//! Bind an admitted Host instance to the service before its generation starts.
//! The caller supplies the exact registry and current contract implementation.
use crate::{
    journal_store::Contracts,
    service::{ServiceOwner, ServiceSetup, TaskBoardService, admit_service, admitted_service},
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
    let settings = value.required("settings")?.clone();
    let component_id = text("componentId")?;
    let credential = value
        .get("credential")
        .and_then(Json::as_js_string)
        .cloned()
        .unwrap_or_default();
    // Admit the original JSON before any transport conversion. The same check
    // serves direct ServiceSetup callers and runs exactly once on either path.
    admit_service(
        &settings,
        &component_id,
        !credential.0.is_empty(),
        contracts,
    )?;
    let (service, owner) = admitted_service(ServiceSetup {
        component_id,
        // URL takes a USVString in the original constructor, unlike JSON
        // identities and credentials. Keep the retained config untouched.
        public_base_url: String::from_utf16_lossy(&text("publicBaseUrl")?.0),
        credential,
        service_node_id: text("serviceNodeId")?,
        host_id: Some(text("hostId")?),
        version: text("version")?,
        build_id: text("buildId")?,
        settings,
        registry,
        tools,
    })?;
    let health = Arc::new(HealthFile::new(config.clone()));
    Ok(PreparedTaskBoard {
        config,
        health,
        service,
        owner,
    })
}
