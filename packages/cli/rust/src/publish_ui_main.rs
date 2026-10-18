use ivy_contracts::{json::stringify, limits::MANAGEMENT_FRAME_BYTES};
#[tokio::main]
async fn main() {
    let args: Vec<_> = std::env::args_os()
        .skip(1)
        .map(|value| value.to_string_lossy().into_owned())
        .collect();
    match ivy_cli::publish_ui::command(&args, |key| std::env::var(key).ok()).await {
        Ok(value) => println!("{}", stringify(&value, usize::MAX).expect("JSON result")),
        Err(error) => {
            eprintln!(
                "{}",
                stringify(&error.to_wire(), MANAGEMENT_FRAME_BYTES).expect("wire error")
            );
            std::process::exit(1);
        }
    }
}
