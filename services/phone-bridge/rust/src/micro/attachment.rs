//! Resource supervision for one installed Micro endpoint. Discovery and recovery
//! never replay microphone input; the call's existing RPC ledger owns it.
use super::{
    BUS_ID, MicroAttachmentStatus, MicroClose, MicroDiagnostic, MicroInputReceipt,
    MicroInputResult, MicroUsbDevice, MicroVoiceInput, MicroVoiceObservation, SERIAL, own_port,
};
use crate::native_config::NativeMicroSettings;
use std::{
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicroClientResult {
    pub started: bool,
    pub completed: bool,
    pub exit_code: Option<i32>,
    pub output: String,
    pub error_code: Option<String>,
}
pub enum MicroCommandError {
    Cancelled,
    Failed(Box<dyn std::error::Error + Send + Sync>),
}
pub type MicroCommand =
    Pin<Box<dyn Future<Output = Result<MicroClientResult, MicroCommandError>> + Send>>;
type Execute = dyn Fn(Vec<String>, CancellationToken) -> MicroCommand + Send + Sync;
struct State {
    device: MicroUsbDevice,
    observed: MicroAttachmentStatus,
    monitoring: Option<MicroClose>,
    disposal: Option<MicroClose>,
}
struct Owner {
    port: u16,
    diagnostic: Option<MicroDiagnostic>,
    execute: Arc<Execute>,
    stop: CancellationToken,
    state: Mutex<State>,
}
#[derive(Clone)]
pub struct MicroAttachment(Arc<Owner>);
impl MicroAttachment {
    #[cfg(windows)]
    pub fn new(
        settings: &NativeMicroSettings,
        diagnostic: Option<MicroDiagnostic>,
    ) -> MicroInputResult<Self> {
        let client = super::MicroUsbClient::new(settings.clone());
        Self::with_executor(
            settings,
            move |args, stop| client.run(args, stop),
            diagnostic,
        )
    }
    /// The executor is the installed-client boundary, as in the original
    /// constructor overload. Device creation and the first inventory invocation
    /// precede return; command completion remains monitor-owned.
    pub fn with_executor(
        settings: &NativeMicroSettings,
        execute: impl Fn(Vec<String>, CancellationToken) -> MicroCommand + Send + Sync + 'static,
        diagnostic: Option<MicroDiagnostic>,
    ) -> MicroInputResult<Self> {
        let device = MicroUsbDevice::new(settings.port().into(), 0, diagnostic.clone())?;
        let attachment = Self(Arc::new(Owner {
            port: settings.port(),
            diagnostic,
            execute: Arc::new(execute),
            stop: CancellationToken::new(),
            state: Mutex::new(State {
                device,
                observed: status("starting", None),
                monitoring: None,
                disposal: None,
            }),
        }));
        let owner = attachment.clone();
        let observed = attachment.clone();
        let mut monitor = Box::pin(async move { owner.monitor().await });
        let mut work = Box::pin(async move {
            // The original outer monitor catch records transport failure and
            // completes normally; it does not fault the disposal task.
            let result = std::future::poll_fn(|cx| {
                match catch_unwind(AssertUnwindSafe(|| monitor.as_mut().poll(cx))) {
                    Ok(value) => value.map(|_| true),
                    Err(_) => Poll::Ready(false),
                }
            })
            .await;
            if !result {
                observed.set_state("unavailable", Some("micro_transport_failed"));
            }
            Ok(())
        });
        // C# async methods run through already completed awaits before returning.
        // Retain that constructor prefix; the same pending future is then owned
        // by MicroClose, with its waker replaced on the next runtime poll.
        let monitoring = match work.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(result) => MicroClose::completed(result),
            Poll::Pending => MicroClose::start(work)?,
        };
        attachment.state().monitoring = Some(monitoring);
        Ok(attachment)
    }
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn device(&self) -> MicroUsbDevice {
        self.state().device.clone()
    }
    fn set_state(&self, state: &str, code: Option<&str>) {
        self.state().observed = status(state, code);
    }
    fn command(&self, args: Vec<String>) -> MicroCommand {
        match catch_unwind(AssertUnwindSafe(|| {
            (self.0.execute)(args, self.0.stop.clone())
        })) {
            Ok(work) => work,
            Err(_) => Box::pin(async { Err(failed()) }),
        }
    }
    async fn execute(
        &self,
        mut work: MicroCommand,
    ) -> Result<MicroClientResult, MicroCommandError> {
        std::future::poll_fn(
            |cx| match catch_unwind(AssertUnwindSafe(|| work.as_mut().poll(cx))) {
                Ok(value) => value,
                Err(_) => Poll::Ready(Err(failed())),
            },
        )
        .await
    }
    async fn pause(&self, milliseconds: u64) -> bool {
        tokio::select! {
            biased;
            _ = self.0.stop.cancelled() => false,
            _ = tokio::time::sleep(Duration::from_millis(milliseconds)) => true,
        }
    }
    async fn backoff(&self, delay: &mut u64) -> bool {
        if !self.pause(*delay).await {
            return false;
        }
        *delay = (*delay * 2).min(30000);
        true
    }
    async fn monitor(&self) {
        let mut unknown = false;
        let mut delay = 1000;
        while !self.0.stop.is_cancelled() {
            let previous = self.device();
            if !previous.healthy() {
                self.set_state("unavailable", Some("micro_transport_failed"));
                let generation = previous.last_generation();
                if let Ok(work) = previous.disposal() {
                    let _ = work.wait().await;
                }
                if self.0.stop.is_cancelled() {
                    break;
                }
                match MicroUsbDevice::new(self.0.port.into(), generation, self.0.diagnostic.clone())
                {
                    Ok(device) => {
                        self.state().device = device;
                        self.set_state("starting", None);
                        delay = 1000;
                    }
                    Err(_) => {
                        self.set_state("unavailable", Some("micro_transport_failed"));
                        if !self.backoff(&mut delay).await {
                            break;
                        }
                        continue;
                    }
                }
            }
            let inventory = self.execute(self.command(vec!["port".into()])).await;
            let inventory = match inventory {
                Ok(inventory) => inventory,
                Err(MicroCommandError::Cancelled) if self.0.stop.is_cancelled() => break,
                Err(_) => {
                    self.set_state(
                        if unknown {
                            "outcome_unknown"
                        } else {
                            "unavailable"
                        },
                        Some(if unknown {
                            "micro_attach_unknown"
                        } else {
                            "micro_query_failed"
                        }),
                    );
                    if !self.backoff(&mut delay).await {
                        break;
                    }
                    continue;
                }
            };
            if !inventory.completed || inventory.exit_code != Some(0) {
                self.set_state(
                    if unknown {
                        "outcome_unknown"
                    } else {
                        "unavailable"
                    },
                    Some(if unknown {
                        "micro_attach_unknown"
                    } else {
                        inventory
                            .error_code
                            .as_deref()
                            .unwrap_or("micro_query_failed")
                    }),
                );
            } else {
                let existing = match own_port(Some(&inventory.output), self.0.port.into()) {
                    Ok(port) => port,
                    Err(_) => {
                        self.set_state("unavailable", Some("micro_attachment_conflict"));
                        if !self.backoff(&mut delay).await {
                            break;
                        }
                        continue;
                    }
                };
                if existing.is_some() {
                    unknown = false;
                    self.set_state("attached", None);
                    delay = 1000;
                    if !self.pause(15000).await {
                        break;
                    }
                    continue;
                }
                if unknown {
                    self.set_state("outcome_unknown", Some("micro_attach_unknown"));
                } else {
                    // A successful ordinary attach cancels an old driver reattach
                    // request. --once would accumulate it across native rollouts.
                    let args = vec![
                        "--tcp-port".into(),
                        self.0.port.to_string(),
                        "attach".into(),
                        "--remote".into(),
                        "127.0.0.1".into(),
                        "--bus-id".into(),
                        BUS_ID.into(),
                        "--serial".into(),
                        SERIAL.into(),
                        "--receive-mode".into(),
                        "low-latency".into(),
                    ];
                    let attached = match self.execute(self.command(args)).await {
                        Ok(attached) => attached,
                        Err(MicroCommandError::Cancelled) if self.0.stop.is_cancelled() => break,
                        Err(_) => {
                            unknown = true;
                            self.set_state("outcome_unknown", Some("micro_attach_unknown"));
                            if !self.backoff(&mut delay).await {
                                break;
                            }
                            continue;
                        }
                    };
                    unknown = attached.started && !attached.completed;
                    self.set_state(
                        if unknown {
                            "outcome_unknown"
                        } else {
                            "starting"
                        },
                        if unknown {
                            Some("micro_attach_unknown")
                        } else if attached.completed && attached.exit_code == Some(0) {
                            None
                        } else {
                            Some(
                                attached
                                    .error_code
                                    .as_deref()
                                    .unwrap_or("micro_attach_failed"),
                            )
                        },
                    );
                }
            }
            if !self.backoff(&mut delay).await {
                break;
            }
        }
    }
    pub fn attachment_status(&self) -> MicroAttachmentStatus {
        let value = self.state().observed.clone();
        let device = self.device();
        if matches!(value.state.as_str(), "attached" | "ready") {
            let generation = device.ready_generation();
            if !device.healthy() {
                status("unavailable", Some("micro_transport_failed"))
            } else {
                MicroAttachmentStatus {
                    state: if generation.is_some() {
                        "ready"
                    } else {
                        "attached"
                    }
                    .into(),
                    generation,
                    error_code: None,
                }
            }
        } else {
            value
        }
    }
    pub fn disposal(&self) -> MicroInputResult<MicroClose> {
        let mut state = self.state();
        if let Some(work) = &state.disposal {
            return Ok(work.clone());
        }
        self.0.stop.cancel();
        let monitoring = state
            .monitoring
            .clone()
            .expect("Started attachment monitor");
        let attachment = self.clone();
        let work = MicroClose::start(async move {
            let outcome = async {
                monitoring.wait().await?;
                let device = attachment.device();
                let disposal = device.disposal().map_err(Arc::from)?;
                disposal.wait().await
            }
            .await;
            if outcome.is_ok() {
                attachment.set_state("closed", None);
            } else {
                attachment.set_state("outcome_unknown", Some("micro_cleanup_unknown"));
            }
            outcome
        })?;
        state.disposal = Some(work.clone());
        Ok(work)
    }
}
impl MicroVoiceInput for MicroAttachment {
    fn close(&self) -> MicroInputResult<MicroClose> {
        self.disposal()
    }
    fn status(&self) -> MicroInputResult<MicroAttachmentStatus> {
        Ok(self.attachment_status())
    }
    fn send_microphone(
        &self,
        pressed: bool,
        generation: i64,
        original: &dyn Fn() -> MicroInputResult<bool>,
    ) -> MicroInputResult<MicroInputReceipt> {
        if self.0.stop.is_cancelled() || self.attachment_status().state != "ready" {
            return Ok(MicroInputReceipt {
                phase: "not_submitted".into(),
                pressed,
                generation,
            });
        }
        Ok(self.device().send_microphone(pressed, generation, &|| {
            Ok(!self.0.stop.is_cancelled() && original()?)
        }))
    }
    fn observe_voice(&self, generation: i64) -> MicroInputResult<MicroVoiceObservation> {
        Ok(self.device().observe_voice(generation))
    }
}
fn status(state: &str, code: Option<&str>) -> MicroAttachmentStatus {
    MicroAttachmentStatus {
        state: state.into(),
        generation: None,
        error_code: code.map(str::to_owned),
    }
}
fn failed() -> MicroCommandError {
    MicroCommandError::Failed(std::io::Error::other("Micro client execution failed.").into())
}

#[cfg(all(test, windows))]
#[path = "../../../../../tests/rust/phone_micro_attachment.rs"]
mod tests;
