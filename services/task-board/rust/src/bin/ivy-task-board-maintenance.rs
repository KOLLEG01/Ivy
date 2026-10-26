use ivy_contracts::{IvyJson as Json, json::stringify};
fn main() -> std::process::ExitCode {
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| ivy_contracts::IvyError::internal())
        .and_then(|runtime| {
            tokio::task::LocalSet::new().block_on(
                &runtime,
                ivy_task_board::maintenance::operator::command(std::env::args().skip(1).collect()),
            )
        });
    match result {
        Ok(value) => {
            println!("{}", stringify(&value, 4096).unwrap());
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{}",stringify(&Json::object([("code",error.code.into()),("message","TaskBoard maintenance could not complete; retain the original plan and job before retrying.".into())]),4096).unwrap());
            std::process::ExitCode::FAILURE
        }
    }
}
