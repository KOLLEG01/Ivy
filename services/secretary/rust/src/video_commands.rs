//! Binary video commands over the shared admitted process-tree owner.
use crate::media_video::{VideoCommand, VideoCommands, VideoFuture};
use ivy_contracts::{IvyError, IvyResult, Outcome};
use ivy_host_runtime::process::{self, Command, LaunchContext};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};
use tokio::{io::AsyncReadExt, sync::watch, task::JoinHandle, time::Instant};

#[derive(Clone)]
pub struct HostVideoCommands {
    root: PathBuf,
    job_launcher: PathBuf,
}
impl HostVideoCommands {
    pub fn new(root: PathBuf, job_launcher: PathBuf) -> Self {
        Self { root, job_launcher }
    }
}
impl VideoCommands for HostVideoCommands {
    fn run<'a>(
        &'a self,
        command: VideoCommand,
        cancel: watch::Receiver<bool>,
    ) -> VideoFuture<'a, Vec<u8>> {
        let owner = self.clone();
        Box::pin(async move {
            // An admitted command retains its owner, pipes and deadline even if
            // its caller stops waiting. No child escapes into an unowned task.
            tokio::spawn(run(owner, command, cancel))
                .await
                .map_err(|_| unknown())?
        })
    }
}
fn failure(code: &str) -> IvyError {
    IvyError::new(code, "The bounded local video decoder did not complete.")
}
fn unknown() -> IvyError {
    failure("outcome_unknown").with_outcome(Outcome::Unknown)
}
async fn cancelled(mut cancel: watch::Receiver<bool>) {
    loop {
        if *cancel.borrow_and_update() {
            return;
        }
        if cancel.changed().await.is_err() {
            // Losing a sender is not a cancellation request.
            std::future::pending::<()>().await;
        }
    }
}
async fn output(mut pipe: tokio::process::ChildStdout, maximum: usize) -> IvyResult<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut part = [0; 8192];
    loop {
        let count = pipe
            .read(&mut part)
            .await
            .map_err(|_| failure("media_video_decoder_failed"))?;
        if count == 0 {
            return Ok(bytes);
        }
        if count > maximum.saturating_sub(bytes.len()) {
            return Err(failure("media_native_input_too_large"));
        }
        bytes.extend_from_slice(&part[..count]);
    }
}
async fn drain<T>(handle: &mut Option<JoinHandle<T>>, deadline: Instant) -> bool {
    let Some(mut handle) = handle.take() else {
        return true;
    };
    match tokio::time::timeout_at(deadline, &mut handle).await {
        Ok(result) => result.is_ok(),
        Err(_) => {
            handle.abort();
            let _ = handle.await;
            false
        }
    }
}
async fn run(
    owner: HostVideoCommands,
    command: VideoCommand,
    cancel: watch::Receiver<bool>,
) -> IvyResult<Vec<u8>> {
    if *cancel.borrow() {
        return Err(failure("media_video_cancelled"));
    }
    let deadline = Instant::now() + Duration::from_millis(command.timeout_ms.into());
    let selected = Command {
        executable: "video-decoder".into(),
        args: command.args.into_iter().map(Into::into).collect(),
        cwd: Some(command.cwd),
        timeout_ms: command.timeout_ms.into(),
    };
    // Do not cancel a launch handshake halfway through and infer nonexecution.
    // The Host owns its bounded acknowledgement and reports uncertain launch.
    let mut child = process::spawn_exact_environment(
        &selected,
        &owner.root,
        &BTreeMap::from([("video-decoder".into(), command.executable)]),
        LaunchContext {
            environment: command.environment,
            job_launcher: Some(owner.job_launcher),
            raw_output: true,
            ..Default::default()
        },
    )
    .await
    .map_err(|error| {
        if error.outcome == Outcome::Unknown {
            failure("media_video_decoder_failed").with_outcome(Outcome::Unknown)
        } else {
            failure("media_video_decoder_failed")
        }
    })?;
    drop(child.stdin.take());
    let Some(stdout) = child.stdout.take() else {
        let _ = child.stop(Duration::ZERO).await;
        return Err(unknown());
    };
    let Some(mut stderr) = child.stderr.take() else {
        let _ = child.stop(Duration::ZERO).await;
        return Err(unknown());
    };
    let mut out = Some(tokio::spawn(output(stdout, command.maximum_bytes)));
    let mut err = Some(tokio::spawn(async move {
        // Container diagnostics can contain private metadata. Retain no bytes.
        tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await
    }));
    let mut bytes = None;
    let mut result = None;
    let mut failed = None;
    while out.is_some() || err.is_some() || result.is_none() {
        tokio::select! {
            value = async { out.as_mut().unwrap().await }, if out.is_some() => {
                out.take();
                match value { Ok(Ok(value)) => bytes = Some(value), Ok(Err(error)) => { failed = Some(error); break; }, Err(_) => { failed = Some(unknown()); break; } }
            },
            value = async { err.as_mut().unwrap().await }, if err.is_some() => {
                err.take();
                match value { Ok(Ok(_)) => (), Ok(Err(_)) => { failed = Some(failure("media_video_decoder_failed")); break; }, Err(_) => { failed = Some(unknown()); break; } }
            },
            value = child.completion.wait(), if result.is_none() => {
                if value.error_code.is_some() { failed = Some(unknown()); result = Some(value); break; }
                result = Some(value);
            },
            _ = tokio::time::sleep_until(deadline) => { failed = Some(failure("media_video_timeout")); break; },
            _ = cancelled(cancel.clone()) => { failed = Some(failure("media_video_cancelled")); break; },
        }
    }
    let status = if failed.is_some() {
        child.stop(Duration::ZERO).await.map_err(|_| unknown())?
    } else {
        result.ok_or_else(unknown)?
    };
    let drain_deadline = Instant::now() + Duration::from_secs(2);
    let (out_drained, err_drained) = tokio::join!(
        drain(&mut out, drain_deadline),
        drain(&mut err, drain_deadline)
    );
    if status.error_code.is_some()
        || !out_drained
        || !err_drained
        || !process::owned_process_tree_stopped(&child)
            .await
            .unwrap_or(false)
    {
        return Err(unknown());
    }
    if let Some(error) = failed {
        return Err(error);
    }
    if status.exit_code != Some(0) {
        return Err(failure("media_video_decode_failed"));
    }
    bytes.ok_or_else(unknown)
}
