//! The original 500ms control callback skips ticks while one read is pending.
use ivy_contracts::JsString;
use ivy_sdk::service::ServiceFuture;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub type ControlRead = dyn Fn() -> ServiceFuture<bool> + Send + Sync;
pub type ControlStop = dyn Fn(JsString) + Send + Sync;
struct Inner {
    read: Arc<ControlRead>,
    stop: Arc<ControlStop>,
    closing: Arc<AtomicBool>,
    busy: AtomicBool,
}
#[derive(Clone)]
pub struct AgentControl(Arc<Inner>);
struct Reading(Arc<Inner>);
impl Drop for Reading {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::Release);
    }
}
impl AgentControl {
    pub fn new(read: Arc<ControlRead>, stop: Arc<ControlStop>, closing: Arc<AtomicBool>) -> Self {
        Self(Arc::new(Inner {
            read,
            stop,
            closing,
            busy: AtomicBool::new(false),
        }))
    }
    /// Invoke from the timer without awaiting accepted I/O. Timer retirement
    /// prevents new ticks; it does not cancel an already admitted control read.
    pub fn tick(&self) -> bool {
        if self.0.closing.load(Ordering::Acquire)
            || self
                .0
                .busy
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return false;
        }
        let reading = Reading(self.0.clone());
        let future = (self.0.read)();
        tokio::spawn(async move {
            let _reading = reading;
            match future.await {
                Ok(false) => {}
                Ok(true) => (_reading.0.stop)("agent_stopped".into()),
                Err(_) if !_reading.0.closing.load(Ordering::Acquire) => {
                    (_reading.0.stop)("agent_control_failed".into())
                }
                Err(_) => {}
            }
        });
        true
    }
}
