//! Offline ownership and complete private backup of current Host state.
use crate::{
    IvyError, IvyJson, IvyResult, config, executor_lock::ExecutorLock, layout, process_fence,
};
use ivy_contracts::JsString;
use ivy_sdk::clock::Clock;
use std::{collections::BTreeSet, path::PathBuf, sync::Arc, time::Duration};
mod snapshot;
pub use snapshot::snapshot_journal;
mod host;
pub use crate::native_backup_policy::NativeBackupPaths;
pub use host::{BackupBindings, backup_host};

pub struct OfflineOwners(Vec<ExecutorLock>);
impl OfflineOwners {
    pub async fn close(&mut self) -> IvyResult<()> {
        while let Some(lock) = self.0.pop() {
            lock.close().await?;
        }
        Ok(())
    }
}
/// Acquire executor, then every configured owner in the executing locale order.
/// The caller retains this value throughout the admitted maintenance operation.
pub async fn offline_owners(
    config: &IvyJson,
    compare: &crate::artifact::FileCompare,
    docker: &crate::docker::DockerAdapter,
) -> IvyResult<OfflineOwners> {
    let root = config::path_value(config.required("runtimeRoot")?)?;
    let mut owners = OfflineOwners(Vec::new());
    let result = async {
        owners
            .0
            .push(ExecutorLock::acquire(root.join("executor"), false).await?);
        let mut instances = config
            .required("instances")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .collect::<Vec<_>>();
        instances.sort_by(|a, b| {
            compare(
                a.get("instanceId")
                    .and_then(IvyJson::as_js_string)
                    .unwrap_or(&JsString::default()),
                b.get("instanceId")
                    .and_then(IvyJson::as_js_string)
                    .unwrap_or(&JsString::default()),
            )
        });
        for instance in instances {
            let id = instance
                .required("instanceId")?
                .as_js_string()
                .cloned()
                .ok_or_else(IvyError::internal)?;
            owners.0.push(
                ExecutorLock::acquire(
                    root.join("owners").join(crate::node_path::to_native(&id)),
                    false,
                )
                .await?,
            );
            let paths = layout::service_paths(config, instance)?;
            process_fence::require_clear_process_stop_fence(config::path_value(
                paths.required("data")?,
            )?)
            .await?;
            if instance.get("engine") == Some(&"docker".into()) {
                if let Some(container) = docker.inspect_container(config.clone(), id).await? {
                    if !["created", "exited", "dead"]
                        .iter()
                        .any(|state| container.get("state") == Some(&IvyJson::from(*state)))
                    {
                        return Err(IvyError::new(
                            "backup_busy",
                            "Owned container must be stopped before complete backup.",
                        ));
                    }
                }
            }
        }
        Ok(())
    }
    .await;
    if let Err(error) = result {
        owners.close().await?;
        return Err(if error.code == "executor_already_running" {
            IvyError::new(
                "backup_busy",
                "Stop the explicitly owned OS bootstrap processes before complete backup.",
            )
        } else {
            error
        });
    }
    Ok(owners)
}
async fn stopped_owner(path: PathBuf, clock: &dyn Clock) -> IvyResult<ExecutorLock> {
    let deadline = clock.utc_millis() + 5000;
    loop {
        match ExecutorLock::acquire(path.clone(), false).await {
            Ok(lock) => return Ok(lock),
            Err(error)
                if error.code == "executor_already_running" && clock.utc_millis() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(error) => return Err(error),
        }
    }
}
/// Hold the executor first, then each distinct bootstrap owner in UTF-16 order.
/// Ordinary service owners remain live. A stop fence is never cleared here.
pub async fn offline_bootstrap_owners(
    config: &IvyJson,
    instance_ids: &[JsString],
    clock: Arc<dyn Clock>,
) -> IvyResult<OfflineOwners> {
    let root = config::path_value(config.required("runtimeRoot")?)?;
    let mut owners = OfflineOwners(Vec::new());
    let result = async {
        owners
            .0
            .push(stopped_owner(root.join("executor"), clock.as_ref()).await?);
        for id in instance_ids.iter().cloned().collect::<BTreeSet<_>>() {
            let instance = config
                .required("instances")?
                .as_array()
                .ok_or_else(IvyError::internal)?
                .iter()
                .find(|value| value.get("instanceId") == Some(&IvyJson::String(id.clone())));
            let valid = instance.is_some_and(|instance| {
                instance.get("engine") == Some(&"process".into())
                    && ["host-executor", "service-manager"]
                        .iter()
                        .any(|component| {
                            instance.get("componentId") == Some(&IvyJson::from(*component))
                        })
            });
            if !valid {
                return Err(IvyError::new(
                    "target_conflict",
                    "Bootstrap maintenance may lock only configured bootstrap process owners.",
                ));
            }
            owners.0.push(
                stopped_owner(
                    root.join("owners")
                        .join(config::path_value(&IvyJson::String(id))?),
                    clock.as_ref(),
                )
                .await?,
            );
            let paths = layout::service_paths(config, instance.unwrap())?;
            process_fence::require_clear_process_stop_fence(config::path_value(
                paths.required("data")?,
            )?)
            .await?;
        }
        Ok(())
    }
    .await;
    if let Err(error) = result {
        owners.close().await?;
        return Err(if error.code == "executor_already_running" {
            IvyError::new(
                "backup_busy",
                "Stop the explicitly owned OS bootstrap processes before bootstrap maintenance.",
            )
        } else {
            error
        });
    }
    Ok(owners)
}
