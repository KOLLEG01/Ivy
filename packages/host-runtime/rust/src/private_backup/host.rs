use super::{offline_owners, snapshot_journal};
use crate::{
    IvyError, IvyJson as J, IvyResult,
    artifact::FileCompare,
    backup_files::{self, BackupFile},
    config,
    host_config::HostContext,
    journal::HostJournal,
    layout::{self, StorageArea, StorageContext},
    native_backup_policy::NativeBackupPaths,
    native_path::{self, Style},
    node_runtime::NodeRuntime,
    paths, project_storage, storage_space, streaming_hash,
};
use chrono::{DateTime, SecondsFormat, Utc};
use ivy_contracts::{JsString, core_schema::validate_host, hash::hash_json};
use ivy_sdk::clock::Clock;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
};

pub struct BackupBindings {
    pub compare: FileCompare,
    pub native_paths: Arc<dyn NativeBackupPaths>,
    pub node: Option<NodeRuntime>,
    /// Supplied by the service which owns Hive; run only under offline locks.
    pub hive_snapshot: fn(&Path, &Path) -> IvyResult<J>,
    pub distribution_root: PathBuf,
    pub clock: Arc<dyn Clock>,
}
async fn node_platform(config: &J, journal: &HostJournal, node: Option<&NodeRuntime>) -> IvyResult<String> {
    if let Some(node) = node { return Ok(format!("v{}", node.version)); }
    let mut selected = BTreeSet::new();
    for instance in array(config.required("instances")?)?.iter().filter(|value| value.get("engine") == Some(&"process".into())) {
        let id = instance.required("instanceId")?.as_str()?;
        for selection in [journal.installed(&id).await?, journal.target(&id).await?].into_iter().flatten() {
            selected.insert(selection.required("candidateId")?.as_str()?);
        }
    }
    for id in selected {
        let manifest = journal.manifest(&id).await?;
        require(crate::native_plan::is_native_plan(&manifest) || manifest.get("kind") == Some(&"app".into()),
            "unsupported_runtime", "A retained JavaScript workload requires its Node toolchain for backup.")?;
    }
    Ok("unavailable".into())
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
fn text(v: &J) -> IvyResult<JsString> {
    v.as_js_string().cloned().ok_or_else(IvyError::internal)
}
fn field(v: &J, k: &str) -> IvyResult<JsString> {
    text(v.required(k)?)
}
fn array(v: &J) -> IvyResult<&[J]> {
    v.as_array().ok_or_else(IvyError::internal)
}
fn concat(parts: &[JsString]) -> JsString {
    JsString(parts.iter().flat_map(|s| s.0.iter().copied()).collect())
}
fn join(parts: &[JsString]) -> JsString {
    native_path::join(Style::native(), parts)
}
async fn metadata(p: &Path) -> IvyResult<std::fs::Metadata> {
    internal(tokio::fs::symlink_metadata(p).await)
}
async fn realpath(p: &Path) -> IvyResult<PathBuf> {
    let p = p.to_owned();
    internal(tokio::task::spawn_blocking(move || paths::realpath(&p)).await)
        .and_then(internal)
}
fn member_file(root: &Path, member: &JsString) -> IvyResult<PathBuf> {
    // Validate at the public member boundary, then retain the native I/O root.
    backup_files::relative_file(&native_path::from_native(root), member)?;
    Ok(root.join(native_path::to_native(member)))
}
fn now(b: &BackupBindings) -> IvyResult<J> {
    Ok(
        internal(DateTime::<Utc>::from_timestamp_millis(b.clock.utc_millis()).ok_or(()))?
            .to_rfc3339_opts(SecondsFormat::Millis, true)
            .into(),
    )
}
fn mode(m: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        m.mode() & 0o777
    }
    #[cfg(windows)]
    {
        if m.permissions().readonly() {
            0o444
        } else {
            0o666
        }
    }
}
async fn describe(path: &Path, local: JsString, snapshot: bool) -> IvyResult<BackupFile> {
    let m = metadata(path).await?;
    Ok(BackupFile {
        path: local,
        hash: streaming_hash::file_hash(path.to_owned()).await?,
        bytes: m.len(),
        mode: if snapshot { 0o600 } else { mode(&m) },
    })
}
async fn mkdir(path: &Path, recursive: bool) -> IvyResult<()> {
    let mut b = tokio::fs::DirBuilder::new();
    b.recursive(recursive);
    #[cfg(unix)]
    b.mode(0o700);
    internal(b.create(path).await)
}
async fn current_configuration(path: &Path, context: &StorageContext) -> IvyResult<J> {
    let input = config::json_file(path.to_owned(), config::CONFIGURATION_BYTES).await?;
    layout::resolve_host_configuration(
        &input,
        &native_path::from_native(&internal(paths::absolute(path))?),
        &crate::native_account::environment(),
        context.paths.style,
        &context.account_home,
    )
}
#[derive(Clone)]
struct RuntimeFile {
    record: BackupFile,
    source: PathBuf,
}
impl RuntimeFile {
    fn json(&self) -> J {
        let mut v = self.record.json();
        v.insert("source", J::String(native_path::from_native(&self.source)));
        v
    }
}
struct Inventory {
    config: J,
    areas: Vec<StorageArea>,
    context: Arc<StorageContext>,
    hive: Vec<J>,
    ephemeral: Vec<J>,
    owner_locks: Vec<JsString>,
}
impl Inventory {
    fn excluded(&self, key: &JsString) -> IvyResult<bool> {
        if self.owner_locks.iter().any(|p| {
            ["", "-journal", "-wal", "-shm"]
                .iter()
                .any(|s| *key == concat(&["state/".into(), p.clone(), (*s).into()]))
        }) {
            return Ok(true);
        }
        if ["", "-wal", "-shm"]
            .iter()
            .any(|s| *key == concat(&["state/deployments.sqlite".into(), (*s).into()]))
        {
            return Ok(true);
        }
        for e in &self.ephemeral {
            let p = field(e, "path")?;
            let p = if p.0.starts_with(&JsString::from("services/").0) {
                p
            } else {
                concat(&["state/".into(), p])
            };
            if *key == p {
                return Ok(true);
            }
        }
        for instance in &self.hive {
            let data = field(&layout::service_paths(&self.config, instance)?, "data")?;
            for suffix in ["", "-wal", "-shm"] {
                let path = join(&[data.clone(), format!("hive.sqlite{suffix}").into()]);
                if *key == layout::storage_path(&self.config, &path, &self.context)? {
                    return Ok(true);
                }
            }
        }
        for instance in &self.hive {
            let directory = field(
                instance.required("settings")?.required("backup")?,
                "directory",
            )?;
            let file = layout::storage_file(&self.config, key, Some(&self.areas), &self.context)?;
            if self.context.paths.inside(&directory, &file) {
                return Ok(true);
            }
        }
        Ok(false)
    }
    async fn files(self: &Arc<Self>) -> IvyResult<Vec<RuntimeFile>> {
        let mut result = Vec::new();
        for area in &self.areas {
            let root = native_path::to_native(&area.path);
            if !backup_files::exists(&root).await? {
                continue;
            }
            let inventory = self.clone();
            let key = area.key.clone();
            let excluded = Arc::new(move |local: &JsString| {
                inventory.excluded(&concat(&[key.clone(), "/".into(), local.clone()]))
            });
            for file in backup_files::inventory_files(root.clone(), Some(excluded)).await? {
                let source = member_file(&root, &file.path)?;
                let mut record = file;
                record.path = concat(&[area.key.clone(), "/".into(), record.path]);
                result.push(RuntimeFile { record, source });
            }
        }
        Ok(result)
    }
}
/// Admission owns all work and locks through publication even if its observer
/// drops. Hive storage stays bound to its native owner at the service layer.
pub async fn backup_host(
    config_path: PathBuf,
    destination: PathBuf,
    bindings: Arc<BackupBindings>,
) -> IvyResult<J> {
    internal(tokio::spawn(async move { backup(config_path, destination, bindings).await }).await)?
}
async fn backup(
    config_path: PathBuf,
    destination: PathBuf,
    b: Arc<BackupBindings>,
) -> IvyResult<J> {
    let started = b.clock.monotonic();
    let config = crate::host_config::host_config(config_path.clone()).await?;
    require(
        destination.is_absolute(),
        "invalid_arguments",
        "Backup destination must be absolute.",
    )?;
    let account = HostContext::current()?;
    let context = Arc::new(StorageContext {
        paths: account.paths,
        account_home: account.account_home,
        compare: b.compare.clone(),
    });
    let mut roots = layout::host_storage_areas(&config, &context)?
        .into_iter()
        .map(|a| native_path::to_native(&a.path))
        .collect::<Vec<_>>();
    roots.push(native_path::to_native(&field(&config, "artifactRoot")?));
    roots.push(native_path::to_native(&field(&config, "stagingRoot")?));
    let mut source_roots = Vec::new();
    for root in roots {
        source_roots.push(if backup_files::exists(&root).await? {
            realpath(&root).await?
        } else {
            internal(paths::absolute(&root))?
        });
    }
    let parent = realpath(destination.parent().unwrap_or(&destination)).await?;
    let actual_destination = parent.join(destination.file_name().unwrap_or_default());
    for root in &source_roots {
        require(
            !internal(paths::inside(root, &actual_destination))?
                && !internal(paths::inside(&destination, root))?,
            "target_conflict",
            "Private backup storage must be outside live runtime, artifact and staging roots.",
        )?;
    }
    let docker = crate::docker::DockerAdapter::new(b.distribution_root.clone())?;
    let mut owners = offline_owners(&config, &b.compare, &docker).await?;
    let mut journal = None;
    let mut created = false;
    let backup_id = uuid::Uuid::new_v4().to_string();
    let created_at = now(&b)?;
    let result: IvyResult<J>=async{
        let areas=project_storage::project_storage_areas(&config,&context).await?;
        for area in &areas{let path=native_path::to_native(&area.path);if backup_files::exists(&path).await?{let actual=realpath(&path).await?;
            require(!internal(paths::inside(&actual,&actual_destination))?&&!internal(paths::inside(&actual_destination,&actual))?,"target_conflict","Backup destination overlaps retained internal work.")?;
        }}
        journal=Some(HostJournal::open(config.clone()).await?);journal.as_ref().unwrap().begin_maintenance().await?;
        let node_platform=node_platform(&config,journal.as_ref().unwrap(),b.node.as_ref()).await?;
        let configuration_hash=hash_json(&config)?;
        require(hash_json(&current_configuration(&config_path,&context).await?)?==configuration_hash,"configuration_changed","Backup master configuration changed.")?;
        let hive=array(config.required("instances")?)?.iter().filter(|i|i.get("componentId")==Some(&"hive".into())).cloned().collect::<Vec<_>>();
        for instance in &hive{
            let settings=instance.required("settings")?;validate_host("HiveSettings",settings)?;
            let directory=field(settings.required("backup")?,"directory")?;
            require(areas.iter().all(|a|!context.paths.inside(&directory,&a.path)),"target_conflict","Rotating Hive backup storage cannot cover authoritative runtime data.")?;
        }
        let mut owner_locks=["executor","restore-decision-lock","bootstrap-maintenance-lock"].map(JsString::from).to_vec();
        for instance in array(config.required("instances")?)?{owner_locks.push(concat(&["owners/".into(),field(instance,"instanceId")?]));}
        for path in &mut owner_locks{*path=concat(&[path.clone(),"/executor-lock.sqlite".into()]);}
        let ephemeral=b.native_paths.exclusions(&config)?;
        let shared_homes=b.native_paths.shared_homes(&config)?;
        for entry in &ephemeral{
            let p=field(entry,"path")?;let key=if p.0.starts_with(&JsString::from("services/").0){p}else{concat(&["state/".into(),p])};
            let p=native_path::to_native(&layout::storage_file(&config,&key,None,&context)?);
            if backup_files::exists(&p).await?{require(metadata(&p).await?.is_dir()&&!metadata(&p).await?.file_type().is_symlink(),"target_conflict","Native helper cache root must be an owned directory.")?;}
        }
        let inventory=Arc::new(Inventory{config:config.clone(),areas:areas.clone(),context:context.clone(),hive:hive.clone(),ephemeral:ephemeral.clone(),owner_locks});
        let runtime_files=inventory.files().await?;
        let mut external_paths=config.required("executables")?.as_object().ok_or_else(IvyError::internal)?.iter().map(|(_,v)|text(v)).collect::<IvyResult<BTreeSet<_>>>()?;
        for instance in array(config.required("instances")?)?.iter().filter(|i|i.get("componentId")==Some(&"agent-manager".into())){
            let settings=instance.required("settings")?;
            if let Some(p)=settings.get("nativeExecutable").and_then(J::as_js_string){external_paths.insert(p.clone());}
            if let Some(p)=settings.get("windowsShell").and_then(|v|v.get("executable")).and_then(J::as_js_string).filter(|s|!s.0.is_empty()){external_paths.insert(p.clone());}
        }
        let mut external_files=Vec::new();
        for p in external_paths{
            let path=native_path::to_native(&p);
            require(path.is_absolute(),"invalid_arguments","Backup executable prerequisites must be absolute.")?;
            let info=metadata(&realpath(&path).await?).await?;
            require(info.is_file(),"backup_invalid","Runtime prerequisite is not a file.")?;
            external_files.push(BackupFile{path:p.clone(),hash:streaming_hash::file_hash(path).await?,bytes:info.len(),mode:mode(&info)}.json());
        }
        let journal_path=native_path::to_native(&field(&config,"runtimeRoot")?).join("deployments.sqlite");
        let mut sqlite_bytes=metadata(&journal_path).await?.len() as f64;
        for instance in &hive{let data=native_path::to_native(&field(&layout::service_paths(&config,instance)?,"data")?);
            for suffix in ["","-wal"]{let p=data.join(format!("hive.sqlite{suffix}"));if backup_files::exists(&p).await?{sqlite_bytes+=metadata(&p).await?.len() as f64;}}
        }
        let reserve=runtime_files.iter().map(|f|f.record.bytes as f64).sum::<f64>()+sqlite_bytes*2.0+64.0*1024.0*1024.0;
        require(reserve<=64.0*1024_f64.powi(3),"limit_exceeded","Private backup exceeds its bounded allocation workload.")?;
        storage_space::require_storage_space(parent,reserve).await?;
        let job=crate::native_plan::native_binary_path(&b.distribution_root,"ivy-job");
        backup_files::private_directory(destination.clone(),job.clone()).await?;created=true;
        let configuration=destination.join("configuration.json");
        config::atomic_json(configuration.clone(),config.clone()).await?;
        let mut files=vec![describe(&configuration,"configuration.json".into(),false).await?];
        mkdir(&destination.join("state"),false).await?;
        let snapshot=destination.join("state/deployments.sqlite");
        snapshot_journal(journal_path,snapshot.clone()).await?;
        files.push(describe(&snapshot,"state/deployments.sqlite".into(),true).await?);
        for file in &runtime_files{
            let target=member_file(&destination,&file.record.path)?;
            let mut copied=backup_files::copy_verified(file.source.clone(),target,Some(file.record.clone()),false).await?;
            copied.path=file.record.path.clone();files.push(copied);
        }
        let mut hive_snapshots=Vec::new();
        for instance in &hive{
            let source=native_path::to_native(&field(&layout::service_paths(&config,instance)?,"data")?).join("hive.sqlite");
            if !backup_files::exists(&source).await?{continue}
            let local=layout::storage_path(&config,&native_path::from_native(&source),&context)?;let target=member_file(&destination,&local)?;
            mkdir(target.parent().ok_or_else(IvyError::internal)?,true).await?;
            let snapshot=b.hive_snapshot;
            let (source_path,target_path)=(source,target.clone());
            let snapshot=internal(tokio::task::spawn_blocking(move || snapshot(&source_path,&target_path)).await)??;
            let value=J::object([("instanceId",instance.required("instanceId")?.clone()),("path",J::String(local.clone())),
                ("format",snapshot.required("format")?.clone()),("pages",snapshot.required("pages")?.clone())]);
            hive_snapshots.push(value);files.push(describe(&target,local,true).await?);
        }
        require(hash_json(&J::Array(inventory.files().await?.iter().map(RuntimeFile::json).collect()))?==hash_json(&J::Array(runtime_files.iter().map(RuntimeFile::json).collect()))?&&hash_json(&current_configuration(&config_path,&context).await?)?==configuration_hash,"backup_changed","Runtime state or configuration changed during offline snapshot.")?;
        files.sort_by(|a,b|a.path.cmp(&b.path));
        let completed_at=now(&b)?;
        let manifest=J::object([
            ("schemaVersion",J::Number(1.0)),("backupId",backup_id.clone().into()),("hostId",config.required("hostId")?.clone()),("createdAt",created_at.clone()),("completedAt",completed_at.clone()),
            ("consistency","offline-owned-state".into()),("platform",J::object([("os",if cfg!(windows){"win32".into()}else{"linux".into()}),("arch",match std::env::consts::ARCH{"x86_64"=>"x64","aarch64"=>"arm64",v=>v}.into()),("node",node_platform.into())])),
            ("configurationHash",configuration_hash.into()),("files",J::Array(files.iter().map(BackupFile::json).collect())),("externalFiles",J::Array(external_files)),("hiveSnapshots",J::Array(hive_snapshots)),("ephemeralNativePaths",J::Array(ephemeral)),("sharedNativeHomes",J::Array(shared_homes)),("storageAreas",J::Array(areas.iter().map(StorageArea::json).collect())),
            ("excluded",J::Array(["OS owner lock files","Source checkouts, staging payloads and local releases","OS bootstrap definitions","Runtime executables/native bundles outside runtime data (hashed prerequisites)","Docker daemon images (retained immutable candidate prerequisites)","Rotating Hive snapshot directories","Shared Codex user homes and externally managed App Servers (references only; separate user-state backup required)"].into_iter().map(J::from).collect())),
        ]);
        validate_host("BackupManifest",&manifest)?;
        backup_files::sync_directories(destination.clone()).await?;
        let manifest_path=destination.join("backup.json");
        config::atomic_json(manifest_path.clone(),manifest).await?;
        let elapsed=b.clock.monotonic().checked_duration_since(started).unwrap_or_default().as_secs_f64()*1000.0;
        let result=J::object([("schemaVersion",J::Number(1.0)),("backupId",backup_id.clone().into()),("hostId",config.required("hostId")?.clone()),("directory",J::String(native_path::from_native(&destination))),("manifestHash",streaming_hash::file_hash(manifest_path).await?.into()),("completedAt",completed_at),("files",J::Number(files.len() as f64)),("bytes",J::Number(files.iter().map(|f|f.bytes as f64).sum())),("elapsedMs",J::Number(elapsed.round()))]);
        validate_host("BackupResult",&result)?;Ok(result)
    }.await;
    if let Err(error) = &result {
        if created {
            if let Ok(at) = now(&b) {
                let _ = config::atomic_json(
                    destination.join("backup-failure.json"),
                    J::object([
                        ("schemaVersion", J::Number(1.0)),
                        ("backupId", backup_id.into()),
                        ("at", at),
                        ("code", error.code.clone().into()),
                    ]),
                )
                .await;
            }
        }
    }
    if let Some(journal) = journal {
        journal.rollback_maintenance().await?;
        journal.close().await?;
    }
    owners.close().await?;
    result
}
