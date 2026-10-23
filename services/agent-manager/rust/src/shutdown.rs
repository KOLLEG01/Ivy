//! One owned shutdown attempt. Later cleanup cannot hide an earlier failure.
use crate::{
    inventory_manager::InventoryManager, native_operations::ReceiptWork, service_work::AgentWork,
};
use ivy_contracts::{IvyError, IvyResult, JsString};
use ivy_sdk::service::ServiceFuture;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::watch;

pub type ShutdownStep = dyn Fn() -> ServiceFuture<()> + Send + Sync;
pub type FinalHealth = dyn Fn(JsString) -> ServiceFuture<()> + Send + Sync;
/// Startup supplies each existing resource boundary, including optional owners
/// which have not been created yet. There are no alternate stop/force paths.
pub struct ShutdownPhases {
    pub stop_timer: Arc<dyn Fn() + Send + Sync>,
    pub native: Arc<ShutdownStep>,
    pub work: Arc<ShutdownStep>,
    pub service: Arc<ShutdownStep>,
    pub instructions: Arc<ShutdownStep>,
    pub mcp: Arc<ShutdownStep>,
    pub skills: Arc<ShutdownStep>,
    pub locations: Arc<ShutdownStep>,
    pub state: Arc<ShutdownStep>,
    pub pending_health: Arc<ShutdownStep>,
    pub final_health: Arc<FinalHealth>,
    pub journal: Arc<dyn Fn() -> IvyResult<()> + Send + Sync>,
}
#[derive(Clone)]
pub struct ShutdownRun(Arc<watch::Receiver<Option<IvyResult<()>>>>);
impl ShutdownRun {
    pub fn shares_completion(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub async fn wait(&self) -> IvyResult<()> {
        let mut receiver = self.0.as_ref().clone();
        loop {
            if let Some(result) = receiver.borrow_and_update().clone() {
                return result;
            }
            receiver.changed().await.map_err(|_| IvyError::internal())?;
        }
    }
}
struct Inner {
    phases: ShutdownPhases,
    closing: Arc<AtomicBool>,
    running: Mutex<Option<ShutdownRun>>,
    closed: watch::Sender<Option<JsString>>,
}
#[derive(Clone)]
pub struct AgentShutdown(Arc<Inner>);
#[derive(Default)]
struct Failure(Mutex<Option<IvyError>>);
impl Failure {
    fn record(&self, result: IvyResult<()>) {
        if let Err(error) = result {
            let mut first = self.0.lock().unwrap_or_else(|error| error.into_inner());
            if first.is_none() {
                *first = Some(error);
            }
        }
    }
    fn result(&self) -> IvyResult<()> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
            .map_or(Ok(()), Err)
    }
}
async fn attempt(future: ServiceFuture<()>, first: &Failure) {
    first.record(future.await);
}
struct Finalize {
    owner: Arc<Inner>,
    first: Arc<Failure>,
    sender: Option<watch::Sender<Option<IvyResult<()>>>>,
}
impl Finalize {
    fn finish(&mut self, code: JsString, result: IvyResult<()>) {
        // Journal-only failures retain the original requested closed code. An
        // earlier cleanup failure (especially native outcome_unknown) wins.
        let journal = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (self.owner.phases.journal)()
        }))
        .unwrap_or_else(|_| Err(IvyError::internal()));
        let result = result.and(journal);
        self.owner.closed.send_replace(Some(code));
        if let Some(sender) = self.sender.take() {
            sender.send_replace(Some(result));
        }
    }
}
impl Drop for Finalize {
    fn drop(&mut self) {
        if self.sender.is_some() {
            let result = self.first.result().and(Err(IvyError::internal()));
            self.finish("agent_shutdown_failed".into(), result);
        }
    }
}
impl AgentShutdown {
    pub fn new(phases: ShutdownPhases, closing: Arc<AtomicBool>) -> Self {
        Self(Arc::new(Inner {
            phases,
            closing,
            running: Mutex::new(None),
            closed: watch::channel(None).0,
        }))
    }
    /// The first caller owns the requested code and one execution. Every later
    /// caller joins that same completion, even after all waiters are dropped.
    pub fn close(&self, code: JsString) -> IvyResult<ShutdownRun> {
        let mut running = self.0.running.lock().map_err(|_| IvyError::internal())?;
        if let Some(run) = running.as_ref() {
            return Ok(run.clone());
        }
        self.0.closing.store(true, Ordering::Release);
        (self.0.phases.stop_timer)();
        let (sender, receiver) = watch::channel(None);
        let run = ShutdownRun(Arc::new(receiver));
        *running = Some(run.clone());
        let owner = self.0.clone();
        tokio::spawn(async move {
            let first = Arc::new(Failure::default());
            let mut finalize = Finalize {
                owner: owner.clone(),
                first: first.clone(),
                sender: Some(sender),
            };
            let phases = &owner.phases;
            attempt((phases.native)(), &first).await;
            attempt((phases.work)(), &first).await;
            attempt((phases.service)(), &first).await;
            // Start every manager before awaiting any; first observed failure
            // is captured at settlement, not from argument/manager order.
            let instructions = (phases.instructions)();
            let mcp = (phases.mcp)();
            let skills = (phases.skills)();
            tokio::join!(
                biased;
                attempt(instructions, &first),
                attempt(mcp, &first),
                attempt(skills, &first)
            );
            attempt((phases.locations)(), &first).await;
            attempt((phases.state)(), &first).await;
            // Existing healthWork.catch(() => undefined) remains non-fatal.
            let _ = (phases.pending_health)().await;
            let stopped_code = if first.result().is_err() {
                "agent_shutdown_failed".into()
            } else {
                code.clone()
            };
            attempt((phases.final_health)(stopped_code), &first).await;
            let result = first.result();
            let code = if result.is_err() {
                "agent_shutdown_failed".into()
            } else {
                code
            };
            finalize.finish(code, result);
        });
        Ok(run)
    }
    pub async fn closed(&self) -> IvyResult<JsString> {
        let mut receiver = self.0.closed.subscribe();
        loop {
            if let Some(code) = receiver.borrow_and_update().clone() {
                return Ok(code);
            }
            receiver.changed().await.map_err(|_| IvyError::internal())?;
        }
    }
}

#[derive(Clone)]
pub struct AgentShutdownWork {
    pub invocations: AgentWork,
    pub receipts: ReceiptWork,
    pub inventory: InventoryManager,
}
impl AgentShutdownWork {
    /// Snapshot accepted invocations after native retirement and also settle
    /// independently owned terminal receipt/inventory writers before SQLite.
    pub fn drain(&self) -> ServiceFuture<()> {
        let snapshot = self.invocations.drain_snapshot();
        let receipts = self.receipts.clone();
        let inventory = self.inventory.clone();
        Box::pin(async move {
            let _ = tokio::join!(snapshot, receipts.drain(), inventory.drain());
            Ok(())
        })
    }
}
