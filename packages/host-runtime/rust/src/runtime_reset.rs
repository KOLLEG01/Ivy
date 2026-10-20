//! Coordinated reset identity and uncertain-release recovery. Local destructive
//! phases and UI publication are supplied by their admitted owning operations.
use crate::{IvyError, IvyJson as J, IvyResult};
use ivy_contracts::JsString;
use std::{collections::BTreeSet, future::Future};
mod plan;
pub use plan::local_runtime_reset_plan;
mod local;
pub use local::{
    complete_reset_record, confirm_reset_record, prepare_reset_hold, prove_reset_locks_released,
    reset_record_path, restored_bootstrap_target, saved_reset_runtime,
};
mod host;
pub use host::{ResetHost, reset_process_alive, wait_reset_processes_stopped};
fn truthy(value: &J) -> bool {
    match value {
        J::Null => false,
        J::Bool(value) => *value,
        J::Number(value) => *value != 0.0 && !value.is_nan(),
        J::String(value) => !value.0.is_empty(),
        _ => true,
    }
}

/// An interrupted deployment retains the same reset ID and epoch until the
/// matching coordinator record confirms completion.
pub fn retained_reset_identity(previous: Option<&J>, plan_hash: &JsString) -> Option<J> {
    let previous = previous.filter(|value| !value.is_null())?;
    if previous.get("deploymentComplete").is_some_and(truthy)
        || previous.get("planHash") != Some(&J::String(plan_hash.clone()))
    {
        return None;
    }
    let mut value = J::object([(
        "resumeFromReleased",
        (previous.get("phase") == Some(&"released".into())).into(),
    )]);
    for key in ["resetId", "runtimeEpoch"] {
        if let Some(identity) = previous.get(key) {
            value.insert(key, identity.clone());
        }
    }
    Some(value)
}
fn collect_scope_roots(value: &J, roots: &mut BTreeSet<JsString>) {
    match value {
        J::Array(values) => {
            for value in values {
                collect_scope_roots(value, roots);
            }
        }
        J::Object(values) => {
            for (key, value) in values {
                if key == &JsString::from("rootObjectId") {
                    if let Some(value) = value.as_js_string() {
                        roots.insert(value.clone());
                        continue;
                    }
                }
                collect_scope_roots(value, roots);
            }
        }
        _ => (),
    }
}
pub fn configured_scope_root_ids(config: &J) -> IvyResult<Vec<JsString>> {
    let mut roots = BTreeSet::new();
    for instance in config
        .required("instances")?
        .as_array()
        .ok_or_else(IvyError::internal)?
    {
        if instance.is_null() {
            return Err(IvyError::internal());
        }
        if let Some(settings) = instance.get("settings") {
            collect_scope_roots(settings, &mut roots);
        }
    }
    Ok(roots.into_iter().collect())
}
pub fn coordinated_runtime_reset_plans(plans: &[J]) -> IvyResult<Vec<J>> {
    let mut roots = BTreeSet::new();
    for plan in plans {
        for root in plan
            .required("scopeRootIds")?
            .as_array()
            .ok_or_else(IvyError::internal)?
        {
            roots.insert(
                root.as_js_string()
                    .cloned()
                    .ok_or_else(IvyError::internal)?,
            );
        }
    }
    let roots = J::Array(roots.into_iter().map(J::String).collect());
    Ok(plans
        .iter()
        .map(|plan| {
            let mut plan = plan.clone();
            plan.insert("scopeRootIds", roots.clone());
            plan
        })
        .collect())
}

async fn fenced_phase<P, PF, R, RF, A>(
    hosts: &[J],
    phase: P,
    reprepare: R,
    after: A,
    possibly_key: &str,
) -> IvyResult<()>
where
    P: Fn(J) -> PF,
    PF: Future<Output = IvyResult<()>>,
    R: Fn(J) -> RF,
    RF: Future<Output = IvyResult<()>>,
    A: Future<Output = IvyResult<()>>,
{
    let mut possibly = Vec::new();
    let result = async {
        for host in hosts {
            // Include the current host before dispatch: a missing response does
            // not prove that its release/bootstrap operation did not execute.
            possibly.push(host.clone());
            phase(host.clone()).await?;
        }
        after.await
    }
    .await;
    if let Err(mut failure) = result {
        let recovery =
            futures_util::future::join_all(possibly.iter().cloned().map(reprepare)).await;
        let ids = |selected: &dyn Fn(usize) -> bool| -> J {
            J::Array(
                possibly
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| selected(*i))
                    .map(|(_, host)| host.get("hostId").cloned().unwrap_or(J::Null))
                    .collect(),
            )
        };
        let mut details = J::object([
            (possibly_key, ids(&|_| true)),
            ("repreparedHosts", ids(&|i| recovery[i].is_ok())),
            ("reprepareFailures", ids(&|i| recovery[i].is_err())),
        ]);
        if let Some(cause) = failure.details.take() {
            details.insert("cause", cause);
        }
        return Err(failure.with_details(details));
    }
    Ok(())
}

/// Reprepare every possibly released host after any release/confirmation error,
/// retaining both the original failure and all recovery outcomes.
pub async fn release_reset_hosts<P, PF, R, RF, A>(
    hosts: &[J],
    release: P,
    reprepare: R,
    after_release: A,
) -> IvyResult<()>
where
    P: Fn(J) -> PF,
    PF: Future<Output = IvyResult<()>>,
    R: Fn(J) -> RF,
    RF: Future<Output = IvyResult<()>>,
    A: Future<Output = IvyResult<()>>,
{
    fenced_phase(
        hosts,
        release,
        reprepare,
        after_release,
        "possiblyReleasedHosts",
    )
    .await
}

/// Bootstrap and verify all selected UIs while the deployment remains held.
/// Failure reprepares even a host whose bootstrap response was lost.
pub async fn bootstrap_reset_uis<P, PF, R, RF, A>(
    hosts: &[J],
    bootstrap: P,
    reprepare: R,
    restore_uis: A,
) -> IvyResult<()>
where
    P: Fn(J) -> PF,
    PF: Future<Output = IvyResult<()>>,
    R: Fn(J) -> RF,
    RF: Future<Output = IvyResult<()>>,
    A: Future<Output = IvyResult<()>>,
{
    fenced_phase(
        hosts,
        bootstrap,
        reprepare,
        restore_uis,
        "possiblyBootstrappedHosts",
    )
    .await
}
