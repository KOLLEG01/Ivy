//! Explicit maintenance after a verified enclosing Host owner shutdown.
use crate::process_evidence::OwnedProcessEvidence;
use ivy_contracts::{IvyError, IvyJson as J, IvyResult, core_schema::validate_host, native_backend::BackendIdentity};
use ivy_host_runtime::{agent_backends::configured_backends, config::{atomic_json, json_file}, journal::HostJournal, layout::service_paths, node_path, process_fence::{clear_process_stop_fence, process_stop_fence}};
use std::path::PathBuf;

fn require(value: bool) -> IvyResult<()> {
    if value { Ok(()) } else { Err(IvyError::new("native_stop_unverified", "The native fence does not belong to this stopped original owner.")) }
}
fn time(value: &J, key: &str) -> IvyResult<f64> {
    let result = ivy_sdk::clock::parse_date(value.required(key)?.as_js_string().ok_or_else(IvyError::internal)?);
    require(result.is_finite())?;
    Ok(result)
}

pub async fn recover(request_path: PathBuf) -> IvyResult<J> {
    let request = json_file(request_path, 16 * 1024).await?;
    require(request.as_object().is_some_and(|fields| fields.len() == 5))?;
    let host_path = node_path::filesystem_path(request.required("hostConfigPath")?.as_js_string().ok_or_else(IvyError::internal)?);
    require(host_path.is_absolute())?;
    let config = ivy_host_runtime::host_config::host_config(host_path).await?;
    let instance_id = request.required("instanceId")?.as_str()?;
    let deployment_id = request.required("deploymentId")?.as_str()?;
    let identity = BackendIdentity::from_json(request.required("backend")?)?;
    let fence = request.required("fence")?.clone();
    validate_host("ProcessStopFence", &fence)?;
    require(fence.get("owner") == Some(&"native".into()))?;
    let journal = HostJournal::open(config).await?;
    let handle = tokio::runtime::Handle::current();
    let result = journal.with_stopped_owner(instance_id.clone(), deployment_id.clone(), move |policy, previous, stopped| {
        require(time(&fence, "observedAt")? >= time(&previous, "requestedAt")?
            && time(&fence, "observedAt")? <= time(&stopped, "observedAt")?)?;
        let instance = policy.required("instances")?.as_array().ok_or_else(IvyError::internal)?.iter()
            .find(|value| value.get("instanceId") == Some(&instance_id.clone().into())).ok_or_else(IvyError::internal)?;
        require(instance.get("componentId") == Some(&"agent-manager".into()))?;
        let paths = service_paths(&policy, instance)?;
        let backends = configured_backends(instance.required("settings")?, paths.get("data"))?;
        let backend = backends.iter().find(|backend| backend.identity.as_ref() == Some(&identity)).ok_or_else(IvyError::internal)?;
        require(!ivy_host_runtime::codex_home::uses_external_app_server_json(backend.settings)?)?;
        let root = node_path::filesystem_path(backend.data_root.ok_or_else(IvyError::internal)?.as_js_string().ok_or_else(IvyError::internal)?);
        handle.block_on(async move {
            require(process_stop_fence(root.clone()).await?.as_ref() == Some(&fence))?;
            let epoch = fence.required("ownerId")?.as_js_string().ok_or_else(IvyError::internal)?.clone();
            // Retain the proof before releasing admission. Neither the native
            // request's unknown outcome nor the stored turn is rewritten.
            atomic_json(root.join("native-stop-recovery.json"), J::object([
                ("schemaVersion", J::Number(1.0)), ("backend", identity.json()),
                ("deploymentId", deployment_id.clone().into()), ("fence", fence.clone()),
                ("stoppedOwner", stopped),
            ])).await?;
            OwnedProcessEvidence::new(root.clone()).retain(epoch.clone()).await?;
            require(clear_process_stop_fence(root, fence).await?)?;
            Ok(J::object([("ok", true.into()), ("backend", identity.json()),
                ("epoch", J::String(epoch)), ("deploymentId", deployment_id.into())]))
        })
    }).await;
    let closed = journal.close().await;
    let value = result?;
    closed?;
    Ok(value)
}
