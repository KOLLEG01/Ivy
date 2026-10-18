use ivy_contracts::{IvyError, IvyJson, IvyResult, json::stringify};
use ivy_host_runtime::{config::configuration_path, node_runtime::NodeRuntime};
use ivy_service_manager::manager::Manager;

async fn start() -> IvyResult<Manager> {
    let path = configuration_path(
        &std::env::args_os().skip(1).collect::<Vec<_>>(),
        std::env::var_os("IVY_INSTANCE_CONFIG"),
    )?;
    let node = NodeRuntime::discover(None).await?;
    Manager::from_path(path, node).await
}
async fn run(manager: Manager) -> IvyResult<()> {
    let result = tokio::select! {
        result = manager.completion() => return result,
        result = signal() => result,
    };
    // A signal-listener error still drains every owned resource.
    let closed = manager.close().await;
    result.and(closed)
}
async fn signal() -> IvyResult<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut interrupt = signal(SignalKind::interrupt()).map_err(|_| IvyError::internal())?;
        let mut terminate = signal(SignalKind::terminate()).map_err(|_| IvyError::internal())?;
        tokio::select! { _ = interrupt.recv() => (), _ = terminate.recv() => () }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .map_err(|_| IvyError::internal())
    }
}
#[tokio::main]
async fn main() {
    match start().await {
        Ok(manager) => {
            if run(manager).await.is_err() {
                std::process::exit(1);
            }
        }
        Err(error) => {
            let message = IvyJson::object([
                ("code", error.code.into()),
                ("message", "ServiceManager failed to start.".into()),
            ]);
            eprintln!("{}", stringify(&message, 1024).unwrap_or_else(|_| "{\"code\":\"internal_error\",\"message\":\"ServiceManager failed to start.\"}".into()));
            std::process::exit(1);
        }
    }
}
