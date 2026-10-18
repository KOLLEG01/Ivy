use ivy_contracts::{IvyError, IvyJson, IvyResult};
use ivy_host_runtime::config::configuration_path;
use std::process::ExitCode;

async fn signal() -> IvyResult<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .map_err(|_| IvyError::internal())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result.map_err(|_| IvyError::internal()),
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .map_err(|_| IvyError::internal())
    }
}
async fn run() -> IvyResult<()> {
    let path = configuration_path(
        &std::env::args_os().skip(1).collect::<Vec<_>>(),
        std::env::var_os("IVY_INSTANCE_CONFIG"),
    )?;
    let running = ivy_secretary::service::start(path).await?;
    tokio::select! {
        result = running.closed() => result,
        result = signal() => { let closed = running.close().await; result.and(closed) },
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match tokio::task::LocalSet::new().run_until(run()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let value = IvyJson::object([
                ("code", IvyJson::String(error.code_js())),
                ("message", "Secretary failed to start.".into()),
            ]);
            if let Ok(line) = ivy_contracts::json::canonical(&value, usize::MAX) {
                eprintln!("{line}");
            }
            ExitCode::FAILURE
        }
    }
}
