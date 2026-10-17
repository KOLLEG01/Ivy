use crate::{IvyError, IvyJson, IvyResult};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

struct Waiting {
    id: u64,
    provider: bool,
    sender: oneshot::Sender<RequestPermit>,
}
struct State {
    active: usize,
    providers: usize,
    sequence: u64,
    waiting: VecDeque<Waiting>,
}
pub struct RequestQueue {
    maximum: usize,
    provider_maximum: usize,
    waiting_maximum: usize,
    state: Mutex<State>,
}
pub struct RequestPermit {
    queue: Weak<RequestQueue>,
    provider: bool,
}
impl Drop for RequestPermit {
    fn drop(&mut self) {
        if let Some(queue) = self.queue.upgrade() {
            queue.release(self.provider);
        }
    }
}
struct WaitingGuard {
    queue: Arc<RequestQueue>,
    id: u64,
}
impl Drop for WaitingGuard {
    fn drop(&mut self) {
        self.queue
            .state
            .lock()
            .unwrap()
            .waiting
            .retain(|w| w.id != self.id);
    }
}
impl RequestQueue {
    pub fn new(
        maximum: usize,
        provider_maximum: usize,
        waiting_maximum: usize,
    ) -> IvyResult<Arc<Self>> {
        if maximum == 0
            || provider_maximum == 0
            || waiting_maximum == 0
            || provider_maximum > maximum
        {
            return Err(IvyError::new(
                "invalid_arguments",
                "Invalid client request admission limits.",
            ));
        }
        Ok(Arc::new(Self {
            maximum,
            provider_maximum,
            waiting_maximum,
            state: Mutex::new(State {
                active: 0,
                providers: 0,
                sequence: 0,
                waiting: VecDeque::new(),
            }),
        }))
    }
    pub async fn acquire(
        self: &Arc<Self>,
        provider: bool,
        cancel: &CancellationToken,
    ) -> IvyResult<RequestPermit> {
        if cancel.is_cancelled() {
            return Err(IvyError::new(
                "cancelled",
                "Request cancelled before sending.",
            ));
        }
        let (guard, receiver) = {
            let mut state = self.state.lock().unwrap();
            if state.active < self.maximum && (!provider || state.providers < self.provider_maximum)
            {
                state.active += 1;
                if provider {
                    state.providers += 1;
                }
                return Ok(RequestPermit {
                    queue: Arc::downgrade(self),
                    provider,
                });
            }
            if state.waiting.len() >= self.waiting_maximum {
                return Err(
                    IvyError::new("limit_exceeded", "Hive client request queue is full.")
                        .with_details(IvyJson::object([
                            ("budget", "client_queue".into()),
                            ("active", IvyJson::Number(state.waiting.len() as f64)),
                            ("limit", IvyJson::Number(self.waiting_maximum as f64)),
                        ])),
                );
            }
            state.sequence += 1;
            let id = state.sequence;
            let (sender, receiver) = oneshot::channel();
            state.waiting.push_back(Waiting {
                id,
                provider,
                sender,
            });
            (
                WaitingGuard {
                    queue: self.clone(),
                    id,
                },
                receiver,
            )
        };
        let result = tokio::select! {biased;_ = cancel.cancelled()=>Err(IvyError::new("cancelled","Request cancelled before sending.")),result=receiver=>result.map_err(|_|IvyError::internal())};
        drop(guard);
        result
    }
    fn release(self: &Arc<Self>, provider: bool) {
        // Sending permits outside the mutex is essential: a cancelled receiver drops
        // its permit synchronously, which must be able to release it again.
        let grants = {
            let mut state = self.state.lock().unwrap();
            state.active -= 1;
            if provider {
                state.providers -= 1;
            }
            let mut grants = Vec::new();
            while state.active < self.maximum {
                let Some(index) = state
                    .waiting
                    .iter()
                    .position(|w| !w.provider || state.providers < self.provider_maximum)
                else {
                    break;
                };
                let waiting = state.waiting.remove(index).unwrap();
                state.active += 1;
                if waiting.provider {
                    state.providers += 1;
                }
                grants.push((
                    waiting.sender,
                    RequestPermit {
                        queue: Arc::downgrade(self),
                        provider: waiting.provider,
                    },
                ));
            }
            grants
        };
        for (sender, permit) in grants {
            let _ = sender.send(permit);
        }
    }
}
