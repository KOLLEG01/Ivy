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
    if active.iter().any(|entry| entry.exclusive) {
        return Vec::new();
    }
    let mut instances: HashSet<&str> = active.iter().map(|e| e.instance_id).collect();
    let running: HashSet<&str> = active.iter().map(|e| e.deployment_id).collect();
    let mut selected = Vec::new();
    for (index, entry) in queued.iter().enumerate() {
        if running.contains(entry.deployment_id) {
            continue;
        }
        if entry.exclusive {
            if active.is_empty() && selected.is_empty() {
                selected.push(index);
            }
            break;
        }
        if active.len() + selected.len() >= limit {
            break;
        }
        if instances.insert(entry.instance_id) {
            selected.push(index);
        }
    }
    selected
}

/// A source build or infrastructure replacement owns the entire activation queue.
/// `hive_manifest` comes from the verified candidate kind, including custom components.
pub fn exclusive_deployment(phase: &str, component_id: &str, hive_manifest: bool) -> bool {
    phase == "preparing" || component_id == "host-executor" || hive_manifest
}
