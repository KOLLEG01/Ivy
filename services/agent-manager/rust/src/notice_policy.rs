//! Inventory and push metadata used to avoid duplicate internal-task notices.
use crate::project_model::{NativeProjects, ProjectMembership};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::codex_rpc::{NativeRpc, RequestOptions};
use std::{
    collections::{HashMap, HashSet},
    mem::size_of,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

static NULL: IvyJson = IvyJson::Null;
const MAX_THREADS: usize = 1024;
const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_MEMBERSHIP_BYTES: usize = 256 * 1024;
const MAX_READS: usize = 8;
type PolicyRead = watch::Receiver<Option<IvyResult<()>>>;

struct Thread {
    membership: ProjectMembership,
    revision: f64,
    order: u64,
}
impl Thread {
    fn bytes(&self, id: &JsString) -> usize {
        (id.0.capacity()
            + self.membership.cwd.as_ref().map_or(0, |v| v.0.capacity())
            + self
                .membership
                .project_id
                .as_ref()
                .and_then(Option::as_ref)
                .map_or(0, |v| v.0.capacity()))
            * size_of::<u16>()
    }
}
#[derive(Default)]
pub struct NativeNoticePolicy {
    projects: Arc<NativeProjects>,
    threads: HashMap<JsString, Thread>,
    thread_bytes: usize,
    order: u64,
    // A bounded cache cannot keep a tombstone per evicted ID. An older read or
    // snapshot must leave a missing row unknown after a newer row was evicted.
    discarded_revision: Option<f64>,
    revision: f64,
    reads: HashMap<JsString, PolicyRead>,
}
impl NativeNoticePolicy {
    pub fn revision(&self) -> f64 {
        self.revision
    }
    pub fn set_projects(&mut self, projects: Arc<NativeProjects>) {
        self.projects = projects;
    }
    /// Cache storage only; the immutable project set and RPC bodies have owners
    /// and budgets of their own. Account conservatively for spare hash buckets.
    pub fn retained_bytes(&self) -> usize {
        size_of::<Self>()
            + self.thread_bytes
            + if self.threads.capacity() == 0 {
                0
            } else {
                self.threads.capacity() * 2 * (size_of::<(JsString, Thread)>() + 1) + 16
            }
    }
    fn stale(&self, id: &JsString, revision: f64) -> bool {
        self.threads.get(id).map_or_else(
            || self.discarded_revision.is_some_and(|v| v > revision),
            |value| value.revision > revision,
        )
    }
    fn discard(&mut self, revision: f64) {
        if self.discarded_revision.is_none_or(|v| revision > v) {
            self.discarded_revision = Some(revision);
        }
    }
    fn remove(&mut self, id: &JsString) -> Option<(JsString, Thread)> {
        let (id, thread) = self.threads.remove_entry(id)?;
        self.thread_bytes -= thread.bytes(&id);
        Some((id, thread))
    }
    fn evict(&mut self) -> bool {
        let Some(id) = self
            .threads
            .iter()
            .min_by_key(|(_, thread)| thread.order)
            .map(|(id, _)| id.clone())
        else {
            return false;
        };
        let (_, thread) = self.remove(&id).unwrap();
        self.discard(thread.revision);
        true
    }
    fn shrink(&mut self) {
        if self.threads.len() < self.threads.capacity() / 4 {
            self.threads.shrink_to_fit();
        }
    }
    fn store(&mut self, id: JsString, membership: ProjectMembership, revision: f64) {
        self.remove(&id);
        let thread = Thread {
            membership,
            revision,
            order: self.order,
        };
        let bytes = thread.bytes(&id);
        if bytes > MAX_MEMBERSHIP_BYTES {
            self.discard(revision);
            self.shrink();
            return;
        }
        while self.threads.len() >= MAX_THREADS {
            self.evict();
        }
        self.threads.reserve(1);
        while self.retained_bytes() + bytes > MAX_BYTES {
            if !self.evict() {
                self.discard(revision);
                return;
            }
        }
        self.order = self.order.saturating_add(1);
        self.thread_bytes += bytes;
        self.threads.insert(id, thread);
    }
    pub fn set_thread(&mut self, id: JsString, thread: &IvyJson, revision: Option<f64>) {
        let revision = revision.unwrap_or_else(|| {
            self.revision += 1.0;
            self.revision
        });
        if self.stale(&id, revision) {
            return;
        }
        // Check borrowed lengths before copying a possibly oversized path.
        let bytes = id.0.capacity()
            + ["cwd", "projectId"]
                .iter()
                .filter_map(|key| thread.get(key).and_then(IvyJson::as_js_string))
                .map(|value| value.0.len())
                .sum::<usize>();
        if bytes > MAX_MEMBERSHIP_BYTES / size_of::<u16>() {
            self.remove(&id);
            self.discard(revision);
            self.shrink();
            return;
        }
        self.store(id, ProjectMembership::from_thread(thread), revision);
    }
    pub fn set_threads(&mut self, entries: &[IvyJson], revision: f64) -> IvyResult<()> {
        if entries.len() > 5000 {
            return Err(IvyError::new(
                "native_inventory_capacity",
                "Native thread inventory exceeds its declared capacity.",
            ));
        }
        let ids = entries
            .iter()
            .map(|entry| {
                entry
                    .required("nativeId")?
                    .as_js_string()
                    .ok_or_else(IvyError::internal)
            })
            .collect::<IvyResult<HashSet<_>>>()?;
        self.threads.retain(|id, value| {
            let keep = ids.contains(id) || !(value.revision <= revision);
            if !keep {
                self.thread_bytes -= value.bytes(id);
            }
            keep
        });
        self.shrink();
        for entry in entries {
            self.set_thread(
                entry.required("nativeId")?.as_js_string().unwrap().clone(),
                entry.get("summary").unwrap_or(&IvyJson::Null),
                Some(revision),
            );
        }
        Ok(())
    }
    pub fn observe(&mut self, method: &JsString, params: &IvyJson) {
        let thread = params.get("thread").unwrap_or(&NULL);
        let id = params.get("threadId").and_then(IvyJson::as_js_string);
        if method.eq_str("thread/started") {
            if let Some(id) = thread.get("id").and_then(IvyJson::as_js_string) {
                self.set_thread(id.clone(), thread, None);
            }
        }
        if method.eq_str("thread/project/updated") {
            if let (Some(id), Some(project)) = (id, params.get("projectId")) {
                self.revision += 1.0;
                if self.stale(id, self.revision) {
                    return;
                }
                let bytes = id.0.len()
                    + self
                        .threads
                        .get(id)
                        .and_then(|thread| thread.membership.cwd.as_ref())
                        .map_or(0, |cwd| cwd.0.capacity())
                    + project.as_js_string().map_or(0, |v| v.0.len());
                if bytes > MAX_MEMBERSHIP_BYTES / size_of::<u16>() {
                    self.remove(id);
                    self.discard(self.revision);
                    self.shrink();
                    return;
                }
                let (id, mut membership) = self.remove(id).map_or_else(
                    || (id.clone(), ProjectMembership::default()),
                    |(id, thread)| (id, thread.membership),
                );
                membership.project_id = Some(ProjectMembership::project_id(project));
                self.store(id, membership, self.revision);
            }
        }
        if method.eq_str("thread/deleted") {
            if let Some(id) = id {
                self.revision += 1.0;
                self.discard(self.revision);
                self.remove(id);
                self.shrink();
            }
        }
    }
    pub fn is_internal(&self, id: &JsString) -> IvyResult<Option<bool>> {
        let Some(thread) = self.threads.get(id) else {
            return Ok(None);
        };
        Ok(Some(
            self.projects
                .for_thread(&thread.membership)
                .is_some_and(|project| project.name.eq_str("IvyInternal")),
        ))
    }
    pub(crate) fn forget_thread(&mut self, id: &JsString, revision: f64) {
        if self
            .threads
            .get(id)
            .is_some_and(|thread| thread.revision <= revision)
        {
            self.remove(id);
            self.shrink();
        }
    }
    /// Join an existing read or start one bounded observation. Accepted work
    /// finishes independently of its waiter, using the original RPC owner.
    pub async fn ensure(owner: &Arc<Mutex<Self>>, rpc: &NativeRpc, id: JsString) -> IvyResult<()> {
        let mut work = {
            let mut policy = owner.lock().map_err(|_| IvyError::internal())?;
            if policy.threads.contains_key(&id) {
                return Ok(());
            }
            if let Some(work) = policy.reads.get(&id) {
                work.clone()
            } else {
                if policy.reads.len() >= MAX_READS || id.0.len() > 256 {
                    return Err(IvyError::new(
                        "native_inventory_capacity",
                        "Notification policy observations are at capacity.",
                    ));
                }
                let revision = policy.revision;
                let (sender, work) = watch::channel(None);
                policy.reads.insert(id.clone(), work.clone());
                let owner = owner.clone();
                let rpc = rpc.clone();
                tokio::spawn(async move {
                    let result = async {
                        let reply = rpc
                            .request(
                                "thread/read".into(),
                                IvyJson::object([
                                    ("threadId", IvyJson::String(id.clone())),
                                    ("includeTurns", false.into()),
                                ]),
                                RequestOptions {
                                    timeout_ms: 15_000,
                                    detach_on_timeout: true,
                                    ..Default::default()
                                },
                            )
                            .await?;
                        let thread = reply.get("result").and_then(|v| v.get("thread"));
                        if reply.get("error").is_some()
                            || thread
                                .and_then(|v| v.get("id"))
                                .and_then(IvyJson::as_js_string)
                                != Some(&id)
                        {
                            return Err(IvyError::new(
                                "native_inventory_unavailable",
                                "Cannot determine the task's notification policy.",
                            ));
                        }
                        owner.lock().map_err(|_| IvyError::internal())?.set_thread(
                            id.clone(),
                            thread.unwrap(),
                            Some(revision),
                        );
                        Ok(())
                    }
                    .await;
                    owner
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .reads
                        .remove(&id);
                    sender.send_replace(Some(result));
                });
                work
            }
        };
        loop {
            if let Some(result) = work.borrow_and_update().clone() {
                return result;
            }
            work.changed().await.map_err(|_| IvyError::internal())?;
        }
    }
}
