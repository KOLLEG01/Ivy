//! Installed-client admission and the original bounded root/pipe command policy.
use super::{
    MicroClientFileError, MicroClientFiles, MicroClientPins, MicroClientResult, MicroCommand,
    MicroCommandError, read_micro_console_output,
};
use crate::native_config::NativeMicroSettings;
use std::{
    future::Future,
    io,
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    sync::Arc,
    task::{Context, Poll, Waker},
    time::Duration,
};
use tokio::io::AsyncRead;
use tokio_util::sync::CancellationToken;

type Execute = dyn Fn(Vec<String>, CancellationToken) -> MicroCommand + Send + Sync;
#[derive(Clone)]
pub struct MicroUsbClient {
    settings: NativeMicroSettings,
    execute: Arc<Execute>,
}
impl MicroUsbClient {
    /// Use the installed direct executable with the original root lifetime.
    /// Release pins and sibling read leases are still admitted by `run`.
    pub fn new(settings: NativeMicroSettings) -> Self {
        let executable = settings.executable().to_owned();
        Self::with_executor(settings, move |arguments, stop| {
            let executable = executable.clone();
            Box::pin(async move { launch(&executable, arguments, &stop).await })
        })
    }
    /// The executor starts one command after the installed file set is leased.
    /// It must report a started failure as unknown, never as safe to replay.
    pub fn with_executor(
        settings: NativeMicroSettings,
        execute: impl Fn(Vec<String>, CancellationToken) -> MicroCommand + Send + Sync + 'static,
    ) -> Self {
        Self {
            settings,
            execute: Arc::new(execute),
        }
    }
    pub fn run(&self, arguments: Vec<String>, stop: CancellationToken) -> MicroCommand {
        self.run_pinned(arguments, stop, MicroClientPins::release())
    }
    fn run_pinned(
        &self,
        arguments: Vec<String>,
        stop: CancellationToken,
        pins: Result<MicroClientPins, MicroClientFileError>,
    ) -> MicroCommand {
        let owner = self.clone();
        retain(Box::pin(async move {
            let pins = pins.map_err(file_error)?;
            if Some(owner.settings.executable_hash())
                != pins
                    .get("usbip.exe")
                    .map(|hash| format!("sha256:{hash}"))
                    .as_deref()
            {
                return Ok(failure(false, "micro_client_changed"));
            }
            let directory = Path::new(owner.settings.executable())
                .parent()
                .ok_or_else(|| {
                    MicroCommandError::Failed(Box::new(io::Error::other(
                        "Client directory required.",
                    )))
                })?;
            let _files = match MicroClientFiles::acquire(directory, &pins, &stop).await {
                Ok(files) => files,
                Err(MicroClientFileError::Changed) => {
                    return Ok(failure(false, "micro_client_changed"));
                }
                Err(MicroClientFileError::Cancelled | MicroClientFileError::Io(_)) => {
                    return Ok(failure(false, "micro_client_unavailable"));
                }
                Err(error) => return Err(file_error(error)),
            };
            if stop.is_cancelled() {
                return Ok(failure(false, "micro_client_unavailable"));
            }
            // The task owns the lease through completion, including the original
            // bounded-unknown return. Dropping an observer never drops this work.
            (owner.execute)(arguments, stop).await
        }))
    }
}
async fn launch(
    executable: &str,
    arguments: Vec<String>,
    stop: &CancellationToken,
) -> Result<MicroClientResult, MicroCommandError> {
    use ivy_host_runtime::{
        process::{Command, root::spawn_exact_environment},
        process_output::console_output_code_page,
    };
    use std::collections::BTreeMap;
    let Some(directory) = Path::new(executable).parent() else {
        return Err(failed_task());
    };
    let command = Command {
        executable: "usbip.exe".into(),
        args: arguments.into_iter().map(Into::into).collect(),
        cwd: None,
        // This root primitive installs no timeout. Phone's all-task deadline
        // starts only after launch, as in Process.Start/CancelAfter.
        timeout_ms: 10000,
    };
    let executables = BTreeMap::from([("usbip.exe".into(), executable.into())]);
    let mut process = match spawn_exact_environment(
        &command,
        directory,
        &executables,
        std::env::vars_os().collect(),
    )
    .await
    {
        Ok(process) => process,
        Err(_) => return Ok(failure(false, "micro_client_unavailable")),
    };
    let stdout = process.stdout.take().expect("Redirected root stdout");
    let stdout_code_page = console_output_code_page();
    let stderr = process.stderr.take().expect("Redirected root stderr");
    let stderr_code_page = console_output_code_page();
    let process = tokio::sync::Mutex::new(process);
    Ok(run_started_micro_command(
        stdout,
        stderr,
        async {
            let exit = process.lock().await.wait().await?;
            exit.code()
                .ok_or_else(|| io::Error::other("Root exit code unavailable."))
        },
        || {
            let mut process = process.try_lock().expect("Root wait released its guard");
            // Kill failure is swallowed only here, as in the original catch.
            // A natural exit leaves descendants alone, including held pipes.
            let _ = process.kill_tree_if_running();
            async move {
                let _ = process.wait().await;
            }
        },
        stop,
        [stdout_code_page, stderr_code_page],
    )
    .await)
}
fn file_error(error: MicroClientFileError) -> MicroCommandError {
    MicroCommandError::Failed(Box::new(error))
}
fn failure(started: bool, code: &str) -> MicroClientResult {
    MicroClientResult {
        started,
        completed: false,
        exit_code: None,
        output: String::new(),
        error_code: Some(code.into()),
    }
}
fn retain(mut work: MicroCommand) -> MicroCommand {
    // C# starts through its first incomplete await. Transfer that same future,
    // rather than replaying its already-executed prefix in a second task.
    let polled = catch_unwind(AssertUnwindSafe(|| {
        work.as_mut().poll(&mut Context::from_waker(Waker::noop()))
    }));
    match polled {
        Ok(Poll::Ready(result)) => Box::pin(std::future::ready(result)),
        Err(_) => Box::pin(async { Err(failed_task()) }),
        Ok(Poll::Pending) => match tokio::runtime::Handle::try_current() {
            Err(error) => Box::pin(async move { Err(MicroCommandError::Failed(Box::new(error))) }),
            Ok(runtime) => {
                let running = runtime.spawn(work);
                Box::pin(async move {
                    running
                        .await
                        .unwrap_or_else(|error| Err(MicroCommandError::Failed(Box::new(error))))
                })
            }
        },
    }
}
fn failed_task() -> MicroCommandError {
    MicroCommandError::Failed(Box::new(io::Error::other("Micro command task failed.")))
}

/// Called only after a child has started. The process owner supplies its exit
/// observation and kill-if-live/root-drain sequence; this code owns Phone's
/// output, all-task wait, linked ten-second deadline and two-second drain bound.
pub(crate) async fn run_started_micro_command<D: Future<Output = ()>>(
    mut stdout: impl AsyncRead + Unpin,
    mut stderr: impl AsyncRead + Unpin,
    exit: impl Future<Output = io::Result<i32>>,
    kill_and_drain: impl FnOnce() -> D,
    stop: &CancellationToken,
    code_pages: [u32; 2],
) -> MicroClientResult {
    let deadline = stop.child_token();
    let exit = async {
        tokio::select! {
            biased;
            _ = deadline.cancelled() => Err(io::Error::new(io::ErrorKind::Interrupted,"cancelled")),
            result = exit => result,
        }
    };
    let all = async {
        tokio::join!(
            biased;
            read_micro_console_output(&mut stdout, &deadline, code_pages[0]),
            read_micro_console_output(&mut stderr, &deadline, code_pages[1]),
            exit,
        )
    };
    tokio::pin!(all);
    // Task.WhenAll does not short-circuit a pipe failure while the other pipe
    // or the root wait is still pending. Only the shared deadline cancels them.
    let result = match tokio::time::timeout(Duration::from_secs(10), &mut all).await {
        Ok(result) => result,
        Err(_) => {
            deadline.cancel();
            all.await
        }
    };
    if let (Ok(output), Ok(_), Ok(code)) = result {
        return MicroClientResult {
            started: true,
            completed: true,
            exit_code: Some(code),
            output,
            error_code: None,
        };
    }
    // The original synchronous kill attempt precedes creation of its two-second
    // root-wait token. Only the returned drain future is inside that bound.
    let drain = kill_and_drain();
    let _ = tokio::time::timeout(Duration::from_secs(2), drain).await;
    deadline.cancel();
    failure(true, "micro_attach_unknown")
}

#[cfg(test)]
#[path = "../../../../../tests/rust/phone_micro_client.rs"]
mod tests;
