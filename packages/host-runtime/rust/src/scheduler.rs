//! Host activation admission. The caller supplies journal order and owns each lane.

use std::collections::HashSet;

/// A borrowed view keeps admission independent of a journal connection or task context.
#[derive(Clone, Copy, Debug)]
pub struct Deployment<'a> {
    pub deployment_id: &'a str,
    pub instance_id: &'a str,
    pub exclusive: bool,
}

/// Select indices into the queue, preserving the existing fair infrastructure barriers.
/// Active work is never selected twice. Operations for one instance remain ordered,
/// while a busy ordinary instance does not block unrelated work before a barrier.
pub fn parallel_deployments(
    queued: &[Deployment<'_>],
    active: &[Deployment<'_>],
    limit: usize,
) -> Vec<usize> {
    let mut admission = Admission::new(active, limit);
    let mut selected = Vec::new();
    for (index, entry) in queued.iter().enumerate() {
        if admission.consider(*entry) {
            selected.push(index);
        }
        if admission.stopped() {
            break;
        }
    }
    selected
}

/// Incremental form for a journal caller that must resolve exclusivity lazily.
/// No later manifest may be inspected beyond the same fair queue barrier.
pub struct Admission<'a> {
    instances: HashSet<&'a str>,
    running: HashSet<&'a str>,
    active: usize,
    selected: usize,
    limit: usize,
    stopped: bool,
}
impl<'a> Admission<'a> {
    pub fn new(active: &[Deployment<'a>], limit: usize) -> Self {
        Self {
            instances: active.iter().map(|e| e.instance_id).collect(),
            running: active.iter().map(|e| e.deployment_id).collect(),
            active: active.len(),
            selected: 0,
            limit,
            stopped: active.iter().any(|e| e.exclusive),
        }
    }
    pub fn stopped(&self) -> bool {
        self.stopped
    }
    pub fn ignores(&self, id: &str) -> bool {
        self.stopped || self.running.contains(id)
    }
    pub fn consider(&mut self, entry: Deployment<'a>) -> bool {
        if self.ignores(entry.deployment_id) {
            return false;
        }
        if entry.exclusive {
            self.stopped = true;
            if self.active == 0 && self.selected == 0 {
                self.selected += 1;
                return true;
            }
            return false;
        }
        if self.active + self.selected >= self.limit {
            self.stopped = true;
            return false;
        }
        if !self.instances.insert(entry.instance_id) {
            return false;
        }
        self.selected += 1;
        true
    }
}

/// A source build or infrastructure replacement owns the entire activation queue.
/// `hive_manifest` comes from the verified candidate kind, including custom components.
pub fn exclusive_deployment(phase: &str, component_id: &str, hive_manifest: bool) -> bool {
    phase == "preparing" || component_id == "host-executor" || hive_manifest
}
