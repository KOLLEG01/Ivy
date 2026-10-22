use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    ivy_data_collector::entrypoint::main().await
}
