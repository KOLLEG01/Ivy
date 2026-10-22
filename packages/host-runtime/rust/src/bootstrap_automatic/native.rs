use super::AutomaticBootstrapRuntime;
use crate::{IvyJson, IvyResult, bootstrap_maintenance::BootstrapMaintenance};
use futures_util::future::BoxFuture;
use std::path::PathBuf;

/// Share the admitted maintenance owner with the independent OS launch boundary.
/// Construction supplies the actual OS, locale, Node and immutable distribution.
pub struct NativeAutomaticBootstrapRuntime(pub BootstrapMaintenance);

impl AutomaticBootstrapRuntime for NativeAutomaticBootstrapRuntime {
    fn inspect(
        &self,
        config: PathBuf,
        _distribution: PathBuf,
    ) -> BoxFuture<'_, IvyResult<IvyJson>> {
        Box::pin(self.0.inspect(config))
    }
    fn launch_automatic(
        &self,
        plan: IvyJson,
        request: PathBuf,
        operation: String,
    ) -> Option<BoxFuture<'_, IvyResult<()>>> {
        self.0.os().launch_automatic(plan, request, operation)
    }
}
