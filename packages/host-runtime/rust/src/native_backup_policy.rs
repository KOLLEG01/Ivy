//! Native backup policy boundary, independent of backup execution.
use ivy_contracts::{IvyJson as J, IvyResult};

/// AgentManager supplies its actual native cache/shared-home ownership policy.
/// It is mandatory even when its result happens to be empty; no fallback policy.
pub trait NativeBackupPaths: Send + Sync {
    fn exclusions(&self, config: &J) -> IvyResult<Vec<J>>;
    fn shared_homes(&self, config: &J) -> IvyResult<Vec<J>>;
}
