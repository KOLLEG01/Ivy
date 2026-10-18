use super::{ServiceFixture, host_configuration, compare_config_oracle};
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
    assert_eq!(journal.observations().await.unwrap().required("fixture").unwrap(), &observation);
    compare(&config, "recordObservation", vec!["other".into(), observation.clone()], journal.record_observation("other", observation.clone()).await.map(|_| IvyJson::Null));
    assert_eq!(journal.observations().await.unwrap().required("fixture").unwrap(), &observation);
    let retention=IvyJson::object([("schemaVersion", IvyJson::Number(1.0)), ("hostId", "fixture-host".into()), ("state", "succeeded".into()), ("trigger", "manual".into()), ("startedAt", DATE.into()), ("completedAt", DATE.into()), ("lastSuccessAt", DATE.into()),
        ("removed", IvyJson::object(["candidates", "preparations", "payloads", "snapshots", "maintenance", "bootstrapPlans"].map(|key| (key, IvyJson::Number(0.0))))),
        ("availableBytes", IvyJson::object(["runtime", "artifacts", "staging"].map(|key| (key, IvyJson::Number(1024.0))))), ("lowSpace", false.into()), ("skipped", IvyJson::Array(vec![])), ("errorCode", IvyJson::Null)]);
    journal.save_storage_retention_status(retention.clone()).await.unwrap(); assert_eq!(journal.storage_retention_status().await.unwrap(), Some(retention.clone()));
    compare(&config, "storageRetentionStatus", vec![], journal.storage_retention_status().await.map(|v| v.unwrap()));
    let mut invalid=retention.clone(); invalid.insert("state", "invalid".into());
    compare(&config, "saveStorageRetentionStatus", vec![invalid.clone()], journal.save_storage_retention_status(invalid).await.map(|_| IvyJson::Null));
    assert_eq!(journal.storage_retention_status().await.unwrap(), Some(retention)); journal.close().await.unwrap();
}

#[tokio::test]
async fn journal_adoption_is_idempotent_persistent_and_compare_before_delete() {
    let fixture=ServiceFixture::new(); let config=host_configuration(&fixture); let journal=open(&config).await; let value=adoption(&config);
    journal.stage_configuration_adoption(value.clone()).await.unwrap();
    assert_eq!(journal.configuration_adoption().await.unwrap(), Some(value.clone()));
    compare(&config, "stageConfigurationAdoption", vec![value.clone()], journal.stage_configuration_adoption(value.clone()).await.map(|_| IvyJson::Null));
    let mut conflict=value.clone(); conflict.insert("revision", IvyJson::Number(2.0));
    compare(&config, "stageConfigurationAdoption", vec![conflict.clone()], journal.stage_configuration_adoption(conflict.clone()).await.map(|_| IvyJson::Null));
    compare(&config, "finishConfigurationAdoption", vec![conflict.clone()], journal.finish_configuration_adoption(conflict).await.map(|_| IvyJson::Null));
    assert_eq!(journal.configuration_adoption().await.unwrap(), Some(value.clone()));
    journal.close().await.unwrap(); let journal=open(&config).await;
    compare(&config, "configurationAdoption", vec![], journal.configuration_adoption().await.map(|v| v.unwrap()));
    journal.finish_configuration_adoption(value.clone()).await.unwrap(); assert_eq!(journal.configuration_adoption().await.unwrap(), None);
    let mut oracle=input(&config, "finishConfigurationAdoption", vec![value.clone()]);
    oracle.insert("seed", IvyJson::Array(vec![IvyJson::object([("sql", "INSERT INTO meta VALUES ('configurationAdoption',?)".into()), ("params", IvyJson::Array(vec![encode(&value).into()]))])]));
    compare_config_oracle(oracle, Ok(IvyJson::Null));
    compare(&config, "finishConfigurationAdoption", vec![value.clone()], journal.finish_configuration_adoption(value.clone()).await.map(|_| IvyJson::Null));
    for revision in [0.0, 1.5, 9_007_199_254_740_992.0] {
        let mut invalid=value.clone(); invalid.insert("revision", IvyJson::Number(revision));
        journal.stage_configuration_adoption(invalid.clone()).await.unwrap();
        assert_eq!(journal.configuration_adoption().await.unwrap_err().code, "configuration_changed");
        compare(&config, "configurationAdoption", vec![], journal.configuration_adoption().await.map(|v| v.unwrap()));
        journal.finish_configuration_adoption(invalid).await.unwrap();
    }
    let mut wrong=value; wrong.insert("contentHash", hash("f"));
    compare(&config, "stageConfigurationAdoption", vec![wrong.clone()], journal.stage_configuration_adoption(wrong).await.map(|_| IvyJson::Null));
    journal.close().await.unwrap();
}

#[tokio::test]
async fn journal_candidate_publication_and_installed_target_writes_enforce_identity() {
    let fixture=ServiceFixture::new(); let config=host_configuration(&fixture); let journal=open(&config).await; let candidate=candidate(); let manifest=manifest();
    journal.save_candidate(candidate.clone(), manifest.clone()).await.unwrap();
    assert_eq!(journal.candidate(&hash("a").as_str().unwrap()).await.unwrap(), candidate);
    compare(&config, "saveCandidate", vec![candidate.clone(), manifest.clone()], journal.save_candidate(candidate.clone(), manifest.clone()).await.map(|_| IvyJson::Null));
    let mut changed=manifest.clone(); changed.insert("version", "2.0.0".into());
    compare(&config, "saveCandidate", vec![candidate.clone(), changed.clone()], journal.save_candidate(candidate.clone(), changed).await.map(|_| IvyJson::Null));
    let mut wrong=candidate.clone(); wrong.insert("candidateId", hash("b"));
    compare(&config, "saveCandidate", vec![wrong.clone(), manifest.clone()], journal.save_candidate(wrong, manifest.clone()).await.map(|_| IvyJson::Null));
    compare(&config, "candidateForBuild", vec!["fixture".into(), hash("a")], journal.candidate_for_build("fixture", &hash("a").as_str().unwrap()).await);
    let installed=IvyJson::object([("instanceId", "fixture".into()), ("candidateId", hash("a")), ("buildId", hash("a")), ("enabled", true.into()), ("installedAt", DATE.into())]);
    journal.set_installed(installed.clone()).await.unwrap(); assert_eq!(journal.installed("fixture").await.unwrap(), Some(installed.clone()));
    for key in ["buildId", "instanceId"] {
        let mut wrong=installed.clone(); wrong.insert(key, if key=="buildId" { hash("b") } else { "other".into() });
        compare(&config, "setInstalled", vec![wrong.clone()], journal.set_installed(wrong).await.map(|_| IvyJson::Null));
        assert_eq!(journal.installed("fixture").await.unwrap(), Some(installed.clone()));
    }
    let target=IvyJson::object([("schemaVersion", IvyJson::Number(1.0)), ("instanceId", "fixture".into()), ("revision", "launch".into()), ("candidateId", hash("a")), ("desired", "running".into()), ("configPath", "instance.json".into()), ("requestedAt", DATE.into())]);
    journal.restore_target(target.clone()).await.unwrap(); assert_eq!(journal.target("fixture").await.unwrap(), Some(target.clone()));
    compare(&config, "restoreTarget", vec![target.clone()], journal.restore_target(target.clone()).await.map(|_| IvyJson::Null));
    let mut missing=target; missing.insert("candidateId", hash("f"));
    compare(&config, "restoreTarget", vec![missing.clone()], journal.restore_target(missing).await.map(|_| IvyJson::Null));
    let db=sql(&config);
    db.execute("DELETE FROM candidates", []).unwrap();
    db.execute("INSERT INTO candidate_retirements VALUES (?,?)", params![hash("a").as_str().unwrap(), encode(&candidate)]).unwrap();
    compare(&config, "saveCandidate", vec![candidate.clone(), manifest.clone()], journal.save_candidate(candidate.clone(), manifest.clone()).await.map(|_| IvyJson::Null));
    assert_eq!(journal.pending_candidate_retirements().await.unwrap(), vec![candidate.clone()]);
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
