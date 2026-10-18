use super::{ServiceFixture, host_configuration, compare_config_oracle, assert_json_eq, assert_optional_json_eq};
use ivy_contracts::{IvyJson, IvyResult, core_schema::{validate_host, validate_shared}, json};
use ivy_host_runtime::{journal::HostJournal, layout::service_paths};
use ivy_sdk::clock::Clock;
use ivy_store::rusqlite::{Connection, params};
use std::{fs, sync::Arc, time::Instant};

const NOW: i64 = 1_767_225_600_000;
const DATE: &str = "2026-01-01T00:00:00.000Z";
struct FixedClock;
impl Clock for FixedClock { fn utc_millis(&self) -> i64 { NOW } fn monotonic(&self) -> Instant { Instant::now() } }
fn hash(c: &str) -> IvyJson { format!("sha256:{}", c.repeat(64)).into() }
fn sql(config: &IvyJson) -> Connection { Connection::open(std::path::PathBuf::from(config.required("runtimeRoot").unwrap().as_str().unwrap()).join("deployments.sqlite")).unwrap() }
fn encode(value: &IvyJson) -> String { json::canonical(value, usize::MAX).unwrap() }
async fn open(config: &IvyJson) -> HostJournal { HostJournal::open_with_clock(config.clone(), Arc::new(FixedClock)).await.unwrap() }
fn input(config: &IvyJson, method: &str, args: Vec<IvyJson>) -> IvyJson {
    IvyJson::object([("kind", "journal".into()), ("config", config.clone()), ("operation", method.into()), ("args", IvyJson::Array(args)), ("now", IvyJson::Number(NOW as f64)), ("pid", IvyJson::Number(std::process::id() as f64))])
}
fn compare(config: &IvyJson, method: &str, args: Vec<IvyJson>, result: IvyResult<IvyJson>) { compare_config_oracle(input(config, method, args), result); }
fn adoption(config: &IvyJson) -> IvyJson {
    IvyJson::object([("configuration", config.clone()), ("revision", IvyJson::Number(1.0)), ("contentHash", ivy_contracts::hash::hash_json(config).unwrap().into()), ("actions", IvyJson::Array(vec![IvyJson::object([("instanceId", "fixture".into()), ("action", "restart".into()), ("operationId", "config-op".into())])])), ("bootstrapRequired", IvyJson::Array(vec![]))])
}
fn empty() -> IvyJson { IvyJson::Object(vec![]) }
#[tokio::test]
async fn journal_executor_lock_excludes_owners_and_allows_shared_readers_after_close() {
    use ivy_host_runtime::journal::ExecutorLock;
    let fixture=ServiceFixture::new(); let root=fixture.0.join("locks");
    let owner=ExecutorLock::acquire(root.clone(),false).await.unwrap();
    assert_eq!(ExecutorLock::acquire(root.clone(),false).await.err().unwrap().code,"executor_already_running");
    assert_eq!(ExecutorLock::acquire(root.clone(),true).await.err().unwrap().code,"executor_already_running");
    owner.close().await.unwrap();
    let first=ExecutorLock::acquire(root.clone(),true).await.unwrap(); let second=ExecutorLock::acquire(root.clone(),true).await.unwrap();
    assert_eq!(ExecutorLock::acquire(root.clone(),false).await.err().unwrap().code,"executor_already_running");
    first.close().await.unwrap(); second.close().await.unwrap();
    let owner=ivy_host_runtime::executor_lock::ExecutorLock::acquire(root.clone(),false).await.unwrap();
    let _: &ExecutorLock = &owner; // The journal path remains the same type.
    drop(owner);
    let owner=ExecutorLock::acquire(root,false).await.unwrap(); owner.close().await.unwrap(); owner.close().await.unwrap();
}
fn request(action: &str, operation: &str) -> IvyJson { IvyJson::object([("action", action.into()), ("operationId", operation.into()), ("instanceId", "fixture".into())]) }

fn policy_target(fixture: &ServiceFixture, revision: &str) -> IvyJson {
    IvyJson::object([("schemaVersion", IvyJson::Number(1.0)), ("instanceId", "fixture".into()), ("revision", revision.into()), ("candidateId", hash("a")), ("desired", "running".into()), ("configPath", fixture.0.join("state/instance.json").to_string_lossy().to_string().into()), ("requestedAt", DATE.into())])
}
fn policy_key(target: &IvyJson) -> String {
    format!("runtime-policy:{}", ivy_contracts::hash::hash_json(&IvyJson::object([("instanceId",target.required("instanceId").unwrap().clone()),("revision",target.required("revision").unwrap().clone())])).unwrap())
}
async fn policy_operation(journal: &HostJournal, operation: &str) -> String {
    let mut request=request("deploy",operation); request.insert("candidateId",hash("a"));
    let entry=journal.accept(request,None).await.unwrap();
    let id=entry.required("record").unwrap().required("deploymentId").unwrap().as_str().unwrap();
    journal.advance(&id,"prepared","checking",empty()).await.unwrap(); id
}
async fn select_policy_target(journal: &HostJournal, fixture: &ServiceFixture, operation: &str, revision: &str) -> (String,IvyJson) {
    let id=policy_operation(journal,operation).await;
    journal.advance(&id,"checking","draining",empty()).await.unwrap();
    let target=policy_target(fixture,revision);
    journal.advance(&id,"draining","activating",IvyJson::object([("target",target.clone())])).await.unwrap();
    (id,target)
}

#[tokio::test]
async fn accepted_target_policy_survives_restart_desired_edits_and_original_ts_cross_open() {
    let fixture=ServiceFixture::new(); let config=host_configuration(&fixture); let journal=open(&config).await;
    journal.save_candidate(candidate(),manifest()).await.unwrap();
    let id=policy_operation(&journal,"policy-accepted").await;
    let mut desired=config.clone(); let mut executables=desired.required("executables").unwrap().clone(); executables.insert("tool","different-tool".into()); desired.insert("executables",executables);
    journal.use_configuration(desired.clone()).await.unwrap();
    journal.advance(&id,"checking","draining",empty()).await.unwrap(); let target=policy_target(&fixture,"policy-1");
    journal.advance(&id,"draining","activating",IvyJson::object([("target",target.clone())])).await.unwrap();
    assert_optional_json_eq(journal.accepted_target_policy(target.clone()).await.unwrap(),Some(config.clone()));
    let key=policy_key(&target); let retained:String=sql(&config).query_row("SELECT value FROM meta WHERE key=?",[&key],|r|r.get(0)).unwrap();
    assert!(!retained.contains("different-tool"));
    journal.close().await.unwrap();
    compare(&desired,"target",vec!["fixture".into()],Ok(target.clone()));
    let journal=open(&desired).await;
    assert_optional_json_eq(journal.accepted_target_policy(target.clone()).await.unwrap(),Some(config));
    assert_eq!(sql(&desired).query_row::<String,_,_>("SELECT value FROM meta WHERE key=?",[&key],|r|r.get(0)).unwrap(),retained);
    let mut legacy=target.clone(); legacy.insert("revision","old-writer-revision".into());
    compare(&desired,"restoreTarget",vec![legacy.clone()],Ok(IvyJson::Null));
    assert_optional_json_eq(journal.target("fixture").await.unwrap(),Some(legacy.clone()));
    assert_eq!(journal.accepted_target_policy(legacy).await.unwrap(),None);
    assert!(journal.accepted_target_policy(target).await.unwrap().is_some()); journal.close().await.unwrap();
}

#[tokio::test]
async fn accepted_target_policy_carries_drain_and_rollback_including_unassociated_legacy() {
    for legacy in [false,true] {
        let fixture=ServiceFixture::new(); let config=host_configuration(&fixture); let journal=open(&config).await;
        journal.save_candidate(candidate(),manifest()).await.unwrap();
        let (_,mut prior)=select_policy_target(&journal,&fixture,"old-policy","old-launch").await;
        if legacy { prior.insert("revision","legacy-launch".into()); journal.restore_target(prior.clone()).await.unwrap(); }
        journal.set_installed(IvyJson::object([("instanceId","fixture".into()),("candidateId",hash("a")),("buildId",hash("a")),("enabled",true.into()),("installedAt",DATE.into())])).await.unwrap();
        let mut desired=config.clone(); let mut executables=desired.required("executables").unwrap().clone(); executables.insert("tool","new-tool".into()); desired.insert("executables",executables);
        journal.use_configuration(desired.clone()).await.unwrap(); let id=policy_operation(&journal,"new-policy").await;
        let mut drain=prior.clone(); drain.insert("revision","draining-launch".into()); drain.insert("desired","stopped".into());
        let mut wrong=drain.clone(); wrong.insert("configPath",fixture.0.join("state/other.json").to_string_lossy().to_string().into());
        assert_eq!(journal.advance(&id,"checking","draining",IvyJson::object([("target",wrong)])).await.unwrap_err().code,"target_conflict");
        journal.advance(&id,"checking","draining",IvyJson::object([("target",drain.clone())])).await.unwrap();
        assert_optional_json_eq(journal.accepted_target_policy(drain).await.unwrap(),(!legacy).then_some(config.clone()));
        let target=policy_target(&fixture,"new-launch"); journal.advance(&id,"draining","activating",IvyJson::object([("target",target.clone())])).await.unwrap();
        assert_optional_json_eq(journal.accepted_target_policy(target.clone()).await.unwrap(),Some(desired.clone()));
        let mut stopped=target; stopped.insert("revision","rollback-stop".into()); stopped.insert("desired","stopped".into());
        journal.advance(&id,"activating","rolling_back",IvyJson::object([("target",stopped.clone())])).await.unwrap();
        assert_optional_json_eq(journal.accepted_target_policy(stopped).await.unwrap(),Some(desired.clone()));
        let rollback=policy_target(&fixture,"rollback-launch");
        journal.advance(&id,"rolling_back","rolling_back",IvyJson::object([("target",rollback.clone()),("rollbackTargetRevision","rollback-launch".into())])).await.unwrap();
        assert_optional_json_eq(journal.accepted_target_policy(rollback.clone()).await.unwrap(),(!legacy).then_some(config.clone()));
        journal.close().await.unwrap(); let journal=open(&desired).await;
        assert_optional_json_eq(journal.accepted_target_policy(rollback).await.unwrap(),(!legacy).then_some(config));
        assert_eq!(journal.accepted_target_policy(prior).await.unwrap().is_none(),legacy); journal.close().await.unwrap();
    }
}

#[tokio::test]
async fn accepted_target_policy_rejects_corrupt_evidence_and_never_rebinds_a_revision() {
    let fixture=ServiceFixture::new(); let config=host_configuration(&fixture); let journal=open(&config).await;
    journal.save_candidate(candidate(),manifest()).await.unwrap();
    let (_,target)=select_policy_target(&journal,&fixture,"policy-original","immutable-policy").await;
    let db=sql(&config); let key=policy_key(&target); let original:String=db.query_row("SELECT value FROM meta WHERE key=?",[&key],|r|r.get(0)).unwrap();
    let mut different=target.clone(); different.insert("desired","stopped".into());
    assert_eq!(journal.accepted_target_policy(different.clone()).await.unwrap_err().code,"target_conflict");
    assert_eq!(journal.restore_target(different).await.unwrap_err().code,"target_conflict");
    for mode in ["invalid-json","version","missing-field","extra-field","wrong-target","outside","missing","directory","invalid-config","different-installation"] {
        let mut value=json::parse(&original,usize::MAX).unwrap();
        match mode {
            "version"=>value.insert("schemaVersion",IvyJson::Number(2.0)),
            "missing-field"=>{value.remove("configurationPath");},
            "extra-field"=>value.insert("extension",true.into()),
            "wrong-target"=>{let mut wrong=target.clone();wrong.insert("requestedAt","2026-01-02T00:00:00.000Z".into());value.insert("target",wrong);},
            "outside"=>value.insert("configurationPath",fixture.0.join("elsewhere.json").to_string_lossy().to_string().into()),
            "missing"=>value.insert("configurationPath",fixture.0.join("state/accepted-configurations/missing.json").to_string_lossy().to_string().into()),
            "directory"=>value.insert("configurationPath",fixture.0.join("state/accepted-configurations").to_string_lossy().to_string().into()),
            "invalid-config" | "different-installation"=>{
                let file=fixture.0.join("state/accepted-configurations/bad.json"); let mut changed=config.clone();changed.insert("hostId","different-host".into());
                fs::write(&file,if mode=="invalid-config" {"{}".to_string()}else{encode(&changed)}).unwrap();value.insert("configurationPath",file.to_string_lossy().to_string().into());
            },
            _=>(),
        }
        db.execute("UPDATE meta SET value=? WHERE key=?",params![if mode=="invalid-json" {"{".to_string()}else{encode(&value)},key]).unwrap();
        assert!(journal.accepted_target_policy(target.clone()).await.is_err(),"{mode}");
        assert_optional_json_eq(journal.target("fixture").await.unwrap(),Some(target.clone()));
    }
    db.execute("UPDATE meta SET value=? WHERE key=?",params![original,key]).unwrap();
    let id=policy_operation(&journal,"same-revision-different-acceptance").await; journal.advance(&id,"checking","draining",empty()).await.unwrap();
    assert_eq!(journal.advance(&id,"draining","activating",IvyJson::object([("target",target.clone())])).await.unwrap_err().code,"target_conflict");
    assert_eq!(journal.get(&id).await.unwrap().required("record").unwrap().required("phase").unwrap(),&IvyJson::from("draining"));
    assert_optional_json_eq(journal.target("fixture").await.unwrap(),Some(target));journal.close().await.unwrap();
}

#[tokio::test]
async fn accepted_target_policy_is_atomic_and_restore_clears_only_launch_metadata() {
    let fixture=ServiceFixture::new(); let config=host_configuration(&fixture);
    let journal=HostJournal::open_with_options(config.clone(),ivy_host_runtime::journal::HistoryLimits{count:1,bytes:1},Arc::new(FixedClock)).await.unwrap();
    journal.save_candidate(candidate(),manifest()).await.unwrap(); let id=policy_operation(&journal,"policy-transaction").await;
    journal.advance(&id,"checking","draining",empty()).await.unwrap(); let target=policy_target(&fixture,"atomic-policy"); let db=sql(&config);
    db.execute_batch("CREATE TRIGGER reject_target BEFORE INSERT ON runtime_targets BEGIN SELECT RAISE(ABORT,'fixture failure'); END").unwrap();
    assert!(journal.advance(&id,"draining","activating",IvyJson::object([("target",target.clone())])).await.is_err());
    assert_eq!(journal.accepted_target_policy(target.clone()).await.unwrap(),None); assert_eq!(journal.target("fixture").await.unwrap(),None);
    assert_eq!(journal.get(&id).await.unwrap().required("record").unwrap().required("phase").unwrap(),&IvyJson::from("draining"));
    db.execute_batch("DROP TRIGGER reject_target").unwrap(); journal.advance(&id,"draining","activating",IvyJson::object([("target",target.clone())])).await.unwrap();
    let record:String=db.query_row("SELECT value FROM meta WHERE key=?",[policy_key(&target)],|r|r.get(0)).unwrap();let record=json::parse(&record,usize::MAX).unwrap();
    let file=std::path::PathBuf::from(record.required("configurationPath").unwrap().as_str().unwrap()); assert!(file.is_file());
    journal.advance(&id,"activating","verifying",empty()).await.unwrap();journal.advance(&id,"verifying","succeeded",empty()).await.unwrap();
    // The original history policy always keeps the newest two terminal entries.
    for operation in ["retention-pressure-1","retention-pressure-2"] {
        let newer=policy_operation(&journal,operation).await;
        for (from,to) in [("checking","draining"),("draining","activating"),("activating","verifying"),("verifying","succeeded")] {
            journal.advance(&newer,from,to,empty()).await.unwrap();
        }
    }
    assert_eq!(journal.get(&id).await.unwrap_err().code,"operation_expired");
    assert_optional_json_eq(journal.accepted_target_policy(target.clone()).await.unwrap(),Some(config.clone()));assert!(file.is_file());
    db.execute("INSERT INTO meta VALUES ('recovery-original','retained')",[]).unwrap(); db.execute("INSERT INTO meta VALUES ('runtime-policy-other','untouched')",[]).unwrap();
    db.execute_batch("CREATE TRIGGER reject_policy_clear BEFORE DELETE ON meta WHEN OLD.key GLOB 'runtime-policy:*' BEGIN SELECT RAISE(ABORT,'fixture restore failure'); END").unwrap();
    assert!(journal.clear_runtime_targets_for_restore().await.is_err());assert_optional_json_eq(journal.target("fixture").await.unwrap(),Some(target.clone()));
    assert!(journal.accepted_target_policy(target.clone()).await.unwrap().is_some());db.execute_batch("DROP TRIGGER reject_policy_clear").unwrap();
    journal.clear_runtime_targets_for_restore().await.unwrap(); assert_eq!(journal.target("fixture").await.unwrap(),None); assert_eq!(journal.accepted_target_policy(target).await.unwrap(),None);
    assert_eq!(db.query_row::<String,_,_>("SELECT value FROM meta WHERE key='recovery-original'",[],|r|r.get(0)).unwrap(),"retained");
    assert_eq!(db.query_row::<String,_,_>("SELECT value FROM meta WHERE key='runtime-policy-other'",[],|r|r.get(0)).unwrap(),"untouched");
    assert_eq!(db.query_row::<i64,_,_>("PRAGMA user_version",[],|r|r.get(0)).unwrap(),1);assert!(file.is_file());
    journal.close().await.unwrap();
}

#[tokio::test]
async fn journal_admission_replays_exact_identity_and_advances_atomically() {
    let fixture=ServiceFixture::new(); let config=host_configuration(&fixture); let journal=open(&config).await;
    journal.save_candidate(candidate(), manifest()).await.unwrap();
    for request in [request("restart", "no-install"), request("deploy", "no-candidate"), request("rollback", "no-target"), { let mut v=request("deploy", "source"); v.insert("source", "source".into()); v }] {
        compare(&config, "accept", vec![request.clone()], journal.accept(request, None).await);
    }
    let mut request=request("deploy", "deploy-1"); request.insert("candidateId", hash("a"));
    let accepted=journal.accept(request.clone(), None).await.unwrap();
    assert_eq!(accepted.required("record").unwrap().required("phase").unwrap(), &IvyJson::from("prepared"));
    let source=std::path::PathBuf::from(accepted.required("configurationPath").unwrap().as_str().unwrap());
    assert_json_eq(&journal.accepted_configuration(source).await.unwrap(), &config);
    assert_json_eq(&journal.accept(request.clone(), None).await.unwrap(), &accepted);
    compare(&config, "accept", vec![request.clone()], Ok(accepted.clone()));
    let mut conflict=request.clone(); conflict.insert("candidateId", hash("b"));
    compare(&config, "accept", vec![conflict.clone()], journal.accept(conflict, None).await);
    let id=accepted.required("record").unwrap().required("deploymentId").unwrap().as_str().unwrap();
    compare(&config, "advance", vec![id.clone().into(), "checking".into(), "draining".into(), empty()], journal.advance(&id, "checking", "draining", empty()).await);
    journal.advance(&id, "prepared", "checking", empty()).await.unwrap();
    journal.advance(&id, "checking", "draining", empty()).await.unwrap();
    let target=IvyJson::object([("schemaVersion", IvyJson::Number(1.0)), ("instanceId", "fixture".into()), ("revision", "launch-1".into()), ("candidateId", hash("a")), ("desired", "running".into()), ("configPath", fixture.0.join("state/instance.json").to_string_lossy().to_string().into()), ("requestedAt", DATE.into())]);
    let mut invalid=IvyJson::object([("target", target.clone()), ("rollbackTargetRevision", "unrelated".into())]);
    let error=journal.advance(&id, "draining", "activating", invalid.clone()).await;
    assert_eq!(error.as_ref().unwrap_err().code, "target_conflict"); assert_eq!(journal.target("fixture").await.unwrap(), None);
    assert_eq!(journal.get(&id).await.unwrap().required("record").unwrap().required("phase").unwrap(), &IvyJson::from("draining"));
    compare(&config, "advance", vec![id.clone().into(), "draining".into(), "activating".into(), invalid.clone()], error);
    invalid.remove("rollbackTargetRevision"); journal.advance(&id, "draining", "activating", invalid).await.unwrap();
    assert_optional_json_eq(journal.target("fixture").await.unwrap(), Some(target));
    journal.advance(&id, "activating", "verifying", empty()).await.unwrap();
    let installed=IvyJson::object([("instanceId", "fixture".into()), ("candidateId", hash("a")), ("buildId", hash("a")), ("enabled", true.into()), ("installedAt", DATE.into())]);
    let mut wrong=installed.clone(); wrong.insert("buildId", hash("f"));
    let change=IvyJson::object([("installed", wrong)]);
    compare(&config, "advance", vec![id.clone().into(), "verifying".into(), "succeeded".into(), change.clone()], journal.advance(&id, "verifying", "succeeded", change).await);
    assert_eq!(journal.installed("fixture").await.unwrap(), None);
    journal.advance(&id, "verifying", "succeeded", IvyJson::object([("installed", installed.clone())])).await.unwrap();
    assert_optional_json_eq(journal.installed("fixture").await.unwrap(), Some(installed));
    compare(&config, "advance", vec![id.clone().into(), "succeeded".into(), "succeeded".into(), empty()], journal.advance(&id, "succeeded", "succeeded", empty()).await);
    journal.close().await.unwrap(); let journal=open(&config).await;
    assert_eq!(journal.accept(request, None).await.unwrap().required("record").unwrap().required("phase").unwrap(), &IvyJson::from("succeeded")); journal.close().await.unwrap();
}

#[tokio::test]
async fn journal_history_keeps_recovery_and_adoption_references_and_retires_old_ids() {
    use ivy_host_runtime::journal::HistoryLimits;
    let fixture=ServiceFixture::new(); let config=host_configuration(&fixture);
    let journal=HostJournal::open_with_options(config.clone(), HistoryLimits{count:1,bytes:1}, Arc::new(FixedClock)).await.unwrap(); let db=sql(&config);
    let mut seed=vec![IvyJson::object([("sql","DELETE FROM operations".into()),("params",IvyJson::Array(vec![]))]),IvyJson::object([("sql","DELETE FROM retired_operations".into()),("params",IvyJson::Array(vec![]))])];
    for (id,phase) in [("old0","succeeded"),("old1","failed"),("adoption2","succeeded"),("latest3","succeeded"),("latest4","rolled_back"),("active","preparing")] {
        db.execute("INSERT INTO operations VALUES (?,?,?,?,?,?)", params![format!("op-{id}"),id,"hash",phase,DATE,encode(&entry(id,phase,false))]).unwrap();
        seed.push(IvyJson::object([("sql","INSERT INTO operations VALUES (?,?,?,?,?,?)".into()),("params",IvyJson::Array(vec![format!("op-{id}").into(),id.into(),"hash".into(),phase.into(),DATE.into(),encode(&entry(id,phase,false)).into()]))]));
    }
    let mut adoption=adoption(&config); adoption.insert("actions", IvyJson::Array(vec![IvyJson::object([("instanceId","fixture".into()),("action","restart".into()),("operationId","op-adoption2".into())])]));
    journal.stage_configuration_adoption(adoption).await.unwrap();
    let advanced=journal.advance("active","preparing","preparing",empty()).await.unwrap();
    for id in ["old0","old1"] { assert_eq!(journal.get(id).await.unwrap_err().code,"operation_expired"); }
    for id in ["adoption2","latest3","latest4","active"] { assert!(journal.get(id).await.is_ok()); }
    assert_eq!(journal.list(200.0).await.unwrap().len(),4);
    let mut oracle=input(&config,"advance",vec!["active".into(),"preparing".into(),"preparing".into(),empty()]);
    oracle.insert("historyLimits",IvyJson::object([("count",IvyJson::Number(1.0)),("bytes",IvyJson::Number(1.0))]));oracle.insert("seed",IvyJson::Array(seed));
    compare_config_oracle(oracle,Ok(advanced));assert_eq!(journal.list(200.0).await.unwrap().len(),4);
    let mut replay=request("restart","op-old0");
    // The retired request hash is authoritative even after entry eviction.
    assert_eq!(journal.accept(replay.clone(),None).await.unwrap_err().code,"mutation_conflict");
    db.execute("UPDATE retired_operations SET request_hash=? WHERE operation_id='op-old0'",[ivy_contracts::hash::hash_json(&replay).unwrap()]).unwrap();
    compare(&config,"accept",vec![replay.clone()],journal.accept(replay.clone(),None).await);
    replay.insert("action","disable".into()); compare(&config,"accept",vec![replay.clone()],journal.accept(replay,None).await);
    journal.close().await.unwrap();
}

#[tokio::test]
async fn journal_candidate_retention_preserves_nearest_and_exact_rollback_selections() {
    let fixture=ServiceFixture::new(); let config=host_configuration(&fixture); let journal=open(&config).await;
    for (id,version) in [("a","1.0.0"),("b","2.0.0"),("c","2.0.0"),("d","3.0.0"),("e","4.0.0")] {
        let mut candidate=candidate(); candidate.insert("candidateId",hash(id));candidate.insert("buildId",hash(id));
        let mut manifest=manifest();manifest.insert("buildId",hash(id));manifest.insert("version",version.into());
        journal.save_candidate(candidate,manifest).await.unwrap();
    }
    journal.set_installed(IvyJson::object([("instanceId","fixture".into()),("candidateId",hash("d")),("buildId",hash("d")),("enabled",true.into()),("installedAt",DATE.into())])).await.unwrap();
    let ids=journal.retention_candidate_ids().await.unwrap();assert_eq!(ids, std::collections::BTreeSet::from([hash("c").as_str().unwrap(),hash("d").as_str().unwrap()]));
    compare(&config,"retentionCandidateIds",vec![],Ok(IvyJson::Array(ids.into_iter().map(IvyJson::from).collect())));
    assert!(!journal.retire_candidate(&hash("c").as_str().unwrap()).await.unwrap());
    let db=sql(&config);db.execute("INSERT INTO meta VALUES ('bootstrap-rollback:fixture',?)",[encode(&IvyJson::object([("candidateId",hash("a")),("selectedCandidateId",hash("d"))]))]).unwrap();
    let ids=journal.retention_candidate_ids().await.unwrap();assert_eq!(ids,std::collections::BTreeSet::from([hash("a").as_str().unwrap(),hash("d").as_str().unwrap()]));
    compare(&config,"retentionCandidateIds",vec![],Ok(IvyJson::Array(ids.into_iter().map(IvyJson::from).collect())));
    assert!(journal.retire_candidate(&hash("c").as_str().unwrap()).await.unwrap());
    assert_eq!(journal.candidate(&hash("c").as_str().unwrap()).await.unwrap_err().code,"not_found");
    assert_eq!(journal.pending_candidate_retirements().await.unwrap().len(),1);
    let mut legacy=manifest();legacy.insert("sourceRoot","ignored-legacy".into());
    db.execute("UPDATE candidates SET manifest_json=? WHERE candidate_id=?",params![encode(&legacy),hash("a").as_str().unwrap()]).unwrap();
    compare(&config,"manifest",vec![hash("a")],journal.manifest(&hash("a").as_str().unwrap()).await);
    journal.close().await.unwrap();
}
fn candidate() -> IvyJson {
    IvyJson::object([("candidateId", hash("a")), ("componentId", "fixture".into()), ("artifactRoot", "artifact".into()), ("manifestPath", "manifest.json".into()), ("createdAt", DATE.into()),
        ("platform", IvyJson::object([("os", "win32".into()), ("arch", "x64".into()), ("node", "24.18.0".into())])), ("buildId", hash("a"))])
}
fn manifest() -> IvyJson {
    let value = IvyJson::object([("schemaVersion", IvyJson::Number(1.0)), ("componentId", "fixture".into()), ("kind", "app".into()), ("version", "1.0.0".into()), ("buildId", hash("a")), ("connectsToHive", false.into()),
        ("requirements", IvyJson::object([("node", ">=24.18.0 <25.0.0".into()), ("hiveProtocol", IvyJson::Null), ("contracts", IvyJson::Array(vec![]))])),
        ("app", IvyJson::object([("appId", "fixture".into()), ("dist", "dist/apps/fixture".into()), ("entryPath", "index.html".into())]))]);
    validate_host("ReleaseManifest", &value).unwrap(); value
}

#[tokio::test]
async fn journal_observation_and_retention_writes_validate_before_replacing_state() {
    let fixture=ServiceFixture::new(); let mut config=host_configuration(&fixture);
    let mut instances=config.required("instances").unwrap().as_array().unwrap().to_vec(); instances[0].insert("componentId", "host-executor".into()); config.insert("instances", IvyJson::Array(instances));
    let journal=open(&config).await;
    let observation=journal.current_observations(None).await.unwrap().required("fixture").unwrap().clone();
    journal.record_observation("fixture", observation.clone()).await.unwrap();
    assert_json_eq(journal.observations().await.unwrap().required("fixture").unwrap(), &observation);
    compare(&config, "recordObservation", vec!["other".into(), observation.clone()], journal.record_observation("other", observation.clone()).await.map(|_| IvyJson::Null));
    assert_json_eq(journal.observations().await.unwrap().required("fixture").unwrap(), &observation);
    let retention=IvyJson::object([("schemaVersion", IvyJson::Number(1.0)), ("hostId", "fixture-host".into()), ("state", "succeeded".into()), ("trigger", "manual".into()), ("startedAt", DATE.into()), ("completedAt", DATE.into()), ("lastSuccessAt", DATE.into()),
        ("removed", IvyJson::object(["candidates", "preparations", "payloads", "snapshots", "maintenance", "bootstrapPlans"].map(|key| (key, IvyJson::Number(0.0))))),
        ("availableBytes", IvyJson::object(["runtime", "artifacts", "staging"].map(|key| (key, IvyJson::Number(1024.0))))), ("lowSpace", false.into()), ("skipped", IvyJson::Array(vec![])), ("errorCode", IvyJson::Null)]);
    journal.save_storage_retention_status(retention.clone()).await.unwrap(); assert_optional_json_eq(journal.storage_retention_status().await.unwrap(), Some(retention.clone()));
    compare(&config, "storageRetentionStatus", vec![], journal.storage_retention_status().await.map(|v| v.unwrap()));
    let mut invalid=retention.clone(); invalid.insert("state", "invalid".into());
    compare(&config, "saveStorageRetentionStatus", vec![invalid.clone()], journal.save_storage_retention_status(invalid).await.map(|_| IvyJson::Null));
    assert_optional_json_eq(journal.storage_retention_status().await.unwrap(), Some(retention)); journal.close().await.unwrap();
}

#[tokio::test]
async fn journal_adoption_is_idempotent_persistent_and_compare_before_delete() {
    let fixture=ServiceFixture::new(); let config=host_configuration(&fixture); let journal=open(&config).await; let value=adoption(&config);
    journal.stage_configuration_adoption(value.clone()).await.unwrap();
    assert_optional_json_eq(journal.configuration_adoption().await.unwrap(), Some(value.clone()));
    compare(&config, "stageConfigurationAdoption", vec![value.clone()], journal.stage_configuration_adoption(value.clone()).await.map(|_| IvyJson::Null));
    let mut conflict=value.clone(); conflict.insert("revision", IvyJson::Number(2.0));
    compare(&config, "stageConfigurationAdoption", vec![conflict.clone()], journal.stage_configuration_adoption(conflict.clone()).await.map(|_| IvyJson::Null));
    compare(&config, "finishConfigurationAdoption", vec![conflict.clone()], journal.finish_configuration_adoption(conflict).await.map(|_| IvyJson::Null));
    assert_optional_json_eq(journal.configuration_adoption().await.unwrap(), Some(value.clone()));
    journal.close().await.unwrap(); let journal=open(&config).await;
    compare(&config, "configurationAdoption", vec![], journal.configuration_adoption().await.map(|v| v.unwrap()));
    journal.finish_configuration_adoption(value.clone()).await.unwrap(); assert_eq!(journal.configuration_adoption().await.unwrap(), None);
    let mut oracle=input(&config, "finishConfigurationAdoption", vec![value.clone()]);
    oracle.insert("seed", IvyJson::Array(vec![IvyJson::object([("sql", "INSERT INTO meta VALUES ('configurationAdoption',?)".into()), ("params", IvyJson::Array(vec![encode(&value).into()]))])]));
    compare_config_oracle(oracle, Ok(IvyJson::Null));
    compare(&config, "finishConfigurationAdoption", vec![value.clone()], journal.finish_configuration_adoption(value.clone()).await.map(|_| IvyJson::Null));
    for revision in [0.0, 1.5] {
        let mut invalid=value.clone(); invalid.insert("revision", IvyJson::Number(revision));
        journal.stage_configuration_adoption(invalid.clone()).await.unwrap();
        assert_eq!(journal.configuration_adoption().await.unwrap_err().code, "configuration_changed");
        compare(&config, "configurationAdoption", vec![], journal.configuration_adoption().await.map(|v| v.unwrap()));
        journal.finish_configuration_adoption(invalid).await.unwrap();
    }
    let mut unsafe_revision=value.clone(); unsafe_revision.insert("revision", IvyJson::Number(9_007_199_254_740_992.0));
    compare(&config, "stageConfigurationAdoption", vec![unsafe_revision.clone()], journal.stage_configuration_adoption(unsafe_revision).await.map(|_| IvyJson::Null));
    assert_eq!(journal.configuration_adoption().await.unwrap(), None);
    // Unsafe numbers cannot pass canonical writes. Corrupt retained bytes must
    // enter through the database fixture to exercise the independent read gate.
    let corrupt=encode(&value).replace("\"revision\":1", "\"revision\":9007199254740992");
    sql(&config).execute("INSERT INTO meta VALUES ('configurationAdoption',?)",[corrupt]).unwrap();
    compare(&config, "configurationAdoption", vec![], journal.configuration_adoption().await.map(|v| v.unwrap()));
    sql(&config).execute("DELETE FROM meta WHERE key='configurationAdoption'",[]).unwrap();
    let mut wrong=value; wrong.insert("contentHash", hash("f"));
    compare(&config, "stageConfigurationAdoption", vec![wrong.clone()], journal.stage_configuration_adoption(wrong).await.map(|_| IvyJson::Null));
    journal.close().await.unwrap();
}

#[tokio::test]
async fn journal_candidate_publication_and_installed_target_writes_enforce_identity() {
    let fixture=ServiceFixture::new(); let config=host_configuration(&fixture); let journal=open(&config).await; let candidate=candidate(); let manifest=manifest();
    journal.save_candidate(candidate.clone(), manifest.clone()).await.unwrap();
    assert_json_eq(&journal.candidate(&hash("a").as_str().unwrap()).await.unwrap(), &candidate);
    compare(&config, "saveCandidate", vec![candidate.clone(), manifest.clone()], journal.save_candidate(candidate.clone(), manifest.clone()).await.map(|_| IvyJson::Null));
    let mut changed=manifest.clone(); changed.insert("version", "2.0.0".into());
    compare(&config, "saveCandidate", vec![candidate.clone(), changed.clone()], journal.save_candidate(candidate.clone(), changed).await.map(|_| IvyJson::Null));
    let mut wrong=candidate.clone(); wrong.insert("candidateId", hash("b"));
    compare(&config, "saveCandidate", vec![wrong.clone(), manifest.clone()], journal.save_candidate(wrong, manifest.clone()).await.map(|_| IvyJson::Null));
    compare(&config, "candidateForBuild", vec!["fixture".into(), hash("a")], journal.candidate_for_build("fixture", &hash("a").as_str().unwrap()).await);
    let installed=IvyJson::object([("instanceId", "fixture".into()), ("candidateId", hash("a")), ("buildId", hash("a")), ("enabled", true.into()), ("installedAt", DATE.into())]);
    journal.set_installed(installed.clone()).await.unwrap(); assert_optional_json_eq(journal.installed("fixture").await.unwrap(), Some(installed.clone()));
    for key in ["buildId", "instanceId"] {
        let mut wrong=installed.clone(); wrong.insert(key, if key=="buildId" { hash("b") } else { "other".into() });
        compare(&config, "setInstalled", vec![wrong.clone()], journal.set_installed(wrong).await.map(|_| IvyJson::Null));
        assert_optional_json_eq(journal.installed("fixture").await.unwrap(), Some(installed.clone()));
    }
    let target=IvyJson::object([("schemaVersion", IvyJson::Number(1.0)), ("instanceId", "fixture".into()), ("revision", "launch".into()), ("candidateId", hash("a")), ("desired", "running".into()), ("configPath", "instance.json".into()), ("requestedAt", DATE.into())]);
    journal.restore_target(target.clone()).await.unwrap(); assert_optional_json_eq(journal.target("fixture").await.unwrap(), Some(target.clone()));
    compare(&config, "restoreTarget", vec![target.clone()], journal.restore_target(target.clone()).await.map(|_| IvyJson::Null));
    let mut missing=target; missing.insert("candidateId", hash("f"));
    compare(&config, "restoreTarget", vec![missing.clone()], journal.restore_target(missing).await.map(|_| IvyJson::Null));
    let db=sql(&config);
    db.execute("DELETE FROM candidates", []).unwrap();
    db.execute("INSERT INTO candidate_retirements VALUES (?,?)", params![hash("a").as_str().unwrap(), encode(&candidate)]).unwrap();
    compare(&config, "saveCandidate", vec![candidate.clone(), manifest.clone()], journal.save_candidate(candidate.clone(), manifest.clone()).await.map(|_| IvyJson::Null));
    assert_json_eq(&IvyJson::Array(journal.pending_candidate_retirements().await.unwrap()), &IvyJson::Array(vec![candidate.clone()]));
    compare(&config, "pendingCandidateRetirements", vec![], journal.pending_candidate_retirements().await.map(IvyJson::Array));
    journal.finish_candidate_retirement(&hash("a").as_str().unwrap()).await.unwrap(); assert!(journal.pending_candidate_retirements().await.unwrap().is_empty());
    journal.save_candidate(candidate, manifest).await.unwrap();
    compare(&config, "finishCandidateRetirement", vec![hash("a")], journal.finish_candidate_retirement(&hash("a").as_str().unwrap()).await.map(|_| IvyJson::Null));
    journal.close().await.unwrap();
}

#[tokio::test]
async fn journal_bootstrap_rolls_back_partial_installation_and_binds_configuration() {
    let fixture=ServiceFixture::new(); let config=host_configuration(&fixture); let journal=open(&config).await;
    let mut candidate=candidate(); candidate.insert("artifactRoot", fixture.0.join("artifact").to_string_lossy().to_string().into());
    journal.save_candidate(candidate.clone(), manifest()).await.unwrap();
    let source=fixture.0.join("state/bootstrap.json");
    let mut runtime=fixture.value(); runtime.insert("hostId", "fixture-host".into()); runtime.insert("instanceId", "fixture".into()); runtime.insert("componentId", "fixture".into());
    fs::write(&source, encode(&runtime)).unwrap();
    let process=IvyJson::object([("instanceId", "fixture".into()), ("componentId", "fixture".into()), ("name", "fixture".into()), ("candidateId", hash("a")), ("configPath", source.to_string_lossy().to_string().into())]);
    let mut plan=IvyJson::object([("schemaVersion", IvyJson::Number(1.0)), ("hostId", "fixture-host".into()), ("os", "win32".into()), ("installationId", "fixture-installation".into()), ("candidateId", hash("a")),
        ("artifactRoot", candidate.required("artifactRoot").unwrap().clone()), ("configPath", "host.json".into()), ("configHash", hash("b")), ("runtimeRoot", config.required("runtimeRoot").unwrap().clone()), ("nodeExecutable", "fixture-node".into()), ("processes", IvyJson::Array(vec![process.clone()]))]);
    validate_host("BootstrapPlan", &plan).unwrap();
    let mut wrong=process.clone(); wrong.insert("instanceId", "not-configured".into()); plan.insert("processes", IvyJson::Array(vec![process.clone(), wrong]));
    let failed=journal.sync_bootstrap(plan.clone(), false).await; assert_eq!(failed.as_ref().unwrap_err().code, "not_found");
    assert_eq!(journal.installed("fixture").await.unwrap(), None); assert_eq!(journal.target("fixture").await.unwrap(), None);
    compare(&config, "syncBootstrap", vec![plan.clone(), false.into()], failed.map(|_| IvyJson::Null));
    plan.insert("processes", IvyJson::Array(vec![process])); journal.sync_bootstrap(plan.clone(), false).await.unwrap();
    let target=journal.target("fixture").await.unwrap().unwrap();
    assert_eq!(target.required("revision").unwrap(), &IvyJson::from(ivy_host_runtime::journal::bootstrap_launch_id(&plan, "fixture").unwrap()));
    compare(&config, "syncBootstrap", vec![plan.clone(), false.into()], journal.sync_bootstrap(plan.clone(), false).await.map(|_| IvyJson::Null));
    compare(&config, "target", vec!["fixture".into()], journal.target("fixture").await.map(|v| v.unwrap()));
    journal.sync_bootstrap(plan.clone(), true).await.unwrap(); assert_eq!(journal.target("fixture").await.unwrap(), None);
    compare(&config, "syncBootstrap", vec![plan, true.into()], Ok(IvyJson::Null));
    journal.close().await.unwrap();
}
fn entry(id: &str, phase: &str, legacy: bool) -> IvyJson {
    let record = IvyJson::object([
        ("deploymentId", id.into()), ("hostId", "fixture-host".into()), ("instanceId", "fixture".into()), ("componentId", "fixture".into()),
        ("requestHash", hash("a")), ("previousBuild", IvyJson::Null), ("targetBuild", hash("b")), ("observedBuild", IvyJson::Null), ("phase", phase.into()),
        ("createdAt", DATE.into()), ("updatedAt", DATE.into()), ("readiness", IvyJson::object([("state", "not_run".into()), ("message", "".into())])),
    ]);
    validate_shared("DeploymentRecord", &record).unwrap();
    let mut value = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)), ("request", IvyJson::object([("action", "restart".into()), ("operationId", format!("op-{id}").into()), ("instanceId", "fixture".into())])),
        ("record", record), ("candidateId", IvyJson::Null), ("previousCandidateId", IvyJson::Null), ("configurationPath", "accepted.json".into()),
        ("activation", IvyJson::object([("previousTarget", IvyJson::Null), ("previousEnabled", true.into()), ("targetEnabled", true.into()), ("rollbackTargetRevision", IvyJson::Null)])),
    ]);
    validate_host("JournalEntry", &value).unwrap();
    if legacy { value.insert("configurationHash", hash("c")); }
    value
}

#[tokio::test]
async fn journal_reads_preserve_order_retirement_and_legacy_reconciliation() {
    let fixture = ServiceFixture::new(); let config = host_configuration(&fixture); let journal = open(&config).await;
    let db = sql(&config);
    for (id, phase, legacy) in [("a", "succeeded", false), ("b", "activating", true), ("c", "prepared", false)] {
        db.execute("INSERT INTO operations VALUES (?,?,?,?,?,?)", params![format!("op-{id}"), id, "hash", phase, DATE, encode(&entry(id, phase, legacy))]).unwrap();
    }
    db.execute("INSERT INTO retired_operations VALUES ('old-op','old-deployment','hash')", []).unwrap();
    let values = journal.list(200.0).await.unwrap(); assert_eq!(values.len(), 3);
    assert_eq!(values[1].required("record").unwrap().required("phase").unwrap(), &IvyJson::from("needs_attention"));
    for limit in [200.0, 0.0, 1.0, 2.0, 500.0, 1.5] { compare(&config, "list", vec![IvyJson::Number(limit)], journal.list(limit).await.map(IvyJson::Array)); }
    for id in ["a", "b", "absent", "old-deployment"] { compare(&config, "get", vec![id.into()], journal.get(id).await); }
    for id in ["op-a", "absent", "old-op"] { compare(&config, "operation", vec![id.into()], journal.operation(id).await.map(|v| v.unwrap_or(IvyJson::Null))); }
    let unfinished = journal.unfinished().await.unwrap(); assert_eq!(unfinished.len(), 1);
    compare(&config, "unfinished", vec![], Ok(IvyJson::Array(unfinished)));
    let status = journal.status(None).await.unwrap(); assert_eq!(status.required("unfinished").unwrap().as_array().unwrap().len(), 2);
    compare(&config, "status", vec![IvyJson::Null], Ok(status));
    journal.close().await.unwrap(); drop(db);
    let journal = open(&config).await; assert_eq!(journal.get("old-deployment").await.unwrap_err().code, "operation_expired"); journal.close().await.unwrap();
}

#[tokio::test]
async fn journal_sequences_are_atomic_persistent_and_validate_before_commit() {
    let fixture = ServiceFixture::new(); let config = host_configuration(&fixture); let journal = open(&config).await; let db = sql(&config);
    for (prior, minimum) in [("0", 0.0), ("40", 2.0), ("10", 50.0), (" 0x10 ", 0.0), ("invalid", 0.0), ("9007199254740991", 0.0), ("0", -1.0), ("0", 1.5)] {
        db.execute("INSERT INTO meta VALUES ('hostReportSequence',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [prior]).unwrap();
        let result = journal.management_snapshot(None, minimum, None).await;
        let mut oracle = input(&config, "managementSnapshot", vec![IvyJson::Null, IvyJson::Number(minimum)]); oracle.insert("sequence", prior.into());
        compare_config_oracle(oracle, result);
    }
    db.execute("UPDATE meta SET value='0' WHERE key='hostReportSequence'", []).unwrap();
    let mut wrong = journal.status(None).await.unwrap(); wrong.insert("hostId", "other".into());
    compare(&config, "managementSnapshot", vec![IvyJson::Null, IvyJson::Number(0.0), wrong.clone()], journal.management_snapshot(None, 0.0, Some(wrong)).await);
    let (a, b) = tokio::join!(journal.management_snapshot(None, 0.0, None), journal.management_snapshot(None, 0.0, None));
    let mut sequences = [a.unwrap().required("sequence").unwrap().as_f64().unwrap() as i64, b.unwrap().required("sequence").unwrap().as_f64().unwrap() as i64]; sequences.sort(); assert_eq!(sequences, [1, 2]);
    journal.close().await.unwrap(); drop(db);
    let journal = open(&config).await; assert_eq!(journal.management_snapshot(None, 0.0, None).await.unwrap().required("sequence").unwrap(), &IvyJson::Number(3.0)); journal.close().await.unwrap();
}

#[tokio::test]
async fn journal_configuration_reload_retains_identity_and_failed_transactions() {
    let fixture = ServiceFixture::new(); let config = host_configuration(&fixture); let journal = open(&config).await;
    let mut changed = config.clone(); changed.insert("hostId", "another-host".into());
    compare(&config, "useConfiguration", vec![changed.clone()], journal.use_configuration(changed).await.map(|_| IvyJson::Null));
    assert_eq!(journal.config().await.unwrap(), config);
    let mut removed = config.clone(); removed.insert("instances", IvyJson::Array(vec![])); journal.use_configuration(removed.clone()).await.unwrap();
    let mut new = config.clone(); let mut instances = config.required("instances").unwrap().as_array().unwrap().to_vec(); instances[0].insert("componentId", "different".into()); new.insert("instances", IvyJson::Array(instances));
    assert_eq!(journal.use_configuration(new.clone()).await.unwrap_err().code, "target_conflict");
    compare(&removed, "useConfiguration", vec![new.clone()], journal.use_configuration(new.clone()).await.map(|_| IvyJson::Null));
    assert_eq!(journal.config().await.unwrap(), removed); journal.close().await.unwrap();
    assert_eq!(HostJournal::open(new).await.err().unwrap().code, "target_conflict");
    let journal = open(&config).await; journal.close().await.unwrap();
    sql(&config).execute_batch("PRAGMA user_version=2").unwrap();
    let error = HostJournal::open(config.clone()).await.err().unwrap(); assert_eq!(error.code, "unsupported_storage");
    compare(&config, "config", vec![], Err(error));
}

fn health() -> IvyJson {
    let value = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)), ("instanceId", "fixture".into()), ("componentId", "fixture".into()), ("buildId", hash("a")),
        ("pid", IvyJson::Number(123.0)), ("bootId", "boot".into()), ("startedAt", DATE.into()), ("observedAt", DATE.into()), ("ready", true.into()), ("generation", IvyJson::Null),
        ("launchId", "launch".into()), ("details", "😀".repeat(400).into()),
    ]); validate_host("Health", &value).unwrap(); value
}
fn executor() -> IvyJson {
    let value = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)), ("hostId", "fixture-host".into()), ("pid", IvyJson::Number(123.0)), ("bootId", "boot".into()), ("buildId", hash("a")),
        ("observedAt", DATE.into()), ("state", "ready".into()), ("activeDeploymentId", IvyJson::Null), ("code", IvyJson::Null),
    ]); validate_host("ExecutorStatus", &value).unwrap(); value
}
#[tokio::test]
async fn journal_target_and_build_mismatches_never_preserve_readiness() {
    let fixture = ServiceFixture::new(); let config = host_configuration(&fixture); let journal = open(&config).await; let db = sql(&config);
    let data = std::path::PathBuf::from(service_paths(&config, &"fixture".into()).unwrap().required("data").unwrap().as_str().unwrap()); fs::create_dir_all(&data).unwrap();
    fs::write(data.join("health.json"), encode(&health())).unwrap();
    let mut candidate = IvyJson::object([
        ("candidateId", hash("b")), ("componentId", "fixture".into()), ("artifactRoot", "artifact".into()), ("manifestPath", "manifest.json".into()), ("createdAt", DATE.into()),
        ("platform", IvyJson::object([("os", "win32".into()), ("arch", "x64".into()), ("node", "24.18.0".into())])), ("buildId", hash("a")),
    ]);
    validate_host("Candidate", &candidate).unwrap(); candidate.insert("sourceRoot", "legacy-source".into());
    db.execute("INSERT INTO candidates VALUES (?,?,?,?,?)", params![hash("b").as_str().unwrap(), "fixture", hash("a").as_str().unwrap(), encode(&candidate), "{}"]).unwrap();
    compare(&config, "candidate", vec![hash("b")], journal.candidate(&hash("b").as_str().unwrap()).await);
    for (desired, revision, build, expected) in [("running", "launch", "a", "ready"), ("running", "wrong", "a", "unknown"), ("running", "launch", "c", "unknown"), ("stopped", "launch", "a", "stopped")] {
        candidate.insert("buildId", hash(build)); db.execute("UPDATE candidates SET candidate_json=?", [encode(&candidate)]).unwrap();
        let mut target = IvyJson::object([("schemaVersion", IvyJson::Number(1.0)), ("instanceId", "fixture".into()), ("revision", revision.into()), ("candidateId", hash("b")), ("desired", desired.into()), ("configPath", "instance.json".into()), ("requestedAt", DATE.into())]);
        validate_host("RuntimeTarget", &target).unwrap(); target.insert("configHash", hash("d"));
        db.execute("INSERT INTO runtime_targets VALUES ('fixture',?) ON CONFLICT(instance_id) DO UPDATE SET value=excluded.value", [encode(&target)]).unwrap();
        compare(&config, "target", vec!["fixture".into()], journal.target("fixture").await.map(|v| v.unwrap()));
        let observed = journal.current_observations(None).await.unwrap(); assert_eq!(observed.required("fixture").unwrap().required("state").unwrap(), &IvyJson::from(expected));
        compare(&config, "currentObservations", vec![IvyJson::Null], Ok(observed));
    }
    journal.close().await.unwrap();
}
#[tokio::test]
async fn journal_observes_current_health_and_configured_executor_identity() {
    let fixture = ServiceFixture::new(); let mut config = host_configuration(&fixture);
    let mut instances = config.required("instances").unwrap().as_array().unwrap().to_vec(); let mut renamed = instances[0].clone();
    renamed.insert("instanceId", "renamed-executor".into()); renamed.insert("serviceNodeId", "host.runtime".into()); renamed.insert("componentId", "host-executor".into()); instances.push(renamed); config.insert("instances", IvyJson::Array(instances));
    let journal = open(&config).await; let db = sql(&config);
    let data = std::path::PathBuf::from(service_paths(&config, &"fixture".into()).unwrap().required("data").unwrap().as_str().unwrap()); fs::create_dir_all(&data).unwrap();
    for (mode, health) in [("ready", health()), ("stale", { let mut v=health(); v.insert("observedAt", "2025-12-31T23:59:45.000Z".into()); v }), ("future", { let mut v=health(); v.insert("observedAt", "2026-01-01T00:00:01.001Z".into()); v }), ("identity", { let mut v=health(); v.insert("instanceId", "other".into()); v }), ("starting", { let mut v=health(); v.insert("ready", false.into()); v })] {
        fs::write(data.join("health.json"), encode(&health)).unwrap();
        let result = journal.status(Some(executor())).await.unwrap();
        let rows = result.required("instances").unwrap().as_array().unwrap();
        assert_eq!(rows[1].required("observedState").unwrap(), &IvyJson::from("ready"), "{mode}");
        if mode == "ready" { assert_eq!(rows[0].required("observedState").unwrap(), &IvyJson::from("ready")); assert_eq!(rows[0].required("message").unwrap().as_js_string().unwrap().0.len(), 512); }
        compare(&config, "status", vec![executor()], Ok(result));
    }
    fs::write(data.join("health.json"), encode(&health())).unwrap();
    let observations = journal.current_observations(Some(executor())).await.unwrap();
    for (key, value) in observations.as_object().unwrap() { db.execute("INSERT INTO observations VALUES (?,?)", params![key.to_utf8().unwrap(), encode(value)]).unwrap(); }
    fs::write(data.join("health.json"), "{").unwrap();
    for exec in [IvyJson::Null, executor(), { let mut v=executor(); v.insert("observedAt", "invalid".into()); v }, { let mut v=executor(); v.insert("buildId", IvyJson::Null); v }] {
        let option = (!exec.is_null()).then_some(exec.clone());
        let result = journal.current_observations(option.clone()).await.unwrap();
        assert_eq!(result.required("fixture").unwrap().required("state").unwrap(), &IvyJson::from("unknown"));
        compare(&config, "currentObservations", vec![exec.clone()], Ok(result)); compare(&config, "status", vec![exec], journal.status(option).await);
    }
    journal.close().await.unwrap();
}
