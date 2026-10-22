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
    node_path::{self, Style},
    node_runtime::NodeRuntime,
    process, project_storage, storage_space, streaming_hash,
};
use chrono::{DateTime, SecondsFormat, Utc};
use ivy_contracts::{JsString, core_schema::validate_host, hash::hash_json, json};
use ivy_sdk::clock::Clock;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};

pub struct BackupBindings {
    pub compare: FileCompare,
    pub native_paths: Arc<dyn NativeBackupPaths>,
    pub node: NodeRuntime,
    pub distribution_root: PathBuf,
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
    node_path::join(Style::native(), parts)
}
fn native(p: &JsString) -> IvyResult<PathBuf> {
    internal(node_path::node_filesystem_path(p))
}
async fn exists(p: &JsString) -> IvyResult<bool> {
    backup_files::exists(&native(p)?).await
}
async fn metadata(p: &JsString) -> IvyResult<std::fs::Metadata> {
    internal(tokio::fs::symlink_metadata(native(p)?).await)
}
async fn realpath(p: &JsString) -> IvyResult<JsString> {
    let p = native(p)?;
    internal(tokio::task::spawn_blocking(move || crate::paths::realpath(&p)).await)
        .and_then(internal)
        .map(|p| node_path::from_native(&p))
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
async fn describe(path: &JsString, local: JsString, snapshot: bool) -> IvyResult<BackupFile> {
    let m = metadata(path).await?;
    Ok(BackupFile {
        path: local,
        hash: streaming_hash::file_hash(native(path)?).await?,
        bytes: m.len(),
        mode: if snapshot { 0o600 } else { mode(&m) },
    })
}
async fn mkdir(path: &JsString, recursive: bool) -> IvyResult<()> {
    let mut b = tokio::fs::DirBuilder::new();
    b.recursive(recursive);
    #[cfg(unix)]
    b.mode(0o700);
    internal(b.create(native(path)?).await)
}
async fn current_configuration(path: &JsString, context: &StorageContext) -> IvyResult<J> {
    let input = config::json_file(native(path)?, config::CONFIGURATION_BYTES).await?;
    layout::resolve_host_configuration(
        &input,
        &context.paths.resolve(&[path.clone()]),
        &crate::native_account::environment(),
        context.paths.style,
        &context.account_home,
    )
}
#[derive(Clone)]
struct RuntimeFile {
    record: BackupFile,
    source: JsString,
}
impl RuntimeFile {
    fn json(&self) -> J {
        let mut v = self.record.json();
        v.insert("source", J::String(self.source.clone()));
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
            if !exists(&area.path).await? {
                continue;
            }
            let inventory = self.clone();
            let key = area.key.clone();
            let excluded = Arc::new(move |local: &JsString| {
                inventory.excluded(&concat(&[key.clone(), "/".into(), local.clone()]))
            });
            for file in backup_files::inventory_files(native(&area.path)?, Some(excluded)).await? {
                let source = backup_files::relative_file(&area.path, &file.path)?;
                let mut record = file;
                record.path = concat(&[area.key.clone(), "/".into(), record.path]);
                result.push(RuntimeFile { record, source });
            }
        }
        Ok(result)
    }
}
/// Admission owns all work and locks through publication even if its observer
/// drops. Distribution helpers use the existing v1 command/Node envelope.
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
    let config_path = node_path::from_native(&config_path);
    let destination = node_path::from_native(&destination);
    require(
        node_path::is_absolute(Style::native(), &destination),
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
        .map(|a| a.path)
        .collect::<Vec<_>>();
    roots.push(field(&config, "artifactRoot")?);
    roots.push(field(&config, "stagingRoot")?);
    let mut source_roots = Vec::new();
    for root in roots {
        source_roots.push(if exists(&root).await? {
            realpath(&root).await?
        } else {
            context.paths.resolve(&[root])
        });
    }
    let parent = realpath(&node_path::dirname(context.paths.style, &destination)).await?;
    let actual_destination = join(&[
        parent.clone(),
        node_path::basename(context.paths.style, &destination),
    ]);
    require(
        source_roots.iter().all(|root| {
            !context.paths.inside(root, &actual_destination)
                && !context.paths.inside(&destination, root)
        }),
        "target_conflict",
        "Private backup storage must be outside live runtime, artifact and staging roots.",
    )?;
    let docker = crate::docker::DockerAdapter::new(b.distribution_root.clone())?;
    let mut owners = offline_owners(&config, &b.compare, &docker).await?;
    let mut journal = None;
    let mut created = false;
    let backup_id = uuid::Uuid::new_v4().to_string();
    let created_at = now(&b)?;
    let result: IvyResult<J>=async{
        let areas=project_storage::project_storage_areas(&config,&context).await?;
        for area in &areas{if exists(&area.path).await?{let actual=realpath(&area.path).await?;
            require(!context.paths.inside(&actual,&actual_destination)&&!context.paths.inside(&actual_destination,&actual),"target_conflict","Backup destination overlaps retained internal work.")?;
        }}
        journal=Some(HostJournal::open(config.clone()).await?);journal.as_ref().unwrap().begin_maintenance().await?;
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
            let p=layout::storage_file(&config,&key,None,&context)?;
            if exists(&p).await?{require(metadata(&p).await?.is_dir()&&!metadata(&p).await?.file_type().is_symlink(),"target_conflict","Native helper cache root must be an owned directory.")?;}
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
            require(node_path::is_absolute(context.paths.style,&p),"invalid_arguments","Backup executable prerequisites must be absolute.")?;
            let info=metadata(&realpath(&p).await?).await?;
            require(info.is_file(),"backup_invalid","Runtime prerequisite is not a file.")?;
            external_files.push(BackupFile{path:p.clone(),hash:streaming_hash::file_hash(native(&p)?).await?,bytes:info.len(),mode:mode(&info)}.json());
        }
        let journal_path=join(&[field(&config,"runtimeRoot")?,"deployments.sqlite".into()]);
        let mut sqlite_bytes=metadata(&journal_path).await?.len() as f64;
        for instance in &hive{let data=field(&layout::service_paths(&config,instance)?,"data")?;
            for suffix in ["","-wal"]{let p=join(&[data.clone(),format!("hive.sqlite{suffix}").into()]);if exists(&p).await?{sqlite_bytes+=metadata(&p).await?.len() as f64;}}
        }
        let reserve=runtime_files.iter().map(|f|f.record.bytes as f64).sum::<f64>()+sqlite_bytes*2.0+64.0*1024.0*1024.0;
        require(reserve<=64.0*1024_f64.powi(3),"limit_exceeded","Private backup exceeds its bounded allocation workload.")?;
        storage_space::require_storage_space(native(&parent)?,reserve).await?;
        let job=crate::native_plan::native_binary_path(&b.distribution_root,"ivy-job");
        backup_files::private_directory(native(&destination)?,job.clone()).await?;created=true;
        let configuration=join(&[destination.clone(),"configuration.json".into()]);
        config::atomic_json(native(&configuration)?,config.clone()).await?;
        let mut files=vec![describe(&configuration,"configuration.json".into(),false).await?];
        mkdir(&join(&[destination.clone(),"state".into()]),false).await?;
        let snapshot=join(&[destination.clone(),"state/deployments.sqlite".into()]);
        snapshot_journal(native(&journal_path)?,native(&snapshot)?).await?;
        files.push(describe(&snapshot,"state/deployments.sqlite".into(),true).await?);
        for file in &runtime_files{
            let target=backup_files::relative_file(&destination,&file.record.path)?;
            let mut copied=backup_files::copy_verified(native(&file.source)?,native(&target)?,Some(file.record.clone()),false).await?;
            copied.path=file.record.path.clone();files.push(copied);
        }
        let mut hive_snapshots=Vec::new();
        for instance in &hive{
            let source=join(&[field(&layout::service_paths(&config,instance)?,"data")?,"hive.sqlite".into()]);
            if !exists(&source).await?{continue}
            let local=layout::storage_path(&config,&source,&context)?;let target=backup_files::relative_file(&destination,&local)?;
            mkdir(&node_path::dirname(context.paths.style,&target),true).await?;
            let commands=config.required("executables")?.as_object().ok_or_else(IvyError::internal)?.iter().map(|(k,v)|Ok((k.to_utf8()?,native(&text(v)?)?))).collect::<IvyResult<BTreeMap<_,_>>>()?;
            let result=process::run_command(&process::Command{executable:"node".into(),args:vec!["dist/services/hive/src/offline-backup.js".into(),node_path::to_native(&source).into_os_string(),node_path::to_native(&target).into_os_string()],cwd:None,timeout_ms:600_000},&b.distribution_root,&commands,process::LaunchContext{job_launcher:Some(job.clone()),..Default::default()},None).await?;
            let snapshot=json::parse(&result.stdout,usize::MAX)?;
            let mut value=J::object([("instanceId",instance.required("instanceId")?.clone()),("path",J::String(local.clone()))]);
            for (key,value2) in snapshot.as_object().ok_or_else(IvyError::internal)?{value.insert(key.to_utf8()?,value2.clone());}
            hive_snapshots.push(value);files.push(describe(&target,local,true).await?);
        }
        require(hash_json(&J::Array(inventory.files().await?.iter().map(RuntimeFile::json).collect()))?==hash_json(&J::Array(runtime_files.iter().map(RuntimeFile::json).collect()))?&&hash_json(&current_configuration(&config_path,&context).await?)?==configuration_hash,"backup_changed","Runtime state or configuration changed during offline snapshot.")?;
        files.sort_by(|a,b|a.path.cmp(&b.path));
        let completed_at=now(&b)?;
        let manifest=J::object([
            ("schemaVersion",J::Number(1.0)),("backupId",backup_id.clone().into()),("hostId",config.required("hostId")?.clone()),("createdAt",created_at.clone()),("completedAt",completed_at.clone()),
            ("consistency","offline-owned-state".into()),("platform",J::object([("os",if cfg!(windows){"win32".into()}else{"linux".into()}),("arch",match std::env::consts::ARCH{"x86_64"=>"x64","aarch64"=>"arm64",v=>v}.into()),("node",format!("v{}",b.node.version).into())])),
            ("configurationHash",configuration_hash.into()),("files",J::Array(files.iter().map(BackupFile::json).collect())),("externalFiles",J::Array(external_files)),("hiveSnapshots",J::Array(hive_snapshots)),("ephemeralNativePaths",J::Array(ephemeral)),("sharedNativeHomes",J::Array(shared_homes)),("storageAreas",J::Array(areas.iter().map(StorageArea::json).collect())),
            ("excluded",J::Array(["OS owner lock files","Source checkouts, staging payloads and local releases","OS bootstrap definitions","Runtime executables/native bundles outside runtime data (hashed prerequisites)","Docker daemon images (retained immutable candidate prerequisites)","Rotating Hive snapshot directories","Shared Codex user homes and externally managed App Servers (references only; separate user-state backup required)"].into_iter().map(J::from).collect())),
        ]);
        validate_host("BackupManifest",&manifest)?;
        backup_files::sync_directories(native(&destination)?).await?;
        let manifest_path=join(&[destination.clone(),"backup.json".into()]);
        config::atomic_json(native(&manifest_path)?,manifest).await?;
        let elapsed=b.clock.monotonic().checked_duration_since(started).unwrap_or_default().as_secs_f64()*1000.0;
        let result=J::object([("schemaVersion",J::Number(1.0)),("backupId",backup_id.clone().into()),("hostId",config.required("hostId")?.clone()),("directory",J::String(destination.clone())),("manifestHash",streaming_hash::file_hash(native(&manifest_path)?).await?.into()),("completedAt",completed_at),("files",J::Number(files.len() as f64)),("bytes",J::Number(files.iter().map(|f|f.bytes as f64).sum())),("elapsedMs",J::Number(elapsed.round()))]);
        validate_host("BackupResult",&result)?;Ok(result)
    }.await;
    if let Err(error) = &result {
        if created {
            if let Ok(at) = now(&b) {
                let _ = config::atomic_json(
                    native(&join(&[destination, "backup-failure.json".into()]))?,
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
