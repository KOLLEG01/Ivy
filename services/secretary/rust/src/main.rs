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
async fn run() -> IvyResult<bool> {
    let mut arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    if arguments
        .first()
        .is_some_and(|value| value == "--document-worker")
    {
        ivy_secretary::document_worker::run(&arguments[1..])?;
        return Ok(true);
    }
    let importing = arguments.first().is_some_and(|v| v == "import");
    let operation =
        if importing {
            arguments.remove(0);
            let at = arguments
                .iter()
                .position(|v| v == "--operation-id")
                .ok_or_else(|| {
                    IvyError::new(
                        "invalid_arguments",
                        "Use import --operation-id ORIGINAL_ID with the instance configuration.",
                    )
                })?;
            if at + 1 >= arguments.len() {
                return Err(IvyError::new(
                    "invalid_arguments",
                    "The import operation ID is missing.",
                ));
            }
            let value = arguments.remove(at + 1);
            arguments.remove(at);
            Some(value.into_string().map_err(|_| {
                IvyError::new("invalid_arguments", "Use a valid import operation ID.")
            })?)
        } else {
            None
        };
    let path = configuration_path(&arguments, std::env::var_os("IVY_INSTANCE_CONFIG"))?;
    if let Some(operation) = operation {
        let mut cancellation = ivy_sdk::rpc::RequestOptions::default();
        cancellation.cancel_error = IvyError::new(
            "cancelled",
            "Import cancellation retains original receipts for explicit resumption.",
        );
        let work = ivy_secretary::import_command::run(path, operation.into(), cancellation.clone());
        tokio::pin!(work);
        let value = tokio::select! {result=&mut work=>result?,result=signal()=>{cancellation.cancellation.cancel();result?;work.await?}};
        println!(
            "{}",
            ivy_contracts::json::canonical(&value, 16 * 1024 * 1024)?
        );
        return Ok(value.get("complete") == Some(&IvyJson::Bool(true)));
    }
    let running = ivy_secretary::service::start(path).await?;
    tokio::select! {
        result = running.closed() => result,
        result = signal() => { let closed = running.close().await; result.and(closed) },
    }?;
    Ok(true)
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match tokio::task::LocalSet::new().run_until(run()).await {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(2),
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
