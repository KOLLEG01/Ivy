//! Native backup assembly. Hive owns SQLite snapshotting; HostRuntime owns
//! offline admission, inventory, verification and publication.
use ivy_contracts::IvyResult;
use ivy_host_runtime::{
    artifact::FileCompare, host_config::HostContext, layout::StorageContext,
    native_backup_paths::NativeBackupPolicy, node_runtime::NodeRuntime,
    private_backup::BackupBindings,
};
use ivy_sdk::clock::Clock;
use std::{path::PathBuf, sync::Arc};

pub fn native_backup_bindings(
    node: impl Into<Option<NodeRuntime>>,
    compare: FileCompare,
    distribution_root: PathBuf,
    clock: Arc<dyn Clock>,
) -> IvyResult<BackupBindings> {
    let account = HostContext::current()?;
    let cwd = account.paths.cwd.clone();
    Ok(BackupBindings {
        compare: compare.clone(),
        native_paths: Arc::new(NativeBackupPolicy::new(
            StorageContext {
                paths: account.paths,
                account_home: account.account_home,
                compare,
            },
            account.environment,
            cwd,
        )),
        node: node.into(),
        hive_snapshot: ivy_hive::storage_read::snapshot_stopped_hive,
        distribution_root,
        clock,
    })
}
