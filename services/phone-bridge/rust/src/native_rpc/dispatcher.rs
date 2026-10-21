//! The native dispatcher owns invocation and safe receipts independently of
//! pipe response waiters. Epoch loss is still resolved by the service journal.
use super::*;
use std::{future::Future, pin::Pin, sync::Mutex};

pub type NativeInvocation = Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'static>>;
pub type NativeInvocationStart = Pin<Box<dyn Future<Output = NativeInvocation> + Send + 'static>>;
pub trait NativeHandler: Send + Sync + 'static {
    /// Called under admission serialization after the stopping recheck.
    /// Construct owned work without blocking or re-entering the dispatcher.
    /// Validation/typed errors belong in the safe NativeOutcome; raw stack or
    /// protected request details must never become result/error diagnostics.
    fn invoke(&self, request: NativeRequest) -> NativeInvocation;
    /// Preserve the original synchronous invocation prefix under the same gate
    /// as receipt admission. Async Rust startup may suspend here; only the
    /// returned action runs after releasing that gate.
    fn start(&self, request: NativeRequest) -> NativeInvocationStart {
        let action = self.invoke(request);
        Box::pin(async move { action })
    }
    fn cleanup_slots(&self) -> usize;
}

#[derive(Clone, Debug, PartialEq)]
pub struct NativeReply {
    epoch: String,
    request_id: u64,
    outcome: NativeOutcome,
}
impl NativeReply {
    pub fn request_id(&self) -> u64 {
        self.request_id
    }
    pub fn outcome(&self) -> &NativeOutcome {
        &self.outcome
    }
    pub fn to_json(&self) -> Result<String, FrameError> {
        let quote = |value: &str| {
            ivy_contracts::json::stringify(&IvyJson::String(value.into()), FRAME_BYTES)
                .map_err(|_| FrameError::Oversized)
        };
        let result = self.outcome.result_json().unwrap_or("null");
        let error = match self.outcome.error_code() {
            Some(error) => quote(error)?,
            None => "null".into(),
        };
        let json = format!(
            "{{\"version\":1,\"epoch\":{},\"requestId\":{},\"ok\":{},\"result\":{result},\"error\":{error}}}",
            quote(&self.epoch)?,
            self.request_id,
            self.outcome.error_code().is_none()
        );
        if json.len() > FRAME_BYTES {
            return Err(FrameError::Oversized);
        }
        // The response envelope consumes a level too, as in the original
        // native serializer's MaxDepth=24 write boundary.
        parse_with_options(
            &json,
            FRAME_BYTES,
            ParseOptions {
                maximum_container_depth: Some(24),
                ..Default::default()
            },
        )
        .map_err(|_| FrameError::InvalidRequest)?;
        Ok(json)
    }
}
struct Shared {
    receipts: Mutex<NativeReceipts>,
    admission: Arc<tokio::sync::Mutex<()>>,
    epoch: String,
    handler: Arc<dyn NativeHandler>,
}
#[derive(Clone)]
pub struct NativeDispatcher(Arc<Shared>);
impl NativeDispatcher {
    pub fn new(
        epoch: String,
        capacity: usize,
        handler: Arc<dyn NativeHandler>,
    ) -> Result<Self, FrameError> {
        Ok(Self(Arc::new(Shared {
            receipts: Mutex::new(NativeReceipts::new(epoch.clone(), capacity)?),
            admission: Arc::new(tokio::sync::Mutex::new(())),
            epoch,
            handler,
        })))
    }
    pub async fn stop(&self) {
        let _gate = self.0.admission.lock().await;
        self.0.receipts.lock().unwrap().stop();
    }
    pub fn receipt_count(&self) -> usize {
        self.0.receipts.lock().unwrap().len()
    }
    pub async fn execute(&self, request: NativeRequest) -> NativeReply {
        self.begin(request).await.await
    }
    /// Admit before the pipe reads its next frame, independently of when its
    /// response observer is scheduled. Actual invocation still rechecks stop.
    pub(crate) async fn begin(
        &self,
        request: NativeRequest,
    ) -> impl Future<Output = NativeReply> + Send + use<> {
        let request_id = request.request_id();
        let receipt = match self.0.admission.clone().try_lock_owned() {
            Ok(_gate) => self.admit(request),
            Err(_) => {
                // Once entered, admission belongs to the dispatcher even when
                // its observer leaves while an original synchronous prefix runs.
                let owner = self.clone();
                tokio::spawn(async move {
                    let _gate = owner.0.admission.lock().await;
                    owner.admit(request)
                })
                .await
                .unwrap_or_else(|_| Err(NativeOutcome::error("operation_outcome_unknown")))
            }
        };
        let owner = self.clone();
        async move {
            let outcome = match receipt {
                Ok(receipt) => receipt.wait().await,
                Err(outcome) => outcome,
            };
            owner.reply(request_id, outcome)
        }
    }
    fn admit(&self, request: NativeRequest) -> Result<Arc<NativeReceipt>, NativeOutcome> {
        let admission = self
            .0
            .receipts
            .lock()
            .unwrap()
            .admit_with(&request, || self.0.handler.cleanup_slots());
        match admission {
            Admission::Reject(error) => Err(NativeOutcome::error(error)),
            Admission::Replay(receipt) => Ok(receipt),
            Admission::Dispatch(receipt) => {
                let receipt = receipt.unwrap_or_else(|| {
                    Arc::new(NativeReceipt {
                        hash: String::new(),
                        outcome: OnceLock::new(),
                        changed: Notify::new(),
                    })
                });
                let completed = receipt.clone();
                let owner = self.0.clone();
                tokio::spawn(async move {
                    // A supervisor observes panics without losing the saved
                    // receipt. It never aborts work when an IPC waiter leaves.
                    let invocation = tokio::spawn(async move {
                        tokio::task::yield_now().await;
                        let pending = {
                            let _gate = owner.admission.lock().await;
                            if !owner.receipts.lock().unwrap().may_invoke() {
                                return NativeOutcome::error("runtime_stopping");
                            }
                            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                owner.handler.start(request)
                            })) {
                                Ok(start) => Ok(start.await),
                                Err(error) => Err(error),
                            }
                        };
                        match pending {
                            Ok(pending) => pending.await,
                            Err(_) => NativeOutcome::error("operation_outcome_unknown"),
                        }
                    });
                    let outcome = invocation
                        .await
                        .unwrap_or_else(|_| NativeOutcome::error("operation_outcome_unknown"));
                    let _ = completed.complete(outcome);
                });
                Ok(receipt)
            }
        }
    }
    pub(crate) fn reply(&self, request_id: u64, outcome: NativeOutcome) -> NativeReply {
        NativeReply {
            epoch: self.0.epoch.clone(),
            request_id,
            outcome,
        }
    }
}
