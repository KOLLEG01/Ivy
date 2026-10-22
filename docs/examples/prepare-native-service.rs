//! Initial offline setup for an already-built native service on an isolated host.
//! Uses the normal journal APIs; it does not claim deployment readiness or replace
//! an installed service. Start ServiceManager after this command returns.
//!
//! Run with `cargo run --locked -p ivy-service-manager --example
//! prepare-native-service -- ABS_HOST_CONFIG ABS_INSTANCE_CONFIG
//! ABS_RELEASE_MANIFEST OPERATION_ID`. The manifest and native executables must
//! already belong to the host artifact root. The result remains `activating`
//! until the deployment coordinator verifies the running service.
use ivy_contracts::{IvyError, IvyJson, IvyResult, json};
use ivy_host_runtime::{
    accepted_configuration::accepted_instance_configuration_path,
    artifact, config,
    executor_lock::ExecutorLock,
    host_config,
    journal::HostJournal,
    layout::service_paths,
    node_path,
    node_runtime::NodeRuntime,
    process::{self, Command},
    runtime_maintenance,
};
use std::{collections::BTreeMap, path::PathBuf};

fn require(condition: bool, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new("invalid_arguments", message))
    }
}
fn path(value: &IvyJson) -> IvyResult<PathBuf> {
    value
        .as_js_string()
        .map(node_path::to_native)
        .ok_or_else(IvyError::internal)
}
fn command(value: &IvyJson) -> IvyResult<Command> {
    Ok(Command {
        executable: value.required("executable")?.as_str()?,
        args: value
            .required("args")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .map(|v| v.as_str().map(Into::into))
            .collect::<IvyResult<_>>()?,
        cwd: value.get("cwd").map(path).transpose()?,
        timeout_ms: value
            .required("timeoutMs")?
            .as_f64()
            .ok_or_else(IvyError::internal)? as u64,
    })
}

async fn prepare(
    host: IvyJson,
    instance_path: PathBuf,
    manifest_path: PathBuf,
    operation: String,
) -> IvyResult<IvyJson> {
    require(
        !runtime_maintenance::runtime_reset_active(&host).await?,
        "Finish runtime reset before setup.",
    )?;
    let runtime = path(host.required("runtimeRoot")?)?;
    let _host_lock = ExecutorLock::acquire(runtime.join("executor"), false).await?;
    let configured_node = host
        .required("executables")?
        .get("node")
        .map(path)
        .transpose()?;
    let node = NodeRuntime::discover(configured_node.as_deref()).await?;
    let instance = config::instance_config(instance_path, &node).await?;
    let value = instance.json();
    let id = value.required("instanceId")?.as_str()?;
    require(
        !["host-executor", "service-manager"]
            .contains(&value.required("componentId")?.as_str()?.as_str()),
        "This example prepares service children, not the bootstrap owners.",
    )?;
    let configured = host
        .required("instances")?
        .as_array()
        .ok_or_else(IvyError::internal)?
        .iter()
        .find(|v| v.get("instanceId") == value.get("instanceId"))
        .ok_or_else(|| IvyError::new("not_found", "Instance is not configured on this host."))?;
    require(
        value.get("hostId") == host.get("hostId")
            && ["componentId", "serviceNodeId"]
                .iter()
                .all(|key| value.get(key) == configured.get(key))
            && configured.get("engine") == Some(&"process".into())
            && configured.get("enabled") == Some(&true.into()),
        "Instance must identify an enabled local process service.",
    )?;
    let storage = service_paths(&host, configured)?;
    for (instance_key, storage_key) in [
        ("dataRoot", "data"),
        ("workRoot", "work"),
        ("logsRoot", "logs"),
    ] {
        if let Some(selected) = value.get(instance_key) {
            require(
                path(selected)? == path(storage.required(storage_key)?)?,
                "Instance storage must match the host layout.",
            )?;
        }
    }
    let _owner_lock = ExecutorLock::acquire(runtime.join("owners").join(&id), false).await?;
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let candidate = IvyJson::object([
        ("candidateId", value.required("buildId")?.clone()),
        ("componentId", value.required("componentId")?.clone()),
        ("buildId", value.required("buildId")?.clone()),
        ("artifactRoot", value.required("artifactRoot")?.clone()),
        (
            "manifestPath",
            IvyJson::String(node_path::from_native(&manifest_path)),
        ),
        ("createdAt", now.clone().into()),
        (
            "platform",
            IvyJson::object([
                ("os", if cfg!(windows) { "win32" } else { "linux" }.into()),
                (
                    "arch",
                    if cfg!(target_arch = "aarch64") {
                        "arm64"
                    } else {
                        "x64"
                    }
                    .into(),
                ),
                ("node", node.version.clone().into()),
            ]),
        ),
    ]);
    let manifest = artifact::verify_candidate(
        candidate.clone(),
        path(host.required("artifactRoot")?)?,
        &node,
    )
    .await?;
    let executables = host
        .required("executables")?
        .as_object()
        .ok_or_else(IvyError::internal)?
        .iter()
        .map(|(name, value)| Ok((name.to_utf8()?, path(value)?)))
        .collect::<IvyResult<BTreeMap<_, _>>>()?;
    for selected in [
        manifest.required("entrypoint")?,
        manifest.required("readiness")?.required("command")?,
    ] {
        let selected = command(selected)?;
        require(
            selected.executable != "node",
            "Supply direct native entry and health executables.",
        )?;
        process::resolve_command(&selected, instance.artifact_root(), &executables)?;
    }
    let journal = HostJournal::open(host.clone()).await?;
    let result = async {
        require(journal.target(&id).await?.is_none() && journal.installed(&id).await?.is_none() &&
            !journal.unfinished().await?.iter().any(|v| v.get("record").and_then(|v| v.get("instanceId")) == value.get("instanceId")),
            "Initial setup requires an unused instance; inspect its original operation before retrying.")?;
        journal.save_candidate(candidate, manifest).await?;
        let entry = journal.accept(IvyJson::object([
            ("action", "deploy".into()), ("operationId", operation.into()),
            ("instanceId", id.clone().into()), ("candidateId", value.required("buildId")?.clone()),
        ]), None).await?;
        let deployment = entry.required("record")?.required("deploymentId")?.as_str()?;
        let selected_path = accepted_instance_configuration_path(&host, &deployment)?;
        config::atomic_json(selected_path.clone(), value.clone()).await?;
        let target = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)), ("instanceId", id.into()),
            ("revision", deployment.clone().into()), ("candidateId", value.required("buildId")?.clone()),
            ("desired", "running".into()), ("configPath", IvyJson::String(node_path::from_native(&selected_path))),
            ("requestedAt", now.into()),
        ]);
        journal.advance(&deployment, "prepared", "checking", IvyJson::object([] as [(&str, IvyJson); 0])).await?;
        journal.advance(&deployment, "checking", "draining", IvyJson::object([] as [(&str, IvyJson); 0])).await?;
        let entry = journal.advance(&deployment, "draining", "activating", IvyJson::object([("target", target.clone())])).await?;
        require(journal.accepted_target_policy(target.clone()).await?.is_some(), "Target policy association is unavailable.")?;
        Ok(IvyJson::object([
            ("record", entry.required("record")?.clone()), ("target", target),
            ("readinessVerified", false.into()),
        ]))
    }.await;
    let closed = journal.close().await;
    match (result, closed) {
        (Err(error), _) | (_, Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

#[tokio::main]
async fn main() {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let result = async {
        require(args.len() == 4, "Usage: prepare-native-service ABS_HOST_CONFIG ABS_INSTANCE_CONFIG ABS_RELEASE_MANIFEST OPERATION_ID")?;
        let paths = args[..3].iter().map(PathBuf::from).collect::<Vec<_>>();
        require(paths.iter().all(|p| p.is_absolute()), "All configuration and manifest paths must be absolute.")?;
        let operation = args[3].to_str().ok_or_else(IvyError::internal)?.to_owned();
        let host = host_config::host_config(paths[0].clone()).await?;
        prepare(host, paths[1].clone(), paths[2].clone(), operation).await
    }.await;
    let (value, code) = match result {
        Ok(value) => (value, 0),
        Err(error) => (
            IvyJson::object([
                ("code", error.code.into()),
                ("message", error.message.into()),
            ]),
            1,
        ),
    };
    println!(
        "{}",
        json::stringify(&value, 65536).expect("bounded setup result")
    );
    std::process::exit(code);
}
