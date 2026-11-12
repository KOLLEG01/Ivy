//! Native host startup. The shared coordinator owns activation and recovery.
use ivy_contracts::{IvyError, IvyJson, IvyResult};
use ivy_host_runtime::{
    config::InstanceConfig,
    journal::HostJournal,
    node_path,
};
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;

pub const USAGE: &str = "ivy-host-executor [--config ABS_HOST_CONFIG] [--instance-config ABS_HOST_INSTANCE_CONFIG] [--distribution ABS_ARTIFACT_ROOT] [--job-launcher ABS_EXECUTABLE]";

/// The normal service owns the existing configuration, activation and update lanes.
pub async fn run_native(args: &[OsString], stop: CancellationToken) -> IvyResult<()> {
    use ivy_host_runtime::{
        artifact, host_config::HostContext, native_plan, node_runtime::NodeRuntime,
        package_updater::NativePackageClient, portable_http::PortableHttp,
        prepare::PreparationRuntime,
    };
    let started = crate::process_started_at();
    let mut values = BTreeMap::new();
    let mut input = args.iter();
    while let Some(key) = input.next() {
        if ![
            "--config",
            "--instance-config",
            "--distribution",
            "--job-launcher",
        ]
        .iter()
        .any(|allowed| key == *allowed)
        {
            return Err(invalid(USAGE));
        }
        let value = input
            .next()
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| invalid(USAGE))?;
        if values.insert(key.clone(), value).is_some() {
            return Err(invalid("Native service options may appear only once."));
        }
    }
    let context = HostContext::current()?;
    let startup = crate::admit(args, &context.environment, &context.account_home, None).await?;
    if let Some(own) = &startup.own {
        matching_instance(&startup.host, own, "host-executor").await?;
    }
    let distribution = values
        .remove(&OsString::from("--distribution"))
        .or_else(|| {
            startup
                .own
                .as_ref()
                .map(|own| own.artifact_root().to_path_buf())
        })
        .ok_or_else(|| {
            invalid("Native host startup requires its instance configuration or --distribution.")
        })?;
    let configured = startup
        .host
        .required("executables")?
        .get("node")
        .map(path)
        .transpose()?;
    let node = NodeRuntime::discover_optional(configured.as_deref()).await?;
    let job = values
        .remove(&OsString::from("--job-launcher"))
        .unwrap_or_else(|| native_plan::native_binary_path(&distribution, "ivy-job"));
    let options = ivy_host_runtime::executor::ExecutorOptions {
        own_instance_id: None,
        account_home: context.account_home,
        build_id: startup
            .own
            .as_ref()
            .and_then(|own| own.json().get("buildId"))
            .and_then(IvyJson::as_js_string)
            .cloned(),
    };
    let preparation = PreparationRuntime {
        node,
        compare: artifact::native_file_compare()?,
        http: PortableHttp::new(Default::default())?,
    };
    let bindings = crate::Bindings::native_for_startup(
        &startup,
        preparation,
        distribution,
        job,
        options,
        Some(Arc::new(NativePackageClient::new()?)),
        started,
    )?;
    crate::serve(startup, bindings, stop).await
}

fn invalid(message: &str) -> IvyError {
    IvyError::new("invalid_arguments", message)
}
fn path(value: &IvyJson) -> IvyResult<PathBuf> {
    value
        .as_js_string()
        .map(node_path::to_native)
        .ok_or_else(IvyError::internal)
}
async fn matching_instance(
    host: &IvyJson,
    instance: &InstanceConfig,
    component: &str,
) -> IvyResult<()> {
    let value = instance.json();
    let matched = value.get("componentId") == Some(&component.into())
        && value.get("hostId") == host.get("hostId")
        && host
            .required("instances")?
            .as_array()
            .is_some_and(|instances| {
                instances.iter().any(|entry| {
                    ["instanceId", "serviceNodeId", "componentId"]
                        .iter()
                        .all(|key| entry.get(key) == value.get(key))
                        && entry.get("engine") == Some(&"process".into())
                })
            });
    if !matched {
        return Err(invalid(
            "Native service configuration must match a process instance on this host.",
        ));
    }
    // Initial installation selects and enables the durable release before its
    // OS owner starts; the configuration's disabled default remains unchanged.
    let journal = HostJournal::open(host.clone()).await?;
    let installed = journal
        .installed(&value.required("instanceId")?.as_str()?)
        .await;
    let closed = journal.close().await;
    let installed = installed?;
    closed?;
    let configured = host
        .required("instances")?
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry.get("instanceId") == value.get("instanceId"))
        .unwrap();
    let enabled = installed
        .as_ref()
        .and_then(|record| record.get("enabled"))
        .unwrap_or(configured.required("enabled")?);
    if enabled != &true.into()
        || installed
            .as_ref()
            .is_some_and(|record| record.get("buildId") != value.get("buildId"))
    {
        return Err(invalid(
            "Native service configuration must match its enabled installed process on this host.",
        ));
    }
    Ok(())
}
