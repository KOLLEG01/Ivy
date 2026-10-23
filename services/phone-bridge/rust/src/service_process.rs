//! The service's verified native lifetime. Shared process/Job ownership supplies
//! the only stop authority; Phone retains pipe receipts, epoch and stop evidence.
use crate::{
    journal::PhoneJournal,
    native_client::{PhoneNativeClient, new_phone_epoch},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, Outcome};
use ivy_host_runtime::{
    config::json_file,
    node_path, paths,
    process::{self, Command, LaunchContext, OwnedProcess, ProcessResult},
    process_fence::{
        clear_process_stop_fence, process_stop_fence, record_process_stop_fence,
        require_clear_process_stop_fence,
    },
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite},
    sync::{oneshot, watch},
};

pub struct PhoneProcessOptions {
    pub artifact_root: PathBuf,
    pub executable: PathBuf,
    pub executable_hash: String,
    pub journal: PhoneJournal,
}

type Work<T> = Pin<Box<dyn Future<Output = T> + Send>>;
type Reader = Box<dyn AsyncRead + Unpin + Send>;
type Writer = Box<dyn AsyncWrite + Unpin + Send>;
trait ChildOwner: Send + Sync {
    fn pid(&self) -> Option<u32>;
    fn completion(&self) -> Work<Arc<ProcessResult>>;
    fn stop(&self, grace: Duration) -> Work<IvyResult<Arc<ProcessResult>>>;
}
struct SharedChild(Arc<OwnedProcess>);
impl ChildOwner for SharedChild {
    fn pid(&self) -> Option<u32> {
        Some(self.0.pid)
    }
    fn completion(&self) -> Work<Arc<ProcessResult>> {
        let completion = self.0.completion.clone();
        Box::pin(async move { completion.wait().await })
    }
    fn stop(&self, grace: Duration) -> Work<IvyResult<Arc<ProcessResult>>> {
        let owner = self.0.clone();
        Box::pin(async move { OwnedProcess::stop(&owner, grace).await })
    }
}
struct Launched {
    owner: Arc<dyn ChildOwner>,
    input: Writer,
    output: Reader,
    stderr: Option<Reader>,
}
trait Launcher: Send + Sync {
    fn launch(
        &self,
        command: Command,
        root: PathBuf,
        context: LaunchContext,
    ) -> Work<IvyResult<Launched>>;
}
struct SharedLauncher;
impl Launcher for SharedLauncher {
    fn launch(
        &self,
        command: Command,
        root: PathBuf,
        context: LaunchContext,
    ) -> Work<IvyResult<Launched>> {
        Box::pin(async move {
            let mut child = process::spawn(&command, &root, &BTreeMap::new(), context).await?;
            // raw_output guarantees these inherited pipes. Never drop an owned
            // process on an unexpected pipe error without first stopping it.
            let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
                let result = child.stop(Duration::from_millis(250)).await?;
                if result.error_code.as_deref() == Some("outcome_unknown") {
                    record_process_stop_fence(
                        PathBuf::from(&command.args[3]),
                        stop_fence(
                            &node_path::from_native(Path::new(&command.args[1])),
                            Some(child.pid),
                        ),
                    )
                    .await?;
                    return Err(lost_owner());
                }
                return Err(IvyError::internal());
            };
            let stderr = child.stderr.take().map(|v| Box::new(v) as Reader);
            Ok(Launched {
                owner: Arc::new(SharedChild(Arc::new(child))),
                input: Box::new(input),
                output: Box::new(output),
                stderr,
            })
        })
    }
}

#[derive(Clone)]
pub struct PhoneProcessCompletion(watch::Receiver<Option<IvyResult<Arc<ProcessResult>>>>);
impl PhoneProcessCompletion {
    pub async fn wait(&self) -> IvyResult<Arc<ProcessResult>> {
        let mut receive = self.0.clone();
        loop {
            if let Some(result) = receive.borrow_and_update().clone() {
                return result;
            }
            receive.changed().await.map_err(|_| lost_owner())?;
        }
    }
}
#[derive(Clone)]
pub struct PhoneProcessClose(watch::Receiver<Option<IvyResult<()>>>);
impl PhoneProcessClose {
    pub async fn wait(&self) -> IvyResult<()> {
        let mut receive = self.0.clone();
        loop {
            if let Some(result) = receive.borrow_and_update().clone() {
                return result;
            }
            receive.changed().await.map_err(|_| lost_owner())?;
        }
    }
}
struct Running {
    child: Arc<dyn ChildOwner>,
    client: PhoneNativeClient,
    completion: PhoneProcessCompletion,
    stopping: Mutex<Option<PhoneProcessClose>>,
    epoch_gate: tokio::sync::Mutex<()>,
}
impl Running {
    fn close(self: &Arc<Self>) -> PhoneProcessClose {
        let mut stopping = self.stopping.lock().unwrap();
        if let Some(work) = &*stopping {
            return work.clone();
        }
        let (done, receive) = watch::channel(None);
        let work = PhoneProcessClose(receive);
        *stopping = Some(work.clone());
        self.client.close("phone_owner_stopping");
        let running = self.clone();
        tokio::spawn(async move {
            // Closing the client drains admitted writes then closes stdin. An
            // evidence failure must never skip the shared owned-process stop.
            let _ = tokio::time::timeout(Duration::from_secs(5), running.completion.wait()).await;
            let result = async {
                let result = running.child.stop(Duration::from_millis(250)).await?;
                running.completion.wait().await?;
                require(
                    result.error_code.as_deref() != Some("outcome_unknown"),
                    "outcome_unknown",
                    "Phone stop remains unknown; its durable fence prevents another owner.",
                )
                .map_err(|e| e.with_outcome(Outcome::Unknown))
            }
            .await;
            done.send_replace(Some(result));
        });
        work
    }
}
struct Lease(Arc<Running>);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.close();
    }
}
/// Dropping the final process handle revokes pipe admission and retains cleanup.
/// A request/client/completion observer never acquires the process stop authority.
#[derive(Clone)]
pub struct PhoneProcess {
    pub client: PhoneNativeClient,
    pub epoch: JsString,
    pub completion: PhoneProcessCompletion,
    lease: Arc<Lease>,
}
impl PhoneProcess {
    pub fn close(&self) -> PhoneProcessClose {
        self.lease.0.close()
    }
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn lost_owner() -> IvyError {
    IvyError::new(
        "outcome_unknown",
        "Phone process ownership could not be observed.",
    )
    .with_outcome(Outcome::Unknown)
}
fn io_error(_: std::io::Error) -> IvyError {
    IvyError::internal()
}
fn stop_fence(epoch: &JsString, pid: Option<u32>) -> IvyJson {
    IvyJson::object([
        ("schemaVersion", 1.0.into()),
        ("owner", "native".into()),
        ("ownerId", epoch.clone().into()),
        (
            "processId",
            pid.map_or(IvyJson::Null, |pid| (pid as f64).into()),
        ),
        (
            "observedAt",
            chrono::Utc::now()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                .into(),
        ),
        ("code", "outcome_unknown".into()),
    ])
}

async fn native_lease_released(path: PathBuf) -> IvyResult<bool> {
    tokio::task::spawn_blocking(move || {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
        {
            Ok(lease) => {
                drop(lease);
                Ok(true)
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::PermissionDenied
                    || if cfg!(windows) {
                        matches!(error.raw_os_error(), Some(5 | 32 | 33))
                    } else {
                        matches!(error.raw_os_error(), Some(1 | 13 | 16))
                    } =>
            {
                Ok(false)
            }
            Err(error) => Err(io_error(error)),
        }
    })
    .await
    .map_err(|_| IvyError::internal())?
}
/// Only a matching original record plus released lifetime lease can clear the
/// exact retained native fence. Record syntax intentionally follows the service,
/// whose safe-integer/20-digit ranges differ from the native companion's parser.
pub async fn reconcile_phone_process_stop_fence(data_root: impl Into<PathBuf>) -> IvyResult<bool> {
    reconcile_with(data_root.into(), native_lease_released).await
}
async fn reconcile_with<F: Future<Output = IvyResult<bool>>>(
    data_root: PathBuf,
    probe: impl FnOnce(PathBuf) -> F,
) -> IvyResult<bool> {
    let Some(fence) = process_stop_fence(data_root.clone()).await? else {
        return Ok(false);
    };
    if fence.get("owner") != Some(&"native".into()) {
        return Ok(false);
    }
    let record = json_file(data_root.join("phone-native-owner.json"), 4096).await?;
    if record.is_null() {
        return Err(IvyError::internal());
    }
    let fields = ["creationFileTime", "epoch", "pid", "schemaVersion"];
    require(
        record.as_object().is_some_and(|o| o.len() == fields.len())
            && fields.iter().all(|key| record.get(key).is_some())
            && record.get("schemaVersion") == Some(&1.0.into())
            && record.get("epoch") == fence.get("ownerId")
            && record
                .get("pid")
                .and_then(IvyJson::as_f64)
                .is_some_and(|n| n > 0.0 && n <= 9_007_199_254_740_991.0 && n.fract() == 0.0)
            && record
                .get("creationFileTime")
                .and_then(IvyJson::as_js_string)
                .is_some_and(|s| {
                    (1..=20).contains(&s.0.len())
                        && matches!(s.0[0], 49..=57)
                        && s.0.iter().all(|c| matches!(*c, 48..=57))
                }),
        "outcome_unknown",
        "Phone native owner record does not match its retained stop fence.",
    )?;
    if !probe(data_root.join("phone-native.lock")).await? {
        return Ok(false);
    }
    clear_process_stop_fence(data_root, fence).await
}

async fn file_hash(path: &Path) -> IvyResult<String> {
    let mut file = tokio::fs::File::open(path).await.map_err(io_error)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 4 * 1024 * 1024];
    loop {
        let count = file.read(&mut buffer).await.map_err(io_error)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    let bytes = hash.finalize();
    let mut digest = String::from("sha256:");
    for byte in bytes {
        use std::fmt::Write;
        write!(&mut digest, "{byte:02x}").map_err(|_| IvyError::internal())?;
    }
    Ok(digest)
}

pub async fn start_phone_process(options: PhoneProcessOptions) -> IvyResult<PhoneProcess> {
    start_with(
        options,
        Arc::new(SharedLauncher),
        cfg!(all(windows, target_arch = "x86_64")),
    )
    .await
}
async fn start_with(
    options: PhoneProcessOptions,
    launcher: Arc<dyn Launcher>,
    supported_platform: bool,
) -> IvyResult<PhoneProcess> {
    let (send, receive) = oneshot::channel();
    // Once polled, startup is retained independently of its requesting future.
    // If delivery loses its observer, dropping the returned lease still stops it.
    tokio::spawn(async move {
        let _ = send.send(start_inner(options, launcher, supported_platform).await);
    });
    receive.await.map_err(|_| lost_owner())?
}
async fn start_inner(
    options: PhoneProcessOptions,
    launcher: Arc<dyn Launcher>,
    supported_platform: bool,
) -> IvyResult<PhoneProcess> {
    require(
        supported_platform,
        "phone_platform",
        "Phone requires its owning Windows x64 host.",
    )?;
    let journal = options.journal;
    let data_root = journal.data_root().to_path_buf();
    require(
        node_path::is_absolute(
            node_path::Style::native(),
            &node_path::from_native(&options.artifact_root),
        ) && !node_path::is_absolute(
            node_path::Style::native(),
            &node_path::from_native(&options.executable),
        ),
        "invalid_arguments",
        "Phone executable must be relative to its owned artifact.",
    )?;
    reconcile_phone_process_stop_fence(data_root.clone()).await?;
    require_clear_process_stop_fence(data_root.clone()).await?;
    let executable_name = node_path::from_native(&options.executable).to_utf8()?;
    let (root, executable) = tokio::task::spawn_blocking(move || {
        let root = paths::realpath(&options.artifact_root).map_err(io_error)?;
        let selected = node_path::Context::current().map_err(io_error)?.resolve(&[
            node_path::from_native(&root),
            node_path::from_native(&options.executable),
        ]);
        let executable = paths::realpath(&node_path::to_native(&selected)).map_err(io_error)?;
        require(
            paths::inside(&root, &executable).map_err(io_error)?
                && std::fs::metadata(&executable).map_err(io_error)?.is_file(),
            "target_conflict",
            "Phone executable leaves its verified artifact.",
        )?;
        Ok::<_, IvyError>((root, executable))
    })
    .await
    .map_err(|_| IvyError::internal())??;
    require(
        file_hash(&executable).await? == options.executable_hash,
        "phone_build_mismatch",
        "Phone executable differs from its selected artifact.",
    )?;
    let epoch = new_phone_epoch();
    let previous = journal.epoch().await?;
    let launched = launcher
        .launch(
            Command {
                executable: executable_name,
                args: vec![
                    "--epoch".into(),
                    node_path::to_native(&epoch).into_os_string(),
                    "--owner-root".into(),
                    data_root.clone().into_os_string(),
                ],
                cwd: None,
                timeout_ms: 15000,
            },
            root.clone(),
            LaunchContext {
                job_launcher: Some(root.join("dist/native/ivy-job.exe")),
                raw_output: true,
                ..LaunchContext::default()
            },
        )
        .await?;
    let client = match PhoneNativeClient::start(
        epoch.clone(),
        launched.input,
        launched.output,
        Arc::new(journal.clone()),
    ) {
        Ok(client) => client,
        Err(error) => {
            let result = launched.owner.stop(Duration::from_millis(250)).await?;
            if result.error_code.as_deref() == Some("outcome_unknown") {
                record_process_stop_fence(data_root, stop_fence(&epoch, launched.owner.pid()))
                    .await?;
                return Err(lost_owner());
            }
            return Err(error);
        }
    };
    let (completed, receive) = watch::channel(None);
    let running = Arc::new(Running {
        child: launched.owner,
        client: client.clone(),
        completion: PhoneProcessCompletion(receive),
        stopping: Mutex::new(None),
        epoch_gate: tokio::sync::Mutex::new(()),
    });
    if let Some(mut stderr) = launched.stderr {
        let completion = running.completion.clone();
        tokio::spawn(async move {
            // The original captureOutput:false still drains/discards stderr.
            let mut discard = tokio::io::sink();
            tokio::select! {
                _ = tokio::io::copy(&mut stderr, &mut discard) => (),
                _ = completion.wait() => (),
            }
        });
    }
    let owner = running.clone();
    let completed_epoch = epoch.clone();
    let completed_journal = journal.clone();
    tokio::spawn(async move {
        let result = async {
            let result = owner.child.completion().await;
            let _epoch = owner.epoch_gate.lock().await;
            if result.error_code.as_deref() == Some("outcome_unknown") {
                record_process_stop_fence(
                    data_root,
                    stop_fence(&completed_epoch, owner.child.pid()),
                )
                .await?;
            }
            owner.client.close(
                result
                    .error_code
                    .as_deref()
                    .unwrap_or("phone_process_exited"),
            );
            completed_journal.lose_epoch(completed_epoch).await?;
            Ok(result)
        }
        .await;
        if result.is_err() {
            owner.client.close("phone_stop_evidence_failed");
        }
        completed.send_replace(Some(result));
    });
    let closing = running.clone();
    tokio::spawn(async move {
        closing.client.closed().await;
        let _ = closing.close().wait().await;
    });
    let ready = async {
        let ready = client
            .request(
                "heartbeat",
                IvyJson::object([] as [(&str, IvyJson); 0]),
                None,
                15000,
            )?
            .wait()
            .await?;
        require(
            ready.get("ok") == Some(&true.into()),
            "phone_start_failed",
            "Original native Phone owner did not complete its startup handshake.",
        )?;
        client.start_heartbeat()?;
        let hello = client.observe(None).await?;
        require(
            hello.get("configured") == Some(&false.into())
                && ["call", "endpoint", "registration"]
                    .iter()
                    .all(|k| hello.get(k).is_some_and(IvyJson::is_null)),
            "phone_start_failed",
            "Phone native process must begin unconfigured behind its exclusive owner lease.",
        )?;
        let _epoch = running.epoch_gate.lock().await;
        require(
            !client.is_closed(),
            "phone_unavailable",
            "Original native Phone status is unavailable.",
        )?;
        journal.commit_process_epoch(previous, epoch.clone()).await
    }
    .await;
    if let Err(error) = ready {
        running.close().wait().await?;
        return Err(error);
    }
    Ok(PhoneProcess {
        client,
        epoch,
        completion: running.completion.clone(),
        lease: Arc::new(Lease(running)),
    })
}

#[cfg(test)]
#[path = "../../../../tests/rust/phone_service_process.rs"]
mod tests;
