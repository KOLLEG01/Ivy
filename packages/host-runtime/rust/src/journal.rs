//! Host-local journal and current-state projections. SQLite and filesystem work
//! is serialized on blocking workers, including configuration reload.
use crate::{IvyError, IvyJson, IvyResult, accepted_configuration::{installation_record, read_accepted_configuration, same_installation}, layout::{path, path_json, service_paths}};
use ivy_contracts::{JsString, core_schema::{validate_host, validate_shared}, hash::hash_json, json};
use ivy_sdk::clock::{Clock, SystemClock, parse_date};
use ivy_store::{rusqlite::{Connection, OptionalExtension, Params, params}, sqlite};
use std::{collections::{BTreeMap, BTreeSet}, fs, path::PathBuf, sync::{Arc, Mutex}, time::Duration};

const TERMINAL: [&str; 4] = ["succeeded", "rolled_back", "failed", "needs_attention"];
const SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
const SCHEMA: &str = "BEGIN IMMEDIATE;
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS operations(operation_id TEXT PRIMARY KEY,deployment_id TEXT NOT NULL UNIQUE,request_hash TEXT NOT NULL,phase TEXT NOT NULL,created_at TEXT NOT NULL,entry_json TEXT NOT NULL) STRICT;
CREATE INDEX IF NOT EXISTS operations_by_phase ON operations(phase,created_at,deployment_id);
CREATE TABLE IF NOT EXISTS retired_operations(operation_id TEXT PRIMARY KEY,deployment_id TEXT NOT NULL UNIQUE,request_hash TEXT NOT NULL) STRICT;
CREATE TRIGGER IF NOT EXISTS retired_operation_insert BEFORE INSERT ON operations
WHEN EXISTS(SELECT 1 FROM retired_operations WHERE operation_id=NEW.operation_id)
BEGIN SELECT RAISE(ABORT,'operation_expired'); END;
CREATE TABLE IF NOT EXISTS candidates(candidate_id TEXT PRIMARY KEY,component_id TEXT NOT NULL,build_id TEXT NOT NULL,candidate_json TEXT NOT NULL,manifest_json TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS candidate_retirements(candidate_id TEXT PRIMARY KEY,candidate_json TEXT NOT NULL) STRICT;
CREATE INDEX IF NOT EXISTS candidates_by_build ON candidates(component_id,build_id);
CREATE TABLE IF NOT EXISTS installed(instance_id TEXT PRIMARY KEY,value TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS observations(instance_id TEXT PRIMARY KEY,value TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS runtime_targets(instance_id TEXT PRIMARY KEY,value TEXT NOT NULL) STRICT;
PRAGMA user_version=1; COMMIT;";

fn internal<T>(result: Result<T, impl std::fmt::Debug>) -> IvyResult<T> { result.map_err(|_| IvyError::internal()) }
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> { if ok { Ok(()) } else { Err(IvyError::new(code, message)) } }
fn decoded(text: &str) -> IvyResult<IvyJson> { internal(json::parse(text, usize::MAX)) }
fn encoded(value: &IvyJson) -> IvyResult<String> { json::canonical(value, ivy_contracts::limits::MANAGEMENT_FRAME_BYTES) }
fn field<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson { static NULL: IvyJson = IvyJson::Null; value.get(key).unwrap_or(&NULL) }
fn non_null(value: &IvyJson) -> Option<&IvyJson> { (!value.is_null()).then_some(value) }
fn truthy(value: &IvyJson) -> bool { match value { IvyJson::Null => false, IvyJson::Bool(v) => *v, IvyJson::Number(v) => *v != 0.0 && !v.is_nan(), IvyJson::String(v) => !v.0.is_empty(), _ => true } }
fn fallback(value: &IvyJson, other: &IvyJson) -> IvyJson { non_null(value).unwrap_or(other).clone() }
fn project(value: &IvyJson, keys: &[&str]) -> IvyJson {
    IvyJson::object(keys.iter().filter_map(|key| value.get(key).map(|v| (*key, v.clone()))))
}
fn legacy_path(config: &IvyJson, hash: &IvyJson) -> IvyResult<PathBuf> {
    let hash = json::try_js_string(hash)?.to_utf8()?;
    require(hash.len() == 71 && hash.starts_with("sha256:") && hash.as_bytes()[7..].iter().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b)), "configuration_changed", "Retained configuration identity is invalid.")?;
    Ok(path(config.required("runtimeRoot")?)?.join("host-configurations").join(format!("{}.json", &hash[7..])))
}
fn fresh(value: &IvyJson, clock: &dyn Clock) -> bool {
    let Some(value) = value.as_js_string() else { return false };
    let at = parse_date(value);
    at.is_finite() && clock.utc_millis() as f64 - at < 15_000.0 && at <= clock.utc_millis() as f64 + 1000.0
}
pub fn fresh_executor_status(value: &IvyJson, clock: &dyn Clock) -> bool { fresh(field(value, "observedAt"), clock) }
pub fn requires_bootstrap_replacement(instance: &IvyJson) -> bool {
    field(instance, "componentId") == &IvyJson::from("host-executor") || cfg!(windows) && field(instance, "componentId") == &IvyJson::from("service-manager")
}
pub fn bootstrap_launch_id(plan: &IvyJson, instance_id: &str) -> IvyResult<String> {
    hash_json(&IvyJson::object([("planHash", hash_json(plan)?.into()), ("instanceId", instance_id.into())]))
}

pub use crate::executor_lock::ExecutorLock;

#[derive(Clone, Copy)]
pub struct HistoryLimits { pub count: usize, pub bytes: usize }
impl Default for HistoryLimits { fn default() -> Self { Self { count: 10_000, bytes: 64 * 1024 * 1024 } } }
struct State { db: Connection, config: IvyJson, clock: Arc<dyn Clock>, history_limits: HistoryLimits }
/// Clones share one connection and configuration. Call `close` before releasing
/// the final handle when shutdown must wait for all journal work to finish.
#[derive(Clone)]
pub struct HostJournal { state: Arc<Mutex<Option<State>>> }
impl HostJournal {
    pub async fn open(config: IvyJson) -> IvyResult<Self> { Self::open_with_clock(config, Arc::new(SystemClock)).await }
    pub async fn open_with_clock(config: IvyJson, clock: Arc<dyn Clock>) -> IvyResult<Self> {
        Self::open_with_options(config, HistoryLimits::default(), clock).await
    }
    pub async fn open_with_options(config: IvyJson, history_limits: HistoryLimits, clock: Arc<dyn Clock>) -> IvyResult<Self> {
        let state = internal(tokio::task::spawn_blocking(move || State::open(config, clock, history_limits)).await)??;
        Ok(Self { state: Arc::new(Mutex::new(Some(state))) })
    }
    async fn with<T: Send + 'static>(&self, work: impl FnOnce(&mut State) -> IvyResult<T> + Send + 'static) -> IvyResult<T> {
        let state = self.state.clone();
        internal(tokio::task::spawn_blocking(move || {
            let mut state = internal(state.lock())?;
            work(state.as_mut().ok_or_else(IvyError::internal)?)
        }).await)?
    }
    pub async fn close(&self) -> IvyResult<()> {
        let state = self.state.clone();
        internal(tokio::task::spawn_blocking(move || { internal(state.lock())?.take(); Ok(()) }).await)?
    }
    pub async fn config(&self) -> IvyResult<IvyJson> { self.with(|s| Ok(s.config.clone())).await }
    pub async fn accepted_configuration(&self, source: PathBuf) -> IvyResult<IvyJson> { self.with(move |s| read_accepted_configuration(&s.config, &source)).await }
    pub async fn use_configuration(&self, config: IvyJson) -> IvyResult<()> {
        self.with(move |s| { same_installation(&s.config, &config)?; s.transaction(|s| s.identities(&config))?; s.config = config; Ok(()) }).await
    }
    pub async fn configuration_adoption(&self) -> IvyResult<Option<IvyJson>> {
        self.with(|s| s.configuration_adoption()).await
    }
    pub async fn stage_configuration_adoption(&self, value: IvyJson) -> IvyResult<()> {
        self.with(move |s| {
            let config = value.required("configuration")?;
            validate_host("HostConfig", config)?;
            same_installation(&s.config, config)?;
            require(value.get("contentHash") == Some(&IvyJson::from(hash_json(config)?)), "configuration_changed", "Configuration adoption hash does not match its exact input.")?;
            s.transaction(|s| {
                let prior = s.text("SELECT value FROM meta WHERE key=?", "configurationAdoption")?;
                let value = encoded(&value)?;
                require(prior.is_none_or(|prior| prior == value), "service_busy", "Another configuration adoption is still pending.")?;
                s.save_meta("configurationAdoption", &value)
            })
        }).await
    }
    pub async fn finish_configuration_adoption(&self, value: IvyJson) -> IvyResult<()> {
        self.with(move |s| s.transaction(|s| {
            let prior = s.text("SELECT value FROM meta WHERE key=?", "configurationAdoption")?;
            // Preserve the original short circuit: absent state is a revision conflict.
            require(match prior { Some(prior) => prior == encoded(&value)?, None => false }, "revision_conflict", "Configuration adoption changed before completion.")?;
            internal(s.db.execute("DELETE FROM meta WHERE key=?", ["configurationAdoption"]))?;
            Ok(())
        })).await
    }
    pub async fn instance(&self, instance_id: &str) -> IvyResult<IvyJson> {
        let id = instance_id.to_owned(); self.with(move |s| s.instance(&id).cloned()).await
    }
    pub async fn get(&self, deployment_id: &str) -> IvyResult<IvyJson> {
        let id = deployment_id.to_owned(); self.with(move |s| s.lookup(&id, false)?.ok_or_else(|| IvyError::new("not_found", "Host-local deployment was not found."))).await
    }
    pub async fn operation(&self, operation_id: &str) -> IvyResult<Option<IvyJson>> {
        let id = operation_id.to_owned(); self.with(move |s| s.lookup(&id, true)).await
    }
    pub async fn list(&self, limit: f64) -> IvyResult<Vec<IvyJson>> {
        self.with(move |s| s.entries("SELECT entry_json FROM operations ORDER BY created_at DESC,deployment_id DESC LIMIT ?", [limit.clamp(1.0, 200.0)])).await
    }
    pub async fn unfinished(&self) -> IvyResult<Vec<IvyJson>> {
        self.with(|s| s.entries("SELECT entry_json FROM operations WHERE phase NOT IN ('succeeded','rolled_back','failed','needs_attention') AND json_extract(entry_json,'$.configurationHash') IS NULL ORDER BY created_at,deployment_id LIMIT 200", [])).await
    }
    pub async fn installed(&self, instance_id: &str) -> IvyResult<Option<IvyJson>> {
        let id = instance_id.to_owned(); self.with(move |s| s.value("SELECT value FROM installed WHERE instance_id=?", &id)).await
    }
    pub async fn set_installed(&self, value: IvyJson) -> IvyResult<()> { self.with(move |s| s.set_installed(&value)).await }
    pub async fn target(&self, instance_id: &str) -> IvyResult<Option<IvyJson>> {
        let id = instance_id.to_owned(); self.with(move |s| s.target(&id)).await
    }
    pub async fn restore_target(&self, value: IvyJson) -> IvyResult<()> {
        self.with(move |s| {
            validate_host("RuntimeTarget", &value)?;
            s.instance(&value.required("instanceId")?.as_str()?)?;
            s.candidate(&value.required("candidateId")?.as_str()?)?;
            s.save_target(&value)
        }).await
    }
    pub async fn sync_bootstrap(&self, plan: IvyJson, clear_targets: bool) -> IvyResult<()> {
        self.with(move |s| s.sync_bootstrap(&plan, clear_targets)).await
    }
    pub async fn candidate(&self, candidate_id: &str) -> IvyResult<IvyJson> {
        let id = candidate_id.to_owned(); self.with(move |s| s.candidate(&id)).await
    }
    pub async fn manifest(&self, candidate_id: &str) -> IvyResult<IvyJson> {
        let id = candidate_id.to_owned(); self.with(move |s| {
            let value = s.value("SELECT manifest_json FROM candidates WHERE candidate_id=?", &id)?.ok_or_else(|| IvyError::new("not_found", "Prepared candidate manifest was not found."))?;
            crate::artifact::release_manifest(&value)
        }).await
    }
    pub async fn retention_candidate_ids(&self) -> IvyResult<BTreeSet<String>> { self.with(|s| s.retention_candidate_ids()).await }
    pub async fn retire_candidate(&self, candidate_id: &str) -> IvyResult<bool> {
        let id = candidate_id.to_owned(); self.with(move |s| s.transaction(|s| {
            let Some(value) = s.text("SELECT candidate_json FROM candidates WHERE candidate_id=?", &id)? else { return Ok(false) };
            if s.retention_candidate_ids()?.contains(&id) { return Ok(false) }
            internal(s.db.execute("INSERT INTO candidate_retirements VALUES (?,?)", params![id, value]))?;
            internal(s.db.execute("DELETE FROM candidates WHERE candidate_id=?", [&id]))?;
            Ok(true)
        })).await
    }
    pub async fn accept(&self, request: IvyJson, source_snapshot: Option<IvyJson>) -> IvyResult<IvyJson> {
        self.with(move |s| s.accept(&request, source_snapshot.as_ref())).await
    }
    pub async fn advance(&self, deployment_id: &str, expected_phase: &str, phase: &str, change: IvyJson) -> IvyResult<IvyJson> {
        let id = deployment_id.to_owned(); let expected = expected_phase.to_owned(); let phase = phase.to_owned();
        self.with(move |s| s.advance(&id, &expected, &phase, &change)).await
    }
    pub async fn save_candidate(&self, candidate: IvyJson, manifest: IvyJson) -> IvyResult<()> {
        self.with(move |s| {
            validate_host("Candidate", &candidate)?;
            validate_host("ReleaseManifest", &manifest)?;
            require(manifest.get("buildId") == candidate.get("buildId") && manifest.get("componentId") == candidate.get("componentId") && candidate.get("candidateId") == candidate.get("buildId"), "build_mismatch", "Candidate identity must match its release manifest.")?;
            s.transaction(|s| {
                let id = candidate.required("candidateId")?.as_str()?;
                require(s.text("SELECT candidate_id FROM candidate_retirements WHERE candidate_id=?", &id)?.is_none(), "mutation_conflict", "Candidate deletion is pending; retry publication after storage collection.")?;
                let prior = s.text("SELECT candidate_json FROM candidates WHERE candidate_id=?", &id)?;
                if let Some(prior) = prior {
                    require(prior == encoded(&candidate)? && s.text("SELECT manifest_json FROM candidates WHERE candidate_id=?", &id)?.as_ref() == Some(&encoded(&manifest)?), "mutation_conflict", "Candidate is immutable.")?;
                } else {
                    internal(s.db.execute("INSERT INTO candidates VALUES (?,?,?,?,?)", params![id, candidate.required("componentId")?.as_str()?, candidate.required("buildId")?.as_str()?, encoded(&candidate)?, encoded(&manifest)?]))?;
                }
                Ok(())
            })
        }).await
    }
    pub async fn candidate_for_build(&self, component_id: &str, build_id: &str) -> IvyResult<IvyJson> {
        let component = component_id.to_owned(); let build = build_id.to_owned();
        self.with(move |s| s.candidate_for_build(&component, &build)).await
    }
    pub async fn pending_candidate_retirements(&self) -> IvyResult<Vec<IvyJson>> {
        self.with(|s| {
            let mut statement = internal(s.db.prepare("SELECT candidate_json FROM candidate_retirements ORDER BY candidate_id"))?;
            let rows = internal(statement.query_map([], |r| r.get::<_, String>(0)))?;
            rows.map(|r| normalize_candidate(&decoded(&internal(r)?)?)).collect()
        }).await
    }
    pub async fn finish_candidate_retirement(&self, candidate_id: &str) -> IvyResult<()> {
        let id = candidate_id.to_owned();
        self.with(move |s| s.transaction(|s| {
            require(s.text("SELECT candidate_id FROM candidates WHERE candidate_id=?", &id)?.is_none(), "mutation_conflict", "A retiring candidate cannot be registered.")?;
            internal(s.db.execute("DELETE FROM candidate_retirements WHERE candidate_id=?", [id]))?;
            Ok(())
        })).await
    }
    pub async fn observations(&self) -> IvyResult<IvyJson> { self.with(|s| s.observations()).await }
    pub async fn record_observation(&self, instance_id: &str, value: IvyJson) -> IvyResult<()> {
        let id = instance_id.to_owned();
        self.with(move |s| {
            validate_host("RuntimeObservation", &value)?;
            require(value.get("instanceId") == Some(&IvyJson::from(id.clone())), "target_conflict", "Observation must belong to its instance.")?;
            s.instance(&id)?;
            internal(s.db.execute("INSERT INTO observations VALUES (?,?) ON CONFLICT(instance_id) DO UPDATE SET value=excluded.value", params![id, json::canonical(&value, 65536)?]))?;
            Ok(())
        }).await
    }
    pub async fn storage_retention_status(&self) -> IvyResult<Option<IvyJson>> { self.with(|s| s.storage_retention_status()).await }
    pub async fn save_storage_retention_status(&self, value: IvyJson) -> IvyResult<()> {
        self.with(move |s| { validate_host("StorageRetentionStatus", &value)?; s.save_meta("storage-retention", &encoded(&value)?) }).await
    }
    pub async fn current_observations(&self, executor: Option<IvyJson>) -> IvyResult<IvyJson> { self.with(move |s| s.current_observations(executor.as_ref())).await }
    pub async fn status(&self, executor: Option<IvyJson>) -> IvyResult<IvyJson> { self.with(move |s| s.status(executor.as_ref())).await }
    pub async fn management_snapshot(&self, executor: Option<IvyJson>, minimum_sequence: f64, status: Option<IvyJson>) -> IvyResult<IvyJson> {
        self.with(move |s| {
            let status = match status { Some(v) => v, None => s.status(executor.as_ref())? };
            require(minimum_sequence.is_finite() && minimum_sequence.fract() == 0.0 && (0.0..=SAFE_INTEGER).contains(&minimum_sequence), "invalid_arguments", "Invalid host observation sequence.")?;
            require(status.get("hostId") == s.config.get("hostId"), "target_conflict", "Management status belongs to another host.")?;
            s.transaction(|s| {
                let prior = s.text("SELECT value FROM meta WHERE key=?", "hostReportSequence")?.unwrap_or_else(|| "0".into());
                let prior = json::number_from_string(&prior.into());
                let sequence = if prior.is_nan() { f64::NAN } else { prior.max(minimum_sequence) + 1.0 };
                require(sequence.is_finite() && sequence.fract() == 0.0 && sequence.abs() <= SAFE_INTEGER, "limit_exceeded", "Host observation sequence is exhausted.")?;
                let snapshot = IvyJson::object([("sequence", IvyJson::Number(sequence)), ("status", status)]);
                validate_host("ManagementSnapshot", &snapshot)?;
                internal(s.db.execute("INSERT INTO meta VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params!["hostReportSequence", sequence.to_string()]))?;
                Ok(snapshot)
            })
        }).await
    }
}
impl State {
    fn now(&self) -> IvyResult<String> {
        Ok(chrono::DateTime::from_timestamp_millis(self.clock.utc_millis()).ok_or_else(IvyError::internal)?.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
    }
    fn save_meta(&self, key: &str, value: &str) -> IvyResult<()> {
        internal(self.db.execute("INSERT INTO meta VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key, value]))?; Ok(())
    }
    fn configuration_adoption(&self) -> IvyResult<Option<IvyJson>> {
        let Some(value) = self.value("SELECT value FROM meta WHERE key=?", "configurationAdoption")? else { return Ok(None) };
        let config = value.required("configuration")?;
        validate_host("HostConfig", config)?;
        same_installation(&self.config, config)?;
        let revision = field(&value, "revision").as_f64().unwrap_or(f64::NAN);
        let valid_hash = field(&value, "contentHash").as_js_string().is_some_and(|v| v.0.len() == 71 && v.0[..7] == "sha256:".encode_utf16().collect::<Vec<_>>() && v.0[7..].iter().all(|v| (48..=57).contains(v) || (97..=102).contains(v)));
        require(revision.fract() == 0.0 && (1.0..=SAFE_INTEGER).contains(&revision) && valid_hash &&
            field(&value, "actions").as_array().is_some_and(|actions| actions.iter().all(|a|
                ["restart", "enable", "disable"].iter().any(|v| field(a, "action") == &IvyJson::from(*v)) && field(a, "instanceId").as_js_string().is_some() && field(a, "operationId").as_js_string().is_some())) &&
            field(&value, "bootstrapRequired").as_array().is_some_and(|v| v.iter().all(|v| v.as_js_string().is_some())),
            "configuration_changed", "Retained configuration adoption is invalid.")?;
        Ok(Some(value))
    }
    fn storage_retention_status(&self) -> IvyResult<Option<IvyJson>> {
        let value = self.value("SELECT value FROM meta WHERE key=?", "storage-retention")?;
        if let Some(v) = &value { validate_host("StorageRetentionStatus", v)?; } Ok(value)
    }
    fn set_installed(&self, value: &IvyJson) -> IvyResult<()> {
        let id = value.required("instanceId")?.as_str()?;
        self.instance(&id)?;
        let candidate = self.candidate(&value.required("candidateId")?.as_str()?)?;
        require(candidate.get("buildId") == value.get("buildId"), "build_mismatch", "Installed pointer must identify its exact candidate build.")?;
        internal(self.db.execute("INSERT INTO installed VALUES (?,?) ON CONFLICT(instance_id) DO UPDATE SET value=excluded.value", params![id, encoded(value)?]))?;
        Ok(())
    }
    fn save_target(&self, value: &IvyJson) -> IvyResult<()> {
        internal(self.db.execute("INSERT INTO runtime_targets VALUES (?,?) ON CONFLICT(instance_id) DO UPDATE SET value=excluded.value", params![value.required("instanceId")?.as_str()?, encoded(value)?]))?; Ok(())
    }
    fn sync_bootstrap(&self, plan: &IvyJson, clear_targets: bool) -> IvyResult<()> {
        validate_host("BootstrapPlan", plan)?;
        self.transaction(|s| {
            let requested_at = s.now()?;
            for process in plan.required("processes")?.as_array().ok_or_else(IvyError::internal)? {
                if !truthy(field(process, "candidateId")) { continue; }
                let id = process.required("instanceId")?.as_str()?;
                let instance = s.instance(&id)?;
                require(instance.get("componentId") == process.get("componentId"), "target_conflict", "Bootstrap process belongs to another configured component.")?;
                let candidate = s.candidate(&process.required("candidateId")?.as_str()?)?;
                if let Some(previous) = s.value("SELECT value FROM installed WHERE instance_id=?", &id)? {
                    if previous.get("candidateId") != candidate.get("candidateId") {
                        s.save_meta(&format!("bootstrap-rollback:{id}"), &encoded(&IvyJson::object([("candidateId", field(&previous, "candidateId").clone()), ("selectedCandidateId", field(&candidate, "candidateId").clone())]))?)?;
                    }
                }
                s.set_installed(&IvyJson::object([("instanceId", id.clone().into()), ("candidateId", field(&candidate, "candidateId").clone()), ("buildId", field(&candidate, "buildId").clone()), ("enabled", true.into()), ("installedAt", s.now()?.into())]))?;
                if clear_targets { internal(s.db.execute("DELETE FROM runtime_targets WHERE instance_id=?", [&id]))?; continue; }
                if !truthy(field(process, "configPath")) || field(process, "componentId") == &IvyJson::from("host-executor") { continue; }
                let source = path(process.required("configPath")?)?;
                require(internal(crate::paths::inside(&path(s.config.required("runtimeRoot")?)?, &source))?, "target_conflict", "Bootstrap runtime configuration leaves the host runtime root.")?;
                let metadata = internal(fs::symlink_metadata(&source))?;
                require(metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= 1024 * 1024, "configuration_changed", "Bootstrap runtime configuration is not a bounded regular file.")?;
                let config = decoded(&String::from_utf8_lossy(&internal(fs::read(source))?))?;
                validate_host("InstanceConfig", &config)?;
                require(config.get("instanceId") == process.get("instanceId") && config.get("componentId") == process.get("componentId") && config.get("hostId") == s.config.get("hostId") && config.get("artifactRoot") == candidate.get("artifactRoot") && config.get("buildId") == candidate.get("buildId"), "build_mismatch", "Bootstrap runtime configuration does not identify the installed candidate.")?;
                let target = IvyJson::object([("schemaVersion", IvyJson::Number(1.0)), ("instanceId", id.clone().into()), ("revision", bootstrap_launch_id(plan, &id)?.into()), ("candidateId", field(&candidate, "candidateId").clone()), ("desired", "running".into()), ("configPath", field(process, "configPath").clone()), ("requestedAt", requested_at.clone().into())]);
                validate_host("RuntimeTarget", &target)?; s.save_target(&target)?;
            }
            Ok(())
        })
    }
    fn candidate_for_build(&self, component: &str, build: &str) -> IvyResult<IvyJson> {
        let mut statement = internal(self.db.prepare("SELECT candidate_json FROM candidates WHERE component_id=? AND build_id=? ORDER BY candidate_id LIMIT 2"))?;
        let rows = internal(statement.query_map(params![component, build], |r| r.get::<_, String>(0)))?;
        let values = internal(rows.collect::<Result<Vec<_>, _>>())?;
        require(values.len() == 1, if values.is_empty() { "not_found" } else { "ambiguous_candidate" }, "The retained build must identify one exact local candidate.")?;
        normalize_candidate(&decoded(&values[0])?)
    }
    fn open(config: IvyJson, clock: Arc<dyn Clock>, history_limits: HistoryLimits) -> IvyResult<Self> {
        validate_host("HostConfig", &config)?;
        let root = path(config.required("runtimeRoot")?)?;
        internal(fs::create_dir_all(&root))?;
        let db = internal(sqlite::open(&root.join("deployments.sqlite"), sqlite::OpenOptions { busy_timeout: Duration::from_millis(5000), read_only: false }))?;
        internal(db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;"))?;
        let format: i64 = internal(db.query_row("PRAGMA user_version", [], |r| r.get(0)))?;
        require(format == 0 || format == 1, "unsupported_storage", "Host journal format is not supported by this executor.")?;
        internal(db.execute_batch(SCHEMA))?;
        let state = Self { db, config, clock, history_limits };
        state.transaction(|s| {
            let host = s.config.required("hostId")?.as_str()?;
            require(s.text("SELECT value FROM meta WHERE key=?", "hostId")?.is_none_or(|v| v == host), "target_conflict", "A host journal cannot be reused for another host identity.")?;
            internal(s.db.execute("INSERT OR IGNORE INTO meta VALUES (?,?)", params!["hostId", host]))?;
            let installation = installation_record(&s.config)?;
            let anchor = s.text("SELECT value FROM meta WHERE key=?", "installation")?;
            if anchor.is_none() {
                if let Some(hash) = s.text("SELECT value FROM meta WHERE key=?", "configurationAnchor")? {
                    let source = legacy_path(&s.config, &hash.clone().into())?;
                    let missing = || IvyError::new("configuration_changed", "Retained installation configuration is missing or invalid.");
                    let metadata = fs::symlink_metadata(&source).map_err(|_| missing())?;
                    require(metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= 1024 * 1024, "configuration_changed", "Retained installation configuration is not a bounded regular file.")?;
                    let bytes = fs::read(&source).map_err(|_| missing())?;
                    let prior = json::parse(&String::from_utf8_lossy(&bytes), usize::MAX).map_err(|_| missing())?;
                    validate_host("HostConfig", &prior)?;
                    require(hash_json(&prior)? == hash, "configuration_changed", "Retained installation configuration changed.")?;
                    same_installation(&prior, &s.config)?;
                }
            }
            require(anchor.is_none_or(|v| v == installation), "target_conflict", "Host operational roots and recovery identity cannot change within an installation.")?;
            internal(s.db.execute("INSERT OR IGNORE INTO meta VALUES (?,?)", params!["installation", installation]))?;
            s.identities(&s.config)
        })?;
        Ok(state)
    }
    fn transaction<T>(&self, work: impl FnOnce(&Self) -> IvyResult<T>) -> IvyResult<T> {
        internal(self.db.execute_batch("BEGIN IMMEDIATE"))?;
        match work(self).and_then(|value| { internal(self.db.execute_batch("COMMIT"))?; Ok(value) }) {
            Ok(v) => Ok(v), Err(e) => { internal(self.db.execute_batch("ROLLBACK"))?; Err(e) }
        }
    }
    fn text(&self, sql: &str, key: &str) -> IvyResult<Option<String>> { internal(self.db.query_row(sql, [key], |r| r.get(0)).optional()) }
    fn value(&self, sql: &str, key: &str) -> IvyResult<Option<IvyJson>> { self.text(sql, key)?.map(|s| decoded(&s)).transpose() }
    fn identities(&self, config: &IvyJson) -> IvyResult<()> {
        for instance in config.required("instances")?.as_array().ok_or_else(IvyError::internal)? {
            let key = format!("instanceIdentity:{}", instance.required("instanceId")?.as_str()?);
            let identity = encoded(&project(instance, &["componentId", "serviceNodeId"]))?;
            require(self.text("SELECT value FROM meta WHERE key=?", &key)?.is_none_or(|v| v == identity), "target_conflict", "A previously configured instance identity cannot be reused.")?;
            internal(self.db.execute("INSERT OR IGNORE INTO meta VALUES (?,?)", params![key, identity]))?;
        }
        Ok(())
    }
    fn instance(&self, id: &str) -> IvyResult<&IvyJson> {
        self.config.required("instances")?.as_array().ok_or_else(IvyError::internal)?.iter().find(|i| i.get("instanceId") == Some(&IvyJson::from(id)))
            .ok_or_else(|| IvyError::new("not_found", "Instance is not in this explicit host configuration."))
    }
    fn normalize_entry(&self, value: IvyJson) -> IvyResult<IvyJson> {
        if !truthy(field(&value, "configurationHash")) { validate_host("JournalEntry", &value)?; return Ok(value) }
        let request = value.required("request")?;
        let mut record = value.required("record")?.clone();
        validate_host("LocalRequest", request)?;
        validate_shared("DeploymentRecord", &record)?;
        if !TERMINAL.contains(&record.required("phase")?.as_str()?.as_str()) {
            record.insert("phase", "needs_attention".into()); record.insert("errorCode", "legacy_activation_requires_reconciliation".into());
        }
        let mut entry = project(&value, &["request", "candidateId", "previousCandidateId", "sourceSnapshot"]);
        entry.insert("schemaVersion", IvyJson::Number(1.0)); entry.insert("record", record);
        entry.insert("configurationPath", path_json(&legacy_path(&self.config, field(&value, "configurationHash"))?));
        entry.insert("activation", IvyJson::object([("previousTarget", IvyJson::Null), ("previousEnabled", true.into()), ("targetEnabled", (field(request, "action") != &IvyJson::from("disable")).into()), ("rollbackTargetRevision", IvyJson::Null)]));
        validate_host("JournalEntry", &entry)?; Ok(entry)
    }
    fn lookup(&self, id: &str, operation: bool) -> IvyResult<Option<IvyJson>> {
        let (sql, retired, message) = if operation {
            ("SELECT entry_json FROM operations WHERE operation_id=?", "SELECT operation_id FROM retired_operations WHERE operation_id=?", "The completed deployment left the history ring; its identity cannot be reused.")
        } else {
            ("SELECT entry_json FROM operations WHERE deployment_id=?", "SELECT deployment_id FROM retired_operations WHERE deployment_id=?", "The completed deployment left the history ring; reconcile the current host state.")
        };
        let row = self.value(sql, id)?;
        require(row.is_some() || self.text(retired, id)?.is_none(), "operation_expired", message)?;
        row.map(|v| self.normalize_entry(v)).transpose()
    }
    fn entries(&self, sql: &str, params: impl Params) -> IvyResult<Vec<IvyJson>> {
        let mut statement = internal(self.db.prepare(sql))?;
        let rows = internal(statement.query_map(params, |r| r.get::<_, String>(0)))?;
        rows.map(|r| self.normalize_entry(decoded(&internal(r)?)?)).collect()
    }
    fn target(&self, id: &str) -> IvyResult<Option<IvyJson>> {
        self.value("SELECT value FROM runtime_targets WHERE instance_id=?", id)?.map(|v| {
            let value = project(&v, &["schemaVersion", "instanceId", "revision", "candidateId", "desired", "configPath", "requestedAt"]);
            validate_host("RuntimeTarget", &value)?; Ok(value)
        }).transpose()
    }
    fn candidate(&self, id: &str) -> IvyResult<IvyJson> {
        let value = self.value("SELECT candidate_json FROM candidates WHERE candidate_id=?", id)?.ok_or_else(|| IvyError::new("not_found", "Prepared host candidate was not found."))?;
        normalize_candidate(&value)
    }
    fn observations(&self) -> IvyResult<IvyJson> {
        let mut statement = internal(self.db.prepare("SELECT instance_id,value FROM observations ORDER BY instance_id"))?;
        let rows = internal(statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))))?;
        let pairs = rows.map(|r| { let (key, value) = internal(r)?; Ok((key, decoded(&value)?)) }).collect::<IvyResult<Vec<_>>>()?;
        Ok(IvyJson::object(pairs))
    }
    fn current_observations(&self, executor: Option<&IvyJson>) -> IvyResult<IvyJson> {
        let prior = self.observations()?;
        let mut result = IvyJson::Object(vec![]);
        for instance in self.config.required("instances")?.as_array().ok_or_else(IvyError::internal)? {
            let id = instance.required("instanceId")?.as_str()?;
            let installed = self.value("SELECT value FROM installed WHERE instance_id=?", &id)?.unwrap_or(IvyJson::Null);
            let target = self.target(&id)?.unwrap_or(IvyJson::Null);
            let previous = field(&prior, &id);
            if field(instance, "componentId") == &IvyJson::from("host-executor") {
                let observation = if let Some(executor) = executor.filter(|v| fresh_executor_status(v, self.clock.as_ref())) {
                    let matches = truthy(field(executor, "buildId")) && (installed.is_null() || executor.get("buildId") == installed.get("buildId"));
                    let state = if !matches { "unknown" } else { match field(executor, "state").as_str()?.as_str() { "ready" => "ready", "stopping" => "stopped", "failed" => "failed", _ => "starting" } };
                    let message: IvyJson = if !matches { "Running executor release does not match the installed release.".into() }
                        else if truthy(field(executor, "code")) { format!("Host executor reported {}.", field(executor, "code").as_str()?).into() } else { "".into() };
                    IvyJson::object([
                        ("schemaVersion", IvyJson::Number(1.0)), ("instanceId", id.clone().into()), ("ownerPid", field(executor, "pid").clone()), ("ownerBootId", field(executor, "bootId").clone()),
                        ("observedAt", field(executor, "observedAt").clone()), ("targetRevision", field(&target, "revision").clone()),
                        ("candidateId", if matches { fallback(field(&installed, "candidateId"), field(&target, "candidateId")) } else { IvyJson::Null }),
                        ("buildId", field(executor, "buildId").clone()), ("state", state.into()), ("health", IvyJson::Null), ("restartCount", IvyJson::Number(0.0)), ("nextRestartAt", IvyJson::Null),
                        ("code", if matches { field(executor, "code").clone() } else { "build_mismatch".into() }), ("message", message),
                    ])
                } else { IvyJson::object([
                    ("schemaVersion", IvyJson::Number(1.0)), ("instanceId", id.clone().into()), ("ownerPid", fallback(field(previous, "ownerPid"), &IvyJson::Number(std::process::id() as f64))),
                    ("ownerBootId", fallback(field(previous, "ownerBootId"), &"00000000-0000-4000-8000-000000000000".into())), ("observedAt", fallback(field(previous, "observedAt"), &"1970-01-01T00:00:00.000Z".into())),
                    ("targetRevision", field(&target, "revision").clone()), ("candidateId", fallback(field(&installed, "candidateId"), field(&target, "candidateId"))), ("buildId", IvyJson::Null),
                    ("state", "unknown".into()), ("health", IvyJson::Null), ("restartCount", fallback(field(previous, "restartCount"), &IvyJson::Number(0.0))), ("nextRestartAt", field(previous, "nextRestartAt").clone()),
                    ("code", "health_stale".into()), ("message", "Host executor status is missing or stale; readiness is not current.".into()),
                ]) };
                result.insert(id, observation); continue;
            }
            let observation = (|| -> IvyResult<IvyJson> {
                let source = path(service_paths(&self.config, instance)?.required("data")?)?.join("health.json");
                let bytes = internal(fs::read(source))?;
                let health = decoded(&String::from_utf8_lossy(&bytes))?;
                validate_host("Health", &health)?;
                require(health.get("instanceId") == instance.get("instanceId") && health.get("componentId") == instance.get("componentId"), "target_conflict", "Health file belongs to another configured instance.")?;
                let expected = if target.is_null() { field(&installed, "buildId").clone() } else { self.candidate(&target.required("candidateId")?.as_str()?)?.required("buildId")?.clone() };
                let launch_matches = !truthy(field(&health, "launchId")) || target.is_null() || health.get("launchId") == target.get("revision");
                let running = field(&target, "desired") != &IvyJson::from("stopped");
                let ready = fresh(field(&health, "observedAt"), self.clock.as_ref()) && truthy(field(&health, "ready")) && running && launch_matches && (!truthy(&expected) || field(&health, "buildId") == &expected);
                let state = if ready { "ready" } else if !fresh(field(&health, "observedAt"), self.clock.as_ref()) { "unknown" } else if !running { "stopped" } else if truthy(field(&health, "ready")) { "unknown" } else { "starting" };
                let code = if ready { IvyJson::Null } else if !fresh(field(&health, "observedAt"), self.clock.as_ref()) { "health_stale".into() } else if !launch_matches || truthy(&expected) && field(&health, "buildId") != &expected { "build_mismatch".into() } else if !running { IvyJson::Null } else { "service_not_ready".into() };
                let message = if ready { field(&health, "details").clone() } else if code == IvyJson::from("health_stale") { "Owner health is stale; readiness is not current.".into() } else if truthy(field(&health, "details")) { field(&health, "details").clone() } else { "Owner health is not ready.".into() };
                Ok(IvyJson::object([
                    ("schemaVersion", IvyJson::Number(1.0)), ("instanceId", id.clone().into()), ("ownerPid", field(&health, "pid").clone()), ("ownerBootId", field(&health, "bootId").clone()),
                    ("observedAt", field(&health, "observedAt").clone()), ("targetRevision", fallback(field(&target, "revision"), field(&health, "launchId"))),
                    ("candidateId", fallback(field(&target, "candidateId"), field(&installed, "candidateId"))), ("buildId", field(&health, "buildId").clone()), ("state", state.into()), ("health", health),
                    ("restartCount", fallback(field(previous, "restartCount"), &IvyJson::Number(0.0))), ("nextRestartAt", field(previous, "nextRestartAt").clone()), ("code", code), ("message", message),
                ]))
            })();
            match observation {
                Ok(value) => result.insert(id, value),
                Err(_) if !previous.is_null() => {
                    let mut value = previous.clone(); value.insert("state", if field(&target, "desired") == &IvyJson::from("stopped") { "stopped".into() } else { "unknown".into() });
                    value.insert("code", "health_stale".into()); value.insert("message", "Current owner health is missing or invalid; the last ready observation is not current.".into()); result.insert(id, value);
                },
                Err(_) => (),
            }
        }
        Ok(result)
    }
    fn status(&self, executor: Option<&IvyJson>) -> IvyResult<IvyJson> {
        let retention = self.storage_retention_status()?;
        let executor = executor.filter(|v| fresh_executor_status(v, self.clock.as_ref()));
        let observed = self.current_observations(executor)?;
        let now = self.now()?;
        let mut instances = Vec::new();
        for instance in self.config.required("instances")?.as_array().ok_or_else(IvyError::internal)? {
            let id = instance.required("instanceId")?.as_str()?;
            let installed = self.value("SELECT value FROM installed WHERE instance_id=?", &id)?.unwrap_or(IvyJson::Null);
            let actual = field(&observed, &id);
            let mut value = project(instance, &["instanceId", "serviceNodeId", "componentId", "engine"]);
            value.insert("desiredEnabled", fallback(field(&installed, "enabled"), field(instance, "enabled")));
            for (key, source) in [("installedBuild", "buildId"), ("installedCandidateId", "candidateId")] { value.insert(key, field(&installed, source).clone()); }
            for (key, source) in [("observedBuild", "buildId"), ("observedState", "state"), ("observedAt", "observedAt"), ("code", "code")] { value.insert(key, field(actual, source).clone()); }
            let message = if actual.is_null() { "".into() } else {
                let units = field(actual, "message").as_js_string().ok_or_else(IvyError::internal)?;
                IvyJson::String(JsString(units.0[..units.0.len().min(512)].to_vec()))
            };
            value.insert("message", message); instances.push(value);
        }
        let unfinished = self.entries("SELECT entry_json FROM operations WHERE phase NOT IN ('succeeded','rolled_back','failed','needs_attention') ORDER BY created_at,deployment_id LIMIT 200", [])?.into_iter().map(|v| v.required("record").cloned()).collect::<IvyResult<Vec<_>>>()?;
        let mut status = IvyJson::object([("schemaVersion", IvyJson::Number(1.0)), ("hostId", self.config.required("hostId")?.clone()), ("observedAt", now.into()), ("executor", executor.cloned().unwrap_or(IvyJson::Null)), ("instances", IvyJson::Array(instances)), ("unfinished", IvyJson::Array(unfinished))]);
        if let Some(retention) = retention { status.insert("storageRetention", retention); }
        validate_host("HostStatus", &status)?; Ok(status)
    }
}
fn normalize_candidate(value: &IvyJson) -> IvyResult<IvyJson> {
    let value = project(value, &["candidateId", "componentId", "artifactRoot", "manifestPath", "createdAt", "platform", "buildId", "archivePath", "archiveHash", "dockerImage"]);
    validate_host("Candidate", &value)?; Ok(value)
}

fn completed(value: &IvyJson) -> bool { ["succeeded", "rolled_back"].iter().any(|phase| value == &IvyJson::from(*phase)) }
fn transition_allowed(previous: &str, next: &str) -> bool {
    if previous == next { return !TERMINAL.contains(&previous); }
    let allowed: &[&str] = match previous {
        "preparing" => &["prepared", "failed", "needs_attention"],
        "prepared" => &["checking", "failed", "needs_attention"],
        "checking" => &["draining", "failed", "needs_attention"],
        "draining" => &["activating", "rolling_back", "failed", "needs_attention"],
        "activating" => &["verifying", "rolling_back", "needs_attention"],
        "verifying" => &["succeeded", "rolling_back", "needs_attention"],
        "rolling_back" => &["rolled_back", "needs_attention"], _ => &[],
    };
    allowed.contains(&next)
}
impl State {
    fn accept(&self, request: &IvyJson, source_snapshot: Option<&IvyJson>) -> IvyResult<IvyJson> {
        validate_host("LocalRequest", request)?;
        let request_hash = hash_json(request)?;
        self.transaction(|s| {
            let operation = request.required("operationId")?.as_str()?;
            let retired = s.text("SELECT request_hash FROM retired_operations WHERE operation_id=?", &operation)?;
            require(retired.is_none_or(|hash| hash == request_hash), "mutation_conflict", "The evicted deployment had different original arguments.")?;
            if let Some(previous) = s.lookup(&operation, true)? {
                require(field(previous.required("record")?, "requestHash") == &IvyJson::from(request_hash.clone()), "mutation_conflict", "Operation ID already identifies different original deployment arguments.")?;
                return Ok(previous);
            }
            let id = request.required("instanceId")?.as_str()?;
            let instance = s.instance(&id)?;
            let installed = s.value("SELECT value FROM installed WHERE instance_id=?", &id)?.unwrap_or(IvyJson::Null);
            require(!requires_bootstrap_replacement(instance) || installed.is_null(), "restart_required", "Bootstrap-owned releases use prepare followed by install-bootstrap.")?;
            require(request.get("componentId").is_none() || request.get("componentId") == instance.get("componentId"), "target_conflict", "Source component does not match the configured instance.")?;
            let action = request.required("action")?.as_str()?;
            if action == "deploy" {
                require(truthy(field(request, "candidateId")) != truthy(field(request, "source")) && request.get("targetBuild").is_none(), "invalid_arguments", "Deploy requires exactly one candidate or source.")?;
            } else if action == "rollback" {
                require(truthy(field(request, "targetBuild")) && !truthy(field(request, "candidateId")) && !truthy(field(request, "source")) && request.get("componentId").is_none(), "invalid_arguments", "Rollback requires one retained target build.")?;
            } else {
                require(!truthy(field(request, "targetBuild")) && !truthy(field(request, "candidateId")) && !truthy(field(request, "source")) && request.get("componentId").is_none() && !installed.is_null(), "invalid_arguments", "Lifecycle actions require one installed instance and no candidate arguments.")?;
            }
            let snapshot = source_snapshot.filter(|v| truthy(v));
            if truthy(field(request, "source")) {
                let snapshot = snapshot.ok_or_else(|| IvyError::new("source_snapshot_required", "Capture source inputs before handing the action to the independent executor."))?;
                validate_host("SourceSnapshot", snapshot)?;
                let root = path(s.config.required("stagingRoot")?)?;
                require(internal(crate::paths::inside(&root, &path(snapshot.required("sourceRoot")?)?))? && internal(crate::paths::inside(&root, &path(snapshot.required("manifestPath")?)?))?, "target_conflict", "Source snapshot belongs to another host staging root.")?;
            } else {
                require(snapshot.is_none(), "invalid_arguments", "This action does not accept a source snapshot.")?;
            }
            let candidate = if truthy(field(request, "candidateId")) { s.candidate(&request.required("candidateId")?.as_str()?)? }
                else if truthy(field(request, "targetBuild")) { s.candidate_for_build(&instance.required("componentId")?.as_str()?, &request.required("targetBuild")?.as_str()?)? }
                else if action != "deploy" && !installed.is_null() { s.candidate(&installed.required("candidateId")?.as_str()?)? } else { IvyJson::Null };
            require(candidate.is_null() || candidate.get("componentId") == instance.get("componentId"), "target_conflict", "Candidate belongs to another component.")?;
            let previous_target = s.target(&id)?.unwrap_or(IvyJson::Null);
            let previous_enabled = fallback(field(&installed, "enabled"), field(instance, "enabled"));
            let previous_configuration = if previous_target.is_null() { None } else {
                let source = crate::accepted_configuration::accepted_instance_configuration_path(&s.config, &previous_target.required("revision")?.as_str()?)?;
                source.exists().then_some(source)
            };
            let target_enabled = match action.as_str() { "enable" => true.into(), "disable" => false.into(), _ => previous_enabled.clone() };
            let configuration_path = crate::accepted_configuration::save_accepted_configuration(&s.config)?;
            let now = s.now()?;
            let deployment = uuid::Uuid::new_v4().to_string();
            let mut activation = IvyJson::object([("previousTarget", previous_target), ("previousEnabled", previous_enabled), ("targetEnabled", target_enabled), ("rollbackTargetRevision", IvyJson::Null)]);
            if let Some(source) = previous_configuration { activation.insert("previousConfigurationPath", path_json(&source)); }
            let record = IvyJson::object([
                ("deploymentId", deployment.clone().into()), ("hostId", s.config.required("hostId")?.clone()), ("instanceId", id.into()), ("componentId", instance.required("componentId")?.clone()),
                ("requestHash", request_hash.clone().into()), ("previousBuild", field(&installed, "buildId").clone()), ("targetBuild", field(&candidate, "buildId").clone()), ("observedBuild", IvyJson::Null),
                ("phase", if candidate.is_null() { "preparing".into() } else { "prepared".into() }), ("createdAt", now.clone().into()), ("updatedAt", now.clone().into()),
                ("readiness", IvyJson::object([("state", "not_run".into()), ("message", "Accepted by the independent host journal.".into())])),
            ]);
            let mut entry = IvyJson::object([("schemaVersion", IvyJson::Number(1.0)), ("request", request.clone()), ("candidateId", field(&candidate, "candidateId").clone()), ("previousCandidateId", field(&installed, "candidateId").clone()), ("configurationPath", path_json(&configuration_path)), ("activation", activation), ("record", record)]);
            if let Some(snapshot) = snapshot { entry.insert("sourceSnapshot", snapshot.clone()); }
            validate_host("JournalEntry", &entry)?;
            internal(s.db.execute("INSERT INTO operations VALUES (?,?,?,?,?,?)", params![operation, deployment, request_hash, entry.required("record")?.required("phase")?.as_str()?, now, encoded(&entry)?]))?;
            s.collect_history(&operation)?;
            Ok(entry)
        })
    }
    fn advance(&self, id: &str, expected: &str, phase: &str, change: &IvyJson) -> IvyResult<IvyJson> {
        self.transaction(|s| {
            let mut entry = s.lookup(id, false)?.ok_or_else(|| IvyError::new("not_found", "Host-local deployment was not found."))?;
            let mut record = entry.required("record")?.clone();
            let mut activation = entry.required("activation")?.clone();
            require(record.get("phase") == Some(&IvyJson::from(expected)) && transition_allowed(expected, phase), "revision_conflict", "Deployment phase changed or transition is invalid.")?;
            if let Some(id) = change.get("candidateId") {
                require(record.get("phase") == Some(&IvyJson::from("preparing")), "revision_conflict", "Candidate can only be fixed during preparation.")?;
                let candidate = s.candidate(&id.as_str()?)?;
                require(candidate.get("componentId") == record.get("componentId"), "target_conflict", "Prepared source changed component.")?;
                entry.insert("candidateId", candidate.required("candidateId")?.clone()); record.insert("targetBuild", candidate.required("buildId")?.clone());
            }
            record.insert("phase", phase.into()); record.insert("updatedAt", s.now()?.into());
            if let Some(value) = change.get("observedBuild") { record.insert("observedBuild", value.clone()); }
            for key in ["readiness", "errorCode"] { if let Some(value) = change.get(key).filter(|v| truthy(v)) { record.insert(key, value.clone()); } }
            if let Some(target) = change.get("target").filter(|v| truthy(v)) {
                validate_host("RuntimeTarget", target)?;
                require(target.get("instanceId") == record.get("instanceId") && [entry.get("candidateId"), entry.get("previousCandidateId")].contains(&target.get("candidateId")), "target_conflict", "Runtime target must belong to this deployment.")?;
                require(internal(crate::paths::inside(&path(s.config.required("runtimeRoot")?)?, &path(target.required("configPath")?)?))?, "target_conflict", "Runtime configuration must belong to this host.")?;
                s.save_target(target)?;
            }
            if let Some(revision) = change.get("rollbackTargetRevision") {
                let target = field(change, "target");
                require(phase == "rolling_back" && target.get("revision") == Some(revision) && target.get("candidateId") == entry.get("previousCandidateId"), "target_conflict", "Rollback revision must identify the retained previous target.")?;
                activation.insert("rollbackTargetRevision", revision.clone());
            }
            if let Some(installed) = change.get("installed").filter(|v| truthy(v)) {
                require(installed.get("instanceId") == record.get("instanceId") && [entry.get("candidateId"), entry.get("previousCandidateId")].contains(&installed.get("candidateId")), "target_conflict", "Installed pointer must belong to this deployment.")?;
                s.set_installed(installed)?;
            }
            validate_shared("DeploymentRecord", &record)?;
            entry.insert("record", record); entry.insert("activation", activation);
            validate_host("JournalEntry", &entry)?;
            internal(s.db.execute("UPDATE operations SET phase=?,entry_json=? WHERE deployment_id=?", params![phase, encoded(&entry)?, id]))?;
            s.collect_history(&entry.required("request")?.required("operationId")?.as_str()?)?;
            Ok(entry)
        })
    }
    fn collect_history(&self, protected_operation: &str) -> IvyResult<()> {
        let (mut count, mut bytes): (i64, i64) = internal(self.db.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(entry_json AS BLOB))),0) FROM operations", [], |r| Ok((r.get(0)?, r.get(1)?))))?;
        let fits = |count: i64, bytes: i64| count as usize <= self.history_limits.count && bytes as usize <= self.history_limits.bytes;
        if fits(count, bytes) { return Ok(()) }
        let mut protected = BTreeSet::from([protected_operation.to_owned()]);
        let mut latest = BTreeMap::<String, usize>::new(); let mut rollback = BTreeSet::new();
        let mut recovered = BTreeSet::new(); let mut resolved = BTreeSet::new();
        let mut statement = internal(self.db.prepare("SELECT operation_id,entry_json FROM operations ORDER BY rowid DESC"))?;
        let rows = internal(statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))))?;
        let rows = internal(rows.collect::<Result<Vec<_>, _>>())?;
        if let Some(adoption) = self.configuration_adoption()? {
            for action in adoption.required("actions")?.as_array().ok_or_else(IvyError::internal)? { protected.insert(action.required("operationId")?.as_str()?); }
        }
        for (operation, text) in &rows {
            let entry = self.normalize_entry(decoded(text)?)?; let record = entry.required("record")?;
            let instance = record.required("instanceId")?.as_str()?; let phase = record.required("phase")?;
            if phase == &IvyJson::from("failed") && recovered.contains(&instance) { resolved.insert(operation.clone()); continue; }
            if !completed(phase) {
                let mut values = vec![&entry];
                while let Some(value) = values.pop() { match value {
                    IvyJson::String(value) => { if let Ok(value) = value.to_utf8() { protected.insert(value); } },
                    IvyJson::Array(value) => values.extend(value), IvyJson::Object(value) => values.extend(value.iter().map(|(_, v)| v)), _ => (),
                } }
                continue;
            }
            recovered.insert(instance.clone());
            let retained = latest.entry(instance.clone()).or_default();
            if *retained < 2 { protected.insert(operation.clone()); *retained += 1; }
            let installed = self.value("SELECT value FROM installed WHERE instance_id=?", &instance)?.unwrap_or(IvyJson::Null);
            if phase == &IvyJson::from("succeeded") && !rollback.contains(&instance) && installed.get("candidateId") == entry.get("candidateId") && truthy(field(&entry, "previousCandidateId")) && entry.get("previousCandidateId") != entry.get("candidateId") {
                protected.insert(operation.clone()); rollback.insert(instance);
            }
        }
        for (operation, text) in rows.iter().rev() {
            if fits(count, bytes) { break; }
            let entry = self.normalize_entry(decoded(text)?)?; let record = entry.required("record")?; let id = record.required("deploymentId")?.as_str()?;
            if !completed(record.required("phase")?) && !resolved.contains(operation) || protected.contains(operation) || protected.contains(&id) { continue; }
            internal(self.db.execute("INSERT INTO retired_operations VALUES (?,?,?)", params![operation, id, record.required("requestHash")?.as_str()?]))?;
            internal(self.db.execute("DELETE FROM operations WHERE operation_id=?", [operation]))?;
            count -= 1; bytes -= text.len() as i64;
        }
        Ok(())
    }
    fn retention_candidate_ids(&self) -> IvyResult<BTreeSet<String>> {
        let mut live = BTreeSet::new(); let mut protected = BTreeSet::new();
        let mut installed = BTreeMap::new(); let mut rollback_found = BTreeSet::new(); let mut rollback_selections = BTreeSet::new();
        for table in ["installed", "runtime_targets", "observations"] {
            let mut statement = internal(self.db.prepare(&format!("SELECT instance_id,value FROM {table}")))?;
            let rows = internal(statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))))?;
            for row in rows { let (instance, text) = internal(row)?; let value = decoded(&text)?;
                if truthy(field(&value, "candidateId")) { let id = value.required("candidateId")?.as_str()?; live.insert(id.clone()); protected.insert(id.clone()); if table == "installed" { installed.insert(instance, id); } }
            }
        }
        for (instance, selected) in &installed {
            if let Some(previous) = self.value("SELECT value FROM meta WHERE key=?", &format!("bootstrap-rollback:{instance}"))? {
                if previous.get("selectedCandidateId") == Some(&IvyJson::from(selected.clone())) {
                    protected.insert(previous.required("candidateId")?.as_str()?); rollback_found.insert(instance.clone()); rollback_selections.insert(selected.clone());
                }
            }
        }
        let mut statement = internal(self.db.prepare("SELECT entry_json FROM operations ORDER BY created_at DESC,operation_id DESC"))?;
        let rows = internal(statement.query_map([], |r| r.get::<_, String>(0)))?;
        for row in rows {
            let entry = decoded(&internal(row)?)?; let record = entry.required("record")?;
            if !completed(record.required("phase")?) {
                let value = self.normalize_entry(entry)?;
                for id in [field(&value, "candidateId"), field(&value, "previousCandidateId"), field(field(&value, "request"), "candidateId"), field(field(field(&value, "activation"), "previousTarget"), "candidateId")] {
                    if truthy(id) { protected.insert(id.as_str()?); }
                }
            } else {
                let instance = record.required("instanceId")?.as_str()?;
                if record.get("phase") == Some(&IvyJson::from("succeeded")) && installed.get(&instance).map(|v| IvyJson::from(v.clone())).as_ref() == entry.get("candidateId") && truthy(field(&entry, "previousCandidateId")) && entry.get("previousCandidateId") != entry.get("candidateId") && !rollback_found.contains(&instance) {
                    protected.insert(entry.required("previousCandidateId")?.as_str()?); rollback_found.insert(instance); rollback_selections.insert(entry.required("candidateId")?.as_str()?);
                }
            }
        }
        let mut statement = internal(self.db.prepare("SELECT candidate_id,component_id,manifest_json FROM candidates"))?;
        let rows = internal(statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))))?;
        let mut releases = Vec::new();
        for row in rows {
            let (id, component, text) = internal(row)?; let value = decoded(&text)?;
            if let Ok(version) = field(&value, "version").as_str() {
                let parts: Vec<_> = version.split('.').collect();
                if parts.len() == 3 && parts.iter().all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()) && (part.len() == 1 || !part.starts_with('0'))) { releases.push((id, component, version)); }
            }
        }
        for id in &live {
            if rollback_selections.contains(id) { continue; }
            let Some(current) = releases.iter().find(|r| &r.0 == id) else { continue };
            let mut previous: Option<&(String, String, String)> = None;
            for candidate in releases.iter().filter(|r| r.1 == current.1) {
                if json::compare_versions(&candidate.2, &current.2)? != std::cmp::Ordering::Less { continue; }
                let later = match previous { None => true, Some(prior) => {
                    let order = json::compare_versions(&candidate.2, &prior.2)?;
                    // Candidate identities are fixed lowercase SHA-256 strings;
                    // their ASCII ordering equals the original locale tie-break.
                    order == std::cmp::Ordering::Greater || order == std::cmp::Ordering::Equal && candidate.0 > prior.0
                } };
                if later { previous = Some(candidate); }
            }
            if let Some(previous) = previous { protected.insert(previous.0.clone()); }
        }
        Ok(protected)
    }
}
