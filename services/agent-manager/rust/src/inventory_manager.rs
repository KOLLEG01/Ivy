//! Coalesced inventory reads, generation-owned publication and retained snapshots.
use crate::{
    inventory::{
        InventoryClock, InventorySignal, NativeInventory, ThreadChange, ThreadChanges, ThreadDelta,
        publish_native_projects, publish_native_threads, publish_thread_delta,
    },
    native_journal::NativeJournal,
    notice_policy::NativeNoticePolicy,
    project_model::{NativeProjects, ProjectSnapshot},
    service_work::{
        AgentRun,
        run::{Budget, Reply, Sender},
    },
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, Outcome};
use ivy_sdk::{
    rpc::RpcClient,
    service::{ServiceConnection, ToolHandler},
};
use std::collections::{BTreeMap, HashMap};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

const THREAD_CHECK_MS: f64 = 60_000.0;
const THREAD_COMPLETE_MS: f64 = 900_000.0;

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
pub type ProjectSink = dyn Fn(Arc<NativeProjects>) -> IvyResult<()> + Send + Sync;
pub type ProjectDefaults = dyn Fn() -> IvyResult<IvyJson> + Send + Sync;

pub type InventoryRun = AgentRun<()>;
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
    project_snapshot: Option<ProjectSnapshot>,
    connection: Option<InventoryConnection>,
    binding: u64,
    thread_revision: Option<u64>,
    thread_index: HashMap<JsString, ThreadRecord>,
    thread_complete_at: f64,
    thread_changes: Option<ThreadChanges>,
    full_threads: bool,
}
#[derive(Clone, Copy)]
struct ThreadRecord {
    archived: bool,
    ephemeral: bool,
}
enum ThreadWork {
    Snapshot,
    Current {
        expected: u64,
    },
    Delta {
        expected: u64,
        changes: ThreadChanges,
    },
}
struct Inner {
    inventory: Arc<NativeInventory>,
    journal: Arc<Mutex<NativeJournal>>,
    policy: Arc<Mutex<NativeNoticePolicy>>,
    set_projects: Arc<ProjectSink>,
    closing: Arc<AtomicBool>,
    now: Arc<InventoryClock>,
    state: Mutex<State>,
    budget: Budget,
}
#[derive(Clone)]
pub struct InventoryManager(Arc<Inner>);
struct Completion {
    owner: InventoryManager,
    projects: bool,
    binding: u64,
    sender: Option<Sender<()>>,
}
impl Completion {
    fn finish(&mut self, result: IvyResult<()>) {
        if let Some(mut sender) = self.sender.take() {
            sender.send(self.owner.finished(self.projects, self.binding, result));
        }
    }
}
impl Drop for Completion {
    fn drop(&mut self) {
        if self.sender.is_some() {
            self.finish(Err(IvyError::internal()));
        }
    }
}

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
        let project_snapshot = inventory.current_project_snapshot()?;
        Ok(Self(Arc::new(Inner {
            inventory,
            journal,
            policy,
            set_projects,
            closing,
            now,
            budget: Budget::default(),
            state: Mutex::new(State {
                threads: Lane::default(),
                projects: Lane::default(),
                project_snapshot,
                connection: None,
                binding: 0,
                thread_revision: None,
                thread_index: HashMap::new(),
                thread_complete_at: 0.0,
                thread_changes: None,
                full_threads: true,
            }),
        })))
    }
    /// Retain the verified native snapshot and its original observation time.
    /// Hive reconnection resets the thread publication base, not native evidence.
    pub fn bind(&self, connection: InventoryConnection) -> IvyResult<()> {
        {
            let mut state = self.0.state.lock().map_err(|_| IvyError::internal())?;
            state.connection = Some(connection);
            state.binding = state
                .binding
                .checked_add(1)
                .ok_or_else(IvyError::internal)?;
            state.thread_revision = None;
            state.full_threads = true;
            state.thread_changes = None;
        }
        let snapshot = self.0.inventory.current_project_snapshot()?;
        self.0
            .state
            .lock()
            .map_err(|_| IvyError::internal())?
            .project_snapshot = snapshot.clone();
        if let Some(snapshot) = snapshot {
            (self.0.set_projects)(snapshot.projects)?;
        }
        Ok(())
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
            .project_snapshot
            .is_some())
    }
    pub fn project_snapshot(&self) -> IvyResult<IvyJson> {
        Ok(self.project_model()?.to_json())
    }
    pub fn project_model(&self) -> IvyResult<ProjectSnapshot> {
        self.0
            .state
            .lock()
            .map_err(|_| IvyError::internal())?
            .project_snapshot
            .clone()
            .ok_or_else(|| {
                IvyError::new(
                    "native_inventory_unavailable",
                    "Native projects have not been observed.",
                )
            })
    }
    pub async fn has_active_threads(&self) -> IvyResult<bool> {
        self.0.inventory.has_active_threads().await
    }
    pub fn refresh_threads(&self, connection: InventoryConnection) -> IvyResult<InventoryRun> {
        self.refresh(connection, false)
    }
    pub fn refresh_projects(&self, connection: InventoryConnection) -> IvyResult<InventoryRun> {
        self.refresh(connection, true)
    }
    fn refresh(&self, connection: InventoryConnection, projects: bool) -> IvyResult<InventoryRun> {
        let mut state = self.0.state.lock().map_err(|_| IvyError::internal())?;
        if !projects {
            state.full_threads = true;
            state.thread_changes = None;
        }
        let lane = if projects {
            &mut state.projects
        } else {
            &mut state.threads
        };
        if let Some(work) = lane.work.clone() {
            lane.queued = true;
            return Ok(work);
        }
        self.start(&mut state, connection, projects)
    }
    fn start(
        &self,
        state: &mut State,
        connection: InventoryConnection,
        projects: bool,
    ) -> IvyResult<InventoryRun> {
        // These are synchronous in the source async function, before its first await.
        let admission = self.0.budget.reserve(true, 0)?;
        let preparation = connection.signal.check();
        let revision = self
            .0
            .policy
            .lock()
            .map_err(|_| IvyError::internal())?
            .revision();
        let thread_work = if projects {
            None
        } else {
            let work = if state.full_threads
                || state.thread_revision.is_none()
                || (self.0.now)() - state.thread_complete_at >= THREAD_COMPLETE_MS
            {
                state.full_threads = false;
                state.thread_changes = None;
                ThreadWork::Snapshot
            } else if state.thread_changes.is_none() {
                ThreadWork::Current {
                    expected: state.thread_revision.unwrap(),
                }
            } else {
                let changes = state.thread_changes.take().ok_or_else(IvyError::internal)?;
                // Keep an ordered archive hint while its metadata read is in flight.
                // Failed publication invalidates the delta base and forces reconciliation.
                for (id, change) in &changes.entries {
                    if let ThreadChange::Read { archived } = change {
                        if let Some(previous) = state.thread_index.get_mut(id) {
                            previous.archived = *archived;
                        }
                    }
                }
                ThreadWork::Delta {
                    expected: state.thread_revision.unwrap(),
                    changes,
                }
            };
            Some(work)
        };
        let observation = projects || !matches!(thread_work, Some(ThreadWork::Delta { .. }));
        let binding = state.binding;
        let lane = if projects {
            &mut state.projects
        } else {
            &mut state.threads
        };
        let (sender, run) = InventoryRun::channel(admission.call.clone());
        lane.work = Some(run.clone());
        lane.queued = false;
        if observation {
            lane.at = (self.0.now)();
        }
        let mut complete = Completion {
            owner: self.clone(),
            projects,
            binding,
            sender: Some(sender),
        };
        tokio::spawn(async move {
            let _input = admission;
            let result = complete
                .owner
                .run(
                    &connection,
                    projects,
                    preparation,
                    revision,
                    thread_work,
                    binding,
                )
                .await;
            complete.finish(result);
        });
        Ok(run)
    }
    async fn run(
        &self,
        connection: &InventoryConnection,
        projects: bool,
        preparation: IvyResult<()>,
        revision: f64,
        thread_work: Option<ThreadWork>,
        binding: u64,
    ) -> IvyResult<()> {
        preparation?;
        if projects {
            let next = self
                .0
                .inventory
                .collect_project_snapshot(Some(&connection.signal))
                .await?;
            {
                let state = self.0.state.lock().map_err(|_| IvyError::internal())?;
                current_binding(&state, binding)?;
            }
            self.0
                .policy
                .lock()
                .map_err(|_| IvyError::internal())?
                .set_projects(next.projects.clone());
            (self.0.set_projects)(next.projects.clone())?;
            publish_native_projects(
                connection.client.as_ref(),
                self.0.journal.as_ref(),
                &next,
                Some(&connection.signal),
            )
            .await?;
            connection.signal.check()?;
            let mut state = self.0.state.lock().map_err(|_| IvyError::internal())?;
            current_binding(&state, binding)?;
            state.project_snapshot = Some(next);
            state.projects.code = None;
        } else {
            let partial = match thread_work {
                Some(ThreadWork::Delta { expected, changes }) => Some((
                    expected,
                    false,
                    self.0
                        .inventory
                        .collect_thread_changes(changes, Some(&connection.signal))
                        .await,
                )),
                Some(ThreadWork::Current { expected }) => Some((
                    expected,
                    true,
                    self.0
                        .inventory
                        .collect_current_threads(Some(&connection.signal))
                        .await
                        .map(|snapshot| ThreadDelta {
                            snapshot,
                            removed: vec![],
                        }),
                )),
                _ => None,
            };
            if let Some((expected, current_only, result)) = partial {
                match result {
                    Ok(delta) => {
                        connection.signal.check()?;
                        let fits = {
                            let state = self.0.state.lock().map_err(|_| IvyError::internal())?;
                            current_binding(&state, binding)?;
                            let added = delta
                                .snapshot
                                .required("threads")?
                                .as_array()
                                .unwrap()
                                .iter()
                                .filter(|entry| {
                                    !state.thread_index.contains_key(
                                        entry.get("nativeId").unwrap().as_js_string().unwrap(),
                                    )
                                })
                                .count();
                            let removed = delta
                                .removed
                                .iter()
                                .filter(|id| state.thread_index.contains_key(*id))
                                .count();
                            let membership_matches = !current_only || {
                                let entries =
                                    delta.snapshot.required("threads")?.as_array().unwrap();
                                let visible = state
                                    .thread_index
                                    .values()
                                    .filter(|entry| !entry.archived && !entry.ephemeral)
                                    .count();
                                entries.len() == visible
                                    && entries.iter().all(|entry| {
                                        state
                                            .thread_index
                                            .get(
                                                entry
                                                    .get("nativeId")
                                                    .unwrap()
                                                    .as_js_string()
                                                    .unwrap(),
                                            )
                                            .is_some_and(|known| {
                                                !known.archived && !known.ephemeral
                                            })
                                    })
                            };
                            membership_matches && state.thread_index.len() + added - removed <= 5000
                        };
                        if fits {
                            match publish_thread_delta(
                                connection.client.as_ref(),
                                self.0.journal.as_ref(),
                                &delta,
                                expected,
                                &connection.signal,
                            )
                            .await
                            {
                                Ok(next) => {
                                    return self
                                        .commit_delta(connection, &delta, next, revision, binding);
                                }
                                Err(error)
                                    if error.code == "revision_conflict"
                                        && error.outcome == Outcome::NotExecuted => {}
                                Err(error) => return Err(error),
                            }
                        }
                    }
                    Err(_) => {
                        // These are metadata reads. Reconcile from a complete observation,
                        // without guessing a missing archive flag or rebasing a Hive delta.
                        connection.signal.check()?;
                    }
                }
            }
            {
                let mut state = self.0.state.lock().map_err(|_| IvyError::internal())?;
                current_binding(&state, binding)?;
                state.threads.at = (self.0.now)();
                state.thread_complete_at = state.threads.at;
            }
            let next = self
                .0
                .inventory
                .collect_threads(Some(&connection.signal))
                .await?;
            {
                let state = self.0.state.lock().map_err(|_| IvyError::internal())?;
                current_binding(&state, binding)?;
            }
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
            let published = publish_native_threads(
                connection.client.as_ref(),
                self.0.journal.as_ref(),
                &next,
                Some(&connection.signal),
            )
            .await?;
            connection.signal.check()?;
            let mut state = self.0.state.lock().map_err(|_| IvyError::internal())?;
            current_binding(&state, binding)?;
            state.thread_index = next
                .required("threads")?
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| {
                    Ok((
                        entry.required("nativeId")?.as_js_string().unwrap().clone(),
                        ThreadRecord {
                            archived: entry
                                .required("summary")?
                                .required("archived")?
                                .as_bool()
                                .unwrap(),
                            ephemeral: entry
                                .required("summary")?
                                .get("ephemeral")
                                .and_then(IvyJson::as_bool)
                                == Some(true),
                        },
                    ))
                })
                .collect::<IvyResult<_>>()?;
            state.thread_revision = Some(published);
            state.threads.code = None;
        }
        Ok(())
    }
    fn commit_delta(
        &self,
        connection: &InventoryConnection,
        delta: &ThreadDelta,
        next: u64,
        revision: f64,
        binding: u64,
    ) -> IvyResult<()> {
        connection.signal.check()?;
        let mut state = self.0.state.lock().map_err(|_| IvyError::internal())?;
        current_binding(&state, binding)?;
        let mut policy = self.0.policy.lock().map_err(|_| IvyError::internal())?;
        for entry in delta.snapshot.required("threads")?.as_array().unwrap() {
            let id = entry.required("nativeId")?.as_js_string().unwrap();
            let summary = entry.required("summary")?;
            state.thread_index.insert(
                id.clone(),
                ThreadRecord {
                    archived: summary.required("archived")?.as_bool().unwrap(),
                    ephemeral: summary.get("ephemeral").and_then(IvyJson::as_bool) == Some(true),
                },
            );
            policy.set_thread(id.clone(), summary, Some(revision));
        }
        for id in &delta.removed {
            state.thread_index.remove(id);
            policy.forget_thread(id, revision);
        }
        state.thread_revision = Some(next);
        state.threads.code = None;
        Ok(())
    }
    fn finished(&self, projects: bool, binding: u64, result: IvyResult<()>) -> Arc<Reply<()>> {
        let Ok(mut state) = self.0.state.lock() else {
            return self.0.budget.finish_unit(result, true);
        };
        if !projects && result.is_err() && state.binding == binding {
            state.thread_revision = None;
            state.full_threads = true;
            state.thread_changes = None;
        }
        let lane = if projects {
            &mut state.projects
        } else {
            &mut state.threads
        };
        lane.work = None;
        // Status describes the retained reply; native success/failure above
        // remains independent of whether that reply fits the result pool.
        let reply = self.0.budget.finish_unit(result, true);
        if let Err(error) = &reply.value {
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
            // Publish the next work under the same lock as completion: another
            // thread cannot turn the single queued follow-up into an extra read.
            if self.start(&mut state, connection, projects).is_err() {
                // The existing queued hint survives unread-result pressure.
                // The regular control tick retries admission without a new task.
                let lane = if projects {
                    &mut state.projects
                } else {
                    &mut state.threads
                };
                lane.queued = true;
            }
        }
        reply
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
        self.refresh_projects(connection)?.wait().await
    }
    /// Notice policy joins an existing refresh, or performs initial discovery.
    /// An already observed complete snapshot does not cause another native read.
    pub async fn ensure_projects(&self, connection: InventoryConnection) -> IvyResult<()> {
        let (running, observed) = {
            let state = self.0.state.lock().map_err(|_| IvyError::internal())?;
            (
                state.projects.work.clone(),
                state.project_snapshot.is_some(),
            )
        };
        if let Some(work) = running {
            work.wait().await
        } else if !observed {
            self.refresh_projects(connection)?.wait().await
        } else {
            Ok(())
        }
    }
    pub fn request_threads(&self, ready: bool) -> IvyResult<()> {
        self.request(ready, false)
    }
    /// Coalesce metadata hints by native ID. One running read plus at most64
    /// pending IDs; overflow and unknown archive state require a complete read.
    pub fn observe_thread(
        &self,
        ready: bool,
        method: &JsString,
        id: Option<JsString>,
        observed_at: &JsString,
    ) -> IvyResult<()> {
        if !ready || self.0.closing.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut state = self.0.state.lock().map_err(|_| IvyError::internal())?;
        let Some(connection) = state.connection.clone() else {
            return Ok(());
        };
        let id = id.filter(|id| !id.0.is_empty() && id.0.len() <= 256);
        if let Some(id) = id {
            let change = if method.eq_str("thread/deleted") {
                Some(ThreadChange::Removed)
            } else {
                let archived = if method.eq_str("thread/archived") {
                    Some(true)
                } else if method.eq_str("thread/unarchived") || method.eq_str("thread/started") {
                    Some(false)
                } else {
                    state
                        .thread_changes
                        .as_ref()
                        .and_then(|changes| changes.entries.get(&id))
                        .and_then(|change| match change {
                            ThreadChange::Read { archived } => Some(*archived),
                            ThreadChange::Removed => None,
                        })
                        .or_else(|| state.thread_index.get(&id).map(|entry| entry.archived))
                };
                archived.map(|archived| ThreadChange::Read { archived })
            };
            if let Some(change) = change {
                if !state.full_threads
                    && (1..=64).contains(&observed_at.0.len())
                    && state.thread_changes.as_ref().is_none_or(|changes| {
                        changes.entries.contains_key(&id) || changes.entries.len() < 64
                    })
                {
                    if state.thread_changes.is_none() {
                        state.thread_changes = observed_at.to_utf8().ok().map(|at| ThreadChanges {
                            observed_at: at.into_boxed_str(),
                            entries: BTreeMap::new(),
                        });
                    }
                    if let Some(changes) = state.thread_changes.as_mut() {
                        changes.entries.insert(id, change);
                    } else {
                        state.full_threads = true;
                    }
                } else {
                    state.full_threads = true;
                    state.thread_changes = None;
                }
            } else {
                state.full_threads = true;
                state.thread_changes = None;
            }
        } else {
            state.full_threads = true;
            state.thread_changes = None;
        }
        if state.threads.work.is_some() {
            state.threads.queued = true;
        } else {
            let _ = self.start(&mut state, connection, false)?;
        }
        Ok(())
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
                let _ = self.refresh(connection, projects)?;
            }
        }
        Ok(())
    }
    /// Current metadata keeps its minute cadence. Historical consistency uses
    /// complete observations every fifteen minutes; notices still apply immediately.
    pub fn refresh_due(&self, connection: InventoryConnection) -> IvyResult<()> {
        let (threads, projects) = {
            let state = self.0.state.lock().map_err(|_| IvyError::internal())?;
            (
                state.threads.work.is_none()
                    && (state.threads.queued
                        || (self.0.now)() - state.threads.at >= THREAD_CHECK_MS
                        || state.thread_revision.is_some()
                            && (self.0.now)() - state.thread_complete_at >= THREAD_COMPLETE_MS),
                state.projects.work.is_none()
                    && (state.projects.queued || (self.0.now)() - state.projects.at >= 300_000.0),
            )
        };
        if threads {
            let mut state = self.0.state.lock().map_err(|_| IvyError::internal())?;
            if state.threads.work.is_none() {
                let _ = self.start(&mut state, connection.clone(), false)?;
            }
        }
        if projects {
            let _ = self.refresh_projects(connection)?;
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
                let snapshot = owner.project_model()?;
                let mut result = snapshot.projects_result();
                result.insert("defaults", defaults()?);
                Ok(result)
            })
        })
    }
    /// Shutdown awaits tracked effects, including admitted queued follow-ups.
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
                let _ = work.completed().await;
            }
            if let Some(work) = projects {
                let _ = work.completed().await;
            }
        }
    }
}
fn current_binding(state: &State, binding: u64) -> IvyResult<()> {
    if state.binding == binding {
        Ok(())
    } else {
        Err(IvyError::new(
            "native_inventory_changed",
            "Inventory connection generation changed during observation.",
        ))
    }
}
