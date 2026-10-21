//! Storage effects for an already admitted local reset. The coordinator owns
//! administrator/hold/identity admission, durable capture and clear/phase order.
//! Hive owns the Store, SQL and bootstrap failure boundaries.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

type Capture = dyn Fn(&Path) -> IvyResult<IvyJson> + Send + Sync;
type Bootstrap = dyn Fn(&ResetBootstrap) -> IvyResult<()> + Send + Sync;

/// Evaluate the admitted clock at Hive's root-bootstrap boundary, after restore.
pub type ResetClock = Arc<dyn Fn() -> IvyResult<JsString> + Send + Sync>;

pub struct ResetBootstrap {
    pub path: PathBuf,
    pub epoch: JsString,
    pub instructions: IvyJson,
    pub scope_roots: Vec<JsString>,
    pub service_contracts: Vec<IvyJson>,
    pub now: ResetClock,
}

#[derive(Clone)]
pub struct ResetStorage {
    capture: Arc<Capture>,
    bootstrap: Arc<Bootstrap>,
}

/// Bind the actual storage owner at the service boundary. HostRuntime does not
/// depend on Hive, and no Store crosses a thread or an async suspension point.
pub fn hive_reset_storage() -> ResetStorage {
    ResetStorage {
        capture: Arc::new(ivy_hive::runtime_reset::saved_instructions),
        bootstrap: Arc::new(|request| {
            ivy_hive::runtime_reset::rebootstrap(
                &request.path,
                &request.epoch,
                &request.instructions,
                &request.scope_roots,
                &request.service_contracts,
                || (request.now)(),
            )
        }),
    }
}

impl ResetStorage {
    /// Polling admits one blocking operation. Losing its result waiter does not
    /// cancel the accepted Store work or replace the owning callback.
    pub async fn saved_instructions(&self, path: PathBuf) -> IvyResult<IvyJson> {
        let capture = self.capture.clone();
        tokio::task::spawn_blocking(move || capture(&path))
            .await
            .map_err(|_| IvyError::internal())?
    }

    pub async fn rebootstrap(&self, request: ResetBootstrap) -> IvyResult<()> {
        let bootstrap = self.bootstrap.clone();
        tokio::task::spawn_blocking(move || bootstrap(&request))
            .await
            .map_err(|_| IvyError::internal())?
    }
}
