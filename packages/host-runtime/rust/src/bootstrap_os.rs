//! Bootstrap definition effects. Domain admission and durable replay live in
//! bootstrap_maintenance; this boundary observes or mutates exact owned entries.
use crate::{IvyJson, IvyResult, bootstrap_restart::NativeBootstrapRestartOs};
use futures_util::future::BoxFuture;
use std::path::PathBuf;
mod automatic;
mod systemd;
pub use systemd::SystemdBootstrapOs;

pub fn bootstrap_os(
    distribution: PathBuf,
    job_launcher: PathBuf,
    node: crate::node_runtime::NodeRuntime,
) -> IvyResult<std::sync::Arc<dyn BootstrapOs>> {
    if cfg!(windows) {
        Ok(std::sync::Arc::new(WindowsBootstrapOs::new(
            distribution,
            job_launcher,
        )))
    } else if cfg!(target_os = "linux") {
        Ok(std::sync::Arc::new(SystemdBootstrapOs::new(
            distribution,
            node,
        )))
    } else {
        Err(crate::IvyError::new(
            "unsupported_runtime",
            "Bootstrap maintenance supports Windows and systemd Linux.",
        ))
    }
}

pub trait BootstrapOs: Send + Sync {
    fn inspect(&self, plans: Vec<IvyJson>) -> BoxFuture<'_, IvyResult<Vec<IvyJson>>>;
    fn render(&self, owner: IvyJson, next: IvyJson) -> BoxFuture<'_, IvyResult<IvyJson>>;
    fn check(&self, definition: IvyJson) -> BoxFuture<'_, IvyResult<()>>;
    fn apply(
        &self,
        owner: IvyJson,
        desired: IvyJson,
        plans: Vec<IvyJson>,
    ) -> BoxFuture<'_, IvyResult<()>>;
    fn reload(&self) -> BoxFuture<'_, IvyResult<()>>;
    fn prepare(&self, _next: IvyJson) -> Option<BoxFuture<'_, IvyResult<()>>> {
        None
    }
    fn pause(
        &self,
        _snapshot: IvyJson,
        _plans: Vec<IvyJson>,
    ) -> Option<BoxFuture<'_, IvyResult<()>>> {
        None
    }
    fn resume(
        &self,
        _snapshot: IvyJson,
        _plans: Vec<IvyJson>,
    ) -> Option<BoxFuture<'_, IvyResult<()>>> {
        None
    }
    fn can_pause_and_resume(&self) -> bool {
        false
    }
    fn launch_automatic(
        &self,
        _plan: IvyJson,
        _request: PathBuf,
        _operation: String,
    ) -> Option<BoxFuture<'_, IvyResult<()>>> {
        None
    }
}

/// Retained PowerShell is OS glue only: the updater/worker is native Rust. This
/// adapter never launches the old JavaScript automatic-bootstrap worker.
#[derive(Clone)]
pub struct WindowsBootstrapOs {
    distribution: PathBuf,
    calls: NativeBootstrapRestartOs,
}
impl WindowsBootstrapOs {
    pub fn new(distribution: PathBuf, job_launcher: PathBuf) -> Self {
        Self {
            distribution,
            calls: NativeBootstrapRestartOs::new(job_launcher),
        }
    }
    fn call(&self, action: &'static str, data: IvyJson) -> BoxFuture<'static, IvyResult<IvyJson>> {
        self.calls.call(action, data, self.distribution.clone())
    }
}
impl BootstrapOs for WindowsBootstrapOs {
    fn inspect(&self, plans: Vec<IvyJson>) -> BoxFuture<'_, IvyResult<Vec<IvyJson>>> {
        let call = self.call(
            "inspect",
            IvyJson::object([("plans", IvyJson::Array(plans))]),
        );
        Box::pin(async move {
            call.await?
                .as_array()
                .map(<[IvyJson]>::to_vec)
                .ok_or_else(crate::IvyError::internal)
        })
    }
    fn render(&self, owner: IvyJson, next: IvyJson) -> BoxFuture<'_, IvyResult<IvyJson>> {
        self.call(
            "render",
            IvyJson::object([("owner", owner), ("next", next)]),
        )
    }
    fn check(&self, definition: IvyJson) -> BoxFuture<'_, IvyResult<()>> {
        let call = self.call("check", IvyJson::object([("definition", definition)]));
        Box::pin(async move { call.await.map(|_| ()) })
    }
    fn apply(
        &self,
        owner: IvyJson,
        desired: IvyJson,
        plans: Vec<IvyJson>,
    ) -> BoxFuture<'_, IvyResult<()>> {
        let call = self.call(
            "apply",
            IvyJson::object([
                ("owner", owner),
                ("desired", desired),
                ("plans", IvyJson::Array(plans)),
            ]),
        );
        Box::pin(async move { call.await.map(|_| ()) })
    }
    fn reload(&self) -> BoxFuture<'_, IvyResult<()>> {
        Box::pin(async { Ok(()) })
    }
    fn pause(
        &self,
        snapshot: IvyJson,
        plans: Vec<IvyJson>,
    ) -> Option<BoxFuture<'_, IvyResult<()>>> {
        let call = self.call(
            "pause",
            IvyJson::object([("snapshot", snapshot), ("plans", IvyJson::Array(plans))]),
        );
        Some(Box::pin(async move { call.await.map(|_| ()) }))
    }
    fn resume(
        &self,
        snapshot: IvyJson,
        plans: Vec<IvyJson>,
    ) -> Option<BoxFuture<'_, IvyResult<()>>> {
        let call = self.call(
            "resume",
            IvyJson::object([("snapshot", snapshot), ("plans", IvyJson::Array(plans))]),
        );
        Some(Box::pin(async move { call.await.map(|_| ()) }))
    }
    fn can_pause_and_resume(&self) -> bool {
        true
    }
    fn launch_automatic(
        &self,
        plan: IvyJson,
        request: PathBuf,
        operation: String,
    ) -> Option<BoxFuture<'_, IvyResult<()>>> {
        let this = self.clone();
        let owner = tokio::spawn(async move {
            let data = tokio::task::spawn_blocking(move || {
                automatic::windows_request(plan, request, operation)
            })
            .await
            .map_err(|_| crate::IvyError::internal())??;
            this.call("launch", data).await.map(|_| ())
        });
        Some(Box::pin(async move {
            owner.await.map_err(|_| crate::IvyError::internal())?
        }))
    }
}
