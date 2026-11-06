//! Original configuration and reset entry, without runtime migration fallbacks.
use crate::service_kernel::{
    AgentRuntime, AgentRuntimeBindings, ValidatedAgentConfig, start_validated_agent,
};
use ivy_contracts::{IvyError, IvyResult};
use ivy_host_runtime::{
    config::{instance_config, native_instance_config},
    node_runtime::NodeRuntime,
};
use std::path::PathBuf;
use tokio::sync::oneshot;

/// Start the real service with native host inputs and the shared Hive client.
pub async fn start_native_agent_manager(path: PathBuf) -> IvyResult<AgentRuntime> {
    let config = ValidatedAgentConfig::new(native_instance_config(path).await?)?;
    let bindings = crate::runtime::native_bindings(config.config())?;
    start_configured(config, bindings).await
}

async fn start_configured(
    config: ValidatedAgentConfig,
    bindings: AgentRuntimeBindings,
) -> IvyResult<AgentRuntime> {
    config.validate_homes(&bindings.home)?;
    start_validated_agent(config, bindings).await
}

/// The dispatcher/host supplies its already discovered Node workload runtime.
/// Instance/build/settings and cross-account home checks precede all backends.
/// Each explicit reset finishes before that backend's SQLite or launch. One-time
/// skills/MCP imports are independent operator actions, never startup recovery.
pub fn start_agent_manager(
    path: PathBuf,
    node: NodeRuntime,
    bindings: AgentRuntimeBindings,
) -> impl std::future::Future<Output = IvyResult<AgentRuntime>> + Send {
    let (sender, receiver) = oneshot::channel();
    tokio::spawn(async move {
        let result = async {
            let config = ValidatedAgentConfig::new(instance_config(path, &node).await?)?;
            start_configured(config, bindings).await
        }
        .await;
        if let Err(Ok(runtime)) = sender.send(result) {
            if let Ok(run) = runtime.close() {
                let _ = run.wait().await;
            }
        }
    });
    async move { receiver.await.map_err(|_| IvyError::internal())? }
}
