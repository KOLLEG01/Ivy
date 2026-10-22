use ivy_contracts::{IvyJson, json};
use ivy_host_executor::{entry, install_signals};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{}", entry::USAGE);
        return;
    }
    let stop = CancellationToken::new();
    let result = match install_signals(stop.clone()) {
        Ok(signal) => {
            let result = async {
                let options = entry::Options::parse(&args, std::env::var_os("IVY_HOST_CONFIG"))?;
                entry::run(options, stop.clone()).await
            }
            .await;
            stop.cancel();
            let _ = signal.await;
            result
        }
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        let message = IvyJson::object([
            ("code", error.code.into()),
            ("message", error.message.into()),
        ]);
        eprintln!(
            "{}",
            json::stringify(&message, 8192)
                .unwrap_or_else(|_| "{\"code\":\"internal_error\"}".into())
        );
        std::process::exit(1);
    }
}
