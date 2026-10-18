use super::{ServiceFixture,host_configuration,fixture_node,assert_json_eq};
use ivy_host_runtime::{IvyJson,config,accepted_configuration::save_accepted_configuration,executor_lock::ExecutorLock,journal::HostJournal,policy_import::{self,PolicyImportRequest,ReviewedPolicyBinding}};
use ivy_contracts::{hash::hash_json,json,core_schema::validate_host};
use ivy_store::rusqlite::Connection;
use std::{fs,io::Write,path::PathBuf,process::{Command,Stdio},sync::Mutex,time::Duration};
static CORPUS:Mutex<()>=Mutex::new(());
fn record(value:IvyJson){if let Some(file)=std::env::var_os("IVY_HOST_POLICY_IMPORT_CORPUS"){let _guard=CORPUS.lock().unwrap();writeln!(fs::OpenOptions::new().create(true).append(true).open(file).unwrap(),"{}",json::stringify(&value,usize::MAX).unwrap()).unwrap();}}
fn original(input:IvyJson)->IvyJson{
 let mut child=Command::new("node").args(["--experimental-transform-types","--disable-warning=ExperimentalWarning","tests/fixtures/host-policy-import-fixture.mjs"]).current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();child.stdin.take().unwrap().write_all(json::stringify(&input,usize::MAX).unwrap().as_bytes()).unwrap();let output=child.wait_with_output().unwrap();assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));json::parse(std::str::from_utf8(&output.stdout).unwrap(),usize::MAX).unwrap()
}

fn hash(c:&str)->IvyJson{format!("sha256:{}",c.repeat(64)).into()}
fn at()->IvyJson{"2026-01-01T00:00:00.000Z".into()}
fn target(host:&IvyJson,id:&str,revision:&str,candidate:&str,desired:&str)->IvyJson{
 let root=PathBuf::from(host.required("runtimeRoot").unwrap().as_str().unwrap());
 IvyJson::object([("schemaVersion",IvyJson::Number(1.0)),("instanceId",id.into()),("revision",revision.into()),("candidateId",hash(candidate)),("desired",desired.into()),("configPath",root.join("instances").join(id).join("configurations").join(revision.replace(':',"-")).join("config.json").to_str().unwrap().into()),("requestedAt",at())])
}
struct CopyFixture{_fixture:ServiceFixture,host:IvyJson,accepted:IvyJson,reference:PathBuf,targets:Vec<IvyJson>,database:PathBuf,source:PathBuf,source_bytes:Vec<u8>,data:IvyJson,original_state:IvyJson,files:Vec<(PathBuf,Vec<u8>)>}
impl CopyFixture{
 async fn new()->Self{
  let f=ServiceFixture::new();let mut accepted=host_configuration(&f);let mut instances=accepted.required("instances").unwrap().as_array().unwrap().to_vec();
  for id in ["stopped-fixture","bootstrap-fixture"]{let mut instance=instances[0].clone();instance.insert("instanceId",id.into());instance.insert("serviceNodeId",id.into());instances.push(instance);}
  accepted.insert("instances",IvyJson::Array(instances));let mut executables=accepted.required("executables").unwrap().clone();executables.insert("tool","historical-tool".into());accepted.insert("executables",executables);
  let root=PathBuf::from(accepted.required("runtimeRoot").unwrap().as_str().unwrap());let database=root.join("deployments.sqlite");
  let mut candidates=Vec::new();let mut files=Vec::new();
  for tag in ["a","b"]{let mut candidate=super::journal_cases::candidate();candidate.insert("candidateId",hash(tag));candidate.insert("buildId",hash(tag));let artifact=f.0.join("artifacts").join(tag);candidate.insert("artifactRoot",artifact.to_str().unwrap().into());candidate.insert("manifestPath",artifact.join("manifest.json").to_str().unwrap().into());let mut manifest=super::journal_cases::manifest();manifest.insert("buildId",hash(tag));candidates.push(IvyJson::Array(vec![candidate,manifest]));}
  let targets=vec![target(&accepted,"fixture","current-launch","a","running"),target(&accepted,"stopped-fixture","stopped-launch","a","stopped"),target(&accepted,"bootstrap-fixture",&format!("sha256:{}","c".repeat(64)),"a","running"),target(&accepted,"fixture","rollback-launch","b","running"),target(&accepted,"bootstrap-fixture","old-bootstrap-launch","b","running")];
  let mut configurations=Vec::new();
  for target in &targets{
   let instance=accepted.required("instances").unwrap().as_array().unwrap().iter().find(|instance|instance.get("instanceId")==target.get("instanceId")).unwrap();let paths=ivy_host_runtime::layout::service_paths(&accepted,instance).unwrap();let selected=candidates.iter().find(|pair|pair.as_array().unwrap()[0].get("candidateId")==target.get("candidateId")).unwrap().as_array().unwrap();let candidate=&selected[0];
   let value=IvyJson::object([("schemaVersion",IvyJson::Number(1.0)),("hostId",accepted.required("hostId").unwrap().clone()),("instanceId",instance.required("instanceId").unwrap().clone()),("serviceNodeId",instance.required("serviceNodeId").unwrap().clone()),("componentId",instance.required("componentId").unwrap().clone()),("publicBaseUrl",accepted.required("publicBaseUrl").unwrap().clone()),("dataRoot",paths.required("data").unwrap().clone()),("workRoot",paths.required("work").unwrap().clone()),("logsRoot",paths.required("logs").unwrap().clone()),("artifactRoot",candidate.required("artifactRoot").unwrap().clone()),("buildId",candidate.required("buildId").unwrap().clone()),("version","1.0.0".into()),("credential","retained-fixture-credential".into()),("settings",IvyJson::object([("retainedValue",target.required("revision").unwrap().clone())]))]);validate_host("InstanceConfig",&value).unwrap();configurations.push(IvyJson::Array(vec![target.required("configPath").unwrap().clone(),value]));
  }
  let mut original_state=original(IvyJson::object([("mode","seed".into()),("host",accepted.clone()),("candidates",IvyJson::Array(candidates.clone())),("targets",IvyJson::Array(targets.clone())),("configurations",IvyJson::Array(configurations.clone()))]));let reference=PathBuf::from(original_state.required("reference").unwrap().as_str().unwrap());original_state.insert("reference",IvyJson::Null);
  for pair in &configurations{let file=PathBuf::from(pair.as_array().unwrap()[0].as_str().unwrap());files.push((file.clone(),fs::read(file).unwrap()));}
  files.push((reference.clone(),fs::read(&reference).unwrap()));
  for pair in &candidates{let candidate=&pair.as_array().unwrap()[0];for file in [PathBuf::from(candidate.required("manifestPath").unwrap().as_str().unwrap()),PathBuf::from(candidate.required("artifactRoot").unwrap().as_str().unwrap()).join("dist/build-info.json")]{files.push((file.clone(),fs::read(file).unwrap()));}}
  for instance in accepted.required("instances").unwrap().as_array().unwrap(){let data=PathBuf::from(ivy_host_runtime::layout::service_paths(&accepted,instance).unwrap().required("data").unwrap().as_str().unwrap()).join("retained.txt");fs::write(&data,b"retained application data").unwrap();files.push((data,Vec::from(b"retained application data")));}
  {let db=Connection::open(&database).unwrap();db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
  }
  // Apply only to a verified copy in this isolated installation. The retained
  // source SQLite file is never opened for writing by the conversion.
  let source=root.join("source-before-import.sqlite");fs::copy(&database,&source).unwrap();let source_bytes=fs::read(&source).unwrap();fs::copy(&source,&database).unwrap();
  let data=saved_data(&database);let mut host=accepted.clone();let mut tools=host.required("executables").unwrap().clone();tools.insert("tool","new-desired-tool".into());host.insert("executables",tools);
  Self{_fixture:f,host,accepted,reference,targets,database,source,source_bytes,data,original_state,files}
 }
 async fn request(&self,id:&str)->PolicyImportRequest{
  let plan=policy_import::preview_target_policies(self.host.clone()).await.unwrap();
  PolicyImportRequest{import_id:id.into(),expected_state_hash:plan.required("stateHash").unwrap().as_str().unwrap(),require_complete:true,bindings:self.targets.iter().map(|target|ReviewedPolicyBinding{target:target.clone(),configuration_path:self.reference.clone(),configuration_hash:hash_json(&self.accepted).unwrap()}).collect()}
 }
 fn unchanged(&self){assert_json_eq(&saved_data(&self.database),&self.data);assert_eq!(fs::read(&self.source).unwrap(),self.source_bytes);for(file,bytes)in &self.files{assert_eq!(&fs::read(file).unwrap(),bytes,"{}",file.display());}}
}
fn saved_data(database:&std::path::Path)->IvyJson{
 let db=Connection::open(database).unwrap();let mut values=Vec::new();
 for(table,key)in[("runtime_targets","instance_id"),("operations","operation_id"),("candidates","candidate_id"),("installed","instance_id"),("retired_operations","operation_id"),("observations","instance_id"),("candidate_retirements","candidate_id")]{
  let mut statement=db.prepare(&format!("SELECT * FROM {table} ORDER BY {key}")).unwrap();let columns=statement.column_count();let rows=statement.query_map([],|row|(0..columns).map(|column|row.get::<_,String>(column).map(IvyJson::from)).collect::<ivy_store::rusqlite::Result<Vec<_>>>()).unwrap().map(|row|IvyJson::Array(row.unwrap())).collect();values.push((table,IvyJson::Array(rows)));
 }
 let mut statement=db.prepare("SELECT key,value FROM meta WHERE key NOT GLOB 'runtime-policy:*' AND key NOT GLOB 'runtime-policy-import:*' ORDER BY key").unwrap();let rows=statement.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?))).unwrap().map(|row|{let(key,value)=row.unwrap();IvyJson::Array(vec![key.into(),value.into()])}).collect();values.push(("meta",IvyJson::Array(rows)));IvyJson::object(values)
}
fn policy_count(database:&std::path::Path)->i64{Connection::open(database).unwrap().query_row("SELECT count(*) FROM meta WHERE key GLOB 'runtime-policy:*'",[],|row|row.get(0)).unwrap()}
fn import_holds_executor(root:&std::path::Path)->bool{
 // A read-only witness does not compete for the importer's exclusive claim.
 let Ok(db)=Connection::open_with_flags(root.join("executor-lock.sqlite"),ivy_store::rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)else{return false};db.busy_timeout(Duration::from_millis(10)).unwrap();
 matches!(db.query_row::<i64,_,_>("SELECT pid FROM owner LIMIT 1",[],|row|row.get(0)),Err(ivy_store::rusqlite::Error::SqliteFailure(error,_)) if matches!(error.code,ivy_store::rusqlite::ErrorCode::DatabaseBusy|ivy_store::rusqlite::ErrorCode::DatabaseLocked))
}

#[tokio::test]
async fn explicit_policy_import_keeps_copied_current_stopped_bootstrap_and_rollback_data_usable(){
 let f=CopyFixture::new().await;let request=f.request("reviewed-import").await;assert_eq!(policy_import::preview_target_policies(f.host.clone()).await.unwrap().get("unassociatedTargets"),Some(&IvyJson::Number(5.0)));f.unchanged();
 let receipt=policy_import::import_target_policies(f.host.clone(),request.clone()).await.unwrap();assert_eq!(receipt.required("remainingTargets").unwrap().as_array().unwrap().len(),0);assert_eq!(policy_count(&f.database),5);f.unchanged();
 assert_json_eq(&policy_import::import_target_policies(f.host.clone(),request.clone()).await.unwrap(),&receipt);f.unchanged();
 let journal=HostJournal::open(f.host.clone()).await.unwrap();for target in &f.targets{assert_json_eq(&journal.accepted_target_policy(target.clone()).await.unwrap().unwrap(),&f.accepted);}
 let mut native_configs=Vec::new();for target in &f.targets{native_configs.push(config::instance_config(PathBuf::from(target.required("configPath").unwrap().as_str().unwrap()),&fixture_node("24.18.0")).await.unwrap().json().clone());}assert_json_eq(&IvyJson::Array(native_configs.clone()),f.original_state.required("configurations").unwrap());
 for id in ["fixture","stopped-fixture","bootstrap-fixture"]{assert!(journal.target(id).await.unwrap().is_some());}
 assert_json_eq(journal.operation("retained-0").await.unwrap().unwrap().required("activation").unwrap().required("previousTarget").unwrap(),&f.targets[3]);assert_eq!(journal.candidate(&hash("b").as_str().unwrap()).await.unwrap().get("buildId"),Some(&hash("b")));journal.close().await.unwrap();f.unchanged();
 let after_original=original(IvyJson::object([("mode","inspect".into()),("host",f.host.clone())]));assert_json_eq(&after_original,&f.original_state);f.unchanged();record(IvyJson::object([("case","copy-original-current-stopped-bootstrap-rollback".into()),("beforeOriginal",f.original_state.clone()),("receipt",receipt),("afterOriginal",after_original),("nativeConfigurations",IvyJson::Array(native_configs)),("allOriginalRowsAndSourceFilePreserved",true.into())]));
 let mut changed=request;changed.bindings[0].configuration_hash=hash("f").as_str().unwrap();assert_eq!(policy_import::import_target_policies(f.host.clone(),changed).await.unwrap_err().code,"mutation_conflict");
}

#[tokio::test]
async fn policy_import_rolls_back_stale_missing_tampered_and_conflicting_review_bindings(){
 for mode in ["stale","incomplete","hash","target","duplicate","file","transaction"]{
  let f=CopyFixture::new().await;let mut request=f.request("rejected-import").await;
  match mode{
   "stale"=>request.expected_state_hash=hash("f").as_str().unwrap(),"incomplete"=>{request.bindings.pop();},"hash"=>request.bindings[4].configuration_hash=hash("f").as_str().unwrap(),
   "target"=>request.bindings[4].target.insert("requestedAt","2026-02-01T00:00:00.000Z".into()),"duplicate"=>request.bindings.push(request.bindings[0].clone()),
   "file"=>request.bindings[4].configuration_path=f.reference.with_file_name("missing.json"),"transaction"=>{Connection::open(&f.database).unwrap().execute_batch("CREATE TRIGGER reject_policy_import BEFORE INSERT ON meta WHEN NEW.key GLOB 'runtime-policy:*' AND (SELECT count(*) FROM meta WHERE key GLOB 'runtime-policy:*')=1 BEGIN SELECT RAISE(ABORT,'copy-only failure'); END").unwrap();},_=>{},
  }
  let error=policy_import::import_target_policies(f.host.clone(),request).await.unwrap_err();assert_eq!(policy_count(&f.database),0,"{mode}");f.unchanged();record(IvyJson::object([("case",mode.into()),("error",error.to_wire()),("allOriginalRowsAndSourceFilePreserved",true.into())]));
 }
 let f=CopyFixture::new().await;let request=f.request("first").await;policy_import::import_target_policies(f.host.clone(),request).await.unwrap();let mut conflicting=f.request("rebind").await;
 let mut other=f.accepted.clone();let mut tools=other.required("executables").unwrap().clone();tools.insert("tool","substituted-policy".into());other.insert("executables",tools);let file=save_accepted_configuration(&other).unwrap();for binding in &mut conflicting.bindings{binding.configuration_path=file.clone();binding.configuration_hash=hash_json(&other).unwrap();}
 assert_eq!(policy_import::import_target_policies(f.host.clone(),conflicting).await.unwrap_err().code,"target_conflict");assert_eq!(policy_count(&f.database),5);f.unchanged();
 Connection::open(&f.database).unwrap().execute("UPDATE meta SET value='{}' WHERE key=(SELECT key FROM meta WHERE key GLOB 'runtime-policy:*' LIMIT 1)",[]).unwrap();assert!(policy_import::preview_target_policies(f.host.clone()).await.is_err());f.unchanged();
}

#[tokio::test]
async fn policy_import_partial_batches_are_explicit_and_live_owner_locks_refuse_conversion(){
 let f=CopyFixture::new().await;let mut first=f.request("part-1").await;first.require_complete=false;first.bindings.truncate(2);let receipt=policy_import::import_target_policies(f.host.clone(),first.clone()).await.unwrap();assert_eq!(receipt.required("remainingTargets").unwrap().as_array().unwrap().len(),3);assert_json_eq(&policy_import::import_target_policies(f.host.clone(),first).await.unwrap(),&receipt);f.unchanged();
 let root=PathBuf::from(f.host.required("runtimeRoot").unwrap().as_str().unwrap());
 for owner in [root.clone(),root.join("owners").join("fixture")]{let held=ExecutorLock::acquire(owner,false).await.unwrap();let request=f.request("part-2").await;assert_eq!(policy_import::import_target_policies(f.host.clone(),request).await.unwrap_err().code,"executor_already_running");held.close().await.unwrap();assert_eq!(policy_count(&f.database),2);f.unchanged();}
 let final_request=f.request("part-2").await;let final_receipt=policy_import::import_target_policies(f.host.clone(),final_request).await.unwrap();assert_eq!(policy_count(&f.database),5);f.unchanged();record(IvyJson::object([("case","partial-batches-and-owner-locks".into()),("firstReceipt",receipt),("finalReceipt",final_receipt),("allOriginalRowsAndSourceFilePreserved",true.into())]));
}

#[tokio::test]
async fn a_dropped_import_waiter_retains_locks_and_finishes_the_original_transaction(){
 let f=CopyFixture::new().await;let request=f.request("dropped-waiter").await;let writer=Connection::open(&f.database).unwrap();writer.execute_batch("BEGIN IMMEDIATE").unwrap();let host=f.host.clone();let waiter=tokio::spawn(async move{policy_import::import_target_policies(host,request).await});let root=PathBuf::from(f.host.required("runtimeRoot").unwrap().as_str().unwrap());
 tokio::time::timeout(Duration::from_secs(5),async{while !import_holds_executor(&root){assert!(!waiter.is_finished(),"Import finished before the blocked transaction witness");tokio::time::sleep(Duration::from_millis(5)).await;}}).await.unwrap();waiter.abort();let _=waiter.await;writer.execute_batch("COMMIT").unwrap();
 tokio::time::timeout(Duration::from_secs(5),async{loop{if policy_count(&f.database)==5{if let Ok(released)=ExecutorLock::acquire(root.clone(),false).await{released.close().await.unwrap();break}}tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();f.unchanged();record(IvyJson::object([("case","dropped-waiter-retains-import-ownership".into()),("associations",IvyJson::Number(5.0)),("allOriginalRowsAndSourceFilePreserved",true.into())]));
}
