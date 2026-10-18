//! Generation-owned assignment work. Poll `run` alongside the generation's
//! foreground chain, then close/drain it before replacing that generation.
use crate::{
    assignment_advance::AssignmentAdvance,
    assignment_execution::AssignmentExecution,
    assignment_preflight::AssignmentPreflight,
    assignment_scan::AssignmentScan,
    inbox_recovery::{RecoveryIssues, RecoveryScan},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    future::{Future, IntoFuture, pending, poll_fn},
    path::PathBuf,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll},
};
use tokio::sync::{Notify, watch};

type LocalWork<'a> = Pin<Box<dyn Future<Output = IvyResult<()>> + 'a>>;
pub(crate) trait PreflightWork {
    fn contains(&self, id: &JsString) -> bool;
    fn is_empty(&self) -> bool;
    fn admit(&self, id: JsString, document: IvyJson) -> Pin<Box<dyn Future<Output = ()> + '_>>;
}
/// Compatibility adapter for the separately qualified scan/advance entrypoints.
pub(crate) struct CollectedPreflight<'a> {
    ids: RefCell<&'a mut HashSet<JsString>>,
    pub jobs: RefCell<Vec<IvyJson>>,
}
impl<'a> CollectedPreflight<'a> {
    pub fn new(ids: &'a mut HashSet<JsString>) -> Self {
        Self {
            ids: RefCell::new(ids),
            jobs: RefCell::default(),
        }
    }
}
impl PreflightWork for CollectedPreflight<'_> {
    fn contains(&self, id: &JsString) -> bool {
        self.ids.borrow().contains(id)
    }
    fn is_empty(&self) -> bool {
        self.ids.borrow().is_empty()
    }
    fn admit(&self, id: JsString, document: IvyJson) -> Pin<Box<dyn Future<Output = ()> + '_>> {
        self.ids.borrow_mut().insert(id);
        self.jobs.borrow_mut().push(document);
        Box::pin(std::future::ready(()))
    }
}

#[derive(Clone)]
struct Completion {
    sender: watch::Sender<Option<IvyResult<()>>>,
    receiver: watch::Receiver<Option<IvyResult<()>>>,
}
impl Completion {
    fn new() -> Self {
        let (sender, receiver) = watch::channel(None);
        Self { sender, receiver }
    }
    fn finish(&self, result: IvyResult<()>) {
        self.sender.send_replace(Some(result));
    }
    fn ticket(&self) -> AssignmentTick {
        AssignmentTick(self.receiver.clone())
    }
}
/// Every tick admitted during one scan observes the same completion, even if an
/// earlier caller drops its waiter. The generation driver owns the actual work.
#[derive(Clone)]
pub struct AssignmentTick(watch::Receiver<Option<IvyResult<()>>>);
impl IntoFuture for AssignmentTick {
    type Output = IvyResult<()>;
    type IntoFuture = LocalWork<'static>;
    fn into_future(mut self) -> Self::IntoFuture {
        Box::pin(async move {
            loop {
                if let Some(result) = self.0.borrow_and_update().clone() {
                    return result;
                }
                self.0.changed().await.map_err(|_| IvyError::internal())?;
            }
        })
    }
}
#[derive(Default)]
struct RunnerState {
    active: RefCell<Option<Completion>>,
    preflight: RefCell<Option<Completion>>,
    closing: Cell<bool>,
    changed: Notify,
}
#[derive(Clone)]
pub struct AssignmentRunnerHandle(Rc<RunnerState>);
impl AssignmentRunnerHandle {
    pub fn tick(&self) -> AssignmentTick {
        if self.0.closing.get() {
            let done = Completion::new();
            done.finish(Err(IvyError::internal()));
            return done.ticket();
        }
        let mut active = self.0.active.borrow_mut();
        active.get_or_insert_with(Completion::new);
        let ticket = active.as_ref().unwrap().ticket();
        self.0.changed.notify_one();
        ticket
    }
    /// Matches the original two-stage drain: capture the active scan now, then
    /// capture its remaining preflight work after that scan settles.
    pub fn drain(&self) -> impl Future<Output = ()> + 'static {
        let active = self.0.active.borrow().as_ref().map(Completion::ticket);
        let state = self.0.clone();
        async move {
            if let Some(active) = active {
                let _ = active.await;
            }
            let preflight = state.preflight.borrow().as_ref().map(Completion::ticket);
            if let Some(preflight) = preflight {
                let _ = preflight.await;
            }
        }
    }
    /// The owner first cancels the old generation, then closes it and joins run.
    /// Closing stops admission; draining does not cancel or replay a command.
    pub fn close(&self) -> impl Future<Output = ()> + 'static {
        self.0.closing.set(true);
        self.0.changed.notify_one();
        self.drain()
    }
}

struct Preflight<'a> {
    id: JsString,
    work: LocalWork<'a>,
    completion: Completion,
}
struct OwnedPreflight<'a, 'b, 'engine, 'journal> {
    execution: &'a AssignmentExecution<'b, 'engine, 'journal>,
    issues: &'a RefCell<RecoveryScan>,
    bootstrap_root: PathBuf,
    job_launcher: Option<PathBuf>,
    state: Rc<RunnerState>,
    active: RefCell<Option<Preflight<'a>>>,
}
impl OwnedPreflight<'_, '_, '_, '_> {
    fn poll(&self, cx: &mut Context<'_>) -> Poll<()> {
        let result = {
            let mut active = self.active.borrow_mut();
            let Some(active) = active.as_mut() else {
                return Poll::Pending;
            };
            active.work.as_mut().poll(cx)
        };
        let Poll::Ready(result) = result else {
            return Poll::Pending;
        };
        let active = self.active.borrow_mut().take().unwrap();
        let key = format!("execution:{}", String::from_utf16_lossy(&active.id.0));
        match result {
            Ok(()) => self.issues.clear(&key),
            Err(error)
                if !self
                    .execution
                    .native
                    .evidence
                    .cancellation
                    .cancellation
                    .is_cancelled() =>
            {
                self.issues.issue(&key, &error.code)
            }
            Err(_) => {}
        }
        self.state.preflight.borrow_mut().take();
        active.completion.finish(Ok(()));
        Poll::Ready(())
    }
}
impl PreflightWork for OwnedPreflight<'_, '_, '_, '_> {
    fn contains(&self, id: &JsString) -> bool {
        self.active
            .borrow()
            .as_ref()
            .is_some_and(|active| &active.id == id)
    }
    fn is_empty(&self) -> bool {
        self.active.borrow().is_none()
    }
    fn admit(&self, id: JsString, document: IvyJson) -> Pin<Box<dyn Future<Output = ()> + '_>> {
        let execution = self.execution;
        let bootstrap_root = self.bootstrap_root.clone();
        let job_launcher = self.job_launcher.clone();
        let completion = Completion::new();
        *self.state.preflight.borrow_mut() = Some(completion.clone());
        *self.active.borrow_mut() = Some(Preflight {
            id,
            completion,
            work: Box::pin(async move {
                AssignmentPreflight { execution }
                    .execute(&document, &bootstrap_root, job_launcher)
                    .await
                    .map(|_| ())
            }),
        });
        // JS starts the async preflight at admission, before advance resolves.
        // Poll once here, and keep driving it independently of subsequent rows.
        Box::pin(poll_fn(|cx| {
            let _ = self.poll(cx);
            Poll::Ready(())
        }))
    }
}
impl Drop for OwnedPreflight<'_, '_, '_, '_> {
    fn drop(&mut self) {
        if let Some(active) = self.active.get_mut().take() {
            active.completion.finish(Err(IvyError::internal()));
            self.state.preflight.borrow_mut().take();
        }
    }
}

pub struct AssignmentDriver<'a, 'b, 'engine, 'journal> {
    execution: &'a AssignmentExecution<'b, 'engine, 'journal>,
    minimum_notification_age_minutes: f64,
    issues: &'a RefCell<RecoveryScan>,
    bootstrap_root: PathBuf,
    job_launcher: Option<PathBuf>,
    scan: AssignmentScan,
    state: Rc<RunnerState>,
}
impl<'a, 'b, 'engine, 'journal> AssignmentDriver<'a, 'b, 'engine, 'journal> {
    pub fn new(
        execution: &'a AssignmentExecution<'b, 'engine, 'journal>,
        minimum_notification_age_minutes: f64,
        issues: &'a RefCell<RecoveryScan>,
        bootstrap_root: PathBuf,
        job_launcher: Option<PathBuf>,
    ) -> (AssignmentRunnerHandle, Self) {
        let state = Rc::new(RunnerState::default());
        (
            AssignmentRunnerHandle(state.clone()),
            Self {
                execution,
                minimum_notification_age_minutes,
                issues,
                bootstrap_root,
                job_launcher,
                scan: AssignmentScan::default(),
                state,
            },
        )
    }
    /// This borrowed local future keeps the journal and execution context alive.
    /// It must be joined before the generation can close/reopen their storage.
    pub async fn run(mut self) {
        let work = Rc::new(OwnedPreflight {
            execution: self.execution,
            issues: self.issues,
            bootstrap_root: self.bootstrap_root.clone(),
            job_launcher: self.job_launcher.clone(),
            state: self.state.clone(),
            active: RefCell::default(),
        });
        let mut scan = Some(std::mem::take(&mut self.scan));
        let mut foreground: Option<
            Pin<Box<dyn Future<Output = (AssignmentScan, IvyResult<()>)> + '_>>,
        > = None;
        loop {
            if foreground.is_none() && self.state.active.borrow().is_some() {
                let mut next_scan = scan.take().unwrap();
                let work = work.clone();
                let execution = self.execution;
                let issues = self.issues;
                let minimum_notification_age_minutes = self.minimum_notification_age_minutes;
                foreground = Some(Box::pin(async move {
                    let advance = AssignmentAdvance {
                        execution,
                        minimum_notification_age_minutes,
                    };
                    let result = next_scan
                        .scan_with_work(&advance, work.as_ref(), issues)
                        .await;
                    (next_scan, result)
                }));
            }
            if self.state.closing.get() && foreground.is_none() && work.is_empty() {
                break;
            }
            tokio::select! {
                biased;
                _ = poll_fn(|cx| work.poll(cx)) => {},
                (next_scan, result) = async {
                    match foreground.as_mut() {
                        Some(foreground) => foreground.await,
                        None => pending().await,
                    }
                } => {
                    foreground = None;
                    scan = Some(next_scan);
                    if let Some(active) = self.state.active.borrow_mut().take() { active.finish(result); }
                },
                _ = self.state.changed.notified() => {},
            }
        }
    }
}
impl Drop for AssignmentDriver<'_, '_, '_, '_> {
    fn drop(&mut self) {
        self.state.closing.set(true);
        if let Some(active) = self.state.active.borrow_mut().take() {
            active.finish(Err(IvyError::internal()));
        }
    }
}
