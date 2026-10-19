//! Adapt Hive's existing error boundary to the shared Node filesystem mapping.
use crate::store::sql_error;
use ivy_contracts::IvyResult;
use ivy_host_runtime::node_path::{from_native, node_filesystem_path};
use std::path::{Path, PathBuf};

pub(crate) fn path(value: impl AsRef<Path>) -> IvyResult<PathBuf> {
    node_filesystem_path(&from_native(value.as_ref())).map_err(sql_error)
}

pub(crate) fn exists(value: impl AsRef<Path>) -> bool {
    path(value).is_ok_and(|path| path.exists())
}
