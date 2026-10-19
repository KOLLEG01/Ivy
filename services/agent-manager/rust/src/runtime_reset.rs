//! Consume the existing host reset intent before creating any native owner.
use crate::native_launch::NativeHomeInputs;
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::{
    codex_home::resolve_codex_home_json,
    config::{CONFIGURATION_BYTES, atomic_json, json_file, json_file_if_present},
    filesystem::remove_force,
    node_path, paths,
};
use ivy_sdk::{clock::parse_date, native_evidence::native_instant};
use std::{fs, path::PathBuf, sync::Arc};

const RESET_PATHS: [&str; 8] = [
    "sessions",
    "archived_sessions",
    "history.jsonl",
    "state_5.sqlite",
    "state_5.sqlite-wal",
    "state_5.sqlite-shm",
    "shell_snapshots",
    "tmp",
];
pub type ResetClock = dyn Fn() -> JsString + Send + Sync;

fn truthy(value: &IvyJson) -> bool {
    match value {
        IvyJson::Null | IvyJson::Bool(false) => false,
        IvyJson::Number(value) => *value != 0.0 && !value.is_nan(),
        IvyJson::String(value) => !value.0.is_empty(),
        _ => true,
    }
}
fn invalid_marker() -> IvyError {
    IvyError::new(
        "invalid_arguments",
        "AgentManager runtime reset marker is invalid.",
    )
}
fn valid_marker(value: &IvyJson) -> IvyResult<()> {
    let bounded = |key| {
        value
            .get(key)
            .and_then(IvyJson::as_js_string)
            .is_some_and(|s| !s.0.is_empty() && s.0.len() <= 128)
    };
    if value.get("schemaVersion") != Some(&IvyJson::Number(1.0))
        || !bounded("resetId")
        || !bounded("runtimeEpoch")
    {
        return Err(invalid_marker());
    }
    let at = value
        .get("completedAt")
        .and_then(IvyJson::as_js_string)
        .ok_or_else(invalid_marker)?;
    // The original toISOString throws for Invalid Date. A finite but
    // noncanonical date instead fails the marker's equality requirement.
    if !parse_date(at).is_finite() {
        return Err(IvyError::internal());
    }
    native_instant(at, None).map_err(|_| invalid_marker())?;
    Ok(())
}
/// Preserve the existing explicit-home scope, including an explicit shared
/// home. Selecting a different reset ownership policy needs a separate decision.
/// Admission is eager: abandoning the waiter never interrupts an accepted reset
/// between removal and its existing acknowledgement file.
pub fn apply_agent_runtime_reset(
    data_root: PathBuf,
    settings: IvyJson,
    home: NativeHomeInputs,
) -> impl std::future::Future<Output = IvyResult<()>> + Send {
    apply_agent_runtime_reset_with_clock(
        data_root,
        settings,
        home,
        Arc::new(|| {
            chrono::Utc::now()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                .into()
        }),
    )
}
pub fn apply_agent_runtime_reset_with_clock(
    data_root: PathBuf,
    settings: IvyJson,
    home: NativeHomeInputs,
    now: Arc<ResetClock>,
) -> impl std::future::Future<Output = IvyResult<()>> + Send {
    let task = tokio::spawn(apply(data_root, settings, home, now));
    async move { task.await.map_err(|_| IvyError::internal())? }
}
async fn apply(
    data_root: PathBuf,
    settings: IvyJson,
    home: NativeHomeInputs,
    now: Arc<ResetClock>,
) -> IvyResult<()> {
    let marker_path = node_path::filesystem_path(&node_path::join(
        home.paths.style,
        &[
            node_path::from_native(&data_root),
            "runtime-reset.json".into(),
        ],
    ));
    let Some(mut marker) = json_file_if_present(marker_path, CONFIGURATION_BYTES)
        .await?
        .filter(truthy)
    else {
        return Ok(());
    };
    valid_marker(&marker)?;
    let applied_path = node_path::filesystem_path(&node_path::join(
        home.paths.style,
        &[
            node_path::from_native(&data_root),
            "runtime-reset-applied.json".into(),
        ],
    ));
    let applied = json_file(&applied_path, CONFIGURATION_BYTES)
        .await
        .unwrap_or(IvyJson::Null);
    if applied.get("schemaVersion") == Some(&IvyJson::Number(1.0))
        && applied.get("resetId") == marker.get("resetId")
        && applied.get("runtimeEpoch") == marker.get("runtimeEpoch")
    {
        return Ok(());
    }
    if settings.is_null() {
        return Err(IvyError::internal());
    }
    if settings.get("codexHome").is_none() && settings.get("nativeHome").is_none() {
        return Err(IvyError::new(
            "unsupported_storage",
            "Runtime reset requires an explicitly configured Codex home.",
        ));
    }
    let configured = home
        .paths
        .resolve(&[resolve_codex_home_json(&settings, &home.context())?.path]);
    if node_path::dirname(home.paths.style, &configured) == configured {
        return Err(IvyError::new(
            "target_conflict",
            "Codex home cannot be a filesystem root.",
        ));
    }
    let configured_path = node_path::filesystem_path(&configured);
    let actual = tokio::task::spawn_blocking(move || {
        let existing = match fs::symlink_metadata(&configured_path) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(IvyError::internal()),
        };
        if !existing.is_dir() || existing.file_type().is_symlink() {
            return Err(IvyError::new(
                "target_conflict",
                "Configured Codex home must be a real directory during runtime reset.",
            ));
        }
        paths::realpath(&configured_path)
            .map(|path| Some(node_path::from_native(&path)))
            .map_err(|_| IvyError::internal())
    })
    .await
    .map_err(|_| IvyError::internal())??;
    if let Some(actual) = actual {
        if node_path::dirname(home.paths.style, &actual) == actual {
            return Err(IvyError::new(
                "target_conflict",
                "Resolved Codex home cannot be a filesystem root.",
            ));
        }
        for path in RESET_PATHS {
            let target = home.paths.resolve(&[node_path::join(
                home.paths.style,
                &[actual.clone(), path.into()],
            )]);
            // All names above are fixed direct children. Check their absolute
            // resolved containment immediately before recursive removal.
            if target == actual || !home.paths.inside(&actual, &target) {
                return Err(IvyError::internal());
            }
            remove_force(node_path::filesystem_path(&target)).await?;
        }
    }
    marker.insert("appliedAt", IvyJson::String(now()));
    atomic_json(applied_path, marker).await
}
