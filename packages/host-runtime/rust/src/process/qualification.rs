//! Explicit consumer observation loss for the private Windows qualification case.
//! The native supervisor and its original proof remain unmodified.
use super::{OwnedProcess, ProcessResult, invalid};
use ivy_contracts::{IvyError, IvyResult, Outcome};
use std::{sync::{Arc, atomic::{AtomicBool, Ordering}}, time::Duration};
use tokio::sync::watch;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Eligibility {
    Unavailable,
    AcknowledgedRawCandidate,
    OrdinaryReady,
    Installed,
}

fn installable(eligibility: Eligibility, pipes: bool, pending: bool, withheld: bool) -> bool {
    eligibility == Eligibility::OrdinaryReady && pipes && pending && !withheld
}

pub(super) fn consumer_view(raw: Arc<ProcessResult>) -> Arc<ProcessResult> {
    if raw.error_code.is_some() {
        return raw;
    }
    let mut result = (*raw).clone();
    result.error_code = Some("outcome_unknown".into());
    result.truncated = true;
    Arc::new(result)
}

/// Original native observation only: no process owner, sender, pipe or handle.
pub struct RetirementWitness {
    raw: watch::Receiver<Option<Arc<ProcessResult>>>,
    job_retired: Arc<AtomicBool>,
}

/// Install before taking any pipe, cloning Completion or publishing the owner.
/// The trusted caller must preserve that order; &mut cannot detect an old clone.
pub fn withhold_retirement_proof(process: &mut OwnedProcess) -> IvyResult<RetirementWitness> {
    let pending = process.completion.raw.borrow();
    if !installable(
        process.qualification,
        process.stdin.is_some() && process.stdout.is_some() && process.stderr.is_some(),
        pending.is_none(),
        process.completion.withheld,
    ) {
        return Err(invalid("Retirement qualification needs a fresh acknowledged ordinary raw Job owner with all pipes and pending completion."));
    }
    let witness = RetirementWitness {
        raw: process.completion.raw.clone(),
        job_retired: process.job_retired.clone(),
    };
    process.completion.withheld = true;
    process.qualification = Eligibility::Installed;
    drop(pending);
    Ok(witness)
}

fn valid_maximum(maximum: Duration) -> bool {
    !maximum.is_zero() && maximum <= Duration::from_secs(60)
}

fn unknown() -> IvyError {
    IvyError::new("outcome_unknown", "The original native retirement observation is unavailable.")
        .with_outcome(Outcome::Unknown)
}

impl RetirementWitness {
    /// Observe the same raw receiver and Acquire proof without initiating cleanup.
    pub async fn wait(&self, maximum: Duration) -> IvyResult<Arc<ProcessResult>> {
        if !valid_maximum(maximum) {
            return Err(invalid("Retirement observation needs a nonzero maximum of at most one minute."));
        }
        let mut receiver = self.raw.clone();
        let observation = async {
            loop {
                let result = receiver.borrow_and_update().clone();
                if let Some(result) = result {
                    return if result.error_code.is_none() && self.job_retired.load(Ordering::Acquire) {
                        Ok(result)
                    } else {
                        Err(unknown())
                    };
                }
                if receiver.changed().await.is_err() {
                    // A final published value can race closure. Closure alone is no receipt.
                    let result = receiver.borrow().clone();
                    return match result {
                        Some(result) if result.error_code.is_none()
                            && self.job_retired.load(Ordering::Acquire) => Ok(result),
                        _ => Err(unknown()),
                    };
                }
            }
        };
        tokio::time::timeout(maximum, observation).await.map_err(|_| unknown())?
    }
}

#[cfg(test)]
#[path = "../../../../../tests/rust/host_live_retirement.rs"]
mod tests;
