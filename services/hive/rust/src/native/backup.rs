//! Complete local snapshots: SQLite, immutable UI files and a publication record.
use super::{failure, require, string};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, json::canonical, limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_hive::{server_rpc::Storage, worker_client::RequestOptions, worker_runtime::StorageClient};
use regex::Regex;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub(super) struct Settings {
    pub directory: PathBuf,
    pub enabled: bool,
    pub interval: Duration,
    retain: usize,
}
impl Settings {
    pub fn new(value: &IvyJson) -> IvyResult<Self> {
        Ok(Self {
            directory: PathBuf::from(string(value, "directory")?),
            enabled: value.get("enabled").and_then(IvyJson::as_bool) != Some(false),
            interval: Duration::from_secs(
                value
                    .required("intervalHours")?
                    .as_f64()
                    .ok_or_else(IvyError::internal)? as u64
                    * 3600,
            ),
            retain: value
                .required("retain")?
                .as_f64()
                .ok_or_else(IvyError::internal)? as usize,
        })
    }
    pub async fn prepare(&self) -> IvyResult<Duration> {
        if self.enabled {
            tokio::fs::create_dir_all(&self.directory)
                .await
                .map_err(failure)?;
        }
        collect(&self.directory, self.enabled).await?;
        let mut newest = UNIX_EPOCH;
        for name in complete(&self.directory).await? {
            let modified = tokio::fs::metadata(self.directory.join(name))
                .await
                .map_err(failure)?
                .modified()
                .map_err(failure)?;
            newest = newest.max(modified);
        }
        Ok((newest + self.interval)
            .duration_since(SystemTime::now())
            .unwrap_or_default())
    }
}
fn names() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^(hive-\d{4}-\d{2}-\d{2}T[\d.-]+Z-[0-9a-f-]{36})\.(partial|ui\.partial|ui|sqlite(?:\.json|-wal|-shm)?)$").unwrap())
}
async fn entries(root: &Path) -> IvyResult<Vec<(String, std::fs::Metadata)>> {
    let mut directory = match tokio::fs::read_dir(root).await {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(failure(error)),
    };
    let mut result = vec![];
    while let Some(entry) = directory.next_entry().await.map_err(failure)? {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if names().is_match(&name) {
            result.push((
                name,
                tokio::fs::symlink_metadata(entry.path())
                    .await
                    .map_err(failure)?,
            ));
        }
    }
    Ok(result)
}
async fn remove(root: &Path, path: &Path) -> IvyResult<()> {
    require(
        root.is_absolute()
            && path
                .strip_prefix(root)
                .is_ok_and(|p| p.components().count() == 1),
        "Invalid backup cleanup path.",
    )?;
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(failure(error)),
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        tokio::fs::remove_dir_all(path).await.map_err(failure)?;
    } else if metadata.is_file() && !metadata.file_type().is_symlink() {
        tokio::fs::remove_file(path).await.map_err(failure)?;
    }
    Ok(())
}
pub(super) async fn collect(root: &Path, enabled: bool) -> IvyResult<()> {
    let entries = entries(root).await?;
    let present: HashSet<_> = entries.iter().map(|(name, _)| name.as_str()).collect();
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(86_400))
        .unwrap_or(UNIX_EPOCH);
    for (name, metadata) in &entries {
        if metadata.file_type().is_symlink()
            || enabled && metadata.modified().map_err(failure)? > cutoff
        {
            continue;
        }
        let captures = names().captures(name).unwrap();
        let base = &captures[1];
        if enabled
            && match &captures[2] {
                "ui" => {
                    present.contains(format!("{base}.sqlite").as_str())
                        && present.contains(format!("{base}.sqlite.json").as_str())
                }
                "sqlite" => present.contains(format!("{base}.sqlite.json").as_str()),
                "sqlite.json" | "sqlite-wal" | "sqlite-shm" => {
                    present.contains(format!("{base}.sqlite").as_str())
                }
                _ => false,
            }
        {
            continue;
        }
        remove(root, &root.join(name)).await?;
    }
    Ok(())
}
async fn complete(root: &Path) -> IvyResult<Vec<String>> {
    let entries = entries(root).await?;
    let present: HashSet<_> = entries
        .iter()
        .filter(|(_, metadata)| !metadata.file_type().is_symlink())
        .map(|(name, _)| name.as_str())
        .collect();
    let mut result = entries
        .iter()
        .filter(|(name, metadata)| {
            metadata.is_file()
                && !metadata.file_type().is_symlink()
                && name.ends_with(".sqlite")
                && present.contains(format!("{name}.json").as_str())
                && present.contains(name.replace(".sqlite", ".ui").as_str())
        })
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    result.sort_unstable_by(|a, b| b.cmp(a));
    Ok(result)
}

#[derive(Clone)]
pub(super) struct Plan {
    root: PathBuf,
    name: String,
}
impl Plan {
    pub fn new(root: PathBuf) -> Self {
        let date = chrono::Utc::now()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            .replace(':', "-");
        Self {
            root,
            name: format!("hive-{date}-{}", uuid::Uuid::new_v4()),
        }
    }
    fn path(&self, suffix: &str) -> PathBuf {
        self.root.join(format!("{}.{suffix}", self.name))
    }
    pub async fn discard_incomplete(&self) -> IvyResult<()> {
        if tokio::fs::try_exists(self.path("sqlite.json"))
            .await
            .map_err(failure)?
            && tokio::fs::try_exists(self.path("sqlite"))
                .await
                .map_err(failure)?
            && tokio::fs::try_exists(self.path("ui"))
                .await
                .map_err(failure)?
        {
            return Ok(());
        }
        for suffix in [
            "ui.partial",
            "ui",
            "partial",
            "partial-wal",
            "partial-shm",
            "sqlite",
            "sqlite-wal",
            "sqlite-shm",
            "sqlite.json",
        ] {
            remove(&self.root, &self.path(suffix)).await?;
        }
        Ok(())
    }
}
async fn copy_ui(source: &Path, destination: &Path) -> IvyResult<()> {
    tokio::fs::create_dir(destination).await.map_err(failure)?;
    let mut pending = vec![(source.to_owned(), destination.to_owned())];
    while let Some((source, target)) = pending.pop() {
        let mut directory = tokio::fs::read_dir(&source).await.map_err(failure)?;
        while let Some(entry) = directory.next_entry().await.map_err(failure)? {
            if target == destination
                && [".incoming", ".staging"]
                    .iter()
                    .any(|name| entry.file_name() == *name)
            {
                continue;
            }
            let kind = entry.file_type().await.map_err(failure)?;
            require(
                !kind.is_symlink(),
                "UI backup cannot follow filesystem links.",
            )?;
            let path = target.join(entry.file_name());
            if kind.is_dir() {
                tokio::fs::create_dir(&path).await.map_err(failure)?;
                pending.push((entry.path(), path));
            } else {
                require(kind.is_file(), "UI backup requires regular files.")?;
                tokio::fs::copy(entry.path(), path).await.map_err(failure)?;
            }
        }
    }
    Ok(())
}

pub(super) async fn snapshot(
    worker: Arc<StorageClient>,
    data: PathBuf,
    settings: Settings,
    plan: Plan,
    build_id: String,
) -> IvyResult<PathBuf> {
    let result = async {
        tokio::fs::create_dir_all(&settings.directory)
            .await
            .map_err(failure)?;
        collect(&settings.directory, true).await?;
        let before = Storage::request(
            worker.as_ref(),
            IvyJson::object([("action", "ui.pointers".into())]),
            RequestOptions::default(),
        )
        .await?;
        copy_ui(&data.join("ui-releases"), &plan.path("ui.partial")).await?;
        let result = Storage::request(
            worker.as_ref(),
            IvyJson::object([
                ("action", "backup".into()),
                (
                    "destination",
                    plan.path("partial").to_string_lossy().to_string().into(),
                ),
            ]),
            RequestOptions {
                timeout_ms: 600_000,
                backup: true,
                ..Default::default()
            },
        )
        .await?;
        let after = Storage::request(
            worker.as_ref(),
            IvyJson::object([("action", "ui.pointers".into())]),
            RequestOptions::default(),
        )
        .await?;
        if canonical(&before, MANAGEMENT_FRAME_BYTES)? != canonical(&after, MANAGEMENT_FRAME_BYTES)?
        {
            return Err(IvyError::new(
                "configuration_changed",
                "UI releases changed during the Hive backup.",
            ));
        }
        tokio::fs::rename(plan.path("ui.partial"), plan.path("ui"))
            .await
            .map_err(failure)?;
        tokio::fs::rename(plan.path("partial"), plan.path("sqlite"))
            .await
            .map_err(failure)?;
        ivy_host_runtime::config::atomic_json(
            plan.path("sqlite.json"),
            IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                ("buildId", build_id.into()),
                (
                    "createdAt",
                    chrono::Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                        .into(),
                ),
                ("uiFilesDirectory", format!("{}.ui", plan.name).into()),
                ("pages", result.required("pages")?.clone()),
                ("bytesHash", result.required("bytesHash")?.clone()),
            ]),
        )
        .await?;
        for name in complete(&settings.directory)
            .await?
            .into_iter()
            .skip(settings.retain)
        {
            for suffix in [
                name.clone(),
                format!("{name}.json"),
                name.replace(".sqlite", ".ui"),
            ] {
                remove(&settings.directory, &settings.directory.join(suffix)).await?;
            }
        }
        Ok(plan.path("sqlite"))
    }
    .await;
    if result.is_err() {
        let _ = plan.discard_incomplete().await;
    }
    result
}
