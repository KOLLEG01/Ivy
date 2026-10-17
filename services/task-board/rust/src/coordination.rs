//! Dependency and delegation policy; reads remain owned by the workflow store.

use std::collections::HashSet;
use std::future::Future;
use std::hash::Hash;

/// Policy failures are separate from the store's original read error. The service
/// adapter preserves that error, including its execution outcome.
#[derive(Debug, PartialEq, Eq)]
pub enum GraphError<E> {
    Read(E),
    DependencyCycle,
    DependencyLimit,
    DelegationCycle,
    DelegationLimit,
    InvalidParent,
}

impl<E> GraphError<E> {
    pub fn policy_error(&self) -> Option<(&'static str, &'static str)> {
        Some(match self {
            Self::Read(_) => return None,
            Self::DependencyCycle => (
                "task_board_dependency_cycle",
                "A task cannot wait on itself, directly or indirectly.",
            ),
            Self::DependencyLimit => ("limit_exceeded", "The blocking graph is too large."),
            Self::DelegationCycle => (
                "task_board_dependency_cycle",
                "A task cannot delegate to itself, directly or indirectly.",
            ),
            Self::DelegationLimit => ("limit_exceeded", "The delegation chain is too large."),
            Self::InvalidParent => (
                "task_board_invalid_parent",
                "The delegating task must be an unfinished, unarchived agent task.",
            ),
        })
    }
}

pub struct Dependencies<I> {
    pub dependencies: Vec<I>,
    pub blocker: Option<I>,
}

/// The reader must include archived records, just like the existing blocker
/// validator. A failed read cannot establish that a dependency is absent.
pub async fn validate_blocker<I, E, F, R>(
    blocker: I,
    own_id: Option<&I>,
    mut read: F,
) -> Result<(), GraphError<E>>
where
    I: Eq + Hash + Clone,
    F: FnMut(I) -> R,
    R: Future<Output = Result<Dependencies<I>, E>>,
{
    let mut pending = vec![blocker];
    let mut seen = HashSet::new();
    while let Some(id) = pending.pop() {
        if Some(&id) == own_id {
            return Err(GraphError::DependencyCycle);
        }
        if seen.contains(&id) {
            continue;
        }
        if seen.len() >= 512 {
            return Err(GraphError::DependencyLimit);
        }
        seen.insert(id.clone());
        let node = read(id).await.map_err(GraphError::Read)?;
        pending.extend(node.dependencies);
        pending.extend(node.blocker);
    }
    Ok(())
}

pub struct Parent<I> {
    pub archived: bool,
    pub agent_controlled: bool,
    pub terminal: bool,
    pub parent: Option<I>,
}

/// Generic IDs allow the adapter to retain the shared contract's full string
/// representation. Traversal order and both bounds match the TypeScript engine.
pub async fn validate_parent<I, E, F, R>(
    parent: I,
    own_id: Option<I>,
    mut read: F,
) -> Result<(), GraphError<E>>
where
    I: Eq + Hash + Clone,
    F: FnMut(I) -> R,
    R: Future<Output = Result<Parent<I>, E>>,
{
    let mut seen: HashSet<I> = own_id.into_iter().collect();
    let mut next = Some(parent);
    while let Some(id) = next {
        if seen.contains(&id) {
            return Err(GraphError::DelegationCycle);
        }
        if seen.len() >= 64 {
            return Err(GraphError::DelegationLimit);
        }
        seen.insert(id.clone());
        let node = read(id).await.map_err(GraphError::Read)?;
        if node.archived || !node.agent_controlled || node.terminal {
            return Err(GraphError::InvalidParent);
        }
        next = node.parent;
    }
    Ok(())
}

/// `idle` is stronger than an empty claim: publication and unfinished native
/// runs must also be absent. A `done` blocker tests the workflow state alone.
pub fn blocker_satisfied(
    until: &str,
    workflow_state: &str,
    claim: bool,
    publication: bool,
    last_run_phase: Option<&str>,
) -> bool {
    if until == "done" {
        return workflow_state == "done";
    }
    !claim
        && !publication
        && last_run_phase.is_none_or(|phase| matches!(phase, "completed" | "failed" | "cancelled"))
}
