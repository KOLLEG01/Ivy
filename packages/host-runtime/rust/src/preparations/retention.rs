use super::*;
use crate::storage_space::RetentionProgress;
use std::{collections::BTreeSet, sync::Arc, time::UNIX_EPOCH};

pub struct PreparationStorageCollection {
    pub preparations: usize,
    pub snapshots: usize,
    pub protected_candidates: BTreeSet<String>,
}
fn uuid_name(name: &str) -> bool {
    build_name(&format!("build-{name}"))
}
fn truthy(v: &IvyJson) -> bool {
    match v {
        IvyJson::Null => false,
        IvyJson::Bool(v) => *v,
        IvyJson::Number(v) => *v != 0.0 && !v.is_nan(),
        IvyJson::String(v) => !v.0.is_empty(),
        _ => true,
    }
}
fn skipped(
    progress: &Option<Arc<dyn RetentionProgress>>,
    area: &str,
    reason: &str,
    attention: bool,
) {
    if let Some(progress) = progress {
        progress.skipped(area, reason, attention);
    }
}
async fn remove_retired(
    path: PathBuf,
    area: &'static str,
    progress: &Option<Arc<dyn RetentionProgress>>,
) -> usize {
    let checked = path.clone();
    let result = async {
        blocking(move || {
            let metadata = internal(fs::symlink_metadata(fs_path(&checked)))?;
            let actual = internal(realpath(&checked))?;
            let parent = checked.parent().ok_or_else(IvyError::internal)?;
            require(
                metadata.is_dir()
                    && !metadata.file_type().is_symlink()
                    && actual.parent() == Some(internal(realpath(parent))?.as_path())
                    && actual.file_name() == checked.file_name(),
                "target_conflict",
                "Retired staging directory must remain directly owned.",
            )?;
            #[cfg(unix)]
            crate::backup_files::sync_file(internal(fs::File::open(fs_path(parent)))?)?;
            Ok(())
        })
        .await?;
        crate::filesystem::remove_payload(path, true, 0).await
    }
    .await;
    match result {
        Ok(()) => {
            if let Some(progress) = progress {
                progress.removed(area);
            }
            1
        }
        Err(error) => {
            skipped(progress, area, &error.code, true);
            0
        }
    }
}
async fn retired_directories(
    parent: PathBuf,
    preparations: bool,
    progress: &Option<Arc<dyn RetentionProgress>>,
) -> IvyResult<usize> {
    let scan = parent.clone();
    let names = blocking(move || {
        let entries = match fs::read_dir(fs_path(&scan)) {
            Ok(value) => value,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(IvyError::internal()),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = internal(entry)?;
            let name = node_path::node_utf8(&entry.file_name());
            if let Ok(name) = name.to_utf8() {
                if name.strip_prefix(".retiring-").is_some_and(|name| {
                    if preparations {
                        build_name(name)
                    } else {
                        uuid_name(name)
                    }
                }) {
                    names.push(name);
                }
            }
        }
        names.sort();
        Ok(names)
    })
    .await?;
    let mut removed = 0;
    for name in names {
        removed += remove_retired(
            joined(&parent, &name),
            if preparations {
                "preparations"
            } else {
                "snapshots"
            },
            progress,
        )
        .await;
    }
    Ok(removed)
}
async fn retire_directory(
    path: PathBuf,
    area: &'static str,
    progress: &Option<Arc<dyn RetentionProgress>>,
) -> IvyResult<usize> {
    let name = path
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or_else(IvyError::internal)?;
    let retired = joined(
        path.parent().ok_or_else(IvyError::internal)?,
        &format!(".retiring-{name}"),
    );
    require(
        !exists(retired.clone()).await?,
        "mutation_conflict",
        "An earlier staging deletion is still pending.",
    )?;
    let destination = retired.clone();
    blocking(move || internal(fs::rename(fs_path(&path), fs_path(&destination)))).await?;
    Ok(remove_retired(retired, area, progress).await)
}
async fn protect_unknown(
    journal: &HostJournal,
    root: &Path,
    ids: &mut BTreeSet<String>,
    progress: &Option<Arc<dyn RetentionProgress>>,
) -> IvyResult<()> {
    skipped(progress, "preparations", "unrecognized_preparation", true);
    let name = root
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or_else(IvyError::internal)?;
    for (candidate, build) in journal.preparation_candidate_references().await? {
        if preparation_id_for_build(&build)? == &name[6..] {
            ids.insert(candidate);
        }
    }
    Ok(())
}
fn at_or_before(record: &IvyJson, cutoff: f64) -> bool {
    record
        .get("updatedAt")
        .and_then(IvyJson::as_js_string)
        .is_some_and(|at| ivy_sdk::clock::parse_date(at) <= cutoff)
}
fn milliseconds(at: std::time::SystemTime) -> f64 {
    let (positive, d) = match at.duration_since(UNIX_EPOCH) {
        Ok(d) => (true, d),
        Err(e) => (false, e.duration()),
    };
    let ms = d.as_secs() as f64 * 1000.0 + d.subsec_nanos() as f64 / 1_000_000.0;
    if positive { ms } else { -ms }
}
/// The admitted task retains its journal handle and any acquired build lock
/// through retirement, including when its observer drops.
pub async fn collect_preparation_storage(
    host: IvyJson,
    journal: HostJournal,
    cutoff: f64,
    progress: Option<Arc<dyn RetentionProgress>>,
) -> IvyResult<PreparationStorageCollection> {
    internal(tokio::spawn(collect(host, journal, cutoff, progress)).await)?
}
async fn collect(
    host: IvyJson,
    journal: HostJournal,
    cutoff: f64,
    progress: Option<Arc<dyn RetentionProgress>>,
) -> IvyResult<PreparationStorageCollection> {
    let staging = path_field(&host, "stagingRoot")?;
    let source_root = joined(&staging, "sources");
    let mut result = PreparationStorageCollection {
        preparations: retired_directories(staging.clone(), true, &progress).await?,
        snapshots: retired_directories(source_root.clone(), false, &progress).await?,
        protected_candidates: BTreeSet::new(),
    };
    let mut protected_snapshots = BTreeSet::new();
    let mut unknown = false;
    for entry in journal.unfinished_preparation_operations().await? {
        if let Some(snapshot) = entry.get("sourceSnapshot").filter(|v| truthy(v)) {
            validate_host("SourceSnapshot", snapshot)?;
            protected_snapshots.insert(snapshot.required("snapshotId")?.as_str()?);
        }
    }
    for root in roots(staging.clone()).await? {
        let mut record = match record_at(staging.clone(), root.clone()).await {
            Ok(Some(record)) => record,
            _ => {
                unknown = true;
                protect_unknown(&journal, &root, &mut result.protected_candidates, &progress)
                    .await?;
                continue;
            }
        };
        let original = record.clone();
        let mut current_missing = false;
        if record.get("phase") == Some(&"compacted".into()) && at_or_before(&record, cutoff) {
            let mut lock = None;
            let removal: IvyResult<bool>=async{
                let build=record.required("buildId")?.as_str()?;
                lock=Some(ExecutorLock::acquire(joined(&joined(&staging,"build-locks"),&build[7..]),false).await?);
                let current=record_at(staging.clone(),root.clone()).await?;
                current_missing=current.is_none();
                record=current.ok_or_else(||IvyError::new("preparation_unrecognized","Preparation changed while collecting its history."))?;
                if record.get("phase")==Some(&"compacted".into())&&at_or_before(&record,cutoff){
                    if let Some(candidate)=record.get("candidate").filter(|v|!v.is_null()){
                        if hash_json(&journal.candidate(&candidate.required("candidateId")?.as_str()?).await?)?==hash_json(candidate)?{
                            let check=root.clone();let parent=staging.clone();
                            blocking(move||{
                                for entry in internal(fs::read_dir(fs_path(&check)))?{
                                    let entry=internal(entry)?;
                                    require(["preparation.json","preparation-before-recovery.json"].iter().any(|n|entry.file_name()==*n),"target_conflict","Compacted preparation contains unrecognized retained evidence.")?;
                                }
                                owned_root(&parent,&check)?;Ok(())
                            }).await?;
                            result.preparations+=retire_directory(root.clone(),"preparations",&progress).await?;
                            return Ok(true);
                        }
                    }
                }
                Ok(false)
            }.await;
            if let Some(lock) = lock {
                lock.close().await?;
            }
            match removal {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => {
                    skipped(&progress, "preparations", &error.code, true);
                    if !["executor_already_running", "not_found"].contains(&error.code.as_str()) {
                        unknown = true;
                        protect_unknown(
                            &journal,
                            &root,
                            &mut result.protected_candidates,
                            &progress,
                        )
                        .await?;
                    }
                }
            }
        }
        let compacted = !current_missing && record.get("phase") == Some(&"compacted".into());
        skipped(
            &progress,
            "preparations",
            if compacted {
                "grace_period"
            } else {
                "unfinished_preparation"
            },
            !compacted,
        );
        for value in std::iter::once(&original).chain((!current_missing).then_some(&record)) {
            protected_snapshots.insert(
                value
                    .required("snapshot")?
                    .required("snapshotId")?
                    .as_str()?,
            );
            result
                .protected_candidates
                .insert(value.required("buildId")?.as_str()?);
            if let Some(candidate) = value.get("candidate").filter(|v| !v.is_null()) {
                result
                    .protected_candidates
                    .insert(candidate.required("candidateId")?.as_str()?);
            }
        }
    }
    if unknown {
        skipped(
            &progress,
            "snapshots",
            "unknown_preparation_ownership",
            true,
        );
        return Ok(result);
    }
    let scan = source_root.clone();
    let entries = blocking(move || {
        let entries = match fs::read_dir(fs_path(&scan)) {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(IvyError::internal()),
        };
        let mut values = internal(entries.collect::<Result<Vec<_>, _>>())?;
        values.sort_by_key(|v| v.file_name());
        Ok(values)
    })
    .await?;
    let boundary = if entries.is_empty() {
        None
    } else {
        let root = source_root.clone();
        Some(blocking(move || internal(realpath(&root))).await?)
    };
    for entry in entries {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let kind = internal(entry.file_type())?;
        if !uuid_name(&name) || !kind.is_dir() || kind.is_symlink() {
            continue;
        }
        if protected_snapshots.contains(&name) {
            skipped(&progress, "snapshots", "referenced", false);
            continue;
        }
        let path = joined(&source_root, &name);
        let checked = path.clone();
        let (metadata, actual) = blocking(move || {
            Ok((
                internal(fs::symlink_metadata(fs_path(&checked)))?,
                internal(realpath(&checked))?,
            ))
        })
        .await?;
        if milliseconds(internal(metadata.modified())?) > cutoff {
            skipped(&progress, "snapshots", "grace_period", false);
            continue;
        }
        if actual.parent() != boundary.as_deref()
            || actual.file_name() != Some(std::ffi::OsStr::new(&name))
        {
            skipped(&progress, "snapshots", "unrecognized_path", true);
            continue;
        }
        let manifest =
            match crate::config::json_file(joined(&path, "snapshot.json"), 8 * 1024 * 1024).await {
                Ok(v) => v,
                Err(_) => {
                    skipped(&progress, "snapshots", "unrecognized_manifest", true);
                    continue;
                }
            };
        if validate_host("SnapshotManifest", &manifest).is_err() {
            skipped(&progress, "snapshots", "unrecognized_manifest", true);
            continue;
        }
        let snapshot = manifest.required("snapshot")?;
        if snapshot.get("snapshotId") != Some(&name.clone().into())
            || snapshot.get("manifestPath")
                != Some(&crate::layout::path_json(&joined(&path, "snapshot.json")))
            || snapshot.get("sourceRoot")
                != Some(&crate::layout::path_json(&joined(&path, "source")))
        {
            skipped(&progress, "snapshots", "unrecognized_path", true);
            continue;
        }
        result.snapshots += retire_directory(path, "snapshots", &progress).await?;
    }
    Ok(result)
}
