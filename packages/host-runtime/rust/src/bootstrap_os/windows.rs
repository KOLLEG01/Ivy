//! Scheduler calls stay bounded; admitted service work retains its own lifetime.
use super::{BootstrapOs, WindowsBootstrapOs};
use crate::{
    IvyError, IvyJson as J, IvyResult, Outcome, config,
    platform::windows_process_inspection::ProcessExit, runtime_owner::stop::shutdown_control,
};
use ivy_contracts::core_schema::validate_host;
use std::time::Duration;

fn conflict() -> IvyError {
    IvyError::new(
        "target_conflict",
        "The selected bootstrap process no longer matches its retained identity.",
    )
}
fn unknown() -> IvyError {
    IvyError::new(
        "outcome_unknown",
        "The selected bootstrap process did not establish a stopped outcome.",
    )
    .with_outcome(Outcome::Unknown)
}

pub(crate) async fn stop(selection: &J) -> IvyResult<()> {
    let owner = selection.required("owner")?;
    if owner.get("running") == Some(&false.into()) {
        return Ok(());
    }
    let path = config::path_value(selection.required("configPath")?)?;
    let configuration = config::InstanceConfig::from_json(
        config::json_file(path, config::CONFIGURATION_BYTES).await?,
    )?;
    if ["instanceId", "componentId"]
        .iter()
        .any(|key| configuration.json().get(key) != owner.get(key))
        || configuration.json().get("buildId") != owner.get("candidateId")
    {
        return Err(conflict());
    }
    let health_path = configuration.data_root().join("health.json");
    let health = config::json_file(&health_path, config::CONFIGURATION_BYTES).await?;
    validate_host("Health", &health)?;
    if ["instanceId", "componentId", "buildId"]
        .iter()
        .any(|key| health.get(key) != configuration.json().get(key))
    {
        return Err(conflict());
    }
    let launch = health
        .required("launchId")?
        .as_js_string()
        .filter(|id| !id.0.is_empty())
        .ok_or_else(conflict)?;
    let pid = health.required("pid")?.as_f64().ok_or_else(conflict)? as u32;
    let process = match ProcessExit::open(pid) {
        Ok(process) => process,
        Err(error) if error.raw_os_error() == Some(87) => return Ok(()),
        Err(_) => return Err(unknown()),
    };
    if process.has_exited().map_err(|_| unknown())? {
        return Ok(());
    }
    let current = config::json_file(&health_path, config::CONFIGURATION_BYTES).await?;
    if [
        "pid",
        "bootId",
        "launchId",
        "instanceId",
        "componentId",
        "buildId",
    ]
    .iter()
    .any(|key| current.get(key) != health.get(key))
    {
        return Err(conflict());
    }
    let requested_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let control = shutdown_control(
        &health,
        owner
            .required("instanceId")?
            .as_js_string()
            .ok_or_else(conflict)?,
        launch,
        &requested_at,
    )
    .ok_or_else(conflict)?;
    config::atomic_json(configuration.data_root().join("control.json"), control).await?;
    while !process.has_exited().map_err(|_| unknown())? {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

pub(super) async fn pause(os: WindowsBootstrapOs, snapshot: J, plans: Vec<J>) -> IvyResult<()> {
    let selected = os
        .call(
            "pause",
            J::object([
                ("snapshot", snapshot.clone()),
                ("plans", J::Array(plans.clone())),
            ]),
        )
        .await?;
    let mut selected = selected.as_array().ok_or_else(conflict)?.to_vec();
    let expected = snapshot
        .required("owners")?
        .as_array()
        .ok_or_else(conflict)?;
    if selected.len() != expected.len()
        || selected.iter().any(|selection| {
            !expected.iter().any(|owner| {
                selection.get("owner").is_some_and(|current| {
                    [
                        "instanceId",
                        "componentId",
                        "name",
                        "candidateId",
                        "definitionHash",
                        "enabled",
                        "running",
                    ]
                    .iter()
                    .all(|key| current.get(key) == owner.get(key))
                })
            })
        })
    {
        return Err(conflict());
    }
    // ServiceManager must drain its services while HostExecutor remains available.
    selected.sort_by_key(|value| {
        value
            .get("owner")
            .and_then(|owner| owner.get("componentId"))
            == Some(&"host-executor".into())
    });
    for selection in &selected {
        stop(selection).await?;
    }
    loop {
        let current = os.inspect(plans.clone()).await?;
        if current.len() != expected.len()
            || current.iter().any(|owner| {
                owner.get("enabled") != Some(&false.into())
                    || !expected.iter().any(|prior| {
                        ["instanceId", "name", "candidateId", "definitionHash"]
                            .iter()
                            .all(|key| owner.get(key) == prior.get(key))
                    })
            })
        {
            return Err(conflict());
        }
        if current
            .iter()
            .all(|owner| owner.get("running") == Some(&false.into()))
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
