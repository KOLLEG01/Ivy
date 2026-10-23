use ivy_chat_bridge::startup::ChatStartup;
use ivy_contracts::{IvyError, IvyResult};
use std::ffi::OsString;

fn main() {
    if std::env::args_os().skip(1).eq([std::ffi::OsString::from(
        ivy_chat_bridge::whatsapp::audio_worker::WORKER_ARGUMENT,
    )]) {
        std::process::exit(ivy_chat_bridge::whatsapp::audio_worker::worker_main());
    }
    service_main();
}

#[tokio::main]
async fn service_main() {
    if let Err(error) = run().await {
        eprintln!("ChatBridge failed: {}", error.code);
        std::process::exit(1);
    }
}

async fn run() -> IvyResult<()> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let startup = ChatStartup::load(&args, std::env::var_os("IVY_INSTANCE_CONFIG")).await?;
    let service = startup.start_native().await?;
    tokio::select! {
        result = service.wait() => result,
        result = shutdown_signal() => {
            let closed = service.close().await;
            result.and(closed)
        }
    }
}

async fn shutdown_signal() -> IvyResult<()> {
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
    tokio::signal::ctrl_c()
        .await
        .map_err(|_| IvyError::internal())
}
