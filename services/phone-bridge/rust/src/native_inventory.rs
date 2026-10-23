//! Read-only native inventory. Each admitted request owns one blocking read;
//! neither observer loss nor shutdown cancels an already admitted enumeration.
use crate::{
    media::Codec,
    native_desktop::native_result_json,
    native_pipe::{NativeRuntime, NativeShutdown, NativeShutdownError},
    native_rpc::{
        NativeHandler, NativeInvocation, NativeInvocationStart, NativeOutcome, NativeRequest,
    },
    native_sip::{NativeSipOperation, NativeSipOperations, retained},
    sip::SipHostError,
};
use ivy_contracts::IvyJson;
use std::sync::{Arc, Mutex};
#[cfg(all(windows, target_pointer_width = "64"))]
#[path = "native_inventory_windows.rs"]
mod windows;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InventoryError;
type Reader = dyn Fn() -> Result<IvyJson, InventoryError> + Send + Sync;
pub struct NativeInventoryCommands {
    sip: NativeSipOperations,
    remaining: Arc<dyn NativeRuntime>,
    reader: Arc<Reader>,
    closed: Mutex<bool>,
    closing: Mutex<Option<NativeSipOperation<()>>>,
}
impl NativeInventoryCommands {
    #[cfg(all(windows, target_pointer_width = "64"))]
    pub fn windows(sip: NativeSipOperations, remaining: Arc<dyn NativeRuntime>) -> Self {
        Self::with_reader(sip, remaining, Self::read_windows)
    }
    #[cfg(all(windows, target_pointer_width = "64"))]
    pub(crate) fn read_windows() -> Result<IvyJson, InventoryError> {
        windows::read()
    }
    pub fn with_reader(
        sip: NativeSipOperations,
        remaining: Arc<dyn NativeRuntime>,
        reader: impl Fn() -> Result<IvyJson, InventoryError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            sip,
            remaining,
            reader: Arc::new(reader),
            closed: Mutex::new(false),
            closing: Mutex::new(None),
        }
    }
    fn read(&self, request: NativeRequest) -> NativeInvocation {
        if !request.params().as_object().is_some_and(|p| p.is_empty()) {
            return Box::pin(async { NativeOutcome::error("invalid_request") });
        }
        let closed = self.closed.lock().unwrap();
        if *closed {
            return Box::pin(async { NativeOutcome::error("runtime_stopping") });
        }
        let work = self.sip.with_running_diagnostic(|| {
            let reader = self.reader.clone();
            tokio::task::spawn_blocking(move || {
                reader().and_then(|v| native_result_json(&v).map_err(|_| InventoryError))
            })
        });
        let work = match work {
            Ok(work) => work,
            Err(_) => return Box::pin(async { NativeOutcome::error("runtime_stopping") }),
        };
        Box::pin(async move {
            match work.await {
                Ok(Ok(json)) => NativeOutcome::result(json),
                _ => NativeOutcome::error("operation_outcome_unknown"),
            }
        })
    }
}
impl NativeHandler for NativeInventoryCommands {
    fn cleanup_slots(&self) -> usize {
        self.remaining.cleanup_slots()
    }
    fn start(&self, request: NativeRequest) -> NativeInvocationStart {
        if request.method() != "inventory" {
            return self.remaining.start(request);
        }
        let work = self.read(request);
        Box::pin(async move { work })
    }
    fn invoke(&self, request: NativeRequest) -> NativeInvocation {
        if request.method() == "inventory" {
            self.read(request)
        } else {
            self.remaining.invoke(request)
        }
    }
}
impl NativeRuntime for NativeInventoryCommands {
    fn shutdown(&self) -> NativeShutdown {
        *self.closed.lock().unwrap() = true;
        let mut saved = self.closing.lock().unwrap();
        let work = saved
            .get_or_insert_with(|| {
                let remaining = self.remaining.clone();
                retained(async move {
                    remaining
                        .shutdown()
                        .await
                        .map_err(|_| SipHostError::CleanupUnconfirmed)
                })
            })
            .clone();
        Box::pin(async move {
            work.wait()
                .await
                .map_err(|_| NativeShutdownError::OutcomeUnknown)
        })
    }
}
fn inventory(devices: Vec<IvyJson>, loopback: bool) -> IvyJson {
    let codecs = [
        Codec::G722,
        Codec::Pcma,
        Codec::Pcmu,
        Codec::Opus,
        Codec::Evs,
    ]
    .into_iter()
    .flat_map(Codec::offered_formats)
    .map(|f| {
        IvyJson::object([
            ("name", f.name().into()),
            ("payload", IvyJson::Number(f.payload() as f64)),
            ("pcmRate", IvyJson::Number(f.pcm_rate() as f64)),
            ("rtpClockRate", IvyJson::Number(f.rtp_rate() as f64)),
            ("channels", IvyJson::Number(f.channels() as f64)),
        ])
    })
    .collect();
    IvyJson::object([
        ("devices", IvyJson::Array(devices)),
        ("codecs", IvyJson::Array(codecs)),
        ("processLoopbackSupported", loopback.into()),
    ])
}
