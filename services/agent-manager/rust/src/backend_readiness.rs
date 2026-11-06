//! Independent backend reconciliation within one Hive service generation.
use crate::{inventory_manager::InventoryConnection, reconciliation::AgentReconciliation};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_sdk::service::{InvocationContext, ServiceConnection, ServiceFuture};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::watch;

#[derive(Default)]
struct State {
    owner: Option<AgentReconciliation>,
    connection: Option<(ServiceConnection, JsString)>,
    ready_generation: Option<u64>,
    running: Option<watch::Receiver<bool>>,
    code: Option<JsString>,
}
struct Inner {
    state: Mutex<State>,
    closing: Arc<AtomicBool>,
}
#[derive(Clone)]
pub struct BackendReadiness(Arc<Inner>);
impl BackendReadiness {
    pub fn new(closing: Arc<AtomicBool>) -> Self {
        Self(Arc::new(Inner {
            state: Mutex::new(State::default()),
            closing,
        }))
    }
    pub fn install(&self, owner: AgentReconciliation) -> IvyResult<()> {
        self.0.state.lock().map_err(|_| IvyError::internal())?.owner = Some(owner);
        self.refresh()
    }
    pub fn connect(&self, connection: ServiceConnection, epoch: JsString) -> IvyResult<()> {
        let mut state = self.0.state.lock().map_err(|_| IvyError::internal())?;
        if self.0.closing.load(Ordering::Acquire) {
            return Ok(());
        }
        state.ready_generation = None;
        state.connection = Some((connection, epoch));
        drop(state);
        self.refresh()
    }
    /// At most one accepted reconciliation per backend, including across Hive
    /// reconnects. A slow account never occupies another backend's task.
    pub fn refresh(&self) -> IvyResult<()> {
        let mut state = self.0.state.lock().map_err(|_| IvyError::internal())?;
        if self.0.closing.load(Ordering::Acquire) || state.running.is_some() {
            return Ok(());
        }
        let (Some(owner), Some((connection, epoch))) = (&state.owner, &state.connection) else {
            return Ok(());
        };
        if let Err(error) = owner.available() {
            state.ready_generation = None;
            state.code = Some(error.code_js());
            return Ok(());
        }
        if connection.signal().is_cancelled() {
            return Ok(());
        }
        let generation = connection.generation();
        if state.ready_generation == Some(generation) {
            return Ok(());
        }
        let owner = owner.clone();
        let epoch = epoch.clone();
        let connection = connection.clone();
        let (finished, receiver) = watch::channel(false);
        state.running = Some(receiver);
        let target = self.clone();
        tokio::spawn(async move {
            let expected_epoch = epoch.clone();
            let result = owner
                .reconcile_epoch(
                    InventoryConnection::from_connection(connection.clone()),
                    epoch,
                )
                .await;
            {
                let mut state = target.0.state.lock().unwrap_or_else(|e| e.into_inner());
                state.running = None;
                if state
                    .connection
                    .as_ref()
                    .is_some_and(|(current, current_epoch)| {
                        current.generation() == generation
                            && current_epoch == &expected_epoch
                            && !connection.signal().is_cancelled()
                            && !current.signal().is_cancelled()
                    })
                {
                    state.ready_generation = result.as_ref().ok().map(|_| generation);
                    state.code = result.err().map(|error| error.code_js());
                }
            }
            finished.send_replace(true);
        });
        Ok(())
    }
    pub fn ready(&self) -> bool {
        self.ready_for(None)
    }
    fn ready_for(&self, expected: Option<u64>) -> bool {
        if self.0.closing.load(Ordering::Acquire) {
            return false;
        }
        self.0.state.lock().is_ok_and(|state| {
            state.connection.as_ref().is_some_and(|(connection, _)| {
                !connection.signal().is_cancelled()
                    && expected.is_none_or(|generation| generation == connection.generation())
                    && state.ready_generation == Some(connection.generation())
            }) && state
                .owner
                .as_ref()
                .is_some_and(|owner| owner.available().is_ok())
        })
    }
    pub fn check(&self, context: &InvocationContext) -> IvyResult<()> {
        if self.ready_for(Some(context.generation)) {
            context.throw_if_aborted()?;
            Ok(())
        } else {
            Err(IvyError::new(
                "native_unavailable",
                "The selected backend has not reconciled this Hive generation.",
            ))
        }
    }
    pub fn observation(&self) -> IvyResult<Option<IvyJson>> {
        self.refresh()?;
        let state = self.0.state.lock().map_err(|_| IvyError::internal())?;
        if state.ready_generation
            != state
                .connection
                .as_ref()
                .map(|(connection, _)| connection.generation())
        {
            return Ok(None);
        }
        match (&state.owner, &state.connection) {
            (Some(owner), Some((connection, _))) if !connection.signal().is_cancelled() => owner
                .observe(InventoryConnection::from_connection(connection.clone()))
                .map(Some),
            _ => Ok(None),
        }
    }
    pub fn code(&self) -> IvyResult<Option<JsString>> {
        Ok(self
            .0
            .state
            .lock()
            .map_err(|_| IvyError::internal())?
            .code
            .clone())
    }
    pub fn disconnect(&self) -> IvyResult<()> {
        let mut state = self.0.state.lock().map_err(|_| IvyError::internal())?;
        state.connection = None;
        state.ready_generation = None;
        Ok(())
    }
    pub fn drain(&self) -> ServiceFuture<()> {
        let running = self
            .0
            .state
            .lock()
            .map(|state| state.running.clone())
            .map_err(|_| IvyError::internal());
        Box::pin(async move {
            if let Some(mut running) = running? {
                while !*running.borrow_and_update() {
                    if running.changed().await.is_err() {
                        break;
                    }
                }
            }
            Ok(())
        })
    }
}
