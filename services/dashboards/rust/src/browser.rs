//! Chromium lifetime uses the shared process owner. Cancellation drops only an
//! observation; launch, context disposal and process retirement keep their owner.
use crate::cdp::{CdpRenderer, RenderError, RenderInput};
use ivy_contracts::{IvyError, IvyResult};
use ivy_host_runtime::process::{self, Command, Completion, LaunchContext, OwnedProcess};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::PathBuf,
    sync::{
        Arc, Mutex as SyncMutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, Semaphore};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct BrowserOptions {
    pub executable: PathBuf,
    pub artifact_root: PathBuf,
    pub temporary_root: PathBuf,
    pub job_launcher: Option<PathBuf>,
}
struct Running {
    process: OwnedProcess,
    profile: PathBuf,
    renderer: Option<Arc<CdpRenderer>>,
}
type Observation = Option<(Arc<CdpRenderer>, Completion)>;
struct Inner {
    options: BrowserOptions,
    running: Mutex<Option<Running>>,
    observation: SyncMutex<Observation>,
    render_slot: Arc<Semaphore>,
    closed: AtomicBool,
    shutdown: CancellationToken,
}
#[derive(Clone)]
pub struct BrowserOwner(Arc<Inner>);

impl BrowserOwner {
    pub fn new(options: BrowserOptions) -> Self {
        Self(Arc::new(Inner {
            options,
            running: Mutex::new(None),
            observation: SyncMutex::new(None),
            render_slot: Arc::new(Semaphore::new(1)),
            closed: AtomicBool::new(false),
            shutdown: CancellationToken::new(),
        }))
    }
    pub fn ready(&self) -> bool {
        !self.0.closed.load(Ordering::Acquire)
            && self
                .0
                .observation
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|(renderer, process)| renderer.ready() && process.current().is_none())
    }
    pub async fn start(&self) -> IvyResult<()> {
        let owner = self.clone();
        tokio::spawn(async move {
            let mut running = owner.0.running.lock().await;
            owner.ensure(&mut running).await.map(|_| ())
        })
        .await
        .map_err(|_| IvyError::internal())?
    }
    pub async fn render(
        &self,
        input: RenderInput,
        cancel: CancellationToken,
    ) -> IvyResult<Vec<u8>> {
        let permit = self
            .0
            .render_slot
            .clone()
            .try_acquire_owned()
            .map_err(|_| RenderError::Busy.into_ivy())?;
        let owner = self.clone();
        let (mut tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _permit = permit;
            let mut running = owner.0.running.lock().await;
            let result = async {
                if cancel.is_cancelled() || tx.is_closed() {
                    return Err(IvyError::internal());
                }
                // Host ownership errors are internal renderer failures on the
                // existing public surface, not new command/process error codes.
                let renderer = owner
                    .ensure(&mut running)
                    .await
                    .map_err(|_| IvyError::internal())?;
                tokio::select! {
                    biased;
                    _=tx.closed()=>Err(IvyError::internal()),
                    _=owner.0.shutdown.cancelled()=>Err(IvyError::internal()),
                    result=renderer.render(input, &cancel)=>result.map_err(RenderError::into_ivy),
                }
            }
            .await;
            // The CDP lease owns its context after a dropped render observation.
            // Keep admission until that lease has actually disposed the context.
            if let Some(current) = running.as_ref().and_then(|r| r.renderer.as_ref()) {
                current.idle().await;
            }
            let _ = tx.send(result);
        });
        rx.await.map_err(|_| IvyError::internal())?
    }
    /// Idempotent and retryable: unknown retirement keeps the process/profile
    /// fenced, so another close can observe it and no replacement is launched.
    pub async fn close(&self) -> IvyResult<()> {
        self.0.closed.store(true, Ordering::Release);
        self.0.shutdown.cancel();
        let owner = self.clone();
        tokio::spawn(async move {
            let mut running = owner.0.running.lock().await;
            owner.retire(&mut running).await
        })
        .await
        .map_err(|_| IvyError::internal())?
    }
    async fn ensure(&self, running: &mut Option<Running>) -> IvyResult<Arc<CdpRenderer>> {
        if self.0.closed.load(Ordering::Acquire) {
            return Err(IvyError::internal());
        }
        if self.ready() {
            return Ok(running.as_ref().unwrap().renderer.as_ref().unwrap().clone());
        }
        self.retire(running).await?;
        let options = &self.0.options;
        let profile = options
            .temporary_root
            .join(format!("ivy-dashboards-{}", uuid::Uuid::new_v4()));
        let directory = tokio::fs::DirBuilder::new();
        #[cfg(unix)]
        let directory = {
            let mut directory = directory;
            directory.mode(0o700);
            directory
        };
        directory
            .create(&profile)
            .await
            .map_err(|_| IvyError::internal())?;
        let command = Command {
            executable: "$chromium".into(),
            args: arguments(&profile),
            cwd: None,
            timeout_ms: 0,
        };
        let process = process::spawn(
            &command,
            &options.artifact_root,
            &BTreeMap::from([("$chromium".into(), options.executable.clone())]),
            LaunchContext {
                job_launcher: options.job_launcher.clone(),
                environment: browser_environment(),
                ..Default::default()
            },
        )
        .await;
        let process = match process {
            Ok(process) => process,
            Err(error) => {
                let _ = tokio::fs::remove_dir_all(&profile).await;
                return Err(error);
            }
        };
        *running = Some(Running {
            process,
            profile,
            renderer: None,
        });
        let current = running.as_ref().unwrap();
        let connect = async {
            loop {
                if current.process.completion.current().is_some() {
                    return Err(IvyError::internal());
                }
                if let Ok(text) =
                    tokio::fs::read_to_string(current.profile.join("DevToolsActivePort")).await
                {
                    let mut lines = text.lines();
                    if let (Some(port), Some(path)) = (
                        lines
                            .next()
                            .and_then(|p| p.parse::<u16>().ok())
                            .filter(|p| *p != 0),
                        lines.next().filter(|p| {
                            p.starts_with("/devtools/browser/")
                                && p.bytes()
                                    .all(|b| b.is_ascii_alphanumeric() || b"/-".contains(&b))
                        }),
                    ) {
                        return CdpRenderer::connect(format!("ws://127.0.0.1:{port}{path}"))
                            .await
                            .map(Arc::new)
                            .map_err(RenderError::into_ivy);
                    }
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        };
        let result = tokio::select! {
            _=self.0.shutdown.cancelled()=>Err(IvyError::internal()),
            result=tokio::time::timeout(Duration::from_secs(30),connect)=>result.unwrap_or_else(|_|Err(IvyError::internal())),
        };
        match result {
            Ok(renderer) => {
                let current = running.as_mut().unwrap();
                current.renderer = Some(renderer.clone());
                *self.0.observation.lock().unwrap() =
                    Some((renderer.clone(), current.process.completion.clone()));
                Ok(renderer)
            }
            Err(error) => {
                self.retire(running).await?;
                Err(error)
            }
        }
    }
    async fn retire(&self, running: &mut Option<Running>) -> IvyResult<()> {
        *self.0.observation.lock().unwrap() = None;
        let Some(current) = running.as_ref() else {
            return Ok(());
        };
        if let Some(renderer) = &current.renderer {
            let _ = tokio::time::timeout(Duration::from_secs(5), renderer.close()).await;
        }
        let result =
            match tokio::time::timeout(Duration::from_secs(2), current.process.completion.wait())
                .await
            {
                Ok(result) => result,
                Err(_) => current.process.stop(Duration::from_secs(2)).await?,
            };
        if result.error_code.is_some() {
            return Err(IvyError::new(
                "outcome_unknown",
                "Chromium retirement could not be confirmed.",
            )
            .with_outcome(ivy_contracts::Outcome::Unknown));
        }
        tokio::fs::remove_dir_all(&current.profile)
            .await
            .map_err(|_| IvyError::internal())?;
        *running = None;
        Ok(())
    }
}

fn browser_environment() -> BTreeMap<OsString, OsString> {
    // Preserve the service account's native font, locale and loader setup in
    // addition to the host runtime's standard environment. Never copy credentials.
    [
        "FONTCONFIG_FILE",
        "FONTCONFIG_PATH",
        "FONTCONFIG_SYSROOT",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_DATA_DIRS",
        "XDG_CONFIG_DIRS",
        "XDG_CACHE_HOME",
        "LD_LIBRARY_PATH",
        "DYLD_LIBRARY_PATH",
        "DYLD_FALLBACK_LIBRARY_PATH",
        "TZ",
        "LC_CTYPE",
    ]
    .into_iter()
    .filter_map(|key| std::env::var_os(key).map(|value| (key.into(), value)))
    .collect()
}

fn arguments(profile: &std::path::Path) -> Vec<OsString> {
    // Same sandboxed configuration used by the qualified chromiumoxide spike.
    // A fresh profile and an ephemeral loopback CDP port belong to this launch.
    let mut args: Vec<OsString> = [
        "--disable-background-networking",
        "--enable-features=NetworkService,NetworkServiceInProcess",
        "--disable-background-timer-throttling",
        "--disable-backgrounding-occluded-windows",
        "--disable-breakpad",
        "--disable-client-side-phishing-detection",
        "--disable-component-extensions-with-background-pages",
        "--disable-default-apps",
        "--disable-dev-shm-usage",
        "--disable-features=TranslateUI",
        "--disable-hang-monitor",
        "--disable-ipc-flooding-protection",
        "--disable-popup-blocking",
        "--disable-prompt-on-repost",
        "--disable-renderer-backgrounding",
        "--disable-sync",
        "--force-color-profile=srgb",
        "--metrics-recording-only",
        "--no-first-run",
        "--enable-automation",
        "--password-store=basic",
        "--use-mock-keychain",
        "--enable-blink-features=IdleDetection",
        "--lang=en_US",
        "--disable-extensions",
        "--headless",
        "--hide-scrollbars",
        "--mute-audio",
        "--remote-debugging-port=0",
        "--remote-debugging-address=127.0.0.1",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    let mut directory = OsString::from("--user-data-dir=");
    directory.push(profile);
    args.push(directory);
    args
}
