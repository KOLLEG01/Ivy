//! Bind an admitted Host instance to the service before its generation starts.
//! The caller supplies the exact registry and current contract implementation.
use crate::{
    journal_store::Contracts,
    service::{
        GenerationFactory, ServiceOwner, ServiceSetup, TaskBoardService, admit_service,
        admitted_service,
    },
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
use ivy_host_runtime::{
    config::{InstanceConfig, native_instance_config},
    health::HealthFile,
};
use ivy_sdk::service::Dependency;
use std::{future::Future, path::PathBuf, sync::Arc, time::Duration};

pub struct PreparedTaskBoard {
    pub config: InstanceConfig,
    pub health: Arc<HealthFile>,
    pub service: TaskBoardService,
    pub owner: ServiceOwner,
}

impl PreparedTaskBoard {
    /// Drive the admitted engine on its owning thread until a host control or
    /// process signal requests shutdown. The factory and journal stay alive
    /// while the SDK and generation finish their accepted work.
    pub async fn run<F: GenerationFactory>(
        self,
        factory: &F,
        shutdown: impl Future<Output = ()>,
    ) -> IvyResult<()> {
        let Self {
            health,
            service,
            owner,
            ..
        } = self;
        health
            .write(false, "Connecting TaskBoard to Hive.".into(), None)
            .await?;
        let generation = owner.run(factory);
        let monitor = async {
            let mut control = tokio::time::interval(Duration::from_millis(500));
            let mut heartbeat = tokio::time::interval(Duration::from_secs(2));
            tokio::pin!(shutdown);
            loop {
                tokio::select! {
                    _ = &mut shutdown => return Ok(()),
                    _ = control.tick() => {
                        if health.control().await.ok().flatten().is_some() {
                            return Ok(());
                        }
                    }
                    _ = heartbeat.tick() => {
                        let current = service.client.connection().ok();
                        health.write(
                            current.is_some(),
                            if current.is_some() {
                                "Workflow storage and the current service connection are available."
                            } else {
                                "Hive workflow connection is not ready."
                            }.into(),
                            current.as_ref().map(|connection| connection.generation()),
                        ).await?;
                    }
                }
            }
        };
        tokio::pin!(generation, monitor);
        service.start();
        let result = tokio::select! {
            result = &mut generation => {
                let closed = service.close().await;
                result.and(closed)
            }
            result = &mut monitor => {
                let (closed, finished) = tokio::join!(service.close(), &mut generation);
                result.and(closed).and(finished)
            }
        };
        let stopped = health.write(false, "TaskBoard stopped.".into(), None).await;
        result.and(stopped)
    }
}

pub async fn prepare<P: Contracts>(
    path: impl Into<PathBuf>,
    contracts: &P,
    registry: Dependency<Json>,
    tools: Vec<JsString>,
) -> IvyResult<PreparedTaskBoard> {
    let config = native_instance_config(path).await?;
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
