//! Coalesced inventory reads, generation-owned publication and retained snapshots.
use crate::{
    inventory::{
        InventoryClock, InventorySignal, NativeInventory, publish_native_projects,
        publish_native_threads,
    },
    native_journal::NativeJournal,
    notice_policy::NativeNoticePolicy,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_sdk::{
    rpc::RpcClient,
    service::{ServiceConnection, ToolHandler},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::watch;

/// Production callers must retain the delivered ServiceConnection and its signal.
#[derive(Clone)]
pub struct InventoryConnection {
    pub client: Arc<dyn RpcClient>,
    pub signal: InventorySignal,
}
impl InventoryConnection {
    pub fn from_connection(connection: ServiceConnection) -> Self {
        Self {
            client: Arc::new(connection.clone()),
            signal: InventorySignal::from_connection(connection),
        }
    }
}
pub type ProjectSink = dyn Fn(&[IvyJson]) -> IvyResult<()> + Send + Sync;
pub type ProjectDefaults = dyn Fn() -> IvyResult<IvyJson> + Send + Sync;

#[derive(Clone)]
pub struct InventoryRun(watch::Receiver<Option<IvyResult<()>>>);
impl InventoryRun {
    pub async fn wait(mut self) -> IvyResult<()> {
        loop {
            if let Some(result) = self.0.borrow_and_update().clone() {
                return result;
            }
            self.0.changed().await.map_err(|_| IvyError::internal())?;
        }
    }
}
#[derive(Default)]
struct Lane {
    at: f64,
    work: Option<InventoryRun>,
    queued: bool,
    code: Option<JsString>,
}
struct State {
    threads: Lane,
    projects: Lane,
    project_snapshot: IvyJson,
    project_observed: bool,
    connection: Option<InventoryConnection>,
}
struct Inner {
    inventory: Arc<NativeInventory>,
    journal: Arc<Mutex<NativeJournal>>,
    policy: Arc<Mutex<NativeNoticePolicy>>,
    set_projects: Arc<ProjectSink>,
    closing: Arc<AtomicBool>,
    now: Arc<InventoryClock>,
    state: Mutex<State>,
}
#[derive(Clone)]
pub struct InventoryManager(Arc<Inner>);

impl InventoryManager {
    pub fn new(
        inventory: Arc<NativeInventory>,
        journal: Arc<Mutex<NativeJournal>>,
        policy: Arc<Mutex<NativeNoticePolicy>>,
        set_projects: Arc<ProjectSink>,
        closing: Arc<AtomicBool>,
    ) -> IvyResult<Self> {
        Self::with_clock(
            inventory,
            journal,
            policy,
            set_projects,
            closing,
            Arc::new(|| chrono::Utc::now().timestamp_millis() as f64),
        )
    }
    pub fn with_clock(
        inventory: Arc<NativeInventory>,
        journal: Arc<Mutex<NativeJournal>>,
        policy: Arc<Mutex<NativeNoticePolicy>>,
        set_projects: Arc<ProjectSink>,
        closing: Arc<AtomicBool>,
        now: Arc<InventoryClock>,
    ) -> IvyResult<Self> {
        let project_snapshot = inventory.current_projects(None)?;
        Ok(Self(Arc::new(Inner {
            inventory,
            journal,
            policy,
            set_projects,
            closing,
            now,
            state: Mutex::new(State {
                threads: Lane::default(),
                projects: Lane::default(),
                project_snapshot,
                project_observed: false,
                connection: None,
            }),
        })))
    }
    /// Reconciliation resets the cached snapshot but retains the original observed flag.
    /// The surrounding service still performs its native availability/transport probe.
    pub fn bind(&self, connection: InventoryConnection) -> IvyResult<()> {
        self.0
            .state
            .lock()
            .map_err(|_| IvyError::internal())?
            .connection = Some(connection);
        let snapshot = self.0.inventory.current_projects(None)?;
        self.0
            .state
            .lock()
            .map_err(|_| IvyError::internal())?
            .project_snapshot = snapshot.clone();
        (self.0.set_projects)(project_entries(&snapshot)?)
    }
    pub fn code(&self) -> IvyResult<Option<JsString>> {
        let state = self.0.state.lock().map_err(|_| IvyError::internal())?;
        Ok(state
            .threads
            .code
            .clone()
            .or_else(|| state.projects.code.clone()))
    }
    pub fn projects_observed(&self) -> IvyResult<bool> {
        Ok(self
            .0
            .state
            .lock()
            .map_err(|_| IvyError::internal())?
            .project_observed)
    }
    pub fn project_snapshot(&self) -> IvyResult<IvyJson> {
        Ok(self
            .0
            .state
            .lock()
            .map_err(|_| IvyError::internal())?
            .project_snapshot
            .clone())
    }
    pub async fn has_active_threads(&self) -> IvyResult<bool> {
        self.0.inventory.has_active_threads().await
    }
    pub fn refresh_threads(&self, connection: InventoryConnection) -> IvyResult<InventoryRun> {
        self.refresh(connection, false, true)
    }
    pub fn refresh_projects(
        &self,
        connection: InventoryConnection,
        native_read: bool,
    ) -> IvyResult<InventoryRun> {
        self.refresh(connection, true, native_read)
    }
    fn refresh(
        &self,
        connection: InventoryConnection,
        projects: bool,
        native_read: bool,
    ) -> IvyResult<InventoryRun> {
        let mut state = self.0.state.lock().map_err(|_| IvyError::internal())?;
        let lane = if projects {
            &mut state.projects
        } else {
            &mut state.threads
        };
        if let Some(work) = lane.work.clone() {
            lane.queued |= !projects || native_read;
            return Ok(work);
        }
        self.start(lane, connection, projects, native_read)
    }
    fn start(
        &self,
        lane: &mut Lane,
        connection: InventoryConnection,
        projects: bool,
        native_read: bool,
    ) -> IvyResult<InventoryRun> {
        // These are synchronous in the source async function, before its first await.
        let preparation = connection.signal.check().and_then(|()| {
            if projects && !native_read {
                self.0.inventory.current_projects(None).map(Some)
            } else {
                Ok(None)
            }
        });
        let revision = self
            .0
            .policy
            .lock()
            .map_err(|_| IvyError::internal())?
            .revision();
        let (complete, receiver) = watch::channel(None);
        let run = InventoryRun(receiver);
        lane.work = Some(run.clone());
        lane.at = (self.0.now)();
        let owner = self.clone();
        tokio::spawn(async move {
            let result = owner
                .run(&connection, projects, preparation, revision)
                .await;
            let _ = owner.finished(projects, &result);
            let _ = complete.send(Some(result));
        });
        Ok(run)
    }
    async fn run(
        &self,
        connection: &InventoryConnection,
        projects: bool,
        preparation: IvyResult<Option<IvyJson>>,
        revision: f64,
    ) -> IvyResult<()> {
        let cached = preparation?;
        if projects {
            let next = match cached {
                Some(value) => value,
                None => {
                    self.0
                        .inventory
                        .collect_projects(Some(&connection.signal))
                        .await?
                }
            };
            let entries = project_entries(&next)?;
            self.0
                .policy
                .lock()
                .map_err(|_| IvyError::internal())?
                .set_projects(entries.to_vec());
            (self.0.set_projects)(entries)?;
            publish_native_projects(
                connection.client.as_ref(),
                self.0.journal.as_ref(),
                &next,
                Some(&connection.signal),
            )
            .await?;
            connection.signal.check()?;
            let mut state = self.0.state.lock().map_err(|_| IvyError::internal())?;
            state.project_snapshot = next;
            state.project_observed = true;
            state.projects.code = None;
        } else {
            let next = self
                .0
                .inventory
                .collect_threads(Some(&connection.signal))
                .await?;
            self.0
                .policy
                .lock()
                .map_err(|_| IvyError::internal())?
                .set_threads(
                    next.required("threads")?
                        .as_array()
                        .ok_or_else(IvyError::internal)?,
                    revision,
                )?;
            publish_native_threads(
                connection.client.as_ref(),
                self.0.journal.as_ref(),
                &next,
                Some(&connection.signal),
            )
            .await?;
            connection.signal.check()?;
            self.0
                .state
                .lock()
                .map_err(|_| IvyError::internal())?
                .threads
                .code = None;
        }
        Ok(())
    }
    fn finished(&self, projects: bool, result: &IvyResult<()>) -> IvyResult<()> {
        let mut state = self.0.state.lock().map_err(|_| IvyError::internal())?;
        let lane = if projects {
            &mut state.projects
        } else {
            &mut state.threads
        };
        lane.work = None;
        if let Err(error) = result {
            lane.code = Some(error.code_js());
        }
        let queued = std::mem::take(&mut lane.queued);
        let follow_up = if queued && !self.0.closing.load(Ordering::Acquire) {
            state
                .connection
                .clone()
                .filter(|connection| !connection.signal.is_aborted())
        } else {
            None
        };
        if let Some(connection) = follow_up {
            let lane = if projects {
                &mut state.projects
            } else {
                &mut state.threads
            };
            // Publish the next work under the same lock as completion: another
            // thread cannot turn the single queued follow-up into an extra read.
            let _ = self.start(lane, connection, projects, true)?;
        }
        Ok(())
    }
    pub async fn fresh_projects(&self, connection: InventoryConnection) -> IvyResult<()> {
        let running = self
            .0
            .state
            .lock()
            .map_err(|_| IvyError::internal())?
            .projects
            .work
            .clone();
        if let Some(work) = running {
            work.wait().await?;
        }
        self.refresh_projects(connection, true)?.wait().await
    }
    /// Notice policy joins an existing refresh, or performs initial discovery.
    /// An already observed complete snapshot does not cause another native read.
    pub async fn ensure_projects(&self, connection: InventoryConnection) -> IvyResult<()> {
        let (running, observed) = {
            let state = self.0.state.lock().map_err(|_| IvyError::internal())?;
            (state.projects.work.clone(), state.project_observed)
        };
        if let Some(work) = running {
            work.wait().await
        } else if !observed {
            self.refresh_projects(connection, true)?.wait().await
        } else {
            Ok(())
        }
    }
    pub fn request_threads(&self, ready: bool) -> IvyResult<()> {
        self.request(ready, false)
    }
    pub fn request_projects(&self, ready: bool) -> IvyResult<()> {
        self.request(ready, true)
    }
    fn request(&self, ready: bool, projects: bool) -> IvyResult<()> {
        if ready {
            let connection = self
                .0
                .state
                .lock()
                .map_err(|_| IvyError::internal())?
                .connection
                .clone();
            if let Some(connection) = connection {
                let _ = self.refresh(connection, projects, true)?;
            }
        }
        Ok(())
    }
    /// Called by service readiness, at the original independent refresh intervals.
    pub fn refresh_due(&self, connection: InventoryConnection) -> IvyResult<()> {
        let (threads, projects) = {
            let state = self.0.state.lock().map_err(|_| IvyError::internal())?;
            (
                state.threads.work.is_none() && (self.0.now)() - state.threads.at >= 60_000.0,
                state.projects.work.is_none() && (self.0.now)() - state.projects.at >= 300_000.0,
            )
        };
        if threads {
            let _ = self.refresh_threads(connection.clone())?;
        }
        if projects {
            let _ = self.refresh_projects(connection, true)?;
        }
        Ok(())
    }
    pub fn projects_handler(&self, defaults: Arc<ProjectDefaults>) -> ToolHandler {
        let owner = self.clone();
        Arc::new(move |_, _| {
            let owner = owner.clone();
            let defaults = defaults.clone();
            Box::pin(async move {
                if !owner.projects_observed()? {
                    let connection = owner
                        .0
                        .state
                        .lock()
                        .map_err(|_| IvyError::internal())?
                        .connection
                        .clone()
                        .ok_or_else(IvyError::internal)?;
                    owner.fresh_projects(connection).await?;
                }
                let snapshot = owner.project_snapshot()?;
                let mut result = snapshot.required("projects")?.clone();
                result.insert("defaults", defaults()?);
                Ok(result)
            })
        })
    }
    /// Shutdown awaits tracked work, including an already-queued follow-up.
    pub async fn drain(&self) -> IvyResult<()> {
        loop {
            let (threads, projects) = {
                let state = self.0.state.lock().map_err(|_| IvyError::internal())?;
                (state.threads.work.clone(), state.projects.work.clone())
            };
            if threads.is_none() && projects.is_none() {
                return Ok(());
            }
            if let Some(work) = threads {
                let _ = work.wait().await;
            }
            if let Some(work) = projects {
                let _ = work.wait().await;
            }
        }
    }
}
fn project_entries(snapshot: &IvyJson) -> IvyResult<&[IvyJson]> {
    snapshot
        .required("projects")?
        .required("projects")?
        .as_array()
        .ok_or_else(IvyError::internal)
}
