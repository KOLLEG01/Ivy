mod native;

fn main() {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "--storage-worker")
    {
        ivy_hive::worker_entry::run(
            ivy_hive::native_runtime::startup_ports(),
            std::rc::Rc::new(ivy_hive::browser_push_native::NativeCrypto),
        );
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Hive runtime");
    let result = tokio::task::LocalSet::new().block_on(&runtime, native::run());
    if let Err(error) = result {
        eprintln!(
            "{}",
            ivy_contracts::json::stringify(
                &ivy_contracts::IvyJson::object([
                    ("code", error.code.clone().into()),
                    ("message", error.message.clone().into()),
                ]),
                65536
            )
            .unwrap_or_default()
        );
        std::process::exit(1);
    }
}
