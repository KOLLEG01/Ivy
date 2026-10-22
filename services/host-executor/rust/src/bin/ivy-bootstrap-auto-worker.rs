use ivy_contracts::{IvyJson, json};

#[tokio::main]
async fn main() {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if let Err(error) = ivy_host_executor::worker::run(&args).await {
        let value = IvyJson::object([
            ("code", IvyJson::String(error.code_js())),
            (
                "message",
                "Automatic bootstrap update needs attention.".into(),
            ),
        ]);
        eprintln!(
            "{}",
            json::stringify(&value, 1024).expect("bounded worker error")
        );
        std::process::exit(1);
    }
}
