//! Explicit one-time association import. Normal runtime lookup never calls this
//! module or guesses historical policy from the current desired configuration.
use crate::{IvyError,IvyJson,IvyResult,config,executor_lock::ExecutorLock,journal::HostJournal};
use std::path::PathBuf;

/// The operator reviews the exact target and an existing immutable accepted
/// HostConfig file. An ambiguous historical policy requires this explicit
/// binding; an operation/candidate similarity is not acceptance evidence.
/// The caller must not manufacture this review from the latest desired config.
#[derive(Clone)]
pub struct ReviewedPolicyBinding {
    pub target: IvyJson,
    pub configuration_path: PathBuf,
    pub configuration_hash: String,
}
#[derive(Clone)]
pub struct PolicyImportRequest {
    pub import_id: String,
    pub expected_state_hash: String,
    pub bindings: Vec<ReviewedPolicyBinding>,
    /// Refuse the entire transaction if any inventoried target remains unbound.
    pub require_complete: bool,
}

/// Read an existing journal without schema initialization, pruning or data
/// normalization. Returns `stateHash`, whole `targets` with their retained
/// origins and any existing policy reference/hash, and `unassociatedTargets`.
/// The state hash is an import precondition, never historical policy evidence.
pub async fn preview_target_policies(host:IvyJson)->IvyResult<IvyJson>{
    let journal=HostJournal::open_policy_import(host,true).await?;
    let result=journal.policy_import_plan().await; journal.close().await?;result
}

/// Call only as an explicit conversion step, before starting normal owners.
/// Hold the existing executor and per-instance kernel locks through commit.
/// A dropped waiter does not release a running import. No live process is
/// stopped and no target, candidate, operation or rollback record is rewritten.
/// Returns before/after state hashes, exact mappings and remaining target keys.
/// A complete batch rolls back if any target is unresolved; a partial batch is
/// explicit. Retry the same import ID with identical arguments to recover its
/// saved receipt. Different arguments, stale state, changed files, identity
/// conflicts and corrupt existing associations refuse without a partial write.
pub async fn import_target_policies(host:IvyJson,request:PolicyImportRequest)->IvyResult<IvyJson>{
    tokio::spawn(async move{
        ivy_contracts::core_schema::validate_host("HostConfig",&host)?;
        let root=config::path_value(host.required("runtimeRoot")?)?;
        let executor=ExecutorLock::acquire(root.clone(),false).await?;
        let journal=HostJournal::open_policy_import(host.clone(),false).await?;
        let result=async{
            let plan=journal.policy_import_plan().await?;
            let mut ids=std::collections::BTreeSet::new();
            for entry in plan.required("targets")?.as_array().ok_or_else(IvyError::internal)?{ids.insert(entry.required("target")?.required("instanceId")?.as_str()?);}
            for instance in host.required("instances")?.as_array().ok_or_else(IvyError::internal)?{ids.insert(instance.required("instanceId")?.as_str()?);}
            let mut owners=Vec::new();
            for id in ids{
                let owner_root=root.join("owners").join(&id);
                if !crate::paths::inside(&root.join("owners"),&owner_root).map_err(|_|IvyError::internal())?{return Err(IvyError::new("target_conflict","Import owner lock leaves its host root."));}
                owners.push(ExecutorLock::acquire(owner_root,false).await?);
            }
            let imported=journal.apply_policy_import(request).await;
            for owner in owners{owner.close().await?;}imported
        }.await;
        journal.close().await?;executor.close().await?;result
    }).await.map_err(|_|IvyError::internal())?
}
