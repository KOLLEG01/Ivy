//! Original configuration and reset entry, without runtime migration fallbacks.
use crate::{
    runtime_reset::apply_agent_runtime_reset,
    service_kernel::{
        AgentRuntime, AgentRuntimeBindings, start_prepared_agent, validate_agent_config,
    },
};
use ivy_contracts::{IvyError, IvyResult};
use ivy_host_runtime::{config::instance_config, node_runtime::NodeRuntime};
use std::path::PathBuf;
use tokio::sync::oneshot;

/// The dispatcher/host supplies its already discovered Node workload runtime.
/// All original instance/build/settings checks and the explicit reset finish
/// before SQLite, environment bootstrap or native launch can begin. One-time
/// skills/MCP imports are independent operator actions, never startup recovery.
pub fn start_agent_manager(
    path: PathBuf,
    node: NodeRuntime,
    bindings: AgentRuntimeBindings,
) -> impl std::future::Future<Output = IvyResult<AgentRuntime>> + Send {
    let (sender, receiver) = oneshot::channel();
    tokio::spawn(async move {
        let result = async {
            let config = instance_config(path, &node).await?;
            validate_agent_config(&config)?;
            apply_agent_runtime_reset(
                config.data_root().to_owned(),
                config.json().required("settings")?.clone(),
                bindings.home.clone(),
            )
            .await?;
            start_prepared_agent(config, bindings).await
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
