use ivy_contracts::{IvyError, IvyJson, IvyResult};
use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{runtime::Handle, sync::oneshot, task::AbortHandle, time::Instant};

trait Work: Send {
    fn start(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>>;
    fn reject(self: Box<Self>, error: IvyError);
}
struct Job<F, T> {
    work: F,
    result: oneshot::Sender<IvyResult<T>>,
}
impl<F, Fut, T> Work for Job<F, T>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = IvyResult<T>> + Send + 'static,
    T: Send + 'static,
{
    fn start(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            let result = (self.work)().await;
            let _ = self.result.send(result);
        })
    }
    fn reject(self: Box<Self>, error: IvyError) {
        let _ = self.result.send(Err(error));
    }
}
struct Pending {
    work: Box<dyn Work>,
    runtime: Handle,
}
struct Waiting {
    key: Arc<()>,
    pending: Pending,
    timer: AbortHandle,
}
#[derive(Default)]
struct State {
    active: bool,
    waiting: VecDeque<Waiting>,
}
struct Gate {
    maximum: usize,
    wait: Duration,
    state: Mutex<State>,
}

/// Eager slot admission and one active asynchronous dispatch. The current Tokio
/// runtime owns accepted work; dropping its result receiver does not cancel it.
/// A closed receiver reports executor loss separately from a returned IvyError.
#[derive(Clone)]
pub struct DispatchGate(Arc<Gate>);
impl Default for DispatchGate {
    fn default() -> Self {
        Self::new(1024, 30_000)
    }
}
impl DispatchGate {
    pub fn new(maximum: usize, wait_ms: u64) -> Self {
        Self(Arc::new(Gate {
            maximum,
            wait: Duration::from_millis(wait_ms),
            state: Mutex::new(State::default()),
        }))
    }
    pub fn run<F, Fut, T>(&self, work: F) -> oneshot::Receiver<IvyResult<T>>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = IvyResult<T>> + Send + 'static,
        T: Send + 'static,
    {
        let (result, receiver) = oneshot::channel();
        let mut state = self.0.state.lock().unwrap();
        let count = state.waiting.len() + usize::from(state.active);
        if count >= self.0.maximum {
            let _ = result.send(Err(IvyError::new(
                "limit_exceeded",
                "Hive dispatch slots are full.",
            )
            .with_details(IvyJson::object([
                ("budget", "dispatch".into()),
                ("active", IvyJson::Number(count as f64)),
                ("limit", IvyJson::Number(self.0.maximum as f64)),
            ]))));
            return receiver;
        }
        let pending = Pending {
            work: Box::new(Job { work, result }),
            runtime: Handle::current(),
        };
        if state.active {
            let key = Arc::new(());
            let timer_key = key.clone();
            let gate = Arc::downgrade(&self.0);
            let deadline = Instant::now() + self.0.wait;
            let timer = pending.runtime.spawn(async move {
                tokio::time::sleep_until(deadline).await;
                let Some(gate) = gate.upgrade() else {
                    return;
                };
                let expired = {
                    let mut state = gate.state.lock().unwrap();
                    state
                        .waiting
                        .iter()
                        .position(|entry| Arc::ptr_eq(&entry.key, &timer_key))
                        .and_then(|index| state.waiting.remove(index))
                };
                if let Some(entry) = expired {
                    entry.pending.work.reject(IvyError::new(
                        "deadline_exceeded",
                        "Hive dispatch wait expired before execution.",
                    ));
                }
            });
            state.waiting.push_back(Waiting {
                key,
                pending,
                timer: timer.abort_handle(),
            });
        } else {
            state.active = true;
            drop(state);
            self.start(pending);
        }
        receiver
    }
    fn start(&self, pending: Pending) {
        let completion = Completion(self.clone());
        pending.runtime.spawn(async move {
            let _completion = completion;
            pending.work.start().await;
        });
    }
    fn finish(&self) {
        let next = {
            let mut state = self.0.state.lock().unwrap();
            let next = state.waiting.pop_front();
            state.active = next.is_some();
            next
        };
        if let Some(entry) = next {
            entry.timer.abort();
            self.start(entry.pending);
        }
    }
}
// Release also runs while unwinding a native task, so a coding failure cannot
// leave the gate occupied forever. No user work or capture is dropped under its lock.
struct Completion(DispatchGate);
impl Drop for Completion {
    fn drop(&mut self) {
        self.0.finish();
    }
}
