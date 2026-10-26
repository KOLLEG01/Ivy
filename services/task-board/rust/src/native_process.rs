//! The process owns configuration and packaged catalogs until final drain.
use crate::{native_runtime::NativeRuntime, prompts::CurrentTaskPrompts, runtime_inputs};
use ivy_contracts::{IvyError, IvyResult};
use ivy_host_runtime::config::native_instance_config;
use ivy_sdk::collation::LocaleComparator;
use std::path::PathBuf;

pub async fn prepare(path: impl Into<PathBuf>) -> IvyResult<NativeRuntime<CurrentTaskPrompts>> {
    let config = native_instance_config(path).await?;
    let locale = LocaleComparator::system()?;
    let inputs = runtime_inputs::packaged(config.artifact_root().join("dist"), locale)?;
    NativeRuntime::from_config(config, inputs)
}

async fn shutdown() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}

pub async fn run(path: impl Into<PathBuf>) -> IvyResult<()> {
    let runtime = prepare(path).await?;
    let mut signal = Ok(());
    let result = runtime
        .run(async {
            signal = shutdown().await.map_err(|_| IvyError::internal());
        })
        .await;
    result.and(signal)
}
