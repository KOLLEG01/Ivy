//! Local reset checkpoints. The outer action admits the administrator, retains
//! ownership and awaits its real stop/clear/release stages around these writes.
use super::*;
use crate::{
    config,
    journal::{ExecutorLock, HostJournal},
    node_path, runtime_maintenance,
};
use chrono::{DateTime, SecondsFormat, Utc};
use ivy_contracts::{core_schema::validate_host, hash::hash_json};
use ivy_sdk::clock::Clock;
use std::{path::PathBuf, sync::Arc, time::Duration};

fn internal<T, E>(value: Result<T, E>) -> IvyResult<T> {
    value.map_err(|_| IvyError::internal())
}
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn field(value: &J, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
pub fn now(clock: &dyn Clock) -> IvyResult<J> {
    Ok(
        internal(DateTime::<Utc>::from_timestamp_millis(clock.utc_millis()).ok_or(()))?
            .to_rfc3339_opts(SecondsFormat::Millis, true)
            .into(),
    )
}
pub(super) fn require_publisher_digest(value: &JsString) -> IvyResult<()> {
    require(
        value.0.len() == 71
            && value.0.starts_with(&JsString::from("sha256:").0)
            && value.0[7..].iter().all(|v| matches!(*v, 48..=57 | 97..=102)),
        "invalid_arguments",
        "Runtime reset requires the checked ui publisher credential digest.",
    )
}
pub fn reset_record_path(config: &J) -> IvyResult<PathBuf> {
    Ok(runtime_maintenance::runtime_reset_marker(config)?
        .parent()
        .ok_or_else(IvyError::internal)?
        .join("current.json"))
}

/// Read the original candidate/manifest authority before publishing a hold.
pub async fn saved_reset_runtime(config: J) -> IvyResult<Vec<J>> {
    let journal = HostJournal::open(config.clone()).await?;
    let result = async {
        let mut saved = Vec::new();
        for instance in config
            .required("instances")?
            .as_array()
            .ok_or_else(IvyError::internal)?
        {
            let id = field(instance, "instanceId")?;
            let Some(installed) = journal.installed(&id.to_utf8()?).await? else {
                continue;
            };
            let candidate_id = field(&installed, "candidateId")?.to_utf8()?;
            saved.push(J::object([
                ("instanceId", J::String(id)),
                ("candidate", journal.candidate(&candidate_id).await?),
                ("manifest", journal.manifest(&candidate_id).await?),
                ("enabled", installed.required("enabled")?.clone()),
            ]));
        }
        Ok(saved)
    }
    .await;
    journal.close().await?;
    result
}

/// Publish the exact reset record, then its maintenance hold. The caller must
/// next stop and prove every writer; a published checkpoint alone is not proof.
/// An admitted write continues if its response waiter is dropped.
pub async fn prepare_reset_hold(
    config: J,
    plan: J,
    reset_id: JsString,
    runtime_epoch: JsString,
    publisher_credential_digest: JsString,
    clock: Arc<dyn Clock>,
) -> IvyResult<J> {
    internal(
        tokio::spawn(async move {
            require_publisher_digest(&publisher_credential_digest)?;
            let path = reset_record_path(&config)?;
            internal(
                tokio::fs::create_dir_all(path.parent().ok_or_else(IvyError::internal)?).await,
            )?;
            let previous = config::json_file(path.clone(), config::CONFIGURATION_BYTES)
                .await
                .ok();
            let plan_hash: J = hash_json(&plan)?.into();
            let reset_id = J::String(reset_id);
            let runtime_epoch = J::String(runtime_epoch);
            let digest = J::String(publisher_credential_digest);
            let same = previous.as_ref().is_some_and(|previous| {
                truthy(previous)
                    && previous.get("resetId") == Some(&reset_id)
                    && previous.get("runtimeEpoch") == Some(&runtime_epoch)
                    && previous.get("planHash") == Some(&plan_hash)
                    && previous.get("publisherCredentialDigest") == Some(&digest)
            });
            if let Some(previous) = previous.as_ref().filter(|v| truthy(v)) {
                require(
                    previous.get("deploymentComplete").is_some_and(truthy) || same,
                    "mutation_conflict",
                    "Another incomplete runtime reset must be resumed exactly.",
                )?;
            }
            let updated_at = now(clock.as_ref())?;
            let record = if same {
                let mut record = previous.unwrap();
                if record.get("instructions").is_none_or(J::is_null) {
                    record.insert("instructions", J::Array(Vec::new()));
                }
                if ["complete", "released"]
                    .iter()
                    .any(|phase| record.get("phase") == Some(&J::from(*phase)))
                {
                    record.insert("phase", "cleared".into());
                }
                record.insert("updatedAt", updated_at);
                record.insert("deploymentComplete", false.into());
                record
            } else {
                J::object([
                    ("schemaVersion", J::Number(1.0)),
                    (
                        "deploymentIdentity",
                        plan.required("deploymentIdentity")?.clone(),
                    ),
                    ("resetId", reset_id.clone()),
                    ("runtimeEpoch", runtime_epoch.clone()),
                    ("hostId", config.required("hostId")?.clone()),
                    ("phase", "prepared".into()),
                    ("updatedAt", updated_at),
                    ("planHash", plan_hash),
                    ("plan", plan),
                    (
                        "saved",
                        J::Array(saved_reset_runtime(config.clone()).await?),
                    ),
                    ("instructions", J::Array(Vec::new())),
                    ("publisherCredentialDigest", digest),
                    ("instructionsCaptured", false.into()),
                    ("deploymentComplete", false.into()),
                ])
            };
            config::atomic_json(path, record.clone()).await?;
            config::atomic_json(
                runtime_maintenance::runtime_reset_marker(&config)?,
                J::object([
                    ("schemaVersion", J::Number(1.0)),
                    (
                        "deploymentIdentity",
                        record.required("deploymentIdentity")?.clone(),
                    ),
                    ("resetId", reset_id),
                    ("runtimeEpoch", runtime_epoch),
                    ("hostId", config.required("hostId")?.clone()),
                    ("createdAt", record.required("updatedAt")?.clone()),
                ]),
            )
            .await?;
            Ok(record)
        })
        .await,
    )?
}

/// Advance only an admitted cleared record. An already complete record is
/// returned byte-for-byte without rewriting its retained completion evidence.
pub async fn complete_reset_record(
    config: J,
    reset_id: JsString,
    runtime_epoch: JsString,
    clock: Arc<dyn Clock>,
) -> IvyResult<J> {
    internal(
        tokio::spawn(async move {
            let path = reset_record_path(&config)?;
            let mut record = config::json_file(path.clone(), config::CONFIGURATION_BYTES).await?;
            if record.is_null() {
                return Err(IvyError::internal());
            }
            require(
                record.get("resetId") == Some(&J::String(reset_id))
                    && record.get("runtimeEpoch") == Some(&J::String(runtime_epoch))
                    && ["cleared", "complete"]
                        .iter()
                        .any(|phase| record.get("phase") == Some(&J::from(*phase))),
                "mutation_conflict",
                "Runtime reset host is not cleared for this identity.",
            )?;
            if record.get("phase") == Some(&"complete".into()) {
                return Ok(record);
            }
            record.insert("phase", "complete".into());
            record.insert("deploymentComplete", false.into());
            record.insert("updatedAt", now(clock.as_ref())?);
            config::atomic_json(path, record.clone()).await?;
            Ok(record)
        })
        .await,
    )?
}

/// Confirm only after the real release stage. A response loss retains the same
/// identity and confirmation; no completed operation is inferred or replayed.
pub async fn confirm_reset_record(
    config: J,
    reset_id: JsString,
    runtime_epoch: JsString,
    clock: Arc<dyn Clock>,
) -> IvyResult<J> {
    internal(
        tokio::spawn(async move {
            let path = reset_record_path(&config)?;
            let mut record = config::json_file(path.clone(), config::CONFIGURATION_BYTES).await?;
            if record.is_null() {
                return Err(IvyError::internal());
            }
            require(
                record.get("resetId") == Some(&J::String(reset_id))
                    && record.get("runtimeEpoch") == Some(&J::String(runtime_epoch))
                    && record.get("phase") == Some(&"released".into()),
                "mutation_conflict",
                "Runtime reset host is not released for this identity.",
            )?;
            if record.get("deploymentComplete").is_some_and(truthy) {
                return Ok(record);
            }
            record.insert("deploymentComplete", true.into());
            record.insert("updatedAt", now(clock.as_ref())?);
            config::atomic_json(path, record.clone()).await?;
            Ok(record)
        })
        .await,
    )?
}

/// Retained Linux bootstrap targets preserve their accepted configuration and
/// launch revision; the executor itself remains bootstrap-owned.
pub fn restored_bootstrap_target(
    plan: &J,
    instance_id: &JsString,
    candidate_id: &JsString,
    enabled: bool,
    requested_at: JsString,
) -> IvyResult<Option<J>> {
    let Some(process) = plan
        .required("processes")?
        .as_array()
        .ok_or_else(IvyError::internal)?
        .iter()
        .find(|process| {
            process.get("instanceId") == Some(&J::String(instance_id.clone()))
                && process.get("candidateId") == Some(&J::String(candidate_id.clone()))
                && process.get("configPath").is_some_and(truthy)
        })
    else {
        return Ok(None);
    };
    if process.get("componentId") == Some(&"host-executor".into()) {
        return Ok(None);
    }
    let target = J::object([
        ("schemaVersion", J::Number(1.0)),
        ("instanceId", J::String(instance_id.clone())),
        (
            "revision",
            crate::journal::bootstrap_launch_id_js(plan, instance_id)?.into(),
        ),
        ("candidateId", J::String(candidate_id.clone())),
        (
            "desired",
            if enabled { "running" } else { "stopped" }.into(),
        ),
        ("configPath", process.required("configPath")?.clone()),
        ("requestedAt", J::String(requested_at)),
    ]);
    validate_host("RuntimeTarget", &target)?;
    Ok(Some(target))
}

/// Reacquire each real executor/instance-owner kernel lock after writers stop.
/// Stale PID records never count as ownership or as proof of process exit.
pub async fn prove_reset_locks_released(config: J, clock: Arc<dyn Clock>) -> IvyResult<()> {
    internal(
        tokio::spawn(async move {
            let runtime = field(&config, "runtimeRoot")?;
            let mut names = vec![JsString::from("executor")];
            for instance in config
                .required("instances")?
                .as_array()
                .ok_or_else(IvyError::internal)?
            {
                names.push(JsString(
                    [
                        JsString::from("owners/").0,
                        field(instance, "instanceId")?.0,
                    ]
                    .concat(),
                ));
            }
            for name in names {
                let root = node_path::node_filesystem_path(&node_path::join(
                    node_path::Style::native(),
                    &[runtime.clone(), name],
                ))
                .map_err(|_| IvyError::internal())?;
                let end = clock.utc_millis() + 30_000;
                let mut lock = None;
                while lock.is_none() && clock.utc_millis() < end {
                    match ExecutorLock::acquire(root.clone(), false).await {
                        Ok(value) => lock = Some(value),
                        Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
                    }
                }
                let lock = lock.ok_or_else(|| {
                    IvyError::new(
                        "outcome_unknown",
                        "A reset writer still owns its maintenance lock.",
                    )
                })?;
                lock.close().await?;
            }
            Ok(())
        })
        .await,
    )?
}
