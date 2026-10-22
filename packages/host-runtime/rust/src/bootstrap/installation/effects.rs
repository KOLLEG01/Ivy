//! Native defaults for the initial installer's private effect composition.
use crate::{
    IvyError, IvyResult, exec_file,
    linux_host::{LinuxHost, NativeLinuxHost},
    node_path,
    systemd::{NativeSystemdHost, SystemdHost},
};
use futures_util::future::BoxFuture;
use ivy_contracts::JsString;
use std::{fs, path::PathBuf, sync::Arc};

#[derive(Clone)]
pub(super) struct UnitDirectoryEntry {
    pub(super) name: JsString,
    pub(super) is_file: bool,
}
type UnitEntries =
    Arc<dyn Fn() -> BoxFuture<'static, IvyResult<Vec<UnitDirectoryEntry>>> + Send + Sync>;
type InstallExec = Arc<
    dyn Fn(
            exec_file::FileCommand,
            PathBuf,
            exec_file::FileOptions,
        ) -> BoxFuture<'static, IvyResult<exec_file::FileResult>>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub(super) struct InitialInstallEffects {
    pub(super) linux: Arc<dyn LinuxHost>,
    pub(super) units: Arc<dyn SystemdHost>,
    pub(super) unit_entries: UnitEntries,
    pub(super) execute_windows: InstallExec,
}
impl InitialInstallEffects {
    pub(super) fn native(distribution: PathBuf) -> Self {
        Self {
            linux: Arc::new(NativeLinuxHost::new(distribution.clone())),
            units: Arc::new(NativeSystemdHost::new(distribution)),
            unit_entries: Arc::new(|| {
                Box::pin(async {
                    tokio::task::spawn_blocking(|| {
                        fs::read_dir("/etc/systemd/system")
                            .map_err(|_| IvyError::internal())?
                            .map(|entry| {
                                let entry = entry.map_err(|_| IvyError::internal())?;
                                let kind = entry.file_type().map_err(|_| IvyError::internal())?;
                                Ok(UnitDirectoryEntry {
                                    name: node_path::from_native(&PathBuf::from(entry.file_name())),
                                    is_file: kind.is_file(),
                                })
                            })
                            .collect()
                    })
                    .await
                    .map_err(|_| IvyError::internal())?
                })
            }),
            execute_windows: Arc::new(|command, distribution, options| {
                Box::pin(exec_file::exec_file(command, distribution, options))
            }),
        }
    }
}
