//! Original ringing INVITE ownership, independent of native command waiters.
use super::{CallError, CallObservation, CallOutcome, FinalPhase, SipHostCall, SipHostError};
use rsipstack::{
    sip::{Method, SipMessage, StatusCode, prelude::HeadersExt},
    transaction::{Transaction, transaction::TransactionEvent},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

struct Stop {
    requested: AtomicBool,
    sender: rsipstack::transaction::transaction::TransactionEventSender,
    key: rsipstack::transaction::key::TransactionKey,
}
impl Stop {
    fn request(&self) {
        self.requested.store(true, Ordering::Release);
        let _ = self
            .sender
            .send(TransactionEvent::Terminate(self.key.clone()));
    }
}
#[derive(Clone)]
pub(super) struct IncomingInvite {
    stop: Arc<Stop>,
    observation: watch::Receiver<CallObservation>,
    done: watch::Receiver<Option<Result<(), CallError>>>,
}
impl IncomingInvite {
    pub fn start(original: SipHostCall, mut transaction: Transaction, ring_seconds: u8) -> Self {
        let stop = Arc::new(Stop {
            requested: AtomicBool::new(false),
            sender: transaction.tu_sender.clone(),
            key: transaction.key.clone(),
        });
        let initial = CallObservation {
            outcome: None,
            phase: FinalPhase::Waiting,
            active: false,
            ending: false,
            uncertain: false,
            cleanup_confirmed: false,
            quiescent: false,
            closed: false,
        };
        let (observations, observation) = watch::channel(initial);
        let (finished, done) = watch::channel(None);
        let stopping = stop.clone();
        tokio::spawn(async move {
            let ring_stop = CancellationToken::new();
            let mut timer = None;
            let result = async {
                match original.constructed().await {
                    Ok(()) => (),
                    Err(SipHostError::CleanupUnconfirmed) => return Err(CallError::CleanupUnconfirmed),
                    Err(_) => return transaction.reply(StatusCode::Forbidden).await.map_err(|_| CallError::CleanupUnconfirmed),
                }
                let to = transaction.original.to_header().map_err(|_| CallError::CleanupUnconfirmed)?.clone();
                if to.tag().map_err(|_| CallError::CleanupUnconfirmed)?.is_none() {
                    transaction.original.headers.unique_push(to.with_tag(rsipstack::transaction::make_tag()).into());
                }
                if stopping.requested.load(Ordering::Acquire) {
                    return transaction.reply(StatusCode::Decline).await.map_err(|_| CallError::CleanupUnconfirmed);
                }
                transaction.reply(StatusCode::Ringing).await.map_err(|_| CallError::CleanupUnconfirmed)?;
                let expire = stopping.clone();
                let cancelled = ring_stop.clone();
                timer = Some(tokio::spawn(async move {
                    tokio::select! {
                        _ = cancelled.cancelled() => (),
                        _ = tokio::time::sleep(std::time::Duration::from_secs(ring_seconds.into())) => expire.request(),
                    }
                }));
                // receive/respond are not cancellation-safe. Only this owner
                // drives them, and a local stop enters through the TU channel.
                loop {
                    match transaction.receive().await {
                        Some(SipMessage::Request(request)) if request.method == Method::Cancel => {
                            return transaction.reply(StatusCode::RequestTerminated).await.map_err(|_| CallError::CleanupUnconfirmed);
                        }
                        Some(_) => (),
                        None if stopping.requested.load(Ordering::Acquire) => {
                            return transaction.reply(StatusCode::Decline).await.map_err(|_| CallError::CleanupUnconfirmed);
                        }
                        None => return Err(CallError::CleanupUnconfirmed),
                    }
                }
            }.await;
            ring_stop.cancel();
            if let Some(timer) = timer {
                let _ = timer.await;
            }
            let mut ended = initial;
            ended.ending = true;
            ended.outcome = Some(if result.is_ok() {
                CallOutcome::Ended
            } else {
                CallOutcome::Unknown
            });
            ended.phase = FinalPhase::Refused;
            ended.uncertain = result.is_err();
            ended.cleanup_confirmed = result.is_ok();
            ended.quiescent = result.is_ok();
            ended.closed = result.is_ok();
            if result.is_ok() {
                // Final non-success responses remain in the stack's finished
                // cache. Drop the original timers/route/socket refs first.
                drop(transaction);
                drop(original);
                observations.send_replace(ended);
                finished.send_replace(Some(Ok(())));
            } else {
                observations.send_replace(ended);
                finished.send_replace(Some(Err(CallError::CleanupUnconfirmed)));
                // A failed original must not silently release its binding.
                std::future::pending::<()>().await;
                drop((transaction, original));
            }
        });
        Self {
            stop,
            observation,
            done,
        }
    }
    pub fn request_hangup(&self) {
        self.stop.request();
    }
    pub fn observation(&self) -> CallObservation {
        let mut current = *self.observation.borrow();
        if self.done.has_changed().is_err() && self.done.borrow().is_none() {
            current.uncertain = true;
            current.ending = true;
            current.outcome = Some(CallOutcome::Unknown);
        }
        current
    }
    pub fn observations(&self) -> watch::Receiver<CallObservation> {
        self.observation.clone()
    }
    pub async fn close(&self) -> Result<(), CallError> {
        self.request_hangup();
        let mut done = self.done.clone();
        tokio::time::timeout(std::time::Duration::from_secs(35), async {
            loop {
                if let Some(result) = *done.borrow_and_update() {
                    return result;
                }
                done.changed()
                    .await
                    .map_err(|_| CallError::CleanupUnconfirmed)?;
            }
        })
        .await
        .map_err(|_| CallError::CleanupUnconfirmed)?
    }
}
