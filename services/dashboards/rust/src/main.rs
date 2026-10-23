use ivy_contracts::IvyResult;
use ivy_host_runtime::config::{configuration_path, native_instance_config};

async fn run() -> IvyResult<()> {
    let path = configuration_path(
        &std::env::args_os().skip(1).collect::<Vec<_>>(),
        std::env::var_os("IVY_INSTANCE_CONFIG"),
    )?;
    let config = native_instance_config(path).await?;
    let runtime = ivy_dashboards::runtime::start(config).await?;
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .map_err(|_| ivy_contracts::IvyError::internal())?;
        tokio::select! { _=runtime.stopped()=>{}, _=tokio::signal::ctrl_c()=>{}, _=terminate.recv()=>{} }
    }
    #[cfg(not(unix))]
    tokio::select! { _=runtime.stopped()=>{}, _=tokio::signal::ctrl_c()=>{} }
    runtime.close().await
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{}", error.message);
        std::process::exit(1);
    }
}
