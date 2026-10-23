//! Current-state retention with durable retirement intent and replay evidence.
use crate::{
    IvyError, IvyJson as J, IvyResult, config,
    executor_lock::ExecutorLock,
    journal::HostJournal,
    node_path,
    node_runtime::NodeRuntime,
    storage_space::{self, RetentionProgress},
};
use ivy_contracts::{JsString, core_schema::validate_host, hash::hash_json};
use ivy_sdk::clock::Clock;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::UNIX_EPOCH,
};
mod history;

pub struct RetentionBindings {
    pub node: Option<NodeRuntime>,
    pub clock: Arc<dyn Clock>,
}
fn internal<T, E>(v: Result<T, E>) -> IvyResult<T> {
    v.map_err(|_| IvyError::internal())
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn array(v: &J) -> IvyResult<&[J]> {
    v.as_array().ok_or_else(IvyError::internal)
}
fn field(v: &J, k: &str) -> IvyResult<JsString> {
    v.required(k)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn path(v: &J, k: &str) -> IvyResult<PathBuf> {
    config::path_value(v.required(k)?)
}
fn joined(root: &Path, name: &str) -> PathBuf {
    node_path::to_native(&node_path::join(
        node_path::Style::native(),
        &[node_path::from_native(root), name.into()],
    ))
}
fn fs_path(path: &Path) -> PathBuf {
    node_path::filesystem_path(&node_path::from_native(path))
}
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> IvyResult<T> + Send + 'static,
) -> IvyResult<T> {
    internal(tokio::task::spawn_blocking(work).await)?
}
async fn metadata(path: PathBuf) -> IvyResult<Option<fs::Metadata>> {
    blocking(move || match fs::symlink_metadata(fs_path(&path)) {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(IvyError::internal()),
    })
    .await
}
async fn realpath(path: PathBuf) -> IvyResult<PathBuf> {
    blocking(move || internal(crate::paths::realpath(&path))).await
}
async fn entries(root: PathBuf) -> IvyResult<Vec<fs::DirEntry>> {
    blocking(move || {
        let values = match fs::read_dir(fs_path(&root)) {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(IvyError::internal()),
        };
        let mut values = internal(values.collect::<Result<Vec<_>, _>>())?;
        values.sort_by_key(|v| v.file_name());
        Ok(values)
    })
    .await
}
fn milliseconds(at: std::time::SystemTime) -> f64 {
    let (positive, d) = match at.duration_since(UNIX_EPOCH) {
        Ok(d) => (true, d),
        Err(e) => (false, e.duration()),
    };
    let ms = d.as_secs() as f64 * 1000.0 + d.subsec_nanos() as f64 / 1_000_000.0;
    if positive { ms } else { -ms }
}
fn timestamp(at: i64) -> IvyResult<J> {
    Ok(
        internal(chrono::DateTime::from_timestamp_millis(at).ok_or(()))?
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            .into(),
    )
}
fn hash_name(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}
fn version_name(value: &J) -> bool {
    let Some(value) = value.as_js_string().and_then(|v| v.to_utf8().ok()) else {
        return false;
    };
    let parts = value.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.bytes().all(|v| v.is_ascii_digit())
                && (p.len() == 1 || !p.starts_with('0'))
        })
}
#[derive(Default)]
struct Progress(Mutex<Option<J>>);
impl Progress {
    fn get(&self) -> Option<J> {
        self.0.lock().unwrap().clone()
    }
    fn set(&self, status: J) {
        *self.0.lock().unwrap() = Some(status);
    }
    fn update(&self, work: impl FnOnce(&mut J)) {
        if let Some(status) = self.0.lock().unwrap().as_mut() {
            work(status);
        }
    }
}
impl RetentionProgress for Progress {
    fn removed(&self, area: &str) {
        self.update(|status| {
            let mut removed = status.get("removed").unwrap().clone();
            let value = removed.get(area).and_then(J::as_f64).unwrap_or(f64::NAN);
            removed.insert(area, J::Number(value + 1.0));
            status.insert("removed", removed);
        });
    }
    fn skipped(&self, area: &str, reason: &str, attention: bool) {
        self.update(|status| {
            let mut entries = status.get("skipped").unwrap().as_array().unwrap().to_vec();
            let matches = |v: &J, a: &str, r: &str| {
                v.get("area") == Some(&a.into()) && v.get("reason") == Some(&r.into())
            };
            let (area, reason) =
                if entries.len() >= 63 && !entries.iter().any(|v| matches(v, area, reason)) {
                    ("other", "additional_retained_entries")
                } else {
                    (area, reason)
                };
            if let Some(entry) = entries.iter_mut().find(|v| matches(v, area, reason)) {
                entry.insert(
                    "count",
                    J::Number(entry.get("count").unwrap().as_f64().unwrap() + 1.0),
                );
                entry.insert(
                    "requiresAttention",
                    (entry.get("requiresAttention") == Some(&true.into()) || attention).into(),
                );
            } else {
                entries.push(J::object([
                    ("area", area.into()),
                    ("reason", reason.into()),
                    ("count", J::Number(1.0)),
                    ("requiresAttention", attention.into()),
                ]));
            }
            status.insert("skipped", J::Array(entries));
        });
    }
}
async fn owned_candidate(config: &J, id: &str) -> IvyResult<Option<(PathBuf, f64)>> {
    let parent = joined(&path(config, "artifactRoot")?, "candidates");
    let path = joined(&parent, id);
    let Some(info) = metadata(path.clone()).await? else {
        return Ok(None);
    };
    if !info.is_dir() || info.file_type().is_symlink() {
        return Ok(None);
    }
    let actual_parent = realpath(parent).await?;
    let actual = realpath(path.clone()).await?;
    if actual.parent() != Some(actual_parent.as_path())
        || actual.file_name() != Some(std::ffi::OsStr::new(id))
    {
        return Ok(None);
    }
    Ok(Some((path, milliseconds(internal(info.modified())?))))
}
async fn finish_removal(
    config: &J,
    journal: &HostJournal,
    candidate: &J,
    progress: &Progress,
) -> IvyResult<()> {
    let id = candidate.required("candidateId")?.as_str()?;
    let root = joined(
        &joined(&path(config, "artifactRoot")?, "candidates"),
        &id[7..],
    );
    require(
        candidate.get("artifactRoot")
            == Some(&crate::layout::path_json(&joined(&root, "artifact")))
            && candidate.get("manifestPath")
                == Some(&crate::layout::path_json(&joined(&root, "component.json")))
            && candidate
                .get("archivePath")
                .is_none_or(|v| *v == crate::layout::path_json(&joined(&root, "package.tar.gz"))),
        "target_conflict",
        "Retiring candidate paths changed.",
    )?;
    if let Some((directory, _)) = owned_candidate(config, &id[7..]).await? {
        crate::filesystem::remove_payload(directory, true, 0).await?;
    } else {
        require(
            metadata(root).await?.is_none(),
            "target_conflict",
            "Retiring candidate directory is no longer owned.",
        )?;
    }
    journal.finish_candidate_retirement(&id).await?;
    progress.removed("candidates");
    Ok(())
}
/// Work admitted by this call owns collection locks through status publication;
/// dropping its observer cannot release ownership while deletion is in flight.
pub async fn collect_host_storage(
    config: J,
    now: Option<i64>,
    trigger: Option<JsString>,
    bindings: Arc<RetentionBindings>,
) -> IvyResult<J> {
    let now = now.unwrap_or_else(|| bindings.clock.utc_millis());
    internal(
        tokio::spawn(collect(
            config,
            now,
            trigger.unwrap_or_else(|| "manual".into()),
            bindings,
        ))
        .await,
    )?
}
async fn collect(
    config: J,
    now: i64,
    trigger: JsString,
    bindings: Arc<RetentionBindings>,
) -> IvyResult<J> {
    let runtime = path(&config, "runtimeRoot")?;
    let lock = ExecutorLock::acquire(joined(&runtime, "artifact-retention-lock"), false).await?;
    let progress = Arc::new(Progress::default());
    let mut journal = None;
    let mut bootstrap_lock = None;
    let result: IvyResult<J> = async {
        journal = Some(HostJournal::open_with_clock(config.clone(), bindings.clock.clone()).await?);
        let journal = journal.as_ref().unwrap();
        let previous = journal.storage_retention_status().await?;
        let mut status = J::object([
            ("schemaVersion", J::Number(1.0)),
            ("hostId", config.required("hostId")?.clone()),
            ("state", "running".into()),
            ("trigger", J::String(trigger)),
            ("startedAt", timestamp(now)?),
            ("completedAt", J::Null),
            (
                "lastSuccessAt",
                previous
                    .as_ref()
                    .and_then(|v| v.get("lastSuccessAt"))
                    .cloned()
                    .unwrap_or(J::Null),
            ),
            (
                "removed",
                J::object(
                    [
                        "candidates",
                        "preparations",
                        "payloads",
                        "snapshots",
                        "maintenance",
                        "bootstrapPlans",
                    ]
                    .map(|k| (k, J::Number(0.0))),
                ),
            ),
            ("skipped", J::Array(vec![])),
            ("errorCode", J::Null),
        ]);
        for (k, v) in storage_space::host_storage_space(config.clone())
            .await?
            .as_object()
            .ok_or_else(IvyError::internal)?
        {
            status.insert(k.to_utf8()?, v.clone());
        }
        progress.set(status.clone());
        journal.save_storage_retention_status(status).await?;
        bootstrap_lock = Some(
            ExecutorLock::acquire(joined(&runtime, "bootstrap-maintenance-lock"), false).await?,
        );
        crate::dependency_cache::collect_dependency_caches(
            path(&config, "stagingRoot")?,
            progress.clone(),
        )
        .await?;
        let compacted = crate::preparations::compact_preparations(
            config.clone(),
            None,
            false,
            bindings.node.clone(),
        )
        .await?;
        for entry in array(compacted.required("entries")?)? {
            if ["compacted", "recovered"]
                .iter()
                .any(|a| entry.get("action") == Some(&(*a).into()))
            {
                progress.removed("payloads");
            } else if let Some(code) = entry
                .get("code")
                .filter(|v| !v.is_null() && **v != "already_compacted".into())
            {
                progress.skipped("payloads", &code.as_str()?, true);
            }
        }
        let cutoff = now as f64 - 3.0 * 24.0 * 60.0 * 60.0 * 1000.0;
        let preparation = crate::preparations::collect_preparation_storage(
            config.clone(),
            journal.clone(),
            cutoff,
            Some(progress.clone()),
        )
        .await?;
        let mut protected = preparation.protected_candidates;
        protected.extend(journal.retention_candidate_ids().await?);
        let mut history = history::load(&config, "bootstrap-maintenance", &progress).await?;
        history.extend(history::load(&config, "bootstrap-automatic", &progress).await?);
        history.sort_by(|a, b| b.at.cmp(&a.at));
        let retained = history::collect_plans(
            &config,
            journal,
            &mut protected,
            cutoff,
            &history,
            &progress,
        )
        .await?;
        history::collect_details(&config, &history, &retained, &progress).await?;
        for entry in &history {
            if entry.completed() {
                continue;
            }
            history::protect_plan(&mut protected, entry.request.required("next")?)?;
            for owner in array(entry.request.required("previous")?.required("owners")?)? {
                protected.insert(owner.required("candidateId")?.as_str()?);
            }
        }
        let candidates = journal.storage_retention_candidates().await?;
        for (candidate, manifest) in &candidates {
            if !manifest.get("version").is_some_and(version_name) {
                protected.insert(candidate.required("candidateId")?.as_str()?);
            }
        }
        let mut removed = 0;
        let mut failures = Vec::new();
        for candidate in journal.pending_candidate_retirements().await? {
            let mut build_lock = None;
            let removal: IvyResult<()> = async {
                let build = candidate.required("buildId")?.as_str()?;
                build_lock = Some(
                    ExecutorLock::acquire(
                        joined(
                            &joined(&path(&config, "stagingRoot")?, "build-locks"),
                            &build[7..],
                        ),
                        false,
                    )
                    .await?,
                );
                finish_removal(&config, journal, &candidate, &progress).await?;
                removed += 1;
                Ok(())
            }
            .await;
            if let Some(lock) = build_lock {
                lock.close().await?;
            }
            if let Err(error) = removal {
                progress.skipped("candidates", &error.code, true);
                failures.push(error);
            }
        }
        for (candidate, _) in candidates {
            let id = candidate.required("candidateId")?.as_str()?;
            if protected.contains(&id) {
                progress.skipped("candidates", "protected_release", false);
                continue;
            }
            let directory = owned_candidate(&config, &id[7..]).await?;
            if directory.as_ref().is_some_and(|(_, mtime)| *mtime > cutoff) {
                progress.skipped("candidates", "grace_period", false);
                continue;
            }
            let Some((directory, _)) = directory else {
                progress.skipped("candidates", "unrecognized_path", true);
                continue;
            };
            if candidate.get("artifactRoot")
                != Some(&crate::layout::path_json(&joined(&directory, "artifact")))
                || candidate.get("manifestPath")
                    != Some(&crate::layout::path_json(&joined(
                        &directory,
                        "component.json",
                    )))
                || candidate.get("archivePath").is_some_and(|v| {
                    *v != crate::layout::path_json(&joined(&directory, "package.tar.gz"))
                })
            {
                progress.skipped("candidates", "unrecognized_path", true);
                continue;
            }
            let matches =
                match crate::artifact::read_release_manifest(path(&candidate, "manifestPath")?)
                    .await
                {
                    Ok(manifest) => {
                        hash_json(&manifest)? == hash_json(&journal.manifest(&id).await?)?
                    }
                    Err(_) => false,
                };
            if !matches {
                progress.skipped("candidates", "unrecognized_manifest", true);
                continue;
            }
            let mut build_lock = None;
            let removal: IvyResult<()> = async {
                build_lock = Some(
                    ExecutorLock::acquire(
                        joined(
                            &joined(&path(&config, "stagingRoot")?, "build-locks"),
                            &id[7..],
                        ),
                        false,
                    )
                    .await?,
                );
                if !journal.retire_candidate(&id).await? {
                    progress.skipped("candidates", "protected_release", false);
                    return Ok(());
                }
                finish_removal(&config, journal, &candidate, &progress).await?;
                removed += 1;
                Ok(())
            }
            .await;
            if let Some(lock) = build_lock {
                lock.close().await?;
            }
            if let Err(error) = removal {
                progress.skipped("candidates", &error.code, true);
                if error.code != "executor_already_running" {
                    failures.push(error);
                }
            }
        }
        if let Some(error) = failures.into_iter().next() {
            return Err(error);
        }
        progress.update(|status| {
            let partial = status
                .get("skipped")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v.get("requiresAttention") == Some(&true.into()));
            status.insert(
                "state",
                if partial {
                    "partial".into()
                } else {
                    "succeeded".into()
                },
            );
        });
        Ok(J::object([
            ("candidates", J::Number(removed as f64)),
            ("preparations", J::Number(preparation.preparations as f64)),
            ("snapshots", J::Number(preparation.snapshots as f64)),
        ]))
    }
    .await;
    if let Err(error) = &result {
        progress.update(|status| {
            status.insert("state", "failed".into());
            status.insert("errorCode", error.code.clone().into());
        });
    }
    let finalized: IvyResult<()> = async {
        if let (Some(journal), Some(mut status)) = (&journal, progress.get()) {
            status.insert("completedAt", timestamp(bindings.clock.utc_millis())?);
            match storage_space::host_storage_space(config).await {
                Ok(space) => {
                    for (k, v) in space.as_object().ok_or_else(IvyError::internal)? {
                        status.insert(k.to_utf8()?, v.clone());
                    }
                }
                Err(_) => {
                    status.insert("state", "failed".into());
                    if status.get("errorCode").is_none_or(J::is_null) {
                        status.insert("errorCode", "storage_probe_failed".into());
                    }
                }
            }
            if status.get("state") == Some(&"succeeded".into()) {
                status.insert("lastSuccessAt", status.required("completedAt")?.clone());
            }
            journal.save_storage_retention_status(status).await?;
        }
        Ok(())
    }
    .await;
    if let Some(journal) = journal {
        journal.close().await?;
    }
    if let Some(lock) = bootstrap_lock {
        lock.close().await?;
    }
    lock.close().await?;
    finalized?;
    result
}
