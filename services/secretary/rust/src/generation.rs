//! One reconciled journal generation. All borrowed domain futures stay on this
//! thread and are joined before the caller may close or replace its journal.
use crate::{
    assignment_driver::{AssignmentDriver, AssignmentRunnerHandle},
    assignment_progress::SchedulerAssignments,
    assignment_scheduler::AssignmentScheduler,
    follow_up::SecretaryFollowUp,
    inbox_recovery::RecoveryIssues,
    notice_scan::NoticeScan,
    notices::SecretaryNotices,
    service_handlers::{Invocation, SecretaryHandlers, invoke},
};
use ivy_contracts::{IvyError, IvyResult, JsString};
use ivy_sdk::service::{ServiceConnection, ServiceState};
use std::{
    cell::{Cell, RefCell},
    future::{Future, pending, poll_fn},
    pin::Pin,
    task::Poll,
    time::Duration,
};
use tokio::{sync::mpsc, time::Instant};

pub enum ServiceEvent {
    Reconcile(ServiceConnection),
    State(ServiceState),
    Stop,
}
/// A consumed replacement/stop event is returned to the service owner after all
/// old work has settled. Other events remain queued in their original order.
pub struct GenerationExit {
    pub next: Option<ServiceEvent>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkName {
    Foreground,
    FollowUps,
    Notices,
    Invocation,
}
impl WorkName {
    fn issue(self) -> Option<&'static str> {
        match self {
            Self::Foreground => Some("service-tick"),
            Self::FollowUps => Some("follow-ups"),
            Self::Notices => Some("notices"),
            Self::Invocation => None,
        }
    }
}
struct Work<'a> {
    name: WorkName,
    future: Pin<Box<dyn Future<Output = IvyResult<()>> + 'a>>,
}
async fn completion(work: &mut Vec<Work<'_>>) -> (WorkName, IvyResult<()>) {
    poll_fn(|cx| {
        for index in 0..work.len() {
            if let Poll::Ready(result) = work[index].future.as_mut().poll(cx) {
                let done = work.remove(index);
                return Poll::Ready((done.name, result));
            }
        }
        Poll::Pending
    })
    .await
}
fn diagnostic(error: &IvyError, stage: &str) {
    eprintln!("{}", serde_json::json!({"code":error.code,"stage":stage}));
}

pub struct ReconciledGeneration<'a, 'engine, 'journal> {
    pub connection: ServiceConnection,
    pub handlers: SecretaryHandlers<'a, 'engine, 'journal>,
    pub scheduler: AssignmentScheduler<'a, 'engine, 'journal>,
    pub runner: AssignmentRunnerHandle,
    pub minimum_message_age_minutes: f64,
}
impl ReconciledGeneration<'_, '_, '_> {
    /// Initialization and scheduler initialization precede this call. The SDK's
    /// ready state starts work; successful mutations enqueue subsequent kicks.
    pub async fn run(
        &self,
        driver: AssignmentDriver<'_, '_, '_, '_>,
        invocations: &mut mpsc::UnboundedReceiver<Invocation>,
        events: &mut mpsc::UnboundedReceiver<ServiceEvent>,
        mut observe_issue: impl FnMut(Option<JsString>),
    ) -> GenerationExit {
        let engine = self.handlers.engine;
        let issues = engine.issues;
        let signal = self.connection.signal();
        let poll_ms = engine
            .evidence
            .settings
            .required("pollMs")
            .and_then(|v| v.as_f64().ok_or_else(IvyError::internal))
            .expect("Validated Secretary poll interval") as u64;
        let limit = engine
            .evidence
            .settings
            .required("recordsPerTick")
            .and_then(|v| v.as_f64().ok_or_else(IvyError::internal))
            .expect("Validated Secretary page size") as usize;
        let follow_up =
            SecretaryFollowUp::new(engine, self.handlers.tools.clone(), self.handlers.retained);
        let scans = RefCell::new(Some(SchedulerAssignments::default()));
        let notices = RefCell::new(Some(NoticeScan::default()));
        let stage = Cell::new("idle");
        let (wake, mut wakes) = mpsc::unbounded_channel::<()>();
        let mut work = Vec::<Work<'_>>::new();
        let mut deadline = None;
        let mut kick = false;
        let mut wake_pending = false;
        let mut last_ready = None;
        let mut closing = false;
        let mut next = None;
        let mut invocation_open = true;
        let mut event_open = true;
        let driver = driver.run();
        tokio::pin!(driver);
        let mut driver_done = false;
        let mut runner_closing = false;
        let mut drain: Option<Pin<Box<dyn Future<Output = ()>>>> = None;
        loop {
            if signal.is_cancelled() {
                closing = true;
            }
            let active = work.iter().any(|w| w.name == WorkName::Foreground);
            if closing {
                deadline = None;
                if !active && !runner_closing {
                    drain = Some(Box::pin(self.runner.close()));
                    runner_closing = true;
                }
                if work.is_empty() && driver_done && drain.is_none() {
                    return GenerationExit { next };
                }
            } else if kick {
                kick = false;
                if !work.iter().any(|w| w.name == WorkName::FollowUps) {
                    work.push(Work {
                        name: WorkName::FollowUps,
                        future: Box::pin(follow_up.tick()),
                    });
                }
                if !work.iter().any(|w| w.name == WorkName::Notices) {
                    let mut scan = notices
                        .borrow_mut()
                        .take()
                        .expect("One notice worker per generation");
                    let notices = &notices;
                    let signal = signal.clone();
                    work.push(Work {
                        name: WorkName::Notices,
                        future: Box::pin(async move {
                            let result = async {
                                let worker = SecretaryNotices::new(
                                    engine,
                                    self.minimum_message_age_minutes,
                                )?;
                                for id in scan
                                    .page(
                                        engine.evidence.store.client.as_ref(),
                                        &engine.evidence.store.root,
                                        limit,
                                    )
                                    .await?
                                {
                                    let key = String::from_utf16_lossy(&id.0);
                                    match worker.step(&id).await {
                                        Ok(()) => issues.clear(&key),
                                        Err(error) if signal.is_cancelled() => return Err(error),
                                        Err(error) => issues.issue(&key, &error.code),
                                    }
                                }
                                Ok(())
                            }
                            .await;
                            *notices.borrow_mut() = Some(scan);
                            result
                        }),
                    });
                }
                if active {
                    wake_pending = true;
                } else {
                    wake_pending = false;
                    deadline = None;
                    let mut scan = scans
                        .borrow_mut()
                        .take()
                        .expect("One foreground chain per generation");
                    let scans = &scans;
                    let stage = &stage;
                    work.push(Work {
                        name: WorkName::Foreground,
                        future: Box::pin(async move {
                            let result = async {
                                stage.set("engine.tick");
                                engine.tick().await?;
                                stage.set("assignments.schedule");
                                scan.tick_shared(&self.scheduler, issues).await?;
                                stage.set("assignments.run");
                                self.runner.tick().await?;
                                Ok(())
                            }
                            .await;
                            *scans.borrow_mut() = Some(scan);
                            result
                        }),
                    });
                }
            }
            let first_issue = issues
                .borrow()
                .issues
                .first()
                .map(|(_, code)| code.as_str().into());
            observe_issue(first_issue);
            tokio::select! {
                biased;
                event = events.recv(), if event_open && next.is_none() => {
                    match event {
                        Some(event @ (ServiceEvent::Reconcile(_) | ServiceEvent::Stop)) => {
                            self.connection.close();
                            closing = true;
                            next = Some(event);
                        },
                        Some(ServiceEvent::State(state)) => {
                            match state.status {
                                "ready" if state.generation == Some(self.connection.generation()) && state.generation != last_ready => {
                                    last_ready = state.generation; kick = true;
                                },
                                "degraded" => last_ready = None,
                                "offline" | "stopped" | "connecting" => { last_ready = None; deadline = None; },
                                _ => {},
                            }
                        },
                        None => { event_open = false; self.connection.close(); closing = true; },
                    }
                },
                _ = &mut driver, if !driver_done => { driver_done = true; },
                (name, result) = completion(&mut work), if !work.is_empty() => {
                    if let Some(key) = name.issue() {
                        match result {
                            Ok(()) => {
                                issues.clear(key);
                                if name == WorkName::Foreground { stage.set("idle"); }
                            },
                            Err(error) if !signal.is_cancelled() => {
                                issues.issue(key, &error.code);
                                if name == WorkName::Foreground { diagnostic(&error, stage.get()); }
                            },
                            Err(_) => {},
                        }
                    }
                    if name == WorkName::Foreground && !closing && !signal.is_cancelled() {
                        deadline = Some(Instant::now() + if wake_pending { Duration::ZERO } else { Duration::from_millis(poll_ms) });
                    }
                },
                _ = async { match drain.as_mut() { Some(drain) => drain.await, None => pending().await } }, if drain.is_some() => {
                    drain = None;
                },
                request = invocations.recv(), if invocation_open => {
                    if let Some(request) = request {
                        if closing || signal.is_cancelled() {
                            let result = invoke(None, request.method, &request.arguments, &request.context, || self.connection.check_open(), || {}).await;
                            let _ = request.reply.send(result);
                        } else {
                            let wake = wake.clone();
                            work.push(Work { name: WorkName::Invocation, future: Box::pin(async move {
                                let result = invoke(Some(&self.handlers), request.method, &request.arguments, &request.context,
                                    || self.connection.check_open(), || { let _ = wake.send(()); }).await;
                                let _ = request.reply.send(result);
                                Ok(())
                            }) });
                        }
                    } else { invocation_open = false; self.connection.close(); closing = true; }
                },
                _ = wakes.recv(), if !closing => { kick = true; },
                _ = async { match deadline { Some(at) => tokio::time::sleep_until(at).await, None => pending().await } }, if !closing => {
                    deadline = None; kick = true;
                },
                _ = signal.cancelled(), if !closing => { closing = true; },
            }
        }
    }
}
