use ivy_cli::output::CliResult;
use ivy_contracts::{IvyError, json::stringify};
use ivy_host_runtime::native_path;
use std::{
    io::{self, Write},
    process::ExitCode,
    sync::Arc,
};

#[tokio::main]
async fn main() -> ExitCode {
    let args = std::env::args_os()
        .skip(1)
        .map(|value| native_path::filesystem_name(&value))
        .collect::<Vec<_>>();
    let progress = Arc::new(|message: &ivy_contracts::JsString| {
        let _ = writeln!(
            io::stderr().lock(),
            "{}",
            String::from_utf16_lossy(&message.0)
        );
    });
    let result = ivy_host_executor::cli::command(&args, Some(progress)).await;
    let json = stringify(&result.output, usize::MAX).unwrap_or_else(|_| {
        stringify(
            &CliResult::failure(&IvyError::internal(), None).output,
            usize::MAX,
        )
        .unwrap()
    });
    if writeln!(io::stdout().lock(), "{json}").is_err() {
        return ExitCode::FAILURE;
    }
    ExitCode::from(result.exit_code)
}
