use ivy_contracts::{IvyJson as Json, IvyResult, json::stringify};
use ivy_host_runtime::config::configuration_path;
use std::process::ExitCode;

async fn run() -> IvyResult<()> {
    let path = configuration_path(
        &std::env::args_os().skip(1).collect::<Vec<_>>(),
        std::env::var_os("IVY_INSTANCE_CONFIG"),
    )?;
    ivy_task_board::native_process::run(path).await
}

fn main() -> ExitCode {
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| ivy_contracts::IvyError::internal())
        .and_then(|runtime| tokio::task::LocalSet::new().block_on(&runtime, run()));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!(
                "{}",
                stringify(
                    &Json::object([
                        ("code", error.code.into()),
                        ("message", "TaskBoard failed to start.".into()),
                    ]),
                    1024,
                )
                .expect("Startup diagnostic")
            );
            ExitCode::FAILURE
        }
    }
}
