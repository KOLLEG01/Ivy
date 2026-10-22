use super::{HostActions, close};
use crate::{
    IvyError, IvyJson as J, IvyResult,
    artifact::FileCompare,
    config,
    host_config::{self, HostContext},
    journal::HostJournal,
    layout::StorageContext,
    native_backup_paths::NativeBackupPolicy,
    native_restore, node_path,
    node_runtime::NodeRuntime,
    private_backup::{self, BackupBindings},
    restore::{self, RestoreBindings},
};
use ivy_contracts::JsString;
use std::{path::PathBuf, sync::Arc, time::Instant};

/// One executing-account generation shared by restore admission and actual
/// native relocation. Node and locale qualification belong to the caller.
pub struct RestoreRuntime {
    node: NodeRuntime,
    bindings: Arc<RestoreBindings>,
    native_policy: Arc<NativeBackupPolicy>,
    project_storage: Arc<StorageContext>,
}
fn storage(account: &HostContext, compare: FileCompare) -> StorageContext {
    StorageContext {
        paths: node_path::Context {
            style: account.paths.style,
            cwd: account.paths.cwd.clone(),
        },
        account_home: account.account_home.clone(),
        compare,
    }
}
impl RestoreRuntime {
    pub fn native(node: NodeRuntime, compare: FileCompare) -> IvyResult<Self> {
        let account = HostContext::current()?;
        let native_policy = Arc::new(NativeBackupPolicy::new(
            storage(&account, compare.clone()),
            account.environment.clone(),
            account.paths.cwd.clone(),
        ));
        let project_storage = Arc::new(storage(&account, compare.clone()));
        let storage = storage(&account, compare);
        Ok(Self {
            node,
            bindings: Arc::new(RestoreBindings {
                storage,
                environment: account.environment,
                native_paths: native_policy.clone(),
            }),
            native_policy,
            project_storage,
        })
    }
}

impl HostActions {
    /// Keep the local command's outer journal alive while the offline backup
    /// owner takes and releases its own maintenance transaction and locks.
    pub async fn backup(
        &self,
        config_path: PathBuf,
        destination: PathBuf,
        bindings: Arc<BackupBindings>,
    ) -> IvyResult<J> {
        let this = self.clone();
        tokio::spawn(async move {
            let config = host_config::host_config(config_path.clone()).await?;
            let journal = HostJournal::open_with_clock(config, this.clock).await?;
            let result = private_backup::backup_host(config_path, destination, bindings).await;
            close(journal, result).await
        })
        .await
        .map_err(|_| IvyError::internal())?
    }

    /// Verify the selected immutable backup and fresh target before creating
    /// any target state. Every later stage operates on the new working copy;
    /// admitted restore continues through final publication after observer loss.
    pub async fn restore(
        &self,
        config_path: PathBuf,
        backup_root: PathBuf,
        manifest_hash: JsString,
        runtime: RestoreRuntime,
    ) -> IvyResult<J> {
        let this = self.clone();
        tokio::spawn(async move {
            let started = Instant::now();
            let backup_root = node_path::from_native(&backup_root);
            let manifest = restore::verify_backup(
                backup_root.clone(),
                manifest_hash.clone(),
                &runtime.bindings,
            )
            .await?;
            restore::require_restore_platform(&manifest, &runtime.node)?;
            let config = restore::target_configuration(
                node_path::from_native(&config_path),
                backup_root.clone(),
                &manifest,
                manifest_hash,
                uuid::Uuid::new_v4().to_string().into(),
                &runtime.bindings,
            )
            .await?;
            let created_at: JsString =
                chrono::DateTime::from_timestamp_millis(this.clock.utc_millis())
                    .ok_or_else(IvyError::internal)?
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                    .into();
            restore::copy_restore_payload(
                backup_root.clone(),
                manifest.clone(),
                config.clone(),
                runtime.bindings.clone(),
                this.job_launcher,
            )
            .await?;
            let maintenance_bytes =
                restore::hold_restored_bootstrap_maintenance(config.clone(), created_at.clone())
                    .await?;
            let original = config::json_file(
                node_path::to_native(&node_path::join(
                    runtime.bindings.storage.paths.style,
                    &[backup_root, "configuration.json".into()],
                )),
                config::CONFIGURATION_BYTES,
            )
            .await?;
            let areas = manifest
                .get("storageAreas")
                .filter(|v| !v.is_null())
                .and_then(J::as_array)
                .unwrap_or(&[])
                .iter()
                .map(|v| {
                    Ok(crate::layout::StorageArea {
                        key: v
                            .required("key")?
                            .as_js_string()
                            .cloned()
                            .ok_or_else(IvyError::internal)?,
                        path: v
                            .required("path")?
                            .as_js_string()
                            .cloned()
                            .ok_or_else(IvyError::internal)?,
                    })
                })
                .collect::<IvyResult<Vec<_>>>()?;
            restore::relocate_projects(
                original.clone(),
                config.clone(),
                areas,
                runtime.project_storage,
            )
            .await?;
            let native = native_restore::relocate_native_storage(
                &original,
                &config,
                &manifest,
                &this.distribution,
                &runtime.native_policy,
            )
            .await?;
            restore::publish_restored_host(
                config,
                manifest,
                native,
                maintenance_bytes,
                created_at,
                started,
                runtime.bindings,
            )
            .await
        })
        .await
        .map_err(|_| IvyError::internal())?
    }
}
