//! Process entry and termination around the Collector lifetime.
use crate::{
    definition_journal::CompareNames,
    lifecycle::{self, StartOptions},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult};
use ivy_host_runtime::config::configuration_path;
use ivy_sdk::collation::LocaleRuntime;
use std::{io::Write, process::ExitCode, sync::Arc};

struct StopSignals {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
    #[cfg(windows)]
    interrupt: tokio::signal::windows::CtrlC,
    #[cfg(windows)]
    terminate: tokio::signal::windows::CtrlBreak,
}
impl StopSignals {
    fn new() -> IvyResult<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Ok(Self {
                interrupt: signal(SignalKind::interrupt()).map_err(|_| IvyError::internal())?,
                terminate: signal(SignalKind::terminate()).map_err(|_| IvyError::internal())?,
            })
        }
        #[cfg(windows)]
        {
            Ok(Self {
                interrupt: tokio::signal::windows::ctrl_c().map_err(|_| IvyError::internal())?,
                terminate: tokio::signal::windows::ctrl_break()
                    .map_err(|_| IvyError::internal())?,
            })
        }
    }
    async fn wait(&mut self) -> IvyResult<()> {
        let received = tokio::select! {
            value = self.interrupt.recv() => value,
            value = self.terminate.recv() => value,
        };
        received.ok_or_else(IvyError::internal)
    }
}

/// The executable must supply the qualified shared default-locale comparator.
/// Node and the Windows workload helper come from the native package layout.
pub async fn run(compare_names: CompareNames) -> IvyResult<()> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let path = configuration_path(&args, std::env::var_os("IVY_INSTANCE_CONFIG"))?;
    let mut signals = StopSignals::new()?;
    #[cfg(windows)]
    let job_launcher = Some(
        std::env::current_exe()
            .map_err(|_| IvyError::internal())?
            .parent()
            .ok_or_else(IvyError::internal)?
            .join("ivy-job.exe"),
    );
    #[cfg(not(windows))]
    let job_launcher = None;
    let collector = lifecycle::start(
        path,
        StartOptions {
            compare_names,
            node_executable: None,
            job_launcher,
        },
    )
    .await?;
    tokio::select! {
        result = collector.wait_closed() => result,
        result = signals.wait() => result.and(collector.close().await),
    }
}

/// Keep startup diagnostics bounded and free of configuration or secret values.
pub async fn main(compare_names: CompareNames) -> ExitCode {
    finish(run(compare_names).await)
}

/// The packaged service requires the qualified runtime beside its executable.
/// The captured comparator retains the loaded libraries for the service lifetime.
pub async fn packaged_main() -> ExitCode {
    let comparator =
        LocaleRuntime::from_current_executable().and_then(|runtime| runtime.executing_default());
    let result = match comparator {
        Ok(comparator) => run(Arc::new(move |left, right| comparator.compare(left, right))).await,
        Err(error) => Err(error),
    };
    finish(result)
}

fn finish(result: IvyResult<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let value = IvyJson::object([
                ("code", IvyJson::String(error.code_js())),
                ("message", "DataCollector failed to start.".into()),
            ]);
            let text =
                ivy_contracts::json::stringify(&value, 1024).expect("bounded startup diagnostic");
            let _ = writeln!(std::io::stderr().lock(), "{text}");
            ExitCode::FAILURE
        }
    }
}
