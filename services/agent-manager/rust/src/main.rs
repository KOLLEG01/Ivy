use ivy_agent_manager::startup::start_native_agent_manager;
use ivy_contracts::{IvyError, IvyJson, IvyResult, json::stringify};
use ivy_host_runtime::{
    config::{configuration_path, native_instance_config},
    health::check_health,
};
use std::ffi::OsString;

async fn stop_signal() -> IvyResult<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .map_err(|_| IvyError::internal())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result.map_err(|_| IvyError::internal()),
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(windows)]
    {
        let mut interrupt = tokio::signal::windows::ctrl_c().map_err(|_| IvyError::internal())?;
        let mut terminate =
            tokio::signal::windows::ctrl_break().map_err(|_| IvyError::internal())?;
        tokio::select! { _ = interrupt.recv() => Ok(()), _ = terminate.recv() => Ok(()) }
    }
}

async fn run(args: &[OsString]) -> IvyResult<Option<IvyJson>> {
    if args.first().is_some_and(|arg| arg == "--claude-stdio") {
        if args.get(1).is_some_and(|arg| arg == "--help") {
            println!("claude-codex-adapter app-server (Rust owner)");
            return Ok(None);
        }
        ivy_agent_manager::claude::stdio::run().await.map_err(|_| {
            IvyError::new("claude_runtime_failed", "Claude native runtime stopped.")
        })?;
        return Ok(None);
    }
    let path = configuration_path(args, std::env::var_os("IVY_INSTANCE_CONFIG"))?;
    if args.iter().any(|arg| arg == "--health") {
        let config = native_instance_config(path).await?;
        let health = check_health(&config, None, true).await?;
        return Ok(Some(IvyJson::object([
            ("ok", true.into()),
            ("buildId", health.required("buildId")?.clone()),
            ("generation", health.required("generation")?.clone()),
        ])));
    }
    let manager = start_native_agent_manager(path).await?;
    let signal = stop_signal();
    tokio::pin!(signal);
    let outcome = tokio::select! {
        code = manager.closed() => code.map(Some),
        result = &mut signal => result.map(|()| None),
    };
    let cleanup = manager.close()?.wait().await;
    let code = outcome?;
    cleanup?;
    match code {
        Some(code) if code != "agent_stopped".into() => Err(IvyError::new_js(
            code,
            "AgentManager stopped after a runtime failure.".into(),
        )),
        _ => Ok(None),
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let outcome = run(&args).await;
    if let Ok(Some(value)) = &outcome {
        println!("{}", stringify(value, 1024).expect("bounded health result"));
    }
    if let Err(error) = outcome {
        if args.iter().any(|arg| arg == "--health") {
            println!(
                "{}",
                stringify(
                    &IvyJson::object([
                        ("ok", false.into()),
                        ("code", IvyJson::String(error.code_js())),
                    ]),
                    1024
                )
                .expect("bounded health failure")
            );
            std::process::exit(1);
        }
        // Native/provider errors may carry request details. Keep credentials
        // and user payloads out of the process diagnostic stream.
        let event = IvyJson::object([
            ("at", chrono::Utc::now().to_rfc3339().into()),
            ("code", IvyJson::String(error.code_js())),
            (
                "message",
                "AgentManager failed to start or stopped after a runtime failure.".into(),
            ),
        ]);
        eprintln!(
            "{}",
            stringify(&event, 16 * 1024).unwrap_or_else(|_| "{\"code\":\"internal_error\"}".into())
        );
        std::process::exit(1);
    }
}
