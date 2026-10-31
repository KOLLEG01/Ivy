#![cfg_attr(windows, windows_subsystem = "windows")]
#[tokio::main]
async fn main() -> std::process::ExitCode {
    match ivy_codex_server::run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!(
                "{{\"code\":\"{}\",\"message\":\"Shared server startup failed.\"}}",
                error.code
            );
            std::process::ExitCode::FAILURE
        }
    }
}
