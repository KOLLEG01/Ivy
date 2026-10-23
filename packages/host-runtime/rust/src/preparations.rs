//! Durable preparation identities and records, independent of build execution.
use crate::{IvyError, IvyJson, IvyResult, Outcome, artifact, config::{read_json, write_json, FileError}, paths::{absolute, realpath}, journal::HostJournal, node_runtime::NodeRuntime, executor_lock::ExecutorLock};
use ivy_contracts::{core_schema::validate_host, hash::hash_json};
use std::{fs, path::{Path, PathBuf}};
mod retention;
pub use retention::{PreparationStorageCollection, collect_preparation_storage};

fn internal<T, E>(result: Result<T, E>) -> IvyResult<T> { result.map_err(|_| IvyError::internal()) }
fn require(ok:bool,code:&str,message:&str)->IvyResult<()>{if ok{Ok(())}else{Err(IvyError::new(code,message))}}
fn root_of(staging:&Path,id:&str)->PathBuf{staging.join(format!("build-{id}"))}
fn now()->IvyJson{chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true).into()}
async fn blocking<T:Send+'static>(work:impl FnOnce()->IvyResult<T>+Send+'static)->IvyResult<T>{internal(tokio::task::spawn_blocking(work).await)?}
async fn exists(path:PathBuf)->IvyResult<bool>{blocking(move||match fs::symlink_metadata(&path){Ok(_)=>Ok(true),Err(e) if e.kind()==std::io::ErrorKind::NotFound=>Ok(false),Err(_)=>Err(IvyError::internal())}).await}
pub fn preparation_id_for_build(build_id: &str) -> IvyResult<String> {
    if build_id.len() != 71 || !build_id.starts_with("sha256:")
        || !build_id[7..].bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) {
        return Err(IvyError::new("build_mismatch", "Preparation needs an exact release build identity."));
    }
    let hash = &build_id[7..]; let digit = hash.as_bytes()[16];
    let value = if digit <= b'9' { digit - b'0' } else { digit - b'a' + 10 };
    Ok(format!("{}-{}-5{}-{:x}{}-{}", &hash[..8], &hash[8..12], &hash[13..16], (value & 3) | 8, &hash[17..20], &hash[20..32]))
}
fn build_name(name: &str) -> bool {
    let Some(id) = name.strip_prefix("build-") else { return false };
    id.len() == 36 && id.bytes().enumerate().all(|(i, c)| {
        if [8, 13, 18, 23].contains(&i) { c == b'-' } else { c.is_ascii_digit() || (b'a'..=b'f').contains(&c) }
    })
}
pub(crate) fn owned_root(staging_root: &Path, root: &Path) -> IvyResult<PathBuf> {
    let valid_name = root.file_name().and_then(|v| v.to_str()).is_some_and(build_name);
    if !valid_name || internal(absolute(root))?.parent() != Some(internal(absolute(staging_root))?.as_path()) {
        return Err(IvyError::new("target_conflict", "Preparation must be one directly owned build directory."));
    }
    let metadata = internal(fs::symlink_metadata(root))?;
    let staging = internal(realpath(staging_root))?; let actual = internal(realpath(root))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() || actual.parent() != Some(staging.as_path()) || actual.file_name() != root.file_name() {
        return Err(IvyError::new("target_conflict", "Preparation directory cannot traverse a link."));
    }
    Ok(actual)
}
pub async fn save_preparation(staging_root: impl Into<PathBuf>, record: IvyJson) -> IvyResult<()> {
    let staging_root = staging_root.into();
    internal(tokio::task::spawn_blocking(move || {
        validate_host("PreparationRecord", &record)?;
        let root = root_of(&staging_root,&record.required("preparationId")?.as_str()?);
        owned_root(&staging_root, &root)?;
        write_json(&root.join("preparation.json"), &record)
    }).await)?
}
/// Read exactly the requested build record; recovery must not scan all history.
pub async fn preparation_record(staging_root: impl Into<PathBuf>, preparation_id: &str) -> IvyResult<Option<IvyJson>> {
    let staging_root = staging_root.into(); let id = preparation_id.to_owned();
    internal(tokio::task::spawn_blocking(move || read_record(&staging_root, &root_of(&staging_root,&id))).await)?
}
pub(crate) fn read_record(staging_root: &Path, root: &Path) -> IvyResult<Option<IvyJson>> {
    owned_root(staging_root, root)?;
    let record = match read_json(&root.join("preparation.json"), 32768) {
        Ok(value) => value,
        Err(FileError::Missing) => return Ok(None),
        Err(error) => return Err(error.error()),
    };
    validate_host("PreparationRecord", &record)?;
    let candidate = record.get("candidate").filter(|v| !v.is_null());
    if root_of(staging_root,&record.required("preparationId")?.as_str()?) != root
        || candidate.is_some_and(|v| v.get("buildId") != record.get("buildId") || v.get("componentId") != record.get("componentId")) {
        return Err(IvyError::new("build_mismatch", "Preparation record does not match its release identity."));
    }
    Ok(Some(record))
}

/// Inventory only directly owned preparation names. Invalid or linked entries
/// remain visible with their error code, without walking outside staging.
pub async fn preparation_inventory(host: IvyJson) -> IvyResult<IvyJson> {
    let staging = crate::config::path_value(host.required("stagingRoot")?)?;
    let inventory_root = staging.clone();
    let roots = internal(tokio::task::spawn_blocking(move || {
        let directory = match fs::read_dir(&inventory_root) {
            Ok(directory) => directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(IvyError::internal()),
        };
        let mut roots = Vec::new();
        for entry in directory {
            let entry = internal(entry)?;
            if let Some(name)=entry.file_name().to_str().filter(|name|build_name(name)){roots.push(inventory_root.join(name));}
        }
        if roots.len() > 10_000 {
            return Err(IvyError::new("limit_exceeded", "Preparation directory count exceeds its bounded inventory."));
        }
        // Admitted names are fixed-length ASCII UUIDs with a stable byte order.
        roots.sort();
        Ok(roots)
    }).await)??;
    let mut entries = Vec::with_capacity(roots.len());
    for root in roots {
        let id = root.file_name().and_then(|name| name.to_str()).ok_or_else(IvyError::internal)?[6..].to_owned();
        let mut entry = IvyJson::object([
            ("preparationId", id.into()), ("path", crate::layout::path_json(&root)),
            ("phase", "unrecognized".into()), ("buildId", IvyJson::Null),
            ("candidateId", IvyJson::Null), ("bytes", IvyJson::Number(0.0)), ("code", IvyJson::Null),
        ]);
        let record_root = root.clone(); let record_staging = staging.clone();
        let record = internal(tokio::task::spawn_blocking(move || read_record(&record_staging, &record_root)).await)?;
        let measured = async {
            if let Some(record) = record? {
                entry.insert("phase", record.required("phase")?.clone());
                entry.insert("buildId", record.required("buildId")?.clone());
                entry.insert("candidateId", record.get("candidate").and_then(|candidate| candidate.get("candidateId")).cloned().unwrap_or(IvyJson::Null));
                entry.insert("code", record.required("errorCode")?.clone());
            }
            let boundary_root = root.clone(); let boundary_staging = staging.clone();
            let boundary = internal(tokio::task::spawn_blocking(move || owned_root(&boundary_staging, &boundary_root)).await)??;
            let bytes = crate::storage_space::tree_bytes(root, Some(boundary)).await?;
            entry.insert("bytes", IvyJson::Number(bytes));
            Ok::<(), IvyError>(())
        }.await;
        if let Err(error) = measured { entry.insert("code", error.code.into()); }
        entries.push(entry);
    }
    let result = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)), ("hostId", host.required("hostId")?.clone()),
        ("observedAt", chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true).into()),
        ("availableBytes", IvyJson::Number(crate::storage_space::available_storage_bytes(staging).await?)),
        ("entries", IvyJson::Array(entries)),
    ]);
    validate_host("PreparationInventory", &result)?;
    Ok(result)
}

async fn roots(staging:PathBuf)->IvyResult<Vec<PathBuf>>{
    blocking(move||{
        let entries=match fs::read_dir(&staging){Ok(value)=>value,Err(e) if e.kind()==std::io::ErrorKind::NotFound=>return Ok(Vec::new()),Err(_)=>return Err(IvyError::internal())};
        let mut roots=Vec::new();
        for entry in entries{let entry=internal(entry)?;if let Some(name)=entry.file_name().to_str().filter(|name|build_name(name)){roots.push(staging.join(name));}}
        require(roots.len()<=10000,"limit_exceeded","Preparation directory count exceeds its bounded inventory.")?;roots.sort();Ok(roots)
    }).await
}
async fn record_at(staging:PathBuf,root:PathBuf)->IvyResult<Option<IvyJson>>{blocking(move||read_record(&staging,&root)).await}
async fn checked_boundary(staging:PathBuf,root:PathBuf)->IvyResult<PathBuf>{blocking(move||owned_root(&staging,&root)).await}
async fn actual(path:PathBuf)->IvyResult<PathBuf>{blocking(move||internal(realpath(&path))).await}
fn path_field(value:&IvyJson,key:&str)->IvyResult<PathBuf>{crate::config::path_value(value.required(key)?)}
fn record_change(record:&IvyJson,phase:&str,error:IvyJson)->IvyJson{let mut next=record.clone();next.insert("phase",phase.into());next.insert("updatedAt",now());next.insert("errorCode",error);next}
async fn publication(journal:&HostJournal,record:&IvyJson,verify_release:bool,node:Option<&NodeRuntime>)->IvyResult<(IvyJson,bool)>{
    let host=journal.config().await?;let candidate=record.get("candidate").filter(|v|!v.is_null()).ok_or_else(||IvyError::new("preparation_incomplete","Preparation has no complete publication identity."))?;
    let artifact_root=path_field(&host,"artifactRoot")?;let id=candidate.required("candidateId")?.as_str()?;
    let directory=artifact_root.join("candidates").join(id.get(7..).unwrap_or_default());
    require(candidate.get("artifactRoot")==Some(&crate::layout::path_json(&directory.join("artifact")))
        && candidate.get("archivePath").filter(|v|!v.is_null()).is_none_or(|path|path==&crate::layout::path_json(&directory.join("package.tar.gz")))
        && candidate.get("manifestPath")==Some(&crate::layout::path_json(&directory.join("component.json"))),"target_conflict","Publication paths must identify the exact candidate directory.")?;
    if verify_release{crate::source::verify_snapshot(record.required("snapshot")?.clone(),path_field(&host,"stagingRoot")?).await?;}
    let manifest=if verify_release{artifact::verify_candidate(candidate.clone(),artifact_root,node).await?}else{artifact::read_release_manifest(path_field(candidate,"manifestPath")?).await?};
    let plan=crate::source::source_build_plan(path_field(record.required("snapshot")?,"sourceRoot")?,&record.required("componentId")?.as_str()?).await?;
    let mut expected=IvyJson::object([("schemaVersion",IvyJson::Number(1.0)),("buildId",candidate.required("buildId")?.clone())]);
    for key in ["componentId","kind","version","connectsToHive","requirements"]{expected.insert(key,plan.required(key)?.clone());}
    let app=plan.get("kind")==Some(&"app".into());
    for key in if app{&["app"][..]}else{&["entrypoint","readiness","shutdown","restart"][..]}{if let Some(value)=plan.get(key){expected.insert(*key,value.clone());}}
    for key in ["storage","runtimeReset"]{if let Some(value)=plan.get(key).filter(|v|!v.is_null()){expected.insert(key,value.clone());}}
    require(hash_json(&expected)?==hash_json(&manifest)?,"build_mismatch","Recovered release manifest differs from its accepted source plan.")?;
    require(plan.required("checks")?.as_array().is_some_and(|v|v.is_empty()),"checks_required","Recovered publication contains unsupported diagnostic checks.")?;
    let recovered=match journal.candidate(&id).await{Ok(prior)=>{require(hash_json(&prior)?==hash_json(candidate)?,"build_mismatch","Published candidate differs from its host journal identity.")?;false},Err(e) if e.code=="not_found"=>true,Err(e)=>return Err(e)};
    journal.save_candidate(candidate.clone(),manifest).await?;Ok((candidate.clone(),recovered))
}

/// Internal caller owns the exact build lock for the entire operation. Public
/// batch/recovery entrypoints retain that ownership after waiter cancellation.
pub(crate) async fn compact_preparation_locked(journal:&HostJournal,record:IvyJson,trusted:bool,measure:bool,node:Option<&NodeRuntime>)->IvyResult<IvyJson>{
    let host=journal.config().await?;let staging=path_field(&host,"stagingRoot")?;let id=record.required("preparationId")?.as_str()?;
    let root=root_of(&staging,&id);let boundary=checked_boundary(staging.clone(),root.clone()).await?;let phase=record.required("phase")?.as_str()?;
    let candidate=record.get("candidate").filter(|v|!v.is_null());
    require(["publishing","verified","compacted"].contains(&phase.as_str())||phase=="unknown"&&candidate.is_some(),"preparation_incomplete","Only completed publication may be compacted.")?;
    let candidate=candidate.ok_or_else(||IvyError::new("preparation_incomplete","Completed preparation needs its original candidate identity."))?;
    let empty=if phase=="compacted"{!crate::concurrency::map_concurrent(["source","artifact","artifact.tar.gz"].map(|name|root.join(name)).to_vec(),3,|path,_|exists(path)).await?.iter().any(|exists|*exists)}else{false};
    if phase=="compacted"&&empty{
        match journal.candidate(&candidate.required("candidateId")?.as_str()?).await{
            Ok(prior)=>{require(hash_json(&prior)?==hash_json(candidate)?,"build_mismatch","Compacted preparation and registered candidate differ.")?;
                return Ok(IvyJson::object([("preparationId",id.into()),("action","retained".into()),("candidateId",candidate.required("candidateId")?.clone()),("reclaimedBytes",IvyJson::Number(0.0)),("code","already_compacted".into())]));},
            Err(e) if e.code=="not_found"=>{},Err(e)=>return Err(e),
        }
    }
    let(candidate,recovered)=publication(journal,&record,!trusted,node).await?;
    let snapshot=record.required("snapshot")?;let mut authoritative=vec![path_field(snapshot,"sourceRoot")?,path_field(snapshot,"manifestPath")?,path_field(&candidate,"artifactRoot")?];
    if let Some(path)=candidate.get("archivePath").filter(|v|!v.is_null()){authoritative.push(crate::config::path_value(path)?);}authoritative.push(path_field(&candidate,"manifestPath")?);
    for path in authoritative{require(!internal(crate::paths::inside(&boundary,&actual(path).await?))?,"target_conflict","Compaction cannot remove an authoritative source snapshot or published candidate.")?;}
    let mut payloads=Vec::new();
    for name in ["source","artifact","artifact.tar.gz"]{
        let path=root.join(name);if !exists(path.clone()).await?{continue}
        let target=path.clone();let metadata=blocking(move||internal(fs::symlink_metadata(&target))).await?;
        require(!metadata.file_type().is_symlink()&&if name=="artifact.tar.gz"{metadata.is_file()}else{metadata.is_dir()},"target_conflict","Compaction payload root must be a regular owned file/directory.")?;
        require(actual(path.clone()).await?.parent()==Some(boundary.as_path()),"target_conflict","Compaction target leaves its exact owned build.")?;
        let bytes=if measure{crate::storage_space::tree_bytes(path.clone(),Some(boundary.clone())).await.unwrap_or(0.0)}else{0.0};payloads.push((path,metadata.is_dir(),bytes));
    }
    if record.get("errorCode").is_some_and(|v|!v.is_null())&&!exists(root.join("preparation-before-recovery.json")).await?{crate::config::atomic_json(root.join("preparation-before-recovery.json"),record.clone()).await?;}
    save_preparation(staging.clone(),record_change(&record,"verified",IvyJson::Null)).await?;
    for(path,directory,_)in &payloads{
        checked_boundary(staging.clone(),root.clone()).await?;
        let target=path.clone();let metadata=blocking(move||internal(fs::symlink_metadata(&target))).await?;
        require(!metadata.file_type().is_symlink()&&actual(path.clone()).await?.parent()==Some(boundary.as_path()),"target_conflict","Compaction target changed before removal.")?;
        crate::filesystem::remove_payload(path.clone(),*directory,3).await?;
    }
    save_preparation(staging,record_change(&record,"compacted",IvyJson::Null)).await?;
    Ok(IvyJson::object([("preparationId",id.into()),("action",if recovered||phase=="unknown"||phase=="publishing"{"recovered"}else{"compacted"}.into()),("candidateId",candidate.required("candidateId")?.clone()),("reclaimedBytes",IvyJson::Number(payloads.iter().map(|(_,_,bytes)|bytes).sum())),("code",IvyJson::Null)]))
}

pub(crate) async fn recover_prepared_build_locked(journal:&HostJournal,build_id:&str,node:Option<&NodeRuntime>)->IvyResult<Option<IvyJson>>{
    let host=journal.config().await?;let staging=path_field(&host,"stagingRoot")?;let root=root_of(&staging,&preparation_id_for_build(build_id)?);
    if !exists(root.clone()).await?{return Ok(None)}
    let Some(record)=record_at(staging.clone(),root).await? else{return Ok(None)};
    require(record.get("buildId")==Some(&build_id.into()),"build_mismatch","Preparation directory belongs to a different release build.")?;
    let phase=record.required("phase")?.as_str()?;
    if phase=="building"{save_preparation(staging,record_change(&record,"unknown","preparation_interrupted".into())).await?;return Ok(None)}
    let Some(candidate)=record.get("candidate").filter(|v|!v.is_null())else{return Ok(None)};
    let unknown=|message:&str,detail:Option<IvyJson>|{let mut details=IvyJson::object([("preparationId",record.required("preparationId").unwrap().clone())]);if let Some(code)=detail{details.insert("code",code);}IvyError::new("outcome_unknown",message).with_outcome(Outcome::Unknown).with_details(details)};
    if phase=="unknown"||phase=="failed"{return Err(unknown("Prior candidate publication requires explicit reconciliation before another preparation.",None))}
    if phase=="compacted"{match journal.candidate(&candidate.required("candidateId")?.as_str()?).await{Ok(value)=>return Ok(Some(value)),Err(e) if e.code=="not_found"=>{},Err(e)=>return Err(e)}}
    match compact_preparation_locked(journal,record.clone(),false,false,node).await{
        Ok(_)=>Ok(Some(candidate.clone())),Err(error)=>{
            let registered=match journal.candidate(&candidate.required("candidateId")?.as_str()?).await{Ok(value)=>artifact::verify_candidate(value.clone(),path_field(&host,"artifactRoot")?,node).await.ok().map(|_|value),Err(_)=>None};
            save_preparation(staging,record_change(&record,if registered.is_some(){"verified"}else{"unknown"},error.code.clone().into())).await?;
            registered.map(Some).ok_or_else(||unknown("Interrupted publication could not be verified; its evidence is retained.",Some(error.code.into())))
        }
    }
}
/// Reconcile exactly one build, never a scan of retained preparation history.
pub async fn recover_prepared_build(journal:HostJournal,build_id:String,node:impl Into<Option<NodeRuntime>>)->IvyResult<Option<IvyJson>>{
    let node = node.into();
    preparation_id_for_build(&build_id)?;
    internal(tokio::spawn(async move{
        let host=journal.config().await?;let lock=ExecutorLock::acquire(path_field(&host,"stagingRoot")?.join("build-locks").join(&build_id[7..]),false).await?;
        let result=recover_prepared_build_locked(&journal,&build_id,node.as_ref()).await;lock.close().await?;result
    }).await)?
}
/// Own locks and journal until every selected build has finished, even if the
/// caller disconnects. Invalid/unresolved payloads remain visible and retained.
pub async fn compact_preparations(host:IvyJson,only_build_id:Option<String>,measure_bytes:bool,node:impl Into<Option<NodeRuntime>>)->IvyResult<IvyJson>{
    let node = node.into();
    internal(tokio::spawn(async move{
        let journal=HostJournal::open(host.clone()).await?;
        let result=compact_all(&host,&journal,only_build_id,measure_bytes,node.as_ref()).await;
        journal.close().await?;result
    }).await)?
}
async fn compact_all(host:&IvyJson,journal:&HostJournal,only:Option<String>,measure:bool,node:Option<&NodeRuntime>)->IvyResult<IvyJson>{
    let staging=path_field(host,"stagingRoot")?;let mut entries=Vec::new();
    for root in roots(staging.clone()).await?{
        let id=root.file_name().and_then(|v|v.to_str()).ok_or_else(IvyError::internal)?[6..].to_owned();
        let mut entry=IvyJson::object([("preparationId",id.into()),("action","retained".into()),("candidateId",IvyJson::Null),("reclaimedBytes",IvyJson::Number(0.0)),("code",IvyJson::Null)]);
        let mut lock=None;
        let operation=async{
            let Some(record)=record_at(staging.clone(),root.clone()).await?else{entry.insert("code","preparation_unrecognized".into());return Ok(true)};
            let build=record.required("buildId")?.as_str()?;if only.as_ref().is_some_and(|filter|!filter.is_empty()&&filter!=&build){return Ok(false)}
            entry.insert("candidateId",record.get("candidate").and_then(|v|v.get("candidateId")).cloned().unwrap_or(IvyJson::Null));
            lock=Some(ExecutorLock::acquire(staging.join("build-locks").join(build.get(7..).unwrap_or_default()),false).await?);
            let record=record_at(staging.clone(),root.clone()).await?.ok_or_else(||IvyError::new("preparation_unrecognized","The original preparation identity must remain present after acquiring its lock."))?;
            let phase=record.required("phase")?.as_str()?;
            if phase=="building"{save_preparation(staging.clone(),record_change(&record,"unknown","preparation_interrupted".into())).await?;}
            if !["publishing","verified","compacted"].contains(&phase.as_str())&&!(phase=="unknown"&&record.get("candidate").is_some_and(|v|!v.is_null())){entry.insert("code",record.get("errorCode").filter(|v|!v.is_null()).cloned().unwrap_or("preparation_interrupted".into()));return Ok(true)}
            entry=compact_preparation_locked(journal,record,false,measure,node).await?;Ok::<bool,IvyError>(true)
        }.await;
        if let Some(lock)=lock{lock.close().await?;}
        match operation{Ok(true)=>entries.push(entry),Ok(false)=>{},Err(e)=>{if e.code=="executor_already_running"{entry.insert("action","busy".into());}entry.insert("code",e.code.into());entries.push(entry);}}
    }
    let result=IvyJson::object([("schemaVersion",IvyJson::Number(1.0)),("hostId",host.required("hostId")?.clone()),("observedAt",now()),("entries",IvyJson::Array(entries))]);validate_host("PreparationCompaction",&result)?;Ok(result)
}
