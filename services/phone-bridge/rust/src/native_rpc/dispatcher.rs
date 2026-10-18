//! The native dispatcher owns invocation and safe receipts independently of
//! pipe response waiters. Epoch loss is still resolved by the service journal.
use super::*;
use std::{future::Future, pin::Pin, sync::Mutex};

pub type NativeInvocation = Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'static>>;
pub trait NativeHandler: Send + Sync + 'static {
    /// Called under admission serialization after the stopping recheck.
    /// Construct owned work without blocking or re-entering the dispatcher.
    /// Validation/typed errors belong in the safe NativeOutcome; raw stack or
    /// protected request details must never become result/error diagnostics.
    fn invoke(&self, request: NativeRequest) -> NativeInvocation;
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
            epoch,
            handler,
        })))
    }
    pub fn stop(&self) {
        self.0.receipts.lock().unwrap().stop();
    }
    pub fn receipt_count(&self) -> usize {
        self.0.receipts.lock().unwrap().len()
    }
    pub async fn execute(&self, request: NativeRequest) -> NativeReply {
        let request_id = request.request_id();
        let admission = self
            .0
            .receipts
            .lock()
            .unwrap()
            .admit_with(&request, || self.0.handler.cleanup_slots());
        let receipt = match admission {
            Admission::Reject(error) => return self.reply(request_id, NativeOutcome::error(error)),
            Admission::Replay(receipt) => receipt,
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
                            let state = owner.receipts.lock().unwrap();
                            if !state.may_invoke() {
                                return NativeOutcome::error("runtime_stopping");
                            }
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                owner.handler.invoke(request)
                            }))
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
                receipt
            }
        };
        self.reply(request_id, receipt.wait().await)
    }
    fn reply(&self, request_id: u64, outcome: NativeOutcome) -> NativeReply {
        NativeReply {
            epoch: self.0.epoch.clone(),
            request_id,
            outcome,
        }
    }
}
