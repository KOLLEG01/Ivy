use crate::model::{self, RecordTask, Run};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult,
    hash::digest,
    json::{parse, stringify},
};
use ivy_store::rusqlite::{self, Connection, OptionalExtension, params};
use std::{fs, path::Path};

pub const SCHEMA: &str = include_str!("schema.sql");

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub operations: u64,
    pub operation_bytes: u64,
    pub runs: u64,
    pub run_bytes: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            operations: 10_000,
            operation_bytes: 256 * 1024 * 1024,
            runs: 1_000,
            run_bytes: 64 * 1024 * 1024,
        }
    }
}

/// A single owner calls synchronous transactions; no network work belongs inside
/// a transaction. The eventual lifecycle owns this connection off the I/O reactor.
pub struct CollectorStore {
    db: Connection,
    limits: Limits,
}

fn sql<T>(r: rusqlite::Result<T>) -> IvyResult<T> {
    r.map_err(|e| {
        if e.to_string().contains("operation_expired") {
            IvyError::new(
                "operation_expired",
                "The completed Collector operation was evicted; its identity cannot be reused.",
            )
        } else {
            IvyError::internal()
        }
    })
}
fn encode(v: &IvyJson) -> IvyResult<String> {
    stringify(v, usize::MAX)
}
fn decode(s: &str) -> IvyResult<IvyJson> {
    parse(s, usize::MAX)
}

impl CollectorStore {
    pub fn open(path: &Path, limits: Limits, now_iso: &str) -> IvyResult<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|_| IvyError::internal())?;
        }
        let db = sql(ivy_store::sqlite::open(
            path,
            ivy_store::sqlite::OpenOptions {
                // A competing writer must fail promptly rather than block this owner.
                busy_timeout: std::time::Duration::ZERO,
                read_only: false,
            },
        ))?;
        sql(db.execute_batch(SCHEMA))?;
        let store = Self { db, limits };
        for mut run in store.runs("running")? {
            run.insert("status", "interrupted".into());
            run.insert("finishedAt", now_iso.into());
            run.insert("error", "outcome_unknown".into());
            store.transaction(|s| {
                s.save_run(&run)?;
                if let Some(mut row) = s.get(&model::text(&run, "taskId")?, false)? {
                    if model::optional_text(&run, "workerJobName").is_none_or(|v| v.is_empty()) {
                        let mut task = model::field(&row, "task")?.clone();
                        task.insert("enabled", false.into());
                        row.insert("task", task);
                        row.insert(
                            "revision",
                            IvyJson::Number(model::number(&row, "revision")? + 1.0),
                        );
                        s.save(&row)?;
                    }
                }
                Ok(())
            })?;
        }
        Ok(store)
    }
    pub fn connection(&self) -> &Connection {
        &self.db
    }
    pub fn usage(&self, kind: &str) -> IvyResult<(u64, u64)> {
        sql(self.db.query_row(
            "SELECT count,bytes FROM journal_usage WHERE kind=?",
            [kind],
            |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64)),
        ))
    }
    pub fn transaction<T>(&self, work: impl FnOnce(&Self) -> IvyResult<T>) -> IvyResult<T> {
        sql(self.db.execute_batch("BEGIN IMMEDIATE"))?;
        // RAII rollback also covers a panic; never leave an open writer transaction.
        struct Rollback<'a>(&'a Connection);
        impl Drop for Rollback<'_> {
            fn drop(&mut self) {
                if !self.0.is_autocommit() {
                    let _ = self.0.execute_batch("ROLLBACK");
                }
            }
        }
        let rollback = Rollback(&self.db);
        let result = work(self)?;
        sql(self.db.execute_batch("COMMIT"))?;
        drop(rollback);
        Ok(result)
    }
    fn document(&self, query: &str, id: &str) -> IvyResult<Option<IvyJson>> {
        sql(self
            .db
            .query_row(query, [id], |r| r.get::<_, String>(0))
            .optional())?
        .map(|s| decode(&s))
        .transpose()
    }
    pub fn get(&self, id: &str, include_deleted: bool) -> IvyResult<Option<RecordTask>> {
        Ok(self
            .document("SELECT document FROM tasks WHERE id=?", id)?
            .filter(|t| include_deleted || !model::truth(t, "deleted")))
    }
    pub fn list(&self, include_deleted: bool) -> IvyResult<Vec<RecordTask>> {
        let mut statement = sql(self.db.prepare("SELECT document FROM tasks ORDER BY id"))?;
        let rows = sql(statement.query_map([], |r| r.get::<_, String>(0)))?;
        let mut result = Vec::new();
        for row in rows {
            let row = decode(&sql(row)?)?;
            if include_deleted || !model::truth(&row, "deleted") {
                result.push(row);
            }
        }
        Ok(result)
    }
    /// SQL projections keep scripts, configuration and private cursors out of
    /// control-plane reads. The original document remains authoritative.
    pub fn summaries(&self, include_deleted: bool) -> IvyResult<Vec<RecordTask>> {
        let mut statement = sql(self.db.prepare("SELECT json_remove(document,'$.task.script','$.task.config','$.task.dependencies','$.state') FROM tasks WHERE ? OR NOT COALESCE(json_extract(document,'$.deleted'),0) ORDER BY id"))?;
        let rows = sql(statement.query_map([include_deleted], |r| r.get::<_, String>(0)))?;
        rows.map(|r| decode(&sql(r)?)).collect()
    }
    pub fn summary(&self, id: &str, include_deleted: bool) -> IvyResult<Option<RecordTask>> {
        Ok(self.document("SELECT json_remove(document,'$.task.script','$.task.config','$.task.dependencies','$.state') FROM tasks WHERE id=?", id)?
            .filter(|r| include_deleted || !model::truth(r, "deleted")))
    }
    pub fn for_job(&self, job: &str) -> IvyResult<Option<RecordTask>> {
        self.document("SELECT json_remove(document,'$.task.script','$.task.config','$.task.dependencies','$.state') FROM tasks WHERE json_extract(document,'$.definition.jobId')=?", job)
    }
    pub fn due_tasks(&self, now: f64) -> IvyResult<Vec<String>> {
        let mut statement = sql(self.db.prepare("SELECT id FROM tasks t WHERE json_extract(document,'$.nextAt')<=? AND NOT COALESCE(json_extract(document,'$.deleted'),0) AND json_extract(document,'$.task.enabled')=1 AND json_extract(document,'$.task.intervalSeconds')>0 AND NOT EXISTS(SELECT 1 FROM runs r WHERE r.task_id=t.id AND r.status IN ('queued','running')) AND NOT EXISTS(SELECT 1 FROM runs r WHERE r.id=json_extract(t.document,'$.lastRunId') AND json_extract(r.document,'$.error')='outcome_unknown') ORDER BY json_extract(document,'$.nextAt'),id LIMIT 64"))?;
        sql(statement.query_map([now], |r| r.get(0)))?
            .map(sql)
            .collect()
    }
    pub fn run_status(&self, id: &str) -> IvyResult<Option<IvyJson>> {
        self.document("SELECT json_object('status',r.status,'error',json(COALESCE(p.error,r.document -> '$.error','null')),'startedAt',json_extract(r.document,'$.startedAt')) FROM runs r LEFT JOIN run_progress p ON p.id=r.id WHERE r.id=?", id)
    }
    pub fn run_count(&self, status: &str) -> IvyResult<usize> {
        sql(self
            .db
            .query_row("SELECT COUNT(*) FROM runs WHERE status=?", [status], |r| {
                r.get::<_, i64>(0)
            }))
        .map(|n| n as usize)
    }
    pub fn next_runs(&self, status: &str, limit: usize) -> IvyResult<Vec<Run>> {
        let mut statement = sql(self
            .db
            .prepare("SELECT document FROM runs WHERE status=? ORDER BY rowid LIMIT ?"))?;
        sql(statement.query_map(params![status, limit as i64], |r| r.get::<_, String>(0)))?
            .map(|r| self.with_progress(decode(&sql(r)?)?))
            .collect()
    }
    pub fn pin_task(&self, task: &IvyJson) -> IvyResult<String> {
        let document = encode(task)?;
        let id = digest(&document);
        sql(self.db.execute(
            "INSERT OR IGNORE INTO task_snapshots VALUES(?,?)",
            [&id, &document],
        ))?;
        Ok(id)
    }
    pub fn execution_task(&self, run: &Run) -> IvyResult<IvyJson> {
        if let Some(id) = model::optional_text(run, "taskSnapshot") {
            self.document("SELECT document FROM task_snapshots WHERE id=?", &id)?
                .ok_or_else(IvyError::internal)
        } else {
            Ok(model::field(run, "task")?.clone())
        }
    }
    pub fn execution_state(&self, id: &str) -> IvyResult<IvyJson> {
        self.document(
            "SELECT COALESCE(document -> '$.state','null') FROM tasks WHERE id=?",
            id,
        )?
        .ok_or_else(IvyError::internal)
    }
    pub fn commit_execution(&self, run: &mut Run, now: f64) -> IvyResult<()> {
        let task = model::text(run, "taskId")?;
        self.transaction(|s| {
            if let Some(state) = model::field(run,"output")?.get("state") {
                sql(s.db.execute("UPDATE tasks SET document=json_set(document,'$.state',json(?),'$.nextAt',?+json_extract(document,'$.task.intervalSeconds')*1000) WHERE id=?", params![encode(state)?,now,task]))?;
            } else {
                sql(s.db.execute("UPDATE tasks SET document=json_set(document,'$.nextAt',?+json_extract(document,'$.task.intervalSeconds')*1000) WHERE id=?", params![now,task]))?;
            }
            run.insert("executionCommitted",true.into());
            s.save_run(run)
        })
    }
    pub fn bind_result(&self, task: &str, binding: &str, revision: &IvyJson) -> IvyResult<()> {
        sql(self.db.execute("UPDATE tasks SET document=json_set(document,'$.resultObjectId',?,'$.resultRevision',json(?)) WHERE id=?",params![binding,encode(revision)?,task]))?;
        Ok(())
    }
    pub fn finish_publication(&self, id: &str) -> IvyResult<()> {
        self.transaction(|s| {
            let snapshot: Option<String> = sql(s.db.query_row("SELECT json_extract(document,'$.taskSnapshot') FROM runs WHERE id=?",[id],|r|r.get(0)))?;
            sql(s.db.execute("UPDATE runs SET status='succeeded',document=json_set(json_remove(document,'$.taskSnapshot'),'$.eventIndex',COALESCE((SELECT event_index FROM run_progress WHERE id=runs.id),json_extract(document,'$.eventIndex')),'$.status','succeeded','$.error',null,'$.input',null,'$.output',null,'$.task.script','','$.task.config',json('{}'),'$.task.dependencies',json('{}')) WHERE id=?",[id]))?;
            sql(s.db.execute("DELETE FROM run_progress WHERE id=?",[id]))?;
            if let Some(snapshot) = snapshot {
                sql(s.db.execute("DELETE FROM task_snapshots WHERE id=? AND NOT EXISTS(SELECT 1 FROM runs WHERE json_extract(document,'$.taskSnapshot')=?)",[&snapshot,&snapshot]))?;
            }
            s.collect_runs(Some(id))
        })
    }
    /// Publication acknowledgement changes only its small progress fields.
    pub fn publication_progress(&self, id: &str, index: usize, error: IvyJson) -> IvyResult<()> {
        sql(self.db.execute("INSERT INTO run_progress SELECT id,?,? FROM runs WHERE id=? AND status='publishing' ON CONFLICT(id) DO UPDATE SET event_index=excluded.event_index,error=excluded.error", params![index as i64, encode(&error)?, id]))?;
        Ok(())
    }
    fn with_progress(&self, mut run: Run) -> IvyResult<Run> {
        let id = model::sqlite_key(model::field(&run, "id")?)?;
        let progress: Option<(i64, String)> = sql(self
            .db
            .query_row(
                "SELECT event_index,error FROM run_progress WHERE id=?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional())?;
        if let Some((index, error)) = progress {
            run.insert("eventIndex", (index as f64).into());
            run.insert("error", decode(&error)?);
        }
        Ok(run)
    }
    pub fn publication_counts(&self) -> IvyResult<std::collections::HashMap<String, usize>> {
        let mut statement = sql(self.db.prepare(
            "SELECT task_id,COUNT(*) FROM runs WHERE status='publishing' GROUP BY task_id",
        ))?;
        sql(statement.query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as usize))))?
            .map(sql)
            .collect()
    }
    pub fn save(&self, value: &RecordTask) -> IvyResult<()> {
        sql(self.db.execute("INSERT INTO tasks VALUES (?,?) ON CONFLICT(id) DO UPDATE SET document=excluded.document", params![model::text(model::field(value,"task")?,"id")?, encode(value)?]))?;
        Ok(())
    }
    pub fn run(&self, id: &str) -> IvyResult<Option<Run>> {
        self.document("SELECT document FROM runs WHERE id=?", id)?
            .map(|r| self.with_progress(r))
            .transpose()
    }
    pub fn runs(&self, status: &str) -> IvyResult<Vec<Run>> {
        let mut statement = sql(self
            .db
            .prepare("SELECT document FROM runs WHERE status=? ORDER BY rowid"))?;
        let rows = sql(statement.query_map([status], |r| r.get::<_, String>(0)))?;
        rows.map(|r| self.with_progress(decode(&sql(r)?)?))
            .collect()
    }
    pub fn save_run(&self, run: &Run) -> IvyResult<()> {
        let status = model::text(run, "status")?;
        let terminal = !matches!(status.as_str(), "queued" | "running" | "publishing")
            && !model::unresolved(run);
        let document = if terminal {
            let mut document = run.clone();
            let mut task = model::field(run, "task")?.clone();
            task.insert("script", "".into());
            task.insert("config", model::object());
            task.insert("dependencies", model::object());
            document.insert("task", task);
            document.insert("input", IvyJson::Null);
            document.insert("output", IvyJson::Null);
            document.remove("taskSnapshot");
            encode(&document)?
        } else {
            encode(run)?
        };
        let work = |s: &Self| {
            sql(s.db.execute("INSERT INTO runs VALUES (?,?,?,?) ON CONFLICT(id) DO UPDATE SET status=excluded.status,document=excluded.document", params![model::sqlite_key(model::field(run,"id")?)?,model::text(run,"taskId")?,status,document]))?;
            let id = model::sqlite_key(model::field(run, "id")?)?;
            sql(s.db.execute("DELETE FROM run_progress WHERE id=?", [&id]))?;
            if terminal {
                if let Some(id) = model::optional_text(run, "taskSnapshot") {
                    sql(s.db.execute("DELETE FROM task_snapshots WHERE id=? AND NOT EXISTS(SELECT 1 FROM runs WHERE json_extract(document,'$.taskSnapshot')=?)", [&id,&id]))?;
                }
            }
            if model::completed(run) {
                s.collect_runs(Some(&model::sqlite_key(model::field(run, "id")?)?))?;
            }
            Ok(())
        };
        if !self.db.is_autocommit() {
            work(self)
        } else {
            self.transaction(work)
        }
    }
    pub fn busy(&self, id: &str) -> IvyResult<bool> {
        self.exists("SELECT 1 FROM runs WHERE task_id=? AND status IN ('queued','running','publishing') LIMIT 1", [id])
    }
    pub fn executing(&self, id: &str) -> IvyResult<bool> {
        self.exists(
            "SELECT 1 FROM runs WHERE task_id=? AND status IN ('queued','running') LIMIT 1",
            [id],
        )
    }
    fn exists(&self, query: &str, values: impl rusqlite::Params) -> IvyResult<bool> {
        Ok(sql(self.db.query_row(query, values, |_| Ok(())).optional())?.is_some())
    }
    pub fn metadata(&self, key: &str) -> IvyResult<Option<String>> {
        sql(self
            .db
            .query_row("SELECT value FROM metadata WHERE key=?", [key], |r| {
                r.get(0)
            })
            .optional())
    }
    pub fn set_metadata(&self, key: &str, value: &str) -> IvyResult<()> {
        sql(self.db.execute(
            "INSERT INTO metadata VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [key, value],
        ))?;
        Ok(())
    }
    pub fn operation(&self, id: &str) -> IvyResult<Option<IvyJson>> {
        let found = self.document("SELECT result FROM operations WHERE id=?", id)?;
        if found.is_none() && self.exists("SELECT 1 FROM retired_operations WHERE id=?", [id])? {
            return Err(expired());
        }
        Ok(found)
    }
    pub fn used_run_id(&self, id: &str) -> IvyResult<bool> {
        self.exists(
            "SELECT 1 FROM runs WHERE id=? UNION ALL SELECT 1 FROM retired_runs WHERE id=? LIMIT 1",
            [id, id],
        )
    }
    fn forget_definition(&self, id: &str) -> IvyResult<u64> {
        let mut bytes = 0;
        for kind in [
            "definition-write:",
            "definition-operation:",
            "definition-request:",
            "definition-expiry:",
        ] {
            let prefix = format!("{kind}{id}");
            let mut statement = sql(self
                .db
                .prepare("DELETE FROM metadata WHERE key=? OR substr(key,1,?)=? RETURNING length(CAST(value AS BLOB))"))?;
            let rows = sql(statement.query_map(
                params![
                    prefix,
                    (prefix.chars().count() + 1) as i64,
                    format!("{prefix}:")
                ],
                |r| r.get::<_, i64>(0),
            ))?;
            for row in rows {
                bytes += sql(row)? as u64;
            }
        }
        Ok(bytes)
    }
    fn collect_operations(&self, protected: Option<&str>) -> IvyResult<()> {
        loop {
            let (count, bytes) = self.usage("operations")?;
            if count <= self.limits.operations
                && bytes + self.usage("definitions")?.1 <= self.limits.operation_bytes
            {
                return Ok(());
            }
            let mut statement = sql(self.db.prepare("SELECT id FROM operations o WHERE (? IS NULL OR id<>?) AND (COALESCE(json_type(result,'$.runId'),'null')<>'text' OR json_extract(result,'$.runId')='' OR EXISTS(SELECT 1 FROM retired_runs r WHERE r.id=json_extract(o.result,'$.runId')) OR EXISTS(SELECT 1 FROM runs r WHERE r.id=json_extract(o.result,'$.runId') AND r.status IN ('succeeded','failed','cancelled') AND lower(COALESCE(json_extract(r.document,'$.error'),'')) NOT GLOB '*unknown*')) ORDER BY created_at,rowid LIMIT 32"))?;
            let ids = sql(sql(
                statement.query_map(params![protected, protected], |r| r.get::<_, String>(0))
            )?
            .collect::<rusqlite::Result<Vec<_>>>())?;
            if ids.is_empty() {
                return Err(IvyError::new(
                    "capacity_exceeded",
                    "Collector journal capacity is occupied by live or unresolved work.",
                ));
            }
            for id in ids {
                let request: String = sql(self.db.query_row(
                    "SELECT request FROM operations WHERE id=?",
                    [&id],
                    |r| r.get(0),
                ))?;
                let hash = digest(&request);
                sql(self.db.execute(
                    "INSERT OR IGNORE INTO retired_operations VALUES(?,?)",
                    [&id, &hash],
                ))?;
                let definition = self
                    .metadata(&format!("definition-request:{id}"))?
                    .is_some();
                self.forget_definition(&id)?;
                if definition {
                    self.set_metadata(
                        &format!("definition-request:{id}"),
                        &encode(&IvyJson::object([
                            ("retired", true.into()),
                            ("requestHash", hash.into()),
                        ]))?,
                    )?;
                }
                sql(self.db.execute("DELETE FROM operations WHERE id=?", [&id]))?;
                let (count, bytes) = self.usage("operations")?;
                if count <= self.limits.operations
                    && bytes + self.usage("definitions")?.1 <= self.limits.operation_bytes
                {
                    return Ok(());
                }
            }
        }
    }
    pub fn mutate(
        &self,
        id: &str,
        request: &str,
        now_ms: i64,
        work: impl FnOnce(&Self) -> IvyResult<IvyJson>,
    ) -> IvyResult<IvyJson> {
        self.transaction(|s| {
            let prior: Option<(String, String)> = sql(s
                .db
                .query_row(
                    "SELECT request,result FROM operations WHERE id=?",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional())?;
            if let Some((previous, result)) = prior {
                if previous != request {
                    return Err(conflict());
                }
                return decode(&result);
            }
            let retired: Option<String> = sql(s
                .db
                .query_row(
                    "SELECT request_hash FROM retired_operations WHERE id=?",
                    [id],
                    |r| r.get(0),
                )
                .optional())?;
            if let Some(hash) = retired {
                if hash != digest(request) {
                    return Err(conflict());
                }
                return Err(expired());
            }
            let result = work(s)?;
            sql(s.db.execute(
                "INSERT INTO operations VALUES (?,?,?,?)",
                params![id, request, encode(&result)?, now_ms],
            ))?;
            s.collect_operations(Some(id))?;
            Ok(result)
        })
    }
    pub fn cleanup(&self, now_ms: i64) -> IvyResult<()> {
        self.transaction(|s| {
            let mut statement = sql(s.db.prepare("SELECT key FROM metadata m WHERE key LIKE 'definition-expiry:%' AND CAST(value AS REAL)<? AND (EXISTS(SELECT 1 FROM operations o WHERE m.key='definition-expiry:'||o.id) OR EXISTS(SELECT 1 FROM retired_operations o WHERE m.key='definition-expiry:'||o.id)) ORDER BY key LIMIT 32"))?;
            let rows = sql(statement.query_map([now_ms],|r| r.get::<_,String>(0)))?;
            let keys = sql(rows.collect::<rusqlite::Result<Vec<_>>>())?;
            for key in keys {
                let id = &key["definition-expiry:".len()..];
                if s.exists("SELECT 1 FROM operations WHERE id=? UNION ALL SELECT 1 FROM retired_operations WHERE id=? LIMIT 1",[id,id])? { s.forget_definition(id)?; }
            }
            s.collect_operations(None)?; s.collect_runs(None)
        })
    }
    fn collect_runs(&self, protected: Option<&str>) -> IvyResult<()> {
        let (mut count, mut bytes) = self.usage("runs")?;
        bytes += self.usage("snapshots")?.1 + self.usage("progress")?.1;
        if count <= self.limits.runs && bytes <= self.limits.run_bytes {
            return Ok(());
        }
        let mut statement = sql(self.db.prepare("SELECT id,length(CAST(document AS BLOB)) FROM runs r WHERE status IN ('succeeded','failed','cancelled') AND lower(COALESCE(json_extract(document,'$.error'),'')) NOT GLOB '*unknown*' AND (? IS NULL OR id<>?) AND NOT EXISTS(SELECT 1 FROM tasks t WHERE json_extract(t.document,'$.lastRunId')=r.id) ORDER BY json_extract(document,'$.finishedAt'),rowid LIMIT 32"))?;
        let rows = sql(sql(statement.query_map(params![protected, protected], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
        }))?
        .collect::<rusqlite::Result<Vec<_>>>())?;
        for (id, size) in rows {
            if count <= self.limits.runs && bytes <= self.limits.run_bytes {
                break;
            }
            sql(self
                .db
                .execute("INSERT OR IGNORE INTO retired_runs VALUES(?)", [&id]))?;
            sql(self.db.execute("DELETE FROM runs WHERE id=?", [&id]))?;
            count -= 1;
            bytes = bytes.saturating_sub(size);
        }
        Ok(())
    }
}
fn expired() -> IvyError {
    IvyError::new(
        "operation_expired",
        "The completed Collector operation was evicted; its identity cannot be reused.",
    )
}
fn conflict() -> IvyError {
    IvyError::new(
        "operation_conflict",
        "Operation ID was already used for another request.",
    )
}
