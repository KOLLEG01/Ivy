//! A server transaction has one driver. Observers never cancel a stack send or
//! receive; replies and retirement complete on that original driver.
use super::CallError;
use rsipstack::{
    sip::{Header, Request, SipMessage, StatusCode},
    transaction::{Transaction, transaction::TransactionEvent},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{mpsc, oneshot, watch};

struct Reply {
    status: StatusCode,
    headers: Vec<Header>,
    body: Option<Vec<u8>>,
    done: oneshot::Sender<Result<(), CallError>>,
}
pub(super) struct ServerExchange {
    replies: mpsc::UnboundedSender<Reply>,
    events: mpsc::UnboundedReceiver<Request>,
    sender: rsipstack::transaction::transaction::TransactionEventSender,
    key: rsipstack::transaction::key::TransactionKey,
    stopping: Arc<AtomicBool>,
    done: watch::Receiver<Option<Result<(), CallError>>>,
}
impl ServerExchange {
    pub fn start(mut original: Transaction) -> Self {
        let sender = original.tu_sender.clone();
        let key = original.key.clone();
        let (replies, mut commands) = mpsc::unbounded_channel::<Reply>();
        let (events, received) = mpsc::unbounded_channel();
        let (finished, done) = watch::channel(None);
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = stopping.clone();
        tokio::spawn(async move {
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
                            let _ = events.send(request);
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
                finished.send_replace(Some(Err(CallError::CleanupUnconfirmed)));
                std::future::pending::<()>().await;
                drop(original);
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
            .map_err(|_| CallError::CleanupUnconfirmed)?;
        self.wake();
        wait.await.map_err(|_| CallError::CleanupUnconfirmed)?
    }
    pub async fn next(&mut self) -> Option<Request> {
        self.events.recv().await
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
