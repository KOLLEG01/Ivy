//! Host-local journal and current-state projections. SQLite and filesystem work
//! is serialized on blocking workers, including configuration reload.
use crate::{IvyError, IvyJson, IvyResult, accepted_configuration::{installation_record, read_accepted_configuration, same_installation}, layout::{path, path_json, service_paths}};
use ivy_contracts::{JsString, core_schema::{validate_host, validate_shared}, hash::hash_json, json};
use ivy_sdk::clock::{Clock, SystemClock, parse_date};
use ivy_store::{rusqlite::{Connection, OptionalExtension, Params, params}, sqlite};
use std::{fs, path::PathBuf, sync::{Arc, Mutex}, time::Duration};

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
fn field<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson { value.get(key).unwrap_or(&IvyJson::Null) }
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

struct State { db: Connection, config: IvyJson, clock: Arc<dyn Clock> }
/// Clones share one connection and configuration. Call `close` before releasing
/// the final handle when shutdown must wait for all journal work to finish.
#[derive(Clone)]
pub struct HostJournal { state: Arc<Mutex<Option<State>>> }
impl HostJournal {
    pub async fn open(config: IvyJson) -> IvyResult<Self> { Self::open_with_clock(config, Arc::new(SystemClock)).await }
    pub async fn open_with_clock(config: IvyJson, clock: Arc<dyn Clock>) -> IvyResult<Self> {
        let state = internal(tokio::task::spawn_blocking(move || State::open(config, clock)).await)??;
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
    pub async fn target(&self, instance_id: &str) -> IvyResult<Option<IvyJson>> {
        let id = instance_id.to_owned(); self.with(move |s| s.target(&id)).await
    }
    pub async fn candidate(&self, candidate_id: &str) -> IvyResult<IvyJson> {
        let id = candidate_id.to_owned(); self.with(move |s| s.candidate(&id)).await
    }
    pub async fn observations(&self) -> IvyResult<IvyJson> { self.with(|s| s.observations()).await }
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
    fn open(config: IvyJson, clock: Arc<dyn Clock>) -> IvyResult<Self> {
        validate_host("HostConfig", &config)?;
        let root = path(config.required("runtimeRoot")?)?;
        internal(fs::create_dir_all(&root))?;
        let db = internal(sqlite::open(&root.join("deployments.sqlite"), sqlite::OpenOptions { busy_timeout: Duration::from_millis(5000), read_only: false }))?;
        internal(db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;"))?;
        let format: i64 = internal(db.query_row("PRAGMA user_version", [], |r| r.get(0)))?;
        require(format == 0 || format == 1, "unsupported_storage", "Host journal format is not supported by this executor.")?;
        internal(db.execute_batch(SCHEMA))?;
        let state = Self { db, config, clock };
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
        let value = project(&value, &["candidateId", "componentId", "artifactRoot", "manifestPath", "createdAt", "platform", "buildId", "archivePath", "archiveHash", "dockerImage"]);
        validate_host("Candidate", &value)?; Ok(value)
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
        let retention = self.value("SELECT value FROM meta WHERE key=?", "storage-retention")?;
        if let Some(v) = &retention { validate_host("StorageRetentionStatus", v)?; }
        let executor = executor.filter(|v| fresh_executor_status(v, self.clock.as_ref()));
        let observed = self.current_observations(executor)?;
        let now = chrono::DateTime::from_timestamp_millis(self.clock.utc_millis()).ok_or_else(IvyError::internal)?.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
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
