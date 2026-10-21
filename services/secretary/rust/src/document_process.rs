//! Disposable document parsing through the qualified shared process-tree owner.
use crate::{
    document::{Format, SOURCE_LIMIT},
    media_document::{DocumentFuture, DocumentWorker, WORKER_REPLY_BYTES, format_name},
    media_input::REQUEST_BYTES,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, Outcome, json::parse};
use ivy_host_runtime::{
    node_runtime::NodeRuntime,
    process::{self, Command, LaunchContext, ProcessMemoryLimit},
};
use std::{collections::BTreeMap, io, num::NonZeroU64, path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    sync::watch,
    task::JoinHandle,
    time::Instant,
};

pub const JOB_COMMIT_BYTES: u64 = 512 * 1024 * 1024;
const LIFETIME: Duration = Duration::from_secs(30);
const DRAIN: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub struct PdfAdapter {
    pub node: NodeRuntime,
    pub script: PathBuf,
}

#[derive(Clone)]
pub struct HostDocumentWorker {
    root: PathBuf,
    executable: PathBuf,
    job_launcher: PathBuf,
    pdf: PdfAdapter,
    cancel: watch::Receiver<bool>,
}
impl HostDocumentWorker {
    /// Paths/runtime come from the admitted component package and its dispatcher.
    /// This binding does not enable the dormant media/triage workers at startup.
    pub fn new(
        root: PathBuf,
        executable: PathBuf,
        job_launcher: PathBuf,
        pdf: PdfAdapter,
        cancel: watch::Receiver<bool>,
    ) -> Self {
        Self {
            root,
            executable,
            job_launcher,
            pdf,
            cancel,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Failure {
    Parser,
    Memory,
    Timeout,
    Cancelled,
    Unknown,
}
impl Failure {
    fn reply(self) -> IvyResult<IvyJson> {
        match self {
            Self::Timeout => Ok(code("media_document_timeout")),
            Self::Unknown => Err(IvyError::new(
                "outcome_unknown",
                "Document worker retirement or private pipe EOF is unproved.",
            )
            .with_outcome(Outcome::Unknown)),
            _ => Err(IvyError::new(
                "media_document_parser_failed",
                "The bounded document worker did not complete.",
            )
            .with_outcome(Outcome::Completed)),
        }
    }
}
fn code(value: &str) -> IvyJson {
    IvyJson::object([("code", value.into())])
}
impl DocumentWorker for HostDocumentWorker {
    fn extract<'a>(&'a self, format: Format, bytes: &'a [u8], budget: f64) -> DocumentFuture<'a> {
        Box::pin(async move {
            if !budget.is_finite()
                || budget.fract() != 0.0
                || budget < 1.0
                || budget > REQUEST_BYTES as f64
            {
                return Ok(code("media_document_invalid"));
            }
            if bytes.len() > SOURCE_LIMIT {
                return Ok(code("media_document_too_large"));
            }
            let owner = self.clone();
            let bytes = bytes.to_vec();
            let deadline = Instant::now() + LIFETIME;
            // Losing a caller's waiter must not drop an admitted process owner.
            // It retains cancellation, its absolute deadline, pipes and retirement.
            match tokio::spawn(run(owner, format, bytes, budget as usize, deadline)).await {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(failure)) => failure.reply(),
                Err(_) => Failure::Unknown.reply(),
            }
        })
    }
}

struct Output {
    bytes: Vec<u8>,
    exceeded: bool,
}
async fn read_private(
    mut pipe: impl AsyncRead + Unpin,
    retain: bool,
    failed: watch::Sender<bool>,
) -> io::Result<Output> {
    let mut bytes = Vec::new();
    let mut total = 0usize;
    let mut exceeded = false;
    let mut part = [0; 8192];
    loop {
        let count = match pipe.read(&mut part).await {
            Ok(count) => count,
            Err(error) => {
                let _ = failed.send(true);
                return Err(error);
            }
        };
        if count == 0 {
            return Ok(Output { bytes, exceeded });
        }
        if !exceeded && count > WORKER_REPLY_BYTES.saturating_sub(total) {
            exceeded = true;
            let _ = failed.send(true);
        }
        if retain && !exceeded {
            bytes.extend_from_slice(&part[..count]);
        }
        total = total.saturating_add(count);
        // After overflow, request stop but continue draining privately to EOF.
        // Neither a full buffer nor dropping a pipe is an EOF proof.
    }
}
async fn joined<T>(mut task: JoinHandle<T>, deadline: Instant) -> Option<T> {
    match tokio::time::timeout_at(deadline, &mut task).await {
        Ok(result) => result.ok(),
        Err(_) => {
            task.abort();
            let _ = task.await;
            None
        }
    }
}
async fn cancelled(mut cancel: watch::Receiver<bool>) {
    loop {
        if *cancel.borrow_and_update() {
            return;
        }
        if cancel.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}
async fn io_failed(mut receiver: watch::Receiver<bool>) {
    loop {
        if *receiver.borrow_and_update() {
            return;
        }
        if receiver.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}
async fn run(
    owner: HostDocumentWorker,
    format: Format,
    bytes: Vec<u8>,
    budget: usize,
    deadline: Instant,
) -> Result<IvyJson, Failure> {
    if *owner.cancel.borrow() {
        return Err(Failure::Cancelled);
    }
    let (executable, args) = if format == Format::Pdf {
        (
            owner.pdf.node.executable.clone(),
            vec![owner.pdf.script.into_os_string()],
        )
    } else {
        (
            owner.executable,
            vec![
                "--document-worker".into(),
                format_name(format).into(),
                budget.to_string().into(),
            ],
        )
    };
    // Await the qualified finite handshake. Cancelling it halfway through cannot
    // establish nonexecution; an uncertain launch retains its Unknown outcome.
    let mut limited = process::spawn_exact_environment_with_memory_limit(
        &Command {
            executable: "document-parser".into(),
            args,
            cwd: Some(owner.root.clone()),
            timeout_ms: 30000,
        },
        &owner.root,
        &BTreeMap::from([("document-parser".into(), executable)]),
        LaunchContext {
            environment: process::runtime_environment(&BTreeMap::new()),
            job_launcher: Some(owner.job_launcher),
            raw_output: true,
            ..Default::default()
        },
        ProcessMemoryLimit {
            maximum_job_commit_bytes: NonZeroU64::new(JOB_COMMIT_BYTES).unwrap(),
        },
    )
    .await
    .map_err(|failure| {
        if failure.error.outcome == Outcome::Unknown {
            Failure::Unknown
        } else {
            Failure::Parser
        }
    })?;
    let (Some(mut stdin), Some(stdout), Some(stderr)) = (
        limited.process.stdin.take(),
        limited.process.stdout.take(),
        limited.process.stderr.take(),
    ) else {
        let _ = limited.process.stop(Duration::ZERO).await;
        return Err(Failure::Unknown);
    };
    let (failed, failures) = watch::channel(false);
    let writer_failure = failed.clone();
    let writer = tokio::spawn(async move {
        let result = async {
            stdin.write_all(&bytes).await?;
            stdin.shutdown().await
        }
        .await;
        if result.is_err() {
            let _ = writer_failure.send(true);
        }
        result
    });
    let out = tokio::spawn(read_private(stdout, true, failed.clone()));
    let err = tokio::spawn(read_private(stderr, false, failed));
    let (status, mut failure) = tokio::select! {
        status = limited.process.completion.wait() => (Some(status), None),
        _ = tokio::time::sleep_until(deadline) => (limited.process.stop(Duration::ZERO).await.ok(), Some(Failure::Timeout)),
        _ = cancelled(owner.cancel) => (limited.process.stop(Duration::ZERO).await.ok(), Some(Failure::Cancelled)),
        _ = io_failed(failures) => (limited.process.stop(Duration::ZERO).await.ok(), Some(Failure::Parser)),
    };
    let drain_deadline = Instant::now() + DRAIN;
    let (written, out, err) = tokio::join!(
        joined(writer, drain_deadline),
        joined(out, drain_deadline),
        joined(err, drain_deadline)
    );
    let Some(status) = status else {
        return Err(Failure::Unknown);
    };
    if status.error_code.is_some()
        || !process::owned_process_tree_stopped(&limited.process)
            .await
            .unwrap_or(false)
    {
        return Err(Failure::Unknown);
    }
    let (Some(written), Some(Ok(out)), Some(Ok(err))) = (written, out, err) else {
        return Err(Failure::Unknown);
    };
    // The installed root policy is context, never an exhausted threshold. Only
    // actual native evidence for this owned tree selects the memory cause.
    if limited.memory_limit_evidence().is_some() {
        return Err(Failure::Memory);
    }
    if failure.is_none()
        && (written.is_err() || out.exceeded || err.exceeded || status.exit_code != Some(0))
    {
        failure = Some(Failure::Parser);
    }
    if let Some(failure) = failure {
        return Err(failure);
    }
    let text = std::str::from_utf8(&out.bytes).map_err(|_| Failure::Parser)?;
    parse(text, WORKER_REPLY_BYTES).map_err(|_| Failure::Parser)
}

#[cfg(all(test, windows))]
#[path = "../../../../tests/secretary-rust/document_process_cases.rs"]
mod tests;
