//! A server transaction has one driver. Observers never cancel a stack send or
//! receive; replies and retirement complete on that original driver.
use super::{
    CallError,
    queue::{self, Priority},
    retirement::Retirements,
};
use rsipstack::{
    sip::{Header, Method, Request, SipMessage, StatusCode},
    transaction::{Transaction, transaction::TransactionEvent},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::task::{Context, Poll};
use tokio::sync::{mpsc, oneshot, watch};

struct Reply {
    status: StatusCode,
    headers: Vec<Header>,
    body: Option<Vec<u8>>,
    done: oneshot::Sender<Result<(), CallError>>,
}
pub(super) struct ServerExchange {
    replies: mpsc::Sender<Reply>,
    events: queue::Receiver<Arc<Request>>,
    sender: rsipstack::transaction::transaction::TransactionEventSender,
    key: rsipstack::transaction::key::TransactionKey,
    stopping: Arc<AtomicBool>,
    done: watch::Receiver<Option<Result<(), CallError>>>,
}
impl ServerExchange {
    pub fn start(mut original: Transaction, retirements: Arc<Retirements>) -> Self {
        let sender = original.tu_sender.clone();
        let key = original.key.clone();
        let (replies, mut commands) = mpsc::channel::<Reply>(8);
        let (events, received) = queue::channel::<Arc<Request>>(8, 2);
        let (finished, done) = watch::channel(None);
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = stopping.clone();
        tokio::spawn(async move {
            let mut last_ordinary: Option<Arc<Request>> = None;
            let mut last_control: Option<Arc<Request>> = None;
            let result: Result<(), CallError> = async {
                loop {
                    while let Ok(reply) = commands.try_recv() {
                        let result = original
                            .reply_with(reply.status, reply.headers, reply.body)
                            .await
                            .map_err(|_| CallError::CleanupUnconfirmed);
                        let _ = reply.done.send(result);
                        result?;
                    }
                    if stop.load(Ordering::Acquire) || original.is_terminated() {
                        return Ok(());
                    }
                    match original.receive().await {
                        Some(SipMessage::Request(request)) => {
                            let (priority, last) = if request.method == Method::Cancel {
                                (Priority::Control, &mut last_control)
                            } else {
                                (Priority::Ordinary, &mut last_ordinary)
                            };
                            let request = Arc::new(request);
                            if events.try_send(request.clone(), priority).is_ok() {
                                *last = Some(request);
                            } else if events.is_closed(priority)
                                || !last.as_ref().is_some_and(|prior| prior == &request)
                            {
                                // Only an exact retransmission of the newest
                                // queued request can coalesce on saturation.
                                // Preserve a distinct overflow as uncertainty;
                                // never wait here and deadlock an original reply.
                                return Err(CallError::CleanupUnconfirmed);
                            }
                        }
                        Some(_) => (),
                        None if stop.load(Ordering::Acquire) || original.is_terminated() => {
                            return Ok(());
                        }
                        None => (), // A queued reply wakes receive through the TU.
                    }
                }
            }
            .await;
            if result.is_ok() {
                drop(original);
                finished.send_replace(Some(Ok(())));
            } else {
                retirements.dialog_failed(original);
                finished.send_replace(Some(Err(CallError::CleanupUnconfirmed)));
            }
        });
        Self {
            replies,
            events: received,
            sender,
            key,
            stopping,
            done,
        }
    }
    fn wake(&self) {
        let _ = self
            .sender
            .send(TransactionEvent::Terminate(self.key.clone()));
    }
    pub async fn reply(
        &self,
        status: StatusCode,
        headers: Vec<Header>,
        body: Option<Vec<u8>>,
    ) -> Result<(), CallError> {
        let (done, wait) = oneshot::channel();
        self.replies
            .send(Reply {
                status,
                headers,
                body,
                done,
            })
            .await
            .map_err(|_| CallError::CleanupUnconfirmed)?;
        self.wake();
        wait.await.map_err(|_| CallError::CleanupUnconfirmed)?
    }
    pub async fn next(&mut self) -> Option<Arc<Request>> {
        self.events.recv().await
    }
    pub fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Option<Arc<Request>>> {
        self.events.poll_recv(cx)
    }
    pub async fn close(&self) -> Result<(), CallError> {
        self.stopping.store(true, Ordering::Release);
        self.wake();
        let mut done = self.done.clone();
        loop {
            if let Some(result) = *done.borrow_and_update() {
                return result;
            }
            done.changed()
                .await
                .map_err(|_| CallError::CleanupUnconfirmed)?;
        }
    }
}
impl Drop for ServerExchange {
    fn drop(&mut self) {
        // Wake the original driver; dropping this observer never drops an
        // in-flight stack receive/send future or leaves a new orphan driver.
        self.stopping.store(true, Ordering::Release);
        self.wake();
    }
}
