mod native;

fn main() {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "settings-recovery")
    {
        let result =
            ivy_hive::settings_recovery::command(&std::env::args_os().skip(2).collect::<Vec<_>>());
        let (value, code) = match result {
            Ok(value) => (ivy_contracts::IvyJson::object([("ok", true.into()), ("result", value)]), 0),
            Err(error) => (ivy_contracts::IvyJson::object([("ok", false.into()), ("code", error.code.into()),
                ("message", "Settings recovery failed without changing any Object content outside its allowlist.".into())]), 1),
        };
        let value = ivy_contracts::json::stringify(&value, 65536).expect("bounded settings result");
        if code == 0 {
            println!("{value}");
        } else {
            eprintln!("{value}");
        }
        std::process::exit(code);
    }
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
    let result = tokio::task::LocalSet::new().block_on(
        &runtime,
        native::run(|| {
            let client = ivy_hive::browser_push_https::NativeHttps::new()?;
            let sender = ivy_hive::browser_push_native::NativeSender::new(
                std::sync::Arc::new(client),
                ivy_hive::browser_push_native::system_expiration(),
            );
            Ok(std::sync::Arc::new(sender))
        }),
    );
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
